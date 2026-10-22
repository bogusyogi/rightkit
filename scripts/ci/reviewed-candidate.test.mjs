import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { sha, executeUpload, checkReceipts, authenticatedArchiveScanResolver, CRATES_PUBLISH_URL, buildCratePublishBody } from "./upload.mjs";
import { validateCandidate, selectCandidate, selectArchiveSupplement, checkProvenance, prepareCandidate, assertAuthenticatedContext, authenticateAdmission, identifyCandidate, runReviewed, runQualified, QUALIFIED_ARTIFACTS } from "./reviewed-candidate.mjs";
import { SUPPLEMENT_WORKFLOW, SUPPLEMENT_POLICY } from "./supplemental-proof.mjs";
import { assertReviewedDoc, REVIEWED_MUTATIONS } from "./reviewed-workflow-contract.mjs";
import { parseYaml } from "./workflow-yaml.mjs";

const SOURCE = "a".repeat(40); const PROMOTION = "b".repeat(40);
const clone = (x) => structuredClone(x);
const archiveScanFixture = (unit) => ({ schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: unit.sha256, status: "pass", files: 1, violations: [], problems: [] });
// Central-directory fixtures are consumed only through mocked unzip; bytes still
// carry real directory metadata & are authenticated by their exact ZIP digest.
function proofArchive(files, modes = {}) {
  const rows = Object.entries(files).map(([name, bytes]) => {
    const nameBytes = Buffer.from(name), header = Buffer.alloc(46);
    header.writeUInt32LE(0x02014b50, 0); header.writeUInt16LE(3 << 8, 4);
    header.writeUInt32LE(bytes.length, 24); header.writeUInt16LE(nameBytes.length, 28);
    header.writeUInt32LE(((modes[name] ?? 0x81a4) << 16) >>> 0, 38);
    return Buffer.concat([header, nameBytes]);
  });
  const payload = Buffer.concat(Object.values(files)), directory = Buffer.concat(rows), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(rows.length, 8); end.writeUInt16LE(rows.length, 10); end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(payload.length, 16);
  return Buffer.concat([payload, directory, end]);
}
function fixture(t, mutate = () => {}, supplemental = false) {
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? process.env.RUNNER_TEMP ?? os.tmpdir();
  const temp = fs.realpathSync.native(fs.mkdtempSync(path.join(scratch, "rk-reviewed-")));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const crate = Buffer.from("original sealed archive including source Git SHA");
  const metadata = Buffer.from(JSON.stringify({ name: "rightkit-a", vers: "0.1.0", deps: [] }));
  const unit = { name: "rightkit-a", version: "0.1.0", file: "rightkit-a-0.1.0.crate", sha256: sha("sha256", crate), size: crate.length, metaFile: "rightkit-a-0.1.0.upload.json", metaSha256: sha("sha256", metadata) };
  const manifest = { schema: 1, registry: "crates", commit: SOURCE, runId: "77", runAttempt: "1", units: [unit] };
  const files = { packed: { [unit.file]: crate, [unit.metaFile]: metadata, "manifest.json": Buffer.from(JSON.stringify(manifest)) } };
  for (const host of ["macos", "windows"]) files[host] = { [`verify-${host}.json`]: Buffer.from(JSON.stringify({ schema: 1, kind: "verify", host, registry: "crates", commit: SOURCE, manifestSha256: sha("sha256", files.packed["manifest.json"]), runId: "77", runAttempt: "1", manifestRunId: "77", manifestRunAttempt: "1", allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [], units: [{ ...unit, archiveScan: archiveScanFixture(unit), bytesVerified: true, status: "pass", executed: 2, ignored: 0, skipped: 0, consumer: { verified: true } }] })) };
  const archives = Object.fromEntries(Object.keys(files).map((host) => [host, Buffer.from(`ZIP fixture ${host}`)]));
  const candidate = { id: "candidate-77", sourceSha: SOURCE, runId: "77", runAttempt: "1", workflowId: "5", manifestSha256: sha("sha256", files.packed["manifest.json"]), registry: "crates", units: ["rightkit-a"], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, artifacts: {} };
  const artifactNames = { packed: "publish-packed", macos: "publish-receipt-verify-xcode-27", windows: "publish-receipt-verify-windows-2025" };
  const artifacts = {};
  Object.keys(files).forEach((host, i) => {
    const id = String(100 + i); const digest = `sha256:${sha("sha256", archives[host])}`;
    candidate.artifacts[host] = { id, digest, ...(host !== "packed" ? { receiptSha256: sha("sha256", Object.values(files[host])[0]) } : {}) };
    artifacts[id] = { id: Number(id), name: artifactNames[host], expired: false, digest, workflow_run: { id: 77, head_sha: SOURCE, head_branch: "main", repository_id: 99, head_repository_id: 99 } };
  });
  const provenance = { candidate, repository: { id: 99, full_name: "bogusyogi/rightkit", default_branch: "main" }, workflow: { id: 5, path: ".github/workflows/publish.yml", state: "active" }, run: { id: 77, run_attempt: 1, repository: { id: 99, full_name: "bogusyogi/rightkit" }, head_repository: { id: 99, full_name: "bogusyogi/rightkit" }, workflow_id: 5, path: ".github/workflows/publish.yml", event: "workflow_dispatch", head_branch: "main", head_sha: SOURCE, status: "completed", conclusion: "failure" }, jobs: { total_count: 5, jobs: ["guard", "plan", "pack", "verify (xcode-27, 26.8.1)", "verify (windows-2025, 26.8.1)"].map((name) => ({ name, conclusion: "success", run_id: 77, run_attempt: 1, status: "completed", head_sha: SOURCE })) }, promotionSha: PROMOTION, comparison: { status: "ahead", merge_base_commit: { sha: SOURCE } } };
  const approval = { schema: 2, candidates: [candidate], artifacts: [{ registry: "crates", name: unit.name, version: unit.version, sha256: unit.sha256, reviewer: "Oracle fixture", reviewRef: "agent-review:fixture" }] };
  const state = { candidate, provenance, files, archives, artifacts, approval }; mutate(state);
  const supplements = { schema: 1, supplements: [] };
  if (supplemental) {
    const policy = { id: SUPPLEMENT_POLICY, sourceSha: PROMOTION, validatorSourceSha: PROMOTION };
    const original = { candidateId: candidate.id, candidateSha256: sha("sha256", JSON.stringify(candidate)), sourceSha: SOURCE, manifestSha256: candidate.manifestSha256, packedArtifact: candidate.artifacts.packed };
    const proof = { schema: 1, kind: "archive-scan-supplement", original, producer: { workflow: SUPPLEMENT_WORKFLOW, runId: "99", runAttempt: "1", sourceSha: PROMOTION }, policy, units: [{ name: unit.name, version: unit.version, file: unit.file, sha256: unit.sha256, archiveScan: archiveScanFixture(unit) }] };
    files.supplement = { "archive-scan-supplement.json": Buffer.from(JSON.stringify(proof)) };
    archives.supplement = Buffer.from("ZIP fixture supplement");
    const pin = { schema: 1, kind: "archive-scan-supplement-pin", original, producer: { workflowId: "6", runId: "99", runAttempt: "1", jobId: "9", sourceSha: PROMOTION }, artifact: { id: "200", digest: `sha256:${sha("sha256", archives.supplement)}`, proofSha256: sha("sha256", files.supplement["archive-scan-supplement.json"]) }, policy };
    supplements.supplements.push(pin);
    artifacts[200] = { id: 200, name: `archive-scan-supplement-${candidate.id}-1`, expired: false, digest: pin.artifact.digest, workflow_run: { id: 99, head_sha: PROMOTION, head_branch: "main", repository_id: 99, head_repository_id: 99 } };
    state.supplementRun = { ...clone(provenance.run), id: 99, workflow_id: 6, path: SUPPLEMENT_WORKFLOW, head_sha: PROMOTION, conclusion: "success" };
    state.supplementJobs = { total_count: 1, jobs: [{ id: 9, name: "archive-scan", run_id: 99, run_attempt: 1, head_sha: PROMOTION, status: "completed", conclusion: "success" }] };
    provenance.jobs.jobs.forEach((job, i) => Object.assign(job, { id: i + 1, run_attempt: 1, status: "completed" }));
  }
  const admission = { schema: 1, kind: "semantic-admission", original: { candidateId: candidate.id, candidateSha256: sha("sha256", JSON.stringify(candidate)), sourceSha: SOURCE, manifestSha256: candidate.manifestSha256, archives: manifest.units.map(({ name, version, file, sha256, metaFile, metaSha256 }) => ({ name, version, file, sha256, metaFile, metaSha256 })) }, sourceTree: "c".repeat(40), sourceHashes: { "Cargo.toml": "d".repeat(64) }, verifierSha: PROMOTION, runId: "88", runAttempt: "1", qa: Object.fromEntries(["macos", "windows"].map((host, i) => [host, { id: String(300 + i), digest: `sha256:${"e".repeat(64)}`, summarySha256: "f".repeat(64), unitEvidenceSha256: "a".repeat(64) }])), units: manifest.units.map((u) => ({ name: u.name, version: u.version, archiveSha256: u.sha256, ok: true, policies: { macos: "required-positive-tests", windows: "required-positive-tests" }, failures: [], adoptionPending: [], runtime: { status: "ok" }, runtimeFeatureProof: { classification: "false", listedOwner: false, required: false, status: "not-applicable", failures: [] }, runtimeInputHashes: {}, evidenceSha256: "b".repeat(64) })) };
  const promotionRun = { ...clone(provenance.run), id: 88, workflow_id: 7, path: ".github/workflows/publish-reviewed.yml", head_sha: PROMOTION, status: "in_progress" };
  const promotionJobs = { total_count: 1, jobs: [{ name: "seal-check", status: "completed", conclusion: "success", run_id: 88, run_attempt: 1, head_sha: PROMOTION }] };
  files.admission = { "admission.json": Buffer.from(JSON.stringify(admission)) };
  archives.admission = proofArchive(files.admission);
  artifacts[400] = { id: 400, name: "reviewed-admission-receipt", expired: false, digest: `sha256:${sha("sha256", archives.admission)}`, workflow_run: { id: 88, head_sha: PROMOTION, head_branch: "main", repository_id: 99, head_repository_id: 99 } };
  for (const [host, runner, id] of [["macos", "xcode-27", 300], ["windows", "windows-2025", 301]]) {
    files[`qa-${host}`] = { "summary.json": Buffer.from('{"results":[]}'), "unit-evidence.json": Buffer.from('{"units":[]}'), "logs/qualification.txt": Buffer.from("nested log is never extracted") };
    archives[`qa-${host}`] = proofArchive(files[`qa-${host}`]);
    artifacts[id] = { id, name: `publish-verify-${runner}-1`, expired: false, digest: `sha256:${sha("sha256", archives[`qa-${host}`])}`, workflow_run: { id: 77, head_sha: SOURCE, head_branch: "main", repository_id: 99, head_repository_id: 99 } };
  }
  const qaListing = { total_count: 2, artifacts: [artifacts[300], artifacts[301]] };

  const approvedFile = path.join(temp, "approved-artifacts.json"); fs.writeFileSync(approvedFile, JSON.stringify(approval));
  const supplementalFile = path.join(temp, "supplemental-proofs.json"); fs.writeFileSync(supplementalFile, JSON.stringify(supplements));
  const requests = []; let published = false; const puts = [];
  const fetchImpl = async (url, options = {}) => {
    const text = String(url); requests.push({ url: text, options });
    if (text === "https://api.github.com/repos/bogusyogi/rightkit" || text.startsWith("https://api.github.com/repos/bogusyogi/rightkit/")) {
      const route = text.slice("https://api.github.com/repos/bogusyogi/rightkit".length).replace(/^\//, "");
      let doc;
      if (route === "") doc = provenance.repository;
      else if (route === "actions/workflows/publish.yml") doc = provenance.workflow;
      else if (route === "actions/workflows/publish-reviewed.yml") doc = { id: 7, path: ".github/workflows/publish-reviewed.yml", state: "active" };
      else if (route === "actions/runs/88/attempts/1") doc = promotionRun;
      else if (route === "actions/runs/88/attempts/1/jobs?per_page=100") doc = promotionJobs;
      else if (route === "actions/workflows/supplement-archive-scan.yml") doc = { id: 6, path: SUPPLEMENT_WORKFLOW, state: "active" };
      else if (route === "actions/runs/99/attempts/1") doc = state.supplementRun;
      else if (route === "actions/runs/99/attempts/1/jobs?per_page=100") doc = state.supplementJobs;
      else if (route === "actions/runs/77/artifacts?per_page=100&page=1") doc = qaListing;
      else if (route === "actions/runs/77/attempts/1") doc = provenance.run;
      else if (route === "actions/runs/77/attempts/1/jobs?per_page=100") doc = provenance.jobs;
      else if (route === `compare/${PROMOTION}...${PROMOTION}`) doc = { status: "identical", merge_base_commit: { sha: PROMOTION } };
      else if (route.startsWith("compare/")) doc = provenance.comparison;
      else if (/^actions\/artifacts\/\d+$/.test(route)) doc = artifacts[route.split("/")[2]];
      else if (/^actions\/artifacts\/\d+\/zip$/.test(route)) { const host = route.includes("/300/") ? "qa-macos" : route.includes("/301/") ? "qa-windows" : route.includes("/400/") ? "admission" : route.includes("/200/") ? "supplement" : Object.keys(candidate.artifacts).find((h) => candidate.artifacts[h].id === route.split("/")[2]); return { ok: true, status: 200, arrayBuffer: async () => archives[host] }; }
      else throw new Error(`unexpected API route ${route}`);
      return { ok: true, status: 200, json: async () => doc };
    }
    if (text === CRATES_PUBLISH_URL) { published = true; puts.push(options); return { ok: true, status: 200, text: async () => '{"warnings":{}}' }; }
    if (text.startsWith("https://index.crates.io/")) return { ok: published, status: published ? 200 : 404, text: async () => JSON.stringify({ vers: unit.version, cksum: unit.sha256 }) };
    throw new Error(`unexpected network ${text}`);
  };
  const spawn = (command, args) => {
    assert.equal(command, "unzip", "candidate code, build, install & registry children never execute");
    const host = path.basename(args[1]).replace(/\.zip$/, "");
    return { status: 0, stdout: args[0] === "-Z1" ? Object.keys(files[host]).join("\n") : files[host][args[2]] };
  };
  const env = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_REF: "refs/heads/main", GITHUB_SHA: PROMOTION, GITHUB_RUN_ID: "88", GITHUB_RUN_ATTEMPT: "1", GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/publish-reviewed.yml@refs/heads/main", GITHUB_TOKEN: "github-read-fixture", RUNNER_TEMP: temp, PUBLISH_CANDIDATE_ID: candidate.id, PUBLISH_ADMISSION_ID: "400", PUBLISH_ADMISSION_DIGEST: artifacts[400].digest };
  const prepareOriginal = () => prepareCandidate({ env, candidateId: candidate.id, approvedFile, supplementalFile, fetchImpl, spawn });
  const prepare = async () => { const prepared = await prepareOriginal(); await authenticateAdmission({ prepared, env, fetchImpl, spawn }); return prepared; };
  const resealAdmission = () => { files.admission["admission.json"] = Buffer.from(JSON.stringify(admission)); archives.admission = proofArchive(files.admission); artifacts[400].digest = `sha256:${sha("sha256", archives.admission)}`; env.PUBLISH_ADMISSION_DIGEST = artifacts[400].digest; };
  const upload = async (prepared, dryRun) => executeUpload({ env: { ...env, PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: candidate.units.join(" "), ...(dryRun ? {} : { CARGO_REGISTRY_TOKEN: "registry-fixture" }) }, packDir: prepared.packDir, receiptsDir: prepared.receiptsDir, candidateContext: prepared.context, approvedFile, outDir: path.join(temp, `out-${dryRun}`), dryRun, fetchImpl, spawn, pause: 0, log: () => {} });
  return { ...state, temp, approvedFile, supplementalFile, supplements, manifest, env, requests, puts, prepare, prepareOriginal, upload, crate, metadata, admission, promotionRun, promotionJobs, qaListing, resealAdmission, fetchImpl, spawn };
}

test("promotion publishes original source archive after approval commit without rebuilding or spoofing identity", async (t) => {
  const f = fixture(t); const prepared = await f.prepare();
  const dry = await f.upload(prepared, true); assert.equal(f.puts.length, 0);
  const actual = await f.upload(prepared, false);
  assert.equal(actual.commit, SOURCE); assert.equal(actual.promotionCommit, PROMOTION); assert.equal(actual.runId, "88"); assert.equal(actual.candidate.runId, "77");
  assert.deepEqual(f.puts[0].body, buildCratePublishBody(f.metadata.toString(), f.crate));
  assert.equal(dry.receipts[0].sha256, actual.receipts[0].sha256);
  assert.ok(f.requests.filter((r) => r.url.startsWith("https://api.github.com/")).every((r) => r.options.headers.authorization === "Bearer github-read-fixture"));
});

test("source provenance refuses wrong repo, workflow, ref, SHA, attempt, event & failed qualification", (t) => {
  const f = fixture(t); checkProvenance(f.provenance);
  const mutations = [
    (p) => { p.repository.full_name = "foreign/rightkit"; }, (p) => { p.run.head_repository.id = 123; },
    (p) => { p.workflow.id = 6; }, (p) => { p.run.workflow_id = 6; }, (p) => { p.run.path = ".github/workflows/other.yml"; },
    (p) => { p.run.head_branch = "topic"; }, (p) => { p.run.head_sha = PROMOTION; }, (p) => { p.run.run_attempt = 2; },
    (p) => { p.run.event = "pull_request"; }, (p) => { p.jobs.jobs[3].conclusion = "failure"; },
    (p) => { p.jobs.jobs[3].run_attempt = 2; },
    (p) => { p.jobs.jobs.pop(); }, (p) => { p.jobs.jobs[0].conclusion = "skipped"; },
    (p) => { p.comparison.status = "diverged"; }, (p) => { p.promotionSha = SOURCE; },
  ];
  for (const mutate of mutations) { const p = clone(f.provenance); mutate(p); assert.throws(() => checkProvenance(p), /refused/); }
});

test("artifact ID, ownership, digest, raw manifest & raw receipt tampering fail before registry access", async (t) => {
  for (const mutate of [
    (s) => { s.artifacts[100].id = 123; }, (s) => { s.artifacts[100].workflow_run.head_repository_id = 123; },
    (s) => { s.artifacts[100].digest = `sha256:${"c".repeat(64)}`; }, (s) => { s.archives.packed = Buffer.from("tampered ZIP"); },
    (s) => { s.files.packed["manifest.json"] = Buffer.from("{}"); },
    (s) => { s.files.windows["verify-windows.json"] = Buffer.from("{}"); },
  ]) {
    const f = fixture(t, mutate); await assert.rejects(f.prepare(), /refused/);
    assert.equal(f.requests.filter((r) => !r.url.startsWith("https://api.github.com/")).length, 0);
  }
});

test("approval refusal, missing host, failed consumer & widened allowsets remain blockers", async (t) => {
  for (const mutate of [
    (s) => { s.approval.artifacts = []; }, (s) => { s.approval.artifacts[0].sha256 = "c".repeat(64); },
    (s) => { s.approval.artifacts[0].reviewer = ""; }, (s) => { s.approval.artifacts.push(clone(s.approval.artifacts[0])); },
    (s) => { s.candidate.allow.ignored = ["rightkit-a"]; },
    (s) => { const receipt = JSON.parse(s.files.windows["verify-windows.json"]); receipt.units[0].consumer.verified = false; s.files.windows["verify-windows.json"] = Buffer.from(JSON.stringify(receipt)); s.candidate.artifacts.windows.receiptSha256 = sha("sha256", s.files.windows["verify-windows.json"]); },
    (s) => { delete s.candidate.artifacts.windows; },
  ]) { const f = fixture(t, mutate); await assert.rejects(f.prepare(), /refused/); assert.equal(f.puts.length, 0); }
});

test("authenticated context cannot be forged, widened, or reused after tampering in either mode", async (t) => {
  const f = fixture(t); const p = await f.prepare();
  assert.throws(() => assertAuthenticatedContext({ candidate: f.candidate }, {}), /unauthenticated/);
  assert.throws(() => selectCandidate(f.approvedFile, "https://example.test/candidate"), /URLs & paths/);
  const invalid = clone(f.candidate); invalid.units.push("../escape"); assert.throws(() => validateCandidate(invalid), /invalid unit/);
  fs.appendFileSync(path.join(p.packDir, "rightkit-a-0.1.0.crate"), "tampered");
  for (const dryRun of [true, false]) await assert.rejects(f.upload(p, dryRun), /changed after packing/);
  assert.equal(f.puts.length, 0);
});

test("promotion uploader repeats every reviewer-list refusal in dry & real modes", async (t) => {
  for (const mutate of [
    (doc) => { doc.artifacts = []; },
    (doc) => { doc.artifacts[0].sha256 = "c".repeat(64); },
    (doc) => { doc.artifacts[0].reviewer = ""; },
    (doc) => { doc.artifacts[0].reviewRef = ""; },
    (doc) => { doc.artifacts[0].version = "0.2.0"; },
    (doc) => { doc.artifacts[0].registry = "npm"; },
    (doc) => { doc.artifacts.push(clone(doc.artifacts[0])); },
    (doc) => { doc.artifacts[0].extra = "unexpected"; },
    (doc) => { doc.schema = 99; },
  ]) {
    const f = fixture(t); const p = await f.prepare(); const doc = clone(f.approval); mutate(doc);
    fs.writeFileSync(f.approvedFile, JSON.stringify(doc));
    for (const dryRun of [true, false]) await assert.rejects(f.upload(p, dryRun), /sealed upload refused/);
    assert.equal(f.puts.length, 0);
  }
});

test("reviewed workflow trust boundary & every declared mutation are checked structurally", () => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8"));
  assert.equal(assertReviewedDoc(doc), true);
  for (const [name, mutate] of REVIEWED_MUTATIONS) { const changed = clone(doc); mutate(changed); assert.throws(() => assertReviewedDoc(changed), undefined, name); }
});

function historicalReceipts(state, value = undefined) {
  for (const host of ["macos", "windows"]) {
    const file = `verify-${host}.json`;
    const doc = JSON.parse(state.files[host][file]);
    if (value === undefined) delete doc.units[0].archiveScan;
    else doc.units[0].archiveScan = value;
    state.files[host][file] = Buffer.from(JSON.stringify(doc));
    state.candidate.artifacts[host].receiptSha256 = sha("sha256", state.files[host][file]);
  }
}

test("historical missing scan accepts only authenticated committed supplement & preserves receipts", async (t) => {
  const f = fixture(t, historicalReceipts, true);
  const p = await f.prepare();
  for (const dryRun of [true, false]) await f.upload(p, dryRun);
  for (const host of ["macos", "windows"]) assert.deepEqual(fs.readFileSync(path.join(p.receiptsDir, host, `verify-${host}.json`)), f.files[host][`verify-${host}.json`]);
  assert.deepEqual(f.puts[0].body, buildCratePublishBody(f.metadata.toString(), f.crate));
  const native = path.join(p.receiptsDir, "windows", "verify-windows.json");
  fs.appendFileSync(native, " ");
  await assert.rejects(f.upload(p, false), /original native receipt changed/);
});

test("absent scan without supplement & present null, malformed or failed scan always refuse", async (t) => {
  const missing = fixture(t, historicalReceipts);
  await assert.rejects(missing.prepare(), /archiveScan/);
  for (const value of [null, {}, { status: "fail" }, { ...archiveScanFixture({ sha256: "a".repeat(64) }), status: "fail" }]) {
    const f = fixture(t, (s) => historicalReceipts(s, value), true);
    await assert.rejects(f.prepare(), /archiveScan/);
    assert.equal(f.puts.length, 0);
  }
});

test("supplement pins reject duplicate, unknown, malformed & unbound collection rows", (t) => {
  const f = fixture(t, historicalReceipts, true);
  assert.deepEqual(selectArchiveSupplement(f.supplementalFile, f.candidate, f.approvedFile), f.supplements.supplements[0]);
  for (const mutate of [
    (d) => { d.extra = true; }, (d) => { d.schema = 2; }, (d) => { d.supplements = null; },
    (d) => { d.supplements.push(clone(d.supplements[0])); },
    (d) => { d.supplements[0].extra = true; },
    (d) => { d.supplements[0].original.candidateId = "unknown"; },
    (d) => { d.supplements[0].original.manifestSha256 = "c".repeat(64); },
  ]) {
    const doc = clone(f.supplements); mutate(doc); fs.writeFileSync(f.supplementalFile, JSON.stringify(doc));
    assert.throws(() => selectArchiveSupplement(f.supplementalFile, f.candidate, f.approvedFile), /refused/);
  }
});

test("unauthenticated scan JSON, booleans & callbacks cannot bypass receipt or upload checks", async (t) => {
  const f = fixture(t, historicalReceipts, true);
  for (const evidence of [true, {}, f.supplements.supplements[0]]) await assert.rejects(authenticatedArchiveScanResolver(evidence, f.candidate, f.manifest), /unauthenticated/);
  const p = await f.prepare();
  const options = { manifest: f.manifest, manifestSha256: f.candidate.manifestSha256, receiptsDir: p.receiptsDir, run: { runId: "77", runAttempt: "1" } };
  for (const archiveScanResolver of [true, {}, () => archiveScanFixture(f.manifest.units[0])]) assert.ok(checkReceipts({ ...options, archiveScanResolver }).some((problem) => /unauthenticated/.test(problem)));
  await assert.rejects(f.upload({ ...p, context: clone(p.context) }, true), /unauthenticated candidate context/);
  for (const archiveScanResolver of [true, {}, () => archiveScanFixture(f.manifest.units[0])]) {
    await assert.rejects(executeUpload({ env: { ...f.env, GITHUB_SHA: SOURCE, GITHUB_RUN_ID: "77", PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: f.candidate.units.join(" ") }, packDir: p.packDir, receiptsDir: p.receiptsDir, approvedFile: f.approvedFile, archiveScanResolver, outDir: path.join(f.temp, "forged"), dryRun: true, log: () => {} }), /archiveScan/);
  }
  f.supplementRun.conclusion = "failure";
  await assert.rejects(f.prepare(), /producer run provenance\/success/);
});

test("supplement preserves native runtime, consumer & closure blockers", async (t) => {
  for (const mutate of [
    (row) => { row.consumer.verified = false; },
    (row) => { row.executed = 0; },
    (row) => { row.status = "fail"; },
    (row) => { row.bytesVerified = false; },
  ]) {
    const f = fixture(t, (s) => {
      historicalReceipts(s);
      const doc = JSON.parse(s.files.windows["verify-windows.json"]); mutate(doc.units[0]);
      s.files.windows["verify-windows.json"] = Buffer.from(JSON.stringify(doc));
      s.candidate.artifacts.windows.receiptSha256 = sha("sha256", s.files.windows["verify-windows.json"]);
    }, true);
    await assert.rejects(f.prepare(), /refused/);
    assert.equal(f.puts.length, 0);
  }
});


test("unadmitted original context cannot reach real registry upload", async (t) => {
  const f = fixture(t), prepared = await f.prepareOriginal();
  await assert.rejects(f.upload(prepared, false), /semantic admission is required/);
  assert.equal(f.puts.length, 0);
});

test("identify authenticates source before exposing checkout SHA", async (t) => {
  const f = fixture(t); f.env.GITHUB_OUTPUT = path.join(f.temp, "output");
  assert.deepEqual(await identifyCandidate({ env: f.env, candidateId: f.candidate.id, approvedFile: f.approvedFile, fetchImpl: f.fetchImpl }), { sourceSha: SOURCE, registry: "crates" });
  assert.equal(fs.readFileSync(f.env.GITHUB_OUTPUT, "utf8"), `source_sha=${SOURCE}\nregistry=crates\n`);
  assert.equal(f.requests.some((r) => /artifacts/.test(r.url)), false);
  f.provenance.run.head_sha = PROMOTION;
  await assert.rejects(identifyCandidate({ env: f.env, candidateId: f.candidate.id, approvedFile: f.approvedFile, fetchImpl: f.fetchImpl }), /source event/);
});

test("fresh admission transfer rejects changed identity, provenance & unsuccessful seal", async (t) => {
  for (const mutate of [
    (f) => { f.env.PUBLISH_ADMISSION_ID = "0"; },
    (f) => { f.env.PUBLISH_ADMISSION_DIGEST = "SHA256:" + "e".repeat(64); },
    (f) => { f.promotionRun.head_sha = SOURCE; },
    (f) => { f.promotionRun.event = "pull_request"; },
    (f) => { f.promotionRun.head_repository.id = 123; },
    (f) => { f.promotionJobs.jobs[0].conclusion = "failure"; },
    (f) => { f.promotionJobs.jobs[0].run_attempt = 2; },
    (f) => { f.promotionJobs.jobs.push(clone(f.promotionJobs.jobs[0])); f.promotionJobs.total_count++; },
    (f) => { f.artifacts[400].workflow_run.id = 77; },
    (f) => { f.artifacts[400].expired = true; },
    (f) => { f.artifacts[400].name = "other"; },
    (f) => { f.archives.admission = Buffer.from("tampered"); },
  ]) {
    const f = fixture(t), prepared = await f.prepareOriginal(); mutate(f);
    await assert.rejects(authenticateAdmission({ prepared, env: f.env, fetchImpl: f.fetchImpl, spawn: f.spawn }), /refused/);
    assert.equal(f.puts.length, 0);
  }
});

test("authenticated admission contents bind every original unit, proof & verifier", async (t) => {
  for (const mutate of [
    (a) => { a.original.candidateSha256 = "f".repeat(64); },
    (a) => { a.original.manifestSha256 = "f".repeat(64); },
    (a) => { a.original.archives[0].sha256 = "f".repeat(64); },
    (a) => { a.verifierSha = SOURCE; },
    (a) => { a.runAttempt = "2"; },
    (a) => { a.sourceTree = ""; },
    (a) => { a.sourceHashes = {}; },
    (a) => { delete a.qa.windows; },
    (a) => { a.qa.windows.id = a.qa.macos.id; },
    (a) => { a.qa.windows.summarySha256 = ""; },
    (a) => { a.units = []; },
    (a) => { a.units.push(clone(a.units[0])); },
    (a) => { a.units[0].ok = false; },
    (a) => { delete a.units[0].policies.windows; },
    (a) => { a.units[0].runtime = null; },
    (a) => { a.units[0].runtimeInputHashes = { "../escape": "a".repeat(64) }; },
    (a) => { a.units[0].failures = ["missing obligations"]; },
    (a) => { a.units[0].archiveSha256 = "c".repeat(64); },
    (a) => { a.units[0].evidenceSha256 = ""; },
    (a) => { a.extra = true; },
  ]) {
    const f = fixture(t); mutate(f.admission); f.resealAdmission();
    await assert.rejects(f.prepare(), /refused/); assert.equal(f.puts.length, 0);
  }
});

test("transfer ZIP refuses extra paths, links & nested admission before extraction", async (t) => {
  for (const variant of ["extra", "link", "nested", "traversal"]) {
    const f = fixture(t), files = f.files.admission;
    if (variant === "extra") files["other.json"] = Buffer.from("{}");
    if (variant === "nested") { files["logs/admission.json"] = files["admission.json"]; delete files["admission.json"]; }
    if (variant === "traversal") files["../escape"] = Buffer.from("{}");
    f.archives.admission = proofArchive(files, variant === "link" ? { "admission.json": 0xa1ff } : {});
    f.artifacts[400].digest = `sha256:${sha("sha256", f.archives.admission)}`; f.env.PUBLISH_ADMISSION_DIGEST = f.artifacts[400].digest;
    await assert.rejects(f.prepare(), /refused/); assert.equal(f.puts.length, 0);
  }
});

test("semantic & transfer verification refuse registry credentials before API", async (t) => {
  for (const key of ["NODE_AUTH_TOKEN", "NPM_TOKEN", "CARGO_REGISTRY_TOKEN"]) {
    const f = fixture(t); f.env[key] = "forbidden";
    for (const mode of [{ dryRun: true }, { verifyAdmission: true }]) await assert.rejects(runReviewed({ env: f.env, ...mode, approvedFile: f.approvedFile, supplementalFile: f.supplementalFile, fetchImpl: f.fetchImpl, spawn: f.spawn }), /secretless/);
    assert.equal(f.requests.length, 0);
  }
});

test("secretless semantic admission refuses OIDC availability before API", async (t) => {
  for (const key of ["ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN"]) {
    const f = fixture(t); f.env[key] = "forbidden";
    await assert.rejects(runReviewed({ env: f.env, dryRun: true, approvedFile: f.approvedFile, supplementalFile: f.supplementalFile, fetchImpl: f.fetchImpl, spawn: f.spawn }), /secretless/);
    assert.equal(f.requests.length, 0);
  }
});

test("credential-free transfer verification exercises fresh job without upload", async (t) => {
  const f = fixture(t);
  const result = await runReviewed({ env: f.env, verifyAdmission: true, approvedFile: f.approvedFile, supplementalFile: f.supplementalFile, fetchImpl: f.fetchImpl, spawn: f.spawn });
  assert.equal(result.original.candidateId, f.candidate.id); assert.equal(f.puts.length, 0);
  const transfer = JSON.parse(fs.readFileSync(path.join(f.temp, "publication-admission-transfer", "admission-transfer.json"), "utf8"));
  assert.equal(transfer.upload, false); assert.equal(transfer.kind, "semantic-admission-transfer"); assert.equal(transfer.candidateSha256, result.original.candidateSha256);
});

test("fresh-job transfer then publication keep exact, separate artifact contents", async (t) => {
  const f = fixture(t);
  const options = { approvedFile: f.approvedFile, supplementalFile: f.supplementalFile, fetchImpl: f.fetchImpl, spawn: f.spawn };
  const admitted = await runReviewed({ ...options, env: f.env, verifyAdmission: true });
  const transferDir = path.join(f.temp, "publication-admission-transfer");
  const publicationDir = path.join(f.temp, "publication-receipt");
  assert.deepEqual(fs.readdirSync(transferDir), ["admission-transfer.json"]);
  assert.equal(fs.existsSync(publicationDir), false);
  assert.equal(f.puts.length, 0);
  const transferBytes = fs.readFileSync(path.join(transferDir, "admission-transfer.json"));
  const published = await runReviewed({ ...options, env: { ...f.env, CARGO_REGISTRY_TOKEN: "registry-fixture" } });
  assert.equal(f.puts.length, 1);
  assert.deepEqual(fs.readdirSync(transferDir), ["admission-transfer.json"]);
  assert.deepEqual(fs.readFileSync(path.join(transferDir, "admission-transfer.json")), transferBytes);
  assert.deepEqual(fs.readdirSync(publicationDir), ["publication-receipt.json"]);
  const receipt = JSON.parse(fs.readFileSync(path.join(publicationDir, "publication-receipt.json"), "utf8"));
  assert.deepEqual(receipt, published);
  assert.equal(receipt.dryRun, false);
  assert.equal(receipt.candidate.id, admitted.original.candidateId);
  assert.equal(receipt.receipts[0].published, true);
  assert.equal(receipt.receipts[0].registryVerified, true);
});


test("fresh transfer tolerates job OIDC availability without requesting or using it", async (t) => {
  const f = fixture(t);
  f.env.ACTIONS_ID_TOKEN_REQUEST_URL = "https://oidc.invalid/never-request";
  f.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN = "unused-oidc-fixture";
  const result = await runReviewed({ env: f.env, verifyAdmission: true, approvedFile: f.approvedFile, supplementalFile: f.supplementalFile, fetchImpl: f.fetchImpl, spawn: f.spawn });
  assert.equal(result.original.candidateId, f.candidate.id);
  assert.equal(f.puts.length, 0);
  assert.ok(f.requests.every((request) => request.url.startsWith("https://api.github.com/") && request.options.headers.authorization === "Bearer github-read-fixture"));
  const transfer = JSON.parse(fs.readFileSync(path.join(f.temp, "publication-admission-transfer", "admission-transfer.json"), "utf8"));
  assert.equal(transfer.upload, false);
});

test("secretless original QA enforces unique host artifacts, digest, run & safe exact proofs", async (t) => {
  for (const mutate of [
    (f) => { f.qaListing.artifacts.pop(); f.qaListing.total_count = 1; },
    (f) => { f.qaListing.artifacts.push(clone(f.artifacts[300])); f.qaListing.total_count++; },
    (f) => { f.qaListing.total_count = 101; },
    (f) => { f.artifacts[300].expired = true; },
    (f) => { f.artifacts[300].workflow_run.head_sha = PROMOTION; },
    (f) => { f.archives["qa-macos"] = Buffer.from("changed ZIP"); },
    (f) => { delete f.files["qa-macos"]["summary.json"]; f.archives["qa-macos"] = proofArchive(f.files["qa-macos"]); f.artifacts[300].digest = `sha256:${sha("sha256", f.archives["qa-macos"])}`; },
    (f) => { f.archives["qa-macos"] = proofArchive(f.files["qa-macos"], { "summary.json": 0xa1ff }); f.artifacts[300].digest = `sha256:${sha("sha256", f.archives["qa-macos"])}`; },
  ]) {
    const f = fixture(t); mutate(f);
    await assert.rejects(runReviewed({ env: f.env, dryRun: true, approvedFile: f.approvedFile, supplementalFile: f.supplementalFile, fetchImpl: f.fetchImpl, spawn: f.spawn }), /refused/);
    assert.equal(f.puts.length, 0);
  }
});

test("secretless QA streams only original proof files & still requires separate source checkout", async (t) => {
  const f = fixture(t), reads = [];
  const spawn = (command, args, options) => { reads.push(args); return f.spawn(command, args, options); };
  await assert.rejects(runReviewed({ env: f.env, dryRun: true, approvedFile: f.approvedFile, supplementalFile: f.supplementalFile, fetchImpl: f.fetchImpl, spawn }), /candidate directory must be absolute/);
  const qaReads = reads.filter((args) => /qa-/.test(args[1]));
  assert.deepEqual(qaReads.map((args) => args[2]), ["summary.json", "unit-evidence.json", "summary.json", "unit-evidence.json"]);
  assert.equal(f.puts.length, 0);
});


test("transfer accepts upload-artifact raw SHA-256 output & canonical GitHub digest", async (t) => {
  for (const raw of [true, false]) {
    const f = fixture(t); if (raw) f.env.PUBLISH_ADMISSION_DIGEST = f.env.PUBLISH_ADMISSION_DIGEST.slice(7);
    const prepared = await f.prepare(); assert.ok(prepared.context);
  }
});


test("producer-shaped noncontinuous & held-resource runtime proofs survive authenticated transfer", async (t) => {
  for (const classification of ["false", "held-resource"]) {
    const f = fixture(t), row = f.admission.units[0];
    row.runtimeFeatureProof = { classification, listedOwner: false, required: false, status: "not-applicable", failures: [] };
    if (classification === "held-resource") row.adoptionPending = ["held-resource structural publication check only; installed-app runtime measurement remains pending"];
    f.resealAdmission();
    const prepared = await f.prepare();
    await f.upload(prepared, true);
    assert.equal(f.puts.length, 0);
  }
});

test("runtime feature proof missing, HOLD, required & incoherent classifications refuse transfer", async (t) => {
  for (const mutate of [
    (row) => { delete row.runtimeFeatureProof; },
    (row) => { row.runtimeFeatureProof = null; },
    (row) => { row.runtimeFeatureProof.classification = "always-on"; row.runtimeFeatureProof.required = true; row.runtimeFeatureProof.status = "HOLD"; row.runtimeFeatureProof.failures = ["authenticated isolated runtime feature/workload proof missing"]; },
    (row) => { row.runtimeFeatureProof.required = true; },
    (row) => { row.runtimeFeatureProof.status = "HOLD"; },
    (row) => { row.runtimeFeatureProof.listedOwner = true; },
    (row) => { row.runtimeFeatureProof.classification = "always-on"; },
    (row) => { row.runtimeFeatureProof.classification = "undeclared"; },
    (row) => { row.runtimeFeatureProof.classification = false; },
    (row) => { row.runtimeFeatureProof.required = "false"; },
    (row) => { row.runtimeFeatureProof.failures = ["missing runtime workload proof"]; },
    (row) => { row.runtimeFeatureProof.extra = true; },
    (row) => { row.runtimeFeatureProof.classification = "held-resource"; row.runtime = null; },
  ]) {
    const f = fixture(t); mutate(f.admission.units[0]); f.resealAdmission();
    await assert.rejects(f.prepare(), /refused/);
    assert.equal(f.puts.length, 0);
  }
});

// This evaluates only the exact expression grammar used by this workflow. It is
// deliberately not a shell or a general GitHub Actions expression interpreter.
function routedRegistry(doc, stepOutputs) {
  const expression = doc.jobs["seal-check"].outputs.registry;
  const match = /^\$\{\{\s*steps\.([a-z][a-z0-9-]*)\.outputs\.([a-z][a-z0-9_]*)\s*\}\}$/.exec(expression ?? "");
  assert.ok(match, "unsupported registry output expression");
  assert.equal(doc.jobs["seal-check"].steps.filter((step) => step.id === match[1]).length, 1, "registry producer must exist exactly once");
  const registry = stepOutputs[match[1]]?.[match[2]];
  assert.ok(["npm", "crates"].includes(registry), "registry producer did not emit an authenticated registry");
  return registry;
}

function selectedUploadSteps(doc, stepOutputs, dryRun) {
  const registry = routedRegistry(doc, stepOutputs);
  const steps = doc.jobs.upload.steps.filter((step) => step.env?.NODE_AUTH_TOKEN || step.env?.CARGO_REGISTRY_TOKEN);
  assert.equal(steps.length, 2, "exactly two credential-bearing upload routes are required");
  return steps.filter((step) => {
    const match = /^\$\{\{\s*!inputs\.dry_run\s*&&\s*needs\.seal-check\.outputs\.registry\s*==\s*'(npm|crates)'\s*\}\}$/.exec(step.if ?? "");
    assert.ok(match, "unsupported upload routing expression");
    return !dryRun && registry === match[1];
  });
}

function identifyWorkflowFixture(t, registry) {
  return fixture(t, (state) => {
    if (registry === "crates") return;
    assert.equal(registry, "npm");
    const manifest = JSON.parse(state.files.packed["manifest.json"]);
    const unit = manifest.units[0], oldFile = unit.file;
    delete state.files.packed[unit.metaFile]; delete unit.metaFile; delete unit.metaSha256;
    unit.name = "@rightkit/a"; unit.file = "rightkit-a-0.1.0.tgz";
    state.files.packed[unit.file] = state.files.packed[oldFile]; delete state.files.packed[oldFile];
    manifest.registry = "npm";
    state.files.packed["manifest.json"] = Buffer.from(JSON.stringify(manifest));
    state.candidate.registry = "npm"; state.candidate.units = [unit.name];
    state.candidate.manifestSha256 = sha("sha256", state.files.packed["manifest.json"]);
    Object.assign(state.approval.artifacts[0], { registry: "npm", name: unit.name });
    for (const host of ["macos", "windows"]) {
      const name = `verify-${host}.json`, receipt = JSON.parse(state.files[host][name]);
      receipt.registry = "npm"; receipt.manifestSha256 = state.candidate.manifestSha256;
      receipt.units[0] = { ...receipt.units[0], ...unit };
      delete receipt.units[0].metaFile; delete receipt.units[0].metaSha256;
      state.files[host][name] = Buffer.from(JSON.stringify(receipt));
      state.candidate.artifacts[host].receiptSha256 = sha("sha256", state.files[host][name]);
    }
  });
}

function runWorkflowIdentify(doc, f, apiMutation = () => {}) {
  const steps = doc.jobs["seal-check"].steps.filter((step) => step.id === "identify");
  assert.equal(steps.length, 1);
  // Execute the actual YAML command tokens, mapping its verifier checkout to
  // this primary checkout. No shell expansion or replacement producer is used.
  const command = /^node (verifier\/scripts\/ci\/reviewed-candidate\.mjs) (identify)$/.exec(steps[0].run ?? "");
  assert.ok(command, "unsupported identify command");
  const root = fileURLToPath(new URL("../../", import.meta.url));
  const script = path.resolve(root, command[1].slice("verifier/".length));
  const routes = {
    "": f.provenance.repository,
    "actions/workflows/publish.yml": f.provenance.workflow,
    "actions/runs/77/attempts/1": f.provenance.run,
    "actions/runs/77/attempts/1/jobs?per_page=100": f.provenance.jobs,
    [`compare/${SOURCE}...${PROMOTION}`]: f.provenance.comparison,
  };
  const state = { approvedFile: f.approvedFile, canonical: path.resolve(root, "ci/approved-artifacts.json"), routes, status: 200 };
  apiMutation(state);
  const dataFile = path.join(f.temp, "identify-fixture.json"), preload = path.join(f.temp, "identify-preload.mjs");
  fs.writeFileSync(dataFile, JSON.stringify(state));
  fs.writeFileSync(preload, `import fs from "node:fs";\nimport path from "node:path";\nconst read = fs.readFileSync.bind(fs);\nconst data = JSON.parse(read(process.env.RK_IDENTIFY_FIXTURE, "utf8"));\nfs.readFileSync = (file, ...args) => read(typeof file === "string" && path.resolve(file) === data.canonical ? data.approvedFile : file, ...args);\nglobalThis.fetch = async (url, options) => {\n  const prefix = "https://api.github.com/repos/bogusyogi/rightkit";\n  const text = String(url);\n  if (!(text === prefix || text.startsWith(prefix + "/")) || options.headers.authorization !== "Bearer github-read-fixture") throw new Error("fixture forbids network or credentials");\n  const route = text.slice(prefix.length).replace(/^\\//, "");\n  if (!Object.hasOwn(data.routes, route)) throw new Error("fixture forbids unexpected API route");\n  return { ok: data.status === 200, status: data.status, json: async () => data.routes[route] };\n};\n`);
  const output = path.join(f.temp, "identify-output");
  const env = Object.fromEntries(["PATH", "Path", "PATHEXT", "SystemRoot", "SYSTEMROOT", "windir", "HOME", "USERPROFILE", "TEMP", "TMP", "TMPDIR"].filter((key) => process.env[key] !== undefined).map((key) => [key, process.env[key]]));
  Object.assign(env, f.env, { GITHUB_OUTPUT: output, RK_IDENTIFY_FIXTURE: dataFile });
  const result = spawnSync(process.execPath, ["--import", pathToFileURL(preload).href, script, command[2]], { cwd: root, env, encoding: "utf8", timeout: 30_000, maxBuffer: 1024 * 1024, windowsHide: true, shell: false });
  assert.ifError(result.error);
  const emitted = fs.existsSync(output) ? fs.readFileSync(output, "utf8") : "";
  const outputs = {};
  for (const line of emitted.split(/\r?\n/).filter(Boolean)) {
    const match = /^([a-z_][a-z0-9_]*)=([^\r\n]*)$/.exec(line);
    assert.ok(match, "malformed GITHUB_OUTPUT record"); assert.ok(!Object.hasOwn(outputs, match[1]), "duplicate GITHUB_OUTPUT record"); outputs[match[1]] = match[2];
  }
  return { result, emitted, outputs, stepId: steps[0].id };
}

test("actual identify CLI output routes parsed workflow across registry & dry-run matrix", (t) => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8"));
  for (const registry of ["npm", "crates"]) {
    const f = identifyWorkflowFixture(t, registry); validateCandidate(f.candidate);
    const { result, outputs, stepId } = runWorkflowIdentify(doc, f);
    assert.equal(result.status, 0, result.stderr); assert.equal(outputs.registry, registry); assert.equal(outputs.source_sha, SOURCE);
    for (const dryRun of [false, true]) {
      const selected = selectedUploadSteps(doc, { [stepId]: outputs }, dryRun);
      assert.equal(selected.length, dryRun ? 0 : 1, `${registry}, dry_run=${dryRun}`);
      if (!dryRun) assert.ok(selected[0].env[registry === "npm" ? "NODE_AUTH_TOKEN" : "CARGO_REGISTRY_TOKEN"]);
    }
    const removed = { ...outputs }; delete removed.registry;
    const renamed = { ...removed, registry_wrong: outputs.registry };
    for (const malformed of [removed, renamed]) assert.throws(() => selectedUploadSteps(doc, { [stepId]: malformed }, false), /did not emit/);
    const rewired = clone(doc); rewired.jobs["seal-check"].outputs.registry = "${{ steps.check.outputs.registry }}";
    assert.throws(() => selectedUploadSteps(rewired, { [stepId]: outputs }, false), /did not emit/);
  }
});

test("actual workflow identify command fails without output on malformed or unauthenticated source API", (t) => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8"));
  for (const registry of ["npm", "crates"]) for (const mutate of [
    (state) => { state.status = 403; },
    (state) => { state.routes[""] = null; },
    (state) => { state.routes["actions/runs/77/attempts/1"].head_repository.id = 123; },
  ]) {
    const f = identifyWorkflowFixture(t, registry), { result, emitted } = runWorkflowIdentify(doc, f, mutate);
    assert.notEqual(result.status, 0); assert.equal(emitted, "");
  }
});

test("CLI dispatch settles a fresh dynamic admission backedge & fails closed on unfinished work", (t) => {
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? process.env.RUNNER_TEMP ?? os.tmpdir();
  const temp = fs.realpathSync.native(fs.mkdtempSync(path.join(scratch, "rk-reviewed-entry-")));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const source = fs.readFileSync(new URL("./reviewed-candidate.mjs", import.meta.url), "utf8");
  const marker = "if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {";
  assert.equal(source.split(marker).length, 2, "actual CLI dispatch must exist exactly once");
  // Use the actual dispatch unchanged, with a pure minimal version of its module
  // graph. Imported-function tests cannot expose a pending entry-module backedge.
  const entry = path.join(temp, "reviewed-candidate.mjs");
  fs.writeFileSync(entry, `import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { retained } from "./supplemental-proof.mjs";
export const entryBinding = "authenticated";
const requireThat = (condition, message) => { if (!condition) throw new Error(message); };
async function identifyCandidate() { throw new Error("unexpected identify route"); }
async function runReviewed({ env, dryRun, verifyAdmission }) {
  requireThat(env === process.env && dryRun === true && verifyAdmission === false, "wrong CLI arguments");
  const { admit } = await import("./semantic-admission.mjs");
  await admit();
  process.stdout.write("entry-admitted\\n");
}
${source.slice(source.indexOf(marker))}`);
  fs.writeFileSync(path.join(temp, "supplemental-proof.mjs"), `import { entryBinding } from "./reviewed-candidate.mjs";
export function retained() { return entryBinding; }
`);
  fs.writeFileSync(path.join(temp, "runtime-supplement.mjs"), `import { retained } from "./supplemental-proof.mjs";
export function authenticatedRuntimeProof() { return retained(); }
`);
  fs.writeFileSync(path.join(temp, "semantic-admission.mjs"), `import { authenticatedRuntimeProof } from "./runtime-supplement.mjs";
export async function admit() {
  if (authenticatedRuntimeProof() !== "authenticated") throw new Error("backedge binding lost");
  if (process.env.RK_ENTRY_OUTCOME === "unfinished") await new Promise(() => {});
  await new Promise((resolve) => setTimeout(resolve, 5));
  if (process.env.RK_ENTRY_OUTCOME === "failure") throw new Error("admission fixture refused");
}
`);
  const env = Object.fromEntries(["PATH", "Path", "PATHEXT", "SystemRoot", "SYSTEMROOT", "windir", "HOME", "USERPROFILE", "TEMP", "TMP", "TMPDIR"].filter((key) => process.env[key] !== undefined).map((key) => [key, process.env[key]]));
  for (const outcome of ["success", "failure", "unfinished"]) {
    const result = spawnSync(process.execPath, ["--unhandled-rejections=strict", entry, "run", "--dry-run"], { cwd: temp, env: { ...env, RK_ENTRY_OUTCOME: outcome }, encoding: "utf8", timeout: 10_000, maxBuffer: 1024 * 1024, windowsHide: true, shell: false });
    assert.ifError(result.error); assert.equal(result.signal, null);
    assert.equal(result.status, outcome === "success" ? 0 : 1, result.stderr);
    assert.equal(result.stdout, outcome === "success" ? "entry-admitted\n" : "");
    assert.equal(result.stderr, outcome === "success" ? "" : outcome === "failure" ? "admission fixture refused\n" : "reviewed candidate refused: CLI execution did not settle\n");
  }
});

// ---------------------------------------------------------------------------------------------------------------
// Runtime proof transfer: any crate with trusted coverage, several pins per candidate
// ---------------------------------------------------------------------------------------------------------------
import { selectSupplement, selectSupplements, validateRuntimeTransfer, RUNTIME_PROOF_WORKLOADS } from "./reviewed-candidate.mjs";
import { workloadsFor as trustedWorkloadsFor } from "./runtime-qualify.mjs";
import { RUNTIME_BUDGET_CRATES } from "../../packages/release/runtime-budget-crates.mjs";
const runtimePin = (f, n, proofSha256 = String(n).repeat(64).slice(0, 64)) => ({ schema: 1, kind: "runtime-workload-pin", original: clone(f.supplements.supplements[0].original), producer: { workflowId: "6", runId: "88", runAttempt: "2", jobId: "104", sourceSha: PROMOTION, jobs: { candidate: "101", win32: "102", darwin: "103", qualify: "104" } }, artifact: { id: String(500 + n), digest: `sha256:${String(n).repeat(64).slice(0, 64)}`, proofSha256 }, policy: { id: "runtime-workloads/v1", sourceSha: PROMOTION, validatorSourceSha: PROMOTION } });
const transferRow = (name, pin, candidateId) => ({ name, runtimeFeatureProof: { classification: "always-on", listedOwner: true, required: true, status: "qualified", failures: [], proof: { candidateId, proofSha256: pin.artifact.proofSha256, workloads: [...RUNTIME_PROOF_WORKLOADS[name]] } } });
test("credential-side runtime workload table equals the trusted coverage table exactly", () => {
  const trusted = {};
  for (const crate of RUNTIME_BUDGET_CRATES) { try { trusted[crate] = trustedWorkloadsFor(crate).map((w) => w.id); } catch (error) { assert.match(error.message, /no trusted supported coverage/); } }
  assert.deepEqual(Object.fromEntries(Object.entries(RUNTIME_PROOF_WORKLOADS).map(([name, ids]) => [name, [...ids]])), trusted);
  assert.deepEqual(Object.keys(trusted).sort(), ["rightkit-daemon", "rightkit-job-queue", "rightkit-logs", "rightkit-qa", "rightkit-tauri-shell"]);
});
test("a candidate may carry one runtime pin per crate; every other kind stays unique", (t) => {
  const f = fixture(t, historicalReceipts, true);
  const write = (...pins) => fs.writeFileSync(f.supplementalFile, JSON.stringify({ schema: 1, supplements: [...f.supplements.supplements, ...pins] }));
  const [a, b, c] = [runtimePin(f, 1), runtimePin(f, 2), runtimePin(f, 3)];
  write(a);
  assert.deepEqual(selectSupplement(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), a);
  write(a, b, c);
  assert.deepEqual(selectSupplements(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), [a, b, c]);
  assert.throws(() => selectSupplement(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), /more than one supplement pin/);
  assert.deepEqual(selectArchiveSupplement(f.supplementalFile, f.candidate, f.approvedFile), f.supplements.supplements[0]);
  assert.deepEqual(selectSupplements(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile).length, 3);
  write();
  assert.deepEqual(selectSupplements(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), []);
  assert.equal(selectSupplement(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), null);
  // Same proof twice, a malformed runtime pin, or a duplicate archive-scan pin is still refused.
  write(a, clone(a)); assert.throws(() => selectSupplements(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), /duplicate/);
  write(a, { ...b, extra: true }); assert.throws(() => selectSupplements(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), /refused/);
  write(a, { ...b, artifact: { ...b.artifact, proofSha256: "not-a-hash" } }); assert.throws(() => selectSupplements(f.supplementalFile, f.candidate, "runtime-workload-pin", f.approvedFile), /refused|invalid/);
  fs.writeFileSync(f.supplementalFile, JSON.stringify({ schema: 1, supplements: [f.supplements.supplements[0], clone(f.supplements.supplements[0])] }));
  assert.throws(() => selectArchiveSupplement(f.supplementalFile, f.candidate, f.approvedFile), /duplicate/);
});
test("runtime transfer admits each trusted crate with its trusted workloads and refuses everything else", (t) => {
  const f = fixture(t, historicalReceipts, true), id = f.candidate.id;
  const names = Object.keys(RUNTIME_PROOF_WORKLOADS);
  const pins = names.map((_, i) => runtimePin(f, i + 1));
  names.forEach((name, i) => validateRuntimeTransfer(transferRow(name, pins[i], id), pins, id));
  const [logs, daemon] = [transferRow("rightkit-logs", pins[0], id), transferRow("rightkit-daemon", pins[1], id)];
  const bad = [
    { ...logs, name: "rightkit-daemon" },                                     // logs workloads claimed for the daemon
    { ...daemon, name: "rightkit-logs" },
    { ...logs, name: "rightkit-a" }, { ...logs, name: "rightkit-job-queue" }, { ...logs, name: "__proto__" }, { ...logs, name: "constructor" },
    (() => { const r = clone(daemon); r.runtimeFeatureProof.proof.workloads = ["local"]; return r; })(),
    (() => { const r = clone(daemon); r.runtimeFeatureProof.proof.workloads = [...r.runtimeFeatureProof.proof.workloads].reverse(); return r; })(),
    (() => { const r = clone(daemon); r.runtimeFeatureProof.proof.workloads.push("tracing"); return r; })(),
    (() => { const r = clone(daemon); r.runtimeFeatureProof.proof.proofSha256 = "f".repeat(64); return r; })(),   // no committed pin has this proof
    (() => { const r = clone(daemon); r.runtimeFeatureProof.proof.candidateId = "other"; return r; })(),
    (() => { const r = clone(daemon); r.runtimeFeatureProof.proof.extra = 1; return r; })(),
    (() => { const r = clone(daemon); r.runtimeFeatureProof.classification = "held-resource"; return r; })(),
    (() => { const r = clone(daemon); r.runtimeFeatureProof.listedOwner = false; return r; })(),
    (() => { const r = clone(daemon); r.runtimeFeatureProof.required = false; return r; })(),
  ];
  for (const row of bad) assert.throws(() => validateRuntimeTransfer(row, pins, id), /refused/);
  assert.throws(() => validateRuntimeTransfer(daemon, [], id), /refused/);
  assert.throws(() => validateRuntimeTransfer(daemon, [pins[0]], id), /refused/);
});
test("admission refuses a committed runtime pin that no qualified unit transfers, and a qualified row for an untrusted crate", async (t) => {
  const pinned = (mutate) => {
    const f = fixture(t, historicalReceipts, true);
    const pin = runtimePin(f, 1);
    fs.writeFileSync(f.supplementalFile, JSON.stringify({ schema: 1, supplements: [...f.supplements.supplements, pin] }));
    mutate(f, pin); f.resealAdmission();
    return f;
  };
  await assert.rejects(pinned(() => {}).prepare(), /runtime supplement pins differ from qualified units/);
  await assert.rejects(pinned((f, pin) => { f.admission.units[0].runtimeFeatureProof = transferRow("rightkit-logs", pin, f.candidate.id).runtimeFeatureProof; }).prepare(), /runtime supplement transfer mismatch/);
  // No pin and no qualified row is the unchanged baseline.
  const f = fixture(t, historicalReceipts, true);
  assert.ok(await f.prepare());
});

test("a Linux-required candidate must pin the Linux receipt and its Linux job; any other candidate must not", (t) => {
  const f = fixture(t); validateCandidate(f.candidate); checkProvenance(f.provenance);
  const linuxPin = { id: "999", digest: `sha256:${"e".repeat(64)}`, receiptSha256: "f".repeat(64) };
  const required = (candidate) => { candidate.registry = "npm"; candidate.units = ["@rightkit/qa-chrome"]; return candidate; };
  // Not required: a Linux pin is surplus and refused; the three-pin shape stays valid.
  const surplus = clone(f.candidate); surplus.artifacts.linux = linuxPin;
  assert.throws(() => validateCandidate(surplus), /artifact pins has unexpected or missing keys/);
  // Required: the Linux pin is mandatory and must be shaped like the native pins.
  const missing = required(clone(f.candidate));
  assert.throws(() => validateCandidate(missing), /artifact pins has unexpected or missing keys/);
  const pinned = required(clone(f.candidate)); pinned.artifacts.linux = linuxPin;
  assert.equal(validateCandidate(pinned), pinned);
  const badPin = required(clone(f.candidate)); badPin.artifacts.linux = { ...linuxPin }; delete badPin.artifacts.linux.receiptSha256;
  assert.throws(() => validateCandidate(badPin), /linux pin has unexpected or missing keys/);
  const dupId = required(clone(f.candidate)); dupId.artifacts.linux = { ...linuxPin, id: f.candidate.artifacts.macos.id };
  assert.throws(() => validateCandidate(dupId), /duplicate artifact ID/);
  // Provenance: the Linux verify job must have succeeded exactly once when required, and must not have run otherwise.
  const job = (conclusion) => ({ id: 70, name: "verify-linux", run_id: 77, run_attempt: 1, head_sha: SOURCE, status: "completed", conclusion });
  const base = clone(f.provenance);
  const withLinux = (conclusion, candidate) => { const p = clone(base); p.candidate = candidate; p.jobs.jobs.push(...(conclusion === null ? [] : [job(conclusion)])); p.jobs.total_count = p.jobs.jobs.length; return p; };
  checkProvenance(withLinux("skipped", clone(f.candidate)));
  checkProvenance(withLinux(null, clone(f.candidate)));
  assert.throws(() => checkProvenance(withLinux("success", clone(f.candidate))), /Linux verify job must succeed exactly when/);
  assert.throws(() => checkProvenance(withLinux("failure", clone(f.candidate))), /refused|Linux verify job/);
  checkProvenance(withLinux("success", pinned));
  for (const bad of ["skipped", "failure", "cancelled", null]) assert.throws(() => checkProvenance(withLinux(bad, pinned)), /Linux verify job must succeed exactly when/, String(bad));
  const twice = withLinux("success", pinned); twice.jobs.jobs.push({ ...job("success"), id: 71 }); twice.jobs.total_count = twice.jobs.jobs.length;
  assert.throws(() => checkProvenance(twice), /Linux verify job must succeed exactly when/);
});

// ---- direct publication of a green qualification run ----------------------------------------
const DIRECT_WORKFLOW_REF = "bogusyogi/rightkit/.github/workflows/publish-direct.yml@refs/heads/main";
const directEnv = (f, extra = {}) => {
  const env = { ...f.env, GITHUB_WORKFLOW_REF: DIRECT_WORKFLOW_REF, GITHUB_RUN_ID: "90", PUBLISH_QUALIFIED_RUN_ID: "77", PUBLISH_QUALIFIED_RUN_ATTEMPT: "1", PUBLISH_QUALIFIED_SHA: SOURCE, ...extra };
  for (const key of ["PUBLISH_CANDIDATE_ID", "PUBLISH_ADMISSION_ID", "PUBLISH_ADMISSION_DIGEST"]) delete env[key];
  return env;
};
function green(f) {
  f.provenance.run.conclusion = "success";
  f.provenance.jobs.jobs.push({ name: "qualified", conclusion: "success", run_id: 77, run_attempt: 1, status: "completed", head_sha: SOURCE });
  f.provenance.jobs.total_count += 1;
  for (const host of ["packed", "macos", "windows"]) f.qaListing.artifacts.push(f.artifacts[f.candidate.artifacts[host].id]);
  f.qaListing.total_count += 3;
  return f;
}

test("a green qualification run publishes its own sealed bytes directly: no candidate, no approval pin, no reviewed dispatch", async (t) => {
  const f = green(fixture(t));
  const identified = await runQualified({ env: directEnv(f), identify: true, fetchImpl: f.fetchImpl, spawn: f.spawn });
  assert.deepEqual(identified, { registry: "crates", units: ["rightkit-a"], sourceSha: SOURCE });
  assert.equal(f.puts.length, 0, "identification touches no registry");
  const actual = await runQualified({ env: directEnv(f, { PUBLISH_REGISTRY: "crates", CARGO_REGISTRY_TOKEN: "registry-fixture" }), fetchImpl: f.fetchImpl, spawn: f.spawn, pause: 0, log: () => {} });
  assert.equal(f.puts.length, 1);
  assert.deepEqual(f.puts[0].body, buildCratePublishBody(f.metadata.toString(), f.crate));
  assert.equal(actual.commit, SOURCE); assert.equal(actual.candidate.runId, "77"); assert.equal(actual.candidate.source, "qualified");
  assert.ok(!f.requests.some((r) => r.url.includes("approved-artifacts") || r.url.includes("publish-reviewed")), "nothing committed is consulted");
  assert.ok(f.requests.filter((r) => r.url.startsWith("https://api.github.com/")).every((r) => r.options.headers.authorization === "Bearer github-read-fixture"));
  assert.ok(fs.readdirSync(path.join(f.temp, "publication-receipt")).length >= 1, "a publication receipt is written for the artifact step");
  const roots = fs.readdirSync(f.temp).filter((name) => name.startsWith("qualified-run-"));
  assert.equal(roots.length, 2, "one prepared root per step");
  const approval = JSON.parse(fs.readFileSync(path.join(f.temp, roots[0], "qualified-approval.json"), "utf8"));
  assert.deepEqual(approval.artifacts.map((row) => [row.registry, row.name, row.version, row.sha256, row.reviewer, row.reviewRef]), [["crates", "rightkit-a", "0.1.0", f.manifest.units[0].sha256, "publish.yml qualified", "https://github.com/bogusyogi/rightkit/actions/runs/77/attempts/1"]]);
});

test("direct publication refuses a run that is not green, foreign, diverged, resealed or credentialed too early", async (t) => {
  const cases = [
    ["run concluded failure", (g) => { g.provenance.run.conclusion = "failure"; }],
    ["qualified job missing", (g) => { g.provenance.jobs.jobs.pop(); g.provenance.jobs.total_count -= 1; }],
    ["qualified job failed", (g) => { g.provenance.jobs.jobs.at(-1).conclusion = "failure"; }],
    ["a native verify job failed", (g) => { g.provenance.jobs.jobs.find((j) => j.name.startsWith("verify (windows")).conclusion = "failure"; }],
    ["wrong workflow", () => {}, { GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/publish-reviewed.yml@refs/heads/main" }],
    ["promotion diverged from the qualified commit", (g) => { g.provenance.comparison.status = "diverged"; }],
    ["event is not a dispatch", (g) => { g.provenance.run.event = "push"; }],
    ["run of another commit", (g) => { g.provenance.run.head_sha = PROMOTION; }],
    ["sealed bytes changed after packing", (g) => { g.files.packed["rightkit-a-0.1.0.crate"] = Buffer.from("other bytes"); }],
    ["receipt artifact owned by another run", (g) => { g.artifacts[g.candidate.artifacts.macos.id].workflow_run.id = 78; }],
    ["packed artifact listed twice", (g) => { g.qaListing.artifacts.push(g.artifacts[g.candidate.artifacts.packed.id]); g.qaListing.total_count += 1; }],
    ["receipt missing", (g) => { g.qaListing.artifacts = g.qaListing.artifacts.filter((a) => a.name !== "publish-receipt-verify-windows-2025"); g.qaListing.total_count -= 1; }],
    ["registry credential during identification", () => {}, { CARGO_REGISTRY_TOKEN: "registry-fixture" }],
    ["unknown run attempt", () => {}, { PUBLISH_QUALIFIED_RUN_ATTEMPT: "x" }],
    ["promotion behind the qualified commit", (g) => { g.provenance.comparison.status = "behind"; }],
    ["windows receipt ran with other allowsets than macOS", (g) => { const doc = JSON.parse(g.files.windows["verify-windows.json"]); doc.allowIgnored = ["rightkit-a"]; g.files.windows["verify-windows.json"] = Buffer.from(JSON.stringify(doc)); }],
    ["sealed manifest of another commit", (g) => { const doc = JSON.parse(g.files.packed["manifest.json"]); doc.commit = PROMOTION; g.files.packed["manifest.json"] = Buffer.from(JSON.stringify(doc)); }],
    ["runtime-proof owner on the direct route", (g) => { const doc = JSON.parse(g.files.packed["manifest.json"]); doc.units[0].name = "rightkit-daemon"; g.files.packed["manifest.json"] = Buffer.from(JSON.stringify(doc)); }],
  ];
  for (const [name, mutate, extra] of cases) {
    const g = green(fixture(t)); mutate(g);
    await assert.rejects(runQualified({ env: directEnv(g, extra), identify: true, fetchImpl: g.fetchImpl, spawn: g.spawn }), /refused/, name);
    assert.equal(g.puts.length, 0, name);
  }
  const h = green(fixture(t));
  await assert.rejects(runQualified({ env: directEnv(h, { PUBLISH_REGISTRY: "npm", NODE_AUTH_TOKEN: "npm-fixture" }), fetchImpl: h.fetchImpl, spawn: h.spawn, pause: 0, log: () => {} }), /the qualified run sealed crates/);
  assert.equal(h.puts.length, 0);
  const noToken = green(fixture(t));
  await assert.rejects(runQualified({ env: directEnv(noToken, { PUBLISH_REGISTRY: "crates" }), fetchImpl: noToken.fetchImpl, spawn: noToken.spawn, pause: 0, log: () => {} }), /CARGO_REGISTRY_TOKEN is not set/);
  assert.equal(noToken.puts.length, 0);
  // Direct publication at the qualified commit itself (nothing pushed since): identical comparison is the legitimate case.
  const same = green(fixture(t)); same.provenance.comparison = { status: "identical", merge_base_commit: { sha: SOURCE } };
  assert.deepEqual(await runQualified({ env: directEnv(same, { GITHUB_SHA: SOURCE }), identify: true, fetchImpl: same.fetchImpl, spawn: same.spawn }), { registry: "crates", units: ["rightkit-a"], sourceSha: SOURCE });
  // A Linux-required unit needs the Linux receipt and job of the same run; the fixture has neither.
  const linux = green(fixture(t)); const linuxFile = path.join(linux.temp, "linux-required.json"); fs.writeFileSync(linuxFile, JSON.stringify({ schema: 1, npm: [], crates: ["rightkit-a"] }));
  await assert.rejects(runQualified({ env: directEnv(linux), identify: true, fetchImpl: linux.fetchImpl, spawn: linux.spawn, linuxFile }), /Linux verify job must succeed exactly when/);
});

test("publish-direct workflow: fires on completed publish runs of main, identifies without credentials, then one credentialed step per registry", () => {
  const text = fs.readFileSync(new URL("../../.github/workflows/publish-direct.yml", import.meta.url), "utf8");
  const doc = parseYaml(text);
  assert.deepEqual(doc.on, { workflow_run: { workflows: ["publish"], types: ["completed"], branches: ["main"] } });
  assert.deepEqual(doc.permissions, { contents: "read" });
  assert.deepEqual(Object.keys(doc.jobs), ["upload"]);
  const job = doc.jobs.upload;
  for (const clause of ["github.event.workflow_run.conclusion == 'success'", "github.event.workflow_run.event == 'workflow_dispatch'", "github.event.workflow_run.head_branch == 'main'", "github.event.workflow_run.head_repository.full_name == 'bogusyogi/rightkit'", "github.ref == 'refs/heads/main'", "github.repository == 'bogusyogi/rightkit'"]) assert.ok(job.if.includes(clause), clause);
  assert.equal(job.environment, "release");
  assert.deepEqual(job.permissions, { contents: "read", actions: "read", "id-token": "write" });
  const [checkout, node, identify, npm, crates, receipt] = job.steps;
  assert.equal(job.steps.length, 6);
  assert.equal(checkout.uses, "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1");
  assert.equal(checkout.with.ref, "${{ github.event.workflow_run.head_sha }}");
  assert.equal(String(checkout.with["persist-credentials"]), "false");
  assert.ok(!String(checkout.with["sparse-checkout"]).includes("approved-artifacts"), "the committed approval file is not an input of direct publication");
  assert.equal(node.uses, "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020");
  const source = { GITHUB_TOKEN: "${{ github.token }}", PUBLISH_QUALIFIED_RUN_ID: "${{ github.event.workflow_run.id }}", PUBLISH_QUALIFIED_RUN_ATTEMPT: "${{ github.event.workflow_run.run_attempt }}", PUBLISH_QUALIFIED_SHA: "${{ github.event.workflow_run.head_sha }}" };
  assert.equal(identify.id, "qualified"); assert.equal(identify.run, "node scripts/ci/reviewed-candidate.mjs run-qualified --identify"); assert.deepEqual(identify.env, source);
  assert.equal(npm.if, "${{ steps.qualified.outputs.registry == 'npm' }}"); assert.deepEqual(npm.env, { ...source, PUBLISH_REGISTRY: "npm", NODE_AUTH_TOKEN: "${{ secrets.NPM_TOKEN }}" }); assert.equal(npm.run, "node scripts/ci/reviewed-candidate.mjs run-qualified");
  assert.equal(crates.if, "${{ steps.qualified.outputs.registry == 'crates' }}"); assert.deepEqual(crates.env, { ...source, PUBLISH_REGISTRY: "crates", CARGO_REGISTRY_TOKEN: "${{ secrets.CARGO_REGISTRY_TOKEN }}" }); assert.equal(crates.run, "node scripts/ci/reviewed-candidate.mjs run-qualified");
  assert.equal(receipt.uses, "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"); assert.equal(receipt.with.path, "${{ runner.temp }}/publication-receipt");
  assert.equal((text.match(/secrets\./g) ?? []).length, 2, "exactly the two registry tokens, one per credentialed step");
  assert.deepEqual(Object.values(QUALIFIED_ARTIFACTS), ["publish-packed", "publish-receipt-verify-xcode-27", "publish-receipt-verify-windows-2025", "publish-receipt-verify-ubuntu-24.04"]);
});
