/**
 * {@link MarkdownDocumentCore}: the single authority for a
 * Markdown document's logical source, source-map, and history.
 *
 * Surfaces subscribe via {@link MarkdownDocumentCore.subscribe}.
 * Surfaces apply edits via {@link MarkdownDocumentCore.applyCommand}.
 * The core never reads from surfaces; it only writes back through
 * the subscription callback.
 *
 * Threading model: all operations are synchronous and run on the
 * caller's thread. Subscribers are invoked synchronously from
 * inside `applyCommand`. Surfaces must therefore not block the
 * subscription callback; if a surface needs to schedule deferred
 * work, it must do so from within the callback.
 *
 * Lifecycle: see {@link MarkdownCoreLifecycle}. Construction is
 * lazy; the core is usable as soon as `state.lifecycle === "ready"`.
 */

import { ChangeSet } from "@codemirror/state";
import { MarkdownChangeMap } from "./MarkdownChangeMap.js";
import { MarkdownHistory } from "./MarkdownHistory.js";
import { MarkdownNodeIdFactory } from "./MarkdownNodeIdFactory.js";
import { MarkdownTextBuffer } from "./MarkdownTextBuffer.js";
import { createInitialMarkdownSourceMap } from "./buildMarkdownSourceMap.js";
import { reconcileMarkdownSourceMap } from "./reconcileMarkdownSourceMap.js";
import { diffLogicalMarkdown, willUseLcsBranch } from "./markdownLogicalDiff.js";
import { splitRawMarkdownSource } from "./markdownTextEdits.js";
import { incHistoryEntries, incSourceWrites } from "@rightkit/document-core/documents/perfEventCounters";
import type { MarkdownSourceMap } from "./MarkdownSourceMap.js";
import type { MarkdownNodeId } from "./MarkdownNodeIdFactory.js";
import type {
  MarkdownAppliedCommand,
  MarkdownHistoryGroup,
  MarkdownResolvedTextEdit,
  MarkdownTextEdit,
} from "./types.js";
import type {
  MarkdownCommand,
  MarkdownCommitSummary,
  MarkdownCoreLifecycle,
  MarkdownCoreSnapshot,
  MarkdownCoreSubscriber,
  MarkdownDiagnostic,
  MarkdownDocumentCoreOptions,
  MarkdownSelectionHint,
} from "./MarkdownDocumentTypes.js";

export type { MarkdownCommand, MarkdownCommitSummary };

/**
 * Internal subscription record. Surfaces register one of these per
 * `MarkdownSurfaceId`.
 */
interface Subscription {
  readonly surface: number;
  readonly subscriber: MarkdownCoreSubscriber;
}

/**
 * Pending surface selection hints. The core forwards the most
 * recent hint to subscribers on the next commit.
 */
interface SelectionState {
  readonly surface: "source" | "prosemirror";
  readonly nodeId: MarkdownNodeId | null;
  readonly anchor: number;
  readonly head: number;
  readonly assoc: -1 | 1;
}

/**
 * The core class. One instance per document mount. Constructed via
 * {@link MarkdownDocumentCore.create}.
 */
export class MarkdownDocumentCore {
  private buffer: MarkdownTextBuffer;
  private sourceMap: MarkdownSourceMap;
  private history: MarkdownHistory;
  /** Small source-keyed cache avoids rebuilding identical maps during local probe undo/replay. */
  private readonly sourceMapCache = new Map<string, MarkdownSourceMap>();
  private readonly subscriptions: Subscription[] = [];
  private selectionState: SelectionState | null = null;
  private diagnostics: readonly MarkdownDiagnostic[] = [];
  private lifecycle: MarkdownCoreLifecycle = "constructing";
  private readonly recordAllCommands: boolean;
  private readonly nodeIds: MarkdownNodeIdFactory;
  private readonly documentId: string;
  private revisionNumber: number;
  private publishRawSource: (rawSource: string) => void;

  private constructor(options: MarkdownDocumentCoreOptions) {
    this.documentId = options.documentId;
    this.recordAllCommands = options.recordAllCommands ?? false;
    this.revisionNumber = options.initialRevision ?? 0;
    this.publishRawSource = options.publishRawSource ?? (() => {});
    this.buffer = MarkdownTextBuffer.fromRaw(options.initialLogicalSource);
    this.nodeIds = new MarkdownNodeIdFactory(options.documentId);
    this.history = new MarkdownHistory();
    this.sourceMap = createInitialMarkdownSourceMap(
      this.buffer.logicalSource,
      this.revisionNumber,
      this.nodeIds,
    );
    this.lifecycle = "ready";
  }

  /**
   * Construct a ready core. The core is in `"ready"` lifecycle as
   * soon as this returns; subscribers may subscribe immediately.
   */
  static create(options: MarkdownDocumentCoreOptions): MarkdownDocumentCore {
    return new MarkdownDocumentCore(options);
  }

  setPublisher(publishRawSource: (rawSource: string) => void): void {
    this.publishRawSource = publishRawSource;
  }

  /**
   * Lifecycle accessor. Used by React layers to gate "ready" UI.
   */
  get state(): {
    readonly lifecycle: MarkdownCoreLifecycle;
    readonly revision: number;
    readonly documentId: string;
  } {
    return {
      lifecycle: this.lifecycle,
      revision: this.revisionNumber,
      documentId: this.documentId,
    };
  }

  /**
   * Reads the current snapshot. The snapshot is a deep copy of the
   * current state; mutating it never affects the core. Used by
   * subscribers at *subscribe* time to seed themselves.
   */
  snapshot(): MarkdownCoreSnapshot {
    if (this.lifecycle !== "ready") {
      throw new Error(`MarkdownDocumentCore: cannot snapshot() in lifecycle '${this.lifecycle}'`);
    }
    return {
      revision: this.revisionNumber,
      logicalSource: this.buffer.logicalSource,
      lineEndings: [...this.buffer.lineEndings],
      preferredLineEnding: this.buffer.preferredLineEnding,
      sourceMap: {
        ...this.sourceMap,
        nodes: this.sourceMap.nodes.map((node) => ({
          ...node,
          range: { ...node.range },
          prefixRange: { ...node.prefixRange },
        })),
        topLevelStartIndex: [...this.sourceMap.topLevelStartIndex],
      },
    };
  }

  /**
   * Registers a subscriber. The subscriber is invoked synchronously
   * from inside `applyCommand`. Returns an unsubscribe handle.
   */
  subscribe(surface: number, subscriber: MarkdownCoreSubscriber): () => void {
    if (this.lifecycle !== "ready") {
      throw new Error(`MarkdownDocumentCore: cannot subscribe() in lifecycle '${this.lifecycle}'`);
    }
    const sub: Subscription = { surface, subscriber };
    this.subscriptions.push(sub);
    return () => {
      const idx = this.subscriptions.indexOf(sub);
      if (idx >= 0) this.subscriptions.splice(idx, 1);
    };
  }

  /**
   * Updates the most recent selection hint. Surfaces call this on
   * every selection change; the hint is forwarded on the next
   * commit to allow the other surface to mirror it.
   */
  postSelectionHint(hint: MarkdownSelectionHint): void {
    this.selectionState = {
      surface: hint.surface,
      nodeId: hint.nodeId,
      anchor: hint.anchor,
      head: hint.head,
      assoc: hint.assoc,
    };
  }

  /**
   * Reads the most recent selection hint, or `null` if none has
   * been posted since the core was created. The returned hint is
   * consumed by `applyCommand` (forwarded to subscribers) and
   * then cleared.
   */
  consumeSelectionHint(): MarkdownSelectionHint | null {
    const s = this.selectionState;
    if (!s) return null;
    this.selectionState = null;
    return {
      surface: s.surface,
      nodeId: s.nodeId,
      anchor: s.anchor,
      head: s.head,
      assoc: s.assoc,
    };
  }

  /**
   * Pushes a diagnostic into the core's diagnostic buffer.
   * Diagnostics are not yet consumed by subscribers in this
   * packet; Packet 04 will wire the diagnostics surface. This
   * entry point is defined now so callers can already record
   * diagnostics without crashing.
   */
  recordDiagnostic(diagnostic: MarkdownDiagnostic): void {
    this.diagnostics = [...this.diagnostics, diagnostic];
  }

  /**
   * Returns the current diagnostic buffer. Read-only.
   */
  getDiagnostics(): readonly MarkdownDiagnostic[] {
    return this.diagnostics;
  }

  /**
   * Clears all recorded diagnostics.
   */
  clearDiagnostics(): void {
    this.diagnostics = [];
  }

  /**
   * Applies a single batch of edits. Updates the buffer,
   * re-derives the source-map, records the command into history,
   * and notifies subscribers.
   *
   * Returns the commit summary so callers can inspect what
   * happened without re-deriving.
   */
  applyCommand(command: MarkdownCommand): MarkdownCommitSummary {
    if (this.lifecycle !== "ready") {
      throw new Error(
        `MarkdownDocumentCore: cannot applyCommand() in lifecycle '${this.lifecycle}'`,
      );
    }
    if (
      command.expectedRevision !== undefined &&
      command.expectedRevision !== this.revisionNumber
    ) {
      throw new Error(
        `MarkdownDocumentCore: stale command expected revision ${command.expectedRevision}, current ${this.revisionNumber}`,
      );
    }
    const timestampMs = command.timestampMs ?? Math.round(performance.now());
    const applyResult = this.buffer.apply(command.edits);
    const newLogical = applyResult.buffer.logicalSource;
    if (newLogical === this.buffer.logicalSource) {
      return this.buildNoopSummary();
    }
    const changeMap = new MarkdownChangeMap(applyResult.changeMap.codeMirror);

    const oldMap = this.sourceMap;
    const cachedMap = this.sourceMapCache.get(newLogical);
    const newMap = cachedMap
      ? { ...cachedMap, revision: this.revisionNumber + 1, logicalSource: newLogical }
      : reconcileMarkdownSourceMap({
          oldMap,
          newLogicalSource: newLogical,
          newRevision: this.revisionNumber + 1,
          changeMap,
          ids: this.nodeIds,
        });
    this.rememberSourceMap(oldMap.logicalSource, oldMap);
    this.rememberSourceMap(newLogical, newMap);
    const preservedNodeIds = computePreservedNodeIds(oldMap, newMap);
    const oldLength = this.buffer.logicalSource.length;
    const inverseEdits = applyResult.inverse.map((edit) => ({
      from: edit.from,
      to: edit.to,
      insert: edit.insert,
    }));
    const recorded: MarkdownAppliedCommand = {
      resolved: applyResult.forward,
      inverse: applyResult.inverse,
      changeMap: changeMap.summary,
      origin: command.origin,
      label: command.label,
      coalesceKey: command.coalesceKey ?? "",
      isolateHistory: command.isolateHistory ?? false,
      timestampMs,
      changedCodeUnits: computeChangedCodeUnits(applyResult.forward, applyResult.inverse),
    };
    const selectionHint = this.consumeSelectionHint();
    const shouldRecord = this.recordAllCommands || command.origin !== "external";
    if (shouldRecord) {
      this.history.push(recorded);
      incHistoryEntries(1);
    }
    incSourceWrites(1);
    this.buffer = applyResult.buffer;
    this.sourceMap = newMap;
    this.revisionNumber = newMap.revision;
    const summary: MarkdownCommitSummary = {
      revision: newMap.revision,
      oldToNew: changeMap.summary.changes,
      oldLength,
      newLength: newLogical.length,
      preservedNodeIds,
      historyGroup: this.history._undoStack[this.history._undoStack.length - 1] ?? {
        commands: [recorded],
        changedCodeUnits: recorded.changedCodeUnits,
        startedAtMs: recorded.timestampMs,
        endedAtMs: recorded.timestampMs,
      },
      coalesced: this.history._undoStack[this.history._undoStack.length - 1]?.commands.length > 1,
      inverseEdits,
      forwardEdits: command.edits,
      selectionHint,
    };
    this.notifySubscribers(summary, newMap);
    this.publishCurrentRawSource();
    return summary;
  }

  /**
   * Returns the logical diff that transforms `oldSource` into
   * `newSource`. Delegates to {@link diffLogicalMarkdown}.
   */
  static diff(oldSource: string, newSource: string): readonly MarkdownTextEdit[] {
    return diffLogicalMarkdown(oldSource, newSource);
  }

  /** True when undo is available in the history model. */
  get canUndo(): boolean {
    return this.history.canUndo;
  }

  /** True when redo is available in the history model. */
  get canRedo(): boolean {
    return this.history.canRedo;
  }

  undo(): MarkdownCommitSummary | null {
    const group = this.history.takeUndoGroup();
    if (!group) return null;
    let nextBuffer = this.buffer;
    const applied: (readonly MarkdownResolvedTextEdit[])[] = [];
    for (const command of [...group.commands].reverse()) {
      nextBuffer = nextBuffer.applyResolved(command.inverse);
      applied.push(command.inverse);
    }
    this.history.pushUndoneGroup(group);
    return this.commitHistoryTransition(nextBuffer, group, applied, "undo");
  }

  redo(): MarkdownCommitSummary | null {
    const group = this.history.takeRedoGroup();
    if (!group) return null;
    let nextBuffer = this.buffer;
    const applied: (readonly MarkdownResolvedTextEdit[])[] = [];
    for (const command of group.commands) {
      nextBuffer = nextBuffer.applyResolved(command.resolved);
      applied.push(command.resolved);
    }
    this.history.pushRedoneGroup(group);
    return this.commitHistoryTransition(nextBuffer, group, applied, "redo");
  }

  /**
   * Replace the entire logical source with `newLogicalSource` and
   * reset the history. Used when the upstream `text` prop
   * diverges (e.g. an external save reload). The input is treated as
   * raw source; CRLF sequences are normalized to logical LF and
   * materialized on publish so external CRLF replacements round-trip
   * exactly.
   */
  replaceFromExternal(newLogicalSource: string): MarkdownCommitSummary {
    if (this.lifecycle !== "ready") {
      throw new Error(
        `MarkdownDocumentCore: cannot replaceFromExternal() in lifecycle '${this.lifecycle}'`,
      );
    }
    // No-op fast path: the 90 ms edit-text mirror feeds the core's own
    // published raw source straight back here after every typing pause.
    // `toRaw()` is memoized per buffer and is the exact string the core
    // published, so the common case is a reference-equal O(1) check and
    // nothing below (normalization, reconcile, id intersection) runs.
    if (newLogicalSource === this.buffer.toRaw()) {
      return this.buildExternalNoopSummary();
    }
    const oldLogical = this.buffer.logicalSource;
    // Normalize the incoming raw source through the buffer so CRLF
    // line endings are handled exactly like initial construction.
    const newParts = splitRawMarkdownSource(newLogicalSource);
    // External replacement owns whole raw document, including its BOM. Local edit
    // insertion rules deliberately strip a leading BOM; applying them here loses
    // it when old buffer no longer begins with one.
    const nextBuffer = MarkdownTextBuffer.fromParts(newParts);
    const newLogical = nextBuffer.logicalSource;
    const changeMap = new MarkdownChangeMap(
      ChangeSet.of({ from: 0, to: oldLogical.length, insert: newLogical }, oldLogical.length),
    );
    const oldMap = this.sourceMap;
    const newMap = reconcileMarkdownSourceMap({
      oldMap,
      newLogicalSource: newLogical,
      newRevision: oldMap.revision + 1,
      changeMap,
      ids: this.nodeIds,
    });
    this.buffer = nextBuffer;
    this.sourceMap = newMap;
    this.revisionNumber = newMap.revision;
    this.history.clear();
    this.sourceMapCache.clear();
    const summary = this.buildExternalSummary(oldLogical, newLogical, oldMap, newMap);
    this.notifySubscribers(summary, newMap);
    return summary;
  }

  private commitHistoryTransition(
    nextBuffer: MarkdownTextBuffer,
    group: MarkdownHistoryGroup,
    appliedEdits: readonly (readonly MarkdownResolvedTextEdit[])[],
    origin: "undo" | "redo",
  ): MarkdownCommitSummary {
    const oldLogical = this.buffer.logicalSource;
    const nextLogical = nextBuffer.logicalSource;
    // History transitions already have an exact target buffer from resolved
    // edits. Build one bounded linear span for reconciliation; the LCS differ
    // is intentionally reserved for the public diff helper, never undo/redo.
    // The applied edits bound where the two sources can differ, so the
    // prefix/suffix scan resumes from the known-unchanged boundaries instead
    // of walking the whole document; the resulting span is identical.
    const bounds = unchangedBoundsForAppliedEdits(
      appliedEdits,
      oldLogical.length,
      nextLogical.length,
    );
    const diff = diffMarkdownTransition(oldLogical, nextLogical, bounds.prefix, bounds.suffix);
    // Same change map `this.buffer.apply(diff)` would build, without
    // re-applying the edit to the old buffer.
    const changeMap = new MarkdownChangeMap(
      ChangeSet.of(
        diff.map((edit) => ({ from: edit.from, to: edit.to, insert: edit.insert })),
        oldLogical.length,
      ),
    );
    const oldMap = this.sourceMap;
    // Undo right after an edit, and undo/redo ping-pong, land on a source
    // whose map is already cached; reuse it exactly as `applyCommand` does.
    const cachedMap = this.sourceMapCache.get(nextLogical);
    const newMap = cachedMap
      ? { ...cachedMap, revision: this.revisionNumber + 1, logicalSource: nextLogical }
      : reconcileMarkdownSourceMap({
          oldMap,
          newLogicalSource: nextLogical,
          newRevision: this.revisionNumber + 1,
          changeMap,
          ids: this.nodeIds,
        });
    this.rememberSourceMap(oldLogical, oldMap);
    this.rememberSourceMap(nextLogical, newMap);
    incSourceWrites(1);
    this.buffer = nextBuffer;
    this.sourceMap = newMap;
    this.revisionNumber = newMap.revision;
    const priorSelection = this.consumeSelectionHint();
    const selectionHint = priorSelection
      ? {
          ...priorSelection,
          anchor: changeMap.mapPos(
            Math.max(0, Math.min(oldLogical.length, priorSelection.anchor)),
            priorSelection.anchor === priorSelection.head
              ? priorSelection.assoc
              : priorSelection.anchor < priorSelection.head
                ? -1
                : 1,
          ),
          head: changeMap.mapPos(
            Math.max(0, Math.min(oldLogical.length, priorSelection.head)),
            priorSelection.anchor === priorSelection.head
              ? priorSelection.assoc
              : priorSelection.anchor < priorSelection.head
                ? 1
                : -1,
          ),
        }
      : null;
    const summary: MarkdownCommitSummary = {
      revision: newMap.revision,
      oldToNew: changeMap.summary.changes,
      oldLength: oldLogical.length,
      newLength: nextLogical.length,
      preservedNodeIds: computePreservedNodeIds(oldMap, newMap),
      historyGroup: group,
      coalesced: false,
      inverseEdits: [],
      forwardEdits: diff,
      selectionHint,
    };
    this.notifySubscribers(summary, newMap);
    this.publishCurrentRawSource();
    void origin;
    return summary;
  }

  private buildNoopSummary(): MarkdownCommitSummary {
    return {
      revision: this.revisionNumber,
      oldToNew: [],
      oldLength: this.buffer.logicalSource.length,
      newLength: this.buffer.logicalSource.length,
      preservedNodeIds: this.sourceMap.nodes.map((node) => node.id),
      historyGroup: {
        commands: [],
        changedCodeUnits: 0,
        startedAtMs: 0,
        endedAtMs: 0,
      },
      coalesced: false,
      inverseEdits: [],
      forwardEdits: [],
      selectionHint: this.consumeSelectionHint(),
    };
  }

  private publishCurrentRawSource(): void {
    try {
      this.publishRawSource(this.buffer.toRaw());
    } catch (error) {
      console.warn("MarkdownDocumentCore: publisher threw", error);
    }
  }

  private rememberSourceMap(source: string, map: MarkdownSourceMap): void {
    this.sourceMapCache.delete(source);
    this.sourceMapCache.set(source, map);
    while (this.sourceMapCache.size > 4) {
      const oldest = this.sourceMapCache.keys().next().value;
      if (oldest === undefined) break;
      this.sourceMapCache.delete(oldest);
    }
  }

  /**
   * Summary for a `replaceFromExternal` call whose raw source already
   * matches the buffer: same revision, every node preserved.
   */
  private buildExternalNoopSummary(): MarkdownCommitSummary {
    const logical = this.buffer.logicalSource;
    return this.buildExternalSummary(logical, logical, this.sourceMap, this.sourceMap);
  }

  private buildExternalSummary(
    oldLogical: string,
    newLogical: string,
    oldMap: MarkdownSourceMap,
    newMap: MarkdownSourceMap,
  ): MarkdownCommitSummary {
    // External replacement must preserve PM's structural selection mapping;
    // any pending local hint belongs to prior surface activity.
    this.consumeSelectionHint();
    const preserved =
      oldMap === newMap ? newMap.nodes.map((n) => n.id) : computePreservedNodeIds(oldMap, newMap);
    return {
      revision: newMap.revision,
      oldToNew: [],
      oldLength: oldLogical.length,
      newLength: newLogical.length,
      preservedNodeIds: preserved,
      historyGroup: {
        commands: [],
        changedCodeUnits: newLogical.length - oldLogical.length,
        startedAtMs: 0,
        endedAtMs: 0,
      },
      coalesced: false,
      inverseEdits: [],
      forwardEdits: [],
      selectionHint: null,
    };
  }

  /**
   * Returns whether the LCS branch will be used for the supplied
   * pair. Exposed for callers that want to log a warning before
   * the fallback engages.
   */
  static willDiffUseLcs(oldSource: string, newSource: string): boolean {
    return willUseLcsBranch(oldSource, newSource);
  }

  /**
   * Disposes the core. After dispose, the lifecycle is `"disposed"`
   * and every method throws. Subscriptions are released.
   */
  dispose(): void {
    if (this.lifecycle !== "ready") return;
    this.lifecycle = "disposing";
    this.subscriptions.splice(0);
    this.selectionState = null;
    this.diagnostics = [];
    this.lifecycle = "disposed";
  }

  private notifySubscribers(summary: MarkdownCommitSummary, newMap: MarkdownSourceMap): void {
    for (const sub of this.subscriptions) {
      try {
        sub.subscriber(summary, newMap);
      } catch (err) {
        // Subscribers must not throw. If one does, we log a
        // warning but continue notifying the rest. The core
        // remains in `"ready"` lifecycle; subscribers are
        // individually responsible for their own error
        // boundaries.
        console.warn(`MarkdownDocumentCore: subscriber for surface ${sub.surface} threw:`, err);
      }
    }
  }
}

function computeChangedCodeUnits(
  forward: readonly { from: number; to: number; insert: string }[],
  inverse: readonly { from: number; to: number; insert: string }[],
): number {
  let n = 0;
  for (const edit of forward) {
    n += edit.to - edit.from + countNewlines(edit.insert);
  }
  for (const edit of inverse) {
    n += edit.to - edit.from + countNewlines(edit.insert);
  }
  return n;
}

function countNewlines(text: string): number {
  let count = 0;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) count++;
  }
  return count;
}

/**
 * Returns one ordered edit spanning the changed middle of two sources.
 * Prefix/suffix scans are O(n), avoid quadratic LCS work on large documents,
 * and preserve exact target text for source-map reconciliation.
 *
 * `knownPrefix` / `knownSuffix` are lengths the caller already knows to be a
 * common prefix / common suffix of both sources. The scans resume from those
 * boundaries, so the result is identical to scanning from the ends while the
 * work stays bounded by the changed region.
 */
function diffMarkdownTransition(
  oldSource: string,
  newSource: string,
  knownPrefix = 0,
  knownSuffix = 0,
): readonly MarkdownTextEdit[] {
  const sharedPrefixLength = Math.min(oldSource.length, newSource.length);
  let prefix = Math.min(knownPrefix, sharedPrefixLength);
  while (
    prefix < sharedPrefixLength &&
    oldSource.charCodeAt(prefix) === newSource.charCodeAt(prefix)
  ) {
    prefix++;
  }
  // A full-length common prefix of equal-length sources means no change.
  if (prefix === oldSource.length && prefix === newSource.length) return [];

  // The suffix scan never crosses `prefix`; resuming from the known common
  // suffix (clamped to that limit) reaches exactly the state a scan from the
  // ends would reach.
  const skip = Math.min(knownSuffix, oldSource.length - prefix, newSource.length - prefix);
  let oldEnd = oldSource.length - skip;
  let newEnd = newSource.length - skip;
  while (
    oldEnd > prefix &&
    newEnd > prefix &&
    oldSource.charCodeAt(oldEnd - 1) === newSource.charCodeAt(newEnd - 1)
  ) {
    oldEnd--;
    newEnd--;
  }

  return [
    {
      from: prefix,
      to: oldEnd,
      insert: newSource.slice(prefix, newEnd),
      inheritMode: "preserve",
    },
  ];
}

/**
 * Composes the per-command edit lists a history transition applied (each
 * expressed against the document the previous list produced) and returns how
 * much of the two sources is provably untouched: the common prefix before the
 * first change and the common suffix after the last one. Returns `{0, 0}` (a
 * full scan) whenever the composed lengths do not describe the two sources,
 * so the bounds are only used when they are exact.
 */
function unchangedBoundsForAppliedEdits(
  appliedEdits: readonly (readonly MarkdownResolvedTextEdit[])[],
  oldLength: number,
  newLength: number,
): { prefix: number; suffix: number } {
  const unknown = { prefix: 0, suffix: 0 };
  try {
    let composed: ChangeSet | null = null;
    for (const edits of appliedEdits) {
      if (edits.length === 0) continue;
      const step = ChangeSet.of(
        edits.map((edit) => ({ from: edit.from, to: edit.to, insert: edit.insert })),
        composed ? composed.newLength : oldLength,
      );
      composed = composed ? composed.compose(step) : step;
    }
    if (!composed || composed.length !== oldLength || composed.newLength !== newLength) {
      return unknown;
    }
    let firstFromA = -1;
    let lastToA = oldLength;
    composed.iterChangedRanges((fromA, toA) => {
      if (firstFromA < 0) firstFromA = fromA;
      lastToA = toA;
    });
    if (firstFromA < 0) return unknown;
    return { prefix: firstFromA, suffix: oldLength - lastToA };
  } catch {
    return unknown;
  }
}

function computePreservedNodeIds(
  oldMap: MarkdownSourceMap,
  newMap: MarkdownSourceMap,
): readonly MarkdownNodeId[] {
  const oldIds = new Set(oldMap.nodes.map((n) => n.id));
  const out: MarkdownNodeId[] = [];
  for (const node of newMap.nodes) {
    if (oldIds.has(node.id)) out.push(node.id);
  }
  return out;
}
