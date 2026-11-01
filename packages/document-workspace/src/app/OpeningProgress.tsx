import { useEffect, useState } from "react";

/** Delay before the indicator appears, so fast opens never flash a progress bar. */
export const OPENING_PROGRESS_DELAY_MS = 300;

interface OpeningProgressProps {
  fileName: string;
  delayMs?: number;
}

/**
 * Progress indicator for a document load that is still running. Mounted only while a load is in
 * flight; it stays hidden until the load has lasted `delayMs`, then shows an indeterminate bar.
 * The backend read reports no byte counts, so the bar makes no percentage claim.
 */
export function OpeningProgress({ fileName, delayMs = OPENING_PROGRESS_DELAY_MS }: OpeningProgressProps) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    setVisible(false);
    const timer = window.setTimeout(() => setVisible(true), delayMs);
    return () => window.clearTimeout(timer);
  }, [delayMs]);

  if (!visible) return null;

  const label = `Opening ${fileName || "document"}`;
  return (
    <div
      role="status"
      aria-live="polite"
      className="pointer-events-none absolute inset-x-0 top-0 z-50 flex flex-col gap-1 border-b border-line bg-panel/95 px-4 py-2 font-mono text-xs text-muted shadow-sm"
    >
      <span>{label}…</span>
      <div
        role="progressbar"
        aria-label={label}
        aria-busy="true"
        className="relative h-1 w-full overflow-hidden rounded bg-line"
      >
        <div className="absolute inset-y-0 left-0 w-1/3 animate-pulse rounded bg-copper" />
      </div>
    </div>
  );
}
