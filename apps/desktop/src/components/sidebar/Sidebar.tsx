import { useState } from "react";
import { NavLink, useLocation, useNavigate } from "react-router-dom";
import { Files, FolderTree, NotebookPen, Plus, Settings, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { Project } from "@ai4s/shared";
import { cn } from "@/lib/cn";
import { isTauri } from "@/lib/tauri";
import { useRuntimeStore } from "@/lib/runtime";
import { StatusPills } from "./StatusPills";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import logo from "@/assets/logo.webp";

interface Row {
  id: string;
  title: string;
  to: string;
  kind: "session" | "example";
}

export function Sidebar({ project }: { project: Project }) {
  const navigate = useNavigate();
  const location = useLocation();
  const { t } = useTranslation();
  const { sessions, hiddenExamples, startDraft, deleteSession, hideExample } = useRuntimeStore();

  const startNew = () => {
    startDraft();
    navigate("/live");
  };

  const rows: Row[] = [
    // Subagent child sessions are internals of their parent conversation —
    // their asks and progress surface there, so they get no row of their own.
    ...sessions
      .filter((s) => !s.parentId)
      .map((s) => ({ id: s.id, title: s.title, to: `/live/${s.id}`, kind: "session" as const })),
    ...project.sessions
      .filter((e) => !hiddenExamples.includes(e.id))
      .map((e) => ({ id: e.id, title: e.title, to: `/example/${e.id}`, kind: "example" as const })),
  ];

  const [pendingDelete, setPendingDelete] = useState<Row | null>(null);

  const confirmDelete = () => {
    const row = pendingDelete;
    setPendingDelete(null);
    if (!row) return;
    if (row.kind === "session") void deleteSession(row.id);
    else hideExample(row.id);
    if (location.pathname === row.to) navigate("/live");
  };

  // With the overlay titlebar (macOS), reserve a draggable strip at the top so
  // the traffic lights don't overlap the logo and the window stays movable.
  const overlayTitlebar = isTauri && navigator.userAgent.includes("Mac");

  return (
    <aside className="flex h-full w-[232px] shrink-0 flex-col border-r border-border bg-surface">
      {overlayTitlebar && <div data-tauri-drag-region className="h-8 shrink-0" />}
      <div className={cn("px-4 pb-3", overlayTitlebar ? "pt-1" : "pt-4")}>
        <div className="flex items-baseline gap-1.5">
          <img src={logo} alt="" className="h-[18px] w-auto self-center" />
          <div className="font-serif text-[17px] font-semibold leading-none tracking-tight text-text">
            Open Science
          </div>
          <span className="text-[10px] uppercase tracking-widest text-muted">Beta</span>
        </div>
      </div>

      <nav className="flex flex-col px-3">
        <NavRow icon={<Plus size={16} />} label={t("sidebar.newSession")} onClick={startNew} />
        <NavRow icon={<NotebookPen size={16} />} label={t("sidebar.notebooks")} onClick={() => navigate("/notebooks")} />
        <NavRow icon={<FolderTree size={16} />} label={t("sidebar.files")} onClick={() => navigate("/files")} />
        <NavRow icon={<Files size={16} />} label={t("sidebar.skills")} onClick={() => navigate("/skills")} />
      </nav>

      <div className="mt-4 flex-1 overflow-y-auto px-3 pb-2">
        <div className="px-2 py-1 text-xs font-medium uppercase tracking-wider text-muted">{t("sidebar.history")}</div>
        {rows.length === 0 && (
          <div className="px-2 py-2 text-xs text-muted">{t("sidebar.noConversations")}</div>
        )}
        {rows.map((row) => (
          <div key={row.to} className="group relative">
            <NavLink
              to={row.to}
              className={cn(
                "flex items-center gap-2 rounded-input py-1 pl-2 pr-8 text-[13px] hover:bg-surface-2",
                location.pathname === row.to ? "bg-surface-2 text-text" : "text-text/90",
              )}
            >
              <span
                className={cn(
                  "h-1.5 w-1.5 shrink-0 rounded-full",
                  row.kind === "example" ? "bg-muted" : "bg-ok",
                )}
              />
              <span className="flex-1 truncate">{row.title}</span>
              {row.kind === "example" && (
                <span className="shrink-0 rounded-full bg-surface-2 px-1.5 text-[10px] uppercase tracking-wide text-muted ring-1 ring-border">
                  {t("sidebar.example")}
                </span>
              )}
            </NavLink>
            <button
              onClick={() => setPendingDelete(row)}
              aria-label={t("sidebar.deleteAria", { title: row.title })}
              className="absolute right-1.5 top-1/2 hidden -translate-y-1/2 rounded p-1 text-muted hover:bg-border hover:text-error group-hover:block"
            >
              <Trash2 size={13} />
            </button>
          </div>
        ))}
      </div>

      <div className="border-t border-border px-3 py-3">
        <StatusPills />
        <button
          className="mt-2 flex items-center gap-2 rounded-input px-2 py-1 text-[13px] text-muted hover:bg-surface-2 hover:text-text"
          onClick={() => navigate("/settings")}
          aria-label={t("sidebar.settings")}
        >
          <Settings size={15} />
          <span>{t("sidebar.settings")}</span>
        </button>
      </div>

      {pendingDelete && (
        <ConfirmDialog
          title={pendingDelete.kind === "session" ? t("sidebar.deleteTitle") : t("sidebar.hideTitle")}
          body={
            pendingDelete.kind === "session"
              ? t("sidebar.deleteSessionBody", { title: pendingDelete.title })
              : t("sidebar.hideExampleBody", { title: pendingDelete.title })
          }
          confirmLabel={pendingDelete.kind === "session" ? t("common.delete") : t("sidebar.hide")}
          onConfirm={confirmDelete}
          onCancel={() => setPendingDelete(null)}
        />
      )}
    </aside>
  );
}

function NavRow({ icon, label, onClick }: { icon: React.ReactNode; label: string; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="flex items-center gap-2 rounded-input px-2 py-1 text-[13px] text-text hover:bg-surface-2"
    >
      <span className="text-muted">{icon}</span>
      <span>{label}</span>
    </button>
  );
}
