import { create } from "zustand";

import {
  REFERENCE_MAX_PER_SESSION,
  REFERENCE_STORAGE_KEY,
  fromReferenceMeta,
  toReferenceMeta,
  type ReferenceId,
  type ReferenceObject,
} from "../lib/types/references";

// D2 — typed ReferenceObject store.
//
// Sits beside `useComposerStore.attachments` (the legacy `Attachment` bytes
// for image vision delivery) and adds `references: ReferenceObject[]` — the
// metadata-only, persistent-across-restart chip strip that the desktop
// composer and `TranscriptBubble` both render.
//
// Persistence: a single `localStorage` key (`cr.references.v1`) holds the
// wire-friendly `ReferenceMeta[]` (no `previewUrl`, no `error`). On boot,
// `restorePersisted` revives those into `ReferenceObject`s and stamps an
// error state on image references whose bytes didn't survive the restart.

export interface ReferenceStoreState {
  /** All references currently attached to the composer. */
  references: ReferenceObject[];
  /** Pending error from `restorePersisted` (e.g. quota, JSON parse). */
  restoreError: string | null;
  addReferences: (next: ReferenceObject[]) => void;
  removeReference: (id: ReferenceId) => void;
  setError: (id: ReferenceId, error: string | undefined) => void;
  clear: () => void;
  restorePersisted: () => void;
  persist: () => void;
  /** Snapshot of the in-memory list as wire-friendly `ReferenceMeta[]`,
   * mirroring the Rust `ReferenceMeta` shape on submit. */
  toWireMeta: () => import("../lib/types/references").ReferenceMeta[];
}

function writePersistedMeta(meta: import("../lib/types/references").ReferenceMeta[]) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(REFERENCE_STORAGE_KEY, JSON.stringify(meta));
  } catch {
    // localStorage may be unavailable (private mode, quota); fall through silently.
  }
}

function dedupeById(refs: ReferenceObject[]): ReferenceObject[] {
  const seen = new Set<ReferenceId>();
  const out: ReferenceObject[] = [];
  for (const ref of refs) {
    if (!ref.id || seen.has(ref.id)) continue;
    seen.add(ref.id);
    out.push(ref);
  }
  return out;
}

export const useReferenceStore = create<ReferenceStoreState>((set, get) => ({
  references: [],
  restoreError: null,

  addReferences(next) {
    if (!Array.isArray(next) || next.length === 0) return;
    set((state) => {
      const merged = dedupeById([...state.references, ...next]);
      const clipped = merged.slice(-REFERENCE_MAX_PER_SESSION);
      return { references: clipped };
    });
    get().persist();
  },

  removeReference(id) {
    if (!id) return;
    set((state) => ({ references: state.references.filter((r) => r.id !== id) }));
    get().persist();
  },

  setError(id, error) {
    if (!id) return;
    set((state) => ({
      references: state.references.map((r) =>
        r.id === id
          ? error === undefined
            ? (() => {
                const { error: _omit, ...rest } = r;
                return rest as ReferenceObject;
              })()
            : { ...r, error }
          : r,
      ),
    }));
    // Errors are in-memory only — never persist.
  },

  clear() {
    set({ references: [], restoreError: null });
    get().persist();
  },

  restorePersisted() {
    try {
      const raw =
        typeof localStorage === "undefined" ? null : localStorage.getItem(REFERENCE_STORAGE_KEY);
      if (!raw) {
        set({ restoreError: null });
        return;
      }
      const parsed: unknown = JSON.parse(raw);
      if (!Array.isArray(parsed)) {
        set({ references: [], restoreError: null });
        return;
      }
      const persisted = parsed as import("../lib/types/references").ReferenceMeta[];
      const revived = persisted.map(fromReferenceMeta);
      set({ references: dedupeById(revived), restoreError: null });
    } catch (err) {
      set({ restoreError: err instanceof Error ? err.message : "restore failed" });
    }
  },

  persist() {
    const meta = get().references.map(toReferenceMeta);
    writePersistedMeta(meta);
  },

  toWireMeta() {
    return get().references.map(toReferenceMeta);
  },
}));
