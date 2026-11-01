// L11 backend sends the matching_message_snippet field with match spans
// wrapped in U+0001 (SOH) / U+0002 (STX) sentinels (see
// src-tauri/src/db/search/snippet.rs). This module is the ONLY place that
// parses those sentinels -- it turns the raw string into typed segments so
// the renderer never has to touch a control character, and never falls back
// to dangerouslySetInnerHTML.

export const SNIPPET_MATCH_START = "";
export const SNIPPET_MATCH_END = "";

export type SnippetSegment = { text: string; matched: boolean };

/**
 * Split a snippet string on the sentinel pair into plain/matched segments.
 * Malformed input (unbalanced or nested sentinels, a stray START with no END,
 * or an END with no preceding START) degrades gracefully: sentinel bytes are
 * stripped and the surrounding text is treated as unmatched, but no sentinel
 * byte is ever left in the output -- a raw control char must never reach the DOM.
 */
export function splitHighlightSentinels(raw: string): SnippetSegment[] {
  if (!raw) return [];
  const segments: SnippetSegment[] = [];
  let cursor = 0;
  while (cursor < raw.length) {
    const start = raw.indexOf(SNIPPET_MATCH_START, cursor);
    if (start === -1) {
      const rest = stripSentinels(raw.slice(cursor));
      if (rest) segments.push({ text: rest, matched: false });
      break;
    }
    const before = stripSentinels(raw.slice(cursor, start));
    if (before) segments.push({ text: before, matched: false });
    const end = raw.indexOf(SNIPPET_MATCH_END, start + 1);
    if (end === -1) {
      // Unterminated match -- treat the remainder as plain text, sentinel stripped.
      const rest = stripSentinels(raw.slice(start + 1));
      if (rest) segments.push({ text: rest, matched: false });
      break;
    }
    const matchText = stripSentinels(raw.slice(start + 1, end));
    if (matchText) segments.push({ text: matchText, matched: true });
    cursor = end + 1;
  }
  return segments;
}

// Split/join instead of a regex -- a literal-control-character regex trips
// eslint no-control-regex, and split/join is just as clear here.
function stripSentinels(s: string): string {
  return s.split(SNIPPET_MATCH_START).join("").split(SNIPPET_MATCH_END).join("");
}
