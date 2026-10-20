import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { BENCH_ARGS, CRATE, validateRun, selectArtifact, validateManifest, safeArchiveMembers, measurementProblems, cleanMeasurementEnvironment } from "./runtime-qualify.mjs";

const sourceSha = "a".repeat(40);
const run = { id: 123, repository: { full_name: "org/rightkit" }, head_repository: { full_name: "org/rightkit" }, path: ".github/workflows/publish.yml", head_branch: "main", event: "workflow_dispatch", status: "completed", run_attempt: 1, head_sha: sourceSha };
test("candidate identity rejects fork, alternate workflow, branch, event & incomplete run", () => {
  assert.equal(validateRun(run, "org/rightkit", "123").sourceSha, sourceSha);
  for (const bad of [{ head_branch: "topic" }, { path: ".github/workflows/other.yml" }, { event: "pull_request" }, { status: "in_progress" }, { head_sha: "main" }, { head_repository: { full_name: "fork/rightkit" } }, { run_attempt: 0 }]) assert.throws(() => validateRun({ ...run, ...bad }, "org/rightkit", "123"));
});
test("artifact selection requires unique live immutable ID & digest", () => {
  const artifact = { id: 42, name: "publish-packed", expired: false, digest: `sha256:${"b".repeat(64)}` };
  assert.equal(selectArtifact([artifact]).id, 42);
  for (const artifacts of [[], [artifact, artifact], [{ ...artifact, expired: true }], [{ ...artifact, digest: null }]]) assert.throws(() => selectArtifact(artifacts));
});
test("sealed unit binds exact candidate run, source SHA, bytes & size", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "runtime-test-"));
  try {
    const bytes = Buffer.from("sealed fixture");
    const unit = { name: CRATE, version: "0.3.4", dir: `crates/${CRATE}`, file: `${CRATE}-0.3.4.crate`, size: bytes.length, sha256: crypto.createHash("sha256").update(bytes).digest("hex") };
    const context = validateRun(run, "org/rightkit", "123");
    const manifest = { schema: 1, registry: "crates", repository: context.repository, runId: "123", runAttempt: "1", commit: sourceSha, units: [unit] };
    fs.writeFileSync(path.join(dir, unit.file), bytes);
    assert.equal(validateManifest(manifest, context, dir).name, CRATE);
    for (const patch of [{ commit: "b".repeat(40) }, { runAttempt: "2" }, { registry: "npm" }, { units: [unit, unit] }]) assert.throws(() => validateManifest({ ...manifest, ...patch }, context, dir));
    fs.appendFileSync(path.join(dir, unit.file), "tampered");
    assert.throws(() => validateManifest(manifest, context, dir));
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
test("archive paths cannot escape extraction directory", () => {
  assert.deepEqual(safeArchiveMembers("rightkit-logs-0.3.4/src/lib.rs\n"), ["rightkit-logs-0.3.4/src/lib.rs"]);
  for (const name of ["../escape", "/root", "C:/root", "dir\\file", "dir/../file", "./file"]) assert.throws(() => safeArchiveMembers(name));
});
test("measurement launcher passes only explicit non-credential runner inputs", () => {
  const clean = cleanMeasurementEnvironment({
    PATH: "tool-path", GITHUB_ACTIONS: "true", GITHUB_REF: "refs/heads/main", RUNTIME_SOURCE: "source", RUNTIME_PACKED: "packed", RUNTIME_OUT: "out",
    PGPASSWORD: "inherited", GITHUB_TOKEN: "inherited", ACTIONS_ID_TOKEN_REQUEST_URL: "inherited", NODE_OPTIONS: "--require injected",
    CARGO_HOME: "injected", CARGO_ENCODED_RUSTFLAGS: "injected", RUSTFLAGS: "injected", CUSTOM_FLAG: "injected",
  });
  assert.deepEqual(clean, { PATH: "tool-path", GITHUB_ACTIONS: "true", GITHUB_REF: "refs/heads/main", RUNTIME_SOURCE: "source", RUNTIME_PACKED: "packed", RUNTIME_OUT: "out" });
});
test("Windows 1.394 wakeups cannot qualify against unchanged ceiling 1", () => {
  const budget = { ceilings: { residentMemoryBytes: 62914560, idleCpuPercent: 1, wakeupsPerSecond: 1, diskOpsPerSecond: 1 }, requiredScale: { unit: "lines", minCount: 200000 } };
  const record = { commit: sourceSha, host: { os: "win32" }, scope: "synthetic", dataScale: { unit: "lines", count: 200000 }, observed: { residentMemoryBytes: 12767232, idleCpuPercent: 0.052, wakeupsPerSecond: 1.394, diskOpsPerSecond: 0 } };
  assert.match(measurementProblems(record, budget, sourceSha, "win32").join(), /wakeupsPerSecond/);
  record.observed.wakeupsPerSecond = 0.033;
  assert.deepEqual(measurementProblems(record, budget, sourceSha, "win32"), []);
  assert.ok(measurementProblems(record, budget, "b".repeat(40), "win32").length);
  record.dataScale.count = 199999;
  assert.ok(measurementProblems(record, budget, sourceSha, "win32").length);
});
test("bench command is fixed, locked & scoped to rightkit-logs", () => {
  assert.deepEqual(BENCH_ARGS, ["test", "--locked", "--profile", "release-iterate", "-p", "rightkit-logs", "--features", "tracing", "--bench", "budget"]);
});
