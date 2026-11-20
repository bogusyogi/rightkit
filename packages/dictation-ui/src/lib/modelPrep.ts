// Model "preparing" (ANE compile) has no real ETA, so we show a synthetic
// percentage against an assumed duration - capped at 99% until the engine reports
// done, then it jumps to 100%. Download % is real; this is only for the compile step.
// Assumed durations: Parakeet ~3 min, Whisper ~5 min.

export const PREP_ASSUMED_SECS = { parakeet: 180, whisper: 300 } as const;

export function prepPercent(elapsedSecs: number, assumedSecs: number): number {
  if (assumedSecs <= 0) return 99;
  return Math.min(99, Math.max(1, Math.floor((elapsedSecs / assumedSecs) * 100)));
}
