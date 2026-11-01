/**
 * The chat a route token points at: a stored session id, `'__new__'` for the
 * new-chat route, or null for a route that is not a chat. This keeps route
 * comparison about the selected chat rather than overlay query/hash state.
 *
 * the host app shell currently keeps the selected session in React state rather
 * than a router package. The parser therefore accepts the stable URL forms
 * used by the desktop/deep-link seam (`/session/:id`, `/sessions/:id`, and
 * `/c/:id`) while remaining conservative for settings and pane URLs.
 */
export type RouteTarget = string | null;

const NEW_CHAT_PATHS = new Set(["/new", "/new-chat", "/session/new", "/sessions/new"]);
const SESSION_PATH = /^\/?(?:session|sessions|c)\/(.+)$/;

/**
 * Reduce a route token to the chat it targets. Tokens are
 * `${pathname}:${search}:${hash}`; search and hash carry pane state and do not
 * select a different session. A plain pathname is accepted for callers that
 * do not append the empty search/hash fields.
 */
export function routeTargetFromToken(token: string): RouteTarget {
  const separator = token.indexOf(":");
  const rawPathname = separator === -1 ? token : token.slice(0, separator);
  const pathname = rawPathname.split(/[?#]/, 1)[0] || "/";

  if (NEW_CHAT_PATHS.has(pathname)) return "__new__";

  const match = pathname.match(SESSION_PATH);
  if (!match?.[1]) return null;

  try {
    const sessionId = decodeURIComponent(match[1]);
    return sessionId || null;
  } catch {
    // Malformed URL encoding must not turn an unknown route into a session
    // target. The selection guard remains available for this navigation.
    return null;
  }
}

interface SessionContextDriftArgs {
  startRouteToken: string;
  nowRouteToken: string;
  startSelectedStoredId: string | null;
  nowSelectedStoredId: string | null;
  /** The stored session this submit is bound to, when known. */
  submitTargetStoredId?: string | null;
  /** The composer scope loaded when the text was submitted. */
  composerScope?: string | null;
  /** The submit target resolved to the composer's lineage-root scope. */
  submitTargetComposerScope?: string | null;
}

/**
 * Decide whether the session context genuinely changed under an in-flight
 * submit. Programmatic churn is deliberately ignored: selection null-resets
 * during reconnect/profile changes, route search/hash changes, and re-homing
 * onto the submit target are not user switches. A returned reason is suitable
 * for a redacted abort log; null means the submit may continue.
 */
export function sessionContextDrift({
  startRouteToken,
  nowRouteToken,
  startSelectedStoredId,
  nowSelectedStoredId,
  submitTargetStoredId,
  composerScope,
  submitTargetComposerScope,
}: SessionContextDriftArgs): string | null {
  // The composer and session refs can each be internally consistent while
  // disagreeing at send time. Compare lineage-root scopes, not a compressed
  // live-tip id, so compression does not create a false positive.
  if (
    composerScope !== undefined &&
    composerScope !== null &&
    composerScope !== submitTargetComposerScope
  ) {
    return `composer:${composerScope}->${submitTargetComposerScope}`;
  }

  const targetStart = routeTargetFromToken(startRouteToken);
  const targetNow = routeTargetFromToken(nowRouteToken);

  // A null target is a non-chat/settings navigation and is intentionally not
  // treated as a different chat. A move to the submit target is the pipeline's
  // own create/resume re-home, not drift.
  if (targetNow !== targetStart && targetNow !== null && targetNow !== submitTargetStoredId) {
    return `route:${targetStart}->${targetNow}`;
  }

  // Null selection is the expected transient state during reconnect/profile
  // switches. Only a different non-null stored session is a real switch.
  if (
    nowSelectedStoredId !== null &&
    nowSelectedStoredId !== startSelectedStoredId &&
    nowSelectedStoredId !== submitTargetStoredId
  ) {
    return `selection:${startSelectedStoredId}->${nowSelectedStoredId}`;
  }

  return null;
}
