/**
 * Contract 14 CE-3 — ordinary fenced-code model.
 *
 * A fenced code block is represented in the hybrid schema by the native
 * `code_fence` node. This module owns the pure fence model:
 *
 *  - parse a raw fenced-code source slice into language/meta/fence metadata
 *    (fence char, length, indent, trailing-newline state) plus the source
 *    sub-ranges for the info string and the content,
 *  - serialize a model back to fenced Markdown, widening the fence only when
 *    the content contains a closing sequence that requires it.
 *
 * Only *fenced* blocks (backtick or tilde fences) are representable here.
 * Indented code and malformed fences return `null` so the projection keeps
 * them on the explicit raw/protected fallback path.
 */

export interface CodeFenceModel {
  readonly language: string;
  readonly meta: string;
  readonly fenceChar: "`" | "~";
  readonly fenceLength: number;
  readonly fenceIndent: string;
  /** Text that follows the info string on the opening line (normally "\n"). */
  readonly openingSuffix: string;
  /** Whether the source has a newline before the closing fence. */
  readonly trailingNewline: boolean;
  /** Code content without the trailing line break (mdast `code.value` shape). */
  readonly content: string;
  /** Relative offsets within the block's raw source slice. */
  readonly infoFrom: number;
  readonly infoTo: number;
  readonly contentFrom: number;
  readonly contentTo: number;
}

/** Matches a fenced-code opening line: optional 0–3 indent, 3+ backticks or tildes, info string. */
const OPENING_RE = /^([ \t]{0,3})(`{3,}|~{3,})([^\n]*)(\n?)/;

function escapeFenceChar(ch: string): string {
  return ch === "`" ? "\\`" : ch;
}

/** Line content (before trailing spaces) matching a closing fence for `fenceChar`/`length`. */
function closingFenceRe(fenceChar: string, fenceLength: number): RegExp {
  return new RegExp(`^[ \\t]*${escapeFenceChar(fenceChar)}{${fenceLength},}[ \\t]*$`);
}

/**
 * Parse a raw fenced-code source slice. Returns `null` when the slice is not
 * a well-formed fenced block (indented code, malformed/unterminated fence, or
 * a closing fence shorter than the opening one).
 */
export function parseCodeFenceSource(raw: string): CodeFenceModel | null {
  const openMatch = OPENING_RE.exec(raw);
  if (!openMatch) return null;
  const fenceIndent = openMatch[1] ?? "";
  const opener = openMatch[2] ?? "";
  const infoText = openMatch[3] ?? "";
  const openingSuffix = openMatch[4] ?? "";
  if (openingSuffix !== "\n") return null;
  const fenceChar = (opener[0] === "~" ? "~" : "`") as "`" | "~";
  const fenceLength = opener.length;

  // Info string: first whitespace-delimited token is the language; the rest is meta.
  const trimmedInfo = infoText.trim();
  const spaceAt = trimmedInfo.search(/\s/);
  const language = spaceAt === -1 ? trimmedInfo : trimmedInfo.slice(0, spaceAt);
  const meta = spaceAt === -1 ? "" : trimmedInfo.slice(spaceAt).trim();

  const infoFrom = fenceIndent.length + fenceLength;
  const infoTo = infoFrom + infoText.length;
  const contentStart = infoTo + openingSuffix.length;

  // Locate the closing fence: a line whose only content is the same fence char,
  // repeated at least the opening length, with optional leading/trailing spaces.
  const closing = closingFenceRe(fenceChar, fenceLength);
  const rest = raw.slice(contentStart);
  const restLines = rest.split("\n");
  let closingLineIndex = -1;
  for (let i = 0; i < restLines.length; i++) {
    if (closing.test(restLines[i] ?? "")) {
      closingLineIndex = i;
      break;
    }
  }
  if (closingLineIndex === -1) return null;

  const contentLines = restLines.slice(0, closingLineIndex);
  // The final line before the closing fence normally ends with "\n"; drop it so
  // the content matches mdast `code.value` (no trailing line break).
  let content = contentLines.join("\n");
  let trailingNewline = true;
  if (contentLines.length > 0 && contentLines[contentLines.length - 1] === "") {
    // "…\n\n```" — content ends with a blank line; keep it (the blank line IS content).
    content = contentLines.join("\n");
    trailingNewline = true;
  } else if (contentLines.length === 0) {
    content = "";
    trailingNewline = true;
  }

  // Content region in the raw slice: [contentStart, closing line start).
  const contentTo = contentStart + restLines.slice(0, closingLineIndex).join("\n").length;

  return {
    language,
    meta,
    fenceChar,
    fenceLength,
    fenceIndent,
    openingSuffix,
    trailingNewline,
    content,
    infoFrom,
    infoTo,
    contentFrom: contentStart,
    contentTo,
  };
}

/**
 * The longest authored closing-fence line in `content`, or 0 when content
 * cannot close the fence. Inline fence characters do not close a fence and
 * must not force unnecessary fence widening.
 */
export function longestFenceRun(content: string, fenceChar: string): number {
  let longest = 0;
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || !new RegExp(`^${escapeFenceChar(fenceChar)}+$`).test(trimmed)) continue;
    longest = Math.max(longest, trimmed.length);
  }
  return longest;
}

/** Serialize a code fence. Widens the fence only when content demands it. */
export function serializeCodeFence(model: {
  language: string;
  meta: string;
  fenceChar: string;
  fenceLength: number;
  fenceIndent: string;
  /** Optional indentation applied to each authored content/closing line. */
  contentIndent?: string;
  openingSuffix: string;
  trailingNewline?: boolean;
  content: string;
}): string {
  const run = longestFenceRun(model.content, model.fenceChar);
  const length = Math.max(model.fenceLength, run + 1);
  const info = model.language + (model.meta ? ` ${model.meta}` : "");
  const closing = model.fenceChar.repeat(length);
  const contentIndent = model.contentIndent ?? "";
  const content = model.content
    .split("\n")
    .map((line) => (line ? `${contentIndent}${line}` : line))
    .join("\n");
  const closingIndent = contentIndent || model.fenceIndent;
  // The closing fence is always on its own line. content's own trailing newline
  // (an empty trailing line) is content, and the added "\n" separates it from
  // the closing fence — so a blank trailing line round-trips as a blank line.
  // trailingNewline preserves authored source: when false, the closing fence
  // follows content without an extra line break (edge case for fidelity).
  const trailingNewline = model.trailingNewline ?? true;
  const newlineBeforeClose = trailingNewline ? "\n" : "";
  return (
    model.fenceIndent +
    closing +
    info +
    model.openingSuffix +
    content +
    newlineBeforeClose +
    closingIndent +
    closing
  );
}

/** The info string as authored (used to compute the exact info-string source range). */
export function codeFenceInfoString(language: string, meta: string): string {
  return language + (meta ? ` ${meta}` : "");
}
