// Optional Jupyter integration for the Jupyter MCP server: the bundled `uv`
// sidecar provisions an ISOLATED environment (own managed Python — nothing on
// the user's machine is touched) under app data, and the app manages a
// headless jupyter-lab process the MCP server connects to.
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

use crate::runtime::{free_port, workspace_dir};

// Pinned per datalayer/jupyter-mcp-server's documented requirements.
const PIP_SPEC: &[&str] = &[
    "jupyterlab==4.4.1",
    "jupyter-collaboration==4.0.2",
    "jupyter-mcp-server",
    "ipykernel",
];

#[derive(Default)]
pub struct JupyterState {
    child: Mutex<Option<CommandChild>>,
    running: Mutex<bool>,
    /// Serializes start / re-root so overlapping workspace switches can never
    /// leave two jupyter-lab processes fighting over the fixed port.
    lifecycle: Mutex<()>,
    /// When set, jupyter-lab runs inside this WSL distro instead of natively.
    wsl_distro: Mutex<Option<String>>,
    /// When set, jupyter-lab runs on this SSH host instead of natively.
    ssh_host: Mutex<Option<String>>,
}

fn env_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("runtime")
        .join("jupyter-env"))
}

fn server_meta_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(env_dir(app)?.join("server.json"))
}

/// Where we record the managed jupyter-lab's PID, so a later run can kill an
/// orphan left by a crash/force-quit before rebinding the fixed port.
fn pid_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(env_dir(app)?.join("jupyter.pid"))
}

/// Kill an orphaned jupyter-lab from a previous app run (a crash or force quit
/// leaves one behind; two instances then fight over the fixed port). Best-effort
/// and precise: never touches unrelated processes.
fn kill_orphan_jupyter(app: &AppHandle) {
    // SSH: pkill port-scoped jupyter-lab on the remote host.
    #[cfg(windows)]
    if let Some(host) = ssh_mode(app) {
        if let Some(meta) = load_meta(app) {
            let pattern = format!("jupyter-lab.*--no-browser.*--port {}", meta.port);
            let _ = crate::remote::ssh_exec(&host, &format!("pkill -f '{}'", pattern));
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
    // WSL: kill via our managed port info (scoped: matches only jupyter-lab started
    // by this app with `--no-browser --ip 0.0.0.0 --port <X>`, not the user's own).
    #[cfg(windows)]
    if let Some(distro) = wsl_mode(app) {
        if let Some(meta) = load_meta(app) {
            let pattern = format!("jupyter-lab.*--no-browser.*--port {}", meta.port);
            let _ = crate::wsl::wsl_exec(&distro, "pkill", &["-f", &pattern]);
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
    // Unix: match the env's own jupyter-lab path — scoped, proven, no PID reuse risk.
    // SIGKILL, not SIGTERM: a wedged orphan survives TERM (observed in the field —
    // jupyter's graceful shutdown hangs on dead kernels) and these are our own
    // headless processes from a dead app run, so there is nothing to save.
    #[cfg(unix)]
    if let Ok(dir) = env_dir(app) {
        let pattern = format!("{}/bin/jupyter-lab", dir.to_string_lossy());
        let _ = std::process::Command::new("pkill").args(["-9", "-f", &pattern]).output();
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    // Windows: taskkill the recorded PID, filtered to python.exe so a recycled
    // PID belonging to some other process is spared.
    #[cfg(windows)]
    if let Ok(path) = pid_path(app) {
        if let Ok(pid) = std::fs::read_to_string(&path).map(|s| s.trim().to_string()) {
            if !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) {
                let _ = std::process::Command::new("taskkill")
                    .args([
                        "/FI",
                        &format!("PID eq {pid}"),
                        "/FI",
                        "IMAGENAME eq python.exe",
                        "/F",
                        "/T",
                    ])
                    .output();
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
        }
    }
}

fn bin(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let dir = env_dir(app)?;
    #[cfg(windows)]
    return Ok(dir.join("Scripts").join(format!("{name}.exe")));
    #[cfg(not(windows))]
    Ok(dir.join("bin").join(name))
}

/// Port + token are chosen once at setup and reused so the MCP config entry
/// (which carries JUPYTER_URL/JUPYTER_TOKEN) stays valid across app restarts.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ServerMeta {
    port: u16,
    token: String,
}

fn load_meta(app: &AppHandle) -> Option<ServerMeta> {
    let text = std::fs::read_to_string(server_meta_path(app).ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

// CSPRNG on every platform — the old Windows fallback (pid + nanos) was
// guessable, and this token is the only thing between localhost and the
// Jupyter server.
fn random_token() -> String {
    crate::runtime::random_hex(16)
}

/// Return the configured WSL distro name when the backend is set to WSL.
/// Returns `None` for native mode or non-Windows platforms.
fn wsl_mode(app: &AppHandle) -> Option<String> {
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(app);
        if config.kind == "wsl" {
            return config.distro;
        }
    }
    None
}

/// Return the configured SSH host when the backend is set to SSH mode.
/// Returns `None` for native mode or non-Windows platforms.
fn ssh_mode(app: &AppHandle) -> Option<String> {
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(app);
        if config.kind == "ssh" {
            return config.host;
        }
    }
    None
}

/// Check whether jupyter-lab is available in the current backend.
/// Native mode: checks the uv-managed env. WSL mode: checks via `which` inside
/// the distro, and jupyter-lab is accessed through the WSL PATH.
fn jupyter_bin_exists(app: &AppHandle) -> bool {
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(app);
        if config.kind == "ssh" {
            return config.host.as_ref().map_or(false, |h| {
                crate::remote::ssh_exec(h, "which jupyter-lab 2>/dev/null").is_ok()
            });
        } else if config.kind == "wsl" {
            return config
                .distro
                .as_ref()
                .map_or(false, |d| crate::wsl::wsl_check_tool(d, "jupyter-lab"));
        }
    }
    bin(app, "jupyter-lab").map(|p| p.exists()).unwrap_or(false)
}

/// Return the jupyter-mcp-server command string for the MCP config entry.
/// Native mode: full path to the uv-managed binary. WSL mode: command name
/// (resolved via WSL PATH).
fn jupyter_mcp_cmd(app: &AppHandle) -> Option<String> {
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(app);
        if config.kind == "ssh" {
            return config.host.as_ref().and_then(|h| {
                crate::remote::ssh_exec(h, "which jupyter-mcp-server 2>/dev/null")
                    .ok()
                    .map(|_| "jupyter-mcp-server".to_string())
            });
        } else if config.kind == "wsl" {
            return config.distro.as_ref().and_then(|d| {
                crate::wsl::wsl_check_tool(d, "jupyter-mcp-server")
                    .then(|| "jupyter-mcp-server".to_string())
            });
        }
    }
    bin(app, "jupyter-mcp-server")
        .ok()
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
}

#[derive(serde::Serialize)]
pub struct JupyterStatus {
    pub installed: bool,
    pub running: bool,
    pub url: Option<String>,
    pub token: Option<String>,
    /// Absolute jupyter-mcp-server path for the MCP config entry.
    pub mcp_command: Option<String>,
}

pub fn jupyter_url_for_backend(app: &AppHandle, port: u16) -> String {
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(app);
        if config.kind == "ssh" {
            // SSH tunnel always forwards to 127.0.0.1
            return format!("http://127.0.0.1:{port}");
        } else if config.kind == "wsl" {
            if let Some(ref distro) = config.distro {
                let addr = format!("127.0.0.1:{port}");
                use std::net::TcpStream;
                use std::time::Duration;
                if TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(300))
                    .is_ok()
                {
                    return format!("http://{addr}");
                }
                if let Some(ip) = crate::wsl::probe_wsl_ip(distro) {
                    return format!("http://{ip}:{port}");
                }
            }
        }
    }
    format!("http://127.0.0.1:{port}")
}

fn status_of(app: &AppHandle, state: &JupyterState) -> JupyterStatus {
    let installed = jupyter_bin_exists(app);
    let running = *state.running.lock().unwrap();
    let meta = load_meta(app);
    JupyterStatus {
        installed,
        running,
        url: meta.as_ref().map(|m| jupyter_url_for_backend(app, m.port)),
        token: meta.map(|m| m.token),
        mcp_command: jupyter_mcp_cmd(app),
    }
}

#[tauri::command]
pub fn jupyter_status(app: AppHandle, state: State<'_, JupyterState>) -> JupyterStatus {
    status_of(&app, &state)
}

/// Provision the isolated Jupyter environment with the bundled uv. First run
/// downloads a managed Python + JupyterLab (a few hundred MB into app data);
/// takes a few minutes. Async so the UI stays responsive.
#[tauri::command]
pub async fn setup_jupyter(app: AppHandle) -> Result<(), String> {
    let dir = env_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // SSH mode: pip3 install on the remote host (no uv sidecar needed).
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(&app);
        if config.kind == "ssh" {
            if let Some(ref host) = config.host {
                if crate::remote::ssh_exec(host, "which pip3 2>/dev/null").is_err() {
                    return Err(
                        "pip3 is not available on the remote host".into(),
                    );
                }
                // Install jupyter packages on the remote
                if crate::remote::ssh_exec(host, "which jupyter-lab 2>/dev/null").is_err() {
                    let spec = PIP_SPEC.iter().map(|s| *s).collect::<Vec<_>>().join(" ");
                    crate::remote::ssh_exec(host, &format!("pip3 install {}", spec))?;
                }
                // Persist server meta (port + token)
                if load_meta(&app).is_none() {
                    let meta = ServerMeta {
                        port: free_port(),
                        token: random_token(),
                    };
                    std::fs::write(
                        server_meta_path(&app)?,
                        serde_json::to_string(&meta).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
        } else if config.kind == "wsl" {
            if let Some(ref distro) = config.distro {
                if !crate::wsl::wsl_check_tool(distro, "pip3") {
                    return Err(
                        "pip3 is not available in WSL — install python3-pip in your distro".into(),
                    );
                }
                if !crate::wsl::wsl_check_tool(distro, "jupyter-lab") {
                    let mut pip_args = vec!["install"];
                    pip_args.extend_from_slice(PIP_SPEC);
                    crate::wsl::wsl_exec(distro, "pip3", &pip_args)?;
                }
                // Persist the server meta (port + token) like the native path does.
                if load_meta(&app).is_none() {
                    let meta = ServerMeta {
                        port: free_port(),
                        token: random_token(),
                    };
                    std::fs::write(
                        server_meta_path(&app)?,
                        serde_json::to_string(&meta).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
        }
    }

    let venv = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("uv sidecar not found: {e}"))?
        .args([
            "venv",
            &dir.to_string_lossy(),
            "--python",
            "3.12",
            "--allow-existing",
        ])
        .output()
        .await
        .map_err(|e| format!("uv venv failed to run: {e}"))?;
    if !venv.status.success() {
        return Err(format!(
            "uv venv failed: {}",
            String::from_utf8_lossy(&venv.stderr)
        ));
    }

    let py = bin(&app, "python")?;
    let mut args = vec![
        "pip".to_string(),
        "install".to_string(),
        "--python".to_string(),
        py.to_string_lossy().to_string(),
    ];
    args.extend(PIP_SPEC.iter().map(|s| s.to_string()));
    let install = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("uv sidecar not found: {e}"))?
        .args(args)
        .output()
        .await
        .map_err(|e| format!("uv pip install failed to run: {e}"))?;
    if !install.status.success() {
        return Err(format!(
            "uv pip install failed: {}",
            String::from_utf8_lossy(&install.stderr)
        ));
    }

    // Fix port + token once so the MCP config entry stays valid.
    if load_meta(&app).is_none() {
        let meta = ServerMeta {
            port: free_port(),
            token: random_token(),
        };
        std::fs::write(
            server_meta_path(&app)?,
            serde_json::to_string(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Start the managed headless jupyter-lab (idempotent). Root dir = workspace,
/// so the agent and the app's Notebooks page see the same files.
#[tauri::command]
pub fn start_jupyter(
    app: AppHandle,
    state: State<'_, JupyterState>,
) -> Result<JupyterStatus, String> {
    let _guard = state.lifecycle.lock().unwrap();
    if *state.running.lock().unwrap() {
        return Ok(status_of(&app, &state));
    }

    // Backend dispatch: SSH then WSL then native.
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(&app);
        if config.kind == "ssh" {
            if let Some(ref host) = config.host {
                return start_ssh_jupyter(&app, &state, host);
            }
        } else if config.kind == "wsl" {
            if let Some(ref distro) = config.distro {
                return start_wsl_jupyter(&app, &state, distro);
            }
        }
    }

    spawn_lab(&app, &state)
}

/// Spawn jupyter-lab rooted in the CURRENT active workspace. Caller holds the
/// lifecycle lock and has ensured no managed instance is running.
fn spawn_lab(app: &AppHandle, state: &JupyterState) -> Result<JupyterStatus, String> {
    let lab = bin(app, "jupyter-lab")?;
    if !lab.exists() {
        return Err("Jupyter is not set up yet".into());
    }
    let meta = load_meta(app).ok_or("Jupyter setup is incomplete (no server meta)")?;
    let workspace = workspace_dir(app)?;

    kill_orphan_jupyter(app);

    let cmd = app
        .shell()
        .command(lab.to_string_lossy().to_string())
        .args([
            "--no-browser".to_string(),
            "--ip".to_string(),
            "127.0.0.1".to_string(),
            "--port".to_string(),
            meta.port.to_string(),
            format!("--IdentityProvider.token={}", meta.token),
            format!("--ServerApp.root_dir={}", workspace.to_string_lossy()),
        ])
        .current_dir(workspace);
    let (mut rx, child) = cmd
        .spawn()
        .map_err(|e| format!("failed to start jupyter: {e}"))?;
    tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });
    // Record the PID so a future run can kill this process if it is orphaned.
    if let Ok(path) = pid_path(app) {
        let _ = std::fs::write(path, child.pid().to_string());
    }
    *state.child.lock().unwrap() = Some(child);
    *state.running.lock().unwrap() = true;
    Ok(status_of(app, state))
}

/// Follow a workspace switch: a running jupyter-lab keeps the root_dir it was
/// born with, so it must be restarted rooted in the NEW active workspace —
/// otherwise the agent's jupyter MCP keeps writing notebooks into the old
/// folder, invisible to the Notebooks page and previews. Port and token are
/// fixed in server meta, so the MCP config entry stays valid across the
/// restart. Runs in the background: a session switch must not wait on it.
pub fn reroot_jupyter(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<JupyterState>();
        let _guard = state.lifecycle.lock().unwrap();
        if !*state.running.lock().unwrap() {
            return;
        }
        kill_jupyter(&state);
        if let Err(e) = spawn_lab(&app, &state) {
            eprintln!("jupyter re-root failed: {e}");
        }
    });
}

/// Start jupyter-lab inside a WSL distro. Binds to `0.0.0.0` inside the WSL VM
/// (WSL2 NAT makes the port accessible from Windows via `127.0.0.1`). Working
/// directory is set via wsl.exe's `--cd` flag.
///
/// Windows only — the dispatch in [`start_jupyter`] is `#[cfg(windows)]`.
#[cfg(windows)]
fn start_wsl_jupyter(
    app: &AppHandle,
    state: &JupyterState,
    distro: &str,
) -> Result<JupyterStatus, String> {
    let meta = load_meta(app).ok_or("Jupyter setup is incomplete (no server meta)")?;
    let workspace = workspace_dir(app)?;
    let wsl_workspace = crate::wsl::windows_to_wsl_path(&workspace.to_string_lossy());

    kill_orphan_jupyter(app);

    let args: Vec<String> = vec![
        "--cd".into(),
        wsl_workspace.clone(),
        "--distribution".into(),
        distro.to_string(),
        "--".into(),
        "jupyter-lab".into(),
        "--no-browser".into(),
        "--ip".into(),
        "0.0.0.0".into(),
        "--port".into(),
        meta.port.to_string(),
        format!("--IdentityProvider.token={}", meta.token),
        format!("--ServerApp.root_dir={wsl_workspace}"),
    ];

    let cmd = app.shell().command("wsl.exe").args(&args);
    let (mut rx, child) = cmd
        .spawn()
        .map_err(|e| format!("failed to start jupyter in WSL: {e}"))?;
    tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });

    // Record the wsl.exe PID so kill_orphan_jupyter can find it (the native
    // taskkill branch is a no-op because IMAGENAME eq wsl.exe won't match, but
    // the WSL pkill branch above handles cleanup inside the distro).
    if let Ok(path) = pid_path(app) {
        let _ = std::fs::write(path, child.pid().to_string());
    }

    *state.child.lock().unwrap() = Some(child);
    *state.running.lock().unwrap() = true;
    *state.wsl_distro.lock().unwrap() = Some(distro.to_string());

    Ok(status_of(app, state))
}

/// Start jupyter-lab on a remote host via SSH. Creates a local tunnel so the
/// Windows frontend can reach the remote Jupyter instance at `127.0.0.1:<port>`.
///
/// Windows only — the dispatch in [`start_jupyter`] is `#[cfg(windows)]`.
#[cfg(windows)]
fn start_ssh_jupyter(
    app: &AppHandle,
    state: &JupyterState,
    host: &str,
) -> Result<JupyterStatus, String> {
    if !crate::hpc::is_safe_host(host) {
        return Err("invalid SSH host".into());
    }
    let meta = load_meta(app).ok_or("Jupyter setup is incomplete (no server meta)")?;

    kill_orphan_jupyter(app);

    let port_str = meta.port.to_string();
    let token_str = meta.token.clone();

    let cmd = app
        .shell()
        .command("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ConnectTimeout=8",
            "-L",
            &format!("127.0.0.1:{}:127.0.0.1:{}", meta.port, meta.port),
            "--",
            host,
            "jupyter-lab",
            "--no-browser",
            "--ip",
            "127.0.0.1",
            "--port",
            &port_str,
            &format!("--IdentityProvider.token={}", token_str),
        ]);

    let (mut rx, child) = cmd
        .spawn()
        .map_err(|e| format!("failed to start jupyter on SSH host '{host}': {e}"))?;
    tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });

    // Record the SSH tunnel PID so kill_orphan_jupyter can find it.
    if let Ok(path) = pid_path(app) {
        let _ = std::fs::write(path, child.pid().to_string());
    }

    *state.child.lock().unwrap() = Some(child);
    *state.running.lock().unwrap() = true;
    *state.ssh_host.lock().unwrap() = Some(host.to_string());

    Ok(status_of(app, state))
}

pub fn kill_jupyter(state: &JupyterState) {
    // SSH: pkill port-scoped jupyter-lab on the remote host (only kills instances
    // started with --no-browser, our own managed labs).
    if let Some(ref host) = state.ssh_host.lock().unwrap().take() {
        let _ = crate::remote::ssh_exec(host, "pkill -f 'jupyter-lab.*--no-browser'");
    }
    // WSL: pkill jupyter-lab inside the distro before killing wsl.exe, so the
    // subprocess doesn't survive as an orphan in the WSL process tree.
    if let Some(distro) = state.wsl_distro.lock().unwrap().take() {
        let _ = crate::wsl::wsl_exec(&distro, "pkill", &["-f", "jupyter-lab.*--no-browser"]);
    }
    if let Some(child) = state.child.lock().unwrap().take() {
        let _ = child.kill();
    }
    *state.running.lock().unwrap() = false;
}

/// Idempotent stop of Jupyter (Tauri command for the frontend).
#[tauri::command]
pub fn stop_jupyter(state: State<'_, JupyterState>) {
    kill_jupyter(&state);
}
