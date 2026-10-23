// Read-only routing observations. Caller must re-evaluate inside pinned broker dispatch admission.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";

export const MIN_HOSTED_SAMPLES = 3;
export const MAX_HISTORY_RUNS = 8;
const SHA = /^[0-9a-f]{40}$/;
const PRIORITIES = new Set(["FIRST", "SECOND", "BATCH"]);
const need = (ok, reason) => { if (!ok) throw new Error(reason); };
const object = value => value !== null && typeof value === "object" && !Array.isArray(value);
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const time = value => value === null || (Number.isSafeInteger(value) && value >= 0);
const sum = values => {
  if (values.some(value => value === null)) return null;
  const result = values.reduce((total, value) => total + value, 0);
  need(Number.isSafeInteger(result), "forecast total exceeds safe integer range");
  return result;
};
const digest = value => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const stamp = value => typeof value === "string" && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,3})?Z$/.test(value) && Number.isFinite(Date.parse(value));
const runId = value => (typeof value === "string" && /^[1-9][0-9]{0,15}$/.test(value)) || (Number.isSafeInteger(value) && value > 0);

/** Canonical cwd is part of each row identity, including standalone consumer rows. No argv is executed. */
export function groupRows({ workspace, sourceSha, rows }, { canonicalize = fs.realpathSync, pathImpl = path } = {}) {
  need(typeof workspace === "string" && pathImpl.isAbsolute(workspace), "workspace must be absolute");
  need(SHA.test(sourceSha ?? ""), "sourceSha must be a full lowercase SHA");
  const root = canonicalize(workspace);
  need(root === workspace, "workspace must already be canonical");
  need(Array.isArray(rows) && rows.length > 0 && rows.length <= 256, "routing requires 1..256 selected rows");
  const ids = new Set(), groups = new Map();
  const boundRows = rows.map(row => {
    need(object(row) && typeof row.id === "string" && row.id.length > 0 && !ids.has(row.id), "row ids must be unique");
    ids.add(row.id);
    need(Array.isArray(row.args) && row.args.length > 0 && row.args.every(arg => typeof arg === "string" && arg.length > 0 && !arg.includes("\0")), "row args must be nonempty data strings");
    need(row.cwd === undefined || (typeof row.cwd === "string" && row.cwd.length > 0), "row cwd must be a path");
    const cwd = canonicalize(pathImpl.resolve(root, row.cwd ?? "."));
    const rel = pathImpl.relative(root, cwd);
    need(rel !== ".." && !rel.startsWith(`..${pathImpl.sep}`) && !pathImpl.isAbsolute(rel), "row cwd escapes workspace");
    const bound = { id: row.id, kind: row.kind ?? null, cwd, argv: [...row.args] };
    if (!groups.has(cwd)) groups.set(cwd, { workspace: cwd, rows: [] });
    groups.get(cwd).rows.push(bound);
    return bound;
  });
  return { workspace: root, sourceSha, rows: boundRows, rowsSha256: digest(boundRows), groups: [...groups.values()] };
}

/** Validate exactly the installed broker's FORECAST response, including unknowns & all selected arguments. */
export function validateForecast(response, request, sourceSha) {
  need(object(response) && response.type === "FORECAST", "broker did not return FORECAST");
  need(response.workspace === request.workspace && response.sourceSha === sourceSha, "forecast workspace/sourceSha mismatch");
  need(PRIORITIES.has(response.priority), "invalid forecast priority");
  need(Array.isArray(response.rows) && response.rows.length === request.rows.length, "forecast row count mismatch");
  const rows = response.rows.map((row, index) => {
    need(object(row) && same(row.argv, request.rows[index].argv), "forecast row arguments mismatch");
    need(row.warm === null || typeof row.warm === "boolean", "invalid forecast warmth");
    need(time(row.forecastMs) && (row.warm !== null || row.forecastMs === null), "invalid row forecast time");
    return { argv: [...row.argv], warm: row.warm, forecastMs: row.forecastMs, warmthReason: row.warmthReason ?? null };
  });
  need(time(response.localWaitMs) && time(response.localRunMs), "invalid local forecast time");
  need(response.localRunMs === sum(rows.map(row => row.forecastMs)), "localRunMs does not match all rows");
  need(response.knownHistory === (response.localRunMs !== null && response.localWaitMs !== null), "inconsistent knownHistory");
  return { workspace: response.workspace, sourceSha, priority: response.priority, rows, localWaitMs: response.localWaitMs, localRunMs: response.localRunMs, knownHistory: response.knownHistory };
}

export function combineForecasts(groups) {
  need(groups.length > 0 && groups.every(group => PRIORITIES.has(group.priority)), "missing forecast groups");
  need(new Set(groups.map(group => group.priority)).size === 1, "forecast priorities changed between queries");
  const localRunMs = sum(groups.map(group => group.localRunMs));
  // Every query observes the same broker queue. Count it once; max retains the most conservative snapshot.
  const localWaitMs = groups.some(group => group.localWaitMs === null) ? null : Math.max(...groups.map(group => group.localWaitMs));
  return { priority: groups[0].priority, localWaitMs, localRunMs, totalMs: sum([localWaitMs, localRunMs]), knownHistory: localWaitMs !== null && localRunMs !== null, queueCombination: "maximum-observed-once", groups };
}

/** State is attribution context from rk-pipeline's singular --unit dispatch path, never authenticated authority. */
export function historyCandidates(state, { unit, registry }) {
  const records = Object.entries(state?.dispatches ?? {}).filter(([key, record]) => object(record) && key === `${record.registry}:${record.sha}:${record.unit}`).map(([, record]) => record);
  return records.filter(record => object(record)
    && record.unit === unit && record.registry === registry && record.phase !== "preflight"
    && ["published", "dispatched"].includes(record.status) && runId(record.runId)
    && record.runAttempt === 1 && SHA.test(record.sha ?? "")
    && typeof record.requestId === "string" && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(record.requestId)
    && stamp(record.dispatchedAt)
    && (record.units === undefined || same(record.units, [unit]))
    && !records.some(other => String(other.runId) === String(record.runId) && (other.unit !== unit || other.registry !== registry || other.sha !== record.sha || other.requestId !== record.requestId)))
    .sort((a, b) => b.dispatchedAt.localeCompare(a.dispatchedAt))
    .filter((record, index, all) => all.findIndex(other => String(other.runId) === String(record.runId)) === index)
    .slice(0, MAX_HISTORY_RUNS);
}

/** Run/attempt endpoints expose no workflow inputs. Singular unit attribution comes from pipeline mapping.
 * Attempt 1 only: rerun APIs do not establish a fresh queue-entry timestamp independent of prior red attempts. */
export function hostedSample(record, run, jobs, { unit, registry, repository = "bogusyogi/rightkit" }) {
  need(record.unit === unit && record.registry === registry && SHA.test(record.sha ?? "") && runId(record.runId) && record.runAttempt === 1
    && ["published", "dispatched"].includes(record.status) && (record.units === undefined || same(record.units, [unit])), "hosted sample unit/registry/attempt mismatch");
  need(object(run) && String(run.id) === String(record.runId) && run.run_attempt === record.runAttempt, "hosted run identity mismatch");
  need(run.repository?.full_name === repository, "hosted repository mismatch");
  need(run.path === ".github/workflows/publish.yml" && run.event === "workflow_dispatch", "not publication qualification workflow");
  need(run.head_sha === record.sha && run.display_title === `publish ${record.requestId}`, "hosted correlation mismatch");
  need(run.status === "completed" && run.conclusion === "success", "hosted qualification not successful");
  need(object(jobs) && Number.isSafeInteger(jobs.total_count) && jobs.total_count > 0 && jobs.total_count <= 100
    && Array.isArray(jobs.jobs) && jobs.jobs.length === jobs.total_count, "hosted jobs incomplete or exceed query bound");
  need(jobs.jobs.every(job => job.status === "completed" && ["success", "skipped"].includes(job.conclusion)), "red or incomplete qualification job");
  const qualified = jobs.jobs.filter(job => job.name === "qualified" && job.conclusion === "success");
  need(qualified.length === 1 && stamp(qualified[0].completed_at) && stamp(run.created_at), "missing qualification timestamps");
  const start = Date.parse(run.created_at), finish = Date.parse(qualified[0].completed_at);
  need(finish > start && jobs.jobs.every(job => stamp(job.started_at) && stamp(job.completed_at)
    && Date.parse(job.started_at) >= start && Date.parse(job.completed_at) >= Date.parse(job.started_at) && Date.parse(job.completed_at) <= finish), "invalid qualification time boundary");
  return { runId: String(run.id), runAttempt: run.run_attempt, sourceSha: run.head_sha, requestId: record.requestId,
    unit, registry, createdAt: run.created_at, completedAt: qualified[0].completed_at, elapsedMs: finish - start };
}

export function estimateHosted(samples) {
  need(Array.isArray(samples) && samples.length <= MAX_HISTORY_RUNS
    && samples.every(sample => object(sample) && runId(sample.runId))
    && new Set(samples.map(sample => String(sample.runId))).size === samples.length, "hosted samples require distinct run IDs");
  const values = samples.map(sample => sample.elapsedMs).sort((a, b) => a - b);
  need(values.every(value => Number.isSafeInteger(value) && value > 0), "invalid hosted sample duration");
  const enough = values.length >= MIN_HOSTED_SAMPLES;
  const middle = Math.floor(values.length / 2);
  return { estimateMs: enough ? (values.length % 2 ? values[middle] : Math.ceil(values[middle - 1] + (values[middle] - values[middle - 1]) / 2)) : null,
    minimumSamples: MIN_HOSTED_SAMPLES, estimator: "median-queue-inclusive-qualification", reason: enough ? "observed-successful-single-unit-history" : "insufficient-attributed-successful-history", samples };
}

export function chooseRoute(local, hosted) {
  need(time(local.totalMs) && time(hosted.estimateMs), "invalid route comparison time");
  if (local.totalMs === null) return { route: "local", reason: "local-forecast-unknown" };
  if (hosted.estimateMs === null) return { route: "local", reason: "hosted-history-unknown" };
  return local.totalMs > hosted.estimateMs ? { route: "ci", reason: "observed-hosted-cost-lower" } : { route: "local", reason: "observed-local-cost-no-greater" };
}

/** Injected functions return parsed installed-CLI/GitHub observations, not caller-written proof. */
export async function routeCrate(input, { forecast, inspectRun, inspectJobs, canonicalize, pathImpl, now = () => new Date().toISOString() } = {}) {
  need(input.registry === "crates" && /^rightkit-[a-z0-9][a-z0-9-]*$/.test(input.unit ?? ""), "routing requires one crate unit");
  need(typeof forecast === "function", "installed forecast query required");
  const plan = groupRows(input, { canonicalize, pathImpl });
  const observedAt = now(); need(stamp(observedAt), "invalid observation timestamp");
  let local;
  const groups = [];
  try {
    for (const group of plan.groups) {
      const request = { workspace: group.workspace, rows: group.rows.map(row => ({ argv: row.argv })) };
      groups.push(validateForecast(await forecast(structuredClone(request)), request, input.sourceSha));
    }
    local = combineForecasts(groups);
  } catch (error) {
    local = { priority: null, localWaitMs: null, localRunMs: null, totalMs: null, knownHistory: false, groups, reason: `forecast-unavailable: ${error.message}` };
  }
  const samples = [], excluded = [];
  for (const record of historyCandidates(input.state, input)) {
    try {
      need(typeof inspectRun === "function" && typeof inspectJobs === "function", "hosted queries unavailable");
      const sample = hostedSample(record, await inspectRun(record.runId, record.runAttempt), await inspectJobs(record.runId, record.runAttempt), input);
      need(Date.parse(sample.completedAt) <= Date.parse(observedAt), "hosted sample completion is in the future");
      samples.push(sample);
    } catch (error) { excluded.push({ runId: String(record.runId), reason: error.message }); }
  }
  const hosted = { ...estimateHosted(samples), excluded, attribution: "pipeline-single-unit-mapping-plus-github-api",
    timingBoundary: "workflow-created_at-to-qualified.completed_at", maximumQueriedRuns: MAX_HISTORY_RUNS };
  return { schema: 1, kind: "routing-decision", authority: "observation-only", observedAt,
    inputs: { workspace: plan.workspace, sourceSha: plan.sourceSha, registry: input.registry, unit: input.unit, repository: input.repository ?? "bogusyogi/rightkit", selectedRowsSha256: digest(input.rows), rows: plan.rows, rowsSha256: plan.rowsSha256 },
    local, hosted, ...chooseRoute(local, hosted) };
}

/** Fixed read-only child commands: Cargo argv is stdin JSON data, never a command or shell string. */
export function readOnlyQueries({ cwd, platform = process.platform, rustright = platform === "win32" ? undefined : "rustright", gh = platform === "win32" ? "C:\\Program Files\\GitHub CLI\\gh.exe" : "/opt/homebrew/bin/gh", repository = "bogusyogi/rightkit", spawn = spawnSync } = {}) {
  need(/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository), "invalid repository");
  if (platform === "win32") need(typeof rustright === "string" && path.win32.isAbsolute(rustright) && /\.exe$/i.test(rustright), "Windows forecast requires the installed absolute native rustright.exe path");
  const query = (command, args, input) => {
    const result = spawn(command, args, { cwd, input, encoding: "utf8", timeout: 15_000, maxBuffer: 1024 * 1024, windowsHide: true, shell: false });
    need(!result.error && result.status === 0 && typeof result.stdout === "string" && Buffer.byteLength(result.stdout) <= 1024 * 1024, "read-only query failed");
    return JSON.parse(result.stdout);
  };
  const endpoint = (id, attempt) => {
    need(runId(id) && Number.isSafeInteger(attempt) && attempt >= 1, "invalid run lookup");
    return `repos/${repository}/actions/runs/${id}/attempts/${attempt}`;
  };
  return {
    forecast: request => query(rustright, ["forecast", "--request", "-"], JSON.stringify(request)),
    inspectRun: (id, attempt) => query(gh, ["api", "--method", "GET", endpoint(id, attempt)]),
    inspectJobs: (id, attempt) => query(gh, ["api", "--method", "GET", `${endpoint(id, attempt)}/jobs?per_page=100`]),
  };
}
