// Renderer-owned editor buffer registry.
//
// This is the the host app seam for the Aethon editor buffer contract. The
// daemon remains authoritative for file contents; this registry keeps the
// in-memory draft, baseline, view state, and external-change warning alive
// while an editor surface unmounts or the active file changes.

import { create } from "zustand";
import type { WorkspaceFileResponse, WorkspaceFileSaveResponse } from "../lib/types/workspace-files";

/** Serializable view state supplied by an editor implementation. */
export interface EditorViewState {
  cursorLine?: number;
  cursorColumn?: number;
  selection?: {
    startLine: number;
    startColumn: number;
    endLine: number;
    endColumn: number;
  } | null;
  scrollTop?: number;
  scrollLeft?: number;
  [key: string]: unknown;
}

/**
 * The durable per-tab state previously held by Monaco's model.
 *
 * `content` and `baselineContent` are the framework-neutral equivalent of
 * the donor's model value and undo-preserving disk baseline. `viewState` is
 * deliberately opaque so Monaco, a textarea, or another editor can round
 * trip its cursor/selection/fold state without this store importing it.
 */
export interface EditorBuffer {
  bufferId: string;
  workspaceRoot: string;
  filePath: string;
  content: string;
  baselineContent: string;
  viewState: EditorViewState | null;
  loaded: boolean;
  loading: boolean;
  dirty: boolean;
  fingerprint: string | null;
  /** Highest daemon fingerprint observed; separate from the save precondition. */
  externalBaselineFingerprint: string | null;
  response: WorkspaceFileResponse | null;
  /** Latest daemon response held without replacing a dirty local draft. */
  remoteConflict: WorkspaceFileResponse | null;
  /** True when a newer daemon fingerprint needs an explicit user choice. */
  externalChanged: boolean;
}

export type ExternalChangeOutcome = "none" | "reload" | "flag";

/**
 * Adapt Aethon's mtime decision to the host app daemon file fingerprint.
 * Missing fingerprints are not treated as evidence of a change.
 */
export function decideExternalChange(
  previousFingerprint: string | null,
  nextFingerprint: string | null,
  isDirty: boolean,
): ExternalChangeOutcome {
  if (!previousFingerprint || !nextFingerprint || previousFingerprint === nextFingerprint) {
    return "none";
  }
  return isDirty ? "flag" : "reload";
}

export interface EditorBufferStoreState {
  buffers: Record<string, EditorBuffer>;
  ensureBuffer: (
    bufferId: string,
    workspaceRoot: string,
    filePath: string,
    initialContent?: string,
  ) => EditorBuffer;
  updateFilePath: (bufferId: string, workspaceRoot: string, filePath: string) => void;
  updateContent: (bufferId: string, content: string) => void;
  setViewState: (bufferId: string, viewState: EditorViewState | null) => void;
  setLoading: (bufferId: string, loading: boolean) => void;
  hydrate: (bufferId: string, response: WorkspaceFileResponse) => void;
  preserveDirtyRemote: (bufferId: string, response: WorkspaceFileResponse) => void;
  markSaved: (bufferId: string, response: WorkspaceFileSaveResponse) => void;
  restoreBaseline: (bufferId: string) => void;
  clearExternalChange: (bufferId: string) => void;
  dispose: (bufferId: string) => void;
  reset: () => void;
}

function emptyBuffer(
  bufferId: string,
  workspaceRoot: string,
  filePath: string,
  initialContent = "",
): EditorBuffer {
  return {
    bufferId,
    workspaceRoot,
    filePath,
    content: initialContent,
    baselineContent: initialContent,
    viewState: null,
    loaded: false,
    loading: false,
    dirty: false,
    fingerprint: null,
    externalBaselineFingerprint: null,
    response: null,
    remoteConflict: null,
    externalChanged: false,
  };
}

export const useEditorBufferStore = create<EditorBufferStoreState>((set, get) => ({
  buffers: {},

  ensureBuffer(bufferId, workspaceRoot, filePath, initialContent = "") {
    const existing = get().buffers[bufferId];
    if (existing) {
      if (existing.workspaceRoot !== workspaceRoot || existing.filePath !== filePath) {
        set((state) => ({
          buffers: {
            ...state.buffers,
            [bufferId]: { ...existing, workspaceRoot, filePath },
          },
        }));
      }
      return { ...existing, workspaceRoot, filePath };
    }
    const created = emptyBuffer(bufferId, workspaceRoot, filePath, initialContent);
    set((state) => ({ buffers: { ...state.buffers, [bufferId]: created } }));
    return created;
  },

  updateFilePath(bufferId, workspaceRoot, filePath) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      return { buffers: { ...state.buffers, [bufferId]: { ...buffer, workspaceRoot, filePath } } };
    });
  },

  updateContent(bufferId, content) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      return {
        buffers: {
          ...state.buffers,
          [bufferId]: { ...buffer, content, dirty: content !== buffer.baselineContent },
        },
      };
    });
  },

  setViewState(bufferId, viewState) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      return { buffers: { ...state.buffers, [bufferId]: { ...buffer, viewState } } };
    });
  },

  setLoading(bufferId, loading) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer || buffer.loading === loading) return state;
      return { buffers: { ...state.buffers, [bufferId]: { ...buffer, loading } } };
    });
  },

  hydrate(bufferId, response) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      const content = response.kind === "file" && response.content !== null ? response.content : "";
      return {
        buffers: {
          ...state.buffers,
          [bufferId]: {
            ...buffer,
            workspaceRoot: response.workspaceRoot,
            filePath: response.path,
            content,
            baselineContent: content,
            loaded: true,
            loading: false,
            dirty: false,
            fingerprint: response.fingerprint,
            externalBaselineFingerprint: response.fingerprint,
            response,
            remoteConflict: null,
            externalChanged: false,
          },
        },
      };
    });
  },

  preserveDirtyRemote(bufferId, response) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      return {
        buffers: {
          ...state.buffers,
          [bufferId]: {
            ...buffer,
            loaded: true,
            loading: false,
            externalBaselineFingerprint: response.fingerprint,
            remoteConflict: response,
            externalChanged: true,
          },
        },
      };
    });
  },

  markSaved(bufferId, response) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      const nextResponse: WorkspaceFileResponse = {
        workspaceRoot: buffer.workspaceRoot,
        path: buffer.filePath,
        kind: "file",
        sizeBytes: response.sizeBytes,
        modifiedAt: response.modifiedAt,
        fingerprint: response.fingerprint,
        content: buffer.content,
        permission: buffer.response?.permission ?? "",
        truncated: false,
        reason: null,
      };
      return {
        buffers: {
          ...state.buffers,
          [bufferId]: {
            ...buffer,
            loaded: true,
            loading: false,
            baselineContent: buffer.content,
            dirty: false,
            fingerprint: response.fingerprint,
            externalBaselineFingerprint: response.fingerprint,
            response: nextResponse,
            remoteConflict: null,
            externalChanged: false,
          },
        },
      };
    });
  },

  restoreBaseline(bufferId) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      return {
        buffers: {
          ...state.buffers,
          [bufferId]: { ...buffer, content: buffer.baselineContent, dirty: false },
        },
      };
    });
  },

  clearExternalChange(bufferId) {
    set((state) => {
      const buffer = state.buffers[bufferId];
      if (!buffer) return state;
      return {
        buffers: {
          ...state.buffers,
          [bufferId]: { ...buffer, externalChanged: false, remoteConflict: null },
        },
      };
    });
  },

  dispose(bufferId) {
    set((state) => {
      if (!(bufferId in state.buffers)) return state;
      const buffers = { ...state.buffers };
      delete buffers[bufferId];
      return { buffers };
    });
  },

  reset() {
    set({ buffers: {} });
  },
}));

/** Get a buffer without subscribing a component. */
export function getEditorBuffer(bufferId: string): EditorBuffer | undefined {
  return useEditorBufferStore.getState().buffers[bufferId];
}

/** Register or update the renderer buffer associated with an editor tab. */
export function registerEditorBuffer(
  bufferId: string,
  workspaceRoot: string,
  filePath: string,
  initialContent = "",
): EditorBuffer {
  return useEditorBufferStore.getState().ensureBuffer(bufferId, workspaceRoot, filePath, initialContent);
}

/** Explicit tab-close disposal; hidden project buckets remain cached. */
export function disposeEditorBuffer(bufferId: string): void {
  useEditorBufferStore.getState().dispose(bufferId);
}

/** Test-only reset, matching the donor registry's lifecycle seam. */
export function __resetEditorBuffers(): void {
  useEditorBufferStore.getState().reset();
}
