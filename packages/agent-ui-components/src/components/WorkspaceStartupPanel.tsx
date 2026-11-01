import { ModalShell } from "@rightkit/react-shell/react";
import { useId } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type {
  WorkspaceStartupTask,
  WorkspaceStartupView,
} from "@rightkit/agent-ui-hooks/hooks/useWorkspaceStartupProjection";

export interface WorkspaceStartupPanelProps {
  startup: WorkspaceStartupView | null;
  scope?: "window" | "workspace";
  onApprove?: () => void;
  onRetry?: () => void;
  onContinue?: () => void;
}

/** Workspace-scoped startup projection; execution remains outside React. */
export function WorkspaceStartupPanel({
  startup,
  scope = "workspace",
  onApprove,
  onRetry,
  onContinue,
}: WorkspaceStartupPanelProps) {
  const titleId = useId();
  const reasonId = useId();
  const outputId = useId();
  const entry = startup?.entry ?? null;
  if (!entry) return null;

  const pendingApproval = entry.state === "approval_required";
  const failed = entry.state === "failed";
  const running = entry.state === "running";
  const interactive = pendingApproval || failed;
  const describedBy = [entry.reason ? reasonId : null, startup?.output ? outputId : null]
    .filter(Boolean)
    .join(" ");
  const title = pendingApproval
    ? "Review Workspace Startup"
    : failed
      ? "Workspace Startup Failed"
      : running
        ? "Preparing Workspace"
        : "Workspace Startup";
  const className = scope === "workspace"
    ? "flex min-h-0 flex-col rounded-xl border border-line bg-panel shadow-xl"
    : "fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-6";

  const content = (
      <div className="grid min-w-0 gap-4 p-4">
        <div className="flex items-start gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full border border-warning/40 bg-warning/10 text-warning" aria-hidden="true">
            <Icon name={failed ? "alert" : "folder"} size={18} />
          </span>
          <div className="min-w-0">
            <h2 id={titleId} className="text-sm font-semibold text-text">{title}</h2>
            <p className="truncate text-[11px] text-text-3" title={entry.root}>{entry.root}</p>
          </div>
        </div>

        <section className="grid gap-1" aria-label="Workspace startup tasks">
          {entry.commands.length === 0 ? (
            <div className="flex items-center gap-2 rounded-md bg-panel-2 px-2 py-1.5 text-[12px] text-text-2">
              <span className="h-1.5 w-1.5 rounded-full bg-accent animate-pulse" aria-hidden="true" />
              <span>Preparing environment</span>
            </div>
          ) : (
            entry.commands.map((task) => (
              <div className="flex items-center gap-2 rounded-md bg-panel-2 px-2 py-1.5 text-[12px]" key={task.id}>
                <span className={`h-1.5 w-1.5 rounded-full ${taskStateClass(task.state)}`} aria-hidden="true" />
                <span className="min-w-0 flex-1 truncate text-text-2">{task.label}</span>
                <span className="text-[10px] text-text-3">{startupTaskMeta(task)}</span>
              </div>
            ))
          )}
        </section>

        {entry.warning ? <p className="text-[11px] text-warning">{entry.warning}</p> : null}
        {entry.reason ? <p className="text-[12px] text-danger" id={reasonId}>{entry.reason}</p> : null}
        {startup?.output ? (
          <pre className="max-h-40 overflow-auto rounded-md border border-line bg-bg px-2 py-1.5 text-[10px] leading-relaxed text-text-3" id={outputId}>
            {startup.output}
          </pre>
        ) : null}

        {pendingApproval || failed ? (
          <div className="flex flex-wrap gap-2">
            {pendingApproval ? (
              <button type="button" className="rounded-md bg-accent px-3 py-1.5 text-[12px] font-semibold text-on-accent" onClick={onApprove}>
                Approve and run
              </button>
            ) : null}
            {failed ? (
              <button type="button" className="rounded-md bg-accent px-3 py-1.5 text-[12px] font-semibold text-on-accent" onClick={onRetry}>
                Retry startup
              </button>
            ) : null}
            <button type="button" className="rounded-md border border-line px-3 py-1.5 text-[12px] text-text-2 hover:bg-panel-2" onClick={onContinue}>
              Skip startup
            </button>
          </div>
        ) : null}
      </div>
  );

  // Window scope with an approval or failure decision is modal: RightKit ModalShell with no dismissal, so
  // only the startup buttons exit. Workspace scope and status states stay inline.
  if (scope === "window" && interactive) {
    return (
      <ModalShell
        label={title}
        labelledBy={titleId}
        describedBy={describedBy || undefined}
        onClose={() => undefined}
        dismissible={false}
        width={480}
      >
        <div data-testid="workspace-startup-panel">{content}</div>
      </ModalShell>
    );
  }

  return (
    // biome-ignore lint/a11y/useAriaPropsSupportedByRole: role & ARIA state vary together between dialog and status modes.
    <div
      className={className}
      role={interactive ? "dialog" : "status"}
      aria-live={interactive ? undefined : "polite"}
      aria-labelledby={titleId}
      aria-describedby={describedBy || undefined}
      data-testid="workspace-startup-panel"
    >
      {content}
    </div>
  );
}

function startupTaskMeta(task: WorkspaceStartupTask): string {
  switch (task.state) {
    case "running":
      return task.id === "aethon-devshell" ? "checking dev shell" : "running";
    case "ready":
      return "ready";
    case "failed":
      return task.required ? "failed" : "non-blocking failed";
    default:
      return "waiting";
  }
}

function taskStateClass(state: string): "bg-accent" | "bg-accent-2" | "bg-danger" | "bg-text-3" {
  switch (state) {
    case "running":
      return "bg-accent";
    case "ready":
      return "bg-accent-2";
    case "failed":
      return "bg-danger";
    default:
      return "bg-text-3";
  }
}

export default WorkspaceStartupPanel;
