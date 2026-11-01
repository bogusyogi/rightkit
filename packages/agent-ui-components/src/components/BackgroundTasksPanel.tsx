import { useEffect, useMemo, useState } from "react";
import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { hostAppName } from "../appName.js";

function fmtTokens(value: number | undefined): string {
  const n = value ?? 0;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function fmtElapsed(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const tail = `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
  return hours > 0 ? `${hours}h ${tail}` : tail;
}

function liveElapsed(agent: SubAgentState, now: number): number {
  const reported = agent.elapsedMs ?? 0;
  const locallyElapsed = agent.status === "running" ? now - agent.startedAt : 0;
  return Math.max(0, Math.floor(Math.max(reported, locallyElapsed) / 1000));
}

const LIVENESS_GRACE_MS = 5_000;

function hasRunningDescendant(node: AgentTreeNode): boolean {
  return node.children.some(
    (child) => child.agent.status === "running" || hasRunningDescendant(child),
  );
}

function isStale(agent: SubAgentState, now: number): boolean {
  return (
    agent.status === "running" &&
    agent.lastUpdatedAt !== undefined &&
    now - agent.lastUpdatedAt >= LIVENESS_GRACE_MS
  );
}

function statusLabel(agent: SubAgentState): string {
  if (agent.status === "running") return "Running";
  if (agent.status === "cancelled") return "Stopped";
  if (agent.success === true) return "Succeeded";
  return agent.success === false ? "Failed" : "Finished";
}

function statusDot(agent: SubAgentState): string {
  if (agent.status === "running") return "bg-accent";
  if (agent.status === "cancelled") return "bg-warning";
  if (agent.success === true) return "bg-success";
  return agent.success === false ? "bg-danger" : "bg-line-2";
}

interface AgentTreeNode {
  agent: SubAgentState;
  children: AgentTreeNode[];
}

// Builds the conductor → agent → subagent forest from `parentSubSessionId`.
// Today no event populates that field (see useSubAgents.ts), so every agent
// is parentless and this returns one flat level per root — the documented
// "defaults to flat when absent" behavior, not a special case.
function buildAgentForest(agents: SubAgentState[]): AgentTreeNode[] {
  const ids = new Set(agents.map((agent) => agent.subSessionId));
  const childrenByParent = new Map<string, SubAgentState[]>();
  const roots: SubAgentState[] = [];
  for (const agent of agents) {
    const parentId = agent.parentSubSessionId;
    if (parentId && parentId !== agent.subSessionId && ids.has(parentId)) {
      const siblings = childrenByParent.get(parentId) ?? [];
      siblings.push(agent);
      childrenByParent.set(parentId, siblings);
    } else {
      roots.push(agent);
    }
  }
  const build = (agent: SubAgentState, ancestors: Set<string>): AgentTreeNode => {
    const nextAncestors = new Set(ancestors).add(agent.subSessionId);
    const children = (childrenByParent.get(agent.subSessionId) ?? []).filter(
      (child) => !ancestors.has(child.subSessionId),
    );
    return { agent, children: children.map((child) => build(child, nextAncestors)) };
  };
  return roots.map((agent) => build(agent, new Set()));
}

function needsAttention(agent: SubAgentState): boolean {
  return agent.status === "completed" && agent.success === false;
}

const COLLAPSE_CHILDREN_THRESHOLD = 3;

export interface BackgroundTasksPanelProps {
  agents: SubAgentState[];
  sessionTitle?: string;
  goalStatus?: string;
  costUsd?: number;
  maximized?: boolean;
  onClose: () => void;
  onMaximize?: () => void;
  onStop: (id: string) => void;
  onCancelAll?: () => void;
  onOpenAgentSession: (id: string) => void;
}

export default function BackgroundTasksPanel({
  agents,
  maximized = false,
  onClose,
  onMaximize,
  onStop,
  onCancelAll,
  onOpenAgentSession,
}: BackgroundTasksPanelProps) {
  const [showFinished, setShowFinished] = useState(false);
  const [expandedAgents, setExpandedAgents] = useState<Set<string>>(() => new Set());
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(() => new Set());
  const [now, setNow] = useState(() => Date.now());
  const running = useMemo(() => agents.filter((agent) => agent.status === "running"), [agents]);
  useEffect(() => {
    if (running.length === 0) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [running.length]);
  const finished = useMemo(() => agents.filter((agent) => agent.status !== "running"), [agents]);
  const forest = useMemo(() => buildAgentForest(agents), [agents]);
  const runningRoots = useMemo(
    () =>
      forest.filter(
        (node) => node.agent.status === "running" || hasRunningDescendant(node),
      ),
    [forest],
  );
  const finishedRoots = useMemo(
    () => forest.filter((node) => !hasRunningDescendant(node) && node.agent.status !== "running"),
    [forest],
  );
  const toggleAgent = (id: string) => {
    setExpandedAgents((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleGroup = (parentId: string) => {
    setExpandedGroups((current) => {
      const next = new Set(current);
      if (next.has(parentId)) next.delete(parentId);
      else next.add(parentId);
      return next;
    });
  };

  const renderAgentRow = (agent: SubAgentState) => {
    const expanded = expandedAgents.has(agent.subSessionId);
    const title = agent.taskTitle || agent.role;
    const stale = isStale(agent, now);
    return (
      <div
        className="overflow-hidden rounded-md border border-line/80 bg-panel-2"
        data-liveness={stale ? "stale" : agent.status === "running" ? "live" : "terminal"}
      >
        <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2 px-2.5 py-2">
          <button
            type="button"
            className="cr-popover-row min-w-0 cursor-pointer rounded text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
            aria-expanded={expanded}
            aria-label={`${expanded ? "Hide" : "Show"} details for ${title}`}
            onClick={() => toggleAgent(agent.subSessionId)}
          >
            <span className="flex min-w-0 items-center gap-2">
              <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${statusDot(agent)}`} />
              <span className="min-w-0 flex-1 truncate text-[12px] font-medium text-text">
                {title}
              </span>
              {needsAttention(agent) ? (
                <span
                  data-testid={`agent-attention-${agent.subSessionId}`}
                  className="inline-flex shrink-0 items-center gap-1 rounded border border-danger/40 px-1 py-px text-[9px] font-semibold uppercase tracking-wide text-danger"
                >
                  <Icon name="alert" size={9} />
                  Needs attention
                </span>
              ) : null}
              <Icon name="chevron" size={11} />
            </span>
            <span className="mt-1 grid grid-cols-[minmax(52px,auto)_minmax(44px,auto)_minmax(50px,auto)] gap-2 pl-3.5 font-mono text-[10px] tabular-nums text-text-3">
              <span>{fmtTokens((agent.tokensIn ?? 0) + (agent.tokensOut ?? 0))} tok</span>
              <span>{agent.toolUses ?? 0} tools</span>
              <span data-testid={`agent-elapsed-${agent.subSessionId}`}>
                {fmtElapsed(liveElapsed(agent, now))}
              </span>
            </span>
            {stale ? (
              <span
                data-testid={`agent-liveness-${agent.subSessionId}`}
                className="mt-0.5 block pl-3.5 text-[10px] text-warning"
                title="The daemon has not reported a roster update recently."
              >
                No recent update
              </span>
            ) : null}
            {/* Which model ran this agent. Previously invisible, so a user could
                not tell single-model (free) from multi-model (Pro) routing. */}
            {agent.model || agent.providerId || agent.vendorSessionId || agent.isolationMode ? (
              <span className="mt-0.5 block truncate pl-3.5 font-mono text-[10px] text-text-3">
                {agent.providerId ? `${agent.providerId} · ` : ""}
                {agent.model ?? "model unavailable"}
                {agent.vendorSessionId ? ` · session ${agent.vendorSessionId}` : ""}
                {agent.isolationMode
                  ? ` · ${agent.isolationMode === "exclusive_coderight_gateway" ? "${hostAppName()} gateway" : "Read-only compatibility"}`
                  : ""}
              </span>
            ) : null}
          </button>
          {agent.status === "running" ? (
            <button
              type="button"
              aria-label={`Stop ${title}`}
              title="Stop agent"
              onClick={() => onStop(agent.subSessionId)}
              className="cr-control-pill grid h-6 w-6 shrink-0 cursor-pointer place-items-center rounded-md border border-line text-text-3 hover:border-danger/70 hover:text-danger focus:outline-none focus-visible:ring-2 focus-visible:ring-danger/60"
            >
              <Icon name="stop" size={11} />
            </button>
          ) : (
            <span className="px-1 text-[10px] text-text-3">{statusLabel(agent)}</span>
          )}
        </div>
        {expanded ? (
          <div className="border-t border-line/70 px-3 py-2 text-[11px] text-text-3">
            {agent.summary ? <p className="leading-4 text-text-2">{agent.summary}</p> : null}
            {agent.lastTools?.length ? (
              <div className="mt-2 flex flex-wrap items-center gap-1">
                <span className="mr-1 text-[10px] uppercase tracking-wide">Recent</span>
                {agent.lastTools.map((tool) => (
                  <span
                    key={tool}
                    className="rounded border border-line bg-panel px-1.5 py-0.5 font-mono text-[10px]"
                  >
                    {tool}
                  </span>
                ))}
              </div>
            ) : null}
            <button
              type="button"
              className="mt-2 cursor-pointer text-accent underline-offset-2 hover:underline"
              onClick={() => onOpenAgentSession(agent.subSessionId)}
            >
              View transcript
            </button>
          </div>
        ) : null}
      </div>
    );
  };

  const renderAgentNode = (node: AgentTreeNode) => {
    const { agent, children } = node;
    const title = agent.taskTitle || agent.role;
    const collapsible = children.length > COLLAPSE_CHILDREN_THRESHOLD;
    const groupExpanded = !collapsible || expandedGroups.has(agent.subSessionId);
    const groupId = `agent-children-${agent.subSessionId}`;
    return (
      <li key={agent.subSessionId} className="min-w-0">
        {renderAgentRow(agent)}
        {children.length > 0 ? (
          <div className="ml-3 mt-1.5 min-w-0 border-l border-line/60 pl-2">
            {collapsible ? (
              <button
                type="button"
                aria-expanded={groupExpanded}
                aria-controls={groupId}
                onClick={() => toggleGroup(agent.subSessionId)}
                className="cr-control-pill mb-1.5 inline-flex cursor-pointer items-center gap-1 rounded border border-line px-1.5 py-0.5 text-[10px] font-medium text-text-3 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
              >
                <Icon name="chevron" size={9} />
                {groupExpanded ? "Hide" : "Show"} {children.length} sub-agents of {title}
              </button>
            ) : null}
            {groupExpanded ? (
              <ul id={groupId} className="flex flex-col gap-1.5">
                {children.map((child) => renderAgentNode(child))}
              </ul>
            ) : null}
          </div>
        ) : null}
      </li>
    );
  };

  return (
    <aside
      aria-label="Agents"
      className="cr-agent-rail flex h-full w-full min-w-0 shrink-0 flex-col overflow-hidden bg-panel/95"
    >
      <header className="flex h-10 flex-none items-center gap-2 border-b border-line/70 px-3 text-[14px] font-medium text-text-2">
        <span className="flex-1 truncate">Agents</span>
        {running.length > 1 && onCancelAll ? (
          <button
            type="button"
            aria-label="Stop all running agents"
            title="Stop all"
            data-testid="stop-all-agents"
            onClick={onCancelAll}
            className="cr-control-pill inline-flex h-6 cursor-pointer items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 text-[11px] font-semibold text-text-2 hover:border-danger/60 hover:text-danger"
          >
            <Icon name="stop" size={10} />
            <span>Stop all</span>
          </button>
        ) : null}
        {onMaximize ? (
          <button
            type="button"
            aria-label={maximized ? "Restore Agents panel" : "Maximize Agents panel"}
            title={maximized ? "Restore" : "Maximize"}
            aria-pressed={maximized}
            onClick={onMaximize}
            className="cr-control-pill grid h-6 w-6 cursor-pointer place-items-center rounded-md border border-transparent text-text-3 hover:bg-panel-2 hover:text-text"
          >
            <Icon name="maximize" size={13} />
          </button>
        ) : null}
        <button
          type="button"
          aria-label="Close Agents panel"
          title="Close"
          onClick={onClose}
          className="cr-control-pill grid h-6 w-6 cursor-pointer place-items-center rounded-md border border-transparent text-text-3 hover:bg-panel-2 hover:text-text"
        >
          <Icon name="close" size={13} />
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto p-3 text-[13px]">
        <section aria-labelledby="running-agents-title">
          <div className="mb-2 flex items-center gap-2">
            <h2
              id="running-agents-title"
              className="text-[11px] font-semibold uppercase tracking-[0.08em] text-text-3"
            >
              Running
            </h2>
            <span className="rounded-full border border-line px-1.5 text-[10px] text-text-2">
              {running.length}
            </span>
          </div>
          {running.length === 0 ? (
            <div
              data-testid="agents-no-running"
              className="rounded-md border border-line/60 bg-panel/40 px-3 py-4 text-[12px] leading-5 text-text-3"
            >
              <p className="text-text-2">No agents running.</p>
              {finished.length === 0 ? <p>Agents appear here when work is delegated.</p> : null}
            </div>
          ) : (
            <ul className="flex flex-col gap-1.5">
              {runningRoots.map((node) => renderAgentNode(node))}
            </ul>
          )}
        </section>

        {finished.length > 0 ? (
          <section aria-labelledby="finished-agents-title" className="mt-4">
            <button
              type="button"
              className="cr-popover-row flex w-full cursor-pointer items-center rounded py-1 text-left"
              aria-expanded={showFinished}
              onClick={() => setShowFinished((value) => !value)}
            >
              <h2
                id="finished-agents-title"
                className="text-[11px] font-semibold uppercase tracking-[0.08em] text-text-3"
              >
                Recent
              </h2>
              <span className="ml-2 rounded-full border border-line px-1.5 text-[10px] text-text-2">
                {finished.length}
              </span>
              <span className="ml-2">
                <Icon name="chevron" size={11} />
              </span>
            </button>
            {showFinished ? (
              <ul className="mt-2 flex flex-col gap-1.5">
                {finishedRoots.map((node) => renderAgentNode(node))}
              </ul>
            ) : null}
          </section>
        ) : null}
      </div>
    </aside>
  );
}
