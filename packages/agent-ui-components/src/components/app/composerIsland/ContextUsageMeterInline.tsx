function formatTokens(tokens: number) {
  if (!tokens) return "0";
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(2)}m`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return String(tokens);
}

export function ContextUsageMeterInline(props: {
  contextPercent: number;
  contextTokens: number;
  promptTokens: number;
  completionTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number;
}) {
  const clamp = (n: number) => Math.max(0, Math.min(1, n));
  const percent = Math.round(clamp(props.contextPercent) * 100);
  const cacheTokens = props.cacheReadTokens + props.cacheCreationTokens;
  return (
    <div className="grid gap-2 text-[11px] text-text-3">
      <div className="grid grid-cols-[auto_minmax(80px,1fr)_auto] items-center gap-2">
        <span className="font-medium text-text-2">Context budget</span>
        {/* biome-ignore lint/a11y/useSemanticElements: styled progress bar; role="meter" is correct ARIA and the native <meter> element cannot be styled to match. */}
        <div
          className="h-1.5 min-w-20 overflow-hidden rounded-full border border-line bg-bg"
          role="meter"
          aria-label="Context budget used"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          title={`Context budget ${percent}%`}
        >
          <span
            className="block h-full rounded-full bg-accent-2"
            style={{ width: `${percent}%` }}
          />
        </div>
        <span className="font-mono tabular-nums">{percent}%</span>
      </div>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span>Prompt {formatTokens(props.promptTokens)}</span>
        <span>Completion {formatTokens(props.completionTokens)}</span>
        <span>Cache {formatTokens(cacheTokens)}</span>
        {props.contextTokens > 0 ? <span>Context {formatTokens(props.contextTokens)}</span> : null}
        <span className="font-mono tabular-nums">Est. ${props.costUsd.toFixed(4)}</span>
      </div>
    </div>
  );
}
