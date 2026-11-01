import { useState, type ReactNode } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";

export interface EditedFileEntry {
  filePath: string;
  additions?: number;
  deletions?: number;
  diff: string;
}

interface EditedFilesSummaryProps {
  files: EditedFileEntry[];
  onReview?: (files: EditedFileEntry[]) => void;
  onUndo?: (files: EditedFileEntry[]) => void;
  /** Per-file detail rendered when expanded — kept as children so this
   * component never has to import DiffCard from TranscriptStream.tsx
   * (avoids a circular module dependency between the two). */
  children?: ReactNode;
}

/** Single-row collapsed summary of every file an assistant turn touched.
 * Replaces one DiffCard-per-file with one compact row; expands to reveal
 * the existing DiffCard list unchanged. */
export default function EditedFilesSummary({
  files,
  onReview,
  onUndo,
  children,
}: EditedFilesSummaryProps) {
  const [expanded, setExpanded] = useState(false);

  if (files.length === 0) return null;

  const totals = files.reduce(
    (acc, file) => ({
      additions: acc.additions + (file.additions ?? 0),
      deletions: acc.deletions + (file.deletions ?? 0),
    }),
    { additions: 0, deletions: 0 },
  );
  const fileWord = files.length === 1 ? "file" : "files";
  const summaryLabel = `${files.length} ${fileWord} edited`;

  return (
    <article className="self-start flex flex-col max-w-[720px] rounded-xl border border-line bg-panel overflow-hidden">
      <div className="flex items-center gap-1.5 w-full px-3 py-1.5">
        <button
          type="button"
          aria-expanded={expanded}
          aria-label={expanded ? `Collapse ${summaryLabel}` : `Expand ${summaryLabel}`}
          onClick={() => setExpanded((v) => !v)}
          className="flex min-w-0 flex-1 items-center gap-2 text-left cursor-pointer border-0 bg-transparent p-0 text-text-2 hover:text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          <Icon name="diff" size={13} aria-hidden="true" />
          <span className="font-mono text-[12px] font-semibold text-text truncate">
            {summaryLabel}
          </span>
          <span className="flex items-center gap-1.5 font-mono text-[10px] font-semibold tabular-nums">
            {totals.additions > 0 ? (
              <span className="inline-flex h-5 items-center rounded-md border border-accent-2/40 bg-accent-2/10 px-1.5 text-accent-2">
                +{totals.additions}
              </span>
            ) : null}
            {totals.deletions > 0 ? (
              <span className="inline-flex h-5 items-center rounded-md border border-danger/40 bg-danger/10 px-1.5 text-danger">
                -{totals.deletions}
              </span>
            ) : null}
          </span>
          <span
            aria-hidden="true"
            className={`transition-transform duration-150 ${expanded ? "rotate-180" : ""}`}
          >
            <Icon name="chevron" size={12} />
          </span>
        </button>
        <button
          type="button"
          aria-label="Review edited files"
          onClick={() => onReview?.(files)}
          disabled={!onReview}
          className="inline-flex h-6 shrink-0 items-center gap-1 rounded-md border border-line bg-panel-2 px-2 text-[11px] font-semibold text-text-2 cursor-pointer hover:bg-panel hover:text-text disabled:cursor-not-allowed disabled:opacity-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          Review
        </button>
        <button
          type="button"
          aria-label="Undo edited files"
          onClick={() => onUndo?.(files)}
          disabled={!onUndo}
          className="inline-flex h-6 shrink-0 items-center gap-1 rounded-md border border-line bg-panel-2 px-2 text-[11px] font-semibold text-text-2 cursor-pointer hover:bg-panel hover:text-danger disabled:cursor-not-allowed disabled:opacity-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          Undo
        </button>
      </div>
      {expanded ? (
        <div className="flex flex-col gap-2 border-t border-line bg-bg p-2">{children}</div>
      ) : null}
    </article>
  );
}
