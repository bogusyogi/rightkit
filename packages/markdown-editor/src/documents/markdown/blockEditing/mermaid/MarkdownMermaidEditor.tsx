/**
 * Mermaid editor shell.
 *
 * Composes {@link parseMarkdownMermaid} and
 * {@link stringifyMarkdownMermaid}. Renders a `<textarea>` for the
 * diagram source. No HTML security lowering (Packet 10 §20.5).
 */

import { useMemo, useState, type ReactNode } from "react";
import {
  parseMarkdownMermaid,
  stringifyMarkdownMermaid,
  setMermaidContent,
  type MarkdownMermaidModel,
} from "../../editors/mermaidModel.js";
import type { MarkdownDocumentCore } from "../../core/MarkdownDocumentCore.js";
import { useMarkdownBlockDraft } from "../useMarkdownBlockDraft.js";
import { MarkdownBlockDraftStatus } from "../MarkdownBlockDraftStatus.js";

export interface MarkdownMermaidEditorProps {
  readonly core: MarkdownDocumentCore;
  readonly range: { from: number; to: number };
  readonly onClose?: () => void;
}

/**
 * Render the Mermaid editor shell. Validation runs through the
 * `validateMermaid` validator in `validateMarkdown.ts` (Packet 05)
 * but the editor commits optimistically and re-renders on failure.
 */
export function MarkdownMermaidEditor({
  core,
  range,
  onClose,
}: MarkdownMermaidEditorProps): ReactNode {
  const initial = useMemo(() => {
    const snap = core.snapshot();
    return parseMarkdownMermaid(snap.logicalSource, range.from, range.to);
  }, [core, range.from, range.to]);

  const [model, setModel] = useState<MarkdownMermaidModel | null>(initial);
  const draft = useMarkdownBlockDraft(core, "mermaid", range);
  const discard = async () => {
    const result = await draft.discard();
    if (result.outcome === "discarded" || result.outcome === "missing") onClose?.();
  };
  const done = async () => {
    const result = await draft.done();
    if (result.outcome === "done") onClose?.();
  };

  if (!model) {
    return <div data-testid="markdown-mermaid-editor-invalid">Not a valid mermaid block.</div>;
  }

  const commit = (next: MarkdownMermaidModel): void => {
    const serialized = stringifyMarkdownMermaid(next);
    draft.update(serialized);
    setModel(next);
  };

  const onContentChange = (content: string): void => {
    commit(setMermaidContent(model, content));
  };

  return (
    <div data-testid="markdown-mermaid-editor">
      <h3 className="vr-protected-editor-title">Edit Mermaid diagram</h3>
      <MarkdownBlockDraftStatus snapshot={draft.snapshot} />
      <textarea
        value={model.content}
        onChange={(e) => onContentChange(e.target.value)}
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
        data-testid="md-mermaid-content"
        aria-label="Mermaid diagram source"
        className="vr-protected-editor-textarea"
      />
      <div className="vr-protected-editor-actions">
        <button
          type="button"
          onClick={() => void discard()}
          data-testid="md-mermaid-cancel"
          className="vr-protected-editor-button"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={() => void done()}
          data-testid="md-mermaid-done"
          className="vr-protected-editor-button vr-protected-editor-button--primary"
        >
          Done
        </button>
      </div>
    </div>
  );
}
