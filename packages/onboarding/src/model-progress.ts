import type { ModelEntry } from "./types.js";

/**
 * The progress a model row shows. It only moves forward: an entry that reports a lower `progress` than the
 * last one keeps the last. A verify or install step that reports no progress holds the download's place, or
 * finishes it (1) once a download had started, instead of dropping to 0. An `error` entry resets it for the retry.
 */
export function nextEntryProgress(last: number, entry: Pick<ModelEntry, "state" | "progress">): number {
  if (entry.state === "error") return 0;
  if (typeof entry.progress === "number") return Math.max(last, entry.progress);
  if (entry.state === "verifying" && last > 0) return 1;
  return last;
}

/**
 * Model step auto-advance. It fires only when the model goes from not installed to installed while the step
 * is mounted (a download that finished here). A model already installed when the step opens (relaunch, Back to
 * the step) never advances by itself; `"unknown"` is the state before the first read and decides nothing.
 */
export function trackModelInstall(
  sawNotInstalled: boolean,
  state: string,
): { sawNotInstalled: boolean; advance: boolean } {
  if (state === "unknown") return { sawNotInstalled, advance: false };
  if (state === "installed") return { sawNotInstalled, advance: sawNotInstalled };
  return { sawNotInstalled: true, advance: false };
}
