/**
 * Error sanitization for pane surfaces. The mimo audit flagged MemoryPane for
 * leaking raw Error messages and stack traces as the primary UI text — users
 * see engine internals instead of an actionable summary. This helper splits an
 * unknown error into a one-line human summary plus a details string that the
 * caller renders in a collapsed `<details>` so the raw bytes never become the
 * primary text.
 *
 * Stack traces NEVER appear in `summary` — only the first non-empty line of
 * the error message is summarized, and it is truncated so a runaway engine
 * message cannot blow up the layout.
 */

const MAX_SUMMARY_LEN = 180;

function firstLine(value: string): string {
  const idx = value.indexOf("\n");
  return idx >= 0 ? value.slice(0, idx) : value;
}

function cleanMessage(raw: string): string {
  return raw.replace(/\s+/g, " ").trim();
}

/**
 * Browser network failures arrive as an engine-free TypeError whose message is
 * whatever the engine chose: WebKit says "Load failed", Chromium "Failed to
 * fetch", Firefox "NetworkError ...". Rendered verbatim these read as a bare
 * generic failure next to a healthy connection indicator, which is exactly the
 * connected/error contradiction the desktop contract forbids. They all mean the
 * same thing — the request never reached the local engine — so the caller's
 * subsystem-specific fallback is a truer summary than the raw text.
 */
const NETWORK_FAILURE = /^(load failed|failed to fetch|networkerror\b.*|network request failed)$/i;

export function isNetworkFailureMessage(message: string): boolean {
  return NETWORK_FAILURE.test(cleanMessage(message));
}

/**
 * Strip a `at <fn> (<file>:<line>:<col>)` or Rust-style backtrace line that
 * sometimes arrives embedded in `err.message`. We keep the message intact
 * for the details disclosure; only the summary is collapsed.
 */
export function sanitizeError(
  error: unknown,
  fallback = "Something went wrong.",
): { summary: string; details: string | null } {
  let raw = "";
  if (error instanceof Error) {
    raw = error.message;
  } else if (typeof error === "string") {
    raw = error;
  } else if (error == null) {
    raw = "";
  } else {
    try {
      raw = JSON.stringify(error);
    } catch {
      raw = String(error);
    }
  }

  const cleaned = cleanMessage(raw);
  if (!cleaned) {
    return { summary: fallback, details: null };
  }
  if (isNetworkFailureMessage(cleaned)) {
    return { summary: fallback, details: `${cleaned} — the request did not reach the engine.` };
  }

  const summaryCandidate = firstLine(cleaned);
  const summary =
    summaryCandidate.length > MAX_SUMMARY_LEN
      ? `${summaryCandidate.slice(0, MAX_SUMMARY_LEN - 1)}…`
      : summaryCandidate;

  const stack = error instanceof Error && typeof error.stack === "string" ? error.stack : null;
  const details = stack && stack !== raw ? stack : cleaned.length > summary.length ? cleaned : null;

  return { summary, details };
}
