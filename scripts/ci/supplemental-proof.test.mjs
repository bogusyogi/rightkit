import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { gzipSync } from "node:zlib";
import { REPOSITORY, sha } from "./upload.mjs";
import { scanArchiveForSecrets } from "./archive-secret-scan.mjs";
import { parseYaml } from "./workflow-yaml.mjs";
import { SUPPLEMENT_WORKFLOW, SUPPLEMENT_POLICY, validateSupplementPin, validateSupplementProof, verifyArchiveSupplement, supplementalArchiveScan, produceArchiveSupplement } from "./supplemental-proof.mjs";

const SOURCE = "a".repeat(40); const PRODUCER = "b".repeat(40); const VALIDATOR = "c".repeat(40);
const hash = (bytes) => sha("sha256", bytes);
function fixtureArchive() {
  const content = Buffer.from("fixture archive content");
  const header = Buffer.alloc(512);
  header.write("rightkit-a-0.1.0/src/lib.rs");
  header.write(`${content.length.toString(8).padStart(11, "0")}\0`, 124);
  header.fill(32, 148, 156); header[156] = 48;
  header.write("ustar\0", 257);
  const checksum = header.reduce((sum, byte) => sum + byte, 0);
  header.write(`${checksum.toString(8).padStart(6, "0")}\0 `, 148);
  return gzipSync(Buffer.concat([header, content, Buffer.alloc(512 - content.length), Buffer.alloc(1024)]));
}
function fixture(t) {
  const parent = process.env.RIGHTKIT_TEST_SCRATCH ?? process.env.RUNNER_TEMP ?? os.tmpdir();
  const temp = fs.realpathSync.native(fs.mkdtempSync(path.join(parent, "supplement-fixture-")));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const archive = fixtureArchive();
  const metadata = Buffer.from('{"name":"rightkit-a","vers":"0.1.0","deps":[]}');
  const unit = { name: "rightkit-a", version: "0.1.0", file: "rightkit-a-0.1.0.crate", size: archive.length, sha256: hash(archive), metaFile: "rightkit-a-0.1.0.upload.json", metaSha256: hash(metadata) };
  const manifest = { schema: 1, registry: "crates", commit: SOURCE, runId: "77", runAttempt: "1", units: [unit] };
  const rawManifest = Buffer.from(JSON.stringify(manifest));
  const zip = { packed: Buffer.from("original packed ZIP fixture"), supplement: Buffer.from("supplement ZIP fixture") };
  const candidate = { id: "candidate-77", sourceSha: SOURCE, runId: "77", runAttempt: "1", workflowId: "5", manifestSha256: hash(rawManifest), registry: "crates", units: [unit.name], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, artifacts: { packed: { id: "10", digest: `sha256:${hash(zip.packed)}` }, macos: { id: "11", digest: `sha256:${"d".repeat(64)}`, receiptSha256: "e".repeat(64) }, windows: { id: "12", digest: `sha256:${"f".repeat(64)}`, receiptSha256: "0".repeat(64) } } };
  const original = { candidateId: candidate.id, candidateSha256: hash(JSON.stringify(candidate)), sourceSha: SOURCE, manifestSha256: candidate.manifestSha256, packedArtifact: candidate.artifacts.packed };
  const policy = { id: SUPPLEMENT_POLICY, sourceSha: PRODUCER, validatorSourceSha: PRODUCER };
  const archiveScan = { schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: unit.sha256, status: "pass", files: 1, violations: [], problems: [] };
  const proof = { schema: 1, kind: "archive-scan-supplement", original: structuredClone(original), producer: { workflow: SUPPLEMENT_WORKFLOW, runId: "88", runAttempt: "2", sourceSha: PRODUCER }, policy: structuredClone(policy), units: [{ name: unit.name, version: unit.version, file: unit.file, sha256: unit.sha256, archiveScan }] };
  const rawProof = Buffer.from(JSON.stringify(proof));
  const pin = { schema: 1, kind: "archive-scan-supplement-pin", original, producer: { workflowId: "6", runId: "88", runAttempt: "2", jobId: "100", sourceSha: PRODUCER }, artifact: { id: "20", digest: `sha256:${hash(zip.supplement)}`, proofSha256: hash(rawProof) }, policy };
  const repository = { id: 99, full_name: REPOSITORY, default_branch: "main" };
  const sourceWorkflow = { id: 5, path: ".github/workflows/publish.yml", state: "active" };
  const workflow = { id: 6, path: SUPPLEMENT_WORKFLOW, state: "active" };
  const runOf = (runId, attempt, head, workflow) => ({ id: runId, run_attempt: attempt, repository, head_repository: repository, workflow_id: workflow.id, path: workflow.path, event: "workflow_dispatch", head_branch: "main", head_sha: head, status: "completed", conclusion: "success" });
  const sourceRun = runOf(77, 1, SOURCE, sourceWorkflow);
  const run = runOf(88, 2, PRODUCER, workflow);
  const jobOf = (id, name, runId, attempt, head) => ({ id, name, run_id: runId, run_attempt: attempt, head_sha: head, status: "completed", conclusion: "success" });
  const sourceJobs = { total_count: 5, jobs: ["guard", "plan", "pack", "verify (xcode-27, 26.8.1)", "verify (windows-2025, 26.8.1)"].map((name, i) => jobOf(i + 1, name, 77, 1, SOURCE)) };
  const jobs = { total_count: 1, jobs: [jobOf(100, "archive-scan", 88, 2, PRODUCER)] };
  const artifactOf = (id, name, runId, head, digest) => ({ id, name, expired: false, digest, workflow_run: { id: runId, head_sha: head, head_branch: "main", repository_id: 99, head_repository_id: 99 } });
  const artifacts = { packed: artifactOf(10, "publish-packed", 77, SOURCE, candidate.artifacts.packed.digest), supplement: artifactOf(20, "archive-scan-supplement-candidate-77-2", 88, PRODUCER, pin.artifact.digest) };
  const files = { packed: { "manifest.json": rawManifest, [unit.file]: archive, [unit.metaFile]: metadata }, supplement: { "archive-scan-supplement.json": rawProof } };
  const env = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: REPOSITORY, GITHUB_REF: "refs/heads/main", GITHUB_SHA: VALIDATOR, GITHUB_WORKFLOW_REF: `${REPOSITORY}/.github/workflows/publish-reviewed.yml@refs/heads/main`, GITHUB_TOKEN: "read-token-fixture", RUNNER_TEMP: temp };
  const state = { candidate, pin, manifest, proof, sourceRun, sourceJobs, run, jobs, workflow, artifacts, zip, files, env, repository, sourceWorkflow, comparison: { status: "ahead", merge_base_commit: { sha: PRODUCER } } };
  const fetchImpl = async (url, options) => {
    assert.equal(options.redirect, "manual");
    assert.equal(options.method, "GET");
    const prefix = `https://api.github.com/repos/${REPOSITORY}`;
    assert.ok(String(url) === prefix || String(url).startsWith(`${prefix}/`));
    const route = String(url).slice(prefix.length).replace(/^\//, "");
    let doc;
    if (!route) doc = state.repository;
    else if (route === "actions/workflows/publish.yml") doc = state.sourceWorkflow;
    else if (route === "actions/workflows/supplement-archive-scan.yml") doc = state.workflow;
    else if (route === "actions/runs/77/attempts/1") doc = state.sourceRun;
    else if (route === "actions/runs/77/attempts/1/jobs?per_page=100") doc = state.sourceJobs;
    else if (route === "actions/runs/88/attempts/2") doc = state.run;
    else if (route === "actions/runs/88/attempts/2/jobs?per_page=100") doc = state.jobs;
    else if (route.startsWith(`compare/${SOURCE}...`)) doc = { status: "ahead", merge_base_commit: { sha: SOURCE } };
    else if (route === `compare/${PRODUCER}...${VALIDATOR}`) doc = state.comparison;
    else if (route === "actions/artifacts/10") doc = state.artifacts.packed;
    else if (route === "actions/artifacts/20") doc = state.artifacts.supplement;
    else if (route === "actions/artifacts/10/zip" || route === "actions/artifacts/20/zip") return { ok: true, status: 200, arrayBuffer: async () => state.zip[route.includes("/10/") ? "packed" : "supplement"] };
    else assert.fail(`unexpected evidence route ${route}`);
    return { ok: true, status: 200, json: async () => structuredClone(doc) };
  };
  const spawn = (command, args) => {
    assert.equal(command, "unzip", "no archived code, builds or tests execute");
    const kind = path.basename(args[1], ".zip");
    return { status: 0, stdout: args[0] === "-Z1" ? Object.keys(state.files[kind]).join("\n") : state.files[kind][args[2]] };
  };
  const verify = () => verifyArchiveSupplement({ candidate: state.candidate, pin: state.pin, manifest: state.manifest, manifestSha256: candidate.manifestSha256, env: state.env, fetchImpl, spawn });
  return { ...state, state, verify, fetchImpl, spawn };
}

test("authenticated supplement covers both hosts without changing native receipt bytes", async (t) => {
  const f = fixture(t); const before = JSON.stringify(f.candidate);
  const result = await f.verify();
  for (const host of ["macos", "windows"]) {
    const report = supplementalArchiveScan(result, f.candidate, f.manifest.units[0]);
    assert.equal(report.archiveSha256, f.manifest.units[0].sha256, host);
    report.status = "fail";
    assert.equal(supplementalArchiveScan(result, f.candidate, f.manifest.units[0]).status, "pass");
  }
  assert.equal(JSON.stringify(f.candidate), before);
  assert.throws(() => supplementalArchiveScan({}, f.candidate, f.manifest.units[0]), /unauthenticated/);
});
test("missing, failed, mismatched & malformed proof bindings fail closed", (t) => {
  const f = fixture(t);
  for (const mutate of [
    (p) => { delete p.original; }, (p) => { p.schema = 2; },
    (p) => { p.original.candidateId = "other"; }, (p) => { p.original.manifestSha256 = "1".repeat(64); },
    (p) => { p.producer.runAttempt = "1"; }, (p) => { p.policy.validatorSourceSha = SOURCE; },
    (p) => { p.units = []; }, (p) => { p.units[0].sha256 = "2".repeat(64); },
    (p) => { p.units[0].archiveScan.status = "fail"; }, (p) => { delete p.units[0].archiveScan; },
  ]) {
    const proof = structuredClone(f.proof); mutate(proof);
    assert.throws(() => validateSupplementProof(proof, f), /refused/);
  }
  assert.throws(() => validateSupplementPin({ ...f.pin, artifact: { ...f.pin.artifact, id: "10" } }, f.candidate), /alias/);
});
test("original expiry, producer provenance, job success & raw proof hashes remain mandatory", async (t) => {
  for (const mutate of [
    (f) => { f.artifacts.packed.expired = true; },
    (f) => { f.zip.packed = Buffer.from("changed original ZIP"); },
    (f) => { f.sourceJobs.jobs[0].run_attempt = 2; },
    (f) => { f.artifacts.supplement.expired = true; },
    (f) => { f.artifacts.supplement.workflow_run.head_repository_id = 66; },
    (f) => { f.run.conclusion = "failure"; }, (f) => { f.run.event = "pull_request"; },
    (f) => { f.run.head_sha = SOURCE; }, (f) => { f.run.run_attempt = 3; },
    (f) => { f.workflow.path = ".github/workflows/other.yml"; },
    (f) => { f.jobs.jobs[0].conclusion = "failure"; },
    (f) => { f.jobs.jobs[0].id = 101; }, (f) => { f.jobs.total_count = 2; },
    (f) => { f.comparison.status = "diverged"; },
    (f) => { f.zip.supplement = Buffer.from("changed supplement ZIP"); },
    (f) => { f.files.supplement["archive-scan-supplement.json"] = Buffer.from("changed proof"); },
    (f) => { f.env.GITHUB_REF = "refs/heads/topic"; },
  ]) {
    const f = fixture(t); mutate(f.state);
    await assert.rejects(f.verify(), /refused/);
  }
});
test("producer refuses registry credentials before fetching or scanning", async (t) => {
  const f = fixture(t);
  await assert.rejects(produceArchiveSupplement({ candidateId: f.candidate.id, env: { ...f.env, GITHUB_SHA: PRODUCER, GITHUB_WORKFLOW_REF: `${REPOSITORY}/${SUPPLEMENT_WORKFLOW}@refs/heads/main`, GITHUB_RUN_ID: "88", GITHUB_RUN_ATTEMPT: "2", NODE_AUTH_TOKEN: "fixture" }, fetchImpl: () => assert.fail("credentials must be refused first") }), /no registry/);
});
test("producer scans retained original bytes without repacking or native execution", async (t) => {
  const f = fixture(t);
  const approvedFile = path.join(f.env.RUNNER_TEMP, "approved-fixture.json");
  fs.writeFileSync(approvedFile, JSON.stringify({ schema: 2, candidates: [f.candidate] }));
  const before = Buffer.from(f.files.packed[f.manifest.units[0].file]);
  const owner = { packageFileViolations: () => [], scanCrateForSecrets: (dir) => { assert.equal(fs.readdirSync(dir).length, 1); return []; } };
  const result = await produceArchiveSupplement({ candidateId: f.candidate.id, approvedFile, env: { ...f.env, GITHUB_SHA: PRODUCER, GITHUB_WORKFLOW_REF: `${REPOSITORY}/${SUPPLEMENT_WORKFLOW}@refs/heads/main`, GITHUB_RUN_ID: "88", GITHUB_RUN_ATTEMPT: "2" }, fetchImpl: f.fetchImpl, spawn: f.spawn, scanArchive: (options) => scanArchiveForSecrets({ ...options, scannerOwner: owner }) });
  assert.equal(result.proof.units[0].archiveScan.archiveSha256, hash(before));
  assert.equal(result.proof.original.manifestSha256, f.candidate.manifestSha256);
  assert.deepEqual(f.files.packed[f.manifest.units[0].file], before);
  assert.equal(result.proofSha256, hash(fs.readFileSync(result.file)));
});

test("producer installs frozen scanner dependencies before scanning with lifecycle scripts disabled", () => {
  const document = parseYaml(fs.readFileSync(new URL("../../.github/workflows/supplement-archive-scan.yml", import.meta.url), "utf8"));
  const guard = (doc) => {
    const job = doc.jobs["archive-scan"];
    const steps = job.steps;
    const setup = steps.filter((step) => step.uses === "./.github/actions/rk-setup");
    assert.equal(setup.length, 1);
    assert.equal(setup[0].if, undefined);
    assert.deepEqual(setup[0].with, { "node-version": "26.8.1", "install-dependencies": "true", "install-filter": "--filter @rightkit/release...", "ignore-install-scripts": "true", rust: "false" });
    const checkoutIndex = steps.findIndex((step) => step.uses?.startsWith("actions/checkout@"));
    const setupIndex = steps.indexOf(setup[0]);
    const scanIndex = steps.findIndex((step) => step.run === "node scripts/ci/supplemental-proof.mjs produce");
    assert.ok(checkoutIndex >= 0 && setupIndex > checkoutIndex && scanIndex > setupIndex);
    assert.equal(steps[scanIndex].if, undefined);
    assert.ok(!JSON.stringify(job.env ?? {}).includes("runner.temp"));
  };
  guard(document);
  for (const mutate of [
    (doc) => { doc.jobs["archive-scan"].steps.splice(1, 1); },
    (doc) => { doc.jobs["archive-scan"].steps[1].with["install-filter"] = ""; },
    (doc) => { doc.jobs["archive-scan"].steps[1].with["ignore-install-scripts"] = "false"; },
    (doc) => { doc.jobs["archive-scan"].steps[1].with.rust = "true"; },
    (doc) => { const steps = doc.jobs["archive-scan"].steps; [steps[1], steps[2]] = [steps[2], steps[1]]; },
  ]) {
    const mutated = structuredClone(document); mutate(mutated);
    assert.throws(() => guard(mutated));
  }
});

 test("runtime pins share strict original binding & pin four distinct successful jobs", (t) => {
  const f = fixture(t);
  const pin = structuredClone(f.pin);
  pin.kind = "runtime-workload-pin";
  pin.policy.id = "runtime-workloads/v1";
  pin.producer.jobs = { candidate: "101", win32: "102", darwin: "103", qualify: pin.producer.jobId };
  validateSupplementPin(pin, f.candidate);
  for (const mutate of [
    (p) => { p.producer.jobs.darwin = p.producer.jobs.win32; },
    (p) => { delete p.producer.jobs.candidate; },
    (p) => { p.original.sourceSha = PRODUCER; },
    (p) => { p.artifact.proofSha256 = "claimed"; },
    (p) => { p.policy.id = SUPPLEMENT_POLICY; },
  ]) { const changed = structuredClone(pin); mutate(changed); assert.throws(() => validateSupplementPin(changed, f.candidate), /refused/); }
});
