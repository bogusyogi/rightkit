/**
 * Public types for the Markdown block-editing framework.
 *
 * Block editors are PM NodeViews that:
 *
 *  - own a slice of the logical source,
 *  - commit edits through the {@link BlockDraft} protocol,
 *  - read selection hints from the document core,
 *  - may be the *direct* editor for inline atoms (math, inline-
 *    math, footnote-reference, footnote-definition, code).
 */

import type { MarkdownNodeId } from "../core/MarkdownNodeIdFactory";

/**
 * Block-editor lifecycle states.
 */
export type BlockDraftState =
  | "inert"
  | "active"
  | "editing"
  | "committing"
  | "committed"
  | "rolled-back";

/**
 * One draft. Drafts are mutable; commit rolls them into the
 * document core as a single edit.
 */
export interface BlockDraft {
  readonly id: string;
  readonly nodeId: MarkdownNodeId;
  readonly kind: BlockKind;
  readonly range: { readonly from: number; readonly to: number };
  state: BlockDraftState;
  /**
   * In-progress text. The block editor mutates this field via
   * {@link updateDraftText}; the draft protocol serializes it
   * back to Markdown on commit.
   */
  text: string;
  /**
   * Cursor position within the draft text. Editors update this
   * on every selection change so the host can re-anchor the
   * caret when the draft finishes.
   */
  cursor: number;
  /** Last time the draft was updated. Used for staleness checks. */
  updatedAtMs: number;
}

/**
 * Kind of protected block. Direct inline editors only apply to
 * `inline-math`, `inline-code`, `math`, `code-raw-fallback`, `footnote-ref`,
 * `footnote-def`.
 *
 * Ordinary fenced code projects to the native `code_fence` PM node
 * (contract 14 CE-3). Only unsupported/malformed fences use the
 * protected `code-raw-fallback` kind — the generic `"code"` route
 * has been removed so ordinary fences cannot be recaptured.
 */
export type BlockKind =
  | "code-raw-fallback"
  | "mermaid"
  | "math"
  | "inline-math"
  | "inline-code"
  | "footnote-ref"
  | "footnote-def"
  | "table"
  | "image"
  | "link"
  | "heading"
  | "list"
  | "faq"
  | "stepper"
  | "comparison"
  | "richTable"
  | "raw";

/**
 * Direct vs. indirect editor. Direct editors swap the inline
 * element for a real textarea; indirect editors show a popup.
 */
export type BlockEditorKind = "direct" | "indirect";

/**
 * Block-editor props contract. PM passes these to every NodeView
 * that opts into the framework.
 */
export interface BlockEditorProps {
  readonly nodeId: MarkdownNodeId;
  readonly kind: BlockKind;
  readonly range: { readonly from: number; readonly to: number };
  readonly initialText: string;
  readonly isDirect: boolean;
}

/**
 * Lifecycle commands emitted by the host.
 */
export type BlockEditorCommand =
  | { readonly kind: "enter" }
  | { readonly kind: "commit" }
  | { readonly kind: "rollback" }
  | { readonly kind: "blur" };

/**
 * Outcome of a block-editor commit. The host reads the outcome to
 * decide whether to keep, advance, or rollback.
 */
export interface BlockEditorOutcome {
  readonly committed: boolean;
  readonly text: string;
  readonly cursor: number;
}
