// Local notebook kernels. One persistent child per language ("python" | "r"),
// each holding shared state across cells like a Jupyter kernel. Fully local —
// uses whatever Python/R is installed, no network, no model key.
//
// Wire protocol (one request, one response line):
//   - Python: write {"id","code"} JSON; the bridge replies with a JSON line.
//   - R:      write the cell code to the kernel's code file, then send "<id>";
//             the bridge reads that file and replies with the SAME JSON shape.
// The read side is therefore identical for both languages.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};

// The bridge scripts ship in-tree; embed them and materialize on first use so
// they are always present next to the app regardless of packaging.
const PY_BRIDGE_SRC: &str = include_str!("../../../../runtime/kernel/kernel_bridge.py");
const R_BRIDGE_SRC: &str = include_str!("../../../../runtime/kernel/kernel_bridge.R");

/// The cell-execution side of a kernel: written/read one request at a time.
struct KernelIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: u64,
    /// For the R kernel: the file the host writes each cell's code into. None for Python.
    code_file: Option<PathBuf>,
    /// When set, the `code_file` lives on this SSH host and must be written
    /// remotely via `ssh_write_file`.  `None` for native / WSL kernels.
    ssh_host: Option<String>,
}

/// A running kernel. Two INDEPENDENT locks by design: `io` is held for the
/// whole cell (including an unbounded blocking read — a `while True: pass`
/// cell holds it forever), while `child` is only ever taken to kill/reap, so
/// a reset can always terminate a hung kernel without waiting on `io`.
struct Kernel {
    child: Mutex<Child>,
    io: Mutex<KernelIo>,
}

impl Kernel {
    fn from_child(mut child: Child, code_file: Option<PathBuf>, ssh_host: Option<String>) -> Result<Self, String> {
        let stdin = child.stdin.take().ok_or("no kernel stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no kernel stdout")?);
        Ok(Kernel {
            child: Mutex::new(child),
            io: Mutex::new(KernelIo { stdin, stdout, seq: 0, code_file, ssh_host }),
        })
    }
}

/// Persistent kernels keyed by `kernel_key` — one per notebook (Jupyter
/// semantics: each notebook gets its own kernel, cwd = the notebook's folder,
/// no state bleed between notebooks). The map lock is held only to look up /
/// insert / remove entries — NEVER across cell I/O (the old design did, and a
/// hung cell wedged every kernel command including reset; see P0-7).
#[derive(Default)]
pub struct KernelState(Mutex<HashMap<String, std::sync::Arc<Kernel>>>);

/// Map key for a kernel: `<lang>:<absolute notebook path>`, or `<lang>:@workspace`
/// for legacy notebook-less execution in the active workspace.
fn kernel_key(lang: &str, notebook_abs: Option<&std::path::Path>) -> String {
    match notebook_abs {
        Some(p) => format!("{lang}:{}", p.to_string_lossy()),
        None => format!("{lang}:@workspace"),
    }
}

/// Stable filename suffix for per-kernel scratch files (the R code file).
fn key_hash(key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[derive(serde::Serialize)]
pub struct ExecResult {
    ok: bool,
    stdout: String,
    result: Option<String>,
    error: Option<String>,
}

/// Normalize a requested language to a supported kernel id. Empty -> python.
fn normalize_lang(language: &str) -> Result<&'static str, String> {
    match language.trim().to_ascii_lowercase().as_str() {
        "" | "python" | "python3" => Ok("python"),
        "r" => Ok("r"),
        other => Err(format!("unsupported kernel language: {other}")),
    }
}

fn kernel_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("runtime")
        .join("kernel");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

// Rewrite the bridge each start so an app update ships a fresh copy.
fn materialize(app: &AppHandle, name: &str, src: &str) -> Result<PathBuf, String> {
    let path = kernel_dir(app)?.join(name);
    std::fs::write(&path, src).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Common Python install locations per OS. A GUI app launched from Finder/Explorer
/// has a minimal PATH, so we fall back to these before giving up.
#[cfg(windows)]
fn python_candidates() -> Vec<String> {
    // `py` (the launcher) and `python` are what Windows installers register.
    let mut c = vec![
        "py".to_string(),
        "python".to_string(),
        "python3".to_string(),
    ];
    if let Ok(profile) = std::env::var("USERPROFILE") {
        c.push(format!("{profile}\\anaconda3\\python.exe"));
        c.push(format!("{profile}\\miniconda3\\python.exe"));
        c.push(format!(
            "{profile}\\AppData\\Local\\Programs\\Python\\python.exe"
        ));
    }
    c
}

#[cfg(not(windows))]
fn python_candidates() -> Vec<String> {
    let mut c = vec!["python3".to_string(), "python".to_string()];
    if let Ok(home) = std::env::var("HOME") {
        c.push(format!("{home}/anaconda3/bin/python3"));
        c.push(format!("{home}/miniconda3/bin/python3"));
        c.push(format!("{home}/.pyenv/shims/python3"));
    }
    c.extend(
        [
            "/opt/anaconda3/bin/python3",
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
            "/usr/bin/python3",
        ]
        .map(String::from),
    );
    c
}

/// Common Rscript locations (same minimal-PATH problem as Python).
#[cfg(windows)]
fn rscript_candidates() -> Vec<String> {
    let mut c = vec!["Rscript".to_string(), "Rscript.exe".to_string()];
    for base in ["C:\\Program Files\\R", "C:\\Program Files (x86)\\R"] {
        // Newest install layout: <base>\R-x.y.z\bin\Rscript.exe — probed via PATH first,
        // this literal fallback covers the common single-version install.
        c.push(format!("{base}\\bin\\Rscript.exe"));
    }
    c
}

#[cfg(not(windows))]
fn rscript_candidates() -> Vec<String> {
    let mut c = vec!["Rscript".to_string()];
    if let Ok(home) = std::env::var("HOME") {
        c.push(format!("{home}/anaconda3/bin/Rscript"));
        c.push(format!("{home}/miniconda3/bin/Rscript"));
    }
    c.extend(
        [
            "/opt/homebrew/bin/Rscript",
            "/usr/local/bin/Rscript",
            "/opt/anaconda3/bin/Rscript",
            "/usr/bin/Rscript",
            // macOS R.framework install (r-project.org .pkg)
            "/Library/Frameworks/R.framework/Resources/bin/Rscript",
        ]
        .map(String::from),
    );
    c
}

/// Probe an interpreter with the SAME PATH the kernel will run it under, so
/// detection and execution resolve to the same binary (e.g. bare `python3` →
/// the user's anaconda, not a system Python).
fn interpreter_ok(bin: &str) -> bool {
    let mut c = Command::new(bin);
    c.arg("--version");
    #[cfg(unix)]
    c.env("PATH", crate::runtime::enriched_path());
    c.output().is_ok()
}

pub(crate) fn python_bin() -> Option<String> {
    python_candidates().into_iter().find(|bin| interpreter_ok(bin))
}

pub(crate) fn rscript_bin() -> Option<String> {
    rscript_candidates().into_iter().find(|bin| interpreter_ok(bin))
}

/// Find Python inside a WSL distro (python3 preferred, python fallback).
/// Windows only — non-Windows platforms always return None.
#[cfg(windows)]
pub(crate) fn wsl_python_bin(distro: &str) -> Option<String> {
    if crate::wsl::wsl_check_tool(distro, "python3") {
        Some("python3".into())
    } else if crate::wsl::wsl_check_tool(distro, "python") {
        Some("python".into())
    } else {
        None
    }
}

/// Find Rscript inside a WSL distro.
/// Windows only — non-Windows platforms always return None.
#[cfg(windows)]
pub(crate) fn wsl_rscript_bin(distro: &str) -> Option<String> {
    if crate::wsl::wsl_check_tool(distro, "Rscript") {
        Some("Rscript".into())
    } else {
        None
    }
}

/// Spawn a kernel inside a WSL distro. The bridge script and R code-file paths
/// are mapped from Windows to WSL paths (`/mnt/...`). Working directory is set
/// via wsl.exe's `--cd` flag so the kernel's working dir matches the workspace.
///
/// Windows only — the WSL dispatch in [`spawn_kernel`] is gated by `#[cfg(windows)]`.
#[cfg(windows)]
fn spawn_kernel_wsl(
    app: &AppHandle,
    lang: &str,
    distro: &str,
    workspace: &Path,
    key: &str,
) -> Result<Kernel, String> {
    let wsl_workspace = crate::wsl::windows_to_wsl_path(&workspace.to_string_lossy());
    let (mut cmd, code_file) = match lang {
        "python" => {
            let script = materialize(app, "kernel_bridge.py", PY_BRIDGE_SRC)?;
            let python = wsl_python_bin(distro)
                .ok_or("no Python found in WSL — install python3 in your WSL distro")?;
            let script_wsl = crate::wsl::windows_to_wsl_path(&script.to_string_lossy());
            let mut c = crate::wsl::wsl_command_cwd(distro, &python, &wsl_workspace);
            c.arg(&script_wsl);
            (c, None)
        }
        "r" => {
            let script = materialize(app, "kernel_bridge.R", R_BRIDGE_SRC)?;
            let rscript = wsl_rscript_bin(distro)
                .ok_or("no R found in WSL — install R (r-project.org) in your WSL distro")?;
            // One code file per kernel — concurrent R notebooks must not share it.
            let code_file = kernel_dir(app)?.join(format!("r_cell_{}.R", key_hash(key)));
            std::fs::write(&code_file, "").map_err(|e| e.to_string())?;
            let script_wsl = crate::wsl::windows_to_wsl_path(&script.to_string_lossy());
            let code_file_wsl = crate::wsl::windows_to_wsl_path(&code_file.to_string_lossy());
            let mut c = crate::wsl::wsl_command_cwd(distro, &rscript, &wsl_workspace);
            c.arg(&script_wsl).arg(&code_file_wsl);
            (c, Some(code_file))
        }
        _ => return Err(format!("unsupported kernel language: {lang}")),
    };
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start {lang} kernel in WSL: {e}"))?;
    Kernel::from_child(child, code_file, None)
}

/// Spawn a kernel on a remote host via SSH.
///
/// The bridge script is materialised locally, uploaded to `/tmp/` on the remote,
/// and then run via `ssh <host> <interpreter> <remote_bridge>` with stdin/stdout
/// piped through the SSH connection.  The working directory on the remote is set
/// to the remote home directory (the default SSH directory for commands).
///
/// Windows only — the SSH dispatch in [`spawn_kernel`] is `#[cfg(windows)]`.
#[cfg(windows)]
fn spawn_kernel_ssh(app: &AppHandle, lang: &str, host: &str) -> Result<Kernel, String> {
    if !crate::hpc::is_safe_host(host) {
        return Err("invalid host".into());
    }

    let remote_ws = crate::runtime::remote_workspace_path(host).unwrap_or_default();
    let safe_ws = remote_ws.replace('\'', "'\\''");

    // Materialise the bridge script locally so we can upload it
    let (bridge_name, bridge_src) = match lang {
        "python" => ("kernel_bridge.py", PY_BRIDGE_SRC),
        "r" => ("kernel_bridge.R", R_BRIDGE_SRC),
        _ => return Err(format!("unsupported kernel language: {lang}")),
    };
    let local_bridge = materialize(app, bridge_name, bridge_src)?;
    let bridge_data =
        std::fs::read(&local_bridge).map_err(|e| format!("failed to read bridge: {e}"))?;

    // Create a remote temp directory and upload the bridge
    let remote_dir = format!("/tmp/openscience-kernel-{}", std::process::id());
    crate::remote::ssh_exec(host, &format!("mkdir -p '{}'", remote_dir))?;
    let remote_bridge = format!("{}/{}", remote_dir, bridge_name);
    crate::remote::ssh_write_file(host, &remote_bridge, &bridge_data)?;

    let ssh_opts = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "-o", "StrictHostKeyChecking=accept-new"];

    match lang {
        "python" => {
            let python = if crate::remote::ssh_exec(host, "which python3 2>/dev/null").is_ok() {
                "python3"
            } else if crate::remote::ssh_exec(host, "which python 2>/dev/null").is_ok() {
                "python"
            } else {
                return Err("no Python found on remote host".into());
            };

            let child = std::process::Command::new("ssh")
                .args(ssh_opts)
                .args([
                    "--",
                    host,
                    "sh",
                    "-c",
                    &format!("cd '{}' && {} '{}'", safe_ws, python, remote_bridge),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("failed to start {} kernel via SSH: {e}", lang))?;

            Kernel::from_child(child, None, None)
        }
        "r" => {
            if crate::remote::ssh_exec(host, "which Rscript 2>/dev/null").is_err() {
                return Err("no Rscript found on remote host".into());
            }
            let code_file = format!("{}/r_cell.R", remote_dir);

            let child = std::process::Command::new("ssh")
                .args(ssh_opts)
                .args([
                    "--",
                    host,
                    "sh",
                    "-c",
                    &format!(
                        "cd '{}' && Rscript '{}' '{}'",
                        safe_ws, remote_bridge, code_file
                    ),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("failed to start R kernel via SSH: {e}"))?;

            Kernel::from_child(child, Some(PathBuf::from(&code_file)), Some(host.to_string()))
        }
        _ => unreachable!(),
    }
}

fn spawn_kernel(app: &AppHandle, lang: &str, cwd: &std::path::Path, key: &str) -> Result<Kernel, String> {
    // WSL/SSH backend: dispatch to the appropriate spawner
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(app);
        if config.kind == "ssh" {
            if let Some(ref host) = config.host {
                return spawn_kernel_ssh(app, lang, host);
            }
        } else if config.kind == "wsl" {
            if let Some(ref distro) = config.distro {
                return spawn_kernel_wsl(app, lang, distro, cwd, key);
            }
        }
    }
    let (mut cmd, code_file) = match lang {
        "python" => {
            let script = materialize(app, "kernel_bridge.py", PY_BRIDGE_SRC)?;
            let python =
                python_bin().ok_or("no Python found — install Python 3 to run Python notebooks")?;
            let mut c = Command::new(python);
            c.arg(script);
            (c, None)
        }
        "r" => {
            let script = materialize(app, "kernel_bridge.R", R_BRIDGE_SRC)?;
            let rscript = rscript_bin().ok_or(
                "no R found — install R (r-project.org) to run R notebooks",
            )?;
            // One code file per kernel — concurrent R notebooks must not share it.
            let code_file = kernel_dir(app)?.join(format!("r_cell_{}.R", key_hash(key)));
            std::fs::write(&code_file, "").map_err(|e| e.to_string())?;
            let mut c = Command::new(rscript);
            c.arg(script).arg(&code_file);
            (c, Some(code_file))
        }
        _ => return Err(format!("unsupported kernel language: {lang}")),
    };
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Give the kernel the SAME PATH as the OpenCode sidecar so it runs the
    // user's scientific Python (anaconda/homebrew) — the one the agent uses,
    // with numpy/matplotlib/etc. A Finder-launched app otherwise has a minimal
    // PATH and `python3` resolves to a bare system Python (no matplotlib).
    #[cfg(unix)]
    cmd.env("PATH", crate::runtime::enriched_path());
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to start {lang} kernel: {e}"))?;
    Kernel::from_child(child, code_file, None)
}

/// Send one request to a running kernel and read its single JSON response line.
fn exec_on(k: &mut KernelIo, code: &str) -> Result<ExecResult, String> {
    k.seq += 1;
    match &k.code_file {
        // R: stage the code in the kernel's file, then poke it with the id.
        Some(path) => {
            if let Some(host) = &k.ssh_host {
                crate::remote::ssh_write_file(host, &path.to_string_lossy(), code.as_bytes())?;
            } else {
                std::fs::write(path, code).map_err(|e| format!("kernel write failed: {e}"))?;
            }
            writeln!(k.stdin, "{}", k.seq).map_err(|e| format!("kernel write failed: {e}"))?;
        }
        // Python: send the code inline as JSON.
        None => {
            let req = serde_json::json!({ "id": k.seq.to_string(), "code": code });
            writeln!(k.stdin, "{req}").map_err(|e| format!("kernel write failed: {e}"))?;
        }
    }
    k.stdin
        .flush()
        .map_err(|e| format!("kernel flush failed: {e}"))?;

    let mut line = String::new();
    let n = k
        .stdout
        .read_line(&mut line)
        .map_err(|e| format!("kernel read failed: {e}"))?;
    if n == 0 {
        return Err("kernel exited unexpectedly".into());
    }
    let v: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("bad kernel response: {e}"))?;
    Ok(ExecResult {
        ok: v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
        stdout: v
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        result: v.get("result").and_then(|x| x.as_str()).map(str::to_string),
        error: v.get("error").and_then(|x| x.as_str()).map(str::to_string),
    })
}

/// Kill a kernel's process and reap it — killed children must be `wait()`ed
/// or they linger as zombies. Takes only the `child` lock, which no cell ever
/// holds, so this always proceeds even while a cell is blocked mid-read.
fn reap(kernel: &Kernel) {
    let mut child = kernel.child.lock().unwrap();
    let _ = child.kill();
    let _ = child.wait();
}

/// Get-or-spawn the kernel for `key` and run one cell on it. The map lock is
/// dropped before any cell I/O; each kernel's own `io` lock serializes its
/// cells, so one notebook's hung cell never blocks another notebook or reset.
fn exec_in<F>(state: &KernelState, key: &str, code: &str, spawn: F) -> Result<ExecResult, String>
where
    F: FnOnce() -> Result<Kernel, String>,
{
    let kernel = {
        let mut map = state.0.lock().unwrap();
        match map.get(key) {
            Some(k) => std::sync::Arc::clone(k),
            None => {
                let k = std::sync::Arc::new(spawn()?);
                map.insert(key.to_string(), std::sync::Arc::clone(&k));
                k
            }
        }
    };
    let mut io = kernel.io.lock().unwrap();
    match exec_on(&mut io, code) {
        Ok(r) => Ok(r),
        Err(e) => {
            // The child died — a crash, or a reset killed it mid-cell. Reap it,
            // then drop THIS kernel from the map so the next run respawns —
            // unless a reset already removed (or replaced) the entry.
            reap(&kernel);
            let mut map = state.0.lock().unwrap();
            if map.get(key).is_some_and(|cur| std::sync::Arc::ptr_eq(cur, &kernel)) {
                map.remove(key);
            }
            Err(e)
        }
    }
}

/// Remove every kernel whose key matches `pred` and kill its process. The map
/// lock is released before killing so a hung cell's error path (which also
/// wants the map) can never contend with the kills.
fn remove_kernels(state: &KernelState, pred: impl Fn(&str) -> bool) {
    let victims: Vec<std::sync::Arc<Kernel>> = {
        let mut map = state.0.lock().unwrap();
        let keys: Vec<String> = map.keys().filter(|k| pred(k)).cloned().collect();
        keys.iter().filter_map(|k| map.remove(k)).collect()
    };
    for kernel in &victims {
        reap(kernel);
    }
}

/// Resolve a cell's kernel: its working directory and map key. `notebook` is a
/// `root`-relative .ipynb path; without it the kernel runs in the active
/// workspace (legacy behavior).
fn resolve_kernel(
    app: &AppHandle,
    lang: &str,
    notebook: Option<&str>,
    root: Option<&str>,
) -> Result<(PathBuf, String), String> {
    match notebook.filter(|s| !s.trim().is_empty()) {
        Some(nb) => {
            let scope = crate::artifact_file::scope_root(app, root)?;
            let abs = crate::artifact_file::resolve_under(&scope, nb)?;
            let dir = abs
                .parent()
                .ok_or("notebook has no parent directory")?
                .to_path_buf();
            Ok((dir, kernel_key(lang, Some(&abs))))
        }
        None => Ok((crate::runtime::workspace_dir(app)?, kernel_key(lang, None))),
    }
}

/// Execute one cell in the persistent local kernel for this notebook (Jupyter
/// semantics: one kernel per notebook, working directory = the notebook's
/// folder), starting it on first use.
#[tauri::command]
pub fn kernel_execute(
    app: AppHandle,
    state: State<'_, KernelState>,
    code: String,
    language: Option<String>,
    notebook: Option<String>,
    root: Option<String>,
) -> Result<ExecResult, String> {
    let lang = normalize_lang(language.as_deref().unwrap_or("python"))?;
    let (cwd, key) = resolve_kernel(&app, lang, notebook.as_deref(), root.as_deref())?;
    exec_in(&state, &key, &code, || spawn_kernel(&app, lang, &cwd, &key))
}

/// Restart kernels (clears their state). With `notebook`, exactly that
/// notebook's kernel is killed (the Stop button on a hung cell); with only
/// `language`, every kernel of that language; with neither, everything.
/// Always returns promptly, even while a cell is blocked mid-run.
#[tauri::command]
pub fn kernel_reset(
    app: AppHandle,
    state: State<'_, KernelState>,
    language: Option<String>,
    notebook: Option<String>,
    root: Option<String>,
) -> Result<(), String> {
    if notebook.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        let lang = normalize_lang(language.as_deref().unwrap_or("python"))?;
        let (_, key) = resolve_kernel(&app, lang, notebook.as_deref(), root.as_deref())?;
        remove_kernels(&state, |k| k == key);
        return Ok(());
    }
    match language.as_deref().and_then(|l| normalize_lang(l).ok()) {
        Some(lang) => {
            let prefix = format!("{lang}:");
            remove_kernels(&state, |k| k.starts_with(&prefix));
        }
        None => remove_kernels(&state, |_| true),
    }
    Ok(())
}

pub fn kill_kernel(state: &KernelState) {
    remove_kernels(state, |_| true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(stdout: &mut BufReader<ChildStdout>) -> serde_json::Value {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[test]
    fn kernel_keys_isolate_notebooks_and_languages() {
        let a = std::path::Path::new("/ws/2026-07-05/analysis.ipynb");
        let b = std::path::Path::new("/ws/2026-07-04/analysis.ipynb");
        // Same basename in different folders → different kernels; language
        // is part of the key; the legacy no-notebook key stays stable.
        assert_ne!(kernel_key("python", Some(a)), kernel_key("python", Some(b)));
        assert_ne!(kernel_key("python", Some(a)), kernel_key("r", Some(a)));
        assert_eq!(kernel_key("python", None), "python:@workspace");
        // Distinct keys get distinct R scratch files.
        assert_ne!(
            key_hash(&kernel_key("r", Some(a))),
            key_hash(&kernel_key("r", Some(b)))
        );
    }

    #[test]
    fn normalizes_languages() {
        assert_eq!(normalize_lang("").unwrap(), "python");
        assert_eq!(normalize_lang("Python3").unwrap(), "python");
        assert_eq!(normalize_lang("R").unwrap(), "r");
        assert!(normalize_lang("julia").is_err());
    }

    // THE P0-7 deadlock case: a `while True: pass` cell used to wedge every
    // kernel command — kernel_execute held the global map mutex across an
    // unbounded blocking read, so even kernel_reset waited forever and only an
    // app restart recovered. With per-kernel locks, reset must (a) return
    // promptly, (b) unblock the hung cell with an error, (c) leave the map
    // empty so the next run respawns fresh.
    #[test]
    fn reset_interrupts_a_hung_cell_without_wedging() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let Some(python) = python_bin() else {
            eprintln!("skipping: no python found");
            return;
        };
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../runtime/kernel/kernel_bridge.py");
        let state = Arc::new(KernelState::default());
        let key = "python:@hung-test";

        let exec_state = state.clone();
        let hung = std::thread::spawn(move || {
            exec_in(&exec_state, key, "while True: pass", || {
                let child = Command::new(python)
                    .arg(script)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| e.to_string())?;
                Kernel::from_child(child, None, None)
            })
        });

        // Wait until the kernel is registered (the cell is running), then give
        // it a beat to enter the blocking read.
        let start = Instant::now();
        while state.0.lock().unwrap().is_empty() {
            assert!(start.elapsed() < Duration::from_secs(10), "kernel never registered");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));

        // Reset must not wedge behind the hung cell.
        let reset_state = state.clone();
        let reset = std::thread::spawn(move || remove_kernels(&reset_state, |_| true));
        let start = Instant::now();
        while !reset.is_finished() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "kernel_reset wedged behind a hung cell (the P0-7 deadlock)"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        reset.join().unwrap();

        // The hung execute errors out instead of blocking forever…
        let res = hung.join().unwrap();
        assert!(res.is_err(), "hung cell must surface an error after reset");
        // …and nothing is left behind: next run respawns a fresh kernel.
        assert!(state.0.lock().unwrap().is_empty());
    }

    // Drives the real in-tree Python bridge with the same JSON protocol
    // kernel_execute uses, proving the Rust <-> Python round trip and shared state.
    #[test]
    fn python_round_trips_and_keeps_state() {
        let Some(python) = python_bin() else {
            eprintln!("skipping: no python found");
            return;
        };
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../runtime/kernel/kernel_bridge.py"
        );
        let mut child = Command::new(python)
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bridge");
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());

        writeln!(
            stdin,
            "{}",
            serde_json::json!({"id":"1","code":"a=41\na+1"})
        )
        .unwrap();
        stdin.flush().unwrap();
        let r1 = read(&mut stdout);
        assert_eq!(r1["ok"], true);
        assert_eq!(r1["result"], "42");

        // Second cell sees `a` from the first — persistent shared namespace.
        writeln!(stdin, "{}", serde_json::json!({"id":"2","code":"a"})).unwrap();
        stdin.flush().unwrap();
        let r2 = read(&mut stdout);
        assert_eq!(r2["result"], "41");

        // Errors come back as ok:false with a traceback.
        writeln!(stdin, "{}", serde_json::json!({"id":"3","code":"1/0"})).unwrap();
        stdin.flush().unwrap();
        let r3 = read(&mut stdout);
        assert_eq!(r3["ok"], false);
        assert!(r3["error"].as_str().unwrap().contains("ZeroDivisionError"));

        let _ = child.kill();
    }

    // Same round trip against the real R bridge (skipped if R is not installed):
    // shared state across cells, last-expression value, and error reporting.
    #[test]
    fn r_round_trips_and_keeps_state() {
        let Some(rscript) = rscript_bin() else {
            eprintln!("skipping: no Rscript found");
            return;
        };
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../runtime/kernel/kernel_bridge.R"
        );
        let dir = std::env::temp_dir().join("os_r_kernel_test");
        std::fs::create_dir_all(&dir).unwrap();
        let code_file = dir.join("r_cell.R");
        std::fs::write(&code_file, "").unwrap();
        let mut child = Command::new(rscript)
            .arg(script)
            .arg(&code_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn R bridge");
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut poke = |code: &str, id: &str| {
            std::fs::write(&code_file, code).unwrap();
            writeln!(stdin, "{id}").unwrap();
            stdin.flush().unwrap();
        };

        poke("a <- 41\na + 1", "1");
        let r1 = read(&mut stdout);
        assert_eq!(r1["ok"], true);
        assert_eq!(r1["result"], "[1] 42");

        // `a` persists into the next cell.
        poke("a", "2");
        let r2 = read(&mut stdout);
        assert_eq!(r2["result"], "[1] 41");

        // stdout is captured; the final visible value is the result.
        poke("cat('hi\\n')\n2 + 2", "3");
        let r3 = read(&mut stdout);
        assert_eq!(r3["stdout"].as_str().unwrap().trim(), "hi");
        assert_eq!(r3["result"], "[1] 4");

        // Errors -> ok:false with a message.
        poke("stop('boom')", "4");
        let r4 = read(&mut stdout);
        assert_eq!(r4["ok"], false);
        assert!(r4["error"].as_str().unwrap().contains("boom"));

        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
