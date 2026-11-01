import { EmptyState } from "@rightkit/react-shell/react";
import type { SpecTaskEditorProps } from "./types.js";

export default function SpecTaskEditor({
  specId,
  taskDraft,
  pending,
  tasks,
  onTaskDraft,
  onAddTask,
}: SpecTaskEditorProps) {
  const taskPending = pending === `task:${specId}`;
  return (
    <div className="min-w-0">
      <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
        Tasks
      </div>
      <div className="border-b border-line px-5 py-3">
        <textarea
          aria-label="Task description"
          className="min-h-20 w-full resize-y rounded-md border border-line bg-panel px-3 py-2 text-[12px] leading-relaxed text-text outline-none placeholder:text-text-3"
          placeholder="Task description"
          value={taskDraft.description}
          onChange={(event) => onTaskDraft({ description: event.target.value })}
        />
        <div className="mt-2 grid grid-cols-2 gap-2">
          <input
            aria-label="Requirement IDs"
            className="h-8 rounded-md border border-line bg-panel px-3 text-[12px] text-text outline-none placeholder:text-text-3"
            placeholder="Requirement IDs"
            value={taskDraft.requirementIds}
            onChange={(event) => onTaskDraft({ requirementIds: event.target.value })}
          />
          <input
            aria-label="Success criterion IDs"
            className="h-8 rounded-md border border-line bg-panel px-3 text-[12px] text-text outline-none placeholder:text-text-3"
            placeholder="Success criterion IDs"
            value={taskDraft.successCriterionIds}
            onChange={(event) => onTaskDraft({ successCriterionIds: event.target.value })}
          />
          <input
            aria-label="Evidence IDs"
            className="h-8 rounded-md border border-line bg-panel px-3 text-[12px] text-text outline-none placeholder:text-text-3"
            placeholder="Evidence IDs"
            value={taskDraft.evidenceIds}
            onChange={(event) => onTaskDraft({ evidenceIds: event.target.value })}
          />
          <input
            aria-label="Foundation phase"
            className="h-8 rounded-md border border-line bg-panel px-3 text-[12px] text-text outline-none placeholder:text-text-3"
            placeholder="Foundation phase"
            value={taskDraft.foundationPhase}
            onChange={(event) => onTaskDraft({ foundationPhase: event.target.value })}
          />
        </div>
        <button
          className="mt-2 inline-flex h-8 items-center rounded-md border border-line bg-raised px-3 text-[12px] font-semibold text-text disabled:cursor-not-allowed disabled:opacity-50"
          disabled={taskPending || !taskDraft.description.trim()}
          type="button"
          onClick={onAddTask}
        >
          {taskPending ? "Adding" : "Add task"}
        </button>
      </div>
      {tasks.length ? (
        <div className="flex flex-col divide-y divide-line">
          {tasks.map((task) => (
            <div className="px-5 py-3" key={task.id}>
              <div className="flex items-center gap-2">
                <code className="rounded border border-line bg-panel px-2 py-0.5 text-[10px] text-accent-2">
                  {task.id}
                </code>
                <strong className="min-w-0 flex-1 truncate text-[13px] text-text">
                  {task.description}
                </strong>
                <span className="text-[11px] text-text-3">{task.status}</span>
              </div>
              <p className="mt-1 truncate text-[11px] text-text-3">
                req {task.requirementIds.join(", ") || "none"} · evidence{" "}
                {task.evidenceIds.join(", ") || "none"}
              </p>
            </div>
          ))}
        </div>
      ) : (
        <EmptyState title="No tasks">{"Mapped tasks are required before spec analysis can prove requirement coverage."}</EmptyState>
      )}
    </div>
  );
}
