/**
 * Cross-format resident-resource census.
 *
 * Markdown and HTML already own deterministic per-format counters
 * (`markdownResourceLifecycle.ts`, `htmlResourceLifecycle.ts`), but they answer only
 * "how many ProseMirrors does Markdown have". The mount policy needs the other question:
 * *what is resident right now, across every format, and how expensive is it?*
 *
 * This module is that single answer. Format code acquires a handle when it allocates a
 * heavyweight runtime and calls the returned release on teardown. Nothing here samples
 * wall-clock time or memory, so counts stay deterministic and test-assertable — the same
 * philosophy as `tabMountPolicy.test.ts`. Byte estimates are explicit inputs from the
 * caller (canvas w*h*4), never measured, so they remain reproducible across platforms.
 *
 * This is the observability step the format architecture pack makes a precondition for
 * changing any lifetime policy: see `docs/format-architecture/01-PERFORMANCE-AND-RESOURCE-CONTRACT.md`.
 */

/** Resident cost class. Drives eviction preference, not correctness. */
export type RuntimeClass = "light" | "medium" | "heavy";

/**
 * Every heavyweight runtime the host app can hold open. Adding a kind here is deliberate
 * friction: a new resident engine should have to declare itself.
 */
export type ResourceKind =
  | "codemirror-view"
  | "prosemirror-view"
  | "markdown-mermaid"
  | "html-iframe"
  | "mdx-iframe"
  | "web-iframe"
  | "docx-editor"
  | "pdf-document"
  | "pdf-worker"
  | "pdf-preview-worker"
  | "pdf-preview-document"
  | "pdf-native-reader"
  | "pdf-native-parked-reader"
  | "pdf-page-canvas"
  | "image-viewer"
  | "image-decoded"
  | "fabric-canvas";

export type DocumentFormat =
  | "txt"
  | "md"
  | "mdx"
  | "html"
  | "web"
  | "docx"
  | "pdf"
  | "image"
  | "unknown";

const RUNTIME_CLASS: Record<ResourceKind, RuntimeClass> = {
  "codemirror-view": "light",
  "prosemirror-view": "medium",
  "markdown-mermaid": "medium",
  "html-iframe": "heavy",
  "mdx-iframe": "heavy",
  "web-iframe": "heavy",
  "docx-editor": "heavy",
  "pdf-document": "heavy",
  "pdf-worker": "heavy",
  "pdf-preview-worker": "heavy",
  "pdf-preview-document": "heavy",
  "pdf-native-reader": "heavy",
  "pdf-native-parked-reader": "heavy",
  "pdf-page-canvas": "heavy",
  "image-viewer": "heavy",
  "image-decoded": "heavy",
  "fabric-canvas": "heavy",
};

export interface ResourceAcquisition {
  readonly kind: ResourceKind;
  /** Owning document. Absent for app-level singletons such as the shared PDF worker. */
  readonly documentPath?: string;
  readonly format?: DocumentFormat;
  /**
   * Caller-computed byte estimate (e.g. canvas width * height * 4). Deliberately an input,
   * not a measurement, so the census stays deterministic. Omit when not meaningfully sized.
   */
  readonly estimatedBytes?: number;
  /**
   * True when this resource holds unsaved state that would be lost on unmount. A dirty
   * resource with no external owner is exactly what makes a pane un-evictable, so the
   * census tracks it rather than leaving it implicit in the tab layer.
   */
  readonly dirty?: boolean;
  /**
   * True when the authoritative edit state lives outside this runtime and remount can
   * restore it exactly. A dirty-but-externalized resource IS evictable. This is the flag
   * image edit-recipe externalization is meant to flip.
   */
  readonly stateExternalized?: boolean;
}

interface LiveEntry extends ResourceAcquisition {
  readonly id: number;
}

export interface FormatCensus {
  readonly live: number;
  readonly heavy: number;
  readonly estimatedBytes: number;
  /** Dirty resources whose state has no external owner — these block eviction. */
  readonly pinnedDirty: number;
  /** Dirty resources whose state is externalized — evictable despite being dirty. */
  readonly evictableDirty: number;
}

export interface ResourceCensus {
  readonly live: number;
  readonly heavy: number;
  readonly estimatedBytes: number;
  readonly pinnedDirty: number;
  readonly evictableDirty: number;
  readonly byKind: Readonly<Partial<Record<ResourceKind, number>>>;
  readonly byFormat: Readonly<Partial<Record<DocumentFormat, FormatCensus>>>;
  /** Cumulative totals; useful for leak detection across a stress run. */
  readonly totals: { readonly acquired: number; readonly released: number };
}

const live = new Map<number, LiveEntry>();
let nextId = 1;
let acquired = 0;
let released = 0;

/** Release handles are opaque, so map back by identity through the registry. */
const handleIds = new WeakMap<() => void, number>();

function findEntry(release: () => void): LiveEntry | undefined {
  const id = handleIds.get(release);
  return id === undefined ? undefined : live.get(id);
}

/**
 * Register a heavyweight runtime. Returns its release; calling it twice is a no-op so a
 * React cleanup that races a StrictMode double-invoke cannot drive counts negative.
 */
export function acquireResource(acquisition: ResourceAcquisition): () => void {
  const id = nextId++;
  live.set(id, { ...acquisition, id });
  acquired++;
  let done = false;
  const release = () => {
    if (done) return;
    done = true;
    live.delete(id);
    released++;
  };
  handleIds.set(release, id);
  return release;
}

/**
 * Update a live resource in place — a pane going dirty, or its state becoming externalized.
 * Returns false when the handle is already released, so callers can detect late updates.
 */
export function updateResource(
  release: () => void,
  patch: Partial<
    Pick<ResourceAcquisition, "kind" | "dirty" | "stateExternalized" | "estimatedBytes">
  >,
): boolean {
  const entry = findEntry(release);
  if (!entry) return false;
  live.set(entry.id, { ...entry, ...patch });
  return true;
}

function emptyFormat(): FormatCensus {
  return { live: 0, heavy: 0, estimatedBytes: 0, pinnedDirty: 0, evictableDirty: 0 };
}

/** Current resident picture. Cheap enough to call from a test assertion or a dev overlay. */
export function getResourceCensus(): ResourceCensus {
  const byKind: Partial<Record<ResourceKind, number>> = {};
  const byFormat: Partial<Record<DocumentFormat, FormatCensus>> = {};
  let heavy = 0;
  let estimatedBytes = 0;
  let pinnedDirty = 0;
  let evictableDirty = 0;

  for (const entry of live.values()) {
    byKind[entry.kind] = (byKind[entry.kind] ?? 0) + 1;
    const isHeavy = RUNTIME_CLASS[entry.kind] === "heavy";
    const bytes = entry.estimatedBytes ?? 0;
    const pinned = Boolean(entry.dirty) && !entry.stateExternalized;
    const evictable = Boolean(entry.dirty) && Boolean(entry.stateExternalized);
    if (isHeavy) heavy++;
    estimatedBytes += bytes;
    if (pinned) pinnedDirty++;
    if (evictable) evictableDirty++;

    const format = entry.format ?? "unknown";
    const prior = byFormat[format] ?? emptyFormat();
    byFormat[format] = {
      live: prior.live + 1,
      heavy: prior.heavy + (isHeavy ? 1 : 0),
      estimatedBytes: prior.estimatedBytes + bytes,
      pinnedDirty: prior.pinnedDirty + (pinned ? 1 : 0),
      evictableDirty: prior.evictableDirty + (evictable ? 1 : 0),
    };
  }

  return {
    live: live.size,
    heavy,
    estimatedBytes,
    pinnedDirty,
    evictableDirty,
    byKind,
    byFormat,
    totals: { acquired, released },
  };
}

/** Live heavyweight runtimes owned by one document. The unit an eviction decision needs. */
export function heavyResourcesForDocument(documentPath: string): number {
  let count = 0;
  for (const entry of live.values())
    if (entry.documentPath === documentPath && RUNTIME_CLASS[entry.kind] === "heavy") count++;
  return count;
}

/**
 * Whether evicting this document would lose unsaved state. The mount policy must consult
 * this before cold-unmounting a dirty pane — the sequence the resource contract requires is
 * externalize → prove restore → evict, never lower a limit and hope.
 */
export function documentHasPinnedState(documentPath: string): boolean {
  for (const entry of live.values())
    if (entry.documentPath === documentPath && entry.dirty && !entry.stateExternalized) return true;
  return false;
}

export function runtimeClassFor(kind: ResourceKind): RuntimeClass {
  return RUNTIME_CLASS[kind];
}

/**
 * Formats whose viewer/editor always allocates at least one heavy resource kind while mounted
 * (html-iframe, mdx-iframe, web-iframe, docx-editor, pdf-document, image-viewer — see
 * `RUNTIME_CLASS`). This is a static
 * fact about the format, independent of whether any instance currently has a live census entry.
 *
 * The mount policy needs this because `heavyResourcesForDocument` answers "what is resident right
 * now" — true zero once a shelled pane's resources are released. A shelled heavyweight document
 * must not look cheap merely because nothing is currently allocated for it; that would let it get
 * silently re-admitted into the live window without the user activating it.
 */
const INTRINSICALLY_HEAVY_FORMATS: ReadonlySet<DocumentFormat> = new Set([
  "html",
  "mdx",
  "web",
  "docx",
  "pdf",
  "image",
]);

export function formatIsIntrinsicallyHeavy(format: DocumentFormat | undefined): boolean {
  return format !== undefined && INTRINSICALLY_HEAVY_FORMATS.has(format);
}

export function resetResourceCensusForTests(): void {
  live.clear();
  nextId = 1;
  acquired = 0;
  released = 0;
}
