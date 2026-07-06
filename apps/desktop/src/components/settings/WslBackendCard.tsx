import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, Loader2 } from "lucide-react";
import {
  checkWslHealth,
  getAvailableBackends,
  getBackendConfig,
  probeRemoteHost,
  saveAndRestartBackend,
  type BackendConfig,
  type RemoteStatus,
} from "@/features/settings/backend-api";
import { isTauri, workspacePath } from "@/lib/tauri";
import { toast } from "@/lib/toast";
import { cn } from "@/lib/cn";

/**
 * Execution Backend card: lets the user pick between native Windows, WSL, and
 * remote SSH hosts.
 *
 * On non-Tauri (browser dev) shows a note that this is a desktop-only feature.
 * On Tauri but non-Windows (macOS, Linux) the backend list contains only
 * "native" — no WSL options will appear. On Windows with WSL, the dropdown
 * includes every detected distro, plus a "SSH (Connect to a remote server)"
 * option that expands into host input and a test-connection probe.
 */
export function WslBackendCard() {
  const { t } = useTranslation();
  const [backends, setBackends] = useState<BackendConfig[]>([]);
  const [currentConfig, setCurrentConfig] = useState<BackendConfig | null>(null);
  const [selected, setSelected] = useState("native");
  const [healthStatus, setHealthStatus] = useState<"idle" | "checking" | "healthy" | "error">("idle");
  const [saving, setSaving] = useState(false);

  // SSH-specific state
  const [sshHost, setSshHost] = useState("");
  const [sshStatus, setSshStatus] = useState<RemoteStatus | null>(null);
  const [sshProbing, setSshProbing] = useState(false);

  const load = useCallback(async () => {
    if (!isTauri) return;
    try {
      const [b, cfg] = await Promise.all([
        getAvailableBackends(),
        getBackendConfig(),
      ]);
      setBackends(b);
      setCurrentConfig(cfg);
      const key = cfg.kind === "wsl" && cfg.distro
        ? `wsl:${cfg.distro}`
        : cfg.kind === "ssh"
          ? "ssh"
          : "native";
      setSelected(key);
      if (cfg.kind === "ssh" && cfg.host) {
        setSshHost(cfg.host);
      }
    } catch {
      /* backend config unavailable — stay at defaults */
    }
  }, []);

  useEffect(() => {
    if (isTauri) void load();
  }, [load]);

  // Run health check whenever the selected option is a WSL distro.
  useEffect(() => {
    const [, distro] = selected.split(":");
    if (!distro) {
      setHealthStatus("idle");
      return;
    }
    setHealthStatus("checking");
    checkWslHealth(distro)
      .then(() => {
        setHealthStatus("healthy");
      })
      .catch(() => {
        setHealthStatus("error");
      });
  }, [selected]);

  // Derived: has the user picked a different backend than what is saved?
  const currentKey =
    currentConfig?.kind === "wsl" && currentConfig.distro
      ? `wsl:${currentConfig.distro}`
      : currentConfig?.kind === "ssh"
        ? "ssh"
        : "native";
  const changed =
    selected !== currentKey ||
    (selected === "ssh" && sshHost.trim() !== (currentConfig?.host ?? ""));

  const wslBackends = backends.filter((b) => b.kind === "wsl");

  const handleProbe = async () => {
    const host = sshHost.trim();
    if (!host) return;
    setSshProbing(true);
    setSshStatus(null);
    try {
      const ws = await workspacePath();
      const status = await probeRemoteHost(host, ws ?? "");
      setSshStatus(status);
      if (!status.reachable) {
        toast.error(`Remote host ${host} is not reachable.`);
      }
    } catch (e) {
      toast.error(
        `Probe failed: ${e instanceof Error ? e.message : String(e)}`,
      );
      setSshStatus({
        reachable: false,
        has_opencode: false,
        has_python3: false,
        has_rscript: false,
        message: String(e),
      });
    } finally {
      setSshProbing(false);
    }
  };

  const handleSave = async () => {
    const [kind, distro] = selected.split(":");
    const host = kind === "ssh" ? sshHost.trim() : undefined;
    if (kind === "ssh" && !host) {
      toast.error("Please enter a remote host before saving.");
      return;
    }
    setSaving(true);
    try {
      await saveAndRestartBackend(
        kind as "native" | "wsl" | "ssh",
        kind === "wsl" ? distro : undefined,
        host,
      );
      toast.success(
        kind === "wsl"
          ? `Switched to WSL (${distro})`
          : kind === "ssh"
            ? `Switched to SSH (${host})`
            : "Switched to Windows (Native)",
      );
      // Re-load so the UI reflects the saved state.
      await load();
    } catch (e) {
      toast.error(
        `Could not switch backend: ${e instanceof Error ? e.message : String(e)}`,
      );
    } finally {
      setSaving(false);
    }
  };

  // ── Browser dev mode ─────────────────────────────────────────────────
  if (!isTauri) {
    return (
      <section className="mt-5 rounded-card border border-border bg-surface shadow-card">
        <header className="border-b border-border px-5 py-3">
          <h2 className="font-serif text-[15px] text-text">{t("settings.executionBackend.title")}</h2>
          <p className="mt-0.5 text-xs text-muted">
            {t("settings.executionBackend.description")}
          </p>
        </header>
        <div className="px-5 py-4">
          <p className="text-[13px] text-muted">{t("errors.desktopOnly")}</p>
        </div>
      </section>
    );
  }

  return (
    <section className="mt-5 rounded-card border border-border bg-surface shadow-card">
      <header className="border-b border-border px-5 py-3">
        <h2 className="font-serif text-[15px] text-text">{t("settings.executionBackend.title")}</h2>
        <p className="mt-0.5 text-xs text-muted">
          {t("settings.executionBackend.descriptionDetailed")}
        </p>
      </header>
      <div className="px-5 py-4">
        {/* ── Backend selector ─────────────────────────────────── */}
        <div className="relative">
          <select
            value={selected}
            onChange={(e) => setSelected(e.target.value)}
            disabled={saving}
            className={cn(inputCls("w-full appearance-none pr-9"), "cursor-pointer")}
          >
            <option value="native">{t("settings.executionBackend.native")}</option>
            {wslBackends.map((b) => (
              <option key={b.distro} value={`wsl:${b.distro}`}>
                {t("settings.executionBackend.wsl", { distro: b.distro })}
              </option>
            ))}
            <option value="ssh">{t("settings.executionBackend.ssh")}</option>
          </select>
          <ChevronDown
            size={14}
            className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 text-muted"
          />
        </div>

        {/* ── WSL health indicator ─────────────────────────────── */}
        {selected.startsWith("wsl") && (
          <div className="mt-3 flex items-center gap-2.5 text-[13px]">
            <span
              className={cn(
                "h-1.5 w-1.5 shrink-0 rounded-full",
                healthStatus === "healthy"
                  ? "bg-ok"
                  : healthStatus === "error"
                    ? "bg-error"
                    : "bg-muted",
              )}
            />
            <span className="font-medium text-text">WSL: {selected.split(":")[1]}</span>
            <span className="truncate text-xs text-muted">
              {healthStatus === "checking"
                ? t("settings.executionBackend.checkingConnectivity")
                : healthStatus === "healthy"
                  ? t("settings.executionBackend.connected")
                  : healthStatus === "error"
                    ? t("settings.executionBackend.notReachable")
                    : ""}
            </span>
          </div>
        )}

        {/* ── SSH configuration ──────────────────────────────── */}
        {selected === "ssh" && (
          <div className="mt-3 space-y-2.5">
            <div className="flex gap-2">
              <input
                type="text"
                placeholder={t("settings.executionBackend.sshHostPlaceholder")}
                value={sshHost}
                onChange={(e) => setSshHost(e.target.value)}
                className={cn(inputCls("flex-1 font-mono"), "font-mono")}
              />
              <button
                className={btnGhost("gap-1.5")}
                onClick={() => void handleProbe()}
                disabled={sshProbing || !sshHost.trim()}
              >
                {sshProbing ? <Loader2 size={12} className="animate-spin" /> : null}
                {sshProbing ? t("settings.executionBackend.probing") : t("settings.executionBackend.testConnection")}
              </button>
            </div>

            {/* Probe results */}
            {sshStatus && (
              <div className="rounded-input border border-border bg-surface-2/50 p-3 text-[13px]">
                <div className="flex items-center gap-2">
                  <span
                    className={cn(
                      "h-1.5 w-1.5 shrink-0 rounded-full",
                      sshStatus.reachable ? "bg-ok" : "bg-error",
                    )}
                  />
                  <span className="font-medium text-text">{t("settings.executionBackend.reachable")}:</span>
                  <span className="text-muted">{sshStatus.reachable ? t("settings.executionBackend.yes") : t("settings.executionBackend.no")}</span>
                </div>
                {sshStatus.reachable && (
                  <>
                    <div className="mt-1.5 flex items-center gap-2">
                      <span
                        className={cn(
                          "h-1.5 w-1.5 shrink-0 rounded-full",
                          sshStatus.has_python3 ? "bg-ok" : "bg-muted",
                        )}
                      />
                      <span className="font-medium text-text">Python 3:</span>
                      <span className="text-muted">{sshStatus.has_python3 ? t("settings.executionBackend.available") : t("settings.executionBackend.notFound")}</span>
                    </div>
                    <div className="mt-1.5 flex items-center gap-2">
                      <span
                        className={cn(
                          "h-1.5 w-1.5 shrink-0 rounded-full",
                          sshStatus.has_rscript ? "bg-ok" : "bg-muted",
                        )}
                      />
                      <span className="font-medium text-text">R:</span>
                      <span className="text-muted">{sshStatus.has_rscript ? t("settings.executionBackend.available") : t("settings.executionBackend.notFound")}</span>
                    </div>
                  </>
                )}
                {sshStatus.message && (
                  <p className="mt-1.5 text-xs text-muted">{sshStatus.message}</p>
                )}
              </div>
            )}
          </div>
        )}

        {/* ── Save & restart prompt ──────────────────────────── */}
        {changed && (
          <div className="mt-3 flex items-center gap-2">
            <button
              className={btnAccent()}
              onClick={() => void handleSave()}
              disabled={saving}
            >
              {saving ? <Loader2 size={12} className="animate-spin" /> : null}
              {saving ? t("settings.executionBackend.restarting") : t("settings.executionBackend.saveRestartRuntime")}
            </button>
            <p className="text-xs text-muted">
              {t("settings.executionBackend.restartHint")}
            </p>
          </div>
        )}
      </div>
    </section>
  );
}

/* ── Shared helpers (same look as SettingsPage / ClusterCard) ── */

const inputCls = (extra = "") =>
  cn(
    "h-9 rounded-input border border-border bg-surface px-3 text-[13px] text-text outline-none",
    "placeholder:text-muted focus:border-accent/60",
    extra,
  );


const btnGhost = (extra = "") =>
  cn(
    "flex h-9 shrink-0 items-center gap-1 rounded-input border border-border bg-surface px-3.5",
    "text-[13px] text-text transition-colors hover:bg-surface-2 disabled:opacity-50",
    extra,
  );

const btnAccent = (extra = "") =>
  cn(
    "flex h-9 shrink-0 items-center gap-1.5 rounded-input bg-accent px-3.5 text-[13px] font-medium",
    "text-accent-fg transition-opacity hover:opacity-90 disabled:opacity-50",
    extra,
  );
