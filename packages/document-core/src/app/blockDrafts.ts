export type BlockDraftStatus = "clean" | "modified" | "committing" | "stale";

export type BlockDraftIdentity = Readonly<{
  documentPath: string;
  pagePath: string;
}>;

export type BlockDraftSourceRange = Readonly<{
  start: number;
  end: number;
}>;

export type BlockDraftSelectionBookmark = unknown;

export type BlockDraftSnapshot = Readonly<{
  identity: BlockDraftIdentity;
  blockKind: string;
  originalRevision: string | number;
  sourceRange?: BlockDraftSourceRange;
  sourceChecksum?: string;
  originalText: string;
  currentText: string;
  /** Structural rename requested through the editor (e.g. `html:p` -> `html:h2`). */
  targetTag?: string;
  selectionBookmark?: BlockDraftSelectionBookmark;
  status: BlockDraftStatus;
  reason?: string;
  error?: string;
}>;

export type BlockDraftCommitResult =
  | { ok: true }
  | { ok: false; kind: "error" | "stale"; reason: string };

export type BlockDraftCommit = (
  snapshot: BlockDraftSnapshot,
) => BlockDraftCommitResult | PromiseLike<BlockDraftCommitResult>;

export type BlockDraftDiscard = (snapshot: BlockDraftSnapshot) => void;

export type BlockDraftController = Readonly<{
  commit: BlockDraftCommit;
  discard: BlockDraftDiscard;
}>;

export type BlockDraftBeginInput = Readonly<{
  identity: BlockDraftIdentity;
  blockKind: string;
  originalRevision: string | number;
  sourceRange?: BlockDraftSourceRange;
  sourceChecksum?: string;
  originalText: string;
  targetTag?: string;
  selectionBookmark?: BlockDraftSelectionBookmark;
  controller?: BlockDraftController;
  commit?: BlockDraftCommit;
  discard?: BlockDraftDiscard;
}>;

export type BlockDraftBeginResult =
  | { ok: true; status: "started"; snapshot: BlockDraftSnapshot }
  | { ok: false; status: "conflict"; snapshot: BlockDraftSnapshot };

export type BlockDraftIntent =
  | "manual-save"
  | "view-change"
  | "close"
  | "close-all"
  | "quit"
  | "setup-replacement"
  | "external-change"
  /** Idle autosave. Never commits or discards a block draft: Done stays the only commit point. */
  | "autosave";

export type BlockDraftDecision = "done" | "discard" | "stay";

export type BlockDraftResolveResult =
  | { outcome: "done"; snapshot?: undefined }
  | { outcome: "discarded"; snapshot?: undefined }
  | { outcome: "cancelled"; snapshot: BlockDraftSnapshot }
  | { outcome: "failed"; snapshot: BlockDraftSnapshot; reason: string }
  | { outcome: "missing"; snapshot?: undefined };

export type BlockDraftUpdateInput = Readonly<{
  identity: BlockDraftIdentity;
  currentText: string;
  targetTag?: string;
  selectionBookmark?: BlockDraftSelectionBookmark;
}>;

export interface BlockDraftRegistry {
  begin(input: BlockDraftBeginInput): BlockDraftBeginResult;
  update(
    identity: BlockDraftIdentity,
    currentText: string,
    selectionBookmark?: BlockDraftSelectionBookmark,
  ): BlockDraftSnapshot | null;
  update(input: BlockDraftUpdateInput): BlockDraftSnapshot | null;
  markStale(identity: BlockDraftIdentity, reason?: string): BlockDraftSnapshot | null;
  get(identity: BlockDraftIdentity): BlockDraftSnapshot | null;
  /** True for every open draft, including a clean draft whose editor must remain mounted. */
  hasDraft(documentPath: string): boolean;
  hasModifiedDraft(identity: BlockDraftIdentity): boolean;
  snapshotModified(documentPath?: string): BlockDraftSnapshot[];
  snapshotAll(documentPath?: string): BlockDraftSnapshot[];
  resolve(
    identity: BlockDraftIdentity,
    intent: BlockDraftIntent,
    decision: BlockDraftDecision,
  ): Promise<BlockDraftResolveResult>;
  discard(identity: BlockDraftIdentity): boolean;
  subscribe(listener: () => void): () => void;
  /** Fires only when a document gains or loses a draft; typing stays local to its editor. */
  subscribePresence(listener: () => void): () => void;
}

type MutableDraft = {
  identity: BlockDraftIdentity;
  blockKind: string;
  originalRevision: string | number;
  sourceRange?: BlockDraftSourceRange;
  sourceChecksum?: string;
  originalText: string;
  currentText: string;
  targetTag?: string;
  selectionBookmark?: BlockDraftSelectionBookmark;
  status: BlockDraftStatus;
  reason?: string;
  error?: string;
  commit: BlockDraftCommit;
  discard: BlockDraftDiscard;
};

const missingController = (operation: string): never => {
  throw new Error(`Block draft ${operation} callback is required.`);
};

function clone<T>(value: T): T {
  if (value === null || typeof value !== "object") return value;
  if (Array.isArray(value)) return value.map((item) => clone(item)) as T;
  const output: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value as Record<string, unknown>))
    output[key] = clone(item);
  return output as T;
}

function freezeDeep<T>(value: T): T {
  if (value === null || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(value as Record<string, unknown>)) freezeDeep(child);
  return Object.freeze(value);
}

function immutable<T>(value: T): T {
  return freezeDeep(clone(value));
}

function equal(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (!a || !b || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((value, index) => equal(value, b[index]));
  }
  const aKeys = Object.keys(a as object);
  const bKeys = Object.keys(b as object);
  const bKeySet = new Set(bKeys);
  return (
    aKeys.length === bKeys.length &&
    aKeys.every(
      (key) =>
        bKeySet.has(key) &&
        equal((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key]),
    )
  );
}

function key(identity: BlockDraftIdentity): string {
  return `${identity.documentPath}\u0000${identity.pagePath}`;
}

function snapshotOf(draft: MutableDraft): BlockDraftSnapshot {
  return immutable({
    identity: draft.identity,
    blockKind: draft.blockKind,
    originalRevision: draft.originalRevision,
    sourceRange: draft.sourceRange,
    sourceChecksum: draft.sourceChecksum,
    originalText: draft.originalText,
    currentText: draft.currentText,
    targetTag: draft.targetTag,
    selectionBookmark: draft.selectionBookmark,
    status: draft.status,
    reason: draft.reason,
    error: draft.error,
  });
}

function errorReason(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function normalizeCommitResult(value: BlockDraftCommitResult | undefined): BlockDraftCommitResult {
  if (value === undefined) return { ok: true };
  if (value.ok === true) return value;
  if (value.ok === false && (value.kind === "error" || value.kind === "stale") && value.reason)
    return value;
  return { ok: false, kind: "error", reason: "Block draft commit failed." };
}

export function createBlockDraftRegistry(
  defaultController?: BlockDraftController,
): BlockDraftRegistry {
  const drafts = new Map<string, MutableDraft>();
  const inFlight = new Map<string, Promise<BlockDraftResolveResult>>();
  const listeners = new Set<() => void>();
  const presenceListeners = new Set<() => void>();

  const notify = () => {
    for (const listener of listeners) listener();
  };
  const notifyPresence = () => {
    for (const listener of presenceListeners) listener();
  };

  const getDraft = (identity: BlockDraftIdentity) => drafts.get(key(identity));

  const discard = (identity: BlockDraftIdentity): boolean => {
    const draft = getDraft(identity);
    if (!draft || draft.status === "committing") return false;
    const snapshot = snapshotOf(draft);
    drafts.delete(key(identity));
    inFlight.delete(key(identity));
    try {
      draft.discard(snapshot);
    } finally {
      notify();
      notifyPresence();
    }
    return true;
  };

  const resolve = (
    identity: BlockDraftIdentity,
    intent: BlockDraftIntent,
    decision: BlockDraftDecision,
  ): Promise<BlockDraftResolveResult> => {
    const draftKey = key(identity);
    const existing = inFlight.get(draftKey);
    if (existing && decision === "done") return existing;

    const draft = drafts.get(draftKey);
    if (!draft) return Promise.resolve({ outcome: "missing" });
    if (intent === "autosave") {
      // Autosave may run mid-edit: report the draft as untouched rather than commit or discard it.
      return Promise.resolve({ outcome: "cancelled", snapshot: snapshotOf(draft) });
    }
    if (decision === "stay")
      return Promise.resolve({ outcome: "cancelled", snapshot: snapshotOf(draft) });
    if (decision === "discard") {
      if (draft.status === "committing") {
        return Promise.resolve({
          outcome: "failed",
          snapshot: snapshotOf(draft),
          reason: "Block draft is already committing.",
        });
      }
      discard(identity);
      return Promise.resolve({ outcome: "discarded" });
    }
    if (draft.status === "committing") {
      return (
        existing ??
        Promise.resolve({
          outcome: "failed",
          snapshot: snapshotOf(draft),
          reason: "Block draft is already committing.",
        })
      );
    }

    const wasStale = draft.status === "stale";
    draft.status = "committing";
    draft.reason = undefined;
    draft.error = undefined;
    notify();
    const commit = Promise.resolve()
      .then(() => draft.commit(snapshotOf(draft)))
      .then(normalizeCommitResult)
      .then((result): BlockDraftResolveResult => {
        if (!drafts.has(draftKey)) {
          return result.ok
            ? { outcome: "done" }
            : { outcome: "failed", snapshot: snapshotOf(draft), reason: result.reason };
        }
        if (result.ok) {
          drafts.delete(draftKey);
          notify();
          notifyPresence();
          return { outcome: "done" };
        }
        draft.status = result.kind === "stale" || wasStale ? "stale" : "modified";
        draft.reason = result.reason;
        draft.error = result.kind === "error" ? result.reason : undefined;
        notify();
        return { outcome: "failed", snapshot: snapshotOf(draft), reason: result.reason };
      })
      .catch((error): BlockDraftResolveResult => {
        const reason = errorReason(error);
        if (!drafts.has(draftKey))
          return { outcome: "failed", snapshot: snapshotOf(draft), reason };
        draft.status = wasStale ? "stale" : "modified";
        draft.reason = reason;
        draft.error = reason;
        notify();
        return { outcome: "failed", snapshot: snapshotOf(draft), reason };
      })
      .finally(() => {
        inFlight.delete(draftKey);
      });
    inFlight.set(draftKey, commit);
    return commit;
  };

  return {
    begin(input) {
      const draftKey = key(input.identity);
      const controller = input.controller;
      const commit =
        input.commit ??
        controller?.commit ??
        defaultController?.commit ??
        (() => missingController("commit"));
      const discardCallback =
        input.discard ??
        controller?.discard ??
        defaultController?.discard ??
        (() => missingController("discard"));
      const existing = drafts.get(draftKey);
      if (existing) {
        // A keep-alive tab may remount its block editor after a tab switch. Rebind the live
        // controller so a later Done/Discard uses current editor lifetime & core references.
        existing.commit = commit;
        existing.discard = discardCallback;
        return { ok: false, status: "conflict", snapshot: snapshotOf(existing) };
      }
      const draft: MutableDraft = {
        identity: immutable(input.identity),
        blockKind: input.blockKind,
        originalRevision: input.originalRevision,
        sourceRange: input.sourceRange ? immutable(input.sourceRange) : undefined,
        sourceChecksum: input.sourceChecksum,
        originalText: input.originalText,
        currentText: input.originalText,
        targetTag: input.targetTag,
        selectionBookmark:
          input.selectionBookmark === undefined ? undefined : clone(input.selectionBookmark),
        status: "clean",
        commit,
        discard: discardCallback,
      };
      drafts.set(draftKey, draft);
      notify();
      notifyPresence();
      return { ok: true, status: "started", snapshot: snapshotOf(draft) };
    },
    update(
      identityOrInput: BlockDraftIdentity | BlockDraftUpdateInput,
      currentText?: string,
      selectionBookmark?: BlockDraftSelectionBookmark,
    ) {
      const input =
        "currentText" in identityOrInput
          ? identityOrInput
          : { identity: identityOrInput, currentText: currentText as string, selectionBookmark };
      const draft = getDraft(input.identity);
      if (!draft || draft.status === "committing") return draft ? snapshotOf(draft) : null;
      const staysStale = draft.status === "stale";
      const nextStatus: BlockDraftStatus = staysStale
        ? "stale"
        : input.currentText === draft.originalText
          ? "clean"
          : "modified";
      const changed =
        draft.currentText !== input.currentText ||
        draft.status !== nextStatus ||
        draft.targetTag !== input.targetTag ||
        !equal(draft.selectionBookmark, input.selectionBookmark);
      if (!changed) return snapshotOf(draft);
      draft.currentText = input.currentText;
      draft.targetTag = input.targetTag;
      draft.selectionBookmark =
        input.selectionBookmark === undefined ? undefined : clone(input.selectionBookmark);
      draft.status = nextStatus;
      if (!staysStale) {
        draft.reason = undefined;
        draft.error = undefined;
      }
      notify();
      return snapshotOf(draft);
    },
    markStale(identity, reason) {
      const draft = getDraft(identity);
      if (!draft) return null;
      const changed =
        draft.status !== "stale" || draft.reason !== reason || draft.error !== undefined;
      if (!changed) return snapshotOf(draft);
      draft.status = "stale";
      draft.reason = reason;
      draft.error = undefined;
      notify();
      return snapshotOf(draft);
    },
    get(identity) {
      const draft = getDraft(identity);
      return draft ? snapshotOf(draft) : null;
    },
    hasDraft(documentPath) {
      for (const draft of drafts.values())
        if (draft.identity.documentPath === documentPath) return true;
      return false;
    },
    hasModifiedDraft(identity) {
      const draft = getDraft(identity);
      return !!draft && draft.status !== "clean" && draft.currentText !== draft.originalText;
    },
    snapshotModified(documentPath) {
      return [...drafts.values()]
        .filter(
          (draft) =>
            (draft.status === "modified" ||
              draft.status === "stale" ||
              draft.status === "committing") &&
            draft.currentText !== draft.originalText &&
            (documentPath === undefined || draft.identity.documentPath === documentPath),
        )
        .sort(
          (a, b) =>
            key(a.identity).localeCompare(key(b.identity)) ||
            a.blockKind.localeCompare(b.blockKind),
        )
        .map(snapshotOf);
    },
    snapshotAll(documentPath) {
      return [...drafts.values()]
        .filter(
          (draft) => documentPath === undefined || draft.identity.documentPath === documentPath,
        )
        .sort(
          (a, b) =>
            key(a.identity).localeCompare(key(b.identity)) ||
            a.blockKind.localeCompare(b.blockKind),
        )
        .map(snapshotOf);
    },
    resolve,
    discard,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    subscribePresence(listener) {
      presenceListeners.add(listener);
      return () => presenceListeners.delete(listener);
    },
  };
}
