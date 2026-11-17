import type { ProgressState } from "../../lib/progress.js";

/** Copy-free view data for hosts composing progress presentation. */
export function progressViewModel(progress: ProgressState, jobError: string | null, assembling: boolean, resumable: boolean, recovered?: string[] | null, renderMissing?: boolean) {
  const jobs = Object.values(progress.jobs);
  const error = progress.error?.message ?? jobError;
  const running = progress.running || assembling;
  const showRecovered = resumable && !running && !progress.done;
  const recoveredLabels = jobs.length > 0 ? jobs.map((job) => job.label) : (recovered ?? []);
  return {
    jobs: jobs.map((job) => ({ ...job, displayStatus: job.status ?? job.request, failed: job.status === "failed" })),
    error,
    running,
    showRecovered,
    canResume: resumable && !running,
    showRenderMissing: !!renderMissing && !running,
    recoveredLabels,
    idle: !running && !showRecovered && jobs.length === 0 && !progress.render && !error && !progress.done && !progress.cancelled,
    stages: progress.stages.map((label, index) => ({ label, running: index === progress.stages.length - 1 && running })),
    renderFraction: renderFraction(progress),
  };
}

/** Fraction is undefined when duration is unavailable; otherwise clamped to [0, 1]. */
export function renderFraction(progress: ProgressState): number | undefined {
  const render = progress.render;
  return render?.total_ms && render.out_time_ms !== undefined
    ? Math.max(0, Math.min(1, render.out_time_ms / render.total_ms))
    : undefined;
}
