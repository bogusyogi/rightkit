// I1 — Compact ChangedFilesCard.
//
// Renders immediately after synthesis. It must:
//   - show totals + classification counts (so the user sees the shape
//     of the run's output at a glance)
//   - render the first few rows inline with a "show more" affordance
//   - offer two primary buttons: "Review" (opens the same durable
//     object in the ReviewPane) and "Undo" (fingerprint-checked)
//   - show partial / stale / conflict indicators up front so the
//     user doesn't open the pane unless something needs attention
//
// It reuses the `useRunChangesState` store so it points at the
// exact same inventory the ReviewPane opens. No second source of
// truth.

import { useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  type ChangeEntry,
  type RunChangesSummary,
  getRunChangesInventory,
  getRunChangesSummary,
  undoRunChange,
} from "./runChangesClient.js";
import { dispatch, useRunChangesState } from "./useRunChangesState.js";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";
import { useRightPanelStore } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";
import { projectLayoutOrDefault, useLayoutStore } from "@rightkit/agent-ui-core/state/layoutStore";

const PARTIAL_KEYS: ChangeEntry["classification"][] = [
  "partial",
  "conflict",
  "secret",
  "too_large",
];

export interface ChangedFilesCardProps {
  config: DaemonConfig | null;
  workspaceRoot: string | null;
  /** Optional: open the ReviewPane in-place. */
  onOpenReview?: () => void;
  /** Optional: hide the partial/conflict banner (callers handle it). */
  hideStateBanner?: boolean;
}

export default function ChangedFilesCard({
  config,
  workspaceRoot,
  onOpenReview,
  hideStateBanner,
}: ChangedFilesCardProps) {
  const state = useRunChangesState();
  const inventory = state.inventory;
  const summary = state.summary;
  const expanded = state.cardExpanded;
  const [pendingUndo, setPendingUndo] = useState<string | null>(null);

  // Nothing changed means nothing to say. This card used to render
  // "Edited 0 files · +0 −0" with a disabled Undo and an accent-styled Review
  // that did nothing — permanently, above the composer, on every turn.
  if (!inventory || !summary || inventory.entries.length === 0) {
    return null;
  }

  const visible = expanded ? inventory.entries : [];
  const partials = inventory.entries.filter((e) => PARTIAL_KEYS.includes(e.classification));
  const staleEntries = inventory.entries.filter((entry) => entry.anchorStatus !== "live");
  const partialTone = partials.length > 0;
  const hasConflicts = (state.undoConflicts.size ?? 0) > 0;
  const additions = inventory.entries.reduce((total, entry) => total + entry.additions, 0);
  const deletions = inventory.entries.reduce((total, entry) => total + entry.deletions, 0);
  const reversible = inventory.entries.filter(
    (entry) =>
      entry.undo !== null &&
      (state.stages.get(entry.mutationId) ?? entry.stage) !== "reverted",
  );
  const generatedDocuments = inventory.entries.filter(
    (entry) =>
      entry.classification === "generated" && /\.(?:md|mdx|txt|pdf|html?)$/i.test(entry.path),
  );

  return (
    <article
      data-testid="changed-files-card"
      title={`Run ${inventory.runId}`}
      className="flex w-full max-w-[var(--cr-measure)] flex-col self-start overflow-hidden rounded-lg border border-line bg-panel"
    >
      <header className="flex h-11 items-center gap-2 px-2.5">
        <span className="grid h-7 w-7 place-items-center rounded-md bg-panel-2 text-text-2">
          <Icon name="diff" size={13} />
        </span>
        <div className="min-w-0 flex-1">
          <strong className="block text-[12px] font-semibold leading-4 text-text">
            Edited {inventory.entries.length} {inventory.entries.length === 1 ? "file" : "files"}
          </strong>
          <span className="font-mono text-[10px] leading-3">
            <span className="text-accent-2">+{additions}</span>{" "}
            <span className="text-danger">-{deletions}</span>
            {inventory.truncated ? <span className="ml-2 text-warning">truncated</span> : null}
          </span>
        </div>
        <button
          type="button"
          aria-label={
            expanded
              ? "Hide edited files"
              : `Show ${inventory.entries.length} edited ${inventory.entries.length === 1 ? "file" : "files"}`
          }
          title={expanded ? "Hide files" : "Show files"}
          onClick={() => dispatch({ type: "card/expanded", expanded: !expanded })}
          data-testid="changed-files-card-show-more"
          className="cr-control-pill inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-[10px] text-text-3 hover:bg-panel-2 hover:text-text"
        >
          <span>{inventory.entries.length}</span>
          <span className={expanded ? "rotate-180" : ""}>
            <Icon name="chevron" size={10} />
          </span>
        </button>
        <button
          type="button"
          className="min-h-6 rounded-md border border-line bg-panel-2 px-2 py-1 text-[10px] text-text-2 hover:bg-panel disabled:opacity-45"
          disabled={pendingUndo !== null || reversible.length === 0}
          onClick={() =>
            void performUndoAll(
              config,
              workspaceRoot,
              summary,
              reversible,
              setPendingUndo,
              dispatch,
            )
          }
          data-testid="changed-files-card-undo-all"
        >
          {pendingUndo === "__all__" ? "Undoing…" : "Undo"}
        </button>
        {onOpenReview ? (
          <button
            type="button"
            className="min-h-6 rounded-md border border-accent/40 bg-accent/10 px-2 py-1 text-[10px] font-medium text-accent hover:bg-accent/15"
            onClick={onOpenReview}
            data-testid="changed-files-card-open-review"
          >
            Review
          </button>
        ) : null}
      </header>
      {!hideStateBanner && partialTone && expanded ? (
        <Banner
          tone="warning"
          title={`${partials.length} partial outcome${partials.length === 1 ? "" : "s"}`}
          subtitle={partials
            .slice(0, 3)
            .map((e) => `${e.classification}: ${e.path}`)
            .join(" · ")}
        />
      ) : null}
      {!hideStateBanner && hasConflicts ? (
        <Banner
          tone="danger"
          title="Undo conflicts"
          subtitle={`${state.undoConflicts.size} revert call${state.undoConflicts.size === 1 ? "" : "s"} rejected by fingerprint check`}
        />
      ) : null}
      {!hideStateBanner && staleEntries.length > 0 ? (
        <Banner
          tone="warning"
          title={`${staleEntries.length} stale review anchor${staleEntries.length === 1 ? "" : "s"}`}
          subtitle={staleEntries
            .slice(0, 3)
            .map((entry) => `${entry.path}: ${entry.anchorStatus}`)
            .join(" · ")}
        />
      ) : null}
      {expanded ? <ChangedFilesTotals summary={summary} /> : null}
      {expanded && generatedDocuments.length > 0 ? (
        <div
          className="border-t border-line bg-panel-2 px-2 py-1.5"
          data-testid="changed-files-generated-documents"
        >
          <span className="mb-1 block text-[10px] font-semibold uppercase tracking-widest text-text-3">
            Generated documents
          </span>
          {generatedDocuments.map((entry) => (
            <button
              key={entry.mutationId}
              type="button"
              className="flex h-7 w-full items-center gap-2 rounded-md px-1.5 text-left text-[11px] text-text-2 hover:bg-panel"
              onClick={() => openGeneratedDocument(entry.path)}
            >
              <Icon name="file" size={12} />
              <span className="min-w-0 flex-1 truncate font-mono">{entry.path}</span>
              <span className="text-text-3">Open</span>
            </button>
          ))}
        </div>
      ) : null}
      {expanded ? (
        <ul
          className="max-h-64 divide-y divide-line/50 overflow-y-auto border-t border-line bg-bg"
          data-testid="changed-files-card-rows"
        >
          {visible.map((entry) => (
            <li key={entry.mutationId} className="flex items-center gap-2 px-2 py-1.5 text-[12px]">
              <span
                className={`inline-flex h-5 items-center rounded-md border px-1.5 text-[10px] font-semibold ${CLASS_BADGE[entry.classification]}`}
                data-testid="changed-files-card-classification"
              >
                {entry.classification}
              </span>
              <span className="font-mono text-text-2 truncate">{entry.path}</span>
              <span className="ml-auto font-mono text-[10px] text-accent-2">
                +{entry.additions}
              </span>
              <span className="font-mono text-[10px] text-danger">-{entry.deletions}</span>
              <button
                type="button"
                className="min-h-6 rounded border border-line bg-panel-2 px-1.5 text-[10px] hover:bg-panel"
                onClick={() =>
                  void performUndo(config, workspaceRoot, summary, entry, setPendingUndo, dispatch)
                }
                disabled={
                  pendingUndo === entry.mutationId ||
                  entry.undo === null ||
                  (state.stages.get(entry.mutationId) ?? entry.stage) === "reverted"
                }
                data-testid="changed-files-card-undo"
              >
                {pendingUndo === entry.mutationId ? "…" : "undo"}
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </article>
  );
}

function openGeneratedDocument(path: string) {
  const workspace = useWorkspaceStore.getState();
  workspace.openFile(path);
  const projectId = workspace.workspaceRoot.trim() || "default";
  const layout = useLayoutStore.getState();
  const visible = projectLayoutOrDefault(layout.byProject, projectId).visible;
  layout.setVisible(projectId, visible.includes("files") ? visible : [...visible, "files"]);
  layout.setRightTab(projectId, "files");
  useRightPanelStore.getState().setTab("files");
  useSidePanelStore.getState().setOpen(true);
}

const CLASS_BADGE: Record<ChangeEntry["classification"], string> = {
  tracked: "bg-panel-2 text-text-2 border-line",
  untracked: "bg-accent-2/10 text-accent-2 border-accent-2/40",
  renamed: "bg-warning/10 text-warning border-warning/40",
  deleted: "bg-danger/10 text-danger border-danger/40",
  binary: "bg-panel-2 text-text-3 border-line",
  generated: "bg-panel-2 text-text-3 border-line",
  secret: "bg-danger/10 text-danger border-danger/40",
  too_large: "bg-warning/10 text-warning border-warning/40",
  conflict: "bg-danger/10 text-danger border-danger/40",
  complete: "bg-accent-2/10 text-accent-2 border-accent-2/40",
  partial: "bg-warning/10 text-warning border-warning/40",
};

function ChangedFilesTotals({ summary }: { summary: RunChangesSummary }) {
  const counts = Object.entries(summary.countsByClassification);
  return (
    <div className="flex flex-wrap items-center gap-2 text-[11px]">
      {counts.length > 0 ? (
        <span className="text-text-3">
          {counts.map(([scope, value]) => `${scope}:${value}`).join(" · ")}
        </span>
      ) : null}
    </div>
  );
}

async function performUndoAll(
  config: DaemonConfig | null,
  workspaceRoot: string | null,
  summary: RunChangesSummary,
  entries: ChangeEntry[],
  setPending: (id: string | null) => void,
  dispatchFn: typeof dispatch,
) {
  if (!config || !workspaceRoot || entries.length === 0) return;
  setPending("__all__");
  try {
    for (const entry of entries) {
      try {
        const response = await undoRunChange(config, {
          runId: summary.runId,
          workspaceRoot,
          mutationId: entry.mutationId,
          observedFingerprint: entry.fingerprint,
        });
        if (!response.applied) {
          dispatchFn({
            type: "undoConflict/set",
            mutationId: entry.mutationId,
            message: response.conflictReason ?? "fingerprint conflict",
          });
          continue;
        }
        dispatchFn({ type: "undoConflict/clear", mutationId: entry.mutationId });
        dispatchFn({ type: "stage/set", mutationId: entry.mutationId, stage: "reverted" });
      } catch (error) {
        dispatchFn({
          type: "undoConflict/set",
          mutationId: entry.mutationId,
          message: error instanceof Error ? error.message : "undo failed",
        });
      }
    }
    await refreshCardInventory(config, workspaceRoot, summary.runId, dispatchFn);
  } finally {
    setPending(null);
  }
}

async function refreshCardInventory(
  config: DaemonConfig,
  workspaceRoot: string,
  runId: string,
  dispatchFn: typeof dispatch,
) {
  try {
    const request = { runId, workspaceRoot };
    const [inventory, summary] = await Promise.all([
      getRunChangesInventory(config, request),
      getRunChangesSummary(config, request),
    ]);
    dispatchFn({ type: "capture/ok", payload: inventory, summary });
  } catch (error) {
    dispatchFn({
      type: "capture/fail",
      message: error instanceof Error ? error.message : "could not refresh changed files",
    });
  }
}

function Banner({
  tone,
  title,
  subtitle,
}: {
  tone: "warning" | "danger";
  title: string;
  subtitle: string;
}) {
  const cls =
    tone === "warning"
      ? "border-warning/40 bg-warning/5 text-warning"
      : "border-danger/40 bg-danger/5 text-danger";
  return (
    <div className={`rounded-md border px-2 py-1 text-[11px] ${cls}`}>
      <strong className="block font-semibold">{title}</strong>
      <p className="truncate">{subtitle}</p>
    </div>
  );
}

async function performUndo(
  config: DaemonConfig | null,
  workspaceRoot: string | null,
  summary: RunChangesSummary,
  entry: ChangeEntry,
  setPending: (id: string | null) => void,
  dispatchFn: typeof dispatch,
) {
  if (!config || !workspaceRoot) return;
  setPending(entry.mutationId);
  try {
    const response = await undoRunChange(config, {
      runId: summary.runId,
      workspaceRoot,
      mutationId: entry.mutationId,
      observedFingerprint: entry.fingerprint,
    });
    if (!response.applied) {
      dispatchFn({
        type: "undoConflict/set",
        mutationId: entry.mutationId,
        message: response.conflictReason ?? "fingerprint conflict",
      });
      return;
    }
    dispatchFn({
      type: "undoConflict/clear",
      mutationId: entry.mutationId,
    });
    dispatchFn({ type: "stage/set", mutationId: entry.mutationId, stage: "reverted" });
    await refreshCardInventory(config, workspaceRoot, summary.runId, dispatchFn);
  } catch (err) {
    dispatchFn({
      type: "undoConflict/set",
      mutationId: entry.mutationId,
      message: err instanceof Error ? err.message : "undo failed",
    });
  } finally {
    setPending(null);
  }
}
