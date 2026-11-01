import { Checkbox } from "@rightkit/react-shell/react";
// I1 — ReviewPane (authoritative diff/change review surface).
//
// A single durable view that:
//   - lists all changed paths in a virtualized tree
//   - shows scope headers (added / modified / deleted / untracked /
//     secret / binary / generated / too large / partial)
//   - renders the currently-selected path's diff in split or unified
//     mode, virtualized, with search, stage/unstage/revert buttons,
//     inline comments, stale-anchor indicators, and structured
//     revision feedback
//   - opens the same durable object the ChangedFilesCard points at
//
// The pane never infers mutations from assistant prose. It only
// reads from `RunChangeStore` via the typed client + reducer. The
// I2 lane wires this into the right-hand dock.

import { useCallback, useEffect, useMemo, useState, type CSSProperties } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  type ChangeEntry,
  type ChangeStage,
  type DiffBodyResponse,
  type RevisionUpdateResponse,
  type RunChangesInventory,
  getDiffBody,
  getRunChangesInventory,
  getRunChangesSummary,
  recordRevisionComment,
  undoRunChange,
} from "./runChangesClient";
import { filterRowsForSearch, type RevisionComment } from "./runChangesStore";
import { dispatch, getRunChangesStateSnapshot, useRunChangesState } from "./useRunChangesState";
import { stagePaths, unstagePaths } from "@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";

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

const SCOPE_ORDER: ChangeEntry["classification"][] = [
  "tracked",
  "untracked",
  "renamed",
  "deleted",
  "generated",
  "binary",
  "secret",
  "too_large",
  "conflict",
  "complete",
  "partial",
];

const SCOPE_LABEL: Record<ChangeEntry["classification"], string> = {
  tracked: "Modified",
  untracked: "Added",
  renamed: "Renamed",
  deleted: "Deleted",
  generated: "Generated",
  binary: "Binary",
  secret: "Secret (read-mask)",
  too_large: "Too large",
  conflict: "Conflict",
  complete: "Synthesized",
  partial: "Partial / failed",
};

export interface ReviewPaneProps {
  config: DaemonConfig | null;
  workspaceRoot: string | null;
  initialInventory?: RunChangesInventory | null;
  /** Optional override; used by the ChangedFilesCard "Open in review" affordance. */
  onRequestClose?: () => void;
  /** Draft a single revision request from all durable review notes. */
  onDraftRevisionRequest?: (prompt: string) => void;
  /** Maximum entries to render in the tree before virtualization kicks in. */
  virtualizationThreshold?: number;
}

export default function ReviewPane(props: ReviewPaneProps) {
  const initial = getRunChangesStateSnapshot();
  if (!initial.inventory && !initial.runId && !props.initialInventory) {
    return <EmptyReviewPane onRequestClose={props.onRequestClose} />;
  }
  return <ReviewPaneContent {...props} />;
}

function ReviewPaneContent({
  config,
  workspaceRoot,
  initialInventory,
  onRequestClose,
  onDraftRevisionRequest,
  virtualizationThreshold = 256,
}: ReviewPaneProps) {
  const state = useRunChangesState();
  const inventory = state.inventory ?? initialInventory ?? null;
  const [splitView, setSplitView] = useState(true);
  const [commentDraft, setCommentDraft] = useState("");
  const [author, setAuthor] = useState("reviewer");

  const grouped = useMemo(() => groupByScope(inventory?.entries ?? []), [inventory]);

  const selectedPath = state.selectedPath ?? inventory?.entries[0]?.path ?? null;
  const runId = state.runId ?? inventory?.runId ?? null;

  const refreshDiff = useCallback(
    async (path: string) => {
      if (!config || !runId || !workspaceRoot) return;
      dispatch({ type: "selectPath", path });
      try {
        // The daemon deliberately returns bounded windows. Hydrate every
        // available window here so the review surface never silently drops
        // the tail of a large diff (the Waku capped-diff invariant).
        let cursor: string | null = null;
        let payload: DiffBodyResponse | null = null;
        const rows: DiffBodyResponse["rows"] = [];
        let truncated = false;
        for (let page = 0; page < 32; page += 1) {
          const next = await getDiffBody(config, {
            runId,
            workspaceRoot,
            path,
            split: splitView,
            cursor,
            window: 1000,
            search: state.search || null,
          });
          payload = next;
          rows.push(...next.rows);
          truncated ||= next.truncated;
          if (!next.nextCursor) break;
          cursor = next.nextCursor;
          if (page === 31) truncated = true;
        }
        if (payload) {
          dispatch({
            type: "diff/ok",
            path,
            payload: { ...payload, rows, truncated },
          });
        }
      } catch (err) {
        dispatch({ type: "capture/fail", message: errorMessage(err) });
      }
    },
    [config, runId, splitView, state.search, workspaceRoot],
  );

  useEffect(() => {
    if (selectedPath) void refreshDiff(selectedPath);
  }, [refreshDiff, selectedPath]);

  async function refreshInventory() {
    if (!config || !runId || !workspaceRoot) return;
    try {
      const request = { runId, workspaceRoot };
      const [inv, summary] = await Promise.all([
        getRunChangesInventory(config, request),
        getRunChangesSummary(config, request),
      ]);
      dispatch({ type: "capture/ok", payload: inv, summary });
      const nextPath =
        inv.entries.find((entry) => entry.path === selectedPath)?.path ??
        inv.entries[0]?.path ??
        null;
      if (nextPath) await refreshDiff(nextPath);
    } catch (err) {
      dispatch({ type: "capture/fail", message: errorMessage(err) });
    }
  }

  async function recordComment(
    entry: ChangeEntry,
    anchorOffset = entry.anchorOffset,
    body = commentDraft,
  ): Promise<boolean> {
    if (!config || !runId || !workspaceRoot) return false;
    if (!body.trim()) return false;
    try {
      const response: RevisionUpdateResponse = await recordRevisionComment(config, {
        runId,
        workspaceRoot,
        mutationId: entry.mutationId,
        path: entry.path,
        anchorOffset,
        kind: "revision",
        body: body.trim(),
        author,
      });
      const comment: RevisionComment = {
        commentId: response.commentId,
        mutationId: entry.mutationId,
        path: entry.path,
        anchorOffset,
        anchorStatus: response.anchorStatus,
        kind: "revision",
        body: body.trim(),
        author,
        createdAt: new Date().toISOString(),
        resolvedAt: null,
      };
      dispatch({ type: "comment/add", comment });
      dispatch({
        type: "staleAnchor/observe",
        anchor: {
          path: entry.path,
          mutationId: entry.mutationId,
          anchorOffset,
          anchorStatus: response.anchorStatus,
          observedAt: new Date().toISOString(),
        },
      });
      setCommentDraft("");
      return true;
    } catch (err) {
      dispatch({ type: "capture/fail", message: errorMessage(err) });
      return false;
    }
  }

  async function performUndo(entry: ChangeEntry) {
    if (!config || !runId || !workspaceRoot) return;
    try {
      const response = await undoRunChange(config, {
        runId,
        workspaceRoot,
        mutationId: entry.mutationId,
        observedFingerprint: entry.fingerprint,
      });
      if (!response.applied) {
        dispatch({
          type: "undoConflict/set",
          mutationId: entry.mutationId,
          message: response.conflictReason ?? "fingerprint conflict",
        });
        return;
      }
      dispatch({ type: "undoConflict/clear", mutationId: entry.mutationId });
      dispatch({ type: "stage/set", mutationId: entry.mutationId, stage: "reverted" });
      await refreshInventory();
    } catch (err) {
      dispatch({
        type: "undoConflict/set",
        mutationId: entry.mutationId,
        message: errorMessage(err),
      });
    }
  }

  async function setStage(entry: ChangeEntry, stage: ChangeStage) {
    if (!config || !workspaceRoot || stage === "reverted") return;
    try {
      if (stage === "staged") {
        await stagePaths(config, workspaceRoot, [entry.path]);
      } else {
        await unstagePaths(config, workspaceRoot, [entry.path]);
      }
      dispatch({ type: "stage/set", mutationId: entry.mutationId, stage });
      await refreshDiff(entry.path);
    } catch (err) {
      dispatch({ type: "capture/fail", message: errorMessage(err) });
    }
  }

  function draftRevisionRequest() {
    if (!onDraftRevisionRequest || state.comments.length === 0) return;
    const notes = state.comments.map(
      (comment, index) =>
        `${index + 1}. ${comment.path} (anchor ${comment.anchorOffset}, ${comment.anchorStatus}): ${comment.body}`,
    );
    onDraftRevisionRequest(
      [
        "Please address these review notes, preserve unrelated work, and report each resolution:",
        "",
        ...notes,
      ].join("\n"),
    );
    onRequestClose?.();
  }

  if (!inventory && !runId) return <EmptyReviewPane onRequestClose={onRequestClose} />;

  return (
    <section data-testid="review-pane" className="flex h-full min-w-0 flex-col bg-panel">
      <header className="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2">
        <div className="flex flex-col">
          <span className="text-[10px] uppercase tracking-widest text-text-3 font-semibold">
            Review
          </span>
          <span className="text-[12px] text-text-2 truncate" data-testid="review-pane-workspace">
            {workspaceRoot ?? "no workspace"}
          </span>
        </div>
        <div className="ml-auto flex min-w-0 items-center gap-2">
          <Checkbox checked={splitView} onChange={(checked) => setSplitView(checked)} label="split view" className="flex items-center gap-1 text-[11px] text-text-3" />
          <input
            aria-label="Search review diffs"
            className="h-7 min-w-24 flex-1 rounded-md border border-line bg-bg px-2 text-[12px] text-text"
            placeholder="search diffs"
            value={state.search}
            onChange={(e) => dispatch({ type: "search/set", query: e.target.value })}
            data-testid="review-pane-search"
          />
          {onRequestClose ? (
            <button
              type="button"
              className="min-h-6 min-w-6 rounded-md border border-line bg-panel-2 p-1 text-text-3 transition hover:text-text"
              onClick={onRequestClose}
              aria-label="Close review pane"
            >
              <Icon name="close" size={12} />
            </button>
          ) : null}
        </div>
      </header>

      <div className="grid min-h-0 flex-1 grid-rows-[minmax(140px,34%)_minmax(0,1fr)] divide-y divide-line overflow-hidden">
        <ReviewTree
          grouped={grouped}
          selectedPath={selectedPath}
          onSelect={(path) => dispatch({ type: "selectPath", path })}
          stages={state.stages}
          undoConflicts={state.undoConflicts}
          onUndo={performUndo}
          onStage={setStage}
          virtualizationThreshold={virtualizationThreshold}
        />
        <ReviewBody
          inventory={inventory}
          config={config}
          selectedPath={selectedPath}
          rows={state.diffByPath.get(selectedPath ?? "")?.rows ?? []}
          truncated={state.diffByPath.get(selectedPath ?? "")?.truncated ?? false}
          search={state.search}
          splitView={splitView}
          comments={state.comments}
          commentDraft={commentDraft}
          setCommentDraft={setCommentDraft}
          author={author}
          setAuthor={setAuthor}
          onRecordComment={recordComment}
          refreshDiff={refreshDiff}
        />
      </div>
      {state.comments.length > 0 && onDraftRevisionRequest ? (
        <div className="flex items-center gap-2 border-t border-line bg-panel-2 px-3 py-2">
          <span className="min-w-0 flex-1 truncate text-[11px] text-text-3">
            {state.comments.length} saved review note{state.comments.length === 1 ? "" : "s"}
          </span>
          <button
            type="button"
            className="inline-flex h-7 items-center gap-1 rounded-md border border-line bg-panel px-2 text-[11px] text-text-2 hover:bg-bg"
            onClick={draftRevisionRequest}
            data-testid="review-pane-draft-revision"
          >
            <Icon name="edit" size={12} />
            Draft request
          </button>
        </div>
      ) : null}
      {state.lastError ? (
        <div
          className="border-t border-line bg-danger/5 px-3 py-2 text-[12px] text-danger"
          role="alert"
          data-testid="review-pane-error"
        >
          {state.lastError}
        </div>
      ) : null}
    </section>
  );
}

function EmptyReviewPane({ onRequestClose }: Pick<ReviewPaneProps, "onRequestClose">) {
  return (
    <section data-testid="review-pane" className="flex h-full min-w-0 flex-col bg-panel">
      <header className="flex items-center justify-between gap-2 border-b border-line px-3 py-2">
        <div>
          <p className="text-[10px] font-semibold uppercase tracking-widest text-text-3">Review</p>
          <p className="mt-1 text-[12px] text-text-2">No completed run to review yet.</p>
        </div>
        {onRequestClose ? (
          <button
            type="button"
            aria-label="Close review pane"
            onClick={onRequestClose}
            className="rounded-md border border-line p-1 text-text-3 hover:text-text"
          >
            <Icon name="close" size={12} />
          </button>
        ) : null}
      </header>
      <div className="m-auto max-w-sm px-5 text-center text-[13px] leading-5 text-text-2">
        Run a task with edits, then open Review to inspect grouped diffs, comments, or reverts.
      </div>
    </section>
  );
}

interface GroupedScope {
  scope: ChangeEntry["classification"];
  entries: ChangeEntry[];
}

function groupByScope(entries: ChangeEntry[]): GroupedScope[] {
  const byScope = new Map<ChangeEntry["classification"], ChangeEntry[]>();
  for (const entry of entries) {
    const bucket = byScope.get(entry.classification) ?? [];
    bucket.push(entry);
    byScope.set(entry.classification, bucket);
  }
  return SCOPE_ORDER.filter((scope) => byScope.has(scope)).map((scope) => ({
    scope,
    entries: byScope.get(scope) ?? [],
  }));
}

interface ReviewTreeProps {
  grouped: GroupedScope[];
  selectedPath: string | null;
  onSelect: (path: string) => void;
  stages: Map<string, ChangeStage>;
  undoConflicts: Map<string, string>;
  onUndo: (entry: ChangeEntry) => void;
  onStage: (entry: ChangeEntry, stage: ChangeStage) => void;
  virtualizationThreshold: number;
}

function ReviewTree({
  grouped,
  selectedPath,
  onSelect,
  stages,
  undoConflicts,
  onUndo,
  onStage,
  virtualizationThreshold,
}: ReviewTreeProps) {
  const total = grouped.reduce((count, g) => count + g.entries.length, 0);
  const virtualized = total >= virtualizationThreshold;
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(320);
  const virtualRows = useMemo(
    () =>
      grouped.flatMap((group) => [
        { kind: "scope" as const, key: `scope:${group.scope}`, group },
        ...group.entries.map((entry) => ({
          kind: "entry" as const,
          key: `entry:${entry.mutationId}`,
          entry,
        })),
      ]),
    [grouped],
  );
  const rowHeight = 48;
  const virtualStart = Math.max(0, Math.floor(scrollTop / rowHeight) - 6);
  const virtualEnd = Math.min(
    virtualRows.length,
    Math.ceil((scrollTop + viewportHeight) / rowHeight) + 6,
  );
  return (
    <div className="flex flex-col overflow-hidden" data-testid="review-pane-tree">
      <div className="border-b border-line bg-panel-2 px-2 py-1.5 text-[10px] uppercase tracking-widest text-text-3 font-semibold">
        {total} files · {virtualized ? "virtualized" : "list"}
      </div>
      <div
        className="flex-1 overflow-auto"
        onScroll={
          virtualized
            ? (event) => {
                setScrollTop(event.currentTarget.scrollTop);
                setViewportHeight(event.currentTarget.clientHeight);
              }
            : undefined
        }
      >
        {virtualized ? (
          <ul
            className="relative"
            style={{ height: virtualRows.length * rowHeight }}
            data-testid="review-pane-virtualized-tree"
          >
            {virtualRows.slice(virtualStart, virtualEnd).map((row, offset) => {
              const index = virtualStart + offset;
              return row.kind === "scope" ? (
                <li
                  key={row.key}
                  className="absolute inset-x-0 overflow-hidden"
                  style={{ height: rowHeight, transform: `translateY(${index * rowHeight}px)` }}
                >
                  <div className="flex h-full items-center gap-2 border-y border-line/60 bg-panel px-2 text-[10px] uppercase tracking-widest text-text-3 font-semibold">
                    <span
                      className={`inline-flex items-center rounded-md border px-1.5 text-[10px] font-semibold ${CLASS_BADGE[row.group.scope]}`}
                    >
                      {SCOPE_LABEL[row.group.scope]}
                    </span>
                    <span>{row.group.entries.length}</span>
                  </div>
                </li>
              ) : (
                <ReviewTreeEntry
                  key={row.key}
                  entry={row.entry}
                  selected={selectedPath === row.entry.path}
                  stage={stages.get(row.entry.mutationId) ?? row.entry.stage}
                  conflict={undoConflicts.get(row.entry.mutationId)}
                  onSelect={onSelect}
                  onUndo={onUndo}
                  onStage={onStage}
                  style={{ height: rowHeight, transform: `translateY(${index * rowHeight}px)` }}
                  virtual
                />
              );
            })}
          </ul>
        ) : (
          grouped.map((g) => (
            <details open className="border-b border-line/60" key={g.scope}>
              <summary className="sticky top-0 z-10 flex items-center gap-2 bg-panel px-2 py-1 text-[10px] uppercase tracking-widest text-text-3 font-semibold">
                <span
                  className={`inline-flex items-center rounded-md border px-1.5 text-[10px] font-semibold ${CLASS_BADGE[g.scope]}`}
                >
                  {SCOPE_LABEL[g.scope]}
                </span>
                <span>{g.entries.length}</span>
              </summary>
              <ul className="py-1">
                {g.entries.map((entry) => (
                  <ReviewTreeEntry
                    key={entry.mutationId}
                    entry={entry}
                    selected={selectedPath === entry.path}
                    stage={stages.get(entry.mutationId) ?? entry.stage}
                    conflict={undoConflicts.get(entry.mutationId)}
                    onSelect={onSelect}
                    onUndo={onUndo}
                    onStage={onStage}
                  />
                ))}
              </ul>
            </details>
          ))
        )}
      </div>
    </div>
  );
}

function ReviewTreeEntry({
  entry,
  selected,
  stage,
  conflict,
  onSelect,
  onUndo,
  onStage,
  style,
  virtual = false,
}: {
  entry: ChangeEntry;
  selected: boolean;
  stage: ChangeStage;
  conflict?: string;
  onSelect: (path: string) => void;
  onUndo: (entry: ChangeEntry) => void;
  onStage: (entry: ChangeEntry, stage: ChangeStage) => void;
  style?: CSSProperties;
  virtual?: boolean;
}) {
  return (
    <li
      className={`group flex h-full flex-col gap-1 overflow-hidden border-l-2 border-transparent px-2 py-1 text-[12px] ${virtual ? "absolute inset-x-0" : ""} ${selected ? "border-accent bg-panel-2" : ""}`}
      style={style}
    >
      <button
        type="button"
        onClick={() => onSelect(entry.path)}
        className="flex items-center gap-2 text-left font-mono text-text-2 hover:text-text"
        data-testid="review-pane-entry"
      >
        <span className="truncate">{entry.path}</span>
        <span
          className={`ml-auto rounded border px-1 text-[10px] ${stage === "staged" ? "border-accent-2/40 bg-accent-2/10 text-accent-2" : stage === "reverted" ? "border-warning/40 bg-warning/10 text-warning" : "border-line bg-panel-2 text-text-3"}`}
        >
          {stage}
        </span>
        <span className="font-mono text-[10px] text-accent-2">+{entry.additions}</span>
        <span className="font-mono text-[10px] text-danger">-{entry.deletions}</span>
      </button>
      {conflict ? (
        <p className="truncate text-[10px] text-danger">undo conflict: {conflict}</p>
      ) : null}
      <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100">
        <button
          type="button"
          className="rounded border border-line bg-panel-2 px-1.5 text-[10px] hover:bg-panel"
          onClick={() => onStage(entry, "unstaged")}
          data-testid="review-pane-unstage"
        >
          unstage
        </button>
        <button
          type="button"
          className="rounded border border-line bg-panel-2 px-1.5 text-[10px] hover:bg-panel"
          onClick={() => onStage(entry, "staged")}
          data-testid="review-pane-stage"
        >
          stage
        </button>
        <button
          type="button"
          className="ml-auto rounded border border-line bg-panel-2 px-1.5 text-[10px] hover:bg-panel"
          onClick={() => onUndo(entry)}
          data-testid="review-pane-revert"
        >
          revert
        </button>
      </div>
    </li>
  );
}

interface ReviewBodyProps {
  inventory: RunChangesInventory | null;
  config: DaemonConfig | null;
  selectedPath: string | null;
  rows: DiffBodyResponse["rows"];
  truncated: boolean;
  search: string;
  splitView: boolean;
  comments: RevisionComment[];
  commentDraft: string;
  setCommentDraft: (value: string) => void;
  author: string;
  setAuthor: (value: string) => void;
  onRecordComment: (
    entry: ChangeEntry,
    anchorOffset: number,
    body: string,
  ) => Promise<boolean>;
  refreshDiff: (path: string) => void;
}

function ReviewBody({
  inventory,
  config,
  selectedPath,
  rows,
  truncated,
  search,
  splitView,
  comments,
  commentDraft,
  setCommentDraft,
  author,
  setAuthor,
  onRecordComment,
  refreshDiff,
}: ReviewBodyProps) {
  const entry = useMemo(
    () => inventory?.entries.find((e) => e.path === selectedPath) ?? null,
    [inventory, selectedPath],
  );
  const visible = useMemo(() => filterRowsForSearch(rows, search), [rows, search]);
  const threadForPath = comments.filter((c) => c.path === selectedPath);
  const [diffScrollTop, setDiffScrollTop] = useState(0);
  const [diffViewportHeight, setDiffViewportHeight] = useState(480);
  const [feedbackAnchor, setFeedbackAnchor] = useState<number | null>(null);
  const [feedbackQuote, setFeedbackQuote] = useState("");
  const virtualizeDiff = visible.length > 400;

  // biome-ignore lint/correctness/useExhaustiveDependencies: changing selected file must clear prior diff feedback.
  useEffect(() => {
    setFeedbackAnchor(null);
    setFeedbackQuote("");
  }, [selectedPath]);

  function selectFeedbackRow(row: DiffBodyResponse["rows"][number]) {
    const anchor = diffRowAnchor(row);
    if (anchor === null) return;
    setFeedbackAnchor(anchor);
    setFeedbackQuote(row.text.trim().slice(0, 120));
  }

  const feedbackCount = (anchor: number | null) =>
    anchor === null ? 0 : threadForPath.filter((comment) => comment.anchorOffset === anchor).length;

  async function submitFeedback() {
    if (!entry) return;
    const saved = await onRecordComment(
      entry,
      feedbackAnchor ?? entry.anchorOffset,
      commentDraft,
    );
    if (saved) {
      setFeedbackAnchor(null);
      setFeedbackQuote("");
    }
  }

  const diffRowHeight = 42;
  const diffStart = Math.max(0, Math.floor(diffScrollTop / diffRowHeight) - 8);
  const diffEnd = Math.min(
    visible.length,
    Math.ceil((diffScrollTop + diffViewportHeight) / diffRowHeight) + 8,
  );
  return (
    <div className="flex flex-col overflow-hidden" data-testid="review-pane-body">
      <div className="flex items-center gap-2 border-b border-line bg-panel-2 px-3 py-1.5 text-[12px]">
        <span className="font-mono text-text-2 truncate">{selectedPath ?? "no file selected"}</span>
        <span className="ml-auto text-[10px] uppercase tracking-widest text-text-3 font-semibold">
          {splitView ? "split" : "unified"} · {visible.length} rows
        </span>
        <button
          type="button"
          className="rounded border border-line bg-panel-2 px-2 text-[10px] hover:bg-panel"
          onClick={() => selectedPath && refreshDiff(selectedPath)}
        >
          refresh
        </button>
      </div>
      <div
        className="flex-1 overflow-auto bg-bg font-mono text-[12px] leading-[1.55]"
        onScroll={
          virtualizeDiff
            ? (event) => {
                setDiffScrollTop(event.currentTarget.scrollTop);
                setDiffViewportHeight(event.currentTarget.clientHeight);
              }
            : undefined
        }
      >
        {visible.length === 0 ? (
          <div className="flex h-full items-center justify-center text-text-3 text-[12px]">
            no diff body available
          </div>
        ) : virtualizeDiff ? (
          <div
            className="relative"
            style={{ height: visible.length * diffRowHeight }}
            data-testid="review-pane-virtualized-diff"
          >
            {visible.slice(diffStart, diffEnd).map((row, offset) => (
              <div
                key={`${row.kind}-${row.oldLine ?? "x"}-${row.newLine ?? "x"}-${row.text}`}
                className="absolute inset-x-0 overflow-hidden px-3"
                style={{
                  height: diffRowHeight,
                  transform: `translateY(${(diffStart + offset) * diffRowHeight}px)`,
                }}
              >
                <ReviewDiffRow
                  row={row}
                  splitView={splitView}
                  feedbackCount={feedbackCount(diffRowAnchor(row))}
                  feedbackSelected={feedbackAnchor === diffRowAnchor(row)}
                  onFeedback={selectFeedbackRow}
                />
              </div>
            ))}
          </div>
        ) : (
          <pre className="m-0 grid grid-cols-[auto_1fr] px-3 py-2">
            {visible.map((row) => (
              <ReviewDiffRow
                key={`${row.kind}-${row.oldLine ?? "x"}-${row.newLine ?? "x"}-${row.text}`}
                row={row}
                splitView={splitView}
                feedbackCount={feedbackCount(diffRowAnchor(row))}
                feedbackSelected={feedbackAnchor === diffRowAnchor(row)}
                onFeedback={selectFeedbackRow}
              />
            ))}
          </pre>
        )}
        {truncated ? (
          <div className="border-t border-line bg-panel-2 px-3 py-1 text-[10px] uppercase tracking-widest text-warning font-semibold">
            truncated — request a higher window
          </div>
        ) : null}
      </div>
      <footer className="border-t border-line bg-panel px-3 py-2">
        <label
          htmlFor="review-author"
          className="block text-[10px] uppercase tracking-widest text-text-3 font-semibold"
        >
          Author
        </label>
        <input
          id="review-author"
          aria-label="Review comment author"
          value={author}
          onChange={(e) => setAuthor(e.target.value)}
          className="mt-1 h-7 w-full rounded-md border border-line bg-bg px-2 text-[12px]"
        />
        <label
          htmlFor="review-comment"
          className="mt-2 block text-[10px] uppercase tracking-widest text-text-3 font-semibold"
        >
          {feedbackAnchor === null ? "Structured revision" : `Line ${feedbackAnchor + 1} revision`}
        </label>
        {feedbackAnchor !== null ? (
          <p
            className="mt-1 truncate rounded-md border border-line bg-panel-2 px-2 py-1 text-[11px] text-text-3"
            data-testid="review-pane-feedback-quote"
          >
            {feedbackQuote || "selected diff line"}
          </p>
        ) : null}
        <textarea
          id="review-comment"
          aria-label="Review comment"
          value={commentDraft}
          onChange={(e) => setCommentDraft(e.target.value)}
          rows={3}
          className="mt-1 w-full rounded-md border border-line bg-bg px-2 py-1 text-[12px]"
          placeholder={
            feedbackAnchor === null
              ? "Explain what needs to change at this anchor"
              : "Explain what needs to change on this line"
          }
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              setFeedbackAnchor(null);
              setFeedbackQuote("");
            } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
              event.preventDefault();
              void submitFeedback();
            }
          }}
          data-testid="review-pane-comment-input"
        />
        <div className="mt-2 flex items-center gap-2">
          <button
            type="button"
            className="inline-flex h-7 items-center gap-1 rounded-md border border-line bg-panel-2 px-2 text-[11px] text-text-2 transition hover:bg-panel disabled:opacity-50"
            onClick={() => void submitFeedback()}
            disabled={!commentDraft.trim() || !entry || !config}
            data-testid="review-pane-comment-submit"
          >
            <Icon name="check" size={12} />
            Submit revision
          </button>
          {threadForPath.length > 0 ? (
            <span className="text-[10px] uppercase tracking-widest text-text-3 font-semibold">
              {threadForPath.length} comment{threadForPath.length === 1 ? "" : "s"}
            </span>
          ) : null}
          {entry?.anchorStatus && entry.anchorStatus !== "live" ? (
            <span className="rounded-md border border-warning/40 bg-warning/10 px-1.5 text-[10px] text-warning font-semibold">
              stale anchor · {entry.anchorStatus}
            </span>
          ) : null}
        </div>
      </footer>
    </div>
  );
}

function ReviewDiffRow({
  row,
  splitView,
  feedbackCount = 0,
  feedbackSelected = false,
  onFeedback,
}: {
  row: DiffBodyResponse["rows"][number];
  splitView: boolean;
  feedbackCount?: number;
  feedbackSelected?: boolean;
  onFeedback?: (row: DiffBodyResponse["rows"][number]) => void;
}) {
  const tone =
    row.kind === "add"
      ? "text-accent-2"
      : row.kind === "del"
        ? "text-danger"
        : row.kind === "hunk"
          ? "text-warning"
          : row.kind === "file"
            ? "text-text-3"
            : "text-text-2";
  const bg =
    row.kind === "add"
      ? "bg-accent-2/5"
      : row.kind === "del"
        ? "bg-danger/5"
        : row.kind === "hunk"
          ? "bg-warning/5"
          : "";
  const anchor = diffRowAnchor(row);
  return (
    <div className={`group relative col-span-2 grid grid-cols-2 ${bg} px-2`}>
      {splitView ? (
        <>
          <div className="border-r border-line/60 pr-2 text-[10px] text-text-3">
            {row.oldLine ?? ""}
          </div>
          <div className="pl-2 text-[10px] text-text-3">{row.newLine ?? ""}</div>
        </>
      ) : (
        <div className="col-span-2 text-[10px] text-text-3">
          {row.oldLine ?? ""} · {row.newLine ?? ""}
        </div>
      )}
      <span className={`col-span-2 truncate ${tone}`}>{row.text}</span>
      {onFeedback && anchor !== null ? (
        <button
          type="button"
          className={`absolute right-1 top-0 inline-flex min-h-6 min-w-6 items-center justify-center rounded border px-1 text-[10px] opacity-0 transition group-hover:opacity-100 group-focus-within:opacity-100 ${feedbackSelected ? "border-accent bg-accent/10 text-accent opacity-100" : "border-line bg-panel-2 text-text-3 hover:text-text"}`}
          aria-label={`${feedbackCount > 0 ? "Add another" : "Add"} feedback on line ${anchor + 1}`}
          title={feedbackCount > 0 ? `${feedbackCount} feedback note${feedbackCount === 1 ? "" : "s"}` : "Add line feedback"}
          onClick={() => onFeedback(row)}
          data-testid="review-pane-line-feedback"
        >
          <Icon name={feedbackCount > 0 ? "edit" : "plus"} size={10} />
          {feedbackCount > 0 ? <span className="ml-0.5">{feedbackCount}</span> : null}
        </button>
      ) : null}
    </div>
  );
}

function diffRowAnchor(row: DiffBodyResponse["rows"][number]): number | null {
  if (row.kind !== "add" && row.kind !== "del" && row.kind !== "ctx") return null;
  const line = row.newLine ?? row.oldLine;
  // Daemon anchors are zero-based file offsets; diff line numbers are one-based.
  return line === null ? null : Math.max(0, line - 1);
}

function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  return "unexpected error";
}
