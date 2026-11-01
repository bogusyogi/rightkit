/**
 * Edit validation, BOM handling, and line-ending helpers shared by
 * the buffer, change map, and history.
 *
 * Invariants enforced here (each is unit-tested in Packet 01):
 *
 *  - edits are sorted ascending by `from`, then `to`
 *  - negative offsets are rejected
 *  - `to < from` is rejected
 *  - `to > logicalSource.length` is rejected
 *  - overlaps are rejected (touching is permitted)
 *  - two zero-width inserts at the same offset are rejected unless
 *    represented as a single combined edit
 *  - edits never split a UTF-16 surrogate pair
 *  - the BOM at logical offset 0 is preserved unless explicitly
 *    overwritten
 *
 * Invalid commands throw a {@link MarkdownEditValidationError}; they
 * never clamp or silently drop.
 */

import {
  MarkdownEditValidationError,
  type MarkdownInheritMode,
  type MarkdownLineEnding,
  type MarkdownTextEdit,
  type MarkdownTextParts,
} from "./types.js";

/**
 * Returns true if `offset` is a legal JS UTF-16 code-unit boundary
 * in `text`. Always true for `offset <= 0` or `offset >= text.length`.
 */
export function isUtf16Boundary(text: string, offset: number): boolean {
  if (offset <= 0 || offset >= text.length) return true;
  const prev = text.charCodeAt(offset - 1);
  const next = text.charCodeAt(offset);
  return !(prev >= 0xd800 && prev <= 0xdbff && next >= 0xdc00 && next <= 0xdfff);
}

/**
 * Counts the number of `\n` characters in `text`. CR and CRLF endings
 * are normalized before this is called, so this counts *logical*
 * newlines.
 */
export function countLogicalNewlines(text: string): number {
  let count = 0;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) count++;
  }
  return count;
}

/**
 * Returns the offsets of every logical newline (`\n`) in `text`, in
 * ascending order. The buffer uses this to map source intervals to
 * line-ending-vector indices via binary search.
 */
export function logicalNewlineOffsets(text: string): number[] {
  const offsets: number[] = [];
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) offsets.push(i);
  }
  return offsets;
}

/**
 * Picks a preferred line-ending style given a tally and the first
 * encountered style. Tie-breaking: highest count, then first
 * encountered, then `\n` for documents without any newlines.
 */
export function choosePreferredLineEnding(
  counts: ReadonlyMap<MarkdownLineEnding, number>,
  first: MarkdownLineEnding | null,
): MarkdownLineEnding {
  let best: MarkdownLineEnding = "\n";
  let bestCount = -1;
  for (const candidate of ["\n", "\r\n", "\r"] as const) {
    const c = counts.get(candidate) ?? 0;
    if (c > bestCount) {
      best = candidate;
      bestCount = c;
    }
  }
  if (bestCount === 0) return "\n";
  void first; // first-seen tie-break is the iteration order above
  return best;
}

/**
 * Splits a raw source string into its logical source and an explicit
 * line-ending vector. The BOM is retained at logical offset 0 if
 * present.
 */
export function splitRawMarkdownSource(rawSource: string): MarkdownTextParts {
  const hasBom = rawSource.startsWith("\uFEFF");
  const body = hasBom ? rawSource.slice(1) : rawSource;
  let logicalSource = hasBom ? "\uFEFF" : "";
  const lineEndings: MarkdownLineEnding[] = [];
  const counts = new Map<MarkdownLineEnding, number>([
    ["\n", 0],
    ["\r\n", 0],
    ["\r", 0],
  ]);
  let first: MarkdownLineEnding | null = null;

  for (let i = 0; i < body.length; i++) {
    const char = body[i];
    if (char === "\r" && body[i + 1] === "\n") {
      logicalSource += "\n";
      lineEndings.push("\r\n");
      counts.set("\r\n", (counts.get("\r\n") ?? 0) + 1);
      first ??= "\r\n";
      i++;
      continue;
    }
    if (char === "\r") {
      logicalSource += "\n";
      lineEndings.push("\r");
      counts.set("\r", (counts.get("\r") ?? 0) + 1);
      first ??= "\r";
      continue;
    }
    if (char === "\n") {
      logicalSource += "\n";
      lineEndings.push("\n");
      counts.set("\n", (counts.get("\n") ?? 0) + 1);
      first ??= "\n";
      continue;
    }
    logicalSource += char;
  }

  const preferredLineEnding = choosePreferredLineEnding(counts, first);
  return {
    logicalSource,
    lineEndings,
    preferredLineEnding,
    hasBom,
  };
}

/**
 * Re-materializes the raw source string from a logical source and an
 * explicit line-ending vector. Asserts that the vector length matches
 * the number of logical newlines; throws otherwise.
 */
export function materializeMarkdownSource(
  logicalSource: string,
  lineEndings: readonly MarkdownLineEnding[],
): string {
  const nlCount = countLogicalNewlines(logicalSource);
  if (nlCount !== lineEndings.length) {
    throw new Error(
      `materializeMarkdownSource: newline count mismatch (${nlCount} vs ${lineEndings.length})`,
    );
  }
  let endingIdx = 0;
  return logicalSource.replace(/\n/g, () => lineEndings[endingIdx++]);
}

/**
 * Sorts edits ascending by `from`, then `to`. Stable: identical
 * (from, to) pairs preserve insertion order. Does not mutate the
 * input array.
 */
export function sortMarkdownEdits(edits: readonly MarkdownTextEdit[]): MarkdownTextEdit[] {
  const indexed = edits.map((edit, index) => ({ edit, index }));
  indexed.sort((a, b) => {
    if (a.edit.from !== b.edit.from) return a.edit.from - b.edit.from;
    if (a.edit.to !== b.edit.to) return a.edit.to - b.edit.to;
    return a.index - b.index;
  });
  return indexed.map((entry) => entry.edit);
}

/**
 * Validates a batch of edits against the logical source. Returns the
 * sorted, validated edits; throws on any violation.
 */
export function validateMarkdownEdits(
  logicalSource: string,
  edits: readonly MarkdownTextEdit[],
): MarkdownTextEdit[] {
  const sorted = sortMarkdownEdits(edits);
  for (let i = 0; i < sorted.length; i++) {
    const edit = sorted[i];
    if (edit.from < 0 || edit.to < 0) {
      throw new MarkdownEditValidationError(`edit ${i}: negative offset`, "negative-offset", {
        edit,
      });
    }
    if (edit.to < edit.from) {
      throw new MarkdownEditValidationError(
        `edit ${i}: backwards range (to < from)`,
        "backwards-range",
        { edit },
      );
    }
    if (edit.to > logicalSource.length) {
      throw new MarkdownEditValidationError(
        `edit ${i}: out-of-bounds to > logicalSource.length`,
        "out-of-bounds",
        { edit, length: logicalSource.length },
      );
    }
    if (!isValidInheritMode(edit.inheritMode)) {
      throw new MarkdownEditValidationError(
        `edit ${i}: invalid inheritMode`,
        "invalid-inherit-mode",
        { edit },
      );
    }
    if (!isUtf16Boundary(logicalSource, edit.from)) {
      throw new MarkdownEditValidationError(
        `edit ${i}: from splits a surrogate pair`,
        "split-surrogate",
        { edit },
      );
    }
    if (!isUtf16Boundary(logicalSource, edit.to)) {
      throw new MarkdownEditValidationError(
        `edit ${i}: to splits a surrogate pair`,
        "split-surrogate",
        { edit },
      );
    }
  }
  // Detect overlaps. Touching ranges (`prev.to === curr.from`) are
  // permitted; two zero-width inserts at the same offset are also
  // permitted (they represent two cursors at the same point, like
  // typing two characters at the cursor position).
  for (let i = 1; i < sorted.length; i++) {
    const prev = sorted[i - 1];
    const curr = sorted[i];
    if (curr.from < prev.to) {
      throw new MarkdownEditValidationError(
        `edit ${i}: overlaps with edit ${i - 1}`,
        "overlapping",
        { prev, curr },
      );
    }
  }
  return sorted;
}

function isValidInheritMode(value: unknown): value is MarkdownInheritMode {
  return value === "preserve" || value === "document" || value === "inherit";
}

/**
 * Splits `insert` into a logical payload plus a line-ending vector.
 * Used by the buffer when applying edits with `inheritMode: "preserve"`.
 */
export function splitInsertion(insert: string): {
  logical: string;
  endings: MarkdownLineEnding[];
} {
  const parts = splitRawMarkdownSource(insert);
  return {
    logical: hasLeadingBom(parts.logicalSource, parts.hasBom)
      ? parts.logicalSource
      : parts.logicalSource,
    endings: [...parts.lineEndings],
  };
}

function hasLeadingBom(logicalSource: string, hasBom: boolean): boolean {
  return hasBom && logicalSource.startsWith("\uFEFF");
}

/**
 * Resolves the per-newline line-ending tokens for `insert`, given the
 * inherit mode, the document's preferred ending, and the removed
 * endings vector (relevant only for `"inherit"`).
 */
export function resolveInsertionEndings(
  parsedEndings: readonly MarkdownLineEnding[],
  removedEndings: readonly MarkdownLineEnding[],
  preferred: MarkdownLineEnding,
  mode: MarkdownInheritMode,
): MarkdownLineEnding[] {
  if (mode === "preserve") return [...parsedEndings];
  if (mode === "document") {
    return parsedEndings.map(() => preferred);
  }
  // inherit: reuse removed endings in order, falling back to preferred.
  const out: MarkdownLineEnding[] = [];
  for (let i = 0; i < parsedEndings.length; i++) {
    out.push(removedEndings[i] ?? preferred);
  }
  return out;
}
