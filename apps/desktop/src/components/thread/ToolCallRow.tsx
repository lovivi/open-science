import { AlertTriangle, Check, Clock, Loader2, ShieldQuestion, X } from "lucide-react";
import type { ToolCallBlock, ToolCallStatus } from "@ai4s/shared";
import { cn } from "@/lib/cn";

const STATUS: Record<
  ToolCallStatus,
  { label: string; icon: React.ReactNode; className: string }
> = {
  pending: { label: "Waiting...", icon: <Clock size={13} />, className: "text-muted" },
  running: { label: "Running...", icon: <Loader2 size={13} className="animate-spin" />, className: "text-accent" },
  "waiting-approval": { label: "Pending approval", icon: <ShieldQuestion size={14} />, className: "text-warn" },
  success: { label: "Completed", icon: <Check size={13} />, className: "text-ok" },
  warning: { label: "Completed with warnings", icon: <AlertTriangle size={14} />, className: "text-warn" },
  failed: { label: "Failed", icon: <X size={14} />, className: "text-error" },
};

// Mechanical steps that succeeded (or are pending/running) are recorded quietly,
// like a calm activity log — a scientist scans the conversation for results and
// artifacts, not every shell command. Only things that need attention
// (waiting for approval, warnings, failures) get a prominent card.
const PROMINENT = new Set<ToolCallStatus>(["waiting-approval", "warning", "failed"]);

export function ToolCallRow({ block, activity }: { block: ToolCallBlock; activity?: string }) {
  const s = STATUS[block.status];
  const prominent = PROMINENT.has(block.status);
  return (
    <div data-status={block.status}>
      <div
        className={cn(
          "flex items-center gap-2",
          prominent
            ? "rounded-input border border-border bg-surface px-3 py-2 text-sm"
            : "px-2 py-1 text-[12.5px]",
        )}
      >
        <span className={cn("shrink-0", s.className)} aria-label={s.label} role="img">
          {s.icon}
        </span>
        <span
          className={cn(
            "flex-1 truncate",
            prominent ? "text-text" : cn("font-mono", block.status === "running" ? "text-text" : "text-muted"),
          )}
        >
          {block.title}
        </span>
        {block.meta && <span className="shrink-0 text-xs text-muted">{block.meta}</span>}
      </div>
      {/* Live pulse of the subagent this task spawned — what it is doing right
          now, one quiet line. Vanishes when the task settles. */}
      {activity && block.status === "running" && (
        <div className="flex items-center gap-2 px-2 pb-0.5 text-xs" data-subagent-activity>
          <span
            aria-hidden
            className="mb-1.5 ml-[6px] h-2 w-2 shrink-0 rounded-bl border-b border-l border-border"
          />
          <span aria-hidden className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-accent" />
          <span className="min-w-0 flex-1 truncate font-mono text-muted">{activity}</span>
        </div>
      )}
      {/* Output of a user-typed "!" shell command — the result they asked for.
          (Agent tool steps never carry outputSummary; they stay one quiet line.) */}
      {block.outputSummary && (
        <pre className="ml-2 mt-0.5 max-h-64 overflow-y-auto whitespace-pre-wrap break-all rounded-input bg-surface-2 px-3 py-2 font-mono text-xs leading-5 text-text">
          {block.outputSummary}
        </pre>
      )}
    </div>
  );
}
