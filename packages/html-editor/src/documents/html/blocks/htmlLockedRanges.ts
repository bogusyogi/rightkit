/**
 * Locked-range preservation check (docs/plans/2026-10-17-block-html-editor.md section 6.4).
 * Pure and dependency-free so the mutation engine can import it without a cycle.
 *
 * Contract, in pre-edit coordinates:
 * - a patch that only partly overlaps a locked range, or lies strictly inside it, is a violation;
 * - a patch that wholly contains a locked range (move, duplicate, wrap, delete of its unit) is
 *   allowed only if the locked bytes survive verbatim inside the mutation boundary of the
 *   candidate, once per locked occurrence;
 * - a locked range listed in `exempt` is the deliberate target of the operation (rawReplace,
 *   deleteNode on that block) and is not checked.
 * Locked ranges untouched by every patch are already proven byte-identical by the engine's
 * direct containment check.
 */

export interface LockedRangeLike {
  start: number;
  end: number;
}

export interface LockedPatchLike {
  range: LockedRangeLike;
  text: string;
}

export interface LockedPlanLike {
  mutationBoundary: LockedRangeLike;
  patches: readonly LockedPatchLike[];
}

const sameRange = (a: LockedRangeLike, b: LockedRangeLike) =>
  a.start === b.start && a.end === b.end;

function countOccurrences(haystack: string, needle: string): number {
  if (!needle) return 0;
  let count = 0;
  let from = 0;
  for (;;) {
    const at = haystack.indexOf(needle, from);
    if (at < 0) return count;
    count += 1;
    from = at + needle.length;
  }
}

/** Returns a violation message, or null when every non-exempt locked range is preserved. */
export function findLockedRangeViolation(
  source: string,
  candidate: string,
  plan: LockedPlanLike,
  locked: readonly LockedRangeLike[],
  exempt: readonly LockedRangeLike[] = [],
): string | null {
  const delta = plan.patches.reduce(
    (sum, p) => sum + p.text.length - (p.range.end - p.range.start),
    0,
  );
  const region = candidate.slice(plan.mutationBoundary.start, plan.mutationBoundary.end + delta);
  const required = new Map<string, number>();
  for (const range of locked) {
    if (range.end <= range.start) continue;
    if (exempt.some((e) => sameRange(e, range))) continue;
    let contained = false;
    for (const patch of plan.patches) {
      const p = patch.range;
      const intersects = p.start < range.end && range.start < p.end;
      const insertsInside = p.start === p.end && p.start > range.start && p.start < range.end;
      if (!intersects && !insertsInside) continue;
      if (p.start <= range.start && p.end >= range.end && !insertsInside) {
        contained = true;
        continue;
      }
      return `patch ${p.start}-${p.end} intersects locked range ${range.start}-${range.end}`;
    }
    if (contained) {
      const text = source.slice(range.start, range.end);
      required.set(text, (required.get(text) ?? 0) + 1);
    }
  }
  for (const [text, need] of required) {
    if (countOccurrences(region, text) < need)
      return `locked bytes (${text.slice(0, 40)}) not preserved inside the mutation boundary`;
  }
  return null;
}
