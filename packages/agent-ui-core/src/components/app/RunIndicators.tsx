import { useDismiss } from "@rightkit/platform-ui/headless";
import { useRef, useState } from "react";
import Icon from "../Icon";
import type { TaskNode } from "../../lib/types";

/**
 * Title-bar run indicators: tasks and agents.
 *
 * Both pills are always present (Adrian, 2026-08-04) — at rest they read
 * "0 Tasks" / "0 Agents" with no dot at all, so the row never reflows when a
 * run starts and the eye always knows where to look. Once work exists the dots
 * appear: grey pending, green done, accent-red for the one in flight.
 * Agents use ring dots so a glance never confuses the two.
 *
 * Past DOT_LIMIT the dots are dropped and the counter carries it alone; a title
 * bar of fifteen dots is noise.
 *
 * This is the single home for both. The transcript-level todo dock and the old
 * "Agents · N" header button were removed when this landed — two places showing
 * one truth is how they drift.
 */
const DOT_LIMIT = 8;
const AGENT_DOT_IDS = [
  "agent-dot-1",
  "agent-dot-2",
  "agent-dot-3",
  "agent-dot-4",
  "agent-dot-5",
  "agent-dot-6",
  "agent-dot-7",
  "agent-dot-8",
];

export function TaskIndicator({ tasks }: { tasks: TaskNode[] }) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const todos = tasks.filter((task) => task.category === "todo");
  const done = todos.filter((task) => task.state === "done").length;
  const active = todos.find((task) => task.state === "inProgress");

  useDismiss(() => setOpen(false), {
    enabled: open,
    container: rootRef,
    preventDefault: false,
    stopPropagation: false,
  });

  return (
    <div ref={rootRef} className="relative flex items-center">
      <button
        type="button"
        aria-expanded={open}
        aria-label={todos.length === 0 ? "Tasks: none" : `Tasks: ${done} of ${todos.length} done`}
        title={active ? active.title : `${done}/${todos.length} tasks done`}
        onClick={() => setOpen((value) => !value)}
        className="cr-control-pill inline-flex h-6 items-center gap-1.5 rounded-full border border-line bg-transparent px-2.5 text-[11px] font-medium text-text-3 cursor-pointer hover:bg-panel hover:text-text-2"
      >
        {todos.length > 0 && todos.length <= DOT_LIMIT ? (
          <span className="flex items-center gap-1">
            {todos.map((task) => (
              <span
                key={task.id}
                className={`h-1.5 w-1.5 rounded-full ${
                  task.state === "done"
                    ? "bg-accent-2"
                    : task.state === "inProgress"
                      ? "bg-accent"
                      : "bg-line-2"
                }`}
              />
            ))}
          </span>
        ) : null}
        <span className="tabular-nums text-text-2">
          {todos.length === 0 ? "0" : `${done}/${todos.length}`}
        </span>
        <span className="max-[900px]:hidden">Tasks</span>
      </button>

      {open ? (
        <div className="cr-panel-reveal absolute right-0 top-8 z-30 w-[320px] rounded-lg border border-line bg-panel py-1.5 text-[12.5px] shadow-2xl">
          <div className="flex items-center justify-between px-3 pb-1.5 pt-1 text-[10px] font-semibold uppercase tracking-[0.1em] text-text-3">
            <span>Tasks</span>
            <span className="tabular-nums normal-case tracking-normal text-text-2">
              {todos.length === 0 ? "None yet" : `${done}/${todos.length} done`}
            </span>
          </div>
          {todos.length === 0 ? (
            <p className="px-3 py-1.5 text-text-3">
              The agent has not started a checklist for this run.
            </p>
          ) : (
            <ul className="m-0 flex max-h-[18rem] list-none flex-col overflow-y-auto">
              {todos.map((task) => (
                <li key={task.id} className="flex items-start gap-2 px-3 py-1">
                  <span
                    className={`mt-[3px] grid h-3 w-3 flex-shrink-0 place-items-center rounded-[4px] border ${
                      task.state === "done"
                        ? "border-accent-2 bg-accent-2 text-on-accent-2"
                        : task.state === "inProgress"
                          ? "border-accent"
                          : "border-line-2"
                    }`}
                  >
                    {task.state === "done" ? <Icon name="check" size={8} /> : null}
                    {task.state === "inProgress" ? (
                      <span className="h-1 w-1 rounded-full bg-accent" />
                    ) : null}
                  </span>
                  <span
                    className={
                      task.state === "done"
                        ? "text-text-3 line-through"
                        : task.state === "inProgress"
                          ? "text-text"
                          : "text-text-2"
                    }
                  >
                    {task.title}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </div>
  );
}

export function AgentIndicator({
  agentCount,
  runningAgentCount,
  agentsOpen,
  onToggleAgents,
}: {
  agentCount: number;
  runningAgentCount: number;
  agentsOpen: boolean;
  onToggleAgents?: () => void;
}) {
  // Ring dots, one per agent, filled-ring for finished and accent-ring for running.
  const dots = agentCount > 0 && agentCount <= DOT_LIMIT ? agentCount : 0;
  return (
    <button
      type="button"
      aria-pressed={agentsOpen}
      aria-label={
        agentCount === 0 ? "Agents: none" : `Agents: ${runningAgentCount} of ${agentCount} running`
      }
      title={
        agentCount === 0 ? "No sub-agents" : `${runningAgentCount}/${agentCount} agents running`
      }
      onClick={onToggleAgents}
      className={`cr-control-pill inline-flex h-6 items-center gap-1.5 rounded-full border px-2.5 text-[11px] font-medium cursor-pointer hover:bg-panel hover:text-text-2 ${
        agentsOpen ? "border-accent/40 text-accent" : "border-line bg-transparent text-text-3"
      }`}
    >
      {dots > 0 ? (
        <span className="flex items-center gap-1">
          {AGENT_DOT_IDS.slice(0, dots).map((dotId, index) => (
            <span
              key={dotId}
              className={`h-[7px] w-[7px] rounded-full border-[1.6px] ${
                index < runningAgentCount ? "border-accent" : "border-accent-2"
              }`}
            />
          ))}
        </span>
      ) : null}
      <span className="tabular-nums text-text-2">
        {agentCount === 0 ? "0" : runningAgentCount || agentCount}
      </span>
      <span className="max-[900px]:hidden">Agents</span>
    </button>
  );
}
