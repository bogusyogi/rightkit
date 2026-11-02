import type { DisplayJob } from "../contract.js";

/** Stage tag per job id, remembered between renders. */
export type SeenStages = Map<string, string>;

function label(job: DisplayJob): string {
  return (job.title ?? "").trim() || job.input;
}

/**
 * Screen-reader text for jobs that newly finished or failed since `seen`.
 * Returns the message (empty when nothing changed) and the stage map to keep.
 * A job already terminal in `seen` is never announced twice; cancels stay quiet.
 */
export function announceJobChanges(
  seen: SeenStages,
  jobs: readonly DisplayJob[],
): { message: string; next: SeenStages } {
  const next: SeenStages = new Map();
  const lines: string[] = [];
  for (const job of jobs) {
    next.set(job.id, job.stage);
    const before = seen.get(job.id);
    if (before === job.stage) continue;
    if (job.stage === "done") lines.push(`Finished: ${label(job)}`);
    else if (job.stage === "error") {
      lines.push(`Failed: ${label(job)}${job.error ? `. ${job.error}` : ""}`);
    }
  }
  return { message: lines.join(". "), next };
}
