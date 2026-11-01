/**
 * Core type definitions for the Markdown architecture v2 pipeline.
 *
 * These types are the canonical contract between the source buffer,
 * the change-map projection, and the shared history model. They are
 * intentionally framework-free: no React, no ProseMirror, no
 * CodeMirror. Downstream packets project these primitives into the
 * editor surfaces.
 *
 * Packet 01 establishes this contract; downstream packets depend on
 * it being frozen in shape, even as methods evolve.
 */

/**
 * A line-ending vector entry. Each value is the literal byte
 * sequence used to terminate a line in the *raw* source.
 *
 * - `"\n"`    — LF (Unix/macOS)
 * - `"\r\n"`  — CRLF (Windows)
 * - `"\r"`    — CR (legacy classic Mac)
 */
export type MarkdownLineEnding = "\n" | "\r\n" | "\r";

/**
 * A request to edit the logical source. `from` and `to` are JS
 * UTF-16 offsets in the logical source; the edit replaces the half-open
 * interval `[from, to)` with `insert`.
 *
 * `insert` may itself contain newlines. Their line-ending tokens are
 * resolved at apply time using the `inheritMode` policy below.
 */
export interface MarkdownTextEdit {
  readonly from: number;
  readonly to: number;
  readonly insert: string;
  /**
   * How inserted newlines should be encoded into the raw output.
   *
   * - `"preserve"`  — use the line endings parsed from `insert`
   * - `"document"`  — use the document's `preferredLineEnding`
   * - `"inherit"`   — reuse removed ending tokens in order, falling
   *                   back to the document's preferred ending
   */
  readonly inheritMode: MarkdownInheritMode;
}

/**
 * A line-ending-resolution strategy for inserted text. See
 * {@link MarkdownTextEdit} for semantics.
 */
export type MarkdownInheritMode = "preserve" | "document" | "inherit";

/**
 * An edit whose `insert` has been resolved into a raw payload. This
 * is what the change-map and history record; it is the unit of work
 * once decisions about line endings and BOM handling are made.
 */
export interface MarkdownResolvedTextEdit {
  readonly from: number;
  readonly to: number;
  /** Raw payload to insert at `[from, to)`. */
  readonly insert: string;
  /**
   * Per-newline line-ending tokens for `insert`, in logical-newline
   * order. Length must equal `countLogicalNewlines(insert)`.
   */
  readonly insertLineEndings: readonly MarkdownLineEnding[];
  /** True if the edit preserves an existing BOM at offset 0. */
  readonly retainsLeadingBom: boolean;
}

/**
 * The result of decomposing raw Markdown into its logical source and
 * an explicit line-ending vector.
 */
export interface MarkdownTextParts {
  /** The logical source. Newlines are always `\n`. */
  readonly logicalSource: string;
  /** Per-newline ending tokens, in source order. */
  readonly lineEndings: readonly MarkdownLineEnding[];
  /** The dominant or fallback line-ending style. */
  readonly preferredLineEnding: MarkdownLineEnding;
  /** True if the raw source began with a UTF-8 BOM. */
  readonly hasBom: boolean;
}

/**
 * A single applied edit plus its derived change-map, used by the
 * history model to record a complete command.
 */
export interface MarkdownAppliedCommand {
  /**
   * The resolved edits that were applied, in document order.
   */
  readonly resolved: readonly MarkdownResolvedTextEdit[];
  /**
   * The inverse edits, expressed against the new document state, in
   * document order. Applying these undoes the command exactly.
   */
  readonly inverse: readonly MarkdownResolvedTextEdit[];
  /**
   * The change-map describing how positions in the old document map
   * to the new document.
   */
  readonly changeMap: MarkdownChangeMapSummary;
  /**
   * Where the command originated. The history model only coalesces
   * commands from the same origin class.
   */
  readonly origin: MarkdownChangeOrigin;
  /** Optional label describing what kind of edit this is. */
  readonly label: MarkdownHistoryLabel;
  /**
   * If non-empty and equal across two adjacent commands within
   * `coalesceWindowMs`, the history model merges them.
   */
  readonly coalesceKey: string;
  /** If true, the history model never coalesces this command. */
  readonly isolateHistory: boolean;
  /** Wall-clock time the command was applied. */
  readonly timestampMs: number;
  /**
   * Sum of (insert length minus removed length) plus newline counts
   * across forward and inverse edits. Used by the history model for
   * coalescing and budget tracking.
   */
  readonly changedCodeUnits: number;
}

/**
 * Coarse origin class. "external" never coalesces.
 */
export type MarkdownChangeOrigin =
  | "source"
  | "hybrid"
  | "external"
  | "diagnostics-fix"
  | "block-editor"
  | "selection-only";

/**
 * Editor-facing label. Used for coalescing and undo menu grouping.
 */
export type MarkdownHistoryLabel =
  | "Typing"
  | "Delete"
  | "Replace"
  | "Paste"
  | "Cut"
  | "Indent"
  | "Outdent"
  | "Format"
  | "BlockEdit"
  | "Table"
  | "Edit"
  | "Unknown";

/**
 * A run of commands grouped into one user-visible undo step. The
 * history model owns the ordering; downstream UI renders one group
 * per undo button press.
 */
export interface MarkdownHistoryGroup {
  readonly commands: readonly MarkdownAppliedCommand[];
  /** Total removed+inserted logical code units, summed across commands. */
  readonly changedCodeUnits: number;
  /** First timestamp in the group (ms). */
  readonly startedAtMs: number;
  /** Last timestamp in the group (ms). */
  readonly endedAtMs: number;
}

/**
 * Reduced form of {@link MarkdownChangeMap} for storage inside a
 * {@link MarkdownAppliedCommand}. Excludes the methods so the value
 * remains a plain JSON-serializable record.
 */
export interface MarkdownChangeMapSummary {
  readonly oldLength: number;
  readonly newLength: number;
  readonly changes: readonly {
    readonly fromA: number;
    readonly toA: number;
    readonly fromB: number;
    readonly toB: number;
  }[];
}

/**
 * Per-command history limits. Defaults are exported as
 * {@link DEFAULT_MARKDOWN_HISTORY_LIMITS}.
 */
export interface MarkdownHistoryLimits {
  /** Maximum number of complete groups retained in the undo stack. */
  readonly maxGroups: number;
  /** Maximum summed changed code units across all retained groups. */
  readonly maxChangedCodeUnits: number;
  /** Maximum elapsed time for two commands to coalesce into one group. */
  readonly coalesceWindowMs: number;
}

/**
 * Conservative defaults for {@link MarkdownHistoryLimits}. Chosen to
 * match ProseMirror's `HistoryConfig` defaults but expressed in
 * absolute code units.
 */
export const DEFAULT_MARKDOWN_HISTORY_LIMITS: MarkdownHistoryLimits = {
  maxGroups: 200,
  maxChangedCodeUnits: 8 * 1024 * 1024,
  coalesceWindowMs: 750,
};

/**
 * Error thrown when an edit is rejected by the validator. Always
 * deterministic; never thrown for transient reasons.
 */
export class MarkdownEditValidationError extends Error {
  constructor(
    message: string,
    readonly code: MarkdownEditValidationCode,
    readonly details?: Record<string, unknown>,
  ) {
    super(message);
    this.name = "MarkdownEditValidationError";
  }
}

/**
 * Stable error codes so callers can branch on type rather than text.
 */
export type MarkdownEditValidationCode =
  | "negative-offset"
  | "backwards-range"
  | "out-of-bounds"
  | "overlapping"
  | "duplicate-insertion"
  | "split-surrogate"
  | "invalid-inherit-mode";

/**
 * Placeholder type so the module exports compile independently of
 * implementation files. The real change-map class lives in
 * `MarkdownChangeMap.ts`.
 */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type
export interface IMarkdownChangeMap {
  readonly oldLength: number;
  readonly newLength: number;
}
