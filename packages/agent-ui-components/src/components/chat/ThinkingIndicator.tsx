// ThinkingIndicator — small orange/clay asterisk + status line rendered
// below an in-progress assistant message. Format: "{elapsed} · {tokens} ·
// {status}". The icon is a 6-arm asterisk glyph; the text is muted to
// match the screenshots.

export interface ThinkingIndicatorProps {
  elapsedSeconds: number;
  tokenCount: number;
  status?: string;
}

function formatElapsed(totalSeconds: number): string {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return "0s";
  if (totalSeconds < 60) return `${Math.floor(totalSeconds)}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = Math.floor(totalSeconds % 60);
  return `${minutes}m ${seconds.toString().padStart(2, "0")}s`;
}

function formatTokens(count: number): string {
  if (!Number.isFinite(count) || count < 0) return "0";
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}k`;
  return `${Math.floor(count)}`;
}

function AsteriskGlyph({ size = 13 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      aria-hidden="true"
      style={{ color: "var(--color-accent)" }}
    >
      {/* 6-arm asterisk, hand-tuned so the strokes align with the screenshot
         glyph (slightly fatter mid-stroke, tapered ends). */}
      <g
        fill="none"
        stroke="currentColor"
        strokeWidth="2.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M12 4.5v15" />
        <path d="M12 4.5l-4.95 12.1" />
        <path d="M12 4.5l4.95 12.1" />
        <path d="M6.6 8.6l10.8 6.8" />
        <path d="M6.6 15.4l10.8-6.8" />
      </g>
    </svg>
  );
}

export default function ThinkingIndicator({
  elapsedSeconds,
  tokenCount,
  status,
}: ThinkingIndicatorProps) {
  const resolvedStatus = status ?? "thinking…";
  return (
    <div
      data-testid="thinking-indicator"
      role="status"
      aria-live="polite"
      className="inline-flex items-center gap-1.5 text-[12px] text-text-3"
    >
      <AsteriskGlyph />
      <span className="font-mono tabular-nums">{formatElapsed(elapsedSeconds)}</span>
      <span className="text-text-3/60">·</span>
      <span className="font-mono tabular-nums">{formatTokens(tokenCount)} tokens</span>
      <span className="text-text-3/60">·</span>
      <span>{resolvedStatus}</span>
    </div>
  );
}

export const THINKING_INDICATOR_FORMATTERS = {
  formatElapsed,
  formatTokens,
};
