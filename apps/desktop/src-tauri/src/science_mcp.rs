// Curated open-source science MCP connectors (P1-2). We do NOT reimplement
// literature/database access — we one-click provision existing open-source MCP
// servers (e.g. paper-search-mcp, biomcp) into a shared ISOLATED uv env under
// app data (the user's Python is untouched), then register them in OpenCode's
// config. The frontend holds the curated catalog; here we just install a pip
// package and report the managed interpreter path.
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;

fn env_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("runtime")
        .join("science-mcp-env"))
}

/// Absolute path to the managed interpreter in the shared science-MCP env.
fn python_bin(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = env_dir(app)?;
    #[cfg(windows)]
    return Ok(dir.join("Scripts").join("python.exe"));
    #[cfg(not(windows))]
    Ok(dir.join("bin").join("python"))
}

/// The managed interpreter path if the shared env exists, else None. The
/// frontend derives launch commands (`<python> -m <module> …`) from this.
///
/// WSL mode: returns `"python3"` (resolved via PATH inside the WSL distro).
/// SSH mode: returns `"python3"` (resolved via remote SSH PATH).
#[tauri::command]
pub fn science_mcp_python(app: AppHandle) -> Result<Option<String>, String> {
    // WSL mode: pip packages are installed into the WSL system Python, so the
    // command is simply "python3" resolved through the WSL PATH.
    #[cfg(windows)]
    {
        let config = crate::runtime::load_backend_config(&app);
        if config.kind == "wsl" {
            if let Some(ref distro) = config.distro {
                return Ok(
                    crate::wsl::wsl_check_tool(distro, "python3").then(|| "python3".to_string())
                );
            }
        }
        // SSH mode: check for python3 on the remote host
        if config.kind == "ssh" {
            if let Some(ref host) = config.host {
                return Ok(
                    crate::remote::ssh_exec(host, "which python3 2>/dev/null")
                        .is_ok()
                        .then(|| "python3".to_string())
                );
            }
        }
    }
    let py = python_bin(&app)?;
    Ok(py.exists().then(|| py.to_string_lossy().to_string()))
}

/// Provision one open-source MCP package into the shared isolated env with the
/// bundled uv (creating the env on first use), and return the managed Python
/// path to launch it with. First run downloads a managed Python (~tens of MB);
/// installing a package is incremental. Async so the UI stays responsive.
#[tauri::command]
pub async fn setup_science_mcp(app: AppHandle, package: String) -> Result<String, String> {
    // Guard against a caller sending an arbitrary spec (flags, extra args).
    if !is_safe_package(&package) {
        return Err("invalid package name".into());
    }
    let dir = env_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // WSL mode: pip3 install inside the distro (no uv sidecar needed).
    // Packages installed into the WSL system Python — jupyter-mcp-server then
    // runs via `python3 -m <module>` when invoked by the OpenCode sidecar.
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
                crate::remote::ssh_exec(host, &format!("pip3 install {}", package))?;
                return Ok("python3".to_string());
            }
        } else if config.kind == "wsl" {
            if let Some(ref distro) = config.distro {
                if !crate::wsl::wsl_check_tool(distro, "pip3") {
                    return Err(
                        "pip3 is not available in WSL — install python3-pip in your distro".into(),
                    );
                }
                crate::wsl::wsl_exec(distro, "pip3", &["install", &package])?;
                return Ok("python3".to_string());
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

    let py = python_bin(&app)?;
    let install = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("uv sidecar not found: {e}"))?
        .args([
            "pip",
            "install",
            "--python",
            &py.to_string_lossy(),
            &package,
        ])
        .output()
        .await
        .map_err(|e| format!("uv pip install failed to run: {e}"))?;
    if !install.status.success() {
        return Err(format!(
            "uv pip install failed: {}",
            String::from_utf8_lossy(&install.stderr)
        ));
    }
    Ok(py.to_string_lossy().to_string())
}

/// A PyPI package name (letters/digits/._-), optionally pinned with `==<version>`.
/// Rejects anything that could smuggle extra pip args or shell metacharacters.
fn is_safe_package(pkg: &str) -> bool {
    let core = pkg.split_once("==").map(|(n, _)| n).unwrap_or(pkg);
    !core.is_empty()
        && !core.starts_with('-')
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && pkg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '='))
}

#[cfg(test)]
mod tests {
    use super::is_safe_package;

    #[test]
    fn accepts_real_package_names_and_pins() {
        assert!(is_safe_package("paper-search-mcp"));
        assert!(is_safe_package("biomcp-python"));
        assert!(is_safe_package("jupyter-mcp-server==0.14.0"));
    }

    #[test]
    fn rejects_flag_and_metacharacter_injection() {
        assert!(!is_safe_package(""));
        assert!(!is_safe_package("--upgrade"));
        assert!(!is_safe_package("pkg; rm -rf /"));
        assert!(!is_safe_package("pkg && echo"));
        assert!(!is_safe_package("pkg --index-url http://evil"));
        assert!(!is_safe_package("pkg\nother"));
    }
}
