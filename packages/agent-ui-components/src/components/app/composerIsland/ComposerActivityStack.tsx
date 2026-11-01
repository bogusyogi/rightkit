import { useEffect, useMemo, useRef, useState } from "react";
import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import type { GoalSnapshotV1 } from "@rightkit/agent-ui-core/lib/types";
import { ProgressBar } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { goalStatusLabel } from "../composerBarHelpers.js";
import { formatTokens } from "./format.js";

export interface ComposerActivityStackProps {
  agents: SubAgentState[];
  goal: GoalSnapshotV1 | null;
  onOpenAgentsPanel?: () => void;
  onPauseGoal: () => void;
  onResumeGoal: () => void;
  onEditGoal: (text: string) => void;
  onClearGoal: () => void;
}

export function ComposerActivityStack(props: ComposerActivityStackProps) {
  const [agentsExpanded, setAgentsExpanded] = useState(false);
  const [goalExpanded, setGoalExpanded] = useState(false);
  const [editingGoal, setEditingGoal] = useState(false);
  const [goalDraft, setGoalDraft] = useState(props.goal?.objective ?? "");
  const [now, setNow] = useState(Date.now());
  const goalInputRef = useRef<HTMLInputElement>(null);
  const running = useMemo(
    () => props.agents.filter((agent) => agent.status === "running"),
    [props.agents],
  );
  const totalTokens = useMemo(
    () =>
      props.agents.reduce(
        (total, agent) => total + (agent.tokensIn ?? 0) + (agent.tokensOut ?? 0),
        0,
      ),
    [props.agents],
  );

  useEffect(() => setGoalDraft(props.goal?.objective ?? ""), [props.goal?.objective]);
  useEffect(() => {
    if (props.goal?.status !== "active") return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [props.goal?.status]);
  useEffect(() => {
    if (editingGoal) goalInputRef.current?.focus();
  }, [editingGoal]);

  if (props.agents.length === 0 && !props.goal) return null;

  const saveGoal = () => {
    const text = goalDraft.trim();
    if (!text) return;
    props.onEditGoal(text);
    setEditingGoal(false);
  };

  return (
    <div className="mb-1.5 overflow-hidden rounded-lg border border-line bg-panel/95 text-[12px] shadow-sm">
      {props.agents.length > 0 ? (
        <section aria-label="Agent activity">
          <div className="flex min-h-8 items-center gap-2 px-2.5 py-1.5">
            <button
              type="button"
              aria-expanded={agentsExpanded}
              aria-label={agentsExpanded ? "Collapse agent summary" : "Expand agent summary"}
              className="cr-popover-row flex min-w-0 flex-1 cursor-pointer items-center gap-2 rounded text-left"
              onClick={() => setAgentsExpanded((value) => !value)}
            >
              <span className="relative grid h-4 w-4 shrink-0 place-items-center">
                <Icon name="activity" size={13} />
                {running.length > 0 ? (
                  <span className="absolute right-0 top-0 h-1.5 w-1.5 rounded-full bg-accent-2" />
                ) : null}
              </span>
              <strong className="shrink-0 font-medium text-text">Agents</strong>
              <span className="min-w-0 flex-1 truncate text-text-3">
                {running.length > 0
                  ? `${running.length} active · ${props.agents.length} total`
                  : `${props.agents.length} finished`}
              </span>
              <span className="shrink-0 font-mono text-[10px] tabular-nums text-text-3">
                {formatTokens(totalTokens)} tok
              </span>
              <span className={agentsExpanded ? "rotate-180" : ""}>
                <Icon name="chevron" size={11} />
              </span>
            </button>
            {props.onOpenAgentsPanel ? (
              <button
                type="button"
                aria-label="Open background tasks panel"
                className="cr-control-pill h-6 shrink-0 cursor-pointer rounded-md border border-line px-2 text-[10px] font-medium text-text-2 hover:bg-panel-2 hover:text-text"
                onClick={props.onOpenAgentsPanel}
              >
                Open panel
              </button>
            ) : null}
          </div>
          {agentsExpanded ? (
            <ul className="grid gap-1 border-t border-line/70 px-2.5 py-2">
              {props.agents.slice(0, 5).map((agent) => (
                <li
                  key={agent.subSessionId}
                  className="flex min-w-0 items-center gap-2 text-[11px]"
                >
                  <span
                    className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                      agent.status === "running"
                        ? "bg-accent-2"
                        : agent.status === "cancelled"
                          ? "bg-warning"
                          : agent.success === false
                            ? "bg-danger"
                            : "bg-success"
                    }`}
                  />
                  <span className="min-w-0 flex-1 truncate text-text-2">
                    {agent.taskTitle || agent.role}
                  </span>
                  <span className="shrink-0 font-mono text-[10px] tabular-nums text-text-3">
                    {agent.toolUses ?? 0} tools
                  </span>
                </li>
              ))}
            </ul>
          ) : null}
        </section>
      ) : null}

      {props.goal ? (
        <section
          aria-label="Active goal"
          className={props.agents.length > 0 ? "border-t border-line/70" : ""}
        >
          <div className="flex min-h-8 items-center gap-2 px-2.5 py-1.5">
            <button
              type="button"
              aria-expanded={goalExpanded}
              aria-label={goalExpanded ? "Collapse goal summary" : "Expand goal summary"}
              className="cr-popover-row flex min-w-0 flex-1 cursor-pointer items-center gap-2 rounded text-left"
              onClick={() => setGoalExpanded((value) => !value)}
            >
              <Icon name="target" size={12} />
              <strong className="shrink-0 font-medium text-text">
                {goalStatusLabel(props.goal.status)}
              </strong>
              <span className="min-w-0 flex-1 truncate text-text-3">{props.goal.objective}</span>
              <span className="shrink-0 font-mono text-[10px] text-text-3">
                {props.goal.completionPercent === null ? "—" : `${props.goal.completionPercent}%`}
              </span>
              <span className={goalExpanded ? "rotate-180" : ""}>
                <Icon name="chevron" size={11} />
              </span>
            </button>
          </div>
          <ProgressBar
            className="mx-2.5 h-1"
            label={props.goal.completionPercent === null ? "Goal completion: plan pending" : "Goal completion"}
            value={props.goal.completionPercent ?? 0}
            max={100}
          />
          <div className="flex items-center gap-1 px-2.5 py-1">
            <span className="mr-auto text-[10px] text-text-3">
              {Math.max(0, Math.floor((now - Date.parse(props.goal.startedAt)) / 60_000))}m elapsed
            </span>
            {props.goal.status === "active" || props.goal.status === "paused" ? (
              <button
                aria-label={props.goal.status === "active" ? "Pause goal" : "Resume goal"}
                className="cr-control-pill grid h-6 w-6 place-items-center rounded border border-line text-text-3 hover:text-text"
                onClick={props.goal.status === "active" ? props.onPauseGoal : props.onResumeGoal}
                type="button"
              >
                <Icon name={props.goal.status === "active" ? "pause" : "play"} size={10} />
              </button>
            ) : null}
            <button
              aria-label="Edit goal"
              className="cr-control-pill grid h-6 w-6 place-items-center rounded border border-line text-text-3 hover:text-text"
              onClick={() => {
                setGoalExpanded(true);
                setEditingGoal(true);
              }}
              type="button"
            >
              <Icon name="edit" size={10} />
            </button>
            <button
              aria-label="Clear goal"
              className="cr-control-pill grid h-6 w-6 place-items-center rounded border border-line text-text-3 hover:text-danger"
              onClick={props.onClearGoal}
              type="button"
            >
              <Icon name="trash" size={10} />
            </button>
          </div>
          {goalExpanded ? (
            <div className="border-t border-line/70 px-2.5 py-2">
              {editingGoal ? (
                <div className="flex gap-2">
                  <input
                    ref={goalInputRef}
                    aria-label="Edit active goal"
                    className="h-7 min-w-0 flex-1 rounded-md border border-line bg-bg px-2 text-[12px] text-text outline-none focus:border-line-2"
                    value={goalDraft}
                    onChange={(event) => setGoalDraft(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") saveGoal();
                      if (event.key === "Escape") setEditingGoal(false);
                    }}
                  />
                  <button
                    type="button"
                    disabled={!goalDraft.trim()}
                    className="cr-control-pill h-7 cursor-pointer rounded-md bg-accent px-2 text-[11px] font-medium text-on-accent disabled:cursor-not-allowed disabled:opacity-40"
                    onClick={saveGoal}
                  >
                    Save
                  </button>
                </div>
              ) : (
                <div className="flex items-start gap-2">
                  <div className="min-w-0 flex-1 text-[11px] text-text-2">
                    <p className="whitespace-pre-wrap break-words">Objective: {props.goal.objective}</p>
                    <p>Now: {props.goal.now ?? "Waiting for plan"}</p>
                    <p>Next: {props.goal.next ?? "Not set"}</p>
                    <p aria-atomic="true" aria-live="polite">
                      Verification: {props.goal.verificationState} · revision{" "}
                      {props.goal.planRevision}
                    </p>
                    <p>
                      Budget: {formatTokens(props.goal.tokensUsed)} /{" "}
                      {formatTokens(props.goal.budgetTokens)} tok
                    </p>
                    <ul className="mt-1 grid gap-1">
                      {props.goal.steps.map((step) => (
                        <li key={step.id}>
                          {step.label} · {step.state} · {step.weight} · {step.verificationState}
                        </li>
                      ))}
                    </ul>
                  </div>
                </div>
              )}
            </div>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}
