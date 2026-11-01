import { memo, useMemo, useState } from "react";

import Icon from "@rightkit/agent-ui-core/components/Icon";
import MarkdownText from "../MarkdownText.js";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

// Document preview: rendered (Markdown) / source (raw) toggle for a
// `generated_document` or `workspace_file` reference. Reads text from a
// registered resolver; falls back to the raw `destination` string when no
// resolver is wired (Agent 4 will plug the workspace resolver in).
//
// Accessibility: a single segmented control switches modes. The toggle is
// keyboard-friendly (Tab + Enter/Space). The body region uses `role="region"`
// so screen readers can navigate to it.

export interface DocumentPreviewProps {
  reference: ReferenceObject;
  /** Optional async resolver — returns the document body text. When absent
   * (e.g. legacy / no resolver registered), the component renders an inert
   * placeholder with the destination path. */
  resolve?: (ref: ReferenceObject) => Promise<string> | string | null;
  /** Optional title override; defaults to `filename ?? destination`. */
  title?: string;
}

function DocumentPreviewImpl({ reference, resolve, title }: DocumentPreviewProps) {
  const [mode, setMode] = useState<"rendered" | "source">("rendered");
  const [resolved, setResolved] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const displayTitle = title ?? reference.filename ?? reference.destination;

  const safeMarkdown = useMemo(() => {
    if (resolved === null) return null;
    // Naive sanitization — strip HTML-ish patterns before rendering as MD.
    return resolved.replace(/<\/?[a-zA-Z][^>]*>/g, "");
  }, [resolved]);

  function ensureResolved() {
    if (resolved !== null || !resolve) return;
    setLoading(true);
    setError(null);
    try {
      const result = resolve(reference);
      if (result instanceof Promise) {
        result
          .then((value) => {
            setResolved(value ?? "");
            setLoading(false);
          })
          .catch((err: unknown) => {
            setError(err instanceof Error ? err.message : "resolve failed");
            setLoading(false);
          });
        return;
      }
      setResolved(result ?? "");
      setLoading(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : "resolve failed");
      setLoading(false);
    }
  }

  function selectMode(next: "rendered" | "source") {
    setMode(next);
    if (next === "rendered" || next === "source") {
      ensureResolved();
    }
  }

  return (
    <section
      aria-label={`Document preview: ${displayTitle}`}
      data-document-preview="true"
      data-document-reference-id={reference.id}
      className="mt-2 flex flex-col gap-2 rounded-lg border border-line bg-bg p-2"
    >
      <header className="flex items-center gap-2">
        <Icon name="document" size={13} />
        <span className="truncate font-mono text-[12px] text-text" title={displayTitle}>
          {displayTitle}
        </span>
        <span className="flex-1" />
        <fieldset
          aria-label="Document preview mode"
          className="inline-flex overflow-hidden rounded-md border border-line"
        >
          <button
            type="button"
            aria-pressed={mode === "rendered"}
            onClick={() => selectMode("rendered")}
            className={`h-6 px-2 text-[11px] ${
              mode === "rendered"
                ? "bg-accent text-on-accent"
                : "bg-panel text-text-2 hover:text-text"
            }`}
          >
            Rendered
          </button>
          <button
            type="button"
            aria-pressed={mode === "source"}
            onClick={() => selectMode("source")}
            className={`h-6 border-l border-line px-2 text-[11px] ${
              mode === "source"
                ? "bg-accent text-on-accent"
                : "bg-panel text-text-2 hover:text-text"
            }`}
          >
            Source
          </button>
        </fieldset>
      </header>
      <div
        className="min-h-[4rem] max-h-[18rem] overflow-y-auto rounded-md border border-line bg-panel p-2"
        aria-live="polite"
      >
        {loading ? (
          <p className="text-[12px] text-text-3">Loading…</p>
        ) : error ? (
          <p className="text-[12px] text-danger">{error}</p>
        ) : !resolve ? (
          <p className="text-[12px] text-text-3">
            Document resolver not wired — destination: <code>{reference.destination}</code>
          </p>
        ) : resolved === null ? (
          <p className="text-[12px] text-text-3">Choose Rendered or Source to load the document.</p>
        ) : mode === "rendered" ? (
          safeMarkdown ? (
            <MarkdownText text={safeMarkdown} />
          ) : (
            <p className="text-[12px] text-text-3">(empty document)</p>
          )
        ) : (
          <pre className="m-0 whitespace-pre-wrap break-words font-mono text-[12px] text-text-2">
            {resolved}
          </pre>
        )}
      </div>
    </section>
  );
}

export const DocumentPreview = memo(DocumentPreviewImpl);
export default DocumentPreview;
