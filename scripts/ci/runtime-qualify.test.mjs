import { parseYaml } from "./workflow-yaml.mjs";
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { workloadsFor, supportedPrescription, validateSourcePrescription, harnessMetadataPaths, bindHarnessMetadata, readSourcePrescription, workloadPrescriptionPath, trustedRequiredSources, preserveWorkloadRecord, validateWorkloadReceipts, assertCrateName, isCrateName, validateRun, selectArtifact, validateManifest, safeArchiveMembers, measurementProblems, cleanMeasurementEnvironment, assertMeasuredSource } from "./runtime-qualify.mjs";

// Every helper below receives the crate explicitly; this lane qualifies rightkit-logs.
const CRATE = "rightkit-logs";
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
    assert.equal(validateManifest(manifest, context, dir, CRATE).name, CRATE);
    for (const patch of [{ commit: "b".repeat(40) }, { runAttempt: "2" }, { registry: "npm" }, { units: [unit, unit] }]) assert.throws(() => validateManifest({ ...manifest, ...patch }, context, dir, CRATE));
    // A crate absent from the sealed manifest, or a malformed crate name, refuses.
    assert.throws(() => validateManifest(manifest, context, dir, "rightkit-daemon"), /exactly one rightkit-daemon/);
    assert.throws(() => validateManifest(manifest, context, dir, "rightkit-Logs"), /crate must match/);
    fs.appendFileSync(path.join(dir, unit.file), "tampered");
    assert.throws(() => validateManifest(manifest, context, dir, CRATE));
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
test("bench commands are derived per crate, locked & scoped to the named crate", () => {
  assert.deepEqual(workloadsFor(CRATE)[0].args, ["test", "--locked", "--profile", "release-iterate", "-p", "rightkit-logs", "--features", "tracing", "--bench", "budget"]);
  // A crate without pinned trusted coverage has no workloads at all.
  assert.throws(() => workloadsFor("rightkit-job-queue"), /no trusted supported coverage for rightkit-job-queue/);
});

test("dual workload inventory keeps detached interval explicit & commands isolated", () => {
  const workloads = workloadsFor(CRATE);
  assert.deepEqual(workloads.map((w) => w.id), ["tracing", "detached"]);
  assert.deepEqual(workloads[1].args, ["test", "--locked", "--profile", "release-iterate", "-p", CRATE, "--features", "tracing,log-bridge", "--bench", "budget_detached"]);
  assert.equal(workloads[1].intervalMs, 2000);
  assert.match(workloads[1].description, /chosen 2000 ms interval/);
});

test("crate names are one rightkit path segment and must match the grammar", () => {
  for (const ok of ["rightkit-logs", "rightkit-job-queue", "rightkit-tauri-shell"]) assert.equal(assertCrateName(ok), ok);
  for (const bad of ["rightkit", "rightkit-", "rightkit-Logs", "rightkit_logs", "logs", "../rightkit-logs", "rightkit-logs/../x", "rightkit-logs\n", "", undefined, null]) {
    assert.throws(() => assertCrateName(bad), /crate must match/);
    assert.equal(isCrateName(bad), false);
  }
});

test("a missing or non-JSON candidate workloads.json refuses", () => {
  assert.throws(() => readSourcePrescription(CRATE, () => { throw new Error("no such blob"); }), /missing or not JSON/);
  assert.throws(() => readSourcePrescription(CRATE, () => Buffer.from("{not json")), /missing or not JSON/);
  assert.equal(workloadPrescriptionPath(CRATE), "crates/rightkit-logs/benches/workloads.json");
});

test("a workloads.json naming a bench that does not exist refuses", () => {
  const real = supportedPrescription(CRATE);
  const renamed = { ...real, workloads: real.workloads.map((w) => (w.id === "tracing" ? { ...w, bench: "budget_absent", args: w.args.map((a) => (a === "budget" ? "budget_absent" : a)) } : w)) };
  assert.throws(() => validateSourcePrescription(CRATE, renamed), /differs from trusted supported coverage/);
  // The declared bench blob must also exist at the candidate SHA, independent of coverage comparison.
  const blobs = new Map([[workloadPrescriptionPath(CRATE), Buffer.from(JSON.stringify(real))], [`crates/${CRATE}/benches/budget.rs`, Buffer.from("// tracing fixture\n")]]);
  const reader = (rel) => { if (!blobs.has(rel)) throw new Error(`missing ${rel}`); return blobs.get(rel); };
  assert.throws(() => bindHarnessMetadata(CRATE, real, reader), /candidate harness metadata missing: crates\/rightkit-logs\/benches\/budget_detached\.rs/);
  blobs.set(`crates/${CRATE}/benches/budget_detached.rs`, Buffer.from("// detached fixture\n"));
  assert.deepEqual(Object.keys(bindHarnessMetadata(CRATE, real, reader)).sort(), [workloadPrescriptionPath(CRATE), `crates/${CRATE}/benches/budget.rs`, `crates/${CRATE}/benches/budget_detached.rs`].sort());
});

test("trusted required sources keep the detached source binding for rightkit-logs", () => {
  assert.deepEqual(trustedRequiredSources(CRATE), ["crates/rightkit-logs/src/detached.rs"]);
});
const fixture = () => {
  const context = { repository: "org/rightkit", runId: "123", runAttempt: "1", sourceSha, crate: CRATE, artifactId: 42, artifactDigest: `sha256:${"b".repeat(64)}` };
  const budget = { ceilings: { residentMemoryBytes: 100, idleCpuPercent: 1, wakeupsPerSecond: 1, diskOpsPerSecond: 1 }, requiredScale: { unit: "lines", minCount: 200000 } };
  const bindings = { "crates/rightkit-logs/src/detached.rs": "c".repeat(64) };
  const receipts = workloadsFor(CRATE).map((w) => {
    const command = `cargo ${w.args.join(" ")}`;
    return { schema: 2, ...context, workload: w.id, intervalMs: w.intervalMs ?? null, crate: CRATE, version: "0.3.4", host: { os: "win32", arch: "x64" }, exitCode: 0, signal: null, problems: [], command, toolchain: { cargo: "cargo pinned", rustc: "rustc pinned" }, bindings, harnessSha: sourceSha, manifestSha256: "d".repeat(64), rawFile: `${w.id}-bench.txt`, rawSha256: "e".repeat(64), record: { commit: sourceSha, crateVersion: "0.3.4", host: { os: "win32", arch: "x64" }, scope: "synthetic", command, observed: { residentMemoryBytes: 10, idleCpuPercent: 0, wakeupsPerSecond: 0, diskOpsPerSecond: 0 }, dataScale: { unit: "lines", count: 200000 }, evidence: { file: `runtime-budget.evidence/win32-x64-0.3.4-synthetic-${w.id}.txt`, sha256: "e".repeat(64) } } };
  });
  const validate = (rows, ctx = context, unit = { name: CRATE, version: "0.3.4" }) => validateWorkloadReceipts(rows, ctx, unit, "d".repeat(64), bindings, "win32", sourceSha, budget);
  return { receipts, context, validate };
};
test("both workload records retain distinct immutable evidence paths", () => {
  const record = fixture().receipts[0].record;
  const original = { ...record, evidence: { ...record.evidence, file: "runtime-budget.evidence/win32-x64-0.3.4-synthetic.txt" } };
  const rows = workloadsFor(CRATE).map((w) => preserveWorkloadRecord(original, w.id, CRATE));
  assert.equal(new Set(rows.map((r) => r.evidence.file)).size, 2);
  assert.equal(original.evidence.file, "runtime-budget.evidence/win32-x64-0.3.4-synthetic.txt");
  assert.throws(() => preserveWorkloadRecord(original, "unknown", CRATE));
  assert.throws(() => preserveWorkloadRecord(original, "tracing", "rightkit-daemon"), /no trusted supported coverage/);
});
test("receipt set rejects legacy, missing, duplicate, unknown & every failed identity", () => {
  const { receipts, context, validate } = fixture();
  assert.equal(validate(receipts).length, 2);
  for (const rows of [[], [receipts[0]], [...receipts, receipts[0]], [receipts[0], receipts[0]], [receipts[0], { ...receipts[1], workload: "unknown" }]]) assert.throws(() => validate(rows));
  for (const patch of [{ schema: 1 }, { exitCode: 1 }, { signal: "SIGTERM" }, { problems: ["failed"] }, { sourceSha: "f".repeat(40) }, { runAttempt: "2" }, { runId: "456" }, { artifactId: 43 }, { artifactDigest: "different" }, { repository: "fork/repo" }, { harnessSha: "f".repeat(40) }, { manifestSha256: "f".repeat(64) }, { command: "cargo other" }, { intervalMs: 10 }, { bindings: {} }, { rawSha256: "0".repeat(64) }, { toolchain: {} }, { crate: "rightkit-daemon" }]) assert.throws(() => validate([receipts[0], { ...receipts[1], ...patch }]));
  // Mixed crate: a context or sealed unit naming another crate refuses even when every other field agrees.
  assert.throws(() => validate(receipts, { ...context, crate: "rightkit-daemon" }), /mixed candidate crate/);
  assert.throws(() => validate(receipts, context, { name: "rightkit-daemon", version: "0.3.4" }), /no trusted supported coverage/);
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
  assert.deepEqual(source, supportedPrescription(CRATE));
  assert.deepEqual(validateSourcePrescription(CRATE, source).map((w) => w.id), ["tracing", "detached"]);
  for (const patch of [{ workloads: [source.workloads[0]] }, { workloads: [...source.workloads, source.workloads[0]] }, { defaultFeatures: false }, { coverage: {} }, { workloads: [source.workloads[0], { ...source.workloads[1], intervalMs: 50 }] }, { crate: "rightkit-daemon" }]) assert.throws(() => validateSourcePrescription(CRATE, { ...source, ...patch }));
  // Crate binding: the file's crate must equal the crate it is read for, and the crate must have pinned coverage.
  assert.throws(() => validateSourcePrescription("rightkit-daemon", source));
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
  assert.deepEqual(harnessMetadataPaths(CRATE, supportedPrescription(CRATE)), [
    "crates/rightkit-logs/benches/workloads.json",
    "crates/rightkit-logs/benches/budget.rs",
    "crates/rightkit-logs/benches/budget_detached.rs",
  ]);
  assert.throws(() => harnessMetadataPaths(CRATE, { ...supportedPrescription(CRATE), workloads: [supportedPrescription(CRATE).workloads[0]] }));
  // Published source keeps archive binding; bench metadata intentionally has a
  // separate source-SHA binding loop because package include excludes benches/**.
  const producer = fs.readFileSync(new URL("./runtime-qualify.mjs", import.meta.url), "utf8");
  assert.match(producer, /for \(const full of \[\.\.\.sourcePaths, `\$\{unit\.dir\}\/Cargo\.toml`\]\)/);
  assert.match(producer, /Object\.assign\(bindings, bindHarnessMetadata\(crate, prescription, \(full\) =>/);
  assert.match(producer, /`\$\{context\.sourceSha\}:\$\{full\}`/);
  assert.doesNotMatch(producer, /\bCRATE\b|\bWORKLOADS\b|BENCH_ARGS|SUPPORTED_PRESCRIPTION|WORKLOAD_PRESCRIPTION_PATH/);
});

test("measurement workflow takes the crate only as a validated input through env", () => {
  const workflow = parseYaml(fs.readFileSync(new URL("../../.github/workflows/qualify-runtime.yml", import.meta.url), "utf8"));
  const crateInput = workflow.on.workflow_dispatch.inputs.crate;
  assert.equal(crateInput.type, "string");
  assert.equal(crateInput.required, "true");
  const resolve = workflow.jobs.candidate.steps.find((s) => s.id === "candidate");
  assert.equal(resolve.env.RUNTIME_CRATE, "${{ inputs.crate }}");
  // No run line may interpolate any workflow input; inputs reach the harness only through env.
  for (const job of Object.values(workflow.jobs)) for (const step of job.steps ?? []) if (step.run) assert.doesNotMatch(step.run, /\$\{\{/);
});
