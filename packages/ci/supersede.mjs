// One CI run in progress per repository, without cancelling work that is nearly done.
//
// Rendered into the managed ci workflow's `supersede` job (see manifest.mjs
// supersedeJob) and run there with Node. The file is self-contained: no imports
// beyond the Node standard library, no placeholder-shaped text, and no
// GitHub expression syntax, because the renderer embeds it verbatim.
//
// A newer push to the default branch cancels an older run of the same workflow
// when that run has only just started, has not started at all, or already has a
// failed job. A run that is past the grace fraction of the median successful
// duration is left to finish; the new run's `ci` job waits for it through a
// job-level concurrency group with cancel-in-progress false.

import { pathToFileURL } from "node:url";

export const DEFAULT_GRACE_FRACTION = 0.7;
export const DEFAULT_MEDIAN_WINDOW = 10;
// Fewer successful runs than this gives no trustworthy median, so nothing is
// cancelled for being young; only unstarted runs and failing runs are.
export const MIN_HISTORY = 3;

const FAILED_JOB_CONCLUSIONS = new Set(["failure", "timed_out"]);

export function median(values) {
  const sorted = values.filter((value) => Number.isFinite(value) && value >= 0).sort((a, b) => a - b);
  if (sorted.length === 0) return null;
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

/** Duration in milliseconds of a finished run, or null when the timestamps are unusable. */
export function runDurationMs(run) {
  const start = Date.parse(run?.run_started_at ?? run?.created_at ?? "");
  const end = Date.parse(run?.updated_at ?? "");
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return null;
  return end - start;
}

/**
 * Pure decision for one older run.
 *   run: { status, run_started_at | created_at }
 *   jobs: [{ conclusion }]
 *   medianMs: median duration of recent successful runs, or null when unknown
 * Returns { cancel: boolean, reason: string }.
 */
export function decideSupersede({ run, jobs = [], medianMs = null, nowMs, graceFraction = DEFAULT_GRACE_FRACTION }) {
  if (run.status !== "in_progress") return { cancel: true, reason: "not started (" + run.status + ")" };
  if (jobs.some((job) => FAILED_JOB_CONCLUSIONS.has(job.conclusion))) return { cancel: true, reason: "a job already failed" };
  if (medianMs === null || !(medianMs > 0)) return { cancel: false, reason: "no median duration to compare with" };
  const startedMs = Date.parse(run.run_started_at ?? run.created_at ?? "");
  if (!Number.isFinite(startedMs)) return { cancel: false, reason: "start time unknown" };
  const elapsedMs = Math.max(0, nowMs - startedMs);
  const limitMs = graceFraction * medianMs;
  if (elapsedMs < limitMs) {
    return { cancel: true, reason: "elapsed " + Math.round(elapsedMs / 1000) + "s is under " + Math.round(limitMs / 1000) + "s" };
  }
  return { cancel: false, reason: "elapsed " + Math.round(elapsedMs / 1000) + "s is past " + Math.round(limitMs / 1000) + "s; left to finish" };
}

/** Older runs of the same workflow: lower run number, never the current run. */
export function olderRuns(runs, current) {
  return runs
    .filter((run) => run.id !== current.id && run.run_number < current.run_number)
    .sort((a, b) => a.run_number - b.run_number);
}

export function parseSettings(env) {
  const fraction = env.RIGHT_GIT_GRACE_FRACTION === undefined || env.RIGHT_GIT_GRACE_FRACTION === ""
    ? DEFAULT_GRACE_FRACTION
    : Number(env.RIGHT_GIT_GRACE_FRACTION);
  const window = env.RIGHT_GIT_MEDIAN_WINDOW === undefined || env.RIGHT_GIT_MEDIAN_WINDOW === ""
    ? DEFAULT_MEDIAN_WINDOW
    : Number(env.RIGHT_GIT_MEDIAN_WINDOW);
  if (!(fraction > 0 && fraction <= 1)) throw new Error("RIGHT_GIT_GRACE_FRACTION must be above 0 and at most 1");
  if (!Number.isInteger(window) || window < 1 || window > 100) throw new Error("RIGHT_GIT_MEDIAN_WINDOW must be an integer from 1 to 100");
  return { graceFraction: fraction, medianWindow: window };
}

async function api(env, method, path) {
  const response = await fetch((env.GITHUB_API_URL ?? "https://api.github.com") + path, {
    method,
    headers: {
      accept: "application/vnd.github+json",
      authorization: "Bearer " + env.GH_TOKEN,
      "x-github-api-version": "2022-11-28",
    },
  });
  if (response.status === 409) return { conflict: true };
  if (!response.ok) throw new Error(method + " " + path + " failed: " + response.status);
  return response.status === 202 || response.status === 204 ? {} : response.json();
}

export async function main(env = process.env, now = () => Date.now()) {
  const settings = parseSettings(env);
  const repo = env.GITHUB_REPOSITORY;
  const branch = env.GITHUB_REF_NAME;
  const current = await api(env, "GET", "/repos/" + repo + "/actions/runs/" + env.GITHUB_RUN_ID);
  const base = "/repos/" + repo + "/actions/workflows/" + current.workflow_id + "/runs?branch=" + encodeURIComponent(branch) + "&event=push&per_page=100";
  const inProgress = (await api(env, "GET", base + "&status=in_progress")).workflow_runs ?? [];
  const queued = (await api(env, "GET", base + "&status=queued")).workflow_runs ?? [];
  const older = olderRuns([...inProgress, ...queued], current);
  if (older.length === 0) {
    console.log("No older run of this workflow is in progress.");
    return [];
  }
  const finished = (await api(env, "GET", base.replace("per_page=100", "per_page=" + settings.medianWindow) + "&status=success")).workflow_runs ?? [];
  const durations = finished.map(runDurationMs).filter((value) => value !== null);
  const medianMs = durations.length >= MIN_HISTORY ? median(durations) : null;
  const decisions = [];
  for (const run of older) {
    const jobs = run.status === "in_progress"
      ? (await api(env, "GET", "/repos/" + repo + "/actions/runs/" + run.id + "/jobs?filter=latest&per_page=100")).jobs ?? []
      : [];
    const decision = decideSupersede({ run, jobs, medianMs, nowMs: now(), graceFraction: settings.graceFraction });
    decisions.push({ id: run.id, ...decision });
    console.log("Run " + run.id + " (#" + run.run_number + "): " + (decision.cancel ? "cancel" : "keep") + ", " + decision.reason);
    if (decision.cancel) await api(env, "POST", "/repos/" + repo + "/actions/runs/" + run.id + "/cancel");
  }
  return decisions;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    // Cancelling is an optimization: a failure here must not fail the gate.
    console.log("::warning::supersede check skipped: " + error.message);
  });
}
