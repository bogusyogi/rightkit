import { useMemo, useState } from "react";
import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import type { TaskRow } from "@rightkit/agent-ui-core/lib/types";

export interface ExecutionGraphPaneProps {
  tasks: TaskRow[];
  agents: SubAgentState[];
  costUsd?: number;
  onClose: () => void;
  onStopAgent: (id: string) => void;
  onOpenAgentSession: (id: string) => void;
}

function taskStateLabel(state: TaskRow["state"]): string {
  return String(state).replace(/([a-z])([A-Z])/g, "$1 $2");
}

function nodeTone(state: string): string {
  const value = state.toLowerCase();
  if (value.includes("fail") || value.includes("block")) return "border-danger/50 text-danger";
  if (value.includes("running") || value.includes("progress")) return "border-accent/50 text-accent";
  if (value.includes("done") || value.includes("complete") || value.includes("success")) {
    return "border-accent-2/50 text-accent-2";
  }
  return "border-line text-text-3";
}

export function ExecutionGraphPane({
  tasks,
  agents,
  costUsd = 0,
  onClose,
  onStopAgent,
  onOpenAgentSession,
}: ExecutionGraphPaneProps) {
  const [selected, setSelected] = useState<string | null>(null);
  const taskById = useMemo(() => new Map(tasks.map((task) => [task.id, task])), [tasks]);
  const selectedTask = selected?.startsWith("task:")
    ? taskById.get(selected.slice(5)) ?? null
    : null;
  const selectedAgent = selected?.startsWith("agent:")
    ? agents.find((agent) => agent.subSessionId === selected.slice(6)) ?? null
    : null;
  const running = agents.filter((agent) => agent.status === "running").length;

  return (
    <section className="flex h-full min-h-0 flex-col bg-panel" aria-label="Execution graph">
      <header className="flex flex-none items-center justify-between border-b border-line px-3 py-2">
        <div>
          <h2 className="text-[13px] font-semibold text-text">Execution graph</h2>
          <p className="text-[10px] text-text-3">
            {tasks.length} tasks · {running} running agents · ${costUsd.toFixed(4)}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className="rounded-full border border-line px-2 py-0.5 text-[10px] text-text-2">
            daemon truth
          </span>
          <button
            type="button"
            aria-label="Close execution graph"
            onClick={onClose}
            className="grid h-7 w-7 place-items-center rounded text-text-3 hover:bg-panel-2 hover:text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
          >
            ×
          </button>
        </div>
      </header>

      <div className="grid min-h-0 flex-1 grid-rows-[minmax(0,1fr)_auto]">
        <div className="min-h-0 overflow-y-auto p-3">
          {tasks.length === 0 && agents.length === 0 ? (
            <p className="rounded-lg border border-line bg-panel-2 p-4 text-[12px] text-text-3">
              Execution nodes appear when tasks or agents start.
            </p>
          ) : (
            <ol className="space-y-2" aria-label="Execution nodes">
              {tasks.map((task) => {
                const isSelected = selected === `task:${task.id}`;
                const blockedBy = task.deps.filter((id) => {
                  const state = taskById.get(id)?.state;
                  return state !== "done" && state !== "merged";
                });
                return (
                  <li key={task.id}>
                    <button
                      type="button"
                      aria-pressed={isSelected}
                      onClick={() => setSelected(`task:${task.id}`)}
                      className={`w-full rounded-lg border bg-panel-2 p-3 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus ${isSelected ? "border-accent" : "border-line"}`}
                    >
                      <span className="flex items-start gap-2">
                        <strong className="min-w-0 flex-1 text-[12px] text-text">{task.title}</strong>
                        <span className={`rounded-full border px-1.5 py-0.5 text-[9px] ${nodeTone(String(task.state))}`}>
                          {taskStateLabel(task.state)}
                        </span>
                      </span>
                      <span className="mt-1 block text-[10px] text-text-3">
                        {task.assignedRole || "Unassigned"}
                        {task.estimatedDuration ? ` · ${task.estimatedDuration}` : ""}
                        {task.category ? ` · ${task.category}` : ""}
                      </span>
                      {task.deps.length ? (
                        <span className="mt-1 block text-[10px] text-text-3">
                          Depends on {task.deps.join(", ")}
                          {blockedBy.length ? ` · blocked by ${blockedBy.join(", ")}` : ""}
                        </span>
                      ) : null}
                    </button>
                  </li>
                );
              })}
              {agents.map((agent) => {
                const isSelected = selected === `agent:${agent.subSessionId}`;
                return (
                  <li key={agent.subSessionId} className={agent.depth ? "ml-4 border-l border-line pl-2" : ""}>
                    <button
                      type="button"
                      aria-pressed={isSelected}
                      onClick={() => setSelected(`agent:${agent.subSessionId}`)}
                      className={`w-full rounded-lg border bg-panel p-3 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus ${isSelected ? "border-accent" : "border-line"}`}
                    >
                      <span className="flex items-start gap-2">
                        <strong className="min-w-0 flex-1 text-[12px] text-text">
                          {agent.taskTitle || agent.role}
                        </strong>
                        <span className={`rounded-full border px-1.5 py-0.5 text-[9px] ${nodeTone(agent.success === false ? "failed" : agent.status)}`}>
                          {agent.status}
                        </span>
                      </span>
                      <span className="mt-1 block text-[10px] text-text-3">
                        {agent.role} · {agent.model ?? "model unavailable"} · {agent.toolUses ?? 0} tools
                        {agent.elapsedMs !== undefined ? ` · ${Math.round(agent.elapsedMs / 1000)}s` : ""}
                      </span>
                      {agent.lastTools?.length ? (
                        <span className="mt-1 block truncate text-[10px] text-text-3">
                          Latest: {agent.lastTools[agent.lastTools.length - 1]}
                        </span>
                      ) : null}
                      {agent.dependencyEdge || agent.taskEdge ? (
                        <span className="mt-1 block text-[10px] text-text-3">
                          {[agent.taskEdge, agent.dependencyEdge].filter(Boolean).join(" · ")}
                        </span>
                      ) : null}
                    </button>
                  </li>
                );
              })}
            </ol>
          )}
        </div>

        {selectedTask || selectedAgent ? (
          <aside className="border-t border-line bg-panel-2 p-3" aria-label="Selected execution node">
            {selectedTask ? (
              <>
                <strong className="text-[12px] text-text">{selectedTask.title}</strong>
                <p className="mt-1 text-[10px] text-text-3">{selectedTask.doneWhen || "No completion predicate reported."}</p>
                {selectedTask.outputPreview ? (
                  <pre className="mt-2 max-h-24 overflow-auto whitespace-pre-wrap rounded border border-line bg-bg p-2 text-[10px] text-text-2">
                    {selectedTask.outputPreview}
                  </pre>
                ) : null}
              </>
            ) : selectedAgent ? (
              <div className="flex items-center gap-2">
                <div className="min-w-0 flex-1">
                  <strong className="text-[12px] text-text">{selectedAgent.taskTitle || selectedAgent.role}</strong>
                  <p className="truncate text-[10px] text-text-3">{selectedAgent.summary || selectedAgent.subSessionId}</p>
                </div>
                <button
                  type="button"
                  onClick={() => onOpenAgentSession(selectedAgent.subSessionId)}
                  className="rounded border border-line px-2 py-1 text-[10px] text-text-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
                >
                  Transcript
                </button>
                {selectedAgent.status === "running" ? (
                  <button
                    type="button"
                    onClick={() => onStopAgent(selectedAgent.subSessionId)}
                    className="rounded border border-danger/50 px-2 py-1 text-[10px] text-danger focus-visible:outline focus-visible:outline-2 focus-visible:outline-danger"
                  >
                    Stop
                  </button>
                ) : null}
              </div>
            ) : null}
          </aside>
        ) : null}
      </div>
    </section>
  );
}
