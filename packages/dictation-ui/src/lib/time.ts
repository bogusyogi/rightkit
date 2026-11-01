// Shared time/duration formatting helpers.
//
// Single implementation shared by the pill's recording timer and the
// onboarding mic-check/PillPreview timer, so a formatting change (e.g. hours
// support) can't drift between the two surfaces. Both files re-export this.
export function mmss(totalSeconds: number): string {
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}
