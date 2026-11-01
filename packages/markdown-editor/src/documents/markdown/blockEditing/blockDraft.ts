/**
 * Block draft protocol.
 *
 * The protocol is the single point of contact between a
 * {@link BlockEditor} and the host. Editors mutate a {@link BlockDraft}
 * via the helpers in this module; the host reads the draft on
 * commit and serializes it back to Markdown through the
 * {@link MarkdownDocumentCore}.
 *
 * The protocol is **pure**: it does not import React, ProseMirror,
 * or Tauri. It can be unit-tested in isolation.
 */

import type { BlockDraft, BlockEditorKind, BlockEditorOutcome, BlockKind } from "./types";
import type { MarkdownNodeId } from "../core/MarkdownNodeIdFactory";
import type { MarkdownTextEdit } from "../core/types";

/**
 * Maximum time a draft may stay in the `active` state without an
 * update before the host discards it. Defends against orphaned
 * drafts when the user closes the document unexpectedly.
 */
export const DRAFT_INACTIVITY_TIMEOUT_MS = 30 * 1000;

/**
 * Create a fresh draft for the given node range.
 */
export function createBlockDraft(
  nodeId: MarkdownNodeId,
  kind: BlockKind,
  range: { from: number; to: number },
  initialText: string,
): BlockDraft {
  return {
    id: `draft-${nodeId}-${Date.now()}`,
    nodeId,
    kind,
    range,
    state: "inert",
    text: initialText,
    cursor: initialText.length,
    updatedAtMs: Date.now(),
  };
}

/**
 * Mark the draft as actively edited and refresh its timestamp.
 */
export function activateDraft(draft: BlockDraft): void {
  draft.state = "active";
  draft.updatedAtMs = Date.now();
}

/**
 * Mark the draft as committing. The host is about to serialize
 * the draft back to the document.
 */
export function startCommittingDraft(draft: BlockDraft): void {
  draft.state = "committing";
  draft.updatedAtMs = Date.now();
}

/**
 * Mark the draft as committed. The host has applied the edit and
 * the editor should leave the editing state.
 */
export function markDraftCommitted(draft: BlockDraft, outcome: BlockEditorOutcome): void {
  draft.state = "committed";
  draft.text = outcome.text;
  draft.cursor = outcome.cursor;
  draft.updatedAtMs = Date.now();
}

/**
 * Mark the draft as rolled back. The host has discarded the
 * draft and the editor must restore the original text.
 */
export function markDraftRolledBack(draft: BlockDraft): void {
  draft.state = "rolled-back";
  draft.updatedAtMs = Date.now();
}

/**
 * Edit the draft text. Returns `true` when the change is
 * accepted, `false` when the draft is in a terminal state and
 * cannot be edited.
 */
export function updateDraftText(draft: BlockDraft, text: string, cursor: number): boolean {
  if (draft.state === "committed" || draft.state === "rolled-back") return false;
  draft.text = text;
  draft.cursor = cursor;
  draft.updatedAtMs = Date.now();
  if (draft.state === "inert") draft.state = "active";
  return true;
}

/**
 * Returns `true` when the draft has been inactive longer than
 * {@link DRAFT_INACTIVITY_TIMEOUT_MS}.
 */
export function isDraftStale(draft: BlockDraft, nowMs: number = Date.now()): boolean {
  return nowMs - draft.updatedAtMs > DRAFT_INACTIVITY_TIMEOUT_MS;
}

/**
 * Serialize a draft back to the {@link MarkdownTextEdit} that
 * replaces the original range with the draft text. The
 * `inheritMode` is `preserve` for all block drafts because block
 * editors own their content entirely.
 */
export function draftToEdit(draft: BlockDraft): MarkdownTextEdit {
  return {
    from: draft.range.from,
    to: draft.range.to,
    insert: draft.text,
    inheritMode: "preserve",
  };
}

/**
 * Decide whether the host should commit a draft. The rule:
 *  - drafts in `committed` or `rolled-back` never re-commit;
 *  - drafts in `inert` are not edited and may be discarded;
 *  - drafts in `active`/`editing`/`committing` are committed on
 *    blur or escape.
 */
export function shouldCommitDraft(draft: BlockDraft): boolean {
  return draft.state === "active" || draft.state === "editing" || draft.state === "committing";
}

/**
 * Resolve whether the editor should be *direct* (in-place
 * textarea) or *indirect* (popup) based on kind.
 */
export function blockEditorKind(kind: BlockKind): BlockEditorKind {
  switch (kind) {
    case "code-raw-fallback":
    case "mermaid":
    case "math":
    case "inline-math":
    case "inline-code":
      return "direct";
    case "footnote-ref":
    case "footnote-def":
    case "table":
    case "image":
    case "link":
    case "heading":
    case "list":
    case "faq":
    case "stepper":
    case "comparison":
    case "richTable":
    case "raw":
      return "indirect";
  }
}
