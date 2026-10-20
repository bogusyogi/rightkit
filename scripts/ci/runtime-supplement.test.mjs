import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { sha, REPOSITORY } from "./upload.mjs";
import { candidateHash } from "./supplemental-proof.mjs";
import { WORKLOADS, SUPPORTED_PRESCRIPTION } from "./runtime-qualify.mjs";
import test from "node:test";
import { validateRuntimeWorkflow, authenticatedRuntimeProof, verifyRuntimeSupplement, validateFrozenRuntimeBudget, validateRuntimeRecords, trustedRuntimeHelperPaths } from "./runtime-supplement.mjs";
test("runtime evidence rejects caller JSON before interpreting candidate", () => {
  assert.throws(() => authenticatedRuntimeProof({}, {}, {}), /unauthenticated/);
});
test("runtime measure workflow requires credential-free pinned producer & retained raw evidence", () => {
  const original = fs.readFileSync(new URL("../../.github/workflows/qualify-runtime.yml", import.meta.url), "utf8");
  validateRuntimeWorkflow(original);
  for (const mutate of [
    (s) => s.replace("contents: read", "contents: write"),
    (s) => s.replace("host: win32", "host: linux"),
    (s) => s.replace(/retention-days: 90/g, "retention-days: 14"),
    (s) => s.replace("node harness/scripts/ci/runtime-qualify.mjs measure-clean", "echo claimed-success"),
    (s) => s.replace("node harness/scripts/ci/runtime-qualify.mjs measure-clean", "node harness/scripts/ci/runtime-qualify.mjs measure-clean # secrets.TEST"),
  ].slice(0, 4)) assert.throws(() => validateRuntimeWorkflow(mutate(original)), /refused/);
});

const SOURCE = "a".repeat(40), PRODUCER = "b".repeat(40), VALIDATOR = "c".repeat(40);
const hash = (bytes) => sha("sha256", bytes);
function runtimeFixture(t) {
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? process.env.RUNNER_TEMP ?? (process.platform === "win32" ? "D:/scratch" : "/Volumes/D/scratch");
  const temp = fs.mkdtempSync(path.join(scratch, "runtime-admission-fixture-"));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const archive = Buffer.from("original sealed crate fixture"), metadata = Buffer.from('{"name":"rightkit-logs","vers":"0.3.4","deps":[]}');
  const unit = { name: "rightkit-logs", version: "0.3.4", file: "rightkit-logs-0.3.4.crate", size: archive.length, sha256: hash(archive), metaFile: "rightkit-logs-0.3.4.upload.json", metaSha256: hash(metadata) };
  const manifest = { schema: 1, registry: "crates", commit: SOURCE, runId: "77", runAttempt: "1", units: [unit] }, rawManifest = Buffer.from(JSON.stringify(manifest));
  const zip = { packed: Buffer.from("packed ZIP"), supplement: Buffer.from("runtime ZIP") };
  const candidate = { id: "candidate-77", sourceSha: SOURCE, runId: "77", runAttempt: "1", workflowId: "5", manifestSha256: hash(rawManifest), registry: "crates", units: [unit.name], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, artifacts: { packed: { id: "10", digest: `sha256:${hash(zip.packed)}` }, macos: { id: "11", digest: `sha256:${"d".repeat(64)}`, receiptSha256: "e".repeat(64) }, windows: { id: "12", digest: `sha256:${"f".repeat(64)}`, receiptSha256: "0".repeat(64) } } };
  const pin = { schema: 1, kind: "runtime-workload-pin", original: { candidateId: candidate.id, candidateSha256: candidateHash(candidate), sourceSha: SOURCE, manifestSha256: candidate.manifestSha256, packedArtifact: candidate.artifacts.packed }, producer: { workflowId: "6", runId: "88", runAttempt: "2", jobId: "104", sourceSha: PRODUCER, jobs: { candidate: "101", win32: "102", darwin: "103", qualify: "104" } }, artifact: { id: "20", digest: `sha256:${hash(zip.supplement)}`, proofSha256: "0".repeat(64) }, policy: { id: "runtime-workloads/v1", sourceSha: PRODUCER, validatorSourceSha: PRODUCER } };
  const budget = JSON.parse(fs.readFileSync(new URL("../../crates/rightkit-logs/runtime-budget.json", import.meta.url), "utf8"));
  budget.status = { api: "status()", file: "src/lib.rs", exposes: ["residentMemoryBytes"] }; budget.measurements = [];
  const data = {
    "Cargo.toml": '[workspace]\nmembers = ["crates/rightkit-logs"]\n',
    "crates/rightkit-logs/Cargo.toml": '[package]\nname = "rightkit-logs"\nversion = "0.3.4"\nedition = "2021"\n',
    "crates/rightkit-logs/src/lib.rs": 'pub fn status() {}\n', "crates/rightkit-logs/src/detached.rs": 'pub struct DetachedLog;\n',
    "crates/rightkit-logs/README.md": '## Runtime budget\n\nfixture\n',
    "crates/rightkit-logs/runtime-budget.json": JSON.stringify(budget),
    "crates/rightkit-logs/benches/workloads.json": JSON.stringify(SUPPORTED_PRESCRIPTION),
    "crates/rightkit-logs/benches/budget.rs": '// tracing fixture\n', "crates/rightkit-logs/benches/budget_detached.rs": '// detached fixture\n',
  };
  const readData = (name) => { assert.ok(Object.hasOwn(data, name), `missing source ${name}`); return Buffer.from(data[name]); };
  const source = { source: { sha: SOURCE }, root: path.join(temp, "candidate"), readData };
  const bindings = Object.fromEntries(Object.keys(data).filter((name) => name !== "crates/rightkit-logs/runtime-budget.json" && name !== "crates/rightkit-logs/README.md").map((name) => [name, hash(readData(name))]));
  const context = { repository: REPOSITORY, runId: "77", runAttempt: "1", sourceSha: SOURCE, artifactId: 10, artifactDigest: candidate.artifacts.packed.digest, harnessSha: PRODUCER };
  const files = { packed: { "manifest.json": rawManifest, [unit.file]: archive, [unit.metaFile]: metadata }, supplement: {} };
  const proof = { schema: 2, ...context, manifestSha256: candidate.manifestSha256, workloads: ["tracing", "detached"], proofFiles: {}, records: [], verdict: { ok: true } };
  const envelopes = {};
  for (const host of ["win32", "darwin"]) {
    const envelope = { schema: 2, workloads: WORKLOADS.map((w) => {
      const raw = Buffer.from(`RESULT budget crate=rightkit-logs version=0.3.4 resident_bytes=100 idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=${host === "darwin" ? "unavailable" : "0"} scale_count=200000 scale_unit=lines\n`);
      const command = `cargo ${w.args.join(" ")}`;
      const record = { host: { os: host, arch: host === "darwin" ? "arm64" : "x64" }, crateVersion: unit.version, commit: SOURCE, date: "2026-10-20", scope: "synthetic", dataScale: { description: w.description, unit: "lines", count: 200000 }, command, observed: { residentMemoryBytes: 100, idleCpuPercent: 0, wakeupsPerSecond: 0, diskOpsPerSecond: host === "darwin" ? null : 0 }, evidence: { file: `runtime-budget.evidence/${host}-${w.id}.txt`, sha256: hash(raw) } };
      for (const suffix of ["bench", "evidence"]) files.supplement[`${host}/${w.id}-${suffix}.txt`] = raw;
      return { schema: 2, workload: w.id, intervalMs: w.intervalMs ?? null, crate: unit.name, version: unit.version, ...context, qualificationRunId: "88", qualificationRunAttempt: "2", manifestSha256: candidate.manifestSha256, bindings, toolchain: { rustc: "rustc fixture", cargo: "cargo fixture" }, host: record.host, command, rawFile: `${w.id}-bench.txt`, rawSha256: hash(raw), exitCode: 0, signal: null, problems: [], record };
    }) };
    envelopes[host] = envelope;
  }
  const refresh = () => {
    proof.records = [];
    for (const [host, envelope] of Object.entries(envelopes)) { files.supplement[`${host}/qualification.json`] = Buffer.from(JSON.stringify(envelope)); proof.records.push(...envelope.workloads.map((r) => r.record)); }
    proof.proofFiles = Object.fromEntries(Object.entries(files.supplement).filter(([name]) => name !== "qualification.json").map(([name, bytes]) => [name, { sha256: hash(bytes), size: bytes.length }]));
    const raw = Buffer.from(JSON.stringify(proof)); files.supplement["qualification.json"] = raw; pin.artifact.proofSha256 = hash(raw);
  };
  refresh();
  const repository = { id: 99, full_name: REPOSITORY, default_branch: "main" };
  const sourceWorkflow = { id: 5, path: ".github/workflows/publish.yml", state: "active" }, workflow = { id: 6, path: ".github/workflows/qualify-runtime.yml", state: "active" };
  const run = (id, attempt, head, flow) => ({ id, run_attempt: attempt, repository, head_repository: repository, workflow_id: flow.id, path: flow.path, event: "workflow_dispatch", head_branch: "main", head_sha: head, status: "completed", conclusion: "success" });
  const job = (id, name, runId, attempt, head) => ({ id, name, run_id: runId, run_attempt: attempt, head_sha: head, status: "completed", conclusion: "success" });
  const artifact = (id, name, runId, head, digest) => ({ id, name, expired: false, digest, workflow_run: { id: runId, head_sha: head, head_branch: "main", repository_id: 99, head_repository_id: 99 } });
  const helperMutations = new Map();
  const fetchImpl = async (url) => {
    const route = String(url).slice(`https://api.github.com/repos/${REPOSITORY}`.length).replace(/^\//, "");
    let doc;
    if (!route) doc = repository;
    else if (route === "actions/workflows/publish.yml") doc = sourceWorkflow;
    else if (route === "actions/workflows/qualify-runtime.yml") doc = workflow;
    else if (route === "actions/runs/77/attempts/1") doc = run(77, 1, SOURCE, sourceWorkflow);
    else if (route === "actions/runs/88/attempts/2") doc = run(88, 2, PRODUCER, workflow);
    else if (route === "actions/runs/77/attempts/1/jobs?per_page=100") { const names = ["guard", "plan", "pack", "verify (xcode-27, 26.8.1)", "verify (windows-2025, 26.8.1)"]; doc = { total_count: names.length, jobs: names.map((name, i) => job(i+1, name, 77, 1, SOURCE)) }; }
    else if (route === "actions/runs/88/attempts/2/jobs?per_page=100") doc = { total_count: 4, jobs: ["candidate", "measure (windows-2025, win32)", "measure (xcode-27, darwin)", "qualify"].map((name, i) => job(i+101, name, 88, 2, PRODUCER)) };
    else if (route.startsWith("compare/")) doc = { status: "ahead", merge_base_commit: { sha: route.includes(SOURCE) ? SOURCE : PRODUCER } };
    else if (route === "actions/artifacts/10") doc = artifact(10, "publish-packed", 77, SOURCE, candidate.artifacts.packed.digest);
    else if (route === "actions/artifacts/20") doc = artifact(20, "runtime-qualified", 88, PRODUCER, pin.artifact.digest);
    else if (route === "actions/artifacts/10/zip" || route === "actions/artifacts/20/zip") return { ok: true, arrayBuffer: async () => zip[route.includes("/10/") ? "packed" : "supplement"] };
    else if (route.startsWith("contents/")) {
      const name = route.slice("contents/".length).split("?ref=")[0];
      const bytes = helperMutations.get(name) ?? fs.readFileSync(new URL(`../../${name}`, import.meta.url));
      doc = { encoding: "base64", content: bytes.toString("base64"), sha: sha("sha1", Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])) };
    } else assert.fail(`unexpected route ${route}`);
    return { ok: true, json: async () => structuredClone(doc) };
  };
  const spawn = (exe, args) => {
    if (exe === "tar") {
      const member = args[2].replace("rightkit-logs-0.3.4/", "").replace("Cargo.toml.orig", "Cargo.toml");
      return { status: 0, stdout: readData(`crates/rightkit-logs/${member}`) };
    }
    assert.equal(exe, "unzip", "admission never executes candidate code");
    const kind = path.basename(args[1], ".zip");
    return { status: 0, stdout: args[0] === "-Z1" ? Object.keys(files[kind]).join("\n") : files[kind][args[2]] };
  };
  const env = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: REPOSITORY, GITHUB_REF: "refs/heads/main", GITHUB_SHA: VALIDATOR, GITHUB_WORKFLOW_REF: `${REPOSITORY}/.github/workflows/publish-reviewed.yml@refs/heads/main`, GITHUB_TOKEN: "read-fixture", RUNNER_TEMP: temp };
  const verify = () => verifyRuntimeSupplement({ candidate, pin, manifest, manifestSha256: candidate.manifestSha256, source, env, fetchImpl, spawn });
  return { verify, candidate, unit, proof, pin, envelopes, files, refresh, helperMutations, budget, data, source };
}
test("fully authenticated runtime supplement admits exactly four raw owner-validated records", async (t) => {
  const f = runtimeFixture(t), handle = await f.verify();
  const records = authenticatedRuntimeProof(handle, f.candidate, f.unit);
  assert.equal(records.length, 4);
  assert.deepEqual(records.map((r) => `${r.host.os}/${r.command.split(" ").at(-1)}`), ["win32/budget", "win32/budget_detached", "darwin/budget", "darwin/budget_detached"]);
});
test("authenticated hashes cannot bless fabricated RESULT values or invalid owner metadata", async (t) => {
  for (const mutation of [
    (f) => { f.envelopes.win32.workloads[0].record.observed.residentMemoryBytes = 101; },
    (f) => { f.envelopes.win32.workloads[0].record.date = "not-a-date"; },
    (f) => { f.envelopes.win32.workloads[0].record.dataScale.description = ""; },
    (f) => { f.files.supplement["win32/tracing-bench.txt"] = Buffer.from("no actual RESULT\n"); const r=f.envelopes.win32.workloads[0]; r.rawSha256=hash(f.files.supplement["win32/tracing-bench.txt"]); r.record.evidence.sha256=r.rawSha256; f.files.supplement["win32/tracing-evidence.txt"]=f.files.supplement["win32/tracing-bench.txt"]; },
    (f) => { const b=JSON.parse(f.data["crates/rightkit-logs/runtime-budget.json"]); b.ceilings.wakeupsPerSecond=1000; f.data["crates/rightkit-logs/runtime-budget.json"]=JSON.stringify(b); },
    (f) => { f.helperMutations.set("packages/release/runtime-budgets.mjs", Buffer.from("// changed owner semantics")); },
    (f) => { f.helperMutations.set("packages/release/crate-graph.mjs", Buffer.from("// changed dependency graph")); },
    (f) => { f.helperMutations.set("scripts/ci/registry-consumer.mjs", Buffer.from("// changed archive tool")); },
    (f) => { f.helperMutations.set("scripts/ci/consumer-recipes.mjs", Buffer.from("// changed transitive harness helper")); },
  ]) { const f = runtimeFixture(t); mutation(f); f.refresh(); await assert.rejects(f.verify(), /runtime supplement refused/); }
});
test("trusted owner gate rejects negative mismatched bare baseline floor even when raw agrees", (t) => {
  const f = runtimeFixture(t), record = structuredClone(f.envelopes.win32.workloads[0].record);
  record.observed.wakeupsPerSecond = 0; record.baseline = { wakeupsPerSecond: 10 }; record.delta = { wakeupsPerSecond: -10 };
  const raw = Buffer.from("RESULT budget crate=rightkit-logs version=0.3.4 resident_bytes=100 idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=0 scale_count=200000 scale_unit=lines bare_wakeups_per_s=10 wakeups_delta_per_s=-10\n");
  record.evidence.sha256 = hash(raw);
  const budget = { ...f.budget, ceilingBasis: { wakeupsPerSecond: "delta-over-bare" }, deltaCeilings: { wakeupsPerSecond: 1 } };
  assert.throws(() => validateRuntimeRecords({ source: f.source, sourceSha: SOURCE, unit: f.unit, budget, records: [record], rawByEvidence: new Map([[record.evidence.file, raw]]) }), /bare baseline/);
});
test("frozen budget & trusted helper closure include consumed parsing/graph semantics", () => {
  const budget = { ceilings: { wakeupsPerSecond: 1 }, requiredScale: { unit: "lines", minCount: 200000 } };
  validateFrozenRuntimeBudget(structuredClone(budget), budget);
  for (const key of ["ceilings", "requiredScale", "ceilingBasis", "deltaCeilings"]) assert.throws(() => validateFrozenRuntimeBudget({ ...budget, [key]: {} }, budget), /frozen/);
  const paths = trustedRuntimeHelperPaths();
  for (const name of ["packages/release/runtime-budgets.mjs", "packages/release/crate-graph.mjs", "packages/release/publication-gate.mjs", "scripts/ci/runtime-qualify.mjs", "scripts/ci/registry-consumer.mjs", "scripts/ci/consumer-recipes.mjs", "scripts/ci/upload.mjs", "pnpm-lock.yaml"]) assert.ok(paths.includes(name), `missing trusted helper ${name}`);
});
