/**
 * Plain-text editor projection for the composer.
 *
 * Ported from OpenChamber's ComposerEditor contract. the host app keeps the
 * existing native textarea seam rather than adding CodeMirror to the desktop
 * dependency graph; the document is still a plain string and all caret,
 * composition, controlled-writeback, and grow-then-scroll rules are retained.
 */

export interface ComposerSelection {
  start: number;
  end: number;
}

export interface ComposerChange {
  value: string;
  selection: ComposerSelection;
  /** True when the edit came from a paste rather than typing. */
  fromPaste: boolean;
  /** Text inserted by this edit; empty for a deletion. */
  insertedText: string;
}

export function readComposerSelection(input: HTMLTextAreaElement): ComposerSelection {
  return { start: input.selectionStart ?? 0, end: input.selectionEnd ?? 0 };
}

/** The modern and legacy browser signals used by the donor's IME guard. */
export function isComposerComposing(event: {
  isComposing?: boolean;
  keyCode?: number;
}): boolean {
  return Boolean(event.isComposing || event.keyCode === 229);
}

/** Enter submits only when it is not an IME confirmation or Shift+Enter. */
export function shouldSubmitComposerEnter(
  event: Pick<KeyboardEvent, "key" | "shiftKey" | "isComposing" | "keyCode">,
): boolean {
  return event.key === "Enter" && !event.shiftKey && !isComposerComposing(event);
}

/**
 * Derive the inserted portion of a native input change. This mirrors
 * CodeMirror's transaction insertion reporting closely enough for the
 * textarea seam, including replacement and deletion edits.
 */
export function insertedTextBetween(previous: string, next: string): string {
  let prefix = 0;
  while (prefix < previous.length && prefix < next.length && previous[prefix] === next[prefix]) {
    prefix += 1;
  }
  let suffix = 0;
  while (
    suffix < previous.length - prefix &&
    suffix < next.length - prefix &&
    previous[previous.length - suffix - 1] === next[next.length - suffix - 1]
  ) {
    suffix += 1;
  }
  return next.slice(prefix, next.length - suffix);
}

/**
 * Controlled writeback guard from OpenChamber's ComposerEditor. Equality is
 * checked before composition so a stale parent echo never interrupts an IME.
 * External rewrites place the caret at the end, matching the old textarea.
 */
export function writeComposerValue(
  input: HTMLTextAreaElement,
  value: string,
  composing: boolean,
): boolean {
  if (input.value === value || composing) return false;
  input.value = value;
  input.setSelectionRange(value.length, value.length);
  return true;
}

export function clampComposerRows(rows: number, maxRows: number): number {
  if (!Number.isFinite(rows)) return 1;
  return Math.min(Math.max(1, Math.round(maxRows)), Math.max(1, Math.round(rows)));
}

/** Grow with content up to maxRows; once capped, the textarea itself scrolls. */
export function growComposerTextarea(
  input: HTMLTextAreaElement,
  value: string,
  maxRows = 8,
  lineHeightPx = 22,
): number {
  if (!value && !input.value) {
    input.rows = 1;
    input.style.overflowY = "hidden";
    return 1;
  }
  const measuredRows = input.scrollHeight > 0 ? input.scrollHeight / lineHeightPx : 0;
  const newlineRows = Math.max(1, value.split("\n").length);
  const rows = clampComposerRows(Math.max(measuredRows, newlineRows), maxRows);
  input.rows = rows;
  input.style.overflowY = rows >= maxRows ? "auto" : "hidden";
  return rows;
}

export function composerMaxHeight(maxRows = 8, lineHeightPx = 22): string {
  return `calc(${Math.max(1, Math.round(maxRows))} * ${lineHeightPx}px)`;
}
