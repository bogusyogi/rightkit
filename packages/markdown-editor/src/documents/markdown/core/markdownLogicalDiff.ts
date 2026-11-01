/**
 * Logical-source differential.
 *
 * Given two logical-source strings (`oldSource`, `newSource`), the
 * differential produces a minimal ordered list of `MarkdownTextEdit`s
 * that transforms `oldSource` into `newSource` when applied in order.
 *
 * The differ is *bounded* and *deterministic*:
 *
 *  - For inputs up to `maxChars` it uses a dynamic-programming
 *    Longest Common Subsequence. The cost is O(mn) in time and
 *    memory.
 *  - For inputs beyond `maxChars` it falls back to a single
 *    "replace whole document" edit so the pipeline never crashes.
 *
 * Both branches return edits that pass the buffer validator
 * (`splitRawMarkdownSource` semantics).
 *
 * The differ is **not** the source of truth for editor changes;
 * editors always compute their own diffs via the buffer's
 * `apply()`. This module exists to:
 *
 *  - surface a change proposal to the diagnostics layer,
 *  - support external merge tools that want a one-shot edit list,
 *  - back the large-diff fallback in `MarkdownDocumentCore.apply`
 *    when the source editor produces a single big insert.
 */

import type { MarkdownTextEdit } from "./types";

/**
 * Maximum input length for the LCS branch. Beyond this, the
 * fallback "replace whole document" edit is returned. 16 384 is a
 * balance between responsiveness and the size of a typical page.
 */
export const MAX_LOGICAL_DIFF_CHARS = 16 * 1024;

/**
 * Compute the minimal ordered edit list transforming `oldSource` into
 * `newSource`. Edits are returned in source order and are safe to
 * apply as a batch to a {@link import("./MarkdownTextBuffer").MarkdownTextBuffer}.
 *
 * Empty / identical inputs short-circuit to `[]`.
 */
export function diffLogicalMarkdown(
  oldSource: string,
  newSource: string,
): readonly MarkdownTextEdit[] {
  if (oldSource === newSource) return [];
  const m = oldSource.length;
  const n = newSource.length;
  if (m === 0) {
    return [
      {
        from: 0,
        to: 0,
        insert: newSource,
        inheritMode: "preserve",
      },
    ];
  }
  if (n === 0) {
    return [{ from: 0, to: m, insert: "", inheritMode: "preserve" }];
  }
  if (m > MAX_LOGICAL_DIFF_CHARS || n > MAX_LOGICAL_DIFF_CHARS) {
    return [{ from: 0, to: m, insert: newSource, inheritMode: "preserve" }];
  }
  return lcsEdits(oldSource, newSource);
}

/**
 * Run the LCS DP and emit edits between consecutive LCS positions.
 */
function lcsEdits(oldSource: string, newSource: string): MarkdownTextEdit[] {
  const m = oldSource.length;
  const n = newSource.length;
  const lcs = buildLcsTable(oldSource, newSource, m, n);
  const positions = collectLcsPositions(oldSource, newSource, m, n, lcs);
  const edits: MarkdownTextEdit[] = [];
  let prevOldEnd = 0;
  let prevNewEnd = 0;
  for (const pos of positions) {
    if (pos.oldIdx > prevOldEnd || pos.newIdx > prevNewEnd) {
      edits.push({
        from: prevOldEnd,
        to: pos.oldIdx,
        insert: newSource.slice(prevNewEnd, pos.newIdx),
        inheritMode: "preserve",
      });
    }
    prevOldEnd = pos.oldIdx + 1;
    prevNewEnd = pos.newIdx + 1;
  }
  if (prevOldEnd < m || prevNewEnd < n) {
    edits.push({
      from: prevOldEnd,
      to: m,
      insert: newSource.slice(prevNewEnd, n),
      inheritMode: "preserve",
    });
  }
  return edits;
}

/**
 * Walk the LCS table backwards from (m, n) and collect LCS
 * positions in source order.
 */
function collectLcsPositions(
  oldSource: string,
  newSource: string,
  m: number,
  n: number,
  lcs: Uint16Array[],
): Array<{ oldIdx: number; newIdx: number }> {
  const out: Array<{ oldIdx: number; newIdx: number }> = [];
  let i = m;
  let j = n;
  while (i > 0 && j > 0) {
    if (oldSource.charCodeAt(i - 1) === newSource.charCodeAt(j - 1)) {
      out.push({ oldIdx: i - 1, newIdx: j - 1 });
      i--;
      j--;
    } else if ((lcs[i - 1]?.[j] ?? 0) >= (lcs[i]?.[j - 1] ?? 0)) {
      i--;
    } else {
      j--;
    }
  }
  out.reverse();
  return out;
}

/**
 * Build the LCS table as `Uint16Array` rows. Each row holds n+1
 * cells; the table is m+1 rows.
 */
function buildLcsTable(oldSource: string, newSource: string, m: number, n: number): Uint16Array[] {
  const table: Uint16Array[] = new Array<Uint16Array>(m + 1);
  for (let r = 0; r <= m; r++) {
    table[r] = new Uint16Array(n + 1);
  }
  for (let i = 1; i <= m; i++) {
    const oc = oldSource.charCodeAt(i - 1);
    const row = table[i] as unknown as number[];
    const prevRow = table[i - 1] as unknown as number[];
    for (let j = 1; j <= n; j++) {
      if (oc === newSource.charCodeAt(j - 1)) {
        row[j] = (prevRow[j - 1] ?? 0) + 1;
      } else {
        const top = prevRow[j] ?? 0;
        const left = row[j - 1] ?? 0;
        row[j] = top >= left ? top : left;
      }
    }
  }
  return table;
}

/**
 * Cheap pre-flight for whether `diffLogicalMarkdown` will run the
 * LCS branch. Useful for callers that want to log a warning before
 * the fallback kicks in.
 */
export function willUseLcsBranch(oldSource: string, newSource: string): boolean {
  return oldSource.length <= MAX_LOGICAL_DIFF_CHARS && newSource.length <= MAX_LOGICAL_DIFF_CHARS;
}
