import Icon from "@rightkit/agent-ui-core/components/Icon";
import { useRunChangesState } from "./useRunChangesState.js";

/**
 * One quiet line in the transcript noting what the run edited.
 *
 * Replaces the bordered card that used to be pinned under every transcript with
 * heading, +/- counts and two buttons — including on turns that edited nothing
 * (Adrian, 2026-08-04). Claude states what it touched as a line of text, not a
 * panel; the browsable list lives in the sidebar, and Review opens the same
 * durable inventory this reads.
 *
 * Renders nothing when the run changed no files.
 */
export default function EditedFilesInline({ onOpenReview }: { onOpenReview?: () => void }) {
  const state = useRunChangesState();
  const entries = state.inventory?.entries ?? [];
  if (entries.length === 0) return null;
  const additions = entries.reduce((total, entry) => total + entry.additions, 0);
  const deletions = entries.reduce((total, entry) => total + entry.deletions, 0);
  const feedbackCount = state.comments.length;
  const staleCount = Math.max(
    state.staleAnchors.length,
    entries.filter((entry) => entry.anchorStatus !== "live").length,
  );

  return (
    <div
      className="flex items-center gap-2 self-start py-1 text-[12px] text-text-3"
      data-testid="edited-files-inline"
    >
      <Icon name="diff" size={12} />
      <span>
        Edited {entries.length} file{entries.length === 1 ? "" : "s"}
      </span>
      <span className="tabular-nums text-success">+{additions}</span>
      <span className="tabular-nums text-danger">-{deletions}</span>
      {feedbackCount > 0 ? (
        <span className="text-text-3" data-testid="edited-files-inline-feedback-count">
          · {feedbackCount} note{feedbackCount === 1 ? "" : "s"}
        </span>
      ) : null}
      {staleCount > 0 ? (
        <span className="text-warning" data-testid="edited-files-inline-stale-count">
          · {staleCount} stale
        </span>
      ) : null}
      {onOpenReview ? (
        <button
          type="button"
          onClick={onOpenReview}
          className="inline-flex min-h-6 min-w-6 items-center rounded-md border-0 bg-transparent p-0 text-[12px] text-text-3 underline underline-offset-2 cursor-pointer hover:text-text"
        >
          Review
        </button>
      ) : null}
    </div>
  );
}
