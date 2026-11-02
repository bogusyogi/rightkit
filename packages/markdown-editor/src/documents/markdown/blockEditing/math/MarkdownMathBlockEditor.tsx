/**
 * Math-block editor shell.
 *
 * Composes {@link parseMarkdownMath} and {@link stringifyMarkdownMath}.
 * Renders a `<textarea>` for the math value. Apply is blocked when
 * validation fails (the validator is async; for the editor shell
 * we surface a "validation pending" message instead).
 */

import { useMemo, useState, type ReactNode } from "react";
import {
  parseMarkdownMath,
  stringifyMarkdownMath,
  setMathValue,
  type MarkdownMathModel,
} from "../../editors/mathModel.js";
import type { MarkdownDocumentCore } from "../../core/MarkdownDocumentCore.js";
import { useMarkdownBlockDraft } from "../useMarkdownBlockDraft.js";
import { MarkdownBlockDraftStatus } from "../MarkdownBlockDraftStatus.js";

export interface MarkdownMathBlockEditorProps {
  readonly core: MarkdownDocumentCore;
  readonly range: { from: number; to: number };
  readonly onClose?: () => void;
}

/**
 * Render the math-block editor. Validation runs synchronously here
 * for the editor shell; the production validator lives in
 * `validateMarkdown.ts` (Packet 05) and can be plugged in via the
 * `onValidate` callback.
 */
export function MarkdownMathBlockEditor({
  core,
  range,
  onClose,
}: MarkdownMathBlockEditorProps): ReactNode {
  const initial = useMemo(() => {
    const snap = core.snapshot();
    return parseMarkdownMath(snap.logicalSource, range.from, range.to);
  }, [core, range.from, range.to]);

  const [model, setModel] = useState<MarkdownMathModel | null>(initial);
  const draft = useMarkdownBlockDraft(core, "math", range);
  const discard = async () => {
    const result = await draft.discard();
    if (result.outcome === "discarded" || result.outcome === "missing") onClose?.();
  };
  const done = async () => {
    const result = await draft.done();
    if (result.outcome === "done") onClose?.();
  };

  if (!model) {
    return <div data-testid="markdown-math-editor-invalid">Not a valid math block.</div>;
  }

  const commit = (next: MarkdownMathModel): void => {
    let serialized = stringifyMarkdownMath(next);
    // Mdast block ranges end before surrounding line endings. The block serializer keeps one
    // canonical EOF LF, but inserting that LF before an existing separator creates an extra blank
    // line. Preserve suffix bytes exactly while retaining canonical EOF termination.
    if (
      next.mode === "block" &&
      serialized.endsWith("\n") &&
      /^[\r\n]/.test(core.snapshot().logicalSource.slice(range.to))
    ) {
      serialized = serialized.slice(0, -1);
    }
    draft.update(serialized);
    setModel(next);
  };

  const onValueChange = (value: string): void => {
    commit(setMathValue(model, value));
  };

  return (
    <div data-testid="markdown-math-editor">
      <h3 className="vr-protected-editor-title">Edit math block</h3>
      <MarkdownBlockDraftStatus snapshot={draft.snapshot} />
      <textarea
        value={model.value}
        onChange={(e) => onValueChange(e.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            void discard();
          }
          if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
            event.preventDefault();
            void done();
          }
        }}
        spellCheck={false}
        data-testid="md-math-value"
        aria-label="Math source"
        className="vr-protected-editor-textarea"
      />
      <p className="md-math-mode text-xs text-muted">{model.mode} math · Ctrl/⌘+Enter to apply</p>
      <div className="vr-protected-editor-actions">
        <button
          type="button"
          onClick={() => void discard()}
          data-testid="md-math-cancel"
          className="vr-protected-editor-button"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={() => void done()}
          data-testid="md-math-done"
          className="vr-protected-editor-button vr-protected-editor-button--primary"
        >
          Done
        </button>
      </div>
    </div>
  );
}
