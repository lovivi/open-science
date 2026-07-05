// SSH remote workspace core module.
//
// Provides SSH tunnel management, remote file operations, workspace probing,
// and security validation for connecting to remote Linux servers over SSH.
//
// Uses the system `ssh` binary via `std::process::Command` (zero extra crate
// dependencies). The user's own SSH config, keys, and `~/.ssh/known_hosts`
// are used — no credentials are stored by the app.
//
// **Windows only** — non-Windows stubs return safe defaults.

use serde::{Deserialize, Serialize};

// ── Types ──────────────────────────────────────────────────────────────────

/// Result of probing a remote host for available tools and reachability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStatus {
    pub reachable: bool,
    pub has_opencode: bool,
    pub has_python3: bool,
    pub has_rscript: bool,
    pub message: Option<String>,
}

// ── Windows implementation ─────────────────────────────────────────────────

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::Write;

    // ── Internal helpers ───────────────────────────────────────────────

    /// Raw SSH invocation returning (exit_code, stdout, stderr).
    ///
    /// All SSH operations go through this helper to ensure consistent flags
    /// and avoid duplicating the safety check.
    pub(super) fn ssh_exec_raw(host: &str, args: &[&str]) -> Result<(i32, String, String), String> {
        if !crate::hpc::is_safe_host(host) {
            return Err("invalid host".into());
        }
        let output = std::process::Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=8",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "--",
                host,
            ])
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("ssh failed: {e}"))?;
        Ok((
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }

    // ── 2a. SSH tunnel management ──────────────────────────────────────

    /// Establish an SSH port-forwarding tunnel:
    ///
    ///   ssh -L <local_port>:127.0.0.1:<remote_port> -N <host>
    ///
    /// The `-N` flag tells SSH not to execute a remote command — only forward
    /// ports. The caller keeps the returned `Child` handle alive for the
    /// lifetime of the tunnel.
    pub fn ssh_tunnel(
        host: &str,
        local_port: u16,
        remote_port: u16,
    ) -> Result<std::process::Child, String> {
        if !crate::hpc::is_safe_host(host) {
            return Err("invalid host".into());
        }
        let child = std::process::Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-L",
                &format!("{local_port}:127.0.0.1:{remote_port}"),
                "-N",
                "--",
                host,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("ssh tunnel failed to spawn: {e}"))?;
        Ok(child)
    }

    /// Terminate an SSH tunnel process.
    ///
    /// Sends SIGKILL / TerminateProcess and reaps the child so it does not
    /// become a zombie.
    pub fn kill_tunnel(child: &mut std::process::Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    // ── 2b. Remote file operations ─────────────────────────────────────

    /// Read the entire content of a remote text file.
    ///
    /// Under the hood:
    ///   ssh <host> cat <path>
    pub fn ssh_read_file(host: &str, path: &str) -> Result<String, String> {
        let (code, stdout, stderr) = ssh_exec_raw(host, &["cat", path])?;
        if code == 0 {
            Ok(stdout)
        } else {
            let msg = stderr.trim();
            if msg.is_empty() {
                Err(format!(
                    "reading remote file '{}' failed (exit {})",
                    path, code
                ))
            } else {
                Err(msg.to_string())
            }
        }
    }

    /// Read a remote file as raw bytes (binary-safe).
    ///
    /// Captures stdout bytes directly from the SSH process without any
    /// text encoding — preserves arbitrary binary content.
    ///
    /// Under the hood:
    ///   ssh <host> cat <path>
    pub fn ssh_read_file_binary(host: &str, path: &str) -> Result<Vec<u8>, String> {
        if !crate::hpc::is_safe_host(host) {
            return Err("invalid host".into());
        }
        let output = std::process::Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=8",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "--",
                host,
                "cat",
                path,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("ssh failed: {e}"))?;

        if output.status.success() {
            Ok(output.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = stderr.trim().to_string();
            if msg.is_empty() {
                Err(format!(
                    "reading remote binary file '{}' failed (exit {})",
                    path,
                    output.status.code().unwrap_or(-1)
                ))
            } else {
                Err(msg)
            }
        }
    }

    /// Write data to a remote file, creating parent directories as needed.
    ///
    /// The data is piped through the SSH process's stdin — no temporary
    /// files on either side. Works for both text and binary content.
    ///
    /// Under the hood:
    ///   ssh <host> "mkdir -p $(dirname <path>) && cat > <path>"
    pub fn ssh_write_file(host: &str, path: &str, data: &[u8]) -> Result<(), String> {
        if !crate::hpc::is_safe_host(host) {
            return Err("invalid host".into());
        }

        // Create parent directory first
        let quoted_path = path.replace('\'', "'\\''");
        let parent_cmd = format!("mkdir -p '{}'", quoted_path);
        let (_pcode, _pstdout, _pstderr) = ssh_exec_raw(host, &["sh", "-c", &parent_cmd])?;

        // Write file content via stdin to remote `cat > path`
        let mut child = std::process::Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "--",
                host,
                "sh",
                "-c",
                &format!("cat > '{}'", quoted_path),
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("ssh write failed: {e}"))?;

        // Pipe data to the remote stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(data)
                .map_err(|e| format!("write data to ssh stdin: {e}"))?;
        }
        // stdin is dropped here → EOF sent to the remote side

        let output = child
            .wait_with_output()
            .map_err(|e| format!("ssh write wait: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = stderr.trim();
            if msg.is_empty() {
                Err(format!("writing remote file '{}' failed", path))
            } else {
                Err(msg.to_string())
            }
        }
    }

    /// List the entries in a remote directory (one per line).
    ///
    /// Under the hood:
    ///   ssh <host> "ls -1 <path> 2>/dev/null"
    pub fn ssh_list_dir(host: &str, path: &str) -> Result<Vec<String>, String> {
        let cmd = format!("ls -1 '{}' 2>/dev/null", path.replace('\'', "'\\''"));
        let (code, stdout, _stderr) = ssh_exec_raw(host, &["sh", "-c", &cmd])?;
        if code != 0 {
            return Err(format!("listing remote directory '{}' failed", path));
        }
        Ok(stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// Check whether a remote file exists.
    ///
    /// Under the hood:
    ///   ssh <host> "test -f <path> && echo yes || echo no"
    pub fn ssh_path_exists(host: &str, path: &str) -> bool {
        let cmd = format!(
            "test -f '{}' && echo yes || echo no",
            path.replace('\'', "'\\''")
        );
        match ssh_exec_raw(host, &["sh", "-c", &cmd]) {
            Ok((_code, stdout, _stderr)) => stdout.trim() == "yes",
            Err(_) => false,
        }
    }

    /// Execute a shell command on the remote host and return its stdout.
    ///
    /// Under the hood:
    ///   ssh <host> sh -c "<cmd>"
    pub fn ssh_exec(host: &str, cmd: &str) -> Result<String, String> {
        let (code, stdout, stderr) = ssh_exec_raw(host, &["sh", "-c", cmd])?;
        if code == 0 {
            Ok(stdout)
        } else {
            let msg = stderr.trim();
            if msg.is_empty() {
                Err(format!("remote command exited with code {}", code))
            } else {
                Err(msg.to_string())
            }
        }
    }

    /// Quick reachability check: can we SSH to the host?
    ///
    /// Runs `ssh <host> echo ok` and checks for a zero exit code.
    pub fn check_reachable(host: &str) -> Result<bool, String> {
        let (code, stdout, _stderr) = ssh_exec_raw(host, &["echo", "ok"])?;
        Ok(code == 0 && stdout.trim() == "ok")
    }

    // ── 2c. Remote workspace probing ───────────────────────────────────

    /// Probe a remote host for reachability and available tools.
    ///
    /// Checks in order:
    /// 1. Can we SSH to the host at all?
    /// 2. Is `opencode` available on the PATH?
    /// 3. Is `python3` available on the PATH?
    /// 4. Is `Rscript` available on the PATH?
    pub fn probe_remote(host: &str, _workspace: &str) -> RemoteStatus {
        // Step 1: basic connectivity
        let reachable = ssh_exec_raw(host, &["echo", "ok"])
            .ok()
            .map(|(c, s, _)| c == 0 && s.trim() == "ok")
            .unwrap_or(false);

        if !reachable {
            return RemoteStatus {
                reachable: false,
                has_opencode: false,
                has_python3: false,
                has_rscript: false,
                message: Some("host is not reachable over SSH".into()),
            };
        }

        // Steps 2-4: tool detection
        let has_opencode = ssh_exec(host, "which opencode 2>/dev/null").is_ok();
        let has_python3 = ssh_exec(host, "which python3 2>/dev/null").is_ok()
            || ssh_exec(host, "which python 2>/dev/null").is_ok();
        let has_rscript = ssh_exec(host, "which Rscript 2>/dev/null").is_ok();

        RemoteStatus {
            reachable: true,
            has_opencode,
            has_python3,
            has_rscript,
            message: None,
        }
    }
}

// ── Non-Windows stubs ───────────────────────────────────────────────────────

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn ssh_tunnel(
        _host: &str,
        _local_port: u16,
        _remote_port: u16,
    ) -> Result<std::process::Child, String> {
        Err("SSH remote is only supported on Windows".into())
    }

    pub fn kill_tunnel(_child: &mut std::process::Child) {}

    pub fn ssh_read_file(_host: &str, _path: &str) -> Result<String, String> {
        Err("SSH remote is only supported on Windows".into())
    }

    pub fn ssh_read_file_binary(_host: &str, _path: &str) -> Result<Vec<u8>, String> {
        Err("SSH remote is only supported on Windows".into())
    }

    pub fn ssh_write_file(_host: &str, _path: &str, _data: &[u8]) -> Result<(), String> {
        Err("SSH remote is only supported on Windows".into())
    }

    pub fn ssh_list_dir(_host: &str, _path: &str) -> Result<Vec<String>, String> {
        Err("SSH remote is only supported on Windows".into())
    }

    pub fn ssh_path_exists(_host: &str, _path: &str) -> bool {
        false
    }

    pub fn ssh_exec(_host: &str, _cmd: &str) -> Result<String, String> {
        Err("SSH remote is only supported on Windows".into())
    }

    pub fn check_reachable(_host: &str) -> Result<bool, String> {
        Err("SSH remote is only supported on Windows".into())
    }

    pub fn probe_remote(_host: &str, _workspace: &str) -> RemoteStatus {
        RemoteStatus {
            reachable: false,
            has_opencode: false,
            has_python3: false,
            has_rscript: false,
            message: Some("SSH remote is only supported on Windows".into()),
        }
    }
}

pub use imp::*;

// ── Tauri commands ──────────────────────────────────────────────────────────

/// Probe a remote host for reachability and available scientific tools.
///
/// Called from the Settings page (SSH backend configuration panel) to let
/// the user verify connectivity before switching to SSH mode.
#[tauri::command]
pub fn probe_remote_host(host: String, workspace: String) -> RemoteStatus {
    imp::probe_remote(&host, &workspace)
}

/// Quick connectivity check: is the given SSH host reachable?
///
/// Returns `true` when the host answers with a clean "echo ok".
#[tauri::command]
pub fn check_remote_host(host: String) -> Result<bool, String> {
    imp::check_reachable(&host)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// On non-Windows, all stub functions return sensible defaults/errors.
    #[cfg(not(windows))]
    #[test]
    fn remote_stubs_return_defaults() {
        assert!(!ssh_path_exists("host", "/path"));
        assert!(ssh_exec("host", "echo ok").is_err());
        assert!(ssh_read_file("host", "/path").is_err());
        assert!(ssh_write_file("host", "/path", b"data").is_err());
        assert!(ssh_list_dir("host", "/path").is_err());
        assert!(ssh_read_file_binary("host", "/path").is_err());
        assert!(ssh_tunnel("host", 8080, 22).is_err());
        assert!(check_reachable("host").is_err());

        let status = probe_remote("host", "/workspace");
        assert!(!status.reachable);
        assert!(!status.has_opencode);
        assert!(!status.has_python3);
        assert!(!status.has_rscript);
        assert!(status.message.is_some());
    }

    /// The tunnel stub's kill_tunnel is a no-op (tests it does not panic).
    #[cfg(not(windows))]
    #[test]
    fn kill_tunnel_stub_noop() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        kill_tunnel(&mut child);
    }

    /// RemoteStatus structure is well-formed on both platforms.
    #[test]
    fn remote_status_default_fields() {
        let s = probe_remote("nonexistent.invalid", "/tmp");
        assert!(!s.reachable);
        assert!(!s.has_opencode);
    }
}
