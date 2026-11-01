import { useEffect, useState } from "react";
import Icon from "./Icon.js";

/**
 * Shared loading surface for panes and tabs that fetch on mount or on refresh.
 * Defers rendering the spinner for 150ms so sub-150ms flashes do not flash a
 * spinner at the user — the audit spec calls this out as the #1 loading UX bug.
 * Caller sets `loading` from the actual fetch state; the defer timeout is a
 * render-only concern handled here.
 */
export interface PaneLoadingProps {
  /** True while the data is still arriving; the spinner is gated on the
   * 150ms defer so callers can flip this freely without flicker. */
  loading: boolean;
  /** Optional caption shown under the spinner. Defaults to "Loading…". */
  label?: string;
  /** Override the defer window in milliseconds. Default 150. */
  deferMs?: number;
  /** Optional className for the outer flex container. */
  className?: string;
}

export default function PaneLoading({
  loading,
  label = "Loading…",
  deferMs = 150,
  className = "",
}: PaneLoadingProps) {
  const [show, setShow] = useState(false);

  useEffect(() => {
    if (!loading) {
      setShow(false);
      return;
    }
    setShow(false);
    const t = setTimeout(() => setShow(true), deferMs);
    return () => clearTimeout(t);
  }, [loading, deferMs]);

  if (!loading || !show) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      aria-busy="true"
      aria-label={label}
      className={`flex flex-1 min-h-0 items-center justify-center gap-3 text-text-3 ${className}`}
      data-testid="pane-loading"
    >
      <span aria-hidden="true" className="inline-grid place-items-center text-accent-2">
        <Icon name="refresh" size={16} />
      </span>
      <span className="text-[13px]">{label}</span>
    </div>
  );
}
