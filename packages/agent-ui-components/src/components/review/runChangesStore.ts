// I1 — Frontend state surface for the authoritative review module.
//
// Owns the in-memory mirror of `RunChangeStore` for the current
// session: the durable inventory, the captured classification counts,
// the diff body, the staged-by-mutation map, the inline comment list,
// and the last Undo-conflict reason. The mirror is hydrated from the
// daemon client and rehydrates across restart because the daemon is
// the source of truth — this store only caches, never authors rows
// independently.
//
// The integration lane wires this store into the `WorkbenchContent`
// panes (I2). For now, callers subscribe via `useRunChangesState` and
// dispatch via the action creators below.

import type {
  ChangeEntry,
  DiffBodyResponse,
  DiffRow,
  RunChangesInventory,
  RunChangesSummary,
  AnchorStatus,
  CommentKind,
} from "./runChangesClient.js";

export interface RevisionComment {
  commentId: string;
  mutationId: string;
  path: string;
  anchorOffset: number;
  anchorStatus: AnchorStatus;
  kind: CommentKind;
  body: string;
  author: string;
  createdAt: string;
  resolvedAt: string | null;
}

export interface StaleAnchorAlert {
  path: string;
  mutationId: string;
  anchorOffset: number;
  anchorStatus: AnchorStatus;
  observedAt: string;
}

export interface RunChangesState {
  runId: string | null;
  workspaceRoot: string | null;
  inventory: RunChangesInventory | null;
  summary: RunChangesSummary | null;
  diffByPath: Map<string, DiffBodyResponse>;
  /** mutation id -> user-controlled stage override */
  stages: Map<string, "unstaged" | "staged" | "reverted">;
  /** mutation id -> last Undo conflict reason (sticky until cleared) */
  undoConflicts: Map<string, string>;
  comments: RevisionComment[];
  staleAnchors: StaleAnchorAlert[];
  search: string;
  selectedPath: string | null;
  loading: boolean;
  lastError: string | null;
  cardExpanded: boolean;
}

export type RunChangesAction =
  | { type: "capture/start"; runId: string; workspaceRoot: string }
  | { type: "capture/ok"; payload: RunChangesInventory; summary: RunChangesSummary }
  | { type: "capture/fail"; message: string }
  | { type: "summary/ok"; summary: RunChangesSummary }
  | { type: "diff/ok"; path: string; payload: DiffBodyResponse }
  | { type: "stage/set"; mutationId: string; stage: "unstaged" | "staged" | "reverted" }
  | { type: "search/set"; query: string }
  | { type: "selectPath"; path: string | null }
  | { type: "comment/add"; comment: RevisionComment }
  | { type: "staleAnchor/observe"; anchor: StaleAnchorAlert }
  | { type: "undoConflict/set"; mutationId: string; message: string }
  | { type: "undoConflict/clear"; mutationId: string }
  | { type: "card/expanded"; expanded: boolean };

export const INITIAL_RUN_CHANGES_STATE: RunChangesState = {
  runId: null,
  workspaceRoot: null,
  inventory: null,
  summary: null,
  diffByPath: new Map(),
  stages: new Map(),
  undoConflicts: new Map(),
  comments: [],
  staleAnchors: [],
  search: "",
  selectedPath: null,
  loading: false,
  lastError: null,
  cardExpanded: false,
};

export function reducer(state: RunChangesState, action: RunChangesAction): RunChangesState {
  switch (action.type) {
    case "capture/start":
      if (state.runId !== action.runId || state.workspaceRoot !== action.workspaceRoot) {
        return {
          ...INITIAL_RUN_CHANGES_STATE,
          diffByPath: new Map(),
          stages: new Map(),
          undoConflicts: new Map(),
          comments: [],
          staleAnchors: [],
          runId: action.runId,
          workspaceRoot: action.workspaceRoot,
          loading: true,
        };
      }
      return {
        ...state,
        runId: action.runId,
        workspaceRoot: action.workspaceRoot,
        loading: true,
        lastError: null,
      };
    case "capture/ok": {
      const stages = new Map<string, "unstaged" | "staged" | "reverted">();
      for (const entry of action.payload.entries) {
        stages.set(entry.mutationId, entry.stage);
      }
      return {
        ...state,
        inventory: action.payload,
        summary: action.summary,
        stages,
        diffByPath: new Map(),
        loading: false,
        lastError: null,
        cardExpanded: false,
      };
    }
    case "capture/fail":
      return { ...state, loading: false, lastError: action.message };
    case "summary/ok":
      return { ...state, summary: action.summary };
    case "diff/ok": {
      const diffByPath = new Map(state.diffByPath);
      diffByPath.set(action.path, action.payload);
      return { ...state, diffByPath };
    }
    case "stage/set": {
      const stages = new Map(state.stages);
      stages.set(action.mutationId, action.stage);
      return { ...state, stages };
    }
    case "search/set":
      return { ...state, search: action.query };
    case "selectPath":
      return { ...state, selectedPath: action.path };
    case "comment/add":
      return {
        ...state,
        comments: [...state.comments, action.comment],
      };
    case "staleAnchor/observe": {
      const others = state.staleAnchors.filter(
        (a) =>
          !(
            a.mutationId === action.anchor.mutationId &&
            a.anchorOffset === action.anchor.anchorOffset
          ),
      );
      if (action.anchor.anchorStatus === "live") return { ...state, staleAnchors: others };
      return { ...state, staleAnchors: [...others, action.anchor] };
    }
    case "undoConflict/set": {
      const conflicts = new Map(state.undoConflicts);
      conflicts.set(action.mutationId, action.message);
      return { ...state, undoConflicts: conflicts };
    }
    case "undoConflict/clear": {
      const conflicts = new Map(state.undoConflicts);
      conflicts.delete(action.mutationId);
      return { ...state, undoConflicts: conflicts };
    }
    case "card/expanded":
      return { ...state, cardExpanded: action.expanded };
    default:
      return state;
  }
}

export function findEntriesByClassification(
  inventory: RunChangesInventory | null,
  classifications: ReadonlyArray<ChangeEntry["classification"]>,
): ChangeEntry[] {
  if (!inventory) return [];
  return inventory.entries.filter((e) => classifications.includes(e.classification));
}

export function totalAdditionsDeletions(entries: ChangeEntry[]): {
  additions: number;
  deletions: number;
} {
  let additions = 0;
  let deletions = 0;
  for (const entry of entries) {
    additions += entry.additions;
    deletions += entry.deletions;
  }
  return { additions, deletions };
}

/** Filter rows by a case-insensitive substring match while preserving order. */
export function filterRowsForSearch(rows: DiffRow[], search: string): DiffRow[] {
  const term = search.trim().toLowerCase();
  if (!term) return rows;
  return rows.filter((row) => row.text.toLowerCase().includes(term));
}
