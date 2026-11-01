import { useMemo, useState } from "react";
import type { MarkdownDocumentCore } from "../../core/MarkdownDocumentCore.js";
import { useMarkdownBlockDraft } from "../useMarkdownBlockDraft.js";
import { MarkdownBlockDraftStatus } from "../MarkdownBlockDraftStatus.js";

export function MarkdownRawBlockEditor({
  core,
  range,
  onClose,
}: {
  core: MarkdownDocumentCore;
  range: { from: number; to: number };
  onClose?: () => void;
}) {
  const initial = useMemo(
    () => core.snapshot().logicalSource.slice(range.from, range.to),
    [core, range.from, range.to],
  );
  const [text, setText] = useState(initial);
  const draft = useMarkdownBlockDraft(core, "raw", range);

  const discard = async () => {
    const result = await draft.discard();
    if (result.outcome === "discarded" || result.outcome === "missing") onClose?.();
  };
  const done = async () => {
    const result = await draft.done();
    if (result.outcome === "done") onClose?.();
  };

  return (
    <div data-testid="markdown-raw-block-editor">
      <h3 className="vr-protected-editor-title">Edit protected source</h3>
      <MarkdownBlockDraftStatus snapshot={draft.snapshot} />
      <textarea
        aria-label="Protected Markdown source"
        value={text}
        onChange={(event) => {
          setText(event.target.value);
          draft.update(event.target.value);
        }}
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
        className="vr-protected-editor-textarea"
      />
      <div className="vr-protected-editor-actions">
        <button type="button" onClick={() => void discard()} className="vr-protected-editor-button">
          Cancel
        </button>
        <button
          type="button"
          onClick={() => void done()}
          className="vr-protected-editor-button vr-protected-editor-button--primary"
        >
          Done
        </button>
      </div>
    </div>
  );
}
