/**
 * One renderer door for opening a stored session.
 *
 * This is a direct port of Hermes Agent's intent contract. the host app does not
 * own tab/window state here: the host supplies those renderer callbacks, while
 * the daemon remains the owner of session state and transcript data.
 */
export type OpenSessionIntent = "in-place" | "stack" | "tab" | "window";
export type FocusedSession = "main" | "tile" | null;
export type OpenSessionNavigate = (to: string, options?: { replace?: boolean }) => void;

export interface SessionOpenAdapter {
  activeSessionId: string | null;
  selectedStoredSessionId: string | null;
  workspaceIsPage: boolean;
  markSessionRead?: (storedSessionId: string) => void;
  focusOpenSession: (storedSessionId: string) => FocusedSession;
  openSessionTile: (storedSessionId: string, placement: "center") => void;
  reuseBlankDraftTile: (storedSessionId: string) => boolean;
  canOpenSessionWindow: () => boolean;
  openSessionInNewWindow: (storedSessionId: string) => void | Promise<void>;
  sessionRoute: (storedSessionId: string) => string;
}

/** A main tab is occupied as soon as either runtime or stored state exists. */
export function mainChatOccupied(
  activeSessionId: string | null,
  selectedStoredSessionId: string | null,
): boolean {
  return Boolean(activeSessionId || selectedStoredSessionId);
}

/** Whether a focused session requires a route back to the workspace page. */
export function focusedSessionNeedsRoute(
  focused: FocusedSession,
  workspaceIsPage: boolean,
): boolean {
  return !focused || (focused === "main" && workspaceIsPage);
}

/**
 * Read modifiers the same way session rows do: meta/control opens a stacked
 * tab, and shift plus either modifier requests a window. The caller's base
 * intent is preserved for an unmodified selection.
 */
export function openSessionIntentFromModifiers(
  event?: null | { ctrlKey?: boolean; metaKey?: boolean; shiftKey?: boolean },
  base: OpenSessionIntent = "in-place",
): OpenSessionIntent {
  if (!event) return base;

  const modifier = Boolean(event.metaKey || event.ctrlKey);
  if (modifier && event.shiftKey) return "window";
  if (modifier) return "tab";
  return base;
}

/**
 * Open or focus a session according to an explicit renderer intent.
 *
 * `navigate` is only required by the in-place path. Tab/window paths use the
 * supplied adapter callbacks and never route the current page implicitly.
 */
export function openSession(
  storedSessionId: string,
  navigate: OpenSessionNavigate,
  intent: OpenSessionIntent,
  adapter: SessionOpenAdapter,
): void;
export function openSession(
  storedSessionId: string,
  navigate: OpenSessionNavigate,
  adapter: SessionOpenAdapter,
  intent?: OpenSessionIntent,
): void;
export function openSession(
  storedSessionId: string,
  navigate: OpenSessionNavigate,
  intentOrAdapter: OpenSessionIntent | SessionOpenAdapter,
  adapterOrIntent?: SessionOpenAdapter | OpenSessionIntent,
): void {
  const intent =
    typeof intentOrAdapter === "string"
      ? intentOrAdapter
      : typeof adapterOrIntent === "string"
        ? adapterOrIntent
        : "in-place";
  const adapter =
    typeof intentOrAdapter === "string"
      ? (adapterOrIntent as SessionOpenAdapter)
      : intentOrAdapter;

  if (!storedSessionId) return;

  // This must precede focus short-circuits so opening an already-visible
  // session still clears its finished-turn marker.
  adapter.markSessionRead?.(storedSessionId);

  let resolved = intent;
  if (resolved === "window") {
    if (adapter.canOpenSessionWindow()) {
      void adapter.openSessionInNewWindow(storedSessionId);
      return;
    }
    resolved = "tab";
  }

  // An external stack open may spend a genuinely blank main surface, but it
  // must not replace a loaded or currently-running conversation.
  let spendBlankDraft = false;
  if (resolved === "stack") {
    spendBlankDraft = mainChatOccupied(
      adapter.activeSessionId,
      adapter.selectedStoredSessionId,
    );
    resolved = spendBlankDraft ? "tab" : "in-place";
  }

  if (resolved === "tab") {
    if (adapter.focusOpenSession(storedSessionId)) return;
    if (spendBlankDraft && adapter.reuseBlankDraftTile(storedSessionId)) return;
    adapter.openSessionTile(storedSessionId, "center");
    return;
  }

  // An in-place open only routes when the requested session is not already
  // visible, or when its main surface is covered by a non-workspace page.
  if (
    focusedSessionNeedsRoute(
      adapter.focusOpenSession(storedSessionId),
      adapter.workspaceIsPage,
    )
  ) {
    navigate(adapter.sessionRoute(storedSessionId));
  }
}

/** Bind the adapter once for hub callbacks without hiding its state source. */
export function createSessionOpener(adapter: SessionOpenAdapter) {
  return (
    storedSessionId: string,
    navigate: OpenSessionNavigate,
    intent: OpenSessionIntent = "in-place",
  ) => openSession(storedSessionId, navigate, intent, adapter);
}
