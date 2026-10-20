import { parseYaml } from "./workflow-yaml.mjs";
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { provisionWorkloadEnv, workloadEnvironment, assertEnvNames, ENV_PROVIDERS, generateTlsFixture, cleanupTlsFixture, findOpenssl, ownerOnlyDirectory, OPENSSL_CANDIDATES } from "./runtime-workload-env.mjs";
import { frozenBudgetView, assertFrozenBudget, budgetForWorkload, workloadsFor, supportedPrescription, validateSourcePrescription, harnessMetadataPaths, bindHarnessMetadata, readSourcePrescription, workloadPrescriptionPath, trustedRequiredSources, preserveWorkloadRecord, validateWorkloadReceipts, assertCrateName, isCrateName, validateRun, selectArtifact, validateManifest, safeArchiveMembers, measurementProblems, cleanMeasurementEnvironment, assertMeasuredSource } from "./runtime-qualify.mjs";

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
  assert.throws(() => preserveWorkloadRecord(original, "tracing", "rightkit-job-queue"), /no trusted supported coverage/);
});
test("receipt set rejects legacy, missing, duplicate, unknown & every failed identity", () => {
  const { receipts, context, validate } = fixture();
  assert.equal(validate(receipts).length, 2);
  for (const rows of [[], [receipts[0]], [...receipts, receipts[0]], [receipts[0], receipts[0]], [receipts[0], { ...receipts[1], workload: "unknown" }]]) assert.throws(() => validate(rows));
  for (const patch of [{ schema: 1 }, { exitCode: 1 }, { signal: "SIGTERM" }, { problems: ["failed"] }, { sourceSha: "f".repeat(40) }, { runAttempt: "2" }, { runId: "456" }, { artifactId: 43 }, { artifactDigest: "different" }, { repository: "fork/repo" }, { harnessSha: "f".repeat(40) }, { manifestSha256: "f".repeat(64) }, { command: "cargo other" }, { intervalMs: 10 }, { bindings: {} }, { rawSha256: "0".repeat(64) }, { toolchain: {} }, { crate: "rightkit-daemon" }]) assert.throws(() => validate([receipts[0], { ...receipts[1], ...patch }]));
  // Mixed crate: a context or sealed unit naming another crate refuses even when every other field agrees.
  assert.throws(() => validate(receipts, { ...context, crate: "rightkit-daemon" }), /mixed candidate crate/);
  assert.throws(() => validate(receipts, context, { name: "rightkit-job-queue", version: "0.3.4" }), /no trusted supported coverage/ );
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

// ---------------------------------------------------------------------------------------------------------------
// rightkit-daemon: trusted coverage, harness-provided environment, remote profile
// ---------------------------------------------------------------------------------------------------------------
const DAEMON = "rightkit-daemon";
const daemonBudget = () => JSON.parse(fs.readFileSync(new URL("../../crates/rightkit-daemon/runtime-budget.json", import.meta.url), "utf8"));
const daemonSource = () => JSON.parse(fs.readFileSync(new URL("../../crates/rightkit-daemon/benches/workloads.json", import.meta.url), "utf8"));
const ARGS = (...tail) => ["test", "--locked", "--profile", "release-iterate", "-p", DAEMON, ...tail];
test("daemon trusted inventory pins args, trailing bench arguments, env names, interval & profile", () => {
  const w = Object.fromEntries(workloadsFor(DAEMON).map((x) => [x.id, x]));
  assert.deepEqual(Object.keys(w), ["local", "remote", "remote-loopback"]);
  assert.deepEqual(w.local.args, ARGS("--bench", "budget", "--", "64", "20000", "30"));
  assert.deepEqual(w.remote.args, ARGS("--features", "remote", "--bench", "remote_budget", "--", "10000", "30"));
  assert.deepEqual(w["remote-loopback"].args, ARGS("--features", "remote-loopback", "--bench", "budget", "--", "--loopback", "64", "20000", "30"));
  assert.deepEqual(w.local.env, ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"]);
  assert.deepEqual(w.remote.env, ["RIGHTKIT_RUNTIME_SCRATCH_ROOT", "RIGHTKIT_DAEMON_BUDGET_TLS_CERT", "RIGHTKIT_DAEMON_BUDGET_TLS_KEY"]);
  assert.deepEqual(w["remote-loopback"].env, []);
  assert.deepEqual([w.local.intervalMs, w.remote.intervalMs, w["remote-loopback"].intervalMs], [1000, null, null]);
  assert.deepEqual([w.local.profile, w["remote-loopback"].profile], [null, null]);
  assert.deepEqual(w.remote.profile, { name: "remote", features: ["remote"], requiredScale: { unit: "submissions", minCount: 10000 } });
  assert.deepEqual(trustedRequiredSources(DAEMON), ["crates/rightkit-daemon/src/server.rs", "crates/rightkit-daemon/src/remote/http.rs", "crates/rightkit-daemon/src/loopback/mod.rs"]);
});
test("daemon candidate workloads.json equals trusted coverage and nothing else is accepted", () => {
  const source = daemonSource();
  assert.deepEqual(source, supportedPrescription(DAEMON));
  assert.deepEqual(validateSourcePrescription(DAEMON, source).map((w) => w.id), ["local", "remote", "remote-loopback"]);
  const withWorkload = (index, change) => ({ ...source, workloads: source.workloads.map((w, i) => (i === index ? change(w) : w)) });
  for (const mutated of [
    withWorkload(0, (w) => ({ ...w, args: [...w.args, "1"] })),                                    // extra trailing argument
    withWorkload(0, (w) => ({ ...w, args: w.args.map((a) => (a === "20000" ? "1" : a)) })),         // smaller request count
    withWorkload(1, (w) => ({ ...w, args: w.args.filter((a) => a !== "--locked") })),
    withWorkload(1, (w) => ({ ...w, args: w.args.map((a) => (a === "remote" ? "link" : a)) })),     // different feature
    withWorkload(2, (w) => ({ ...w, args: w.args.filter((a) => a !== "--loopback") })),             // loopback mode dropped
    withWorkload(1, (w) => ({ ...w, bench: "budget" })),
    withWorkload(0, (w) => ({ ...w, intervalMs: 50 })),
    withWorkload(1, (w) => ({ ...w, unit: "connections" })),                                        // a field the trusted table does not declare
    withWorkload(1, (w) => ({ ...w, env: ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"] })),
    withWorkload(1, (w) => ({ ...w, profile: null })),
    { ...source, workloads: source.workloads.slice(0, 2) },
    { ...source, workloads: [...source.workloads, source.workloads[0]] },
    { ...source, defaultFeatures: false },
    { ...source, coverage: { ...source.coverage, link: "NOT COVERED" } },
    { ...source, coverage: { ...source.coverage, remote: "weaker" } },
    { ...source, crate: "rightkit-logs" },
  ]) assert.throws(() => validateSourcePrescription(DAEMON, mutated), /differs from trusted supported coverage/);
  // The logs prescription cannot stand in for daemon coverage, nor the reverse.
  assert.throws(() => validateSourcePrescription(DAEMON, supportedPrescription(CRATE)), /differs from trusted/);
  assert.throws(() => validateSourcePrescription(CRATE, source), /differs from trusted/);
});
test("daemon harness metadata binds the shared bench blob once", () => {
  assert.deepEqual(harnessMetadataPaths(DAEMON, daemonSource()), ["crates/rightkit-daemon/benches/workloads.json", "crates/rightkit-daemon/benches/budget.rs", "crates/rightkit-daemon/benches/remote_budget.rs"]);
});
test("logs coverage, args and receipts stay exactly as before the daemon entry", () => {
  const [tracing, detached] = workloadsFor(CRATE);
  assert.deepEqual(tracing.args, ["test", "--locked", "--profile", "release-iterate", "-p", "rightkit-logs", "--features", "tracing", "--bench", "budget"]);
  assert.deepEqual(detached.args, ["test", "--locked", "--profile", "release-iterate", "-p", "rightkit-logs", "--features", "tracing,log-bridge", "--bench", "budget_detached"]);
  for (const w of [tracing, detached]) { assert.deepEqual(w.env, []); assert.equal(w.profile, null); }
  const { receipts, validate } = fixture();
  assert.equal(validate(receipts).length, 2);
  // A logs receipt that claims any harness environment is refused.
  assert.throws(() => validate([receipts[0], { ...receipts[1], envNames: ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"] }]), /identity.status mismatch/);
  assert.equal(budgetForWorkload({ ceilings: { x: 1 } }, tracing).ceilings.x, 1);
});

test("launcher environment never carries RIGHTKIT_* variables; only declared names get harness values", () => {
  const ambient = { PATH: "p", RIGHTKIT_RUNTIME_SCRATCH_ROOT: "candidate", RIGHTKIT_DAEMON_BUDGET_TLS_CERT: "candidate", RIGHTKIT_DAEMON_BUDGET_TLS_KEY: "candidate", rightkit_other: "candidate" };
  assert.deepEqual(cleanMeasurementEnvironment(ambient), { PATH: "p" });
  const remote = workloadsFor(DAEMON)[1];
  const values = { RIGHTKIT_RUNTIME_SCRATCH_ROOT: "/h/scratch", RIGHTKIT_DAEMON_BUDGET_TLS_CERT: "/h/c.pem", RIGHTKIT_DAEMON_BUDGET_TLS_KEY: "/h/k.pem" };
  // Ambient values never survive, even for a declared name; an undeclared harness value is refused.
  assert.deepEqual(workloadEnvironment(ambient, remote.env, values), { PATH: "p", ...values });
  assert.deepEqual(workloadEnvironment(ambient, [], {}), { PATH: "p" });
  assert.throws(() => workloadEnvironment(ambient, ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"], values), /differ from the declared/);
  assert.throws(() => workloadEnvironment(ambient, remote.env, { RIGHTKIT_RUNTIME_SCRATCH_ROOT: "/h/scratch" }), /differ from the declared/);
  for (const bad of [["RIGHTKIT_UNKNOWN"], ["PATH"], ["RIGHTKIT_RUNTIME_SCRATCH_ROOT", "RIGHTKIT_RUNTIME_SCRATCH_ROOT"], ["RIGHTKIT_DAEMON_BUDGET_TLS_CERT"], ["RIGHTKIT_DAEMON_BUDGET_TLS_KEY", "RIGHTKIT_RUNTIME_SCRATCH_ROOT"], "RIGHTKIT_RUNTIME_SCRATCH_ROOT"]) assert.throws(() => assertEnvNames(bad));
  assert.deepEqual(Object.keys(ENV_PROVIDERS), ["RIGHTKIT_RUNTIME_SCRATCH_ROOT", "RIGHTKIT_DAEMON_BUDGET_TLS_CERT", "RIGHTKIT_DAEMON_BUDGET_TLS_KEY"]);
});

// Fake native utilities: no real OpenSSL, whoami or icacls runs in tests.
const fakeNative = () => {
  const calls = [];
  const run = (argv) => {
    calls.push(argv);
    const name = path.basename(argv[0]).toLowerCase();
    if (name.startsWith("whoami")) return '"HOST\\runner","S-1-5-21-1-2-3-1001"\r\n';
    if (name.startsWith("icacls")) return "";
    if (argv[1] === "req") { fs.writeFileSync(argv[argv.indexOf("-keyout") + 1], "KEY"); fs.writeFileSync(argv[argv.indexOf("-out") + 1], "CERT"); return ""; }
    if (argv[1] === "x509") return "Hostname localhost does match certificate\n";
    return "OpenSSL fixture 1.0\n";
  };
  return { calls, run };
};
test("provisioned workload environment is fresh per run, owner-only, key-bearing only for TLS & fully removed", () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "runtime-env-test-"));
  try {
    const { calls, run } = fakeNative();
    const openssl = path.resolve("fake-openssl");
    const names = workloadsFor(DAEMON)[1].env;
    const deps = { tmpRoot, platform: "win32", env: { SystemRoot: path.resolve("fake-root") }, run, discover: () => openssl };
    const first = provisionWorkloadEnv(names, deps);
    assert.deepEqual(Object.keys(first.values), names);
    const { RIGHTKIT_RUNTIME_SCRATCH_ROOT: scratch, RIGHTKIT_DAEMON_BUDGET_TLS_CERT: cert, RIGHTKIT_DAEMON_BUDGET_TLS_KEY: key } = first.values;
    for (const file of [scratch, cert, key]) assert.ok(path.isAbsolute(file) && file.startsWith(tmpRoot) && fs.existsSync(file));
    assert.equal(path.dirname(cert), path.dirname(key));
    assert.notEqual(path.dirname(cert), scratch);
    // Exact OpenSSL argv: ephemeral RSA-2048, one day, localhost + 127.0.0.1, serverAuth, explicit minimal config; no -subj, no ambient config.
    const req = calls.find((argv) => argv[1] === "req");
    assert.deepEqual(req.slice(1, 8), ["req", "-x509", "-newkey", "rsa:2048", "-sha256", "-nodes", "-config"]);
    assert.ok(req[req.indexOf("-days") + 1] === "1" && !req.includes("-subj"));
    assert.deepEqual(req.filter((_, i) => req[i - 1] === "-addext"), ["subjectAltName=DNS:localhost,IP:127.0.0.1", "basicConstraints=critical,CA:FALSE", "keyUsage=critical,digitalSignature,keyEncipherment", "extendedKeyUsage=serverAuth"]);
    assert.ok(calls.some((argv) => path.basename(argv[0]).startsWith("icacls") && argv.includes("/inheritance:r") && argv.includes("*S-1-5-21-1-2-3-1001:(OI)(CI)F")), "private directory loses inherited grants");
    const second = provisionWorkloadEnv(names, deps);
    assert.notEqual(second.values.RIGHTKIT_RUNTIME_SCRATCH_ROOT, scratch);
    first.dispose(); second.dispose();
    assert.deepEqual(fs.readdirSync(tmpRoot), [], "key, certificate and scratch are gone");
    // No TLS names: no certificate material is created and OpenSSL is never looked up.
    const plain = provisionWorkloadEnv(["RIGHTKIT_RUNTIME_SCRATCH_ROOT"], { ...deps, discover: () => { throw new Error("must not look for OpenSSL"); } });
    assert.deepEqual(Object.keys(plain.values), ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"]);
    plain.dispose();
    assert.deepEqual(provisionWorkloadEnv([]).values, {});
    assert.deepEqual(fs.readdirSync(tmpRoot), []);
    // A failed provisioning leaves nothing behind.
    assert.throws(() => provisionWorkloadEnv(names, { ...deps, discover: () => { throw new Error("no provisioned native OpenSSL"); } }), /no provisioned/);
    assert.deepEqual(fs.readdirSync(tmpRoot), []);
  } finally { fs.rmSync(tmpRoot, { recursive: true, force: true }); }
});
test("TLS fixture fails closed and leaves nothing behind", () => {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), "runtime-tls-test-"));
  try {
    const openssl = path.resolve("fake-openssl");
    const base = { parent, openssl, platform: "win32", env: { SystemRoot: path.resolve("fake-root") } };
    const { run: ok } = fakeNative();
    const fail = (run, pattern) => assert.throws(() => generateTlsFixture({ ...base, run }), pattern);
    fail((argv) => { if (argv[1] === "req") throw new Error("provisioned utility failed: openssl (exit 1)"); return ok(argv); }, /utility failed/);
    fail((argv) => (argv[1] === "x509" ? "Hostname localhost does NOT match certificate\n" : ok(argv)), /does not name localhost/);
    fail((argv) => { if (argv[1] === "req") { fs.writeFileSync(argv[argv.indexOf("-keyout") + 1], ""); fs.writeFileSync(argv[argv.indexOf("-out") + 1], ""); return ""; } return ok(argv); }, /complete TLS fixture/);
    assert.throws(() => generateTlsFixture({ ...base, openssl: "relative/openssl", run: ok }), /absolute/);
    assert.deepEqual(fs.readdirSync(parent), []);
    // Fixed-name cleanup refuses a directory it did not create.
    const foreign = path.join(parent, "daemon-tls-foreign"); fs.mkdirSync(foreign); fs.writeFileSync(path.join(foreign, "keep.txt"), "x");
    assert.throws(() => cleanupTlsFixture(foreign), /marker/);
    assert.ok(fs.existsSync(path.join(foreign, "keep.txt")));
    fs.mkdirSync(path.join(parent, "plain"));
    assert.throws(() => cleanupTlsFixture(path.join(parent, "plain")), /not a generated/);
    assert.doesNotThrow(() => cleanupTlsFixture(path.join(parent, "absent")));
  } finally { fs.rmSync(parent, { recursive: true, force: true }); }
});
test("OpenSSL comes only from fixed provisioned locations, never PATH", () => {
  const listed = (files) => (file) => files.includes(file);
  const run = (argv) => { if (argv[1] !== "version") throw new Error("unexpected"); return "ok"; };
  for (const platform of ["win32", "darwin"]) {
    const [first, second] = OPENSSL_CANDIDATES[platform];
    assert.equal(findOpenssl({ platform, exists: listed([first, second]), run }), first);
    assert.equal(findOpenssl({ platform, exists: listed([second]), run }), second);
    assert.equal(findOpenssl({ platform, exists: listed([first, second]), run: (argv) => { if (argv[0] === first) throw new Error("broken"); return "ok"; } }), second);
    assert.throws(() => findOpenssl({ platform, exists: listed([]), run }), /no provisioned native OpenSSL/);
  }
  assert.throws(() => findOpenssl({ platform: "linux", exists: () => true, run }), /no provisioned native OpenSSL/);
});
test("owner-only directory rules per platform", () => {
  const modes = [];
  ownerOnlyDirectory("/p", { platform: "darwin", chmod: (d, m) => modes.push([d, m]), stat: () => ({ mode: 0o40700, uid: 501 }), uid: 501 });
  assert.deepEqual(modes, [["/p", 0o700]]);
  assert.throws(() => ownerOnlyDirectory("/p", { platform: "darwin", chmod() {}, stat: () => ({ mode: 0o40755, uid: 501 }), uid: 501 }), /owner-only/);
  assert.throws(() => ownerOnlyDirectory("/p", { platform: "darwin", chmod() {}, stat: () => ({ mode: 0o40700, uid: 0 }), uid: 501 }), /owner-only/);
  assert.throws(() => ownerOnlyDirectory("C:\\p", { platform: "win32", env: {}, run: () => "" }), /SystemRoot/);
  assert.throws(() => ownerOnlyDirectory("C:\\p", { platform: "win32", env: { SystemRoot: "C:\\Windows" }, run: () => "garbage" }), /SID/);
});

const daemonFixture = () => {
  const crate = DAEMON, version = "0.4.0";
  const context = { repository: "org/rightkit", runId: "123", runAttempt: "1", sourceSha, crate, artifactId: 42, artifactDigest: `sha256:${"b".repeat(64)}` };
  const budget = daemonBudget();
  const bindings = { "crates/rightkit-daemon/src/server.rs": "c".repeat(64) };
  const scale = (w) => (w.id === "remote" ? { unit: "submissions", count: 10000 } : { unit: "connections", count: 64 });
  const receipts = workloadsFor(crate).map((w) => {
    const command = `cargo ${w.args.join(" ")}`;
    return { schema: 2, ...context, workload: w.id, intervalMs: w.intervalMs ?? null, envNames: [...w.env], version, host: { os: "win32", arch: "x64" }, exitCode: 0, signal: null, problems: [], command, toolchain: { cargo: "cargo pinned", rustc: "rustc pinned" }, bindings, harnessSha: sourceSha, manifestSha256: "d".repeat(64), rawFile: `${w.id}-bench.txt`, rawSha256: "e".repeat(64), record: { commit: sourceSha, crateVersion: version, host: { os: "win32", arch: "x64" }, scope: "synthetic", command, observed: { residentMemoryBytes: 10_000_000, idleCpuPercent: 0.1, wakeupsPerSecond: 0.5, diskOpsPerSecond: 0 }, dataScale: scale(w), evidence: { file: `runtime-budget.evidence/win32-x64-${version}-synthetic-${w.id}.txt`, sha256: "e".repeat(64) } } };
  });
  const validate = (rows, ctx = context, unit = { name: crate, version }, b = budget) => validateWorkloadReceipts(rows, ctx, unit, "d".repeat(64), bindings, "win32", sourceSha, b);
  return { receipts, validate, budget };
};
test("daemon receipts accept the real prescription and hold remote to the pinned remote profile", () => {
  const { receipts, validate, budget } = daemonFixture();
  assert.equal(validate(receipts).length, 3);
  // Remote RSS between the top-level and the remote-profile ceiling is valid only for the remote workload.
  assert.ok(budget.ceilings.residentMemoryBytes < 50_000_000 && budget.profiles.remote.ceilings.residentMemoryBytes > 50_000_000);
  const heavy = (id) => receipts.map((r) => (r.workload === id ? { ...r, record: { ...r.record, observed: { ...r.record.observed, residentMemoryBytes: 50_000_000 } } } : r));
  assert.equal(validate(heavy("remote")).length, 3);
  assert.throws(() => validate(heavy("local")), /residentMemoryBytes/);
  assert.throws(() => validate(heavy("remote-loopback")), /residentMemoryBytes/);
  const over = receipts.map((r) => (r.workload === "remote" ? { ...r, record: { ...r.record, observed: { ...r.record.observed, residentMemoryBytes: budget.profiles.remote.ceilings.residentMemoryBytes + 1 } } } : r));
  assert.throws(() => validate(over), /residentMemoryBytes/);
});
test("daemon receipts refuse altered env, args, unit, scale, interval & mixed profile", () => {
  const { receipts, validate } = daemonFixture();
  const change = (id, patch) => receipts.map((r) => (r.workload === id ? { ...r, ...patch } : r));
  const record = (id, patch) => receipts.map((r) => (r.workload === id ? { ...r, record: { ...r.record, ...patch } } : r));
  for (const rows of [
    change("remote", { envNames: [] }), change("remote", { envNames: ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"] }), change("local", { envNames: [] }),
    change("remote-loopback", { envNames: ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"] }), change("local", { envNames: ["RIGHTKIT_RUNTIME_SCRATCH_ROOT", "RIGHTKIT_EXTRA"] }),
    change("remote", { envNames: [...receipts[1].envNames, "RIGHTKIT_EXTRA"] }), change("remote", { envNames: [...receipts[1].envNames].reverse() }),
    change("local", { command: `${receipts[0].command} 1` }), change("remote-loopback", { command: receipts[2].command.replace(" --loopback", "") }),
    change("local", { intervalMs: 500 }), change("remote", { intervalMs: 1000 }),
    record("remote", { dataScale: { unit: "connections", count: 10000 } }),   // unit the profile does not accept
    record("remote", { dataScale: { unit: "submissions", count: 9999 } }),
    record("local", { dataScale: { unit: "submissions", count: 10000 } }),   // remote scale cannot stand in for a connection scale
    record("local", { dataScale: { unit: "connections", count: 63 } }),
    record("remote-loopback", { dataScale: { unit: "connections", count: 1 } }),
    record("remote", { commit: "f".repeat(40) }),
  ]) assert.throws(() => validate(rows));
});
test("daemon profile budget cannot be loosened, renamed or re-scaled by the candidate", () => {
  const { receipts, validate, budget } = daemonFixture();
  const edit = (change) => { const b = structuredClone(budget); change(b); return b; };
  const remote = workloadsFor(DAEMON)[1];
  assert.deepEqual(budgetForWorkload(budget, remote), { ceilings: budget.profiles.remote.ceilings, requiredScale: { unit: "submissions", minCount: 10000 } });
  for (const b of [
    edit((x) => { delete x.profiles; }), edit((x) => { delete x.profiles.remote; }), edit((x) => { x.profiles.remote.features = ["remote", "link"]; }),
    edit((x) => { x.profiles.remote.requiredScale.minCount = 1; }), edit((x) => { x.profiles.remote.requiredScale.unit = "connections"; }),
    edit((x) => { x.profiles.remote.ceilingBasis = { wakeupsPerSecond: "delta-over-bare" }; }), edit((x) => { x.profiles.remote.deltaCeilings = { wakeupsPerSecond: 100 }; }),
    edit((x) => { delete x.profiles.remote.ceilings; }),
  ]) { assert.throws(() => budgetForWorkload(b, remote), /profile remote/); assert.throws(() => validate(receipts, undefined, undefined, b), /profile remote/); }
  // Local and loopback workloads ignore profiles entirely.
  assert.equal(budgetForWorkload(budget, workloadsFor(DAEMON)[0]), budget);
  // Frozen: ceilings, scale and each profile's ceilings, scale & features; measurements and notes stay writable.
  assertFrozenBudget(edit((x) => { x.profiles.remote.measurements = [{ history: true }]; x.profiles.remote.measurementsPending = "n"; x.measurements = []; }), budget);
  for (const change of [(x) => { x.profiles.remote.ceilings.idleCpuPercent = 50; }, (x) => { x.profiles.remote.requiredScale.minCount = 1; }, (x) => { x.profiles.remote.features = []; }, (x) => { x.ceilings.wakeupsPerSecond = 99; }, (x) => { x.requiredScale.minCount = 1; }, (x) => { x.profiles.extra = { features: ["x"] }; }, (x) => { delete x.profiles; }]) assert.throws(() => assertFrozenBudget(edit(change), budget), /candidate changes frozen/);
  assert.deepEqual(Object.keys(frozenBudgetView({ ceilings: {} })), ["ceilings", "requiredScale", "ceilingBasis", "deltaCeilings", "profiles"]);
});
test("the producer launches each workload with provisioned env and never the candidate's", () => {
  const producer = fs.readFileSync(new URL("./runtime-qualify.mjs", import.meta.url), "utf8");
  assert.match(producer, /provisioned = provisionWorkloadEnv\(workload\.env\)/);
  assert.match(producer, /env: \{ \.\.\.workloadEnvironment\(env, workload\.env, provisioned\.values\), CARGO_INCREMENTAL: "1" \}/);
  assert.doesNotMatch(producer, /env: \{ \.\.\.env, CARGO_INCREMENTAL/);
  assert.match(producer, /finally \{ try \{ provisioned\?\.dispose\(\)/);
  assert.match(producer, /envNames: \[\.\.\.workload\.env\]/);
});

// ---------------------------------------------------------------------------------------------------------------
// rightkit-tauri-shell: four native rows, no default features, delta-over-bare wake-ups
// ---------------------------------------------------------------------------------------------------------------
const SHELL = "rightkit-tauri-shell";
const shellSource = () => JSON.parse(fs.readFileSync(new URL("../../crates/rightkit-tauri-shell/benches/workloads.json", import.meta.url), "utf8"));
const shellBudget = () => JSON.parse(fs.readFileSync(new URL("../../crates/rightkit-tauri-shell/runtime-budget.json", import.meta.url), "utf8"));
const SHELL_ARGS = (features) => ["test", "--locked", "--profile", "release-iterate", "-p", SHELL, "--no-default-features", "--features", features, "--bench", "budget_native"];
test("shell trusted inventory pins four native rows with exact feature sets, no default features & 1000 ms", () => {
  const rows = workloadsFor(SHELL);
  assert.deepEqual(rows.map((w) => w.id), ["native-shell-wv2-0_38", "native-shell-wv2-0_39", "native-host-wv2-0_38", "native-host-wv2-0_39"]);
  assert.deepEqual(rows.map((w) => w.args), ["wv2-0_38", "wv2-0_39", "wv2-0_38,host-services,logs,crash", "wv2-0_39,host-services,logs,crash"].map(SHELL_ARGS));
  for (const w of rows) { assert.equal(w.bench, "budget_native"); assert.equal(w.intervalMs, 1000); assert.deepEqual(w.env, []); assert.equal(w.profile, null); }
  assert.deepEqual(trustedRequiredSources(SHELL), ["crates/rightkit-tauri-shell/src/watchdog.rs", "crates/rightkit-tauri-shell/src/lib.rs", "crates/rightkit-tauri-shell/src/host/mod.rs", "crates/rightkit-tauri-shell/src/host/policy.rs", "crates/rightkit-tauri-shell/src/logs_commands.rs"]);
  // --no-default-features is a per-workload option: logs and daemon rows never carry it.
  for (const crate of [CRATE, DAEMON]) for (const w of workloadsFor(crate)) assert.ok(!w.args.includes("--no-default-features"));
});
test("shell candidate workloads.json equals trusted coverage, verbatim, and nothing else is accepted", () => {
  const source = shellSource();
  assert.deepEqual(source, supportedPrescription(SHELL));
  assert.equal(Object.keys(source.coverage).length, 9);
  assert.deepEqual(validateSourcePrescription(SHELL, source).map((w) => w.id), workloadsFor(SHELL).map((w) => w.id));
  const withWorkload = (index, change) => ({ ...source, workloads: source.workloads.map((w, i) => (i === index ? change(w) : w)) });
  const { engine, ...withoutEngine } = source.coverage;
  for (const mutated of [
    withWorkload(0, (w) => ({ ...w, args: w.args.filter((a) => a !== "--no-default-features") })),     // default features sneak back in
    withWorkload(1, (w) => ({ ...w, args: w.args.map((a) => (a === "wv2-0_39" ? "wv2-0_38" : a)) })),  // wrong native binding
    withWorkload(2, (w) => ({ ...w, args: w.args.map((a) => a.replace(",crash", "")) })),               // crash adapter dropped
    withWorkload(3, (w) => ({ ...w, args: w.args.map((a) => a.replace("host-services,", "")) })),
    withWorkload(0, (w) => ({ ...w, args: [...w.args.slice(0, -1), "budget"] })),                         // historical bench
    withWorkload(0, (w) => ({ ...w, bench: "budget" })),
    withWorkload(1, (w) => ({ ...w, intervalMs: 2000 })), withWorkload(1, (w) => ({ ...w, intervalMs: null })),
    withWorkload(2, (w) => ({ ...w, env: ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"] })),
    { ...source, workloads: source.workloads.slice(0, 3) }, { ...source, workloads: [...source.workloads].reverse() },
    { ...source, workloads: [...source.workloads, source.workloads[0]] },
    { ...source, defaultFeatures: false },
    { ...source, coverage: withoutEngine },
    { ...source, coverage: { ...source.coverage, crash: "weaker" } },
    { ...source, coverage: { ...source.coverage, "content-devtools": "qualified" } },
    { ...source, coverage: { ...source.coverage, extra: "new key" } },
    { ...source, crate: "rightkit-daemon" },
  ]) assert.throws(() => validateSourcePrescription(SHELL, mutated), /differs from trusted supported coverage/);
  assert.throws(() => validateSourcePrescription(SHELL, supportedPrescription(DAEMON)), /differs from trusted/);
  assert.throws(() => validateSourcePrescription(DAEMON, source), /differs from trusted/);
  assert.deepEqual(harnessMetadataPaths(SHELL, source), ["crates/rightkit-tauri-shell/benches/workloads.json", "crates/rightkit-tauri-shell/benches/budget_native.rs"]);
});
const shellFixture = () => {
  const crate = SHELL, version = "0.5.0";
  const context = { repository: "org/rightkit", runId: "123", runAttempt: "1", sourceSha, crate, artifactId: 42, artifactDigest: `sha256:${"b".repeat(64)}` };
  const budget = shellBudget();
  const bindings = { "crates/rightkit-tauri-shell/src/watchdog.rs": "c".repeat(64) };
  const receipts = workloadsFor(crate).map((w) => {
    const command = `cargo ${w.args.join(" ")}`;
    return { schema: 2, ...context, workload: w.id, intervalMs: w.intervalMs, envNames: [...w.env], version, host: { os: "win32", arch: "x64" }, exitCode: 0, signal: null, problems: [], command, toolchain: { cargo: "cargo pinned", rustc: "rustc pinned" }, bindings, harnessSha: sourceSha, manifestSha256: "d".repeat(64), rawFile: `${w.id}-bench.txt`, rawSha256: "e".repeat(64), record: { commit: sourceSha, crateVersion: version, host: { os: "win32", arch: "x64" }, scope: "synthetic", command, observed: { residentMemoryBytes: 60_000_000, idleCpuPercent: 0.2, wakeupsPerSecond: 5, diskOpsPerSecond: 0 }, baseline: { wakeupsPerSecond: 3 }, delta: { wakeupsPerSecond: 2 }, dataScale: { unit: "watchdogs", count: 1 }, evidence: { file: `runtime-budget.evidence/win32-x64-${version}-synthetic-${w.id}.txt`, sha256: "e".repeat(64) } } };
  });
  const validate = (rows, b = budget) => validateWorkloadReceipts(rows, context, { name: crate, version }, "d".repeat(64), bindings, "win32", sourceSha, b);
  return { receipts, validate, budget };
};
test("shell receipts gate wake-ups as an unchanged delta over a consistent bare baseline", () => {
  const { receipts, validate, budget } = shellFixture();
  assert.deepEqual([budget.ceilingBasis, budget.deltaCeilings.wakeupsPerSecond, budget.requiredScale.unit], [{ wakeupsPerSecond: "delta-over-bare" }, 4, "watchdogs"]);
  assert.equal(validate(receipts).length, 4);
  const record = (index, change) => receipts.map((r, i) => (i === index ? { ...r, record: { ...r.record, ...change(r.record) } } : r));
  // Whole-process 12/s exceeds the 10.2 ceiling, but the gate looks only at the delta.
  assert.equal(validate(record(0, (r) => ({ observed: { ...r.observed, wakeupsPerSecond: 12 }, baseline: { wakeupsPerSecond: 10 }, delta: { wakeupsPerSecond: 2 } }))).length, 4);
  for (const rows of [
    record(0, (r) => ({ observed: { ...r.observed, wakeupsPerSecond: 12 }, baseline: { wakeupsPerSecond: 7 }, delta: { wakeupsPerSecond: 5 } })),   // delta 5 over the ceiling of 4
    record(1, (r) => ({ delta: { wakeupsPerSecond: 1 } })),                                                                                   // reported delta disagrees with raw minus bare
    record(2, () => ({ baseline: undefined })), record(2, () => ({ delta: undefined })), record(3, () => ({ baseline: undefined, delta: undefined })),
    record(3, (r) => ({ baseline: { wakeupsPerSecond: -1 }, delta: { wakeupsPerSecond: 6 } })),
    record(0, (r) => ({ observed: { ...r.observed, wakeupsPerSecond: null } })),
    record(0, (r) => ({ observed: { ...r.observed, residentMemoryBytes: 124_780_545 } })),             // whole-process ceilings still apply to the other figures
    record(1, () => ({ dataScale: { unit: "connections", count: 1 } })), record(1, () => ({ dataScale: { unit: "watchdogs", count: 0 } })),
    receipts.map((r, i) => (i === 0 ? { ...r, envNames: ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"] } : r)),
    receipts.map((r, i) => (i === 0 ? { ...r, command: r.command.replace(" --no-default-features", "") } : r)),
    receipts.map((r, i) => (i === 1 ? { ...r, intervalMs: 2000 } : r)),
    receipts.slice(0, 3), [...receipts, receipts[0]],
  ]) assert.throws(() => validate(rows));
  // A candidate that drops the delta basis from its budget cannot be validated against an inconsistent frozen view.
  const loosened = structuredClone(budget); delete loosened.ceilingBasis;
  assert.throws(() => assertFrozenBudget(loosened, budget), /candidate changes frozen ceilingBasis/);
  const raised = structuredClone(budget); raised.deltaCeilings.wakeupsPerSecond = 40;
  assert.throws(() => assertFrozenBudget(raised, budget), /candidate changes frozen deltaCeilings/);
});
