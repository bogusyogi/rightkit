/**
 * Progress for a model's "preparing" step (an engine compile or warm-up) that reports no progress of its
 * own. It is synthetic: `elapsedSecs` against an assumed duration (`assumedSecs`, the app's number per
 * model), at least 1 and capped at 99 until the engine reports done, then the caller shows 100. A
 * non-positive assumed duration reads 99. Download progress is real; this is only for the compile step.
 */
export function prepPercent(elapsedSecs: number, assumedSecs: number): number {
  if (assumedSecs <= 0) return 99;
  return Math.min(99, Math.max(1, Math.floor((elapsedSecs / assumedSecs) * 100)));
}
