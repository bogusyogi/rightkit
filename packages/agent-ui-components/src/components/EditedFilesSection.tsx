import Icon from "@rightkit/agent-ui-core/components/Icon";
import { useRunChangesState } from "./review/useRunChangesState.js";
import { useRightPanelStore } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";

/**
 * "Edited files" section in the sidebar.
 *
 * The run's edits used to be a card pinned under every transcript — permanent
 * chrome that described the session rather than the turn, and sat there even on
 * turns that changed nothing. Adrian's call (2026-08-04): the strip goes, the
 * sidebar carries the list. This reads the same `useRunChangesState` inventory
 * the review pane opens, so there is still exactly one source of truth.
 *
 * Renders nothing until a run actually edits something.
 */
export default function EditedFilesSection() {
  const state = useRunChangesState();
  const entries = state.inventory?.entries ?? [];

  if (entries.length === 0) return null;
  const additions = entries.reduce((total, entry) => total + entry.additions, 0);
  const deletions = entries.reduce((total, entry) => total + entry.deletions, 0);

  // Same two calls ChangedFilesCard used, so this opens the identical surface.
  function openReview() {
    useRightPanelStore.getState().setTab("files");
    useSidePanelStore.getState().setOpen(true);
  }

  return (
    <div className="flex-none border-t border-line px-2.5 py-2">
      <div className="flex items-center justify-between gap-2 pb-1">
        <span className="text-[10px] font-semibold uppercase tracking-[0.12em] text-text-3">
          Edited files
        </span>
        <button
          type="button"
          onClick={openReview}
          className="rounded-md border-0 bg-transparent px-1 text-[11px] text-text-3 cursor-pointer hover:text-text"
          title="Open review"
        >
          Review
        </button>
      </div>
      <div className="flex items-center gap-2 pb-1 text-[11px] tabular-nums text-text-3">
        <span>
          {entries.length} file{entries.length === 1 ? "" : "s"}
        </span>
        <span className="text-success">+{additions}</span>
        <span className="text-danger">-{deletions}</span>
      </div>
      <ul className="m-0 flex max-h-[9rem] list-none flex-col gap-0.5 overflow-y-auto">
        {entries.map((entry) => (
          <li key={entry.path}>
            <button
              type="button"
              onClick={openReview}
              title={entry.path}
              className="flex w-full items-center gap-1.5 rounded-md border-0 bg-transparent px-1 py-0.5 text-left text-[12px] text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text"
            >
              <Icon name="file" size={11} />
              {/* Show the filename, which is what identifies it at a glance;
                  the full path stays available on hover. */}
              <span className="truncate">{entry.path.split("/").pop() || entry.path}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
