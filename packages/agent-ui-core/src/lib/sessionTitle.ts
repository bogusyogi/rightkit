/**
 * The name to show for a session. Sessions started from the CLI are titled "CLI session" and are not
 * auto-titled by the daemon (that would cost a model request on every headless run), so every one of them
 * read "CLI session" in the sidebar (screen audit 2026-11-11, fault 8). A placeholder title gives way to the
 * session's first message, which the daemon already sends as `preview`.
 */
const PLACEHOLDER_TITLES = new Set(["", "cli session", "new session", "default session", "untitled session"]);

export function sessionDisplayTitle(session: { title?: string | null; preview?: string | null }): string {
  const title = (session.title ?? "").trim();
  if (!PLACEHOLDER_TITLES.has(title.toLowerCase())) return title;
  const preview = (session.preview ?? "").trim();
  if (preview) return preview;
  return title || "Untitled session";
}
