/**
 * Raw-fence editor shell — protected fallback for unrepresentable fenced blocks.
 *
 * Composes {@link parseMarkdownCodeBlock} and
 * {@link stringifyMarkdownCodeBlock} to render an editable fenced
 * block with language + metadata + content fields. Commits through
 * `core.applyCommand`.
 *
 * Ordinary code → native `code_fence` PM node (contract 14 CE-3).
 * Only unsupported/malformed fences (indented code, unterminated, etc.)
 * use this protected `code-raw-fallback` route.
 */

import { useMemo, useState, type ReactNode } from "react";
import {
  parseMarkdownCodeBlock,
  stringifyMarkdownCodeBlock,
  setCodeBlockLanguage,
  setCodeBlockContent,
  type MarkdownCodeBlockModel,
} from "../../editors/codeBlockModel";
import type { MarkdownDocumentCore } from "../../core/MarkdownDocumentCore";
import { useMarkdownBlockDraft } from "../useMarkdownBlockDraft";
import { MarkdownBlockDraftStatus } from "../MarkdownBlockDraftStatus";

export interface MarkdownRawFenceEditorProps {
  readonly core: MarkdownDocumentCore;
  readonly range: { from: number; to: number };
  readonly onClose?: () => void;
}

const SUGGESTED_LANGUAGES: readonly string[] = [
  "ts",
  "tsx",
  "js",
  "jsx",
  "json",
  "yaml",
  "rust",
  "python",
  "go",
  "sh",
  "bash",
  "css",
  "html",
  "sql",
];

/**
 * Render the raw-fence editor. Re-parses on every commit so the
 * editor always reflects the document's current state.
 */
export function MarkdownRawFenceEditor({
  core,
  range,
  onClose,
}: MarkdownRawFenceEditorProps): ReactNode {
  const initial = useMemo(() => {
    const snap = core.snapshot();
    return parseMarkdownCodeBlock(snap.logicalSource, range.from, range.to);
  }, [core, range.from, range.to]);

  const draft = useMarkdownBlockDraft(core, "code-raw-fallback", range);
  const [model, setModel] = useState<MarkdownCodeBlockModel | null>(() => {
    const current = draft.snapshot?.currentText;
    return current ? (parseMarkdownCodeBlock(current, 0, current.length) ?? initial) : initial;
  });
  const discard = async () => {
    const result = await draft.discard();
    if (result.outcome === "discarded" || result.outcome === "missing") onClose?.();
  };
  const done = async () => {
    const result = await draft.done();
    if (result.outcome === "done") onClose?.();
  };

  if (!model) {
    return <div data-testid="markdown-code-editor-invalid">Not a valid code block.</div>;
  }

  const commit = (next: MarkdownCodeBlockModel): void => {
    const serialized = stringifyMarkdownCodeBlock(next);
    draft.update(serialized);
    setModel(next);
  };

  const onLanguageChange = (lang: string): void => {
    commit(setCodeBlockLanguage(model, lang));
  };

  const onContentChange = (content: string): void => {
    commit(setCodeBlockContent(model, content));
  };

  return (
    <div data-testid="markdown-code-editor">
      <h3 className="vr-protected-editor-title">Edit code block</h3>
      <MarkdownBlockDraftStatus snapshot={draft.snapshot} />
      <label className="vr-protected-editor-field">
        Language
        <input
          type="text"
          value={model.language}
          list="md-code-languages"
          onChange={(e) => onLanguageChange(e.target.value)}
          data-testid="md-code-language"
          className="vr-protected-editor-input"
        />
        <datalist id="md-code-languages">
          {SUGGESTED_LANGUAGES.map((lang) => (
            <option key={lang} value={lang} />
          ))}
        </datalist>
      </label>
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
        data-testid="md-code-content"
        aria-label="Code block source"
        className="vr-protected-editor-textarea"
      />
      <div className="vr-protected-editor-actions">
        <button
          type="button"
          onClick={() => void discard()}
          data-testid="md-code-cancel"
          className="vr-protected-editor-button"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={() => void done()}
          data-testid="md-code-done"
          className="vr-protected-editor-button vr-protected-editor-button--primary"
        >
          Done
        </button>
      </div>
    </div>
  );
}
