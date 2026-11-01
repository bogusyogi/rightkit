// APP-SES-013 — the shared session status marker.
//
// This is the Hermes Agent SessionStatusDot port. Sidebar, task switcher, and
// future pane tabs should render this one primitive so attention, background
// work, and unread state cannot drift between surfaces.

import { useSessionAttentionStore, attentionStateForSession, type SessionAttentionState } from "@rightkit/agent-ui-core/state/sessionAttentionStore";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";

const DOT_BASE = "h-1.5 w-1.5 shrink-0 rounded-full";

const DOT_VARIANTS: Record<SessionAttentionState, { className: string; label?: string }> = {
  "needs-input": { className: `${DOT_BASE} bg-warning`, label: "Needs input" },
  draft: { className: `${DOT_BASE} border border-text-3/50`, label: "Draft session" },
  running: { className: `${DOT_BASE} bg-accent-2`, label: "Running" },
  failed: { className: `${DOT_BASE} bg-danger`, label: "Failed" },
  background: { className: `${DOT_BASE} border border-accent-2`, label: "Background work" },
  unread: { className: `${DOT_BASE} bg-accent`, label: "Finished — unread" },
  idle: { className: `${DOT_BASE} bg-text-3/60` },
};

export interface SessionStatusDotProps {
  /** The durable daemon session id. Runtime ids must not be passed here. */
  storedSessionId: string | null;
  /** Optional row status used until the event projection has published. */
  session?: SessionRow | null;
  /** Optional explicit fallback for a caller rendering a non-row surface. */
  status?: string | null;
  /** TUI-style tree stem for a branched session. */
  branchStem?: string;
  className?: string;
}

export function sessionDotClassName(state: SessionAttentionState): string {
  return DOT_VARIANTS[state].className;
}

export function SessionStatusDot({
  storedSessionId,
  session,
  status,
  branchStem,
  className,
}: SessionStatusDotProps) {
  const override = useSessionAttentionStore((state) =>
    storedSessionId ? state.bySessionId[storedSessionId] : undefined,
  );
  // Read the current store snapshot as well as the selector so static/SSR
  // rendering sees an update that happened outside a React event boundary.
  const currentOverride = storedSessionId
    ? useSessionAttentionStore.getState().bySessionId[storedSessionId]
    : undefined;
  const state = currentOverride ?? override ?? attentionStateForSession(storedSessionId, status ?? session?.status);
  const variant = DOT_VARIANTS[state];

  return (
    <span className={`flex items-center gap-0.5 ${className ?? ""}`.trim()} data-status={state}>
      {branchStem ? (
        <span aria-hidden="true" className="shrink-0 font-mono text-[10px] leading-none text-text-3">
          {branchStem}
        </span>
      ) : null}
      <span
        aria-hidden={!variant.label}
        aria-label={variant.label || undefined}
        className={variant.className}
        role="status"
        title={variant.label}
      />
    </span>
  );
}

export default SessionStatusDot;
