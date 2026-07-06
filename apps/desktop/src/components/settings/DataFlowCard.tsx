import { useTranslation } from "react-i18next";
import { HardDrive, Send } from "lucide-react";

/**
 * Plain-language disclosure of what stays local vs. what is sent to the model
 * provider (P0-2 / P2-3). Every statement here must stay true to the actual
 * architecture — when behavior changes, change this copy in the same commit.
 */
export function DataFlowCard({ model, workspace }: { model: string | null; workspace: string | null }) {
  const { t } = useTranslation();
  return (
    <section className="mt-5 rounded-card border border-border bg-surface shadow-card">
      <header className="border-b border-border px-5 py-3">
        <h2 className="font-serif text-[15px] text-text">{t("settings.privacy.title")}</h2>
        <p className="mt-0.5 text-xs text-muted">
          {t("settings.privacy.description")}
        </p>
      </header>
      <div className="grid gap-5 px-5 py-4 sm:grid-cols-2">
        <div>
          <div className="flex items-center gap-1.5 text-[13px] font-medium text-text">
            <HardDrive size={14} className="text-ok" /> {t("settings.privacy.staysLocal")}
          </div>
          <ul className="mt-2 list-disc space-y-1.5 pl-4 text-[13px] leading-relaxed text-muted">
            <li>
              Your workspace files and raw data
              {workspace && <span className="font-mono text-xs"> ({workspace})</span>}.
            </li>
            <li>Code execution — the Python kernel and Jupyter run locally; datasets are processed here, never uploaded in bulk.</li>
            <li>Session history and provenance records, in the app's private data folder.</li>
            <li>Provider keys and login tokens — an app-private file readable only by your account; never written to the workspace, provenance, logs, or exports.</li>
          </ul>
        </div>
        <div>
          <div className="flex items-center gap-1.5 text-[13px] font-medium text-text">
            <Send size={14} className="text-warn" /> {t("settings.privacy.sentToProvider")}
            <span className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-xs text-muted">
              {model ?? t("settings.model.pickDefault")}
            </span>
          </div>
          <ul className="mt-2 list-disc space-y-1.5 pl-4 text-[13px] leading-relaxed text-muted">
            <li>Your messages, and the file contents / command output the agent reads to do the task you asked for.</li>
            <li>Nothing is sent in the background — data leaves only during a conversation turn.</li>
            <li>What the provider retains is governed by its own data policy.</li>
          </ul>
          <p className="mt-2 text-xs text-muted">
            Skills and MCP servers you add may make their own network calls — review them before installing.
          </p>
        </div>
      </div>
    </section>
  );
}
