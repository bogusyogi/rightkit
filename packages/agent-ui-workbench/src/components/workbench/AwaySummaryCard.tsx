import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { AwaySummary } from "@rightkit/agent-ui-core/lib/types";

export default function AwaySummaryCard({
  summary,
  pending,
  available,
  onRefresh,
  onDismiss,
}: {
  summary: AwaySummary | null;
  pending: boolean;
  available: boolean;
  onRefresh: () => void;
  onDismiss?: () => void;
}) {
  if (!summary && !available) return null;
  return (
    <article
      className="self-start flex w-full max-w-[80%] flex-col gap-1.5 rounded-xl border border-line bg-panel p-3"
      data-testid="away-summary-card"
    >
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-0 flex-1">
          <h3 className="text-[13px] font-semibold text-text">Return digest</h3>
          <p className="mt-0.5 text-[11px] text-text-3">
            {summary?.hasActivity
              ? `${summary.messageCount} updates since the last digest`
              : available
                ? "New session activity is ready to summarize"
                : "No new session activity"}
          </p>
        </div>
        <button
          type="button"
          className="inline-flex h-7 items-center gap-1.5 rounded-lg border border-line bg-bg px-2.5 text-[12px] text-text-2 transition-colors hover:border-line-2 hover:text-text disabled:cursor-wait disabled:opacity-50"
          disabled={pending}
          onClick={onRefresh}
          data-testid="away-summary-refresh"
        >
          <Icon name="refresh" size={13} />
          {pending ? "Refreshing" : "Refresh"}
        </button>
        {onDismiss ? (
          <button
            type="button"
            aria-label="Dismiss return digest"
            title="Dismiss"
            className="cr-control-pill grid h-7 w-7 place-items-center rounded-lg border border-transparent text-text-3 hover:bg-panel-2 hover:text-text"
            onClick={onDismiss}
          >
            <Icon name="close" size={12} />
          </button>
        ) : null}
      </div>
      {summary?.bullets.length ? (
        <ul className="mt-2 flex flex-col gap-1.5 pl-4 text-[12px] leading-relaxed text-text-2">
          {summary.bullets.map((bullet) => (
            <li key={bullet}>{bullet}</li>
          ))}
        </ul>
      ) : null}
    </article>
  );
}
