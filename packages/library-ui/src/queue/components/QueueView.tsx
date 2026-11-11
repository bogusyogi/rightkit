import { useCallback, useEffect, useMemo, useState } from "react";
import type { JobInfo, MediaSpec } from "../contract.js";
import { selectWorkbenchJobId } from "../lib/workbench.js";
import { PasteBar } from "./PasteBar.js";
import { QueueBoard } from "./QueueBoard.js";
import { WorkbenchPreview } from "./WorkbenchPreview.js";

type QueueViewProps = {
  jobs: JobInfo[];
  // S-6: while the engine is recovering or down, the paste/file picker
  // affordances show as disabled so a click surfaces a visible cause
  // (the lifecycle banner) instead of vanishing an error in the toaster.
  submitBlocked?: boolean;
  onAdd: (inputs: string[], media?: MediaSpec) => Promise<boolean> | boolean;
  onPickFiles: () => void;
  onOpen: (id: string) => void;
  onRemove: (id: string) => void;
  onCancel: (id: string) => void;
  onRerun: (job: JobInfo) => void;
  onClearCompleted: () => void;
  onClearAll: () => void;
  onRetryAllFailed: () => void;
  onRetryAfterSignin: (job: JobInfo, options?: { skipExport?: boolean }) => void;
  onError: (message: string) => void;
};

export function QueueView({
  jobs,
  submitBlocked = false,
  onAdd,
  onPickFiles,
  onOpen,
  onRemove,
  onCancel,
  onRerun,
  onClearCompleted,
  onClearAll,
  onRetryAllFailed,
  onRetryAfterSignin,
  onError,
}: QueueViewProps) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // When the user explicitly closes the preview we stop auto-reselecting, so
  // the workbench can return to the empty "No source selected" state.
  const [closed, setClosed] = useState(false);
  // Track the set of completed job ids so we can re-enable auto-select when
  // a new job finishes (otherwise the closed flag is permanent).
  const [doneIds, setDoneIds] = useState<Set<string>>(
    () => new Set(jobs.filter((j) => j.stage === "done").map((j) => j.id)),
  );
  // When a new job reaches "done", re-enable auto-select so the workbench
  // shows it immediately — even if the user had previously closed the preview.
  useEffect(() => {
    const currentDone = new Set(jobs.filter((j) => j.stage === "done").map((j) => j.id));
    const hasNew = [...currentDone].some((id) => !doneIds.has(id));
    if (hasNew) {
      setClosed(false);
      setDoneIds(currentDone);
    } else if (currentDone.size !== doneIds.size) {
      setDoneIds(currentDone);
    }
  }, [jobs, doneIds]);

  const nextSelectedId = useMemo(() => {
    if (closed) {
      // Keep a still-valid selection, but never auto-pick a fresh one.
      return selectedId && jobs.some((job) => job.id === selectedId) ? selectedId : null;
    }
    return selectWorkbenchJobId(jobs, selectedId);
  }, [jobs, selectedId, closed]);
  const selectedJob = jobs.find((job) => job.id === selectedId) ?? null;

  useEffect(() => {
    setSelectedId(nextSelectedId);
  }, [nextSelectedId]);

  const selectJob = useCallback((id: string) => {
    setClosed(false);
    setSelectedId(id);
  }, []);

  const closePreview = useCallback(() => {
    setClosed(true);
    setSelectedId(null);
  }, []);

  return (
    <main className="grid min-h-0 flex-1 grid-rows-[auto_auto_minmax(0,1fr)] gap-3 p-4">
      <PasteBar onAdd={onAdd} onPickFiles={onPickFiles} submitBlocked={submitBlocked} />

      <QueueBoard
        jobs={jobs}
        selectedId={selectedId}
        onSelect={selectJob}
        onOpen={onOpen}
        onRemove={onRemove}
        onCancel={onCancel}
        onRerun={onRerun}
        onClearCompleted={onClearCompleted}
        onClearAll={onClearAll}
        onRetryAllFailed={onRetryAllFailed}
        onRetryAfterSignin={onRetryAfterSignin}
        onError={onError}
        preview={
          <WorkbenchPreview
            job={selectedJob}
            onOpen={onOpen}
            onClose={closePreview}
            onRetryAfterSignin={onRetryAfterSignin}
            onError={onError}
          />
        }
      />
    </main>
  );
}
