/**
 * Immutable change-map for the Markdown text buffer.
 *
 * A change-map records how a sorted, non-overlapping set of resolved
 * edits maps positions in the old document to positions in the new
 * document. It is the engine of:
 *
 *  - position-based reconciliation between source and editor states
 *  - inverse-edit coordinate computation (post-command offsets)
 *  - cursor preservation across editor switches
 *
 * Implementation note: this class wraps `@codemirror/state`'s
 * `ChangeSet`. CodeMirror's change-set is a battle-tested primitive
 * for LF-only inputs; we delegate position mapping to it so the
 * buffer can rely on its correctness, and carry the line-ending
 * vector separately at the buffer level.
 */

import { ChangeSet } from "@codemirror/state";
import type { MarkdownChangeMapSummary } from "./types";

interface RangeChange {
  readonly fromA: number;
  readonly toA: number;
  readonly fromB: number;
  readonly toB: number;
}

/**
 * Position association used by {@link MarkdownChangeMap.mapPos}.
 *
 * - `-1` ("before") maps positions inside a replaced range to the
 *   *start* of the replacement. This is what most editor surfaces
 *   want for the anchor side of a selection.
 * - `+1` ("after") maps to the *end* of the replacement.
 */
export type MarkdownMapAssoc = -1 | 1;

export class MarkdownChangeMap implements MarkdownChangeMapSummary {
  private readonly inner: ChangeSet;
  readonly oldLength: number;
  readonly newLength: number;
  readonly changes: readonly RangeChange[];

  constructor(changeSet: ChangeSet);
  constructor(oldLength: number, newLength: number, changes: readonly RangeChange[]);
  constructor(
    changeSetOrOldLength: ChangeSet | number,
    newLength?: number,
    changes?: readonly RangeChange[],
  ) {
    if (typeof changeSetOrOldLength === "number") {
      const cmChanges = (changes ?? []).map((c) => ({
        from: c.fromA,
        to: c.toA,
        insert: "",
      }));
      this.inner = ChangeSet.of(cmChanges, changeSetOrOldLength);
      this.oldLength = changeSetOrOldLength;
      this.newLength = typeof newLength === "number" ? newLength : changeSetOrOldLength;
      this.changes = (changes ?? []).map((c) => ({ ...c }));
    } else {
      this.inner = changeSetOrOldLength;
      this.oldLength = this.inner.length;
      this.newLength = this.inner.newLength;
      const collected: RangeChange[] = [];
      this.inner.iterChanges((fromA, toA, fromB, toB) => {
        collected.push({ fromA, toA, fromB, toB });
      });
      this.changes = collected;
    }
  }

  /**
   * Maps `pos` (an offset in the old document) to an offset in the
   * new document. `assoc` controls the boundary when `pos` falls
   * strictly inside a replaced range.
   */
  mapPos(pos: number, assoc: MarkdownMapAssoc = -1): number {
    return this.inner.mapPos(pos, assoc);
  }

  /**
   * Maps a half-open range in the old document to the new document.
   * Returns `deleted: true` when no character of the original range
   * survives.
   */
  mapRange(from: number, to: number): { from: number; to: number; deleted: boolean } {
    const newFrom = this.inner.mapPos(from, -1);
    const newTo = this.inner.mapPos(to, 1);
    if (from >= to) return { from: newFrom, to: newTo, deleted: false };
    const covered = this.touchesRange(from, to) === "cover";
    return { from: newFrom, to: newTo, deleted: covered };
  }

  /**
   * Returns `true` if any change overlaps `[from, to)`, `"cover"` if
   * a single change entirely covers it, otherwise `false`.
   */
  touchesRange(from: number, to: number): boolean | "cover" {
    let touched = false;
    let covered = false;
    this.inner.iterChanges((fromA, toA) => {
      if (toA <= from) return;
      if (fromA >= to) return;
      touched = true;
      if (fromA <= from && toA >= to) covered = true;
    });
    return covered ? "cover" : touched;
  }

  /**
   * Returns the underlying CodeMirror `ChangeSet`. Used by callers
   * that need direct access to CodeMirror's primitive.
   */
  get codeMirror(): ChangeSet {
    return this.inner;
  }

  /**
   * Summary form of this change-map. Useful for serialization into
   * `MarkdownAppliedCommand`.
   */
  get summary(): MarkdownChangeMapSummary {
    const collected: Array<{ fromA: number; toA: number; fromB: number; toB: number }> = [];
    this.inner.iterChanges((fromA, toA, fromB, toB) => {
      collected.push({ fromA, toA, fromB, toB });
    });
    return {
      oldLength: this.oldLength,
      newLength: this.newLength,
      changes: collected,
    };
  }
}
