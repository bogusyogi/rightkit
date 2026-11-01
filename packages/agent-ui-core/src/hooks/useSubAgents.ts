import { useCallback, useMemo, useState } from "react";
import { interruptRun } from "../lib/daemon.js";
import type { DaemonConfig, ProtocolEvent } from "../lib/types.js";

export type SubAgentStatus = "running" | "completed" | "cancelled";

export interface SubAgentState {
  /** Lifecycle of this sub-agent, set by spawn/complete/cancel/roster events. */
  status: SubAgentStatus;
  subSessionId: string;
  role: string;
  taskTitle: string;
  /// Model this agent ran on. Without it the rail could not show that Pro's
  /// multi-model routing was doing anything — every agent looked identical.
  model?: string;
  /** Provider selected for this agent's authenticated CLI runtime. */
  providerId?: string;
  /** Vendor-issued session identity, when the CLI has completed its handshake. */
  vendorSessionId?: string;
  /** Harness isolation mode applied to this agent. */
  isolationMode?: string;
  startedAt: number;
  success?: boolean;
  summary?: string;
  cancelledBy?: string;
  // F1 agent rail: live metrics from `agentRosterUpdated`.
  tokensIn?: number;
  tokensOut?: number;
  cacheRead?: number;
  toolUses?: number;
  elapsedMs?: number;
  lastTools?: string[];
  /// Parent agent's subSessionId, for rendering conductor → agent → subagent
  /// hierarchy when a future daemon event supplies it; absent → flat list.
  parentSubSessionId?: string;
  /** Depth in the conductor → agent → subagent tree (0 = root). */
  depth?: number;
  /** Owner role that spawned this agent. */
  owner?: string;
  /** Task-graph edge this agent is executing. */
  taskEdge?: string;
  /** Dependency edge blocking this agent, if any. */
  dependencyEdge?: string;
  /** Worktree path this agent is confined to. */
  worktree?: string;
  /** Authority level governing this agent's tool access. */
  authority?: string;
  /** Lifecycle phase: spawning | running | done | failed | cancelled */
  lifecycle?: string;
  /** Local receipt time of the newest roster/lifecycle update. */
  lastUpdatedAt?: number;
}

interface UseSubAgentsOptions {
  config: DaemonConfig | null;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
}

export function reduceSubAgents(
  current: SubAgentState[],
  event: ProtocolEvent,
  now = Date.now(),
): SubAgentState[] {
  switch (event.$kind) {
    case "subAgentSpawned": {
      const spawned = event as unknown as Record<string, unknown> & {
        subSessionId?: string;
        role?: string;
        taskTitle?: string;
        parentSubSessionId?: string;
        depth?: number;
        owner?: string;
        worktree?: string;
        authority?: string;
        lifecycle?: string;
      };
      const subSessionId = String(spawned.subSessionId ?? event.subSessionId);
      const existing = current.find((agent) => agent.subSessionId === subSessionId);
      if (existing) {
        // Replay and reconnect can deliver the spawn after a roster snapshot.
        // Refresh the receipt without regressing a terminal lifecycle.
        return current.map((agent) =>
          agent.subSessionId === subSessionId
            ? {
                ...agent,
                role: String(spawned.role ?? event.role),
                taskTitle: String(spawned.taskTitle ?? event.taskTitle),
                lastUpdatedAt: now,
              }
            : agent,
        );
      }
      return [
        ...current,
        {
          subSessionId,
          role: String(spawned.role ?? event.role),
          taskTitle: String(spawned.taskTitle ?? event.taskTitle),
          status: "running" as SubAgentStatus,
          startedAt: now,
          parentSubSessionId: spawned.parentSubSessionId as string | undefined,
          depth: spawned.depth as number | undefined,
          owner: spawned.owner as string | undefined,
          worktree: spawned.worktree as string | undefined,
          authority: spawned.authority as string | undefined,
          lifecycle: spawned.lifecycle as string | undefined,
          lastUpdatedAt: now,
        },
      ];
    }
    case "subAgentCompleted": {
      const existing = current.find((agent) => agent.subSessionId === event.subSessionId);
      const completed: SubAgentState = existing
        ? {
            ...existing,
            status: existing.status === "cancelled" ? "cancelled" : "completed",
            success: event.success,
            summary: event.summary,
            elapsedMs: Math.max(existing.elapsedMs ?? 0, now - existing.startedAt),
            lastUpdatedAt: now,
          }
        : {
            subSessionId: event.subSessionId,
            role: event.role,
            taskTitle: event.role,
            status: "completed",
            startedAt: now,
            success: event.success,
            summary: event.summary,
            lastUpdatedAt: now,
          };
      return existing
        ? current.map((agent) =>
            agent.subSessionId === event.subSessionId ? completed : agent,
          )
        : [...current, completed];
    }
    case "subAgentCancelled": {
      const existing = current.find((agent) => agent.subSessionId === event.subSessionId);
      const cancelled: SubAgentState = existing
        ? {
            ...existing,
            status: "cancelled",
            cancelledBy: event.cancelledBy,
            elapsedMs: Math.max(existing.elapsedMs ?? 0, now - existing.startedAt),
            lastUpdatedAt: now,
          }
        : {
            subSessionId: event.subSessionId,
            role: event.role,
            taskTitle: event.role,
            status: "cancelled",
            startedAt: now,
            cancelledBy: event.cancelledBy,
            lastUpdatedAt: now,
          };
      return existing
        ? current.map((agent) =>
            agent.subSessionId === event.subSessionId ? cancelled : agent,
          )
        : [...current, cancelled];
    }
    case "agentRosterUpdated": {
      // Merge live per-agent metrics from the daemon roster, preserving any
      // taskTitle/status set by the spawn/complete events; append unseen rows.
      // Roster rows may carry parentSubSessionId/depth/owner/taskEdge etc when
      // daemon emits A-05 fields; fall back to existing when absent.
      const byId = new Map(current.map((a) => [a.subSessionId, a]));
      for (const row of event.agents) {
        const existing = byId.get(row.subSessionId);
        const raw = row as unknown as Record<string, unknown> & {
          parentSubSessionId?: string;
          depth?: number;
          owner?: string;
          taskEdge?: string;
          dependencyEdge?: string;
          worktree?: string;
          authority?: string;
          lifecycle?: string;
          model?: string;
          providerId?: string;
          vendorSessionId?: string;
          isolationMode?: string;
        };
        const rosterTerminalStatus: SubAgentStatus =
          row.status === "cancelled"
            ? "cancelled"
            : row.status === "done" || row.status === "failed"
              ? "completed"
              : "running";
        const status: SubAgentStatus =
          existing?.status === "completed" || existing?.status === "cancelled"
            ? existing.status
            : rosterTerminalStatus;
        byId.set(row.subSessionId, {
          subSessionId: row.subSessionId,
          role: row.role,
          taskTitle: existing?.taskTitle ?? row.role,
          status,
          startedAt: existing?.startedAt ?? now,
          success:
            existing?.success ??
            (row.status === "done" ? true : row.status === "failed" ? false : undefined),
          summary: existing?.summary,
          cancelledBy: existing?.cancelledBy,
          parentSubSessionId: raw.parentSubSessionId ?? existing?.parentSubSessionId,
          depth: raw.depth ?? existing?.depth,
          owner: raw.owner ?? existing?.owner,
          taskEdge: raw.taskEdge ?? existing?.taskEdge,
          dependencyEdge: raw.dependencyEdge ?? existing?.dependencyEdge,
          worktree: raw.worktree ?? existing?.worktree,
          authority: raw.authority ?? existing?.authority,
          lifecycle: raw.lifecycle ?? existing?.lifecycle,
          lastUpdatedAt: now,
          model: raw.model ?? existing?.model,
          providerId: raw.providerId ?? existing?.providerId,
          vendorSessionId: raw.vendorSessionId ?? existing?.vendorSessionId,
          isolationMode: raw.isolationMode ?? existing?.isolationMode,
          tokensIn: row.tokensIn,
          tokensOut: row.tokensOut,
          cacheRead: row.cacheRead,
          toolUses: row.toolUses,
          elapsedMs: row.elapsedMs,
          lastTools: row.lastTools,
        });
      }
      return Array.from(byId.values());
    }
    default:
      return current;
  }
}

export function subAgentStatusForEvent(event: ProtocolEvent): string | null {
  return event.$kind === "subAgentSpawned" ? `Working: ${event.role}` : null;
}

// Running agents on top, then completed (success before failure), cancelled last.
// Array.sort is stable, so spawn order is preserved within each rank.
function statusRank(a: SubAgentState): number {
  if (a.status === "running") return 0;
  if (a.status === "completed") return a.success === false ? 2 : 1;
  return 3; // cancelled
}

export function sortAgents(agents: SubAgentState[]): SubAgentState[] {
  return [...agents].sort((a, b) => statusRank(a) - statusRank(b));
}

export function useSubAgents({ config, formatError, onStatus }: UseSubAgentsOptions) {
  const [subAgents, setSubAgents] = useState<SubAgentState[]>([]);
  const sortedSubAgents = useMemo(() => sortAgents(subAgents), [subAgents]);

  const applySubAgentEvent = useCallback((event: ProtocolEvent) => {
    setSubAgents((current) => reduceSubAgents(current, event));
    return subAgentStatusForEvent(event);
  }, []);

  const resetSubAgents = useCallback(() => {
    setSubAgents([]);
  }, []);

  const cancelSubAgent = useCallback(
    async (subSessionId: string) => {
      if (!config) return;
      try {
        await interruptRun(config, subSessionId);
      } catch (err) {
        onStatus(formatError(err, "Cancel sub-agent failed"));
      }
    },
    [config, formatError, onStatus],
  );

  const cancelAllSubAgents = useCallback(async () => {
    if (!config) return;
    const running = sortedSubAgents.filter((agent) => agent.status === "running");
    if (running.length === 0) return;

    onStatus(`Stopping ${running.length} background agent${running.length === 1 ? "" : "s"}...`);
    const results = await Promise.allSettled(
      running.map((agent) => interruptRun(config, agent.subSessionId)),
    );
    const failed = results.find((result) => result.status === "rejected");
    if (failed?.status === "rejected") {
      onStatus(formatError(failed.reason, "Stop all agents failed"));
      return;
    }
    onStatus(
      `Stop requested for ${running.length} background agent${running.length === 1 ? "" : "s"}`,
    );
  }, [config, formatError, onStatus, sortedSubAgents]);

  return {
    subAgents: sortedSubAgents,
    applySubAgentEvent,
    cancelSubAgent,
    cancelAllSubAgents,
    resetSubAgents,
  };
}
