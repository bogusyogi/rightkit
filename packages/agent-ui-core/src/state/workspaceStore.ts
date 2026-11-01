// D1 — Live workspace files store.
//
// Holds the locally-known state of the file tree: which directories are
// expanded, the last-known entries for each expanded path, the currently
// selected entry, the open tab in the file viewer, and the recents list.
// The store is daemon-source-of-truth: a successful `getWorkspaceTree` or
// `postWorkspaceTreeExpand` overwrites the entries for that path; the
// `lastKnown` flag survives transient errors so the UI can keep showing
// stale content while a retry runs.
//
// "last-known" is the daemon-side contract that D1 mirrors on the client:
// when a forced refresh fails, the previous entries stay attached to the
// path with `lastKnown: true` so the user sees the prior tree plus a
// retry surface, never an empty or duplicated rebuild.

import { create } from "zustand";
import type { WorkspaceEntry, WorkspaceRecentEntry } from "../lib/types/workspace-files.js";

export interface WorkspaceTreePath {
  /** Workspace-relative directory path; "" → root. */
  path: string;
  entries: WorkspaceEntry[];
  truncated: boolean;
  loadedAt: number;
  /** True when the current entries are last-known (refresh failed). */
  lastKnown: boolean;
}

export interface WorkspaceStoreState {
  workspaceRoot: string;
  expanded: Record<string, WorkspaceTreePath>;
  selection: WorkspaceSelection | null;
  openedFilePath: string | null;
  recents: WorkspaceRecentEntry[];
  scrollToken: number;

  setWorkspaceRoot: (root: string) => void;
  rememberPath: (
    path: string,
    entries: WorkspaceEntry[],
    truncated: boolean,
    options?: { lastKnown?: boolean },
  ) => void;
  /**
   * Update only the `lastKnown` flag of an already-remembered path —
   * used when a refresh fails and we want to keep the previous entries
   * attached to the path while flipping the flag.
   */
  markPathLastKnown: (path: string) => void;
  forgetPath: (path: string) => void;
  toggleExpanded: (path: string) => void;
  setExpanded: (path: string, expanded: boolean) => void;
  setSelection: (next: WorkspaceSelection | null) => void;
  openFile: (path: string | null) => void;
  setRecents: (recents: WorkspaceRecentEntry[]) => void;
  bumpScrollToken: () => void;
  resetForWorkspace: (root: string) => void;
}

export interface WorkspaceSelection {
  path: string;
  /** Filled by `getWorkspaceFile`; null while pending. */
  fingerprint: string | null;
}

const EMPTY_SELECTION: WorkspaceSelection | null = null;

export const useWorkspaceStore = create<WorkspaceStoreState>((set, get) => ({
  workspaceRoot: "",
  expanded: {},
  selection: EMPTY_SELECTION,
  openedFilePath: null,
  recents: [],
  scrollToken: 0,

  setWorkspaceRoot(root) {
    if (get().workspaceRoot === root) return;
    set({
      workspaceRoot: root,
      expanded: {},
      selection: EMPTY_SELECTION,
      openedFilePath: null,
      scrollToken: get().scrollToken + 1,
    });
  },

  resetForWorkspace(root) {
    set({
      workspaceRoot: root,
      expanded: {},
      selection: EMPTY_SELECTION,
      openedFilePath: null,
      recents: [],
      scrollToken: get().scrollToken + 1,
    });
  },

  rememberPath(path, entries, truncated, options) {
    const key = normalizePath(path);
    set((state) => {
      const prev = state.expanded[key];
      return {
        expanded: {
          ...state.expanded,
          [key]: {
            path: key,
            entries,
            truncated,
            loadedAt: Date.now(),
            lastKnown: options?.lastKnown ?? prev?.lastKnown ?? false,
          },
        },
      };
    });
  },

  markPathLastKnown(path) {
    const key = normalizePath(path);
    set((state) => {
      const prev = state.expanded[key];
      if (!prev) return state;
      return {
        expanded: {
          ...state.expanded,
          [key]: { ...prev, lastKnown: true },
        },
      };
    });
  },

  forgetPath(path) {
    const key = normalizePath(path);
    set((state) => {
      if (!(key in state.expanded)) return state;
      const next = { ...state.expanded };
      delete next[key];
      return { expanded: next };
    });
  },

  toggleExpanded(path) {
    const key = normalizePath(path);
    const has = Boolean(get().expanded[key]);
    if (has) {
      get().forgetPath(path);
    } else {
      get().rememberPath(path, [], false, { lastKnown: false });
    }
  },

  setExpanded(path, expanded) {
    const key = normalizePath(path);
    if (expanded) {
      get().rememberPath(path, get().expanded[key]?.entries ?? [], false);
    } else {
      get().forgetPath(path);
    }
  },

  setSelection(next) {
    set({ selection: next });
  },

  openFile(path) {
    set({ openedFilePath: path });
  },

  setRecents(recents) {
    set({ recents });
  },

  bumpScrollToken() {
    set({ scrollToken: get().scrollToken + 1 });
  },
}));

export function normalizePath(input: string): string {
  const trimmed = input.trim().replace(/\\/g, "/");
  if (!trimmed || trimmed === "/") return "";
  const noLeading = trimmed.startsWith("/") ? trimmed.slice(1) : trimmed;
  const noTrailing = noLeading.endsWith("/") ? noLeading.slice(0, -1) : noLeading;
  return noTrailing;
}

export function pathJoin(base: string, leaf: string): string {
  const a = normalizePath(base);
  const b = normalizePath(leaf);
  if (!a) return normalizePath(b);
  if (!b) return a;
  return normalizePath(`${a}/${b}`);
}

export function pathParent(path: string): string {
  const normalized = normalizePath(path);
  if (!normalized) return "";
  const idx = normalized.lastIndexOf("/");
  return idx < 0 ? "" : normalized.slice(0, idx);
}

export function pathBasename(path: string): string {
  const normalized = normalizePath(path);
  if (!normalized) return "";
  const idx = normalized.lastIndexOf("/");
  return idx < 0 ? normalized : normalized.slice(idx + 1);
}
