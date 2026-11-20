export type ModelProgressEvent = { phase: string; received: number; total: number };

/**
 * The model row's progress bar only moves forward. Verify and install report no byte total, so they hold the
 * download's place (or finish it) instead of dropping to 0; an error resets it for the retry.
 */
export function nextModelProgress(last: number, event: ModelProgressEvent): number {
  if (event.phase === "error") return 0;
  if (event.total > 0) return Math.max(last, event.received / event.total);
  const verifying = event.phase === "extracting" || event.phase === "verifying";
  if (verifying && last > 0) return 1;
  return last;
}

/**
 * Model step auto-advance: it fires only when the model goes from not installed to installed while the step is
 * mounted (a download that finished here). A model already installed when the step opens (relaunch, Back to the
 * step) never advances by itself; "unknown" is the state before the first read and decides nothing.
 */
export function trackModelInstall(
  sawNotInstalled: boolean,
  state: string,
): { sawNotInstalled: boolean; advance: boolean } {
  if (state === "unknown") return { sawNotInstalled, advance: false };
  if (state === "installed") return { sawNotInstalled: sawNotInstalled, advance: sawNotInstalled };
  return { sawNotInstalled: true, advance: false };
}
