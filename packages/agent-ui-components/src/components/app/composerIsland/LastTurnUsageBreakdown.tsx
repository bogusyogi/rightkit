import { useMemo } from "react";
import type { ProtocolEvent } from "@rightkit/agent-ui-core/lib/types";
import {
  formatCost,
  formatTokenCount,
} from "@rightkit/agent-ui-core/lib/usage/usageProjection";
import {
  hasTurnUsage,
  projectLastTurnUsage,
  type TurnUsageProjection,
} from "@rightkit/agent-ui-core/lib/usage/turnUsageProjection";
import Icon from "@rightkit/agent-ui-core/components/Icon";

export interface LastTurnUsageBreakdownProps {
  /** Replayed/live the host app protocol events; usageUpdated rows are cumulative. */
  events?: readonly ProtocolEvent[];
  /** Optional pre-projected value for callers that already maintain an event ledger. */
  usage?: TurnUsageProjection | null;
}

/** Compact, honest input/output accounting for the most recently completed turn. */
export function LastTurnUsageBreakdown({
  events = [],
  usage,
}: LastTurnUsageBreakdownProps) {
  const projected = useMemo(
    () => usage ?? projectLastTurnUsage(events),
    [events, usage],
  );

  if (!projected || !hasTurnUsage(projected)) return null;

  const hasCache = projected.cacheReadTokens > 0 || projected.cacheCreationTokens > 0;
  return (
    <section
      aria-label="Last turn usage"
      className="grid gap-1.5 border-t border-line/70 pt-2 text-[11px] text-text-3"
      data-testid="last-turn-usage"
    >
      <div className="flex items-center gap-1.5 font-medium text-text-2">
        <Icon name="activity" size={12} />
        <span>Last turn</span>
      </div>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 font-mono tabular-nums">
        <span title="Input tokens">In {formatTokenCount(projected.inputTokens)}</span>
        <span title="Output tokens">Out {formatTokenCount(projected.outputTokens)}</span>
        {hasCache ? <>
          <span title="Cache read tokens">Cache read {formatTokenCount(projected.cacheReadTokens)}</span>
          <span title="Cache creation tokens">Cache write {formatTokenCount(projected.cacheCreationTokens)}</span>
        </> : null}
        <span title="Estimated cost">Est. {formatCost(projected.costUsd)}</span>
      </div>
      {projected.contextTokens > 0 ? (
        <span className="text-[10px] text-text-3">
          Context {formatTokenCount(projected.contextTokens)} · {Math.round(projected.contextPercent * 100)}%
        </span>
      ) : null}
    </section>
  );
}

export default LastTurnUsageBreakdown;
