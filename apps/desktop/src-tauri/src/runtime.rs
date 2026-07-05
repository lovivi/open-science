// Manages the bundled OpenCode sidecar so it never interferes with any OpenCode
// the user already has: it runs the *bundled* binary, on a *dedicated free port*,
// with an *app-private* XDG config/data dir, and is killed on app exit.
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

use crate::opencode_config::merge_config;

#[derive(Default)]
pub struct RuntimeState {
    child: Mutex<Option<CommandChild>>,
    url: Mutex<Option<String>>,
    port: Mutex<Option<u16>>,
}

/// App-private runtime root, e.g. ~/Library/Application Support/com.ai4s.workbench/runtime
fn runtime_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("runtime"))
}

fn xdg_config_home(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(runtime_root(app)?.join("xdg-config"))
}

/// File recording the user's chosen active workspace folder (absolute path).
fn active_workspace_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(runtime_root(app)?.join("active-workspace.txt"))
}

/// File recording the user's chosen BASE folder — the parent every new dated
/// session workspace is created under (Settings → Workspace).
fn base_workspace_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(runtime_root(app)?.join("base-workspace.txt"))
}

/// The active workspace folder OpenCode / the kernel / previews / provenance all
/// operate in. Defaults to the base folder (`~/Documents/OpenScience`) until the
/// user opens or creates another one; the choice persists across restarts.
pub fn workspace_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(f) = active_workspace_file(app) {
        if let Ok(s) = std::fs::read_to_string(&f) {
            let dir = PathBuf::from(s.trim());
            if dir.is_dir() {
                return Ok(dir);
            }
        }
    }
    base_workspace_dir(app)
}

/// The workspace root new dated session folders are created under. A folder
/// the user picked in Settings wins; the default is `~/Documents/OpenScience`
/// (no space — the agent runs shell commands against this path, and unquoted
/// spaces break them), falling back to `$HOME/Documents`.
pub fn base_workspace_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(f) = base_workspace_file(app) {
        if let Ok(s) = std::fs::read_to_string(&f) {
            let dir = PathBuf::from(s.trim());
            if dir.is_dir() {
                return Ok(dir);
            }
        }
    }
    let docs = match app.path().document_dir() {
        Ok(d) => d,
        Err(_) => {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .map_err(|_| "could not resolve a documents directory".to_string())?;
            PathBuf::from(home).join("Documents")
        }
    };
    let dir = docs.join("OpenScience");

    // One-time migrations, oldest name last. A failed rename (e.g. cross-volume)
    // keeps the existing location rather than splitting the user's files.
    if !dir.exists() {
        for old in [
            docs.join("Open Science"),
            runtime_root(app)?.join("workspace"),
        ] {
            if old.is_dir() {
                if std::fs::rename(&old, &dir).is_ok() {
                    break;
                }
                return Ok(old);
            }
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Path OpenCode reads when XDG_CONFIG_HOME points at our private dir.
fn opencode_config_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(xdg_config_home(app)?.join("opencode").join("opencode.json"))
}

/// The config file to edit in place: the server may have rewritten the config
/// as opencode.jsonc — prefer whichever exists, fall back to opencode.json.
fn effective_config_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = xdg_config_home(app)?.join("opencode");
    Ok(["opencode.jsonc", "opencode.json"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| dir.join("opencode.json")))
}

/// The user's existing OpenCode auth file (their login / free credits), if any.
/// Read-only: we copy it into our sandbox so the bundled runtime can use the same
/// login, but we never modify the user's file or sessions.
fn user_auth_source() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            candidates.push(PathBuf::from(xdg).join("opencode").join("auth.json"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/share/opencode/auth.json"));
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("opencode").join("auth.json"));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// Copy the user's OpenCode CLI login into the app-private data dir, EXPLICITLY
/// (from the Settings page) — never silently. Returns false when there is no
/// CLI login to import. Restarts the sidecar so it picks the credentials up.
#[tauri::command]
pub fn import_opencode_login(
    app: AppHandle,
    state: State<'_, RuntimeState>,
) -> Result<bool, String> {
    let Some(src) = user_auth_source() else {
        return Ok(false);
    };
    let dst = runtime_root(&app)?
        .join("xdg-data")
        .join("opencode")
        .join("auth.json");
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(&src, &dst).map_err(|e| format!("copy failed: {e}"))?;

    // Restart the running sidecar so /config/providers reflects the login.
    if state.url.lock().unwrap().is_some() {
        restart_sidecar(&app, &state)?;
    }
    Ok(true)
}

/// Deploy the bundled skill packs (Tauri resources) into the app-private
/// profile's global skills dir (`<xdg-config>/opencode/skills/`), which OpenCode
/// scans regardless of project detection: `skills/` is the external ai4s-skills
/// pack, `skills-office/` Anthropic's document skills (docx/pdf/pptx/xlsx),
/// `skills-core/` the first-party skills from `runtime/skills/core`. The
/// workspace's own `.opencode/skills/` stays reserved for skills the user
/// installs. Runs before every sidecar start so app upgrades refresh the packs.
fn deploy_bundled_skills(app: &AppHandle) {
    let dst = match xdg_config_home(app) {
        Ok(cfg) => cfg.join("opencode").join("skills"),
        Err(_) => return,
    };
    for resource in ["skills", "skills-office", "skills-core"] {
        let src = match app
            .path()
            .resolve(resource, tauri::path::BaseDirectory::Resource)
        {
            Ok(p) if p.is_dir() => p,
            _ => continue, // dev run without `fetch-skills.sh` — nothing to deploy
        };
        if let Err(e) = sync_skill_pack(&src, &dst) {
            eprintln!("failed to deploy bundled skills ({resource}): {e}");
        }
    }
}

/// Copy every skill directory under `src` into `dst`, replacing same-named
/// directories (so bundled updates win) and leaving everything else in `dst`
/// alone (user-installed skills keep their own directories). Directories
/// without a SKILL.md (placeholders) are skipped.
fn sync_skill_pack(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !entry.path().join("SKILL.md").is_file() {
            continue;
        }
        let target = dst.join(entry.file_name());
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        copy_dir(&entry.path(), &target)?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// PATH for the sidecar (and everything the agent runs through it). Apps
/// launched from Finder/Dock get a minimal PATH (`/usr/bin:/bin:…`), so the
/// agent would not find the user's Python/conda/Homebrew tools. Prepend the
/// well-known scientific tool locations that actually exist on this machine —
/// the same order a terminal profile would produce.
#[cfg(unix)]
pub(crate) fn enriched_path() -> String {
    let base = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let extras = [
        format!("{home}/anaconda3/bin"),
        format!("{home}/miniconda3/bin"),
        "/opt/anaconda3/bin".to_string(),
        "/opt/miniconda3/bin".to_string(),
        format!("{home}/.pyenv/shims"),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        format!("{home}/.local/bin"),
    ];
    let mut parts: Vec<String> = extras
        .into_iter()
        .filter(|p| !base.split(':').any(|b| b == p) && std::path::Path::new(p).is_dir())
        .collect();
    if !base.is_empty() {
        parts.push(base);
    }
    parts.join(":")
}

/// Make a secret-holding path owner-only: 700 for directories, 600 for files
/// (unix). The runtime root carries provider/connector API keys in
/// `opencode.jsonc`/`auth.json`, and the sidecar rewrites those files with a
/// default umask while running — locking the DIRECTORY is what holds, since a
/// 700 dir is unreachable for other users whatever the file modes inside. On
/// Windows, %APPDATA% is per-user ACL'd already; nothing to do.
pub(crate) fn tighten_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = if meta.is_dir() { 0o700 } else { 0o600 };
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `bytes` bytes of OS randomness as lowercase hex. Panics only if the OS
/// CSPRNG is unavailable — a machine state where serving anything is unsafe.
pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS random source unavailable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Per-run password the sidecar requires on every HTTP request (OpenCode's
/// built-in Basic auth, `OPENCODE_SERVER_PASSWORD`). Generated fresh each app
/// launch and held only in memory — never written to disk — so a local
/// webpage that scans loopback ports can neither drive agent turns nor read
/// `/global/config` (which carries provider API keys). The webview gets it
/// via the `runtime_password` command; Tauri IPC is app-only.
pub(crate) fn server_password() -> &'static str {
    static PASSWORD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PASSWORD.get_or_init(|| random_hex(16))
}

/// Expose the per-run sidecar password to the frontend SDK client.
#[tauri::command]
pub fn runtime_password() -> String {
    server_password().to_string()
}

pub(crate) fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(43917)
}

fn spawn_sidecar(app: &AppHandle, port: u16) -> Result<CommandChild, String> {
    // Backend dispatch — SSH / WSL / Native
    #[cfg(windows)]
    {
        let cfg = load_backend_config(app);
        if cfg.kind == "ssh" {
            if let Some(ref host) = cfg.host {
                return spawn_ssh(app, port, host);
            }
        } else if cfg.kind == "wsl" {
            if let Some(ref distro) = cfg.distro {
                return spawn_wsl(app, port, distro);
            }
        }
    }

    let root = runtime_root(app)?;
    let cfg = root.join("xdg-config");
    let data = root.join("xdg-data");
    let cache = root.join("xdg-cache");
    let state = root.join("xdg-state");
    // Run OpenCode inside the user-facing workspace, NOT the app's cwd (which is `/`
    // when launched from Finder) — otherwise it scans the whole filesystem root.
    let workspace = workspace_dir(app)?;
    for d in [&cfg, &data, &cache, &state] {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    // Ship the bundled scientific skills into the app-private OpenCode profile.
    deploy_bundled_skills(app);
    // Safety default (AGENTS.md non-negotiable): on first run, seed the
    // "approve" permission mode so dangerous shell commands prompt for
    // approval. A mode the user chose (approve or full) is never overridden.
    let cfg_file = effective_config_file(app)?;
    let existing = std::fs::read_to_string(&cfg_file).unwrap_or_default();
    if let Some(seeded) = crate::opencode_config::seed_default_permission(&existing) {
        if let Some(dir) = cfg_file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&cfg_file, seeded).map_err(|e| e.to_string())?;
    }
    // Secrets live under the runtime root (provider/connector keys in
    // opencode.jsonc, OpenCode's auth.json) — owner-only on every start, so
    // existing installs are repaired and whatever the sidecar later rewrites
    // inside stays unreachable to other users regardless of its umask.
    tighten_private(&root);
    tighten_private(&cfg_file);
    let home = std::env::var("HOME").unwrap_or_default();
    let port_str = port.to_string();

    let cmd = app
        .shell()
        .sidecar("opencode")
        .map_err(|e| format!("sidecar not found: {e}"))?
        .args(["serve", "--hostname", "127.0.0.1", "--port", port_str.as_str()])
        // Require auth on every request (P0-7): without a password the server
        // trusts ANY localhost-origin page (verified in the 1.17.13 source —
        // its CORS allowlist admits http://localhost:*/127.0.0.1:* wholesale,
        // and `--cors "*"` was only ever an exact-match literal, not a
        // wildcard). The webview authenticates via the SDK; nothing else may.
        .env("OPENCODE_SERVER_PASSWORD", server_password())
        // App-private dirs: OpenCode never touches the user's ~/.config/opencode.
        .env("XDG_CONFIG_HOME", cfg.to_string_lossy().to_string())
        .env("XDG_DATA_HOME", data.to_string_lossy().to_string())
        .env("XDG_CACHE_HOME", cache.to_string_lossy().to_string())
        .env("XDG_STATE_HOME", state.to_string_lossy().to_string())
        .env("HOME", home)
        .current_dir(workspace);
    // GUI-launched apps get a minimal PATH; give the agent the user's real tools.
    #[cfg(unix)]
    let cmd = cmd.env("PATH", enriched_path());

    let (mut rx, child) = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn opencode: {e}"))?;
    // Drain events so the child's stdout/stderr buffer never blocks it.
    tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });
    Ok(child)
}

/// Kill and respawn the sidecar on its stable port, returning the base URL.
/// The whole kill→spawn runs while HOLDING the child mutex — it doubles as
/// the lifecycle lock, so two concurrent restarts (e.g. Settings saves racing
/// an approval-mode switch) can never double-spawn and orphan a child. This
/// is the single restart path; config-changing commands must use it.
fn restart_sidecar(app: &AppHandle, state: &RuntimeState) -> Result<String, String> {
    let mut child = state.child.lock().unwrap();
    if let Some(c) = child.take() {
        let _ = c.kill();
    }
    let port = { *state.port.lock().unwrap().get_or_insert_with(free_port) };
    *child = Some(spawn_sidecar(app, port)?);
    let url = format!("http://127.0.0.1:{port}");
    *state.url.lock().unwrap() = Some(url.clone());
    Ok(url)
}

// ── WSL sidecar ─────────────────────────────────────────────────────────────

/// Path to the backend config file under the runtime root.
fn backend_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(runtime_root(app)?.join("backend.json"))
}

/// Persist the backend selection to disk.
pub fn persist_backend_config(
    app: &AppHandle,
    config: &crate::wsl::BackendConfig,
) -> Result<(), String> {
    let path = backend_config_path(app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

/// Save the backend selection from the frontend (Tauri command).
#[tauri::command]
pub fn save_backend_config(
    app: AppHandle,
    kind: String,
    distro: Option<String>,
    host: Option<String>,
) -> Result<(), String> {
    if !matches!(kind.as_str(), "native" | "wsl" | "ssh") {
        return Err(format!("invalid backend kind: {kind}"));
    }
    if kind == "ssh" {
        if let Some(ref h) = host {
            if !crate::hpc::is_safe_host(h) {
                return Err("invalid SSH host format".into());
            }
        } else {
            return Err("SSH backend requires a host".into());
        }
    }
    let config = crate::wsl::BackendConfig { kind, distro, host };
    persist_backend_config(&app, &config)
}

/// Load the current backend config from the frontend (Tauri command).
#[tauri::command]
pub fn get_backend_config(app: AppHandle) -> Result<crate::wsl::BackendConfig, String> {
    Ok(load_backend_config(&app))
}

/// Load the persisted backend selection (defaults to native).
pub fn load_backend_config(app: &AppHandle) -> crate::wsl::BackendConfig {
    let path = match backend_config_path(app) {
        Ok(p) => p,
        Err(_) => {
            return crate::wsl::BackendConfig {
                kind: "native".into(),
                distro: None,
                host: None,
            }
        }
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            return crate::wsl::BackendConfig {
                kind: "native".into(),
                distro: None,
                host: None,
            }
        }
    };
    serde_json::from_str(&text).unwrap_or(crate::wsl::BackendConfig {
        kind: "native".into(),
        distro: None,
        host: None,
    })
}

/// Return the SSH host when the backend is configured for SSH mode, or `None`.
///
/// On non-Windows this always returns `None` because SSH remote execution is
/// only supported under Windows in this release.
pub fn get_remote_host_if_ssh(app: &AppHandle) -> Option<String> {
    #[cfg(windows)]
    {
        let config = load_backend_config(app);
        if config.kind == "ssh" {
            return config.host.clone();
        }
    }
    let _ = app;
    None
}

/// Fetch the remote user's home directory (the workspace root on the remote
/// side).  Only meaningful when the backend is in SSH mode.
#[cfg(windows)]
pub fn remote_workspace_path(host: &str) -> Result<String, String> {
    crate::remote::ssh_exec(host, "echo $HOME").map(|s| s.trim().to_string())
}

/// Resolve the filesystem path of a bundled Tauri sidecar binary.
#[cfg(windows)]
fn resolve_sidecar_path(app: &AppHandle, binary: &str) -> Result<PathBuf, String> {
    // Verify the sidecar is declared in tauri.conf.
    let _ = app
        .shell()
        .sidecar(binary)
        .map_err(|e| format!("sidecar '{binary}' not configured: {e}"))?;

    let exe_name = format!("{binary}.exe");
    // Tauri 2 sidecar naming: <binary>-<target-triple>[.ext]
    let target_name = format!("{binary}-x86_64-pc-windows-msvc");

    // 1. Same directory as the running executable (dev & packaged)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            for name in [format!("{target_name}.exe"), exe_name.clone()] {
                let p = dir.join(&name);
                if p.exists() {
                    return Ok(p);
                }
            }
        }
    }
    // 2. Tauri resource directory (production bundle)
    if let Ok(resource_dir) = app.path().resource_dir() {
        for name in [format!("{target_name}.exe"), exe_name.clone()] {
            let p = resource_dir.join(&name);
            if p.exists() {
                return Ok(p);
            }
        }
    }
    // 3. CARGO_MANIFEST_DIR/binaries/ (dev: built sidecar is staged here)
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest)
            .join("binaries")
            .join(format!("{target_name}.exe"));
        if p.exists() {
            return Ok(p);
        }
    }

    Err(format!("cannot locate sidecar binary for '{binary}'"))
}

/// Ensure the bundled `opencode` binary is present inside the WSL distro at
/// `~/.local/bin/opencode`. Returns the WSL-side absolute path.
///
/// On each call, compares the Windows sidecar's modified time with the WSL
/// copy's mtime so app updates are synced into WSL automatically.
#[cfg(windows)]
fn ensure_opencode_in_wsl(app: &AppHandle, distro: &str) -> Result<String, String> {
    use crate::wsl::{copy_to_wsl, wsl_exec};

    let home = wsl_exec(distro, "sh", &["-c", "echo $HOME"]).unwrap_or_else(|_| "/root".into());
    let bin_dir = format!("{}/.local/bin", home.trim());
    let bin_path = format!("{}/opencode", bin_dir);

    // Compare mtime of the Windows sidecar vs the WSL copy to detect app updates.
    let src = resolve_sidecar_path(app, "opencode")?;
    let force = src
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()))
        .unwrap_or(0);

    // Query WSL copy's mtime via `stat`
    let wsl_mtime = wsl_exec(distro, "stat", &["-c", "%Y", &bin_path])
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);

    if wsl_exec(distro, "test", &["-x", &bin_path]).is_ok() && wsl_mtime >= force {
        return Ok(bin_path);
    }

    // Copy from the bundled sidecar
    wsl_exec(distro, "mkdir", &["-p", &bin_dir])?;
    copy_to_wsl(distro, &src, &Path::new(&bin_path))?;

    Ok(bin_path)
}

/// Spawn the bundled OpenCode inside a WSL distro instead of natively.
///
/// The binary runs inside WSL, binds to `0.0.0.0` (so the Windows frontend can
/// reach it), and stores its config in the WSL Linux filesystem for performance.
#[cfg(windows)]
fn spawn_wsl(app: &AppHandle, port: u16, distro: &str) -> Result<CommandChild, String> {
    use crate::wsl::{windows_to_wsl_path, wsl_available, wsl_exec};

    if !wsl_available() {
        return Err("WSL is not available on this system".into());
    }

    // Create runtime directories on Windows (for config persistence)
    let root = runtime_root(app)?;
    let workspace = workspace_dir(app)?;
    deploy_bundled_skills(app);

    // Ensure the opencode binary is copied into WSL
    let wsl_bin = ensure_opencode_in_wsl(app, distro)?;

    // Resolve WSL-side paths
    let home = wsl_exec(distro, "sh", &["-c", "echo $HOME"]).unwrap_or_else(|_| "/root".into());
    let home = home.trim();
    let wsl_config = format!("{home}/.config");
    let wsl_data = format!("{home}/.local/share");
    let wsl_cache = format!("{home}/.cache");
    let wsl_state = format!("{home}/.local/state");
    let wsl_workspace = windows_to_wsl_path(&workspace.to_string_lossy());

    let port_str = port.to_string();

    // Build the wsl.exe command
    // Use --cd to set the working directory inside WSL
    let args: Vec<String> = vec![
        "--cd".into(),
        wsl_workspace,
        "--distribution".into(),
        distro.to_string(),
        "--".into(),
        wsl_bin,
        "serve".into(),
        "--hostname".into(),
        "0.0.0.0".into(),
        "--port".into(),
        port_str,
        "--cors".into(),
        "*".into(),
    ];

    let cmd = app
        .shell()
        .command("wsl.exe")
        .args(&args)
        .env("XDG_CONFIG_HOME", &wsl_config)
        .env("XDG_DATA_HOME", &wsl_data)
        .env("XDG_CACHE_HOME", &wsl_cache)
        .env("XDG_STATE_HOME", &wsl_state)
        .env("HOME", home);

    let (mut rx, child) = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn WSL opencode: {e}"))?;
    tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });
    Ok(child)
}

/// Spawn the bundled OpenCode on a remote host over SSH.
///
/// 1. Verifies the host is reachable (SSH connectivity check).
/// 2. Validates that `opencode` is available on the remote PATH.
/// 3. Spawns a single SSH process that BOTH starts opencode serve on the remote
///    AND creates a local TCP tunnel (`-L`) so the Windows frontend can reach
///    the remote opencode via `127.0.0.1:<port>`.
///
/// When the returned child is killed (via `stop_runtime`), the SSH connection
/// drops and the remote opencode process is automatically cleaned up — no
/// separate tunnel management needed.
#[cfg(windows)]
fn spawn_ssh(app: &AppHandle, port: u16, host: &str) -> Result<CommandChild, String> {
    if !crate::hpc::is_safe_host(host) {
        return Err("invalid host".into());
    }

    // 1. Check reachability
    match crate::remote::check_reachable(host) {
        Ok(true) => {}
        Ok(false) => {
            return Err(format!("host '{host}' is not reachable over SSH"));
        }
        Err(e) => {
            return Err(format!("SSH connection to '{host}' failed: {e}"));
        }
    }

    // 2. OpenCode must be installed on the remote
    if crate::remote::ssh_exec(host, "which opencode 2>/dev/null").is_err() {
        return Err(format!(
            "opencode not found on '{host}' — install opencode on the remote server"
        ));
    }

    // 3. Start opencode serve on the remote with a local port-forwarding tunnel
    let port_str = port.to_string();
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
            &format!("127.0.0.1:{port}:127.0.0.1:{port}"),
            "--",
            host,
            "opencode",
            "serve",
            "--hostname",
            "127.0.0.1",
            "--port",
            &port_str,
            "--cors",
            "*",
        ]);

    let (mut rx, child) = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn SSH opencode: {e}"))?;
    tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });
    Ok(child)
}

/// Start the bundled OpenCode (idempotent). Returns its base URL.
#[tauri::command]
pub fn start_runtime(app: AppHandle, state: State<'_, RuntimeState>) -> Result<String, String> {
    if let Some(url) = state.url.lock().unwrap().clone() {
        return Ok(url);
    }
    // Reuse a stable port across restarts so the frontend URL doesn't change.
    let port = {
        let mut p = state.port.lock().unwrap();
        *p.get_or_insert_with(free_port)
    };
    let child = spawn_sidecar(&app, port)?;
    let url = format!("http://127.0.0.1:{port}");
    *state.child.lock().unwrap() = Some(child);
    *state.url.lock().unwrap() = Some(url.clone());
    Ok(url)
}

/// The workspace directory the sidecar runs in — the frontend passes it to the
/// SDK so skill discovery is scoped to the right OpenCode instance.
#[tauri::command]
pub fn workspace_path(app: AppHandle) -> Result<String, String> {
    Ok(workspace_dir(&app)?.to_string_lossy().to_string())
}

/// The base folder new dated workspaces are created under (`~/Documents/OpenScience`).
#[tauri::command]
pub fn workspace_base(app: AppHandle) -> Result<String, String> {
    Ok(base_workspace_dir(&app)?.to_string_lossy().to_string())
}

/// Choose the base folder (Settings → Workspace → Change). Creates it if
/// needed and persists the choice; every NEW session's dated folder is created
/// under it. Existing sessions keep their folders.
#[tauri::command]
pub fn set_workspace_base(app: AppHandle, path: String) -> Result<String, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_absolute() {
        return Err("workspace base must be absolute".into());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create folder: {e}"))?;
    let canon = dir.canonicalize().map_err(|e| e.to_string())?;
    std::fs::write(base_workspace_file(&app)?, canon.to_string_lossy().as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(canon.to_string_lossy().to_string())
}

/// Reveal the base workspace folder in the OS file manager. (The sandboxed
/// `open_path` resolves inside the ACTIVE workspace only, which may be a dated
/// subfolder — the base needs its own door.)
#[tauri::command]
pub fn open_workspace_base(app: AppHandle) -> Result<(), String> {
    crate::artifact_file::os_open(&base_workspace_dir(&app)?)
}

/// Switch the active workspace folder: create it if needed and persist the
/// choice. The kernel / Files / provenance read the folder via `workspace_dir`;
/// the agent runtime is scoped per request — the frontend reconnects its event
/// stream with `?directory=` and creates sessions with it (a bare `/event`
/// stream would not see other folders' instances, so the scoped stream is
/// required). `path` must be absolute.
#[tauri::command]
pub fn set_workspace(
    app: AppHandle,
    _state: State<'_, RuntimeState>,
    path: String,
) -> Result<String, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_absolute() {
        return Err("workspace path must be absolute".into());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create folder: {e}"))?;
    let canon = dir.canonicalize().map_err(|e| e.to_string())?;
    std::fs::write(active_workspace_file(&app)?, canon.to_string_lossy().as_bytes())
        .map_err(|e| e.to_string())?;

    // No sidecar restart: OpenCode serves every folder from one process via
    // per-directory instances, and the frontend reconnects its event stream
    // with `?directory=<new folder>`. Restarting here used to cost 3-6 s per
    // history-session switch (process boot + reconnect polling).
    // Jupyter-lab, however, pins its root_dir at spawn time — re-root it (in
    // the background) so agent-created notebooks land in the new folder.
    crate::jupyter::reroot_jupyter(&app);
    Ok(canon.to_string_lossy().to_string())
}

/// Create a new dated folder `<base>/<name>` and switch to it. `name` is a
/// single path segment (the frontend supplies a timestamp); rejects separators.
#[tauri::command]
pub fn new_dated_workspace(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    name: String,
) -> Result<String, String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("invalid folder name".into());
    }
    let dir = base_workspace_dir(&app)?.join(&name);
    set_workspace(app, state, dir.to_string_lossy().to_string())
}

/// Native "choose a folder" dialog; returns the absolute path, or None on cancel.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let Some(picked) = app.dialog().file().blocking_pick_folder() else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().to_string()))
}

/// Kill the bundled OpenCode if running.
#[tauri::command]
pub fn stop_runtime(state: State<'_, RuntimeState>) {
    if let Some(child) = state.child.lock().unwrap().take() {
        kill_process_tree(&child);
        let _ = child.kill();
    }
    *state.url.lock().unwrap() = None;
}

/// Attempt to kill a process and its entire process tree (crucial for WSL: when
/// wsl.exe is killed without its tree the Linux-side opencode stays alive).
#[cfg_attr(not(windows), allow(unused_variables))]
fn kill_process_tree(child: &CommandChild) {
    #[cfg(windows)]
    {
        let pid = child.pid();
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
}

pub fn kill_child(state: &RuntimeState) {
    if let Some(child) = state.child.lock().unwrap().take() {
        kill_process_tree(&child);
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::{random_hex, remove_key_from_config, sync_skill_pack};
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn tighten_private_makes_dir_and_secrets_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("os-private-{}", std::process::id()));
        let sub = dir.join("opencode");
        fs::create_dir_all(&sub).unwrap();
        let cfg = sub.join("opencode.jsonc");
        fs::write(&cfg, b"{\"apiKey\":\"secret\"}").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&cfg, fs::Permissions::from_mode(0o644)).unwrap();

        // The runtime root holds provider/connector keys (opencode.jsonc,
        // auth.json) — it must be unreadable to other users even when the
        // sidecar later rewrites files inside with a default umask.
        super::tighten_private(&dir);
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        super::tighten_private(&cfg);
        assert_eq!(fs::metadata(&cfg).unwrap().permissions().mode() & 0o777, 0o600);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn random_hex_is_csprng_shaped() {
        // 16 bytes → 32 hex chars, fresh per call — the shape the sidecar
        // password and the preview/Jupyter tokens rely on.
        let a = random_hex(16);
        let b = random_hex(16);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two draws must differ");
    }

    #[test]
    fn removes_only_the_named_config_entry() {
        let cfg = r#"{"model":"a/b","provider":{"ollama":{"npm":"x"},"keep":{"npm":"y"}},"mcp":{"pw":{"type":"local"}}}"#;
        let out = remove_key_from_config(cfg, "provider", "ollama").unwrap();
        assert!(!out.contains("ollama"));
        assert!(out.contains("keep"));
        assert!(out.contains("\"model\": \"a/b\""));
        let out2 = remove_key_from_config(cfg, "mcp", "pw").unwrap();
        assert!(!out2.contains("\"pw\""));
        // Absent key and non-JSON input are errors, not silent no-ops.
        assert!(remove_key_from_config(cfg, "provider", "missing").is_err());
        assert!(remove_key_from_config("// jsonc comment\n{}", "provider", "x").is_err());
    }

    fn write(path: &std::path::Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn sync_replaces_bundled_and_keeps_user_skills() {
        let tmp = std::env::temp_dir().join(format!("skillsync-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        let dst = tmp.join("dst");

        // Bundled pack: one skill with a nested reference file, plus a top-level
        // plain file (.commit) that must NOT be copied.
        write(&src.join("paper-writer/SKILL.md"), "v2");
        write(&src.join("paper-writer/references/guide.md"), "ref");
        write(&src.join(".commit"), "abc123");
        // A placeholder dir without SKILL.md must not be deployed.
        fs::create_dir_all(src.join("placeholder")).unwrap();

        // Existing workspace: a stale copy of the bundled skill (with a file the
        // new version no longer has) and a user-installed skill.
        write(&dst.join("paper-writer/SKILL.md"), "v1");
        write(&dst.join("paper-writer/obsolete.md"), "old");
        write(&dst.join("my-skill/SKILL.md"), "user");

        sync_skill_pack(&src, &dst).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("paper-writer/SKILL.md")).unwrap(),
            "v2"
        );
        assert_eq!(
            fs::read_to_string(dst.join("paper-writer/references/guide.md")).unwrap(),
            "ref"
        );
        assert!(
            !dst.join("paper-writer/obsolete.md").exists(),
            "stale file must be gone"
        );
        assert_eq!(
            fs::read_to_string(dst.join("my-skill/SKILL.md")).unwrap(),
            "user"
        );
        assert!(
            !dst.join(".commit").exists(),
            "top-level files are not skills"
        );
        assert!(
            !dst.join("placeholder").exists(),
            "dirs without SKILL.md are not skills"
        );

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn sync_creates_destination_when_missing() {
        let tmp = std::env::temp_dir().join(format!("skillsync-new-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        write(&src.join("literature-survey/SKILL.md"), "s");

        let dst = tmp.join("deep/nested/skills");
        sync_skill_pack(&src, &dst).unwrap();
        assert_eq!(
            fs::read_to_string(dst.join("literature-survey/SKILL.md")).unwrap(),
            "s"
        );
        fs::remove_dir_all(&tmp).unwrap();
    }
}

/// Remove an entry from a map section of the app-private global OpenCode
/// config ("provider" or "mcp") and restart the sidecar (PATCH /global/config
/// cannot delete keys).
#[tauri::command]
pub fn remove_config_entry(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    section: String,
    key: String,
) -> Result<(), String> {
    if !matches!(section.as_str(), "provider" | "mcp") {
        return Err(format!("section \"{section}\" is not removable"));
    }
    let dir = xdg_config_home(&app)?.join("opencode");
    // The server writes opencode.jsonc; older configs may be opencode.json.
    let path = ["opencode.jsonc", "opencode.json"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
        .ok_or("no global OpenCode config found")?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let out = remove_key_from_config(&text, &section, &key)?;
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    tighten_private(&path);

    if state.url.lock().unwrap().is_some() {
        restart_sidecar(&app, &state)?;
    }
    Ok(())
}

/// Drop `key` from the config JSON's `section` map, erroring when the config
/// is not plain JSON or the key is absent.
fn remove_key_from_config(text: &str, section: &str, key: &str) -> Result<String, String> {
    let mut cfg: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("config is not plain JSON: {e}"))?;
    let removed = cfg
        .get_mut(section)
        .and_then(|p| p.as_object_mut())
        .map(|p| p.remove(key).is_some())
        .unwrap_or(false);
    if !removed {
        return Err(format!(
            "\"{key}\" is not in the config's {section} section"
        ));
    }
    serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())
}

/// The current approval mode ("approve" | "full"). Spawn seeding guarantees a
/// mode exists once the runtime has started; before that, report the default.
#[tauri::command]
pub fn get_approval_mode(app: AppHandle) -> Result<String, String> {
    let path = effective_config_file(&app)?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    Ok(crate::opencode_config::permission_mode_of(&existing)
        .unwrap_or(crate::opencode_config::MODE_APPROVE)
        .to_string())
}

/// Switch the approval mode and restart the sidecar so the permission rules
/// take effect. Returns the (stable-port) base URL when it was running.
#[tauri::command]
pub fn set_approval_mode(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    mode: String,
) -> Result<String, String> {
    let path = effective_config_file(&app)?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = crate::opencode_config::set_permission_mode(&existing, &mode)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, updated).map_err(|e| e.to_string())?;
    tighten_private(&path);

    // Same restart flow as configure_opencode: reload rules on a stable port.
    if state.url.lock().unwrap().is_some() {
        restart_sidecar(&app, &state)
    } else {
        Ok(path.to_string_lossy().to_string())
    }
}

/// Write the provider key/model into the app-private OpenCode config and restart
/// the sidecar so it picks them up. Returns the same base URL (stable port).
#[tauri::command]
pub fn configure_opencode(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    provider: String,
    api_key: String,
    model: String,
    base_url: Option<String>,
) -> Result<String, String> {
    let path = opencode_config_file(&app)?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge_config(&existing, &provider, &api_key, &model, base_url.as_deref())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, merged).map_err(|e| e.to_string())?;
    tighten_private(&path);

    // Restart so the running server reloads the new provider config.
    if state.url.lock().unwrap().is_some() {
        restart_sidecar(&app, &state)
    } else {
        Ok(path.to_string_lossy().to_string())
    }
}
