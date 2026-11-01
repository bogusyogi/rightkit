import type { ProtocolEvent } from "../types";

/** The wire shape emitted by the host app after a provider call. Totals are cumulative per session. */
export type UsageUpdatedEvent = Extract<ProtocolEvent, { $kind: "usageUpdated" }>;

export interface TurnUsageProjection {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number;
  contextTokens: number;
  contextPercent: number;
}

function nonNegative(value: number): number {
  return Number.isFinite(value) ? Math.max(0, value) : 0;
}

/**
 * Difference a cumulative usage counter from its previous boundary.
 * A decrease means the daemon/session was reset, so the current value is the
 * only trustworthy value rather than a negative turn.
 */
function counterDelta(current: number, previous: number | undefined): number {
  const now = nonNegative(current);
  if (previous === undefined) return now;
  const before = nonNegative(previous);
  return now >= before ? now - before : now;
}

export function usageUpdatedEvents(events: readonly ProtocolEvent[]): UsageUpdatedEvent[] {
  return events.filter((event): event is UsageUpdatedEvent => event.$kind === "usageUpdated");
}

export function projectTurnUsage(
  current: UsageUpdatedEvent,
  previous?: UsageUpdatedEvent,
): TurnUsageProjection {
  return {
    inputTokens: counterDelta(current.promptTokens, previous?.promptTokens),
    outputTokens: counterDelta(current.completionTokens, previous?.completionTokens),
    cacheReadTokens: counterDelta(current.cacheReadTokens ?? 0, previous?.cacheReadTokens),
    cacheCreationTokens: counterDelta(
      current.cacheCreationTokens ?? 0,
      previous?.cacheCreationTokens,
    ),
    costUsd: counterDelta(current.costUsd, previous?.costUsd),
    // Context is a live reading, not a cumulative counter.
    contextTokens: nonNegative(current.contextTokens),
    contextPercent: Math.min(1, nonNegative(current.contextPercent)),
  };
}

/** Project the most recent completed provider call from replayed/live protocol events. */
export function projectLastTurnUsage(
  events: readonly ProtocolEvent[],
): TurnUsageProjection | null {
  const usage = usageUpdatedEvents(events);
  const current = usage[usage.length - 1];
  if (!current) return null;
  return projectTurnUsage(current, usage.length > 1 ? usage[usage.length - 2] : undefined);
}

export function hasTurnUsage(usage: TurnUsageProjection): boolean {
  return (
    usage.inputTokens > 0 ||
    usage.outputTokens > 0 ||
    usage.cacheReadTokens > 0 ||
    usage.cacheCreationTokens > 0 ||
    usage.costUsd > 0
  );
}
