// ── Debounced writers (T2-1) ────────────────────────────────────────────────────────────────────
// writeDraft/writeFileDraft below are synchronous localStorage.setItem calls — JSON.stringify-ing the
// whole document on every keystroke is O(doc) blocking I/O per char. writeDraftDebounced/
// writeFileDraftDebounced (further down) are what onTxtChange calls instead: they coalesce rapid
// keystrokes into one write ~400ms after typing pauses (matches the useViewState house debounce),
// keyed per-path so two files being edited in quick succession never clobber each other's pending
// write. The pending-write maps live up top so clearDraft/clearFileDraft can cancel them.
//
// Data-loss guardrails:
//  - clearDraft/clearFileDraft cancel any pending write for that path before removing the key, so a
//    save/discard can never be undone by a stale debounced write landing afterward.
//  - flushDraft/flushDrafts synchronously commit a pending write immediately; callers invoke these
//    before an operation that may read the draft back or before the process may exit (save, close,
//    beforeunload/visibilitychange-hidden) so no in-flight edit is silently dropped.
const DRAFT_DEBOUNCE_MS = 400;
const untitledPending = new Map<string, { timer: ReturnType<typeof setTimeout>; text: string }>();
const fileDraftPending = new Map<
  string,
  { timer: ReturnType<typeof setTimeout>; draft: FileDraft }
>();

export const draftKey = (path: string) => `vr.draft.${path}`;

export function readDraft(path: string): string | null {
  return localStorage.getItem(draftKey(path));
}

export function writeDraft(path: string, text: string): void {
  localStorage.setItem(draftKey(path), text);
}

export function clearDraft(path: string): void {
  const pending = untitledPending.get(path);
  if (pending) {
    clearTimeout(pending.timer);
    untitledPending.delete(path);
  }
  localStorage.removeItem(draftKey(path));
}

// File-backed text draft: persists edit buffer + the disk content it was based on across sessions.
// Stored under a separate key prefix so the untitled-buffer startup restore doesn't touch them.
// On reopen: if `base` still matches disk content, the draft is applied; if the file changed on
// disk between sessions, the draft is discarded silently rather than clobbering new disk content.
export interface FileDraft {
  base: string;
  text: string;
}
const fileDraftKey = (path: string) => `vr.filedraft.${path}`;
export function readFileDraft(path: string): FileDraft | null {
  try {
    const raw = localStorage.getItem(fileDraftKey(path));
    if (!raw) return null;
    const d = JSON.parse(raw) as { base?: unknown; text?: unknown };
    if (typeof d.base === "string" && typeof d.text === "string") {
      return { base: d.base, text: d.text };
    }
    return null;
  } catch {
    return null;
  }
}
export function writeFileDraft(path: string, draft: FileDraft): void {
  try {
    localStorage.setItem(fileDraftKey(path), JSON.stringify(draft));
  } catch {
    /* storage full — skip */
  }
}
export function clearFileDraft(path: string): void {
  const pending = fileDraftPending.get(path);
  if (pending) {
    clearTimeout(pending.timer);
    fileDraftPending.delete(path);
  }
  localStorage.removeItem(fileDraftKey(path));
}

export function writeDraftDebounced(path: string, text: string): void {
  const prior = untitledPending.get(path);
  if (prior) clearTimeout(prior.timer);
  const timer = setTimeout(() => {
    untitledPending.delete(path);
    writeDraft(path, text);
  }, DRAFT_DEBOUNCE_MS);
  untitledPending.set(path, { timer, text });
}

export function writeFileDraftDebounced(path: string, draft: FileDraft): void {
  const prior = fileDraftPending.get(path);
  if (prior) clearTimeout(prior.timer);
  const timer = setTimeout(() => {
    fileDraftPending.delete(path);
    writeFileDraft(path, draft);
  }, DRAFT_DEBOUNCE_MS);
  fileDraftPending.set(path, { timer, draft });
}

/** Synchronously commit path's pending debounced write (if any) right now. Idempotent/no-op if nothing pending. */
export function flushDraft(path: string): void {
  const untitled = untitledPending.get(path);
  if (untitled) {
    clearTimeout(untitled.timer);
    untitledPending.delete(path);
    writeDraft(path, untitled.text);
  }
  const file = fileDraftPending.get(path);
  if (file) {
    clearTimeout(file.timer);
    fileDraftPending.delete(path);
    writeFileDraft(path, file.draft);
  }
}

/** Synchronously commit every pending debounced draft write. Call before the process may exit. */
export function flushDrafts(): void {
  for (const path of new Set([...untitledPending.keys(), ...fileDraftPending.keys()]))
    flushDraft(path);
}
