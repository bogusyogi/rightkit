/**
 * Public contract for {@link MarkdownDocumentCore}.
 *
 * The core is the single authority for a Markdown document's
 * logical source, its source-map (stable node IDs across edits),
 * and its shared history. Editor surfaces subscribe to the core's
 * stream and project edits back through the core's apply pipeline.
 *
 * This module declares only types and small enums; no implementation.
 * Implementation lives in `MarkdownDocumentCore.ts`.
 */

import type { MarkdownSourceMap } from "./MarkdownSourceMap.js";
import type { MarkdownNodeId } from "./MarkdownNodeIdFactory.js";
import type { MarkdownHistoryGroup, MarkdownTextEdit } from "./types.js";

/**
 * Identifier of an editor surface that subscribes to a document
 * core. Surfaces are bitmasks; multiple surfaces can be subscribed
 * to a single core simultaneously.
 */
export enum MarkdownSurfaceId {
  None = 0,
  ProseMirror = 1 << 0,
  CodeMirrorSource = 1 << 1,
  Preview = 1 << 2,
  Diagnostics = 1 << 3,
}

/**
 * Caller-supplied metadata attached to an apply operation. The
 * core forwards this verbatim into the recorded command so the
 * history model can group, coalesce, and label edits correctly.
 */
export interface MarkdownCommand {
  /** One or more logical-source edits to apply in source order. */
  readonly edits: readonly MarkdownTextEdit[];
  /** Reject when caller built this command from an older projection. */
  readonly expectedRevision?: number;
  /**
   * Where the command originated. Two commands from different
   * origins never coalesce.
   */
  readonly origin:
    | "source"
    | "hybrid"
    | "external"
    | "diagnostics-fix"
    | "block-editor"
    | "selection-only";
  /** Editor-facing label used by the undo menu. */
  readonly label:
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
   * Coalescing key. Two adjacent commands with the same non-empty
   * key within the history model's coalesce window are merged.
   */
  readonly coalesceKey?: string;
  /**
   * If true, the history model never coalesces this command even
   * if other rules would suggest it. Set by paste, undo, redo,
   * diagnostics-fix, and any block-editor commit.
   */
  readonly isolateHistory?: boolean;
  /**
   * Wall-clock time the caller observed the command. The history
   * model uses this for coalescing. Defaults to `performance.now()`
   * rounded to ms if omitted.
   */
  readonly timestampMs?: number;
}

/**
 * Summary returned from {@link MarkdownDocumentCore.applyCommand}.
 * Surfaces use it to project their state forward without having
 * to re-derive the change-map.
 */
export interface MarkdownCommitSummary {
  /** New source-map revision number. */
  readonly revision: number;
  /**
   * Map of new logical-source positions from old logical-source
   * positions. Used by every surface to translate its own offsets.
   */
  readonly oldToNew: readonly {
    readonly fromA: number;
    readonly toA: number;
    readonly fromB: number;
    readonly toB: number;
  }[];
  /** Old logical-source length. */
  readonly oldLength: number;
  /** New logical-source length. */
  readonly newLength: number;
  /**
   * Identifiers of source-map nodes that were matched to an
   * existing ID (preserved). Surfaces use this to keep their
   * internal node references alive.
   */
  readonly preservedNodeIds: readonly MarkdownNodeId[];
  /** Group containing this command after coalescing. */
  readonly historyGroup: MarkdownHistoryGroup;
  /** True if the command coalesced into the prior undo group. */
  readonly coalesced: boolean;
  /** Inverse edits in *post-command* coordinates. */
  readonly inverseEdits: readonly {
    readonly from: number;
    readonly to: number;
    readonly insert: string;
  }[];
  /** Forward edits as resolved by the buffer (for diagnostics). */
  readonly forwardEdits: readonly MarkdownTextEdit[];
  /** Selection captured before this commit, when a surface posted one. */
  readonly selectionHint?: MarkdownSelectionHint | null;
}

/**
 * Subscriber callback. Called once per committed command with the
 * commit summary plus the new source-map. The callback must be a
 * pure function — no side effects outside React — because it may
 * be invoked inside React's rendering phase.
 */
export type MarkdownCoreSubscriber = (
  summary: MarkdownCommitSummary,
  nextMap: MarkdownSourceMap,
) => void;

/**
 * Snapshot returned by {@link MarkdownDocumentCore.snapshot}. Surfaces
 * read from the snapshot to seed themselves; they never read from
 * the live core directly.
 */
export interface MarkdownCoreSnapshot {
  /** Current source-map revision. */
  readonly revision: number;
  /** Logical source. */
  readonly logicalSource: string;
  /** Line-ending vector. */
  readonly lineEndings: readonly ("\n" | "\r\n" | "\r")[];
  /** Preferred line-ending for new insertions. */
  readonly preferredLineEnding: "\n" | "\r\n" | "\r";
  /** Current source-map. */
  readonly sourceMap: MarkdownSourceMap;
}

/**
 * Diagnostic level. `"warning"` and `"error"` are user-visible;
 * `"info"` is hidden by default. `"ignore"` is used internally for
 * diagnostics that have been suppressed by an override.
 */
export type MarkdownDiagnosticLevel = "info" | "warning" | "error" | "ignore";

/**
 * One diagnostic emitted by the core. Diagnostics are *passive* —
 * the core never blocks an apply on them. Surfaces render them.
 */
export interface MarkdownDiagnostic {
  readonly id: string;
  readonly level: MarkdownDiagnosticLevel;
  readonly message: string;
  readonly range: { readonly from: number; readonly to: number } | null;
  readonly nodeId: MarkdownNodeId | null;
}

/**
 * Selection hint shared between the source editor and the PM editor
 * to keep cursor positions synchronized across switches.
 */
export interface MarkdownSelectionHint {
  /**
   * Logical-source offset for the anchor. `assoc` describes the
   * boundary association when the anchor is on a boundary.
   */
  readonly anchor: number;
  readonly head: number;
  readonly assoc: -1 | 1;
  readonly surface: "source" | "prosemirror";
  readonly nodeId: MarkdownNodeId | null;
}

/**
 * Optional configuration passed to {@link MarkdownDocumentCore.create}.
 * All fields have sensible defaults; tests inject explicit values.
 */
export interface MarkdownDocumentCoreOptions {
  readonly documentId: string;
  readonly initialLogicalSource: string;
  /** Persistence mirror invoked once after each local commit, undo, or redo. */
  readonly publishRawSource?: (rawSource: string) => void;
  /**
   * If true, the core treats the source as authoritative — every
   * apply records a command. If false, applies marked as
   * `external` are not recorded.
   */
  readonly recordAllCommands?: boolean;
  /**
   * Initial revision number. Defaults to 0.
   */
  readonly initialRevision?: number;
}

/**
 * Lifecycle state of a {@link MarkdownDocumentCore}. Used to gate
 * "document is ready" notifications in the React layer.
 */
export type MarkdownCoreLifecycle = "constructing" | "ready" | "disposing" | "disposed";
