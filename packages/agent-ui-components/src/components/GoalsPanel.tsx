import { useState } from "react";

export type GoalStatus = "in_progress" | "blocked" | "done" | "stale";

export interface GoalsPanelGoal {
  id: string;
  title: string;
  status: GoalStatus;
  lastUpdateTurn: number;
  note?: string;
}

export interface GoalsPanelProps {
  goals: GoalsPanelGoal[];
  turn?: number;
  expandedId?: string | null;
  onExpandedIdChange?: (id: string | null) => void;
  onStaleAction?: (id: string, action: "mark_done" | "still_going" | "block") => void;
}

const STATUS_LABEL: Record<GoalStatus, string> = {
  in_progress: "Active",
  blocked: "Blocked",
  done: "Done",
  stale: "Stale",
};

function statusDotClass(status: GoalStatus): string {
  switch (status) {
    case "in_progress":
      return "bg-[var(--goal-status-active,var(--color-accent))]";
    case "blocked":
      return "bg-[var(--goal-status-blocked,var(--color-danger))]";
    case "done":
      return "bg-[var(--goal-status-done,var(--color-success))]";
    case "stale":
      return "bg-[var(--color-warning)]";
    default:
      return "bg-text-3";
  }
}

function isStale(goal: GoalsPanelGoal, currentTurn?: number): boolean {
  if (currentTurn === undefined) return false;
  if (goal.status !== "in_progress") return false;
  return currentTurn - goal.lastUpdateTurn > 5;
}

export default function GoalsPanel({
  goals,
  turn,
  expandedId,
  onExpandedIdChange,
  onStaleAction,
}: GoalsPanelProps) {
  const [internalBlockedGoalId, setInternalBlockedGoalId] = useState<string | null>(null);

  return (
    <section aria-label="Goals" className="flex flex-col gap-1 p-3 text-[12px]">
      {turn !== undefined ? (
        <header className="flex items-center justify-between text-[11px] uppercase tracking-wide text-text-3">
          <span>Goals</span>
          <span>Turn {turn}</span>
        </header>
      ) : null}
      <ul className="grid gap-1">
        {goals.map((goal) => {
          const isExpanded = expandedId === goal.id;
          const blockedOpen = internalBlockedGoalId === goal.id;
          const stale = isStale(goal, turn);
          return (
            <li
              key={goal.id}
              className="rounded-lg border border-transparent bg-[var(--goal-row-bg,transparent)] px-2 py-1.5"
              data-goal-id={goal.id}
              data-status={goal.status}
            >
              <div className="flex items-center gap-2">
                <span
                  aria-hidden="true"
                  className={`h-1.5 w-1.5 shrink-0 rounded-full ${statusDotClass(goal.status)}`}
                />
                <button
                  type="button"
                  className="min-w-0 flex-1 cursor-pointer border-0 bg-transparent p-0 text-left font-body text-[12px] text-text-2 hover:text-text"
                  aria-expanded={isExpanded}
                  onClick={() => onExpandedIdChange?.(isExpanded ? null : goal.id)}
                >
                  {goal.title}
                </button>
                <span className="text-[10px] uppercase tracking-wide text-text-3">
                  {STATUS_LABEL[goal.status]}
                </span>
              </div>
              {isExpanded && goal.note ? (
                <p className="mt-1 pl-3 text-[11px] text-text-3">{goal.note}</p>
              ) : null}
              {stale ? (
                <div
                  className="mt-1 flex items-center justify-between gap-2 rounded-md bg-[var(--goal-status-stale-bg,color-mix(in_srgb,var(--color-warning)_14%,transparent))] px-2 py-1 text-[11px]"
                  data-testid="goal-stale-banner"
                >
                  <span className="text-text-2">
                    No update for {(turn ?? goal.lastUpdateTurn) - goal.lastUpdateTurn} turns.
                  </span>
                  <div className="flex items-center gap-1">
                    <button
                      type="button"
                      className="rounded border border-line bg-transparent px-1.5 py-0.5 text-text-2 hover:border-line-2 hover:text-text"
                      onClick={() => onStaleAction?.(goal.id, "mark_done")}
                    >
                      Mark done
                    </button>
                    <button
                      type="button"
                      className="rounded border border-line bg-transparent px-1.5 py-0.5 text-text-2 hover:border-line-2 hover:text-text"
                      onClick={() => onStaleAction?.(goal.id, "still_going")}
                    >
                      Still going
                    </button>
                    <button
                      type="button"
                      className="rounded border border-line bg-transparent px-1.5 py-0.5 text-text-2 hover:border-line-2 hover:text-text"
                      onClick={() => {
                        setInternalBlockedGoalId(goal.id);
                        onStaleAction?.(goal.id, "block");
                      }}
                    >
                      Blocked
                    </button>
                  </div>
                </div>
              ) : null}
              {blockedOpen ? (
                <div
                  role="dialog"
                  aria-label="Goal blocked"
                  className="mt-1 rounded-md border border-line bg-panel p-2 text-[11px] text-text-2"
                >
                  <p className="mb-1">Block this goal? Add context in the composer.</p>
                  <button
                    type="button"
                    className="rounded border border-line bg-transparent px-1.5 py-0.5 text-text-2 hover:border-line-2 hover:text-text"
                    onClick={() => setInternalBlockedGoalId(null)}
                  >
                    Dismiss
                  </button>
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
