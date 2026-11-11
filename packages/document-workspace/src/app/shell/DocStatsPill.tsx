// Document status row (App's bottom status bar): words/chars/reading time (click cycles),
// save state and autosave, docked to the bottom edge of the page outside the scrolling content.
// Subscribes to the doc's live edit buffer so keystrokes update the count without re-rendering
// the App shell. Only text formats (md, mdx, txt, html, unknown) show counts.
import { memo, useState } from "react";
import { Check } from "@phosphor-icons/react/dist/icons/Check";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { shortcutLabel } from "@rightkit/document-core/platform";
import { useTabDirty } from "../tabs/useTabDirty.js";
import { useEditText } from "../useEditText.js";

const STATS_FORMATS = new Set<LoadedDocument["format"]>(["md", "mdx", "txt", "html", "unknown"]);

type Mode = "words" | "chars" | "read";
const NEXT_MODE: Record<Mode, Mode> = { words: "chars", chars: "read", read: "words" };

export const DocStatsPill = memo(DocStatsPillImpl);

/** Save state + autosave, shown beside the stats (what 0.1.111's status bar used to say). */
export interface DocSaveStatus {
  dirtyHint: boolean;
  isDirty: (doc: LoadedDocument) => boolean;
  subscribeDirty: (path: string, cb: () => void) => () => void;
  onSave: () => void;
  autoSave: boolean;
  onToggleAutoSave: () => void;
}

function DocStatsPillImpl({
  doc,
  subscribeEdits,
  getEditSnapshot,
  status,
}: {
  doc: LoadedDocument;
  subscribeEdits: (path: string, cb: () => void) => () => void;
  getEditSnapshot: (path: string) => string | undefined;
  status?: DocSaveStatus;
}) {
  const liveText = useEditText(subscribeEdits, getEditSnapshot, doc.path) ?? doc.content;
  const [mode, setMode] = useState<Mode>("words");
  const dirty = useTabDirty(
    doc,
    status?.dirtyHint ?? false,
    status?.isDirty ?? NEVER_DIRTY,
    status?.subscribeDirty ?? NO_SUBSCRIBE,
  );
  const showStats = STATS_FORMATS.has(doc.format);
  if (!showStats && !status) return null;

  const trimmed = liveText.trim();
  const words = trimmed ? trimmed.split(/\s+/).length : 0;
  const mins = words ? Math.max(1, Math.round(words / 230)) : 0;
  const label =
    mode === "words"
      ? `${words.toLocaleString()} words`
      : mode === "chars"
        ? `${liveText.length.toLocaleString()} chars`
        : `~${mins} min read`;

  return (
    <div
      className="flex min-w-0 items-center gap-3 whitespace-nowrap font-sans"
      data-vr-doc-status
    >
      {showStats && (
        <button
          type="button"
          onClick={() => setMode((m) => NEXT_MODE[m])}
          title="Document statistics — click to cycle words, characters, reading time"
          aria-label={`${label}. Click to cycle document statistics.`}
          data-vr-stats
          className="vr-focus h-6 rounded px-1 tabular-nums hover:text-paper"
        >
          {label}
        </button>
      )}
      {status && (
        <>
          {dirty ? (
            <button
              type="button"
              onClick={status.onSave}
              title={`Save (${shortcutLabel("mod+s")})`}
              aria-label="Unsaved changes. Save"
              className="vr-focus flex h-6 items-center gap-1.5 rounded px-1.5 text-paper hover:bg-ink/30"
            >
              <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full bg-copper" />
              Save
            </button>
          ) : (
            <span className="flex items-center gap-1.5 px-1 text-paper/70">
              <Check size={11} weight="bold" aria-hidden className="text-accent2" />
              Saved
            </span>
          )}
          <button
            type="button"
            role="switch"
            aria-checked={status.autoSave}
            onClick={status.onToggleAutoSave}
            title="Save automatically after you pause"
            className="vr-focus flex h-6 items-center gap-1.5 rounded px-1 hover:text-paper"
          >
            Autosave
            <span
              aria-hidden="true"
              className={`relative h-3.5 w-6 rounded-full transition-colors ${status.autoSave ? "bg-copper" : "bg-line"}`}
            >
              <span
                className={`absolute left-0 top-0.5 h-2.5 w-2.5 rounded-full shadow-sm transition-transform ${status.autoSave ? "translate-x-3 bg-panel" : "translate-x-0.5 bg-muted"}`}
              />
            </span>
          </button>
        </>
      )}
    </div>
  );
}

const NEVER_DIRTY = () => false;
const NO_SUBSCRIBE = () => () => {};
