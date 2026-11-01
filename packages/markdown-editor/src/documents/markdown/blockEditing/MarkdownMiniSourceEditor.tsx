/**
 * Generic markdown mini source editor.
 *
 * Renders a `<textarea>` bound to a {@link BlockDraft}. On commit, the
 * host reads `draft.text` and applies it through {@link draftToEdit}.
 * Typing stays local draft only — no core writes until Done.
 * Also supports registry-backed mode via `core` + `range` + `kind` for
 * protected blocks routed through {@link ProtectedBlockHost} + blockDrafts.
 */

import { useEffect, useRef, type ReactNode } from "react";
import type { BlockDraft } from "./types";
import { updateDraftText, shouldCommitDraft } from "./blockDraft";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import type { BlockKind } from "./types";
import { useMarkdownBlockDraft } from "./useMarkdownBlockDraft";

export interface MarkdownMiniSourceEditorProps {
  readonly draft?: BlockDraft;
  readonly core?: MarkdownDocumentCore;
  readonly range?: { from: number; to: number };
  readonly kind?: BlockKind;
  readonly placeholder?: string;
  readonly autoFocus?: boolean;
  readonly onDraftChange?: (text: string, cursor: number) => void;
  readonly onCommitRequest?: () => void;
  readonly onRollbackRequest?: () => void;
  readonly commitKey?: "Escape" | "Enter+Meta" | "Enter+Control";
  readonly onClose?: () => void;
}

/**
 * Render the mini source editor. Returns a focused `<textarea>`
 * by default. The textarea is uncontrolled: the draft text is the
 * source of truth; React renders the field via `key` to reset on
 * external changes.
 * Registry mode: when `core` + `range` + `kind` are supplied, typing
 * stays local via `useMarkdownBlockDraft` and Done/Cancel go through
 * the blockDrafts registry — one history entry on Done, zero writes on
 * Cancel, byte-identical restore.
 */
export function MarkdownMiniSourceEditor(props: MarkdownMiniSourceEditorProps): ReactNode {
  if (props.core && props.range) {
    return (
      <RegistryMiniSourceEditor
        core={props.core}
        range={props.range}
        kind={props.kind ?? "raw"}
        placeholder={props.placeholder}
        autoFocus={props.autoFocus}
        onDraftChange={props.onDraftChange}
        onCommitRequest={props.onCommitRequest}
        onRollbackRequest={props.onRollbackRequest}
        commitKey={props.commitKey}
        onClose={props.onClose}
      />
    );
  }
  if (!props.draft) return null;
  return (
    <LegacyMiniSourceEditor
      draft={props.draft}
      placeholder={props.placeholder}
      autoFocus={props.autoFocus}
      onDraftChange={props.onDraftChange}
      onCommitRequest={props.onCommitRequest}
      onRollbackRequest={props.onRollbackRequest}
      commitKey={props.commitKey}
      onClose={props.onClose}
    />
  );
}

function LegacyMiniSourceEditor({
  draft,
  placeholder,
  autoFocus = true,
  onDraftChange,
  onCommitRequest,
  onRollbackRequest,
  commitKey = "Enter+Meta",
  onClose,
}: {
  draft: BlockDraft;
  placeholder?: string;
  autoFocus?: boolean;
  onDraftChange?: (text: string, cursor: number) => void;
  onCommitRequest?: () => void;
  onRollbackRequest?: () => void;
  commitKey?: MarkdownMiniSourceEditorProps["commitKey"];
  onClose?: () => void;
}): ReactNode {
  const ref = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    if (!autoFocus) return;
    const el = ref.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(draft.cursor, draft.cursor);
  }, [autoFocus, draft.cursor]);
  void draft.id;

  const onInput = (e: React.ChangeEvent<HTMLTextAreaElement>): void => {
    const text = e.currentTarget.value;
    const cursor = e.currentTarget.selectionStart ?? text.length;
    const accepted = updateDraftText(draft, text, cursor);
    if (accepted) onDraftChange?.(text, cursor);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    if (matchesCommit(e, commitKey)) {
      if (shouldCommitDraft(draft)) {
        e.preventDefault();
        onCommitRequest?.();
        onClose?.();
      }
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      onRollbackRequest?.();
      onClose?.();
    }
  };

  return (
    <textarea
      ref={ref}
      data-testid="markdown-mini-source-editor"
      value={draft.text}
      placeholder={placeholder}
      onChange={onInput}
      onKeyDown={onKeyDown}
      spellCheck={false}
    />
  );
}

function RegistryMiniSourceEditor({
  core,
  range,
  kind,
  placeholder,
  autoFocus = true,
  onDraftChange,
  onCommitRequest,
  onRollbackRequest,
  commitKey = "Enter+Meta",
  onClose,
}: {
  core: MarkdownDocumentCore;
  range: { from: number; to: number };
  kind: BlockKind;
  placeholder?: string;
  autoFocus?: boolean;
  onDraftChange?: (text: string, cursor: number) => void;
  onCommitRequest?: () => void;
  onRollbackRequest?: () => void;
  commitKey?: MarkdownMiniSourceEditorProps["commitKey"];
  onClose?: () => void;
}): ReactNode {
  const ref = useRef<HTMLTextAreaElement | null>(null);
  const draft = useMarkdownBlockDraft(core, kind, range);

  useEffect(() => {
    if (!autoFocus) return;
    const el = ref.current;
    if (!el) return;
    el.focus();
    const len = draft.snapshot?.currentText.length ?? 0;
    el.setSelectionRange(len, len);
  }, [autoFocus, draft.snapshot]);

  const handleDone = async () => {
    const result = await draft.done();
    if (result.outcome === "done") {
      onCommitRequest?.();
      onClose?.();
    }
  };

  const handleCancel = async () => {
    const result = await draft.discard();
    if (result.outcome === "discarded" || result.outcome === "missing") {
      onRollbackRequest?.();
      onClose?.();
    }
  };

  const onInput = (e: React.ChangeEvent<HTMLTextAreaElement>): void => {
    const text = e.currentTarget.value;
    const cursor = e.currentTarget.selectionStart ?? text.length;
    draft.update(text);
    onDraftChange?.(text, cursor);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    if (matchesCommit(e, commitKey)) {
      e.preventDefault();
      void handleDone();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      void handleCancel();
    }
  };

  const value =
    draft.snapshot?.currentText ?? core.snapshot().logicalSource.slice(range.from, range.to);

  return (
    <div>
      <textarea
        ref={ref}
        data-testid="markdown-mini-source-editor"
        value={value}
        placeholder={placeholder}
        onChange={onInput}
        onKeyDown={onKeyDown}
        spellCheck={false}
      />
      <div className="mt-2 flex justify-end gap-2">
        <button
          type="button"
          onMouseDown={(ev) => ev.preventDefault()}
          onClick={() => void handleCancel()}
        >
          Cancel
        </button>
        <button
          type="button"
          onMouseDown={(ev) => ev.preventDefault()}
          onClick={() => void handleDone()}
        >
          Done
        </button>
      </div>
    </div>
  );
}

function matchesCommit(
  e: React.KeyboardEvent<HTMLTextAreaElement>,
  commitKey: MarkdownMiniSourceEditorProps["commitKey"],
): boolean {
  switch (commitKey) {
    case "Escape":
      return e.key === "Escape";
    case "Enter+Meta":
      return e.key === "Enter" && e.metaKey;
    case "Enter+Control":
      return e.key === "Enter" && e.ctrlKey;
    default:
      return false;
  }
}
