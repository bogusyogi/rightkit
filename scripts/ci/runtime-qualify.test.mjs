import { parseYaml } from "./workflow-yaml.mjs";
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { BENCH_ARGS, WORKLOADS, SUPPORTED_PRESCRIPTION, validateSourcePrescription, harnessMetadataPaths, preserveWorkloadRecord, validateWorkloadReceipts, CRATE, validateRun, selectArtifact, validateManifest, safeArchiveMembers, measurementProblems, cleanMeasurementEnvironment, assertMeasuredSource } from "./runtime-qualify.mjs";

const sourceSha = "a".repeat(40);
test("measurement requires candidate HEAD & no staged or unstaged tracked changes", () => {
  const source = "candidate-source", calls = [];
  const run = (status = "", head = sourceSha) => (exe, args, cwd) => {
    calls.push({ exe, args, cwd });
    return args[0] === "rev-parse" ? `${head}\n` : status;
  };
  assertMeasuredSource(source, sourceSha, run());
  assert.deepEqual(calls, [
    { exe: "git", args: ["rev-parse", "HEAD"], cwd: source },
    { exe: "git", args: ["status", "--porcelain", "--untracked-files=no"], cwd: source },
  ]);
  for (const status of [" M crates/rightkit-logs/src/lib.rs\n", "M  crates/rightkit-logs/benches/budget.rs\n", "A  crates/rightkit-logs/benches/workloads.json\n", " D Cargo.lock\n", "R  old.rs -> new.rs\n"]) {
    assert.throws(() => assertMeasuredSource(source, sourceSha, run(status)), /tracked changes/);
  }
  calls.length = 0;
  assert.throws(() => assertMeasuredSource(source, sourceSha, run("", "b".repeat(40))), /source SHA/);
  assert.equal(calls.length, 1, "wrong HEAD fails before further binding");
  assert.throws(() => assertMeasuredSource(source, sourceSha, () => { throw new Error("git failed"); }), /git failed/);
});
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

test("dual workload inventory keeps detached interval explicit & commands isolated", () => {
  assert.deepEqual(WORKLOADS.map((w) => w.id), ["tracing", "detached"]);
  assert.deepEqual(WORKLOADS[1].args, ["test", "--locked", "--profile", "release-iterate", "-p", CRATE, "--features", "tracing,log-bridge", "--bench", "budget_detached"]);
  assert.equal(WORKLOADS[1].intervalMs, 1000);
  assert.match(WORKLOADS[1].description, />=1000 ms/);
});
const fixture = () => {
  const context = { repository: "org/rightkit", runId: "123", runAttempt: "1", sourceSha, artifactId: 42, artifactDigest: `sha256:${"b".repeat(64)}` };
  const budget = { ceilings: { residentMemoryBytes: 100, idleCpuPercent: 1, wakeupsPerSecond: 1, diskOpsPerSecond: 1 }, requiredScale: { unit: "lines", minCount: 200000 } };
  const bindings = { "crates/rightkit-logs/src/detached.rs": "c".repeat(64) };
  const receipts = WORKLOADS.map((w) => {
    const command = `cargo ${w.args.join(" ")}`;
    return { schema: 2, ...context, workload: w.id, intervalMs: w.intervalMs ?? null, crate: CRATE, version: "0.3.4", host: { os: "win32", arch: "x64" }, exitCode: 0, signal: null, problems: [], command, toolchain: { cargo: "cargo pinned", rustc: "rustc pinned" }, bindings, harnessSha: sourceSha, manifestSha256: "d".repeat(64), rawFile: `${w.id}-bench.txt`, rawSha256: "e".repeat(64), record: { commit: sourceSha, crateVersion: "0.3.4", host: { os: "win32", arch: "x64" }, scope: "synthetic", command, observed: { residentMemoryBytes: 10, idleCpuPercent: 0, wakeupsPerSecond: 0, diskOpsPerSecond: 0 }, dataScale: { unit: "lines", count: 200000 }, evidence: { file: `runtime-budget.evidence/win32-x64-0.3.4-synthetic-${w.id}.txt`, sha256: "e".repeat(64) } } };
  });
  return { receipts, validate: (rows) => validateWorkloadReceipts(rows, context, { version: "0.3.4" }, "d".repeat(64), bindings, "win32", sourceSha, budget) };
};
test("both workload records retain distinct immutable evidence paths", () => {
  const record = fixture().receipts[0].record;
  const original = { ...record, evidence: { ...record.evidence, file: "runtime-budget.evidence/win32-x64-0.3.4-synthetic.txt" } };
  const rows = WORKLOADS.map((w) => preserveWorkloadRecord(original, w.id));
  assert.equal(new Set(rows.map((r) => r.evidence.file)).size, 2);
  assert.equal(original.evidence.file, "runtime-budget.evidence/win32-x64-0.3.4-synthetic.txt");
  assert.throws(() => preserveWorkloadRecord(original, "unknown"));
});
test("receipt set rejects legacy, missing, duplicate, unknown & every failed identity", () => {
  const { receipts, validate } = fixture();
  assert.equal(validate(receipts).length, 2);
  for (const rows of [[], [receipts[0]], [...receipts, receipts[0]], [receipts[0], receipts[0]], [receipts[0], { ...receipts[1], workload: "unknown" }]]) assert.throws(() => validate(rows));
  for (const patch of [{ schema: 1 }, { exitCode: 1 }, { signal: "SIGTERM" }, { problems: ["failed"] }, { sourceSha: "f".repeat(40) }, { runAttempt: "2" }, { runId: "456" }, { artifactId: 43 }, { artifactDigest: "different" }, { repository: "fork/repo" }, { harnessSha: "f".repeat(40) }, { manifestSha256: "f".repeat(64) }, { command: "cargo other" }, { intervalMs: 10 }, { bindings: {} }, { rawSha256: "0".repeat(64) }, { toolchain: {} }]) assert.throws(() => validate([receipts[0], { ...receipts[1], ...patch }]));
  for (const patch of [{ observed: { ...receipts[1].record.observed, residentMemoryBytes: 0 } }, { observed: { ...receipts[1].record.observed, wakeupsPerSecond: 2 } }, { dataScale: { unit: "lines", count: 0 } }, { commit: "f".repeat(40) }, { command: "cargo wrong" }, { evidence: receipts[0].record.evidence }]) assert.throws(() => validate([receipts[0], { ...receipts[1], record: { ...receipts[1].record, ...patch } }]));
});
test("delta basis uses unchanged declared delta ceiling & requires consistent baseline", () => {
  const { receipts } = fixture();
  const budget = { ceilings: { residentMemoryBytes: 100, idleCpuPercent: 1, wakeupsPerSecond: 1, diskOpsPerSecond: 1 }, ceilingBasis: { wakeupsPerSecond: "delta-over-bare" }, deltaCeilings: { wakeupsPerSecond: 1 }, requiredScale: { unit: "lines", minCount: 200000 } };
  const record = { ...receipts[0].record, observed: { ...receipts[0].record.observed, wakeupsPerSecond: 5 }, baseline: { wakeupsPerSecond: 4.5 }, delta: { wakeupsPerSecond: 0.5 } };
  assert.deepEqual(measurementProblems(record, budget, sourceSha, "win32"), []);
  assert.ok(measurementProblems({ ...record, baseline: undefined }, budget, sourceSha, "win32").length);
  assert.ok(measurementProblems({ ...record, delta: { wakeupsPerSecond: 2 } }, budget, sourceSha, "win32").length);
});

test("candidate source prescription cannot omit, add or alter supported workload coverage", () => {
  const source = JSON.parse(fs.readFileSync(new URL("../../crates/rightkit-logs/benches/workloads.json", import.meta.url), "utf8"));
  assert.deepEqual(source, SUPPORTED_PRESCRIPTION);
  assert.deepEqual(validateSourcePrescription(source).map((w) => w.id), ["tracing", "detached"]);
  for (const patch of [{ workloads: [source.workloads[0]] }, { workloads: [...source.workloads, source.workloads[0]] }, { defaultFeatures: false }, { coverage: {} }, { workloads: [source.workloads[0], { ...source.workloads[1], intervalMs: 50 }] }]) assert.throws(() => validateSourcePrescription({ ...source, ...patch }));
});
test("measurement workflow has no credential inputs & retains concurrent native host matrix", () => {
  const workflow = parseYaml(fs.readFileSync(new URL("../../.github/workflows/qualify-runtime.yml", import.meta.url), "utf8"));
  const job = workflow.jobs.measure;
  assert.deepEqual(job.strategy.matrix.include, [{ runner: "windows-2025", host: "win32" }, { runner: "xcode-27", host: "darwin" }]);
  assert.equal(job.strategy["fail-fast"], "false");
  assert.equal(job.strategy["max-parallel"], undefined);
  for (const permissions of [workflow.permissions, job.permissions]) assert.notEqual(permissions?.["id-token"], "write");
  const inspect = (value) => {
    if (!value || typeof value !== "object") return;
    for (const [key, child] of Object.entries(value)) {
      assert.doesNotMatch(key, /^(?:secrets|id-token)$/i);
      if (key === "env") for (const [name, content] of Object.entries(child)) {
        assert.doesNotMatch(name, /TOKEN|PASSWORD|SECRET|CREDENTIAL/i);
        assert.doesNotMatch(String(content), /secrets\.|github\.token|id-token/i);
      }
      if (typeof child === "string") assert.doesNotMatch(child, /secrets\.|github\.token|ACTIONS_ID_TOKEN/);
      else inspect(child);
    }
  };
  inspect(job);
  for (const step of job.steps.filter((s) => String(s.uses).startsWith("actions/checkout@"))) assert.equal(step.with["persist-credentials"], "false");
  for (const current of Object.values(workflow.jobs)) for (const step of current.steps ?? []) if (String(step.uses).startsWith("actions/upload-artifact@")) assert.equal(step.with["retention-days"], "90");
});

test("qualification harness metadata binds candidate Git tree without requiring shipped bench members", () => {
  assert.deepEqual(harnessMetadataPaths(SUPPORTED_PRESCRIPTION), [
    "crates/rightkit-logs/benches/workloads.json",
    "crates/rightkit-logs/benches/budget.rs",
    "crates/rightkit-logs/benches/budget_detached.rs",
  ]);
  assert.throws(() => harnessMetadataPaths({ ...SUPPORTED_PRESCRIPTION, workloads: [SUPPORTED_PRESCRIPTION.workloads[0]] }));
  // Published source keeps archive binding; bench metadata intentionally has a
  // separate source-SHA binding loop because package include excludes benches/**.
  const producer = fs.readFileSync(new URL("./runtime-qualify.mjs", import.meta.url), "utf8");
  assert.match(producer, /for \(const full of \[\.\.\.sourcePaths, `\$\{unit\.dir\}\/Cargo\.toml`\]\)/);
  assert.match(producer, /for \(const full of harnessMetadataPaths\(prescription\)\)/);
  assert.match(producer, /`\$\{context\.sourceSha\}:\$\{full\}`/);
});
