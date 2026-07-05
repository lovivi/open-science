// WSL backend API — thin Tauri invoke wrappers.
// In a plain browser these return safe defaults; in the packaged desktop app they
// call Rust commands for WSL detection, health checks, and config persistence.

import { isTauri, startRuntime } from "@/lib/tauri";
import { useRuntimeStore } from "@/lib/runtime";

export interface BackendConfig {
  kind: "native" | "wsl" | "ssh";
  distro: string | null;
  host: string | null;
}

export interface RemoteStatus {
  reachable: boolean;
  has_opencode: boolean;
  has_python3: boolean;
  has_rscript: boolean;
  message: string | null;
}

/** Quick reachability check for a remote SSH host. */
export async function checkRemoteHost(host: string): Promise<boolean> {
  if (!isTauri) throw new Error("not running in the desktop app");
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<boolean>("check_remote_host", { host });
}

/** Full probe of a remote SSH host — reachability plus tool detection. */
export async function probeRemoteHost(host: string, workspace: string): Promise<RemoteStatus> {
  if (!isTauri) throw new Error("not running in the desktop app");
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<RemoteStatus>("probe_remote_host", { host, workspace });
}

/** List available execution backends (native + installed WSL distros + ssh). */
export async function getAvailableBackends(): Promise<BackendConfig[]> {
  if (!isTauri) return [];
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<BackendConfig[]>("get_available_backends");
}

/** Quick health check for a named WSL distro. Returns the distro info string. */
export async function checkWslHealth(distro: string): Promise<string> {
  if (!isTauri) throw new Error("not running in the desktop app");
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<string>("check_wsl_health", { distro });
}

/** Load the current backend config (defaults to native). */
export async function getBackendConfig(): Promise<BackendConfig> {
  if (!isTauri) return { kind: "native", distro: null, host: null };
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<BackendConfig>("get_backend_config");
}

/**
 * Switch to a different execution backend.
 *
 * Persists the selection, stops Jupyter and the current OpenCode sidecar,
 * starts the sidecar under the new backend, and triggers reconnection
 * so the app follows along.
 */
export async function saveAndRestartBackend(
  kind: "native" | "wsl" | "ssh",
  distro?: string,
  host?: string,
): Promise<void> {
  if (!isTauri) throw new Error("not running in the desktop app");
  const { invoke } = await import("@tauri-apps/api/core");
  // 1. Stop Jupyter first (may be running in a different backend's env).
  try { await invoke("stop_jupyter"); } catch { /* not running */ }
  // 2. Persist the selection.
  await invoke("save_backend_config", { kind, distro: distro ?? null, host: host ?? null });
  // 3. Stop the running sidecar so the next start picks up the new backend.
  await invoke("stop_runtime");
  // 4. Start again — the Rust side reads the saved config and dispatches to the
  //    right backend (native or WSL).
  const url = await startRuntime();
  if (url) {
    useRuntimeStore.getState().setServerUrl(url);
    await useRuntimeStore.getState().connectRetry();
  }
}
