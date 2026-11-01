function formatTokenCount(value: number) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}m`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return String(value);
}
export default function ContextUsageMeter({
  contextPercent,
  contextTokens,
  promptTokens,
  completionTokens,
  cacheReadTokens,
  cacheCreationTokens,
  costUsd,
}: {
  contextPercent: number;
  contextTokens: number;
  promptTokens: number;
  completionTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number;
}) {
  const usage = {
    contextPercent: Math.max(0, Math.min(1, contextPercent)),
    contextTokens: Math.max(0, contextTokens),
    promptTokens: Math.max(0, promptTokens),
    completionTokens: Math.max(0, completionTokens),
    cacheTokens: Math.max(0, cacheReadTokens + cacheCreationTokens),
  };
  const percent = Math.round(usage.contextPercent * 100);
  const meterTone =
    usage.contextPercent > 0.8
      ? "bg-accent"
      : usage.contextPercent > 0.5
        ? "bg-warning"
        : "bg-accent-2";
  const hasUsage =
    usage.contextTokens > 0 ||
    usage.promptTokens > 0 ||
    usage.completionTokens > 0 ||
    usage.cacheTokens > 0 ||
    costUsd > 0;

  // Nothing meaningful to show until the first usage update — keep the composer
  // calm instead of a row of zeros + "waiting".
  if (!hasUsage) return null;

  return (
    <div className="grid gap-1.5 px-3 py-2 border-t border-line bg-[color-mix(in_srgb,var(--color-panel-2)_52%,transparent)]">
      <div className="grid grid-cols-[auto_minmax(80px,1fr)_auto] items-center gap-2 text-[11px] text-text-3">
        <span className="font-medium text-text-2">Context budget</span>
        {/* biome-ignore lint/a11y/useSemanticElements: this custom visual meter needs exact existing styling. */}
        <div
          className="h-1.5 min-w-20 overflow-hidden rounded-full bg-bg border border-line"
          role="meter"
          aria-label="Context budget used"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          title={`Context budget ${percent}%`}
        >
          <span
            className={`block h-full w-full origin-left rounded-full transition-transform duration-150 ${meterTone}`}
            style={{ transform: `scaleX(${percent / 100})` }}
          />
        </div>
        <span className="font-mono tabular-nums">{percent}%</span>
      </div>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-text-3">
        <span>Prompt {formatTokenCount(usage.promptTokens)}</span>
        <span>Completion {formatTokenCount(usage.completionTokens)}</span>
        <span>Cache {formatTokenCount(usage.cacheTokens)}</span>
        {usage.contextTokens > 0 ? (
          <span>Context {formatTokenCount(usage.contextTokens)}</span>
        ) : null}
        <span className="font-mono tabular-nums">Est. ${costUsd.toFixed(4)}</span>
        {!hasUsage ? <span>Waiting for first usage update</span> : null}
      </div>
    </div>
  );
}
