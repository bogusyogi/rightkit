import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { TaskRow } from "@rightkit/agent-ui-core/lib/types";

export default function TasksPane({ tasks }: { tasks: TaskRow[] }) {
  const taskStorePath = tasks.find((task) => task.taskStorePath)?.taskStorePath;
  const completedCount = tasks.filter((task) =>
    ["done", "Done", "merged", "Merged", "3"].includes(String(task.state)),
  ).length;
  const activeCount = tasks.filter((task) =>
    ["inProgress", "InProgress", "1"].includes(String(task.state)),
  ).length;
  const blockedCount = tasks.filter((task) =>
    ["blocked", "Blocked", "escalated", "Escalated", "2"].includes(String(task.state)),
  ).length;
  return (
    <section className="flex flex-col flex-1 min-h-0 overflow-hidden">
      <div className="flex items-center gap-4 px-6 py-4 border-b border-line flex-shrink-0">
        <div className="min-w-0">
          <h2 className="text-[16px] font-bold text-text">Tasks</h2>
          <p className="text-[12px] text-text-3 truncate" title={taskStorePath}>
            {taskStorePath ? "Workspace task records" : "Live session task graph"}
          </p>
        </div>
      </div>
      {tasks.length === 0 ? (
        <div className="grid min-h-0 flex-1 place-content-center gap-2 px-6 pb-20 text-center">
          <span className="mx-auto grid h-9 w-9 place-items-center rounded-lg border border-line bg-panel text-text-3">
            <Icon name="tasks" size={18} />
          </span>
          <p className="text-[13px] font-medium text-text-2">No tasks yet</p>
          <p className="max-w-64 text-[12px] leading-relaxed text-text-3">
            Tasks appear here when a goal is planned or work is delegated.
          </p>
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4 sm:px-6">
          <div className="mx-auto flex w-full max-w-3xl flex-col gap-2">
            <div
              role="status"
              aria-label="Task progress"
              className="flex flex-wrap gap-x-3 gap-y-1 px-1 text-[11px] text-text-3"
            >
              <span>{tasks.length} {tasks.length === 1 ? "task" : "tasks"}</span>
              <span>{completedCount} complete</span>
              {activeCount > 0 ? <span>{activeCount} active</span> : null}
              {blockedCount > 0 ? <span className="text-warning">{blockedCount} blocked</span> : null}
            </div>
            {tasks.map((task) => (
              <TaskCard task={task} key={task.id} />
            ))}
          </div>
        </div>
      )}
    </section>
  );
}

function TaskCard({ task }: { task: TaskRow }) {
  const status = taskStatus(task.state);
  const sourceLabel =
    task.source === "workspace-record"
      ? task.kind || "persisted"
      : task.source === "session-taskgraph"
        ? "live"
        : task.source || "task";
  const meta = [task.assignedRole || task.category, sourceLabel].filter(Boolean).join(" · ");
  const shellMeta = [
    task.shellId,
    task.processStatus,
    task.processAttachment,
    task.canStop ? "stoppable" : null,
    task.canPollOutput === false ? "stored output" : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <article className="flex flex-col gap-1.5 rounded-lg border border-line bg-panel p-3">
      <div className="flex items-start gap-2">
        <strong className="min-w-0 flex-1 text-[13px] font-semibold text-text break-words">
          {task.title}
        </strong>
        <span
          className={`shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-medium ${status.className}`}
        >
          {status.label}
        </span>
        <code className="shrink-0 text-[10px] font-mono text-text-3">{task.id}</code>
      </div>
      <span className="text-[11px] text-text-3 truncate">{meta}</span>
      {task.doneWhen ? (
        <p className="text-[11px] text-text-3 line-clamp-2">{task.doneWhen}</p>
      ) : null}
      {task.deps.length > 0 ? (
        <p className="text-[10px] text-text-3">
          Depends on: <span className="font-mono">{task.deps.join(", ")}</span>
        </p>
      ) : null}
      {task.command ? (
        <code className="text-[13px] font-mono text-text-2 truncate">{task.command}</code>
      ) : null}
      {task.outputPreview ? (
        <pre className="max-h-24 overflow-hidden rounded-md border border-line bg-panel-2 p-2 text-[11px] text-text-2 whitespace-pre-wrap break-words">
          {task.outputPreview}
        </pre>
      ) : null}
      {task.outputBytes ? (
        <span className="text-[10px] text-text-3">
          {task.outputBytes} output bytes
          {shellMeta ? ` · ${shellMeta}` : ""}
        </span>
      ) : task.shellId ? (
        <span className="text-[10px] text-text-3">{shellMeta}</span>
      ) : null}
      {task.processStatusReason ? (
        <p className="text-[10px] text-text-3 leading-snug">{task.processStatusReason}</p>
      ) : null}
      {task.lifecycleStatusReason ? (
        <p className="text-[10px] text-text-3 leading-snug">{task.lifecycleStatusReason}</p>
      ) : null}
    </article>
  );
}

function taskStatus(state: TaskRow["state"]) {
  switch (String(state)) {
    case "open":
    case "Open":
    case "split":
    case "Split":
    case "0":
      return { label: "Open", className: "border-line text-text-2" };
    case "inProgress":
    case "InProgress":
    case "1":
      return { label: "In progress", className: "border-accent/40 text-accent" };
    case "blocked":
    case "Blocked":
    case "escalated":
    case "Escalated":
    case "2":
      return { label: "Blocked", className: "border-warning/40 text-warning" };
    case "done":
    case "Done":
    case "merged":
    case "Merged":
    case "3":
      return { label: "Done", className: "border-accent-2/40 text-accent-2" };
    case "failed":
    case "Failed":
    case "4":
      return { label: "Failed", className: "border-danger/40 text-danger" };
    default:
      return { label: "Task", className: "border-line text-text-3" };
  }
}
