import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { sha, executeUpload, checkReceipts, authenticatedArchiveScanResolver, CRATES_PUBLISH_URL, buildCratePublishBody } from "./upload.mjs";
import { validateCandidate, selectCandidate, selectArchiveSupplement, checkProvenance, prepareCandidate, assertAuthenticatedContext } from "./reviewed-candidate.mjs";
import { SUPPLEMENT_WORKFLOW, SUPPLEMENT_POLICY } from "./supplemental-proof.mjs";
import { assertReviewedDoc, REVIEWED_MUTATIONS } from "./reviewed-workflow-contract.mjs";
import { parseYaml } from "./workflow-yaml.mjs";

const SOURCE = "a".repeat(40); const PROMOTION = "b".repeat(40);
const clone = (x) => structuredClone(x);
const archiveScanFixture = (unit) => ({ schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: unit.sha256, status: "pass", files: 1, violations: [], problems: [] });
function fixture(t, mutate = () => {}, supplemental = false) {
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? (process.platform === "win32" ? "D:/scratch" : "/Volumes/D/scratch");
  const temp = fs.mkdtempSync(path.join(scratch, "rk-reviewed-"));
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
  const provenance = { candidate, repository: { id: 99, full_name: "bogusyogi/rightkit", default_branch: "main" }, workflow: { id: 5, path: ".github/workflows/publish.yml", state: "active" }, run: { id: 77, run_attempt: 1, repository: { id: 99, full_name: "bogusyogi/rightkit" }, head_repository: { id: 99, full_name: "bogusyogi/rightkit" }, workflow_id: 5, path: ".github/workflows/publish.yml", event: "workflow_dispatch", head_branch: "main", head_sha: SOURCE, status: "completed", conclusion: "failure" }, jobs: { total_count: 5, jobs: ["guard", "plan", "pack", "verify (xcode-27, 26.8.1)", "verify (windows-2025, 26.8.1)"].map((name) => ({ name, conclusion: "success", run_id: 77, head_sha: SOURCE })) }, promotionSha: PROMOTION, comparison: { status: "ahead", merge_base_commit: { sha: SOURCE } } };
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
      else if (route === "actions/workflows/supplement-archive-scan.yml") doc = { id: 6, path: SUPPLEMENT_WORKFLOW, state: "active" };
      else if (route === "actions/runs/99/attempts/1") doc = state.supplementRun;
      else if (route === "actions/runs/99/attempts/1/jobs?per_page=100") doc = state.supplementJobs;
      else if (route === "actions/runs/77/attempts/1") doc = provenance.run;
      else if (route === "actions/runs/77/attempts/1/jobs?per_page=100") doc = provenance.jobs;
      else if (route === `compare/${PROMOTION}...${PROMOTION}`) doc = { status: "identical", merge_base_commit: { sha: PROMOTION } };
      else if (route.startsWith("compare/")) doc = provenance.comparison;
      else if (/^actions\/artifacts\/\d+$/.test(route)) doc = artifacts[route.split("/")[2]];
      else if (/^actions\/artifacts\/\d+\/zip$/.test(route)) { const host = route.includes("/200/") ? "supplement" : Object.keys(candidate.artifacts).find((h) => candidate.artifacts[h].id === route.split("/")[2]); return { ok: true, status: 200, arrayBuffer: async () => archives[host] }; }
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
  const env = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_REF: "refs/heads/main", GITHUB_SHA: PROMOTION, GITHUB_RUN_ID: "88", GITHUB_RUN_ATTEMPT: "1", GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/publish-reviewed.yml@refs/heads/main", GITHUB_TOKEN: "github-read-fixture", RUNNER_TEMP: temp };
  const prepare = () => prepareCandidate({ env, candidateId: candidate.id, approvedFile, supplementalFile, fetchImpl, spawn });
  const upload = async (prepared, dryRun) => executeUpload({ env: { ...env, PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: candidate.units.join(" "), ...(dryRun ? {} : { CARGO_REGISTRY_TOKEN: "registry-fixture" }) }, packDir: prepared.packDir, receiptsDir: prepared.receiptsDir, candidateContext: prepared.context, approvedFile, outDir: path.join(temp, `out-${dryRun}`), dryRun, fetchImpl, spawn, pause: 0, log: () => {} });
  return { ...state, temp, approvedFile, supplementalFile, supplements, manifest, env, requests, puts, prepare, upload, crate, metadata };
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
