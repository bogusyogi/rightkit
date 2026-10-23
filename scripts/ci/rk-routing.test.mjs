import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { groupRows, validateForecast, combineForecasts, historyCandidates, hostedSample, estimateHosted, chooseRoute, routeCrate, readOnlyQueries, MAX_HISTORY_RUNS } from "./rk-routing.mjs";

// Real read-only GitHub API response, captured 2026-10-23 from pipeline-mapped successful qualification.
// Public response fields only. Upload run ID remains to prove it is not measured.
const HOSTED = {
  "mapping": {
    "unit": "rightkit-sandbox",
    "runAttempt": 1,
    "registry": "crates",
    "sha": "249c75fb7e3430b759f1da8d61bb65017fe6e189",
    "runId": 39890678933,
    "requestId": "49bc8332-34b2-4123-a29a-88b84f58dab7",
    "dispatchedAt": "2026-10-23T02:38:39.494Z",
    "status": "published",
    "uploadRunId": 39891390961
  },
  "run": {
    "id": 39890678933,
    "status": "completed",
    "conclusion": "success",
    "head_sha": "249c75fb7e3430b759f1da8d61bb65017fe6e189",
    "run_attempt": 1,
    "path": ".github/workflows/publish.yml",
    "event": "workflow_dispatch",
    "display_title": "publish 49bc8332-34b2-4123-a29a-88b84f58dab7",
    "created_at": "2026-10-23T02:38:34Z",
    "run_started_at": "2026-10-23T02:38:34Z",
    "updated_at": "2026-10-23T02:46:18Z",
    "html_url": "https://github.com/bogusyogi/rightkit/actions/runs/39890678933",
    "repository": { "full_name": "bogusyogi/rightkit" }
  },
  "jobs": {
    "total_count": 12,
    "jobs": [
      {
        "id": 120209430777,
        "name": "guard",
        "status": "completed",
        "conclusion": "success",
        "started_at": "2026-10-23T02:38:38Z",
        "completed_at": "2026-10-23T02:38:41Z"
      },
      {
        "id": 120209458965,
        "name": "scans",
        "status": "completed",
        "conclusion": "success",
        "started_at": "2026-10-23T02:38:44Z",
        "completed_at": "2026-10-23T02:39:59Z"
      },
      {
        "id": 120209836635,
        "name": "plan",
        "status": "completed",
        "conclusion": "success",
        "started_at": "2026-10-23T02:40:01Z",
        "completed_at": "2026-10-23T02:40:28Z"
      },
      {
        "id": 120209981940,
        "name": "pack",
        "status": "completed",
        "conclusion": "success",
        "started_at": "2026-10-23T02:40:30Z",
        "completed_at": "2026-10-23T02:41:07Z"
      },
      {
        "id": 120209982688,
        "name": "build-swap",
        "status": "completed",
        "conclusion": "skipped",
        "started_at": "2026-10-23T02:40:28Z",
        "completed_at": "2026-10-23T02:40:28Z"
      },
      {
        "id": 120209982799,
        "name": "sign-credentials-helper",
        "status": "completed",
        "conclusion": "skipped",
        "started_at": "2026-10-23T02:40:28Z",
        "completed_at": "2026-10-23T02:40:28Z"
      },
      {
        "id": 120209982929,
        "name": "build-credentials-helper",
        "status": "completed",
        "conclusion": "skipped",
        "started_at": "2026-10-23T02:40:28Z",
        "completed_at": "2026-10-23T02:40:28Z"
      },
      {
        "id": 120209983318,
        "name": "sign-swap",
        "status": "completed",
        "conclusion": "skipped",
        "started_at": "2026-10-23T02:40:28Z",
        "completed_at": "2026-10-23T02:40:28Z"
      },
      {
        "id": 120210175863,
        "name": "verify (xcode-27, 26.8.1)",
        "status": "completed",
        "conclusion": "success",
        "started_at": "2026-10-23T02:41:10Z",
        "completed_at": "2026-10-23T02:42:41Z"
      },
      {
        "id": 120210175956,
        "name": "verify (windows-2025, 26.8.1)",
        "status": "completed",
        "conclusion": "success",
        "started_at": "2026-10-23T02:41:10Z",
        "completed_at": "2026-10-23T02:46:12Z"
      },
      {
        "id": 120210177319,
        "name": "verify-linux",
        "status": "completed",
        "conclusion": "skipped",
        "started_at": "2026-10-23T02:41:08Z",
        "completed_at": "2026-10-23T02:41:08Z"
      },
      {
        "id": 120211687604,
        "name": "qualified",
        "status": "completed",
        "conclusion": "success",
        "started_at": "2026-10-23T02:46:14Z",
        "completed_at": "2026-10-23T02:46:17Z"
      }
    ]
  }
};
const SHA = HOSTED.run.head_sha;
const ROWS = [{ id: "test", kind: "test", args: ["test", "--locked", "-p", "rightkit-sandbox", "--no-run"] }];
const input = overrides => ({ workspace: "/repo", sourceSha: SHA, unit: "rightkit-sandbox", registry: "crates", rows: structuredClone(ROWS), ...overrides });
const stateOf = records => ({ dispatches: Object.fromEntries(records.map(record => [`${record.registry}:${record.sha}:${record.unit}`, record])) });
const canonical = { canonicalize: value => value, pathImpl: path.posix };
// Synthetic FORECAST fixture follows Rust forecast_query.rs exactly; no installed broker evidence is claimed.
const forecastFor = (request, milliseconds = 100) => ({ type: "FORECAST", workspace: request.workspace, sourceSha: SHA, priority: "SECOND",
  rows: request.rows.map(row => ({ argv: [...row.argv], warm: true, forecastMs: milliseconds, warmthReason: "observed-existing-state" })),
  localWaitMs: 25, localRunMs: request.rows.length * milliseconds, knownHistory: true });
const record = () => structuredClone(HOSTED.mapping);
const sample = () => hostedSample(record(), HOSTED.run, HOSTED.jobs, input());

// These derived test records are synthetic & exercise comparisons only, never authenticated observations.
function history(count = 3) {
  return Array.from({ length: count }, (_, index) => {
    const mapping = { ...record(), runId: HOSTED.run.id + index, sha: (index + 1).toString(16).padStart(40, "0"), requestId: `fixture-${index}` };
    const run = { ...HOSTED.run, id: mapping.runId, head_sha: mapping.sha, display_title: `publish ${mapping.requestId}` };
    return { mapping, run, jobs: structuredClone(HOSTED.jobs) };
  });
}
function injected(records = []) {
  return { ...canonical, forecast: request => forecastFor(request), now: () => "2026-10-23T05:00:00Z",
    inspectRun: id => records.find(row => row.mapping.runId === id).run,
    inspectJobs: id => records.find(row => row.mapping.runId === id).jobs };
}

test("actual qualification sample includes its queue & excludes later upload", () => {
  const observed = sample();
  assert.equal(observed.elapsedMs, 463_000);
  assert.equal(observed.createdAt, "2026-10-23T02:38:34Z");
  assert.equal(observed.completedAt, "2026-10-23T02:46:17Z");
  assert.notEqual(observed.runId, String(HOSTED.mapping.uploadRunId));
  assert.equal(estimateHosted([observed]).estimateMs, null);
  assert.equal(estimateHosted([observed]).reason, "insufficient-attributed-successful-history");
});

test("hosted estimates require three distinct successful attributed runs & use observed median", () => {
  const samples = history().map(row => hostedSample(row.mapping, row.run, row.jobs, input()));
  samples[0].elapsedMs = 100; samples[1].elapsedMs = 900; samples[2].elapsedMs = 400;
  assert.equal(estimateHosted(samples.slice(0, 2)).estimateMs, null);
  assert.equal(estimateHosted(samples).estimateMs, 400);
  assert.throws(() => estimateHosted([samples[0], samples[0], samples[0]]), /distinct/);
});

test("uploads, other units, red runs, changed SHA/attempt/correlation never become samples", () => {
  for (const mutation of [
    row => row.run.path = ".github/workflows/publish-direct.yml",
    row => row.run.event = "workflow_run",
    row => row.run.repository.full_name = "somebody/other",
    row => row.run.status = "in_progress",
    row => row.run.conclusion = "failure",
    row => row.run.head_sha = "a".repeat(40),
    row => row.run.run_attempt = 2,
    row => row.run.display_title += " extra",
    row => row.mapping.unit = "rightkit-other",
    row => row.mapping.registry = "npm",
    row => row.mapping.units = ["rightkit-sandbox", "rightkit-other"],
    row => row.mapping.status = "failed",
    row => row.mapping.runAttempt = 2,
  ]) {
    const row = structuredClone(HOSTED); mutation(row);
    assert.throws(() => hostedSample(row.mapping, row.run, row.jobs, input()));
  }
});

test("incomplete, red or malformed job timing refuses a hosted sample", () => {
  for (const mutation of [
    jobs => jobs.total_count += 1,
    jobs => jobs.total_count = 101,
    jobs => jobs.jobs[0].conclusion = "failure",
    jobs => jobs.jobs[0].status = "in_progress",
    jobs => jobs.jobs.at(-1).name = "not-qualified",
    jobs => jobs.jobs.at(-1).completed_at = "2020-01-01T00:00:00Z",
    jobs => jobs.jobs[0].completed_at = "2027-01-01T00:00:00Z",
    jobs => jobs.jobs[0].completed_at = null,
    jobs => jobs.jobs[0].started_at = "2020-01-01T00:00:00Z",
  ]) {
    const jobs = structuredClone(HOSTED.jobs); mutation(jobs);
    assert.throws(() => hostedSample(record(), HOSTED.run, jobs, input()));
  }
});

test("history mapping scopes exact registry/unit & bounds read-only observations", () => {
  const records = history(12).map(row => row.mapping);
  assert.equal(historyCandidates(stateOf(records), input()).length, MAX_HISTORY_RUNS);
  for (const patch of [{ registry: "npm" }, { unit: "rightkit-other" }, { status: "failed" }, { phase: "preflight" }, { runAttempt: 2 }, { units: ["rightkit-sandbox", "rightkit-other"] }]) {
    assert.deepEqual(historyCandidates(stateOf([{ ...record(), ...patch }]), input()), []);
  }
  assert.deepEqual(historyCandidates({ dispatches: { "wrong-key": record() } }, input()), []);
  const other = { ...record(), unit: "rightkit-other" };
  assert.deepEqual(historyCandidates(stateOf([record(), other]), input()), [], "ambiguous multi-unit attribution refuses");
});

test("canonical cwd groups preserve every selected Cargo argument including consumer rows", () => {
  const rows = [...ROWS, { id: "consumer", kind: "consumer", cwd: "ci/consumers/wv2-0_39", args: ["check", "--locked", "--features", "engine", "$(never-execute)"] }];
  const plan = groupRows(input({ rows }), canonical);
  assert.deepEqual(plan.groups.map(group => group.workspace), ["/repo", "/repo/ci/consumers/wv2-0_39"]);
  assert.deepEqual(plan.rows[1].argv, rows[1].args);
  const changed = groupRows(input({ rows: rows.map(row => ({ ...row, args: [...row.args, "--lib"] })) }), canonical);
  assert.notEqual(plan.rowsSha256, changed.rowsSha256);
  for (const rows of [[], [ROWS[0], ROWS[0]], [{ ...ROWS[0], args: [] }], [{ ...ROWS[0], args: ["build\0"] }], [{ ...ROWS[0], cwd: "../escape" }]]) {
    assert.throws(() => groupRows(input({ rows }), canonical));
  }
  assert.throws(() => groupRows(input({ workspace: "/alias" }), { ...canonical, canonicalize: () => "/repo" }), /canonical/);
});

test("broker response binds exact workspace, SHA & every row; nullable times remain unknown", () => {
  const request = { workspace: "/repo", rows: [{ argv: ["build", "-p", "a"] }, { argv: ["test", "-p", "b"] }] };
  const response = forecastFor(request);
  assert.equal(validateForecast(response, request, SHA).localRunMs, 200);
  const unknown = structuredClone(response);
  unknown.rows[1] = { ...unknown.rows[1], warm: null, forecastMs: null, warmthReason: "no-readable-existing-state" };
  unknown.localRunMs = null; unknown.knownHistory = false;
  const validated = validateForecast(unknown, request, SHA);
  assert.equal(validated.rows[1].warm, null);
  assert.equal(validated.localRunMs, null);
  for (const mutation of [
    value => value.type = "RESULT",
    value => value.workspace = "/other",
    value => value.sourceSha = "a".repeat(40),
    value => value.priority = "CALLER-OVERRIDE",
    value => value.rows.pop(),
    value => value.rows[1].argv.push("--changed"),
    value => value.rows[1].warm = "false",
    value => value.rows[1].forecastMs = -1,
    value => value.rows[1].forecastMs = 1.5,
    value => value.rows[1].forecastMs = Number.MAX_SAFE_INTEGER + 1,
    value => value.localWaitMs = "0",
    value => value.localRunMs = 100,
    value => value.knownHistory = false,
  ]) {
    const value = structuredClone(response); mutation(value);
    assert.throws(() => validateForecast(value, request, SHA));
  }
});

test("combine queue once across cwd groups, preserve any unknown & reject tier drift", () => {
  const request = { workspace: "/repo", rows: [{ argv: ["build"] }] };
  const one = validateForecast(forecastFor(request), request, SHA);
  const two = { ...structuredClone(one), workspace: "/repo/consumer", localWaitMs: 50 };
  assert.equal(combineForecasts([one, two]).localWaitMs, 50);
  assert.equal(combineForecasts([one, two]).totalMs, 250);
  assert.equal(combineForecasts([one, { ...two, localWaitMs: null, knownHistory: false }]).totalMs, null);
  assert.throws(() => combineForecasts([one, { ...two, priority: "FIRST" }]), /priorities/);
});

test("only a strictly lower known hosted cost routes CI; unknowns & ties route local", () => {
  assert.deepEqual(chooseRoute({ totalMs: 100 }, { estimateMs: 50 }), { route: "ci", reason: "observed-hosted-cost-lower" });
  assert.equal(chooseRoute({ totalMs: 100 }, { estimateMs: 100 }).route, "local");
  assert.equal(chooseRoute({ totalMs: 10 }, { estimateMs: 100 }).route, "local");
  assert.equal(chooseRoute({ totalMs: null }, { estimateMs: 1 }).reason, "local-forecast-unknown");
  assert.equal(chooseRoute({ totalMs: 1 }, { estimateMs: null }).reason, "hosted-history-unknown");
});

test("route receipt binds unit, selected rows, source SHA & observed run timestamps", async () => {
  const records = history();
  const result = await routeCrate(input({ state: stateOf(records.map(row => row.mapping)) }), {
    ...injected(records), forecast: request => forecastFor(request, 500_000),
  });
  assert.equal(result.route, "ci");
  assert.equal(result.authority, "observation-only");
  assert.equal(result.inputs.sourceSha, SHA);
  assert.equal(result.inputs.unit, "rightkit-sandbox");
  assert.deepEqual(result.inputs.rows[0].argv, ROWS[0].args);
  assert.equal(result.hosted.samples.length, 3);
  assert.equal(result.hosted.samples[0].completedAt, HOSTED.jobs.jobs.at(-1).completed_at);
  assert.match(result.inputs.rowsSha256, /^[0-9a-f]{64}$/);
  assert.match(result.inputs.selectedRowsSha256, /^[0-9a-f]{64}$/);
});

test("unknown queue or insufficient real hosted history cannot select CI", async () => {
  const records = [{ mapping: record(), run: HOSTED.run, jobs: HOSTED.jobs }];
  const result = await routeCrate(input({ state: stateOf([record()]) }), injected(records));
  assert.equal(result.route, "local");
  assert.equal(result.reason, "hosted-history-unknown");
  const unknown = await routeCrate(input({ state: stateOf(history().map(row => row.mapping)) }), {
    ...injected(history()), forecast: request => ({ ...forecastFor(request), localWaitMs: null, knownHistory: false }),
  });
  assert.equal(unknown.route, "local");
  assert.equal(unknown.local.localWaitMs, null);
  assert.equal(unknown.reason, "local-forecast-unknown");
});

test("CLI invocations are fixed read-only commands; build tokens remain stdin data", () => {
  const calls = [];
  const api = readOnlyQueries({ cwd: "/repo", platform: "darwin", rustright: "/installed/rustright", gh: "/installed/gh", spawn: (command, args, options) => {
    calls.push({ command, args, options }); return { status: 0, stdout: "{}" };
  } });
  const request = { workspace: "/repo", rows: [{ argv: ["build", "$(never-execute)"] }] };
  api.forecast(request); api.inspectRun(HOSTED.run.id, 1); api.inspectJobs(HOSTED.run.id, 1);
  assert.deepEqual(calls[0].args, ["forecast", "--request", "-"]);
  assert.deepEqual(JSON.parse(calls[0].options.input), request);
  for (const call of calls) {
    assert.equal(call.options.shell, false); assert.equal(call.options.windowsHide, true);
    assert.equal(call.options.timeout, 15_000); assert.equal(call.options.maxBuffer, 1024 * 1024);
  }
  assert.deepEqual(calls[1].args.slice(0, 3), ["api", "--method", "GET"]);
  assert.match(calls[2].args.at(-1), /\/attempts\/1\/jobs\?per_page=100$/);
  assert.throws(() => api.inspectRun("../escape", 1));
  assert.throws(() => readOnlyQueries({ repository: "bogusyogi/rightkit/extra" }));
});

test("Windows forecast requires an absolute native client and never invokes a batch shim", () => {
  for (const rustright of [undefined, "rustright", "rustright.exe", "D:\\agent-bin\\rustright.cmd", "D:\\agent-bin\\rustright.bat"]) {
    assert.throws(() => readOnlyQueries({ platform: "win32", rustright }), /absolute native rustright.exe/);
  }
  let call;
  const api = readOnlyQueries({ platform: "win32", rustright: "D:\\.rustright\\runtime\\rustright.exe", spawn: (command, args, options) => {
    call = { command, args, options }; return { status: 0, stdout: "{}" };
  } });
  api.forecast({ workspace: "D:\\Claude\\rightkit", rows: [{ argv: ["check"] }] });
  assert.equal(call.command, "D:\\.rustright\\runtime\\rustright.exe");
  assert.equal(call.options.shell, false);
});

test("installed Windows broker response preserves observed warm timing and RightKit priority", () => {
  // Live read-only FORECAST captured on Dell, 2026-10-23; no compile executed.
  const response = { type: "FORECAST", workspace: "D:\\Claude\\rightkit", sourceSha: "d34b18b68240d0714a187749d9e78c83d02e52ee", priority: "SECOND", rows: [{ argv: ["check", "--locked", "-p", "rightkit-daemon", "--all-targets", "--all-features"], warm: true, forecastMs: 9883, warmthReason: "observed-existing-state" }], localWaitMs: 68771, localRunMs: 9883, knownHistory: true };
  const result = validateForecast(response, { workspace: response.workspace, rows: response.rows.map(({argv}) => ({argv})) }, response.sourceSha);
  assert.equal(result.priority, "SECOND");
  assert.equal(combineForecasts([result]).totalMs, 78654);
});

test("route queries every consumer cwd & keeps malformed bound response local", async () => {
  const rows = [...ROWS, { id: "consumer", kind: "consumer", cwd: "ci/consumers/wv2-0_39", args: ["check", "--locked", "--features", "engine"] }];
  const calls = [];
  const receipt = await routeCrate(input({ rows }), { ...injected(), forecast: request => {
    calls.push(structuredClone(request));
    return { ...forecastFor(request), localWaitMs: request.workspace === "/repo" ? 25 : 50 };
  } });
  assert.deepEqual(calls.map(call => call.workspace), ["/repo", "/repo/ci/consumers/wv2-0_39"]);
  assert.deepEqual(calls[1].rows[0].argv, rows[1].args);
  assert.equal(receipt.local.localRunMs, 200);
  assert.equal(receipt.local.totalMs, 250);
  const invalid = await routeCrate(input(), { ...injected(), forecast: request => ({ ...forecastFor(request), sourceSha: "0".repeat(40) }) });
  assert.equal(invalid.route, "local");
  assert.equal(invalid.local.knownHistory, false);
  assert.match(invalid.local.reason, /sourceSha mismatch/);
});
