// WSL (Windows Subsystem for Linux) backend support.
//
// Provides detection, command construction, path mapping, and utility
// functions for running OpenCode and other tools inside WSL2.
//
// **Windows only** — non-Windows stubs return safe defaults.

use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Types ──────────────────────────────────────────────────────────────────

/// The execution backend the desktop shell is configured to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionBackend {
    Native,
    Wsl { distro: String },
    Ssh { host: String },
}

/// Serialisable form of the backend selection, stored in settings JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub kind: String,
    pub distro: Option<String>,
    /// SSH host (e.g. "user@host"). `#[serde(default)]` for backward
    /// compatibility with configs written before this field existed.
    #[serde(default)]
    pub host: Option<String>,
}

// ── Path mapping (platform-independent pure functions) ──────────────────────

/// Convert a Windows path to a WSL Linux path.
///
/// `C:\Users\foo\Documents` → `/mnt/c/Users/foo/Documents`
/// `D:\data\file.txt`       → `/mnt/d/data/file.txt`
///
/// Non-Windows paths are returned unchanged.
pub fn windows_to_wsl_path(windows_path: &str) -> String {
    let path = windows_path.trim();
    // Drive letter followed by colon?  e.g. "C:\..."
    if path.len() < 2 || path.as_bytes()[1] != b':' {
        return path.to_string();
    }
    let drive = path[..1].to_ascii_lowercase();
    // Everything after "C:"  →  \Users\...
    let rest = path[2..].replace('\\', "/");
    format!("/mnt/{}{}", drive, rest)
}

/// Convert a WSL Linux path to a Windows path.
///
/// `/mnt/c/Users/foo/Documents` → `C:\Users\foo\Documents`
/// `/home/user/file`            → `/home/user/file` (unchanged — native path)
pub fn wsl_to_windows_path(wsl_path: &str) -> String {
    let path = wsl_path.trim();
    if let Some(rest) = path.strip_prefix("/mnt/") {
        // /mnt/c/Users/...  (drive letter + content after /)
        // /mnt/c            (bare drive, no trailing slash)
        if rest.len() >= 2 && rest.as_bytes()[1] == b'/' {
            let drive = rest[..1].to_ascii_uppercase();
            let rest_path = rest[2..].replace('/', "\\");
            if rest_path.is_empty() {
                return format!("{}:\\", drive);
            }
            return format!("{}:\\{}", drive, rest_path);
        }
        // Bare drive:  /mnt/c  →  C:\
        if rest.len() == 1 && rest.as_bytes()[0].is_ascii_alphabetic() {
            return format!("{}:\\", rest.to_ascii_uppercase());
        }
    }
    path.to_string()
}

// ── Windows implementations ─────────────────────────────────────────────────

#[cfg(windows)]
mod imp {
    use super::*;
    use std::net::TcpStream;
    use std::time::Duration;

    /// Check whether `wsl.exe` is available on this system.
    pub fn wsl_available() -> bool {
        std::process::Command::new("wsl.exe")
            .arg("--status")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// List installed WSL distros, filtering out docker-desktop entries.
    pub fn list_distros() -> Vec<String> {
        let out = std::process::Command::new("wsl.exe")
            .args(["--list", "--quiet"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        out.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.contains("docker-desktop"))
            .collect()
    }

    /// Probe a WSL distro's IP address.
    ///
    /// Strategy 1: `hostname -I` (preferred — returns the primary IP).
    /// Strategy 2: `ip -4 addr show eth0` (fallback for minimal systems).
    pub fn probe_wsl_ip(distro: &str) -> Option<String> {
        // Strategy 1: hostname -I
        if let Ok(out) = wsl_exec(distro, "hostname", &["-I"]) {
            if let Some(ip) = out.split_whitespace().next() {
                let ip = ip.trim().to_string();
                if !ip.is_empty() && !ip.starts_with("fe80") && !ip.starts_with("127.") {
                    return Some(ip);
                }
            }
        }
        // Strategy 2: ip -4 addr show eth0
        let script = r#"ip -4 addr show eth0 2>/dev/null | awk '/inet /{print $2}' | cut -d/ -f1"#;
        if let Ok(out) = wsl_exec(distro, "sh", &["-c", script]) {
            if let Some(ip) = out.lines().next() {
                let ip = ip.trim().to_string();
                if !ip.is_empty() && !ip.starts_with("fe80") && !ip.starts_with("127.") {
                    return Some(ip);
                }
            }
        }
        None
    }

    /// Quick health check: is the WSL distro reachable?
    pub fn wsl_health_check(distro: &str) -> Result<String, String> {
        let out = std::process::Command::new("wsl.exe")
            .args(["--distribution", distro, "--", "echo", "ok"])
            .output()
            .map_err(|e| format!("WSL health check failed: {e}"))?;
        if out.status.success() {
            Ok("healthy".into())
        } else {
            Err(format!("WSL distro '{distro}' is not responding"))
        }
    }

    /// Build a native `std::process::Command` that runs a program in the given
    /// WSL distro (for one-shot operations).
    pub fn wsl_command(distro: &str, program: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.args(["--distribution", distro, "--", program]);
        cmd
    }

    /// Build a native `std::process::Command` that runs a program in the given
    /// WSL distro with the working directory set via wsl.exe's `--cd` flag.
    pub fn wsl_command_cwd(distro: &str, program: &str, cwd: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.args(["--cd", cwd, "--distribution", distro, "--", program]);
        cmd
    }

    /// Return the argument vector for a `tauri_plugin_shell` command that
    /// runs `program` inside the given WSL distro.
    ///
    /// Usage: `app.shell().command("wsl.exe").args(wsl_sidecar_args(d, p))`
    pub fn wsl_sidecar_command(distro: &str, program: &str) -> Vec<String> {
        vec![
            "--distribution".into(),
            distro.into(),
            "--".into(),
            program.into(),
        ]
    }

    /// Execute one command inside a WSL distro and return its stdout.
    ///
    /// Equivalent to:
    ///   wsl.exe --distribution <distro> -- <program> [args…]
    pub fn wsl_exec(distro: &str, program: &str, args: &[&str]) -> Result<String, String> {
        let output = std::process::Command::new("wsl.exe")
            .args(["--distribution", distro, "--", program])
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("wsl exec on '{distro}' failed: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "wsl command '{program}' in distro '{distro}' failed: {}",
                stderr.trim()
            ))
        }
    }

    /// Check whether a given tool/executable exists inside the WSL distro.
    pub fn wsl_check_tool(distro: &str, tool: &str) -> bool {
        match tool {
            "python3" | "python" => {
                wsl_exec(distro, "which", &["python3"]).is_ok()
                    || wsl_exec(distro, "which", &["python"]).is_ok()
            }
            _ => wsl_exec(distro, "which", &[tool]).is_ok(),
        }
    }

    /// Copy a file from the Windows filesystem into a WSL distro.
    ///
    /// The destination path must be a WSL Linux path (e.g. `/home/user/.local/bin/opencode`).
    /// Creates the parent directory and sets the executable bit.
    pub fn copy_to_wsl(distro: &str, src: &Path, dst: &Path) -> Result<(), String> {
        let data = std::fs::read(src).map_err(|e| format!("read '{}': {e}", src.display()))?;
        let dst_str = dst.to_string_lossy();
        let parent = dst
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Create parent directory inside WSL
        wsl_exec(distro, "mkdir", &["-p", &parent])?;

        // Stage via a Windows temp file that WSL can reach through /mnt/
        let temp_dir = std::env::temp_dir();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_file = temp_dir.join(format!("os_wsl_copy_{}_{}", std::process::id(), ts));
        std::fs::write(&temp_file, &data).map_err(|e| format!("write temp: {e}"))?;

        let wsl_temp = super::windows_to_wsl_path(&temp_file.to_string_lossy());
        wsl_exec(distro, "cp", &[&wsl_temp, &dst_str])?;
        wsl_exec(distro, "chmod", &["+x", &dst_str])?;

        let _ = std::fs::remove_file(&temp_file);
        Ok(())
    }

    /// Discover the reachable OpenCode URL for a WSL-side `opencode serve`.
    ///
    /// Returns `(url, strategy_name)` where `strategy_name` is one of:
    /// - `"127.0.0.1"` — port-forwarded by WSL2's default NAT
    /// - `"wsl_ip"`    — direct connection to the WSL distro's IP
    /// - `"wsl_curl"`  — confirmed running inside WSL (WSL IP used for URL)
    pub fn discover_opencode_url_impl(distro: &str, port: u16) -> Result<(String, String), String> {
        // Strategy A: 127.0.0.1 (WSL2 default NAT port forwarding)
        let addr = format!("127.0.0.1:{port}");
        if TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500)).is_ok() {
            return Ok((format!("http://{addr}"), "127.0.0.1".into()));
        }

        // Strategy B: Direct WSL IP
        if let Some(ip) = probe_wsl_ip(distro) {
            let addr = format!("{ip}:{port}");
            if TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500))
                .is_ok()
            {
                return Ok((format!("http://{addr}"), "wsl_ip".into()));
            }
        }

        // Strategy C: curl from inside WSL (probes localhost within the distro)
        let out = wsl_exec(
            distro,
            "sh",
            &[
                "-c",
                &format!(
                    "curl -s -o /dev/null -w '%{{http_code}}' http://localhost:{port} 2>/dev/null || echo 'fail'"
                ),
            ],
        )?;
        if out.trim() == "200" {
            // Use WSL IP for the URL so the Windows frontend can reach it
            if let Some(ip) = probe_wsl_ip(distro) {
                return Ok((format!("http://{ip}:{port}"), "wsl_ip".into()));
            }
            return Ok((format!("http://127.0.0.1:{port}"), "wsl_curl".into()));
        }

        Err(format!(
            "cannot reach OpenCode on WSL distro '{distro}' at port {port}"
        ))
    }
}

// ── Non-Windows stubs ───────────────────────────────────────────────────────

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn wsl_available() -> bool {
        false
    }

    pub fn list_distros() -> Vec<String> {
        vec![]
    }

    pub fn probe_wsl_ip(_distro: &str) -> Option<String> {
        None
    }

    pub fn wsl_health_check(_distro: &str) -> Result<String, String> {
        Err("WSL is only supported on Windows".into())
    }

    pub fn wsl_command(_distro: &str, _program: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("false");
        cmd
    }

    pub fn wsl_command_cwd(_distro: &str, _program: &str, _cwd: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("false");
        cmd
    }

    pub fn wsl_sidecar_command(_distro: &str, _program: &str) -> Vec<String> {
        vec![]
    }

    pub fn wsl_exec(_distro: &str, _program: &str, _args: &[&str]) -> Result<String, String> {
        Err("WSL is only supported on Windows".into())
    }

    pub fn wsl_check_tool(_distro: &str, _tool: &str) -> bool {
        false
    }

    pub fn copy_to_wsl(_distro: &str, _src: &Path, _dst: &Path) -> Result<(), String> {
        Err("WSL is only supported on Windows".into())
    }

    pub fn discover_opencode_url_impl(
        _distro: &str,
        _port: u16,
    ) -> Result<(String, String), String> {
        Err("WSL is only supported on Windows".into())
    }
}

pub use imp::*;

// ── Tauri commands ──────────────────────────────────────────────────────────

/// Return the list of available execution backends (native + installed WSL distros).
#[tauri::command]
pub fn get_available_backends() -> Vec<BackendConfig> {
    let mut backends = vec![BackendConfig {
        kind: "native".into(),
        distro: None,
        host: None,
    }];
    #[cfg(windows)]
    {
        if imp::wsl_available() {
            for d in imp::list_distros() {
                backends.push(BackendConfig {
                    kind: "wsl".into(),
                    distro: Some(d),
                    host: None,
                });
            }
        }
    }

    // Always offer SSH as a configurable backend (no probing — user enters host manually)
    backends.push(BackendConfig {
        kind: "ssh".into(),
        distro: None,
        host: None,
    });
    backends
}

/// Quick health check for a named WSL distro.
#[tauri::command]
pub fn check_wsl_health(distro: String) -> Result<String, String> {
    wsl_health_check(&distro)
}

/// Discover the reachable OpenCode URL for a WSL-side `opencode serve`.
///
/// Returns `(url, strategy)` on success.
#[tauri::command]
pub fn discover_opencode_url(distro: String, port: u16) -> Result<(String, String), String> {
    imp::discover_opencode_url_impl(&distro, port)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Path mapping tests (platform-independent) ────────────────────────

    #[test]
    fn windows_to_wsl_converts_full_path() {
        assert_eq!(
            windows_to_wsl_path(r"C:\Users\test\Documents"),
            "/mnt/c/Users/test/Documents"
        );
        assert_eq!(
            windows_to_wsl_path(r"D:\data\file.txt"),
            "/mnt/d/data/file.txt"
        );
        assert_eq!(windows_to_wsl_path(r"C:\"), "/mnt/c/");
    }

    #[test]
    fn wsl_to_windows_converts_full_path() {
        assert_eq!(
            wsl_to_windows_path("/mnt/c/Users/test/Documents"),
            r"C:\Users\test\Documents"
        );
        assert_eq!(
            wsl_to_windows_path("/mnt/d/data/file.txt"),
            r"D:\data\file.txt"
        );
        assert_eq!(wsl_to_windows_path("/mnt/c/"), r"C:\");
    }

    #[test]
    fn path_round_trip() {
        let inputs = [
            r"C:\Users\test\Documents",
            r"D:\data\analysis\results.csv",
            r"C:\temp",
            r"E:\projects\open-science",
        ];
        for input in &inputs {
            let wsl = windows_to_wsl_path(input);
            let back = wsl_to_windows_path(&wsl);
            let norm = |s: &str| {
                s.trim_end_matches('\\')
                    .trim_end_matches('/')
                    .to_lowercase()
            };
            assert_eq!(
                norm(&back),
                norm(input),
                "round-trip failed for '{input}' → '{wsl}' → '{back}'"
            );
        }
    }

    #[test]
    fn non_wsl_paths_unchanged() {
        assert_eq!(wsl_to_windows_path("/home/user/file"), "/home/user/file");
        assert_eq!(wsl_to_windows_path("/var/log/syslog"), "/var/log/syslog");
        assert_eq!(wsl_to_windows_path(""), "");
    }

    #[test]
    fn windows_path_without_drive_left_unchanged() {
        // A path that doesn't fit the C:\ pattern
        assert_eq!(
            windows_to_wsl_path(r"\\server\share\file"),
            r"\\server\share\file"
        );
        assert_eq!(windows_to_wsl_path("relative\\path"), "relative\\path");
    }

    #[test]
    fn mnt_path_without_drive_unchanged() {
        assert_eq!(wsl_to_windows_path("/mnt/"), "/mnt/");
        assert_eq!(
            wsl_to_windows_path("/mnt/something/else"),
            "/mnt/something/else"
        );
    }

    #[test]
    fn path_empty_and_edge_cases() {
        assert_eq!(windows_to_wsl_path(""), "");
        assert_eq!(wsl_to_windows_path(""), "");
        // Drive letter only -> /mnt/c (no trailing slash for bare drive)
        assert_eq!(windows_to_wsl_path("C:"), "/mnt/c");
        assert_eq!(wsl_to_windows_path("/mnt/x/"), "X:\\");
    }

    // ── Platform-specific stubs / real implementation tests ──────────────

    #[cfg(not(windows))]
    #[test]
    fn wsl_stubs_return_defaults() {
        assert!(!wsl_available());
        assert!(list_distros().is_empty());
        assert!(probe_wsl_ip("Ubuntu").is_none());
        assert!(wsl_health_check("Ubuntu").is_err());
        assert!(wsl_exec("Ubuntu", "echo", &["hi"]).is_err());
        assert!(!wsl_check_tool("Ubuntu", "python3"));
        assert!(discover_opencode_url("Ubuntu".to_string(), 8080).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_to_wsl_handles_lowercase_drive() {
        assert_eq!(
            windows_to_wsl_path(r"c:\program files"),
            "/mnt/c/program files"
        );
    }

    #[cfg(windows)]
    #[test]
    fn wsl_to_windows_handles_inconsistent_separators() {
        // WSL paths always use /, but the conversion should produce \
        assert_eq!(wsl_to_windows_path("/mnt/c/Users/test"), r"C:\Users\test");
    }
}
