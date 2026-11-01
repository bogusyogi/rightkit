import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { acquireResource, type ResourceKind } from "@rightkit/document-core/app/resourceCensus";
import { exposePerfFamilyForQa } from "@rightkit/document-core/documents/perfEventCounters";

export interface MarkdownColdTabState {
  readonly scrollTop: number;
  readonly scrollLeft: number;
  readonly selection?: { readonly anchor: number; readonly head: number };
  readonly renderedRevision?: number;
}

export interface MarkdownResourceCounters {
  readonly mountedProseMirror: number;
  readonly mountedMermaid: number;
  readonly activeObservers: number;
  readonly remounts: number;
  readonly cancelledWork: number;
}

const states = new Map<string, MarkdownColdTabState>();
const mounts = new Map<string, number>();
/** Lightweight restoration state covers current 20–50 tab stress sets. */
const COLD_TAB_STATE_MAX = 64;
const counters = {
  mountedProseMirror: 0,
  mountedMermaid: 0,
  activeObservers: 0,
  remounts: 0,
  cancelledWork: 0,
};

function key(documentId?: string): string {
  return documentId || "markdown:anonymous";
}

const PERSIST_KEY = "vr.markdown.tabState.v1";
const PERSIST_MAX = 64;

function readPersisted(): Record<string, MarkdownColdTabState> {
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : null;
    return parsed && typeof parsed === "object" ? (parsed as Record<string, MarkdownColdTabState>) : {};
  } catch {
    return {};
  }
}

/** Per-path scroll and caret that survives relaunch. Unlike the in-memory cold state it omits
 *  renderedRevision (meaningless across processes). Best effort; failures are ignored. */
export function persistMarkdownTabState(
  documentId: string | undefined,
  state: Omit<MarkdownColdTabState, "renderedRevision">,
): void {
  if (!documentId) return;
  try {
    const all = readPersisted();
    delete all[documentId];
    all[documentId] = state;
    const keys = Object.keys(all);
    for (const old of keys.slice(0, Math.max(0, keys.length - PERSIST_MAX))) delete all[old];
    localStorage.setItem(PERSIST_KEY, JSON.stringify(all));
  } catch {
    /* ignore */
  }
}

export function getMarkdownColdTabState(documentId?: string): MarkdownColdTabState | undefined {
  const id = key(documentId);
  let state = states.get(id);
  if (!state && documentId) {
    const saved = readPersisted()[documentId];
    if (saved && Number.isFinite(saved.scrollTop)) {
      state = { scrollTop: saved.scrollTop, scrollLeft: saved.scrollLeft ?? 0, selection: saved.selection };
    }
  }
  if (state) {
    states.delete(id);
    states.set(id, state);
  }
  return state;
}

export function rememberMarkdownColdTabState(
  documentId: string | undefined,
  state: MarkdownColdTabState,
): void {
  const id = key(documentId);
  states.delete(id);
  states.set(id, state);
  persistMarkdownTabState(documentId, state);
  if (states.size > COLD_TAB_STATE_MAX) {
    const oldest = states.keys().next().value;
    if (oldest !== undefined) states.delete(oldest);
  }
}

/** Call from document teardown once a Markdown tab is permanently closed. */
export function clearMarkdownColdTabState(documentId?: string): void {
  const id = key(documentId);
  states.delete(id);
  mounts.delete(id);
}

/** App cold-tab eviction must call this before unmounting a Markdown tab. */
export function canEvictMarkdownTab(
  drafts: BlockDraftRegistry | null | undefined,
  documentId?: string,
): boolean {
  if (!drafts) return true;
  return !drafts.hasDraft(key(documentId));
}

const CENSUS_KIND: Partial<Record<"prosemirror" | "mermaid" | "observer", ResourceKind>> = {
  prosemirror: "prosemirror-view",
  mermaid: "markdown-mermaid",
};

export function acquireMarkdownResource(
  kind: "prosemirror" | "mermaid" | "observer",
  documentId?: string,
): () => void {
  // Observers are cheap bookkeeping, not a resident engine — they stay out of the census.
  const censusKind = CENSUS_KIND[kind];
  const releaseCensus = censusKind
    ? acquireResource({ kind: censusKind, documentPath: documentId, format: "md" })
    : null;
  if (kind === "prosemirror") {
    counters.mountedProseMirror++;
    const id = key(documentId);
    const prior = mounts.get(id) ?? 0;
    if (prior > 0) counters.remounts++;
    mounts.set(id, prior + 1);
  } else if (kind === "mermaid") counters.mountedMermaid++;
  else counters.activeObservers++;

  let released = false;
  return () => {
    if (released) return;
    released = true;
    releaseCensus?.();
    if (kind === "prosemirror") counters.mountedProseMirror--;
    else if (kind === "mermaid") counters.mountedMermaid--;
    else counters.activeObservers--;
  };
}

export function createMarkdownWork(): {
  isCancelled(): boolean;
  complete(): void;
  cancel(): void;
} {
  let cancelled = false;
  let complete = false;
  return {
    isCancelled: () => cancelled,
    complete: () => {
      complete = true;
    },
    cancel: () => {
      if (cancelled || complete) return;
      cancelled = true;
      counters.cancelledWork++;
    },
  };
}

export function getMarkdownResourceCounters(): Readonly<MarkdownResourceCounters> {
  return { ...counters };
}

export function resetMarkdownResourceLifecycleForTests(): void {
  states.clear();
  mounts.clear();
  counters.mountedProseMirror = 0;
  counters.mountedMermaid = 0;
  counters.activeObservers = 0;
  counters.remounts = 0;
  counters.cancelledWork = 0;
}

// QA readout as `window.__viewrightQaPerf.read().markdownResources`. Reset zeroes only the
// cumulative counters; mounted and observer values are live gauges and must keep tracking.
exposePerfFamilyForQa("markdownResources", {
  read: () => getMarkdownResourceCounters(),
  reset: () => {
    counters.remounts = 0;
    counters.cancelledWork = 0;
  },
});
