/**
 * Markdown text buffer.
 *
 * The buffer is the single authority over the document's logical
 * source and its line-ending vector. It owns:
 *
 *  - construction from raw source via {@link splitRawMarkdownSource}
 *  - validation of every edit before applying
 *  - resolution of inserted line endings under `preserve`, `document`,
 *    or `inherit` policies
 *  - BOM preservation semantics
 *  - exact inverse-edit computation, including newline-vector tokens
 *  - a {@link MarkdownChangeMap} summarizing the position shift
 *
 * The constructor is private; use {@link MarkdownTextBuffer.fromRaw}.
 * Apply always returns a new buffer plus the forward/inverse edit
 * lists and a change-map. The buffer never mutates `this`.
 */

import { ChangeSet } from "@codemirror/state";
import { MarkdownChangeMap } from "./MarkdownChangeMap.js";
import {
  countLogicalNewlines,
  logicalNewlineOffsets,
  materializeMarkdownSource,
  resolveInsertionEndings,
  splitInsertion,
  splitRawMarkdownSource,
  validateMarkdownEdits,
} from "./markdownTextEdits.js";
import type {
  MarkdownAppliedCommand,
  MarkdownLineEnding,
  MarkdownResolvedTextEdit,
  MarkdownTextEdit,
  MarkdownTextParts,
} from "./types.js";

/**
 * Apply result. `forward` is the resolved form of the input edits;
 * `inverse` is the inverse list, expressed against the new document,
 * that exactly undoes the change when applied as a resolved set.
 */
export interface MarkdownApplyResult {
  readonly buffer: MarkdownTextBuffer;
  readonly forward: readonly MarkdownResolvedTextEdit[];
  readonly inverse: readonly MarkdownResolvedTextEdit[];
  readonly changeMap: MarkdownChangeMap;
}

const BOM = "\uFEFF";

export class MarkdownTextBuffer {
  readonly logicalSource: string;
  readonly lineEndings: readonly MarkdownLineEnding[];
  readonly preferredLineEnding: MarkdownLineEnding;
  readonly hasBom: boolean;
  /** Memoized {@link toRaw} result; buffers are immutable, so it never goes stale. */
  private rawSource: string | undefined;

  private constructor(parts: MarkdownTextParts) {
    this.logicalSource = parts.logicalSource;
    this.lineEndings = parts.lineEndings;
    this.preferredLineEnding = parts.preferredLineEnding;
    this.hasBom = parts.hasBom;
  }

  /**
   * Constructs a buffer from raw source, splitting it into the
   * logical source and an explicit line-ending vector.
   */
  static fromRaw(rawSource: string): MarkdownTextBuffer {
    return new MarkdownTextBuffer(splitRawMarkdownSource(rawSource));
  }

  /**
   * Constructs a buffer from already-decomposed parts. Useful for
   * tests and for the apply pipeline.
   */
  static fromParts(parts: MarkdownTextParts): MarkdownTextBuffer {
    return new MarkdownTextBuffer(parts);
  }

  /**
   * Returns the raw source string with line endings re-materialized
   * from the line-ending vector. Always round-trips from `fromRaw`.
   */
  toRaw(): string {
    if (this.rawSource !== undefined) return this.rawSource;
    // All-LF documents materialize to the logical source itself; skip the
    // per-newline regex replace. The newline-count check keeps the
    // mismatch assertion of `materializeMarkdownSource`.
    const raw =
      this.lineEndings.every((ending) => ending === "\n") &&
      countLogicalNewlines(this.logicalSource) === this.lineEndings.length
        ? this.logicalSource
        : materializeMarkdownSource(this.logicalSource, this.lineEndings);
    this.rawSource = raw;
    return raw;
  }

  /**
   * Applies a batch of edits. Validates, sorts, and resolves them;
   * returns a new buffer plus forward/inverse resolved edits and a
   * change-map. The buffer itself is never mutated.
   */
  apply(edits: readonly MarkdownTextEdit[]): MarkdownApplyResult {
    const sorted = validateMarkdownEdits(this.logicalSource, edits);
    const offsets = logicalNewlineOffsets(this.logicalSource);
    const resolved: MarkdownResolvedTextEdit[] = [];
    const forwardRaw: MarkdownResolvedTextEdit[] = [];
    void resolved;

    // We build the new logical source and new line-ending vector
    // incrementally. Walking in source order avoids repeated full
    // string splicing.
    let cursor = 0;
    let nextNewLogical = "";
    let nextNewEndings: MarkdownLineEnding[] = [];

    for (const edit of sorted) {
      // Append unchanged prefix.
      if (edit.from > cursor) {
        const prefix = appendUnchangedSegment(
          this.logicalSource,
          this.lineEndings,
          offsets,
          cursor,
          edit.from,
          nextNewLogical,
          nextNewEndings,
        );
        nextNewLogical = prefix.logical;
        nextNewEndings = prefix.endings;
      }

      // Removed endings vector for inherit mode.
      const removedEndings = sliceEndingsForRange(this.lineEndings, offsets, edit.from, edit.to);

      // Parse the insertion.
      const inserted = splitInsertion(edit.insert);
      // Strip a leading BOM from the insertion unless we're
      // inserting at offset 0 and the document has no BOM.
      let insertLogical = inserted.logical;
      if (
        insertLogical.startsWith(BOM) &&
        !(edit.from === 0 && edit.to === 0 && !this.logicalSource.startsWith(BOM))
      ) {
        insertLogical = insertLogical.slice(1);
      }
      const insertEndings = resolveInsertionEndings(
        inserted.endings,
        removedEndings,
        this.preferredLineEnding,
        edit.inheritMode,
      );

      const retainBom = edit.from === 0 && this.logicalSource.startsWith(BOM);
      const forwardEdit: MarkdownResolvedTextEdit = {
        from: edit.from,
        to: edit.to,
        insert: edit.insert,
        insertLineEndings: insertEndings,
        retainsLeadingBom: retainBom,
      };
      forwardRaw.push(forwardEdit);

      // Append inserted logical source and resolved endings.
      const appendedInsertion = appendInsertion(
        nextNewLogical,
        insertLogical,
        nextNewEndings,
        insertEndings,
      );
      nextNewLogical = appendedInsertion.logical;
      nextNewEndings = appendedInsertion.endings;

      cursor = edit.to;
    }

    // Append the trailing unchanged segment.
    const tail = appendUnchangedSegment(
      this.logicalSource,
      this.lineEndings,
      offsets,
      cursor,
      this.logicalSource.length,
      nextNewLogical,
      nextNewEndings,
    );
    nextNewLogical = tail.logical;
    nextNewEndings = tail.endings;

    const newParts: MarkdownTextParts = {
      logicalSource: nextNewLogical,
      lineEndings: nextNewEndings,
      preferredLineEnding: this.preferredLineEnding,
      hasBom: nextNewLogical.startsWith(BOM),
    };

    // Build the inverse edit list expressed against the new
    // document. We iterate the forward edits in *reverse* order so
    // earlier inverses land before later ones, then translate each
    // inverse to post-command coordinates.
    const inversePost = computeInverseEdits(
      sorted,
      forwardRaw,
      this.logicalSource,
      this.lineEndings,
      offsets,
    );

    // Build the change-map from forward edits via CodeMirror ChangeSet.
    const cs = ChangeSet.of(
      forwardRaw.map((edit) => ({
        from: edit.from,
        to: edit.to,
        insert: edit.insert,
      })),
      this.logicalSource.length,
    );
    const changeMap = new MarkdownChangeMap(cs);

    const newBuffer = MarkdownTextBuffer.fromParts(newParts);
    return {
      buffer: newBuffer,
      forward: forwardRaw,
      inverse: inversePost,
      changeMap,
    };
  }

  /**
   * Applies an already-resolved set of edits and returns the change
   * map for the transformation. Used by history undo/redo to preserve
   * node identities without re-diffing.
   */
  applyResolvedWithChangeMap(edits: readonly MarkdownResolvedTextEdit[]): MarkdownApplyResult {
    const cs = ChangeSet.of(
      edits.map((edit) => ({ from: edit.from, to: edit.to, insert: edit.insert })),
      this.logicalSource.length,
    );
    const changeMap = new MarkdownChangeMap(cs);
    // Reuse applyResolved for the buffer itself to keep line-ending
    // vector handling single-sourced.
    const buffer = this.applyResolved(edits);
    // Derive inverse in the same way `apply()` does, but expressed
    // against the *new* buffer so callers don't need to recompute.
    // For replay we only need forward; inverse is empty but kept for
    // shape compatibility.
    const inverse: readonly MarkdownResolvedTextEdit[] = [];
    return { buffer, forward: edits, inverse, changeMap };
  }

  /**
   * Applies an already-resolved set of edits. Skips validation and
   * inheritance resolution; used by history undo/redo.
   */
  applyResolved(edits: readonly MarkdownResolvedTextEdit[]): MarkdownTextBuffer {
    const logical = this.logicalSource;
    const endings = [...this.lineEndings];
    const offsets = logicalNewlineOffsets(logical);
    let cursor = 0;
    let nextLogical = "";
    let nextEndings: MarkdownLineEnding[] = [];

    for (const edit of edits) {
      if (edit.from > cursor) {
        const tail = appendUnchangedSegment(
          logical,
          endings,
          offsets,
          cursor,
          edit.from,
          nextLogical,
          nextEndings,
        );
        nextLogical = tail.logical;
        nextEndings = tail.endings;
      }
      const insertedTail = appendInsertion(
        nextLogical,
        edit.insert,
        nextEndings,
        edit.insertLineEndings,
      );
      nextLogical = insertedTail.logical;
      nextEndings = insertedTail.endings;
      cursor = edit.to;
    }
    const finalTail = appendUnchangedSegment(
      logical,
      endings,
      offsets,
      cursor,
      logical.length,
      nextLogical,
      nextEndings,
    );
    nextLogical = finalTail.logical;
    nextEndings = finalTail.endings;
    void logical;
    void endings;
    return MarkdownTextBuffer.fromParts({
      logicalSource: nextLogical,
      lineEndings: nextEndings,
      preferredLineEnding: this.preferredLineEnding,
      hasBom: nextLogical.startsWith(BOM),
    });
  }

  /**
   * Replays a history group's resolved edits sequentially, composing
   * their ChangeSets so the resulting changeMap maps the original
   * buffer's positions to the final buffer's positions. Returns the
   * final buffer and the composed change map without ever invoking
   * the LCS differ.
   */
  replayResolvedGroups(groups: readonly (readonly MarkdownResolvedTextEdit[])[]): {
    buffer: MarkdownTextBuffer;
    changeMap: MarkdownChangeMap;
  } {
    let cursor: MarkdownTextBuffer = this;
    let composed: import("@codemirror/state").ChangeSet | null = null;
    for (const edits of groups) {
      if (edits.length === 0) continue;
      const step = cursor.applyResolvedWithChangeMap(edits);
      const cs = (step.changeMap as MarkdownChangeMap).codeMirror;
      composed = composed ? (composed as import("@codemirror/state").ChangeSet).compose(cs) : cs;
      cursor = step.buffer;
    }
    if (!composed) {
      const cs = ChangeSet.empty(this.logicalSource.length);
      return { buffer: cursor, changeMap: new MarkdownChangeMap(cs) };
    }
    return { buffer: cursor, changeMap: new MarkdownChangeMap(composed) };
  }

  /**
   * Produces a minimal {@link MarkdownAppliedCommand} describing a
   * hypothetical apply. Useful for the history model.
   */
  toAppliedCommand(
    forward: readonly MarkdownResolvedTextEdit[],
    inverse: readonly MarkdownResolvedTextEdit[],
    changeMap: MarkdownChangeMap,
  ): MarkdownAppliedCommand {
    const changed =
      forward.reduce((acc, e) => acc + (e.to - e.from) + countLogicalNewlines(e.insert), 0) +
      inverse.reduce((acc, e) => acc + (e.to - e.from) + countLogicalNewlines(e.insert), 0);
    return {
      resolved: forward,
      inverse,
      changeMap: changeMap.summary,
      origin: "source",
      label: "Unknown",
      coalesceKey: "",
      isolateHistory: false,
      timestampMs: 0,
      changedCodeUnits: changed,
    };
  }
}

/**
 * Appends the unchanged logical segment `[from, to)` (and its
 * endings) onto the accumulator pair.
 */
function appendUnchangedSegment(
  logical: string,
  endings: readonly MarkdownLineEnding[],
  offsets: readonly number[],
  cursor: number,
  to: number,
  accLogical: string,
  accEndings: MarkdownLineEnding[],
): { logical: string; endings: MarkdownLineEnding[] } {
  let outLogical = accLogical;
  const outEndings = accEndings;
  if (to <= cursor) return { logical: outLogical, endings: outEndings };
  outLogical += logical.slice(cursor, to);
  // For each logical newline in (cursor, to), copy the corresponding
  // ending token.
  const startIdx = lowerBound(offsets, cursor);
  const endIdx = lowerBound(offsets, to);
  for (let i = startIdx; i < endIdx; i++) {
    outEndings.push(endings[i] ?? "\n");
  }
  return { logical: outLogical, endings: outEndings };
}

/**
 * Appends an inserted logical payload and its resolved endings onto
 * the accumulator.
 */
function appendInsertion(
  accLogical: string,
  insertLogical: string,
  accEndings: MarkdownLineEnding[],
  insertEndings: readonly MarkdownLineEnding[],
): { logical: string; endings: MarkdownLineEnding[] } {
  const outLogical = accLogical + insertLogical;
  const outEndings = accEndings;
  let endingIdx = 0;
  for (let i = 0; i < insertLogical.length; i++) {
    if (insertLogical.charCodeAt(i) === 10) {
      outEndings.push(insertEndings[endingIdx] ?? "\n");
      endingIdx++;
    }
  }
  // If the insertion is empty but has endings (impossible by the
  // invariant but defensive), push nothing.
  return { logical: outLogical, endings: outEndings };
}

/**
 * Returns the line-ending vector entries that covered the half-open
 * logical range `[from, to)`.
 */
function sliceEndingsForRange(
  endings: readonly MarkdownLineEnding[],
  offsets: readonly number[],
  from: number,
  to: number,
): MarkdownLineEnding[] {
  const startIdx = lowerBound(offsets, from);
  const endIdx = lowerBound(offsets, to);
  return endings.slice(startIdx, endIdx);
}

/**
 * Lower-bound binary search. Returns the smallest index `i` such that
 * `arr[i] >= target`, or `arr.length` if none.
 */
function lowerBound(arr: readonly number[], target: number): number {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (arr[mid] < target) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/**
 * Computes the inverse edit list for a batch of forward edits,
 * expressed in *post-command* coordinates. The inverses, applied as a
 * single resolved set, must exactly restore the original logical
 * source and line-ending vector.
 *
 * Strategy: compute the new-document position of each forward edit's
 * insertion by walking the forward edits in order and accumulating
 * the per-edit delta (insert length minus removed length). Each
 * inverse replaces the forward edit's inserted span with the
 * originally removed text plus the originally removed line endings.
 */
function computeInverseEdits(
  forward: readonly MarkdownTextEdit[],
  resolved: readonly MarkdownResolvedTextEdit[],
  originalLogical: string,
  originalLineEndings?: readonly MarkdownLineEnding[],
  originalOffsets?: readonly number[],
): MarkdownResolvedTextEdit[] {
  const newPositions: number[] = [];
  let runningDelta = 0;
  for (let i = 0; i < forward.length; i++) {
    const edit = forward[i];
    newPositions.push(edit.from + runningDelta);
    runningDelta += edit.insert.length - (edit.to - edit.from);
  }
  const inverses: MarkdownResolvedTextEdit[] = [];
  for (let i = 0; i < forward.length; i++) {
    const edit = forward[i];
    const newFrom = newPositions[i];
    const newTo = newFrom + edit.insert.length;
    const removedText = originalLogical.slice(edit.from, edit.to);
    const removedEndings =
      originalLineEndings && originalOffsets
        ? sliceEndingsForRange(originalLineEndings, originalOffsets, edit.from, edit.to)
        : sliceEndingsForRangeSimple(originalLogical, edit.from, edit.to);
    inverses.push({
      from: newFrom,
      to: newTo,
      insert: removedText,
      insertLineEndings: removedEndings,
      retainsLeadingBom: newFrom === 0 && originalLogical.startsWith(BOM),
    });
  }
  void resolved;
  return inverses;
}

function sliceEndingsForRangeSimple(
  logical: string,
  from: number,
  to: number,
): MarkdownLineEnding[] {
  const endings: MarkdownLineEnding[] = [];
  for (let i = from; i < to; i++) {
    if (logical.charCodeAt(i) === 10) endings.push("\n");
  }
  return endings;
}
