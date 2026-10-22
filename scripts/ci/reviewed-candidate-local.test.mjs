import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { sha, executeUpload, checkLocalReceipts, loadSealed, localScanShapeProblems, LOCAL_SCAN_SCHEMA, CRATES_PUBLISH_URL, buildCratePublishBody } from "./upload.mjs";
import { validateCandidate, selectCandidate, checkLocalProvenance, localAssetName, localTag, prepareCandidate, authenticateAdmission, assertAuthenticatedContext, identifyCandidate, checkCiRun, ciPolicy, ciProofFor, ciRequiredUnits, ciProofOf, localAllow, localLinuxPolicy } from "./reviewed-candidate.mjs";

const SOURCE = "a".repeat(40); const PROMOTION = "b".repeat(40); const TREE = "c".repeat(40);
const ID = "local-a-010-aaaaaa";
const clone = (x) => structuredClone(x);
const hex = (n) => n.repeat(64);
const archiveScan = (unit) => ({ schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: unit.sha256, status: "pass", files: 1, violations: [], problems: [] });
const scanRecord = (units) => ({ schema: LOCAL_SCAN_SCHEMA, passed: true, problems: [], sourceCommit: SOURCE, headCommit: SOURCE, cargoLockSha256: hex("d"), scope: { units, files: 3 }, tools: { pinned: {}, observed: {} }, classes: ["A", "B"], scanSetDigest: hex("e"), classDigests: { A: hex("e"), B: hex("f") }, inputDigest: null, deferred: [], scans: [{ id: "cargo-metadata", class: "A", command: "cargo metadata", status: "pass" }, { id: "clippy:rightkit-a", class: "B", command: "cargo clippy", status: "pass" }] });
const OS = { macos: "darwin", windows: "win32" };
function receiptFor(host, unit, manifestSha256, overrides = {}) {
  const evidence = { schema: 1, kind: "cargo", name: unit.name, version: unit.version, commit: SOURCE, artifactSha256: unit.sha256, hostPolicy: host === "macos" ? "darwin-only" : "win32-only", hostPolicyReason: "fixture", runs: [{ host: { os: OS[host], arch: "arm64" }, commit: SOURCE, features: ["default"], artifactSha256: unit.sha256, command: "cargo test -p rightkit-a", operator: "agent (local-build grant)", timestamp: "2026-10-22T00:00:00Z", passed: 2, failed: 0, skipped: 0, ignored: 0, steps: [] }] };
  return { schema: 1, kind: "local-verify", host, hostOs: OS[host], arch: "arm64", registry: "crates", candidateId: ID, commit: SOURCE, tree: TREE, manifestSha256, broker: { name: "rustright", client: "C:/.rustright/runtime/rustright.exe", sha256: hex("9") }, qualifiedAt: "2026-10-22T00:00:00Z", allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [], scans: scanRecord(["rightkit-a"]), units: [{ name: unit.name, version: unit.version, file: unit.file, sha256: unit.sha256, bytesVerified: true, archiveScan: archiveScan(unit), status: "pass", executed: 2, ignored: 0, skipped: 0, steps: [], evidence }], ...overrides };
}
// Same central-directory fixture the Actions tests use: consumed only through the mocked unzip, authenticated by digest.
function proofArchive(files) {
  const rows = Object.entries(files).map(([name, bytes]) => {
    const nameBytes = Buffer.from(name), header = Buffer.alloc(46);
    header.writeUInt32LE(0x02014b50, 0); header.writeUInt16LE(3 << 8, 4); header.writeUInt32LE(bytes.length, 24); header.writeUInt16LE(nameBytes.length, 28); header.writeUInt32LE((0x81a4 << 16) >>> 0, 38);
    return Buffer.concat([header, nameBytes]);
  });
  const payload = Buffer.concat(Object.values(files)), directory = Buffer.concat(rows), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(rows.length, 8); end.writeUInt16LE(rows.length, 10); end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(payload.length, 16);
  return Buffer.concat([payload, directory, end]);
}

function fixture(t, mutate = () => {}) {
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? process.env.RUNNER_TEMP ?? os.tmpdir();
  const temp = fs.realpathSync.native(fs.mkdtempSync(path.join(scratch, "rk-local-")));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const crate = Buffer.from("locally qualified sealed archive");
  const metadata = Buffer.from(JSON.stringify({ name: "rightkit-a", vers: "0.1.0", deps: [] }));
  const unit = { name: "rightkit-a", version: "0.1.0", dir: "crates/rightkit-a", file: "rightkit-a-0.1.0.crate", sha256: sha("sha256", crate), size: crate.length, metaFile: "rightkit-a-0.1.0.upload.json", metaSha256: sha("sha256", metadata) };
  const manifest = { schema: 1, registry: "crates", commit: SOURCE, repository: "bogusyogi/rightkit", source: "local", candidateId: ID, units: [unit] };
  const manifestBytes = Buffer.from(JSON.stringify(manifest));
  const manifestSha256 = sha("sha256", manifestBytes);
  const entries = { "manifest.json": manifestBytes, [unit.file]: crate, [unit.metaFile]: metadata };
  const assetBytes = { packed: Buffer.from("ZIP fixture packed"), macos: Buffer.from(JSON.stringify(receiptFor("macos", unit, manifestSha256))), windows: Buffer.from(JSON.stringify(receiptFor("windows", unit, manifestSha256))) };
  const candidate = { id: ID, source: "local", sourceSha: SOURCE, registry: "crates", units: ["rightkit-a"], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, release: { tag: localTag(ID) }, assets: {} };
  const rows = [];
  Object.keys(assetBytes).forEach((kind, i) => { const name = localAssetName(ID, kind); candidate.assets[kind] = { name, sha256: sha("sha256", assetBytes[kind]) }; rows.push({ id: 500 + i, name, size: assetBytes[kind].length, state: "uploaded", kind }); });
  const release = { tag_name: localTag(ID), prerelease: true, draft: false, assets: rows.map(({ kind, ...asset }) => asset) };
  const ref = { ref: `refs/tags/${localTag(ID)}`, object: { type: "commit", sha: SOURCE } };
  const repository = { id: 99, full_name: "bogusyogi/rightkit", default_branch: "main" };
  const approval = { schema: 2, candidates: [candidate], artifacts: [{ registry: "crates", name: unit.name, version: unit.version, sha256: unit.sha256, reviewer: "Oracle fixture", reviewRef: "agent-review:fixture" }] };
  const state = { candidate, release, ref, repository, approval, entries, assetBytes, unit, manifest, manifestSha256, rows, redirects: {}, comparison: { status: "ahead", merge_base_commit: { sha: SOURCE } }, adm: {} }; mutate(state);
  const approvedFile = path.join(temp, "approved-artifacts.json"); fs.writeFileSync(approvedFile, JSON.stringify(approval));
  const supplementalFile = path.join(temp, "supplemental-proofs.json"); fs.writeFileSync(supplementalFile, JSON.stringify({ schema: 1, supplements: [] }));
  const admission = () => ({ schema: 1, kind: "local-admission", original: { candidateId: ID, candidateSha256: sha("sha256", JSON.stringify(state.candidate)), sourceSha: SOURCE, manifestSha256, archives: [{ name: unit.name, version: unit.version, file: unit.file, sha256: unit.sha256, metaFile: unit.metaFile, metaSha256: unit.metaSha256 }] }, sourceTree: TREE, sourceHashes: { "Cargo.toml": hex("d") }, verifierSha: PROMOTION, runId: "88", runAttempt: "1", receipts: { macos: state.candidate.assets.macos.sha256, windows: state.candidate.assets.windows.sha256 }, units: [{ name: unit.name, version: unit.version, archiveSha256: unit.sha256, ok: true, failures: [], policies: { macos: "required-positive-tests", windows: "required-positive-tests" }, runtimeFeatureProof: { classification: "false", listedOwner: false, required: false, status: "not-applicable", failures: [] }, evidenceSha256: hex("b") }], ...(state.candidate.ci ? { ciProof: { runId: String(state.candidate.ci.runId), runAttempt: 1, conclusion: "success", checkedAt: "2026-10-22T12:00:00Z", requiredUnits: [], failedJobs: [] } } : {}) });
  let admissionBytes = Buffer.from(JSON.stringify(state.adm.override ?? admission())); let admissionArchive = proofArchive({ "admission.json": admissionBytes });
  const artifact = { id: 400, name: "reviewed-admission-receipt", expired: false, digest: `sha256:${sha("sha256", admissionArchive)}`, workflow_run: { id: 88, head_sha: PROMOTION, head_branch: "main", repository_id: 99, head_repository_id: 99 } };
  const promotionRun = { id: 88, run_attempt: 1, repository: { id: 99, full_name: "bogusyogi/rightkit" }, head_repository: { id: 99, full_name: "bogusyogi/rightkit" }, workflow_id: 7, path: ".github/workflows/publish-reviewed.yml", event: "workflow_dispatch", head_branch: "main", head_sha: PROMOTION, status: "in_progress" };
  const promotionJobs = { total_count: 1, jobs: [{ name: "seal-check", status: "completed", conclusion: "success", run_id: 88, run_attempt: 1, head_sha: PROMOTION }] };
  const requests = []; const puts = []; let published = false;
  const fetchImpl = async (url, options = {}) => {
    const text = String(url); requests.push({ url: text, options });
    if (text.startsWith("https://release-assets.githubusercontent.com/")) { const bytes = state.redirects[text]; return { ok: Boolean(bytes), status: bytes ? 200 : 404, arrayBuffer: async () => bytes }; }
    if (text.startsWith("https://api.github.com/repos/bogusyogi/rightkit")) {
      const route = text.slice("https://api.github.com/repos/bogusyogi/rightkit".length).replace(/^\//, "");
      const asset = /^releases\/assets\/(\d+)$/.exec(route);
      if (asset) {
        const row = rows.find((item) => String(item.id) === asset[1]); const bytes = state.assetBytes[row.kind];
        assert.equal(options.headers.accept, "application/octet-stream"); assert.equal(options.redirect, "manual");
        if (row.kind === "packed") { const location = state.redirectTo ?? `https://release-assets.githubusercontent.com/${row.name}`; state.redirects[location] = bytes; return { ok: false, status: 302, headers: { get: () => location } }; }
        return { ok: true, status: 200, arrayBuffer: async () => bytes };
      }
      let doc;
      if (route === "") doc = repository;
      else if (route === `releases/tags/${localTag(ID)}`) doc = state.release;
      else if (route === `git/ref/tags/${localTag(ID)}`) doc = state.ref;
      else if (route === "actions/workflows/publish-reviewed.yml") doc = { id: 7, path: ".github/workflows/publish-reviewed.yml", state: "active" };
      else if (route === "actions/runs/88/attempts/1") doc = promotionRun;
      else if (route === "actions/runs/88/attempts/1/jobs?per_page=100") doc = promotionJobs;
      else if (route === `compare/${SOURCE}...${PROMOTION}`) doc = state.comparison;
      else if (route === "actions/artifacts/400") doc = artifact;
      else if (route === "actions/artifacts/400/zip") return { ok: true, status: 200, arrayBuffer: async () => admissionArchive };
      else throw new Error(`unexpected API route ${route}`);
      return { ok: true, status: 200, json: async () => doc };
    }
    if (text === CRATES_PUBLISH_URL) { published = true; puts.push(options); return { ok: true, status: 200, text: async () => '{"warnings":{}}' }; }
    if (text.startsWith("https://index.crates.io/")) return { ok: published, status: published ? 200 : 404, text: async () => JSON.stringify({ vers: unit.version, cksum: unit.sha256 }) };
    throw new Error(`unexpected network ${text}`);
  };
  const spawn = (command, args) => {
    assert.equal(command, "unzip", "only the pinned ZIP is read; nothing the candidate ships executes");
    if (path.basename(args[1]) === "admission.zip") return { status: 0, stdout: admissionBytes };
    assert.equal(path.basename(args[1]), "packed.zip");
    return { status: 0, stdout: args[0] === "-Z1" ? Object.keys(state.entries).join("\n") : state.entries[args[2]] };
  };
  const env = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_REF: "refs/heads/main", GITHUB_SHA: PROMOTION, GITHUB_RUN_ID: "88", GITHUB_RUN_ATTEMPT: "1", GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/publish-reviewed.yml@refs/heads/main", GITHUB_TOKEN: "github-read-fixture", RUNNER_TEMP: temp, PUBLISH_CANDIDATE_ID: ID, PUBLISH_ADMISSION_ID: "400", PUBLISH_ADMISSION_DIGEST: artifact.digest };
  const prepareOriginal = () => prepareCandidate({ env, candidateId: ID, approvedFile, supplementalFile, fetchImpl, spawn, linuxFile: state.linuxFile });
  const prepare = async () => { const prepared = await prepareOriginal(); await authenticateAdmission({ prepared, env, fetchImpl, spawn }); return prepared; };
  const upload = (prepared, dryRun) => executeUpload({ env: { ...env, PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a", ...(dryRun ? {} : { CARGO_REGISTRY_TOKEN: "registry-fixture" }) }, packDir: prepared.packDir, receiptsDir: prepared.receiptsDir, candidateContext: prepared.context, approvedFile, outDir: path.join(temp, `out-${dryRun}`), dryRun, fetchImpl, spawn, pause: 0, log: () => {} });
  const reseal = () => { admissionBytes = Buffer.from(JSON.stringify(state.adm.override ?? admission())); admissionArchive = proofArchive({ "admission.json": admissionBytes }); artifact.digest = `sha256:${sha("sha256", admissionArchive)}`; env.PUBLISH_ADMISSION_DIGEST = artifact.digest; };
  return { ...state, temp, approvedFile, env, requests, puts, prepare, prepareOriginal, upload, crate, metadata, admission, reseal, fetchImpl, spawn, setAdmission: (doc) => { state.adm.override = doc; reseal(); } };
}

const withAsset = (s, kind, bytes) => { s.assetBytes[kind] = bytes; s.rows.find((r) => r.kind === kind).size = bytes.length; s.release.assets.find((a) => a.name === s.candidate.assets[kind].name).size = bytes.length; };

test("a local candidate's pinned assets promote through the same upload path and send the exact packed bytes", async (t) => {
  const f = fixture(t); const prepared = await f.prepare();
  const dry = await f.upload(prepared, true); assert.equal(f.puts.length, 0);
  const actual = await f.upload(prepared, false);
  assert.equal(actual.commit, SOURCE); assert.equal(actual.promotionCommit, PROMOTION); assert.equal(actual.candidate.source, "local");
  assert.deepEqual(f.puts[0].body, buildCratePublishBody(f.metadata.toString(), f.crate));
  assert.equal(dry.receipts[0].sha256, actual.receipts[0].sha256);
  assert.ok(f.requests.filter((r) => r.url.startsWith("https://api.github.com/")).every((r) => r.options.headers.authorization === "Bearer github-read-fixture"));
  // The redirect target never receives the token.
  assert.ok(f.requests.filter((r) => r.url.startsWith("https://release-assets.")).every((r) => !r.options.headers));
  assert.equal(f.requests.filter((r) => r.url.includes("/actions/runs/77")).length, 0, "no Actions qualification run is consulted");
});

test("identify authenticates a local candidate without any Actions run, and writes source & registry outputs", async (t) => {
  const f = fixture(t); const out = path.join(f.temp, "gh-output");
  const result = await identifyCandidate({ env: { ...f.env, GITHUB_OUTPUT: out }, candidateId: ID, approvedFile: f.approvedFile, fetchImpl: f.fetchImpl });
  assert.deepEqual(result, { sourceSha: SOURCE, registry: "crates" });
  assert.match(fs.readFileSync(out, "utf8"), new RegExp(`source_sha=${SOURCE}\\nregistry=crates`));
});

test("local candidate validation: good, wrong sha, missing host, extra keys, wrong names, linux-required without linux", (t) => {
  const f = fixture(t); const good = validateCandidate(clone(f.candidate)); assert.equal(good.source, "local");
  const refused = [
    (c) => { c.assets.windows.sha256 = "xyz"; }, (c) => { c.assets.windows.sha256 = "A".repeat(64); }, (c) => { delete c.assets.windows; }, (c) => { delete c.assets.packed; },
    (c) => { c.extra = 1; }, (c) => { c.assets.packed.extra = 1; }, (c) => { c.assets.linux = { name: localAssetName(ID, "linux"), sha256: hex("1") }; },
    (c) => { c.assets.macos.name = "other.json"; }, (c) => { c.release.tag = "candidate-other"; }, (c) => { c.release = { tag: localTag(ID), url: "x" }; },
    (c) => { c.registry = "npm"; c.units = ["@rightkit/qa"]; }, (c) => { c.source = "actions"; }, (c) => { c.id = "Bad Id"; }, (c) => { c.sourceSha = "abc"; },
    (c) => { c.units = ["rightkit-a", "rightkit-a"]; }, (c) => { c.units = ["../escape"]; }, (c) => { c.allow.ignored = ["rightkit-b"]; }, (c) => { delete c.allow; },
  ];
  for (const mutate of refused) { const c = clone(f.candidate); mutate(c); assert.throws(() => validateCandidate(c), /refused/); }
  // A unit listed as Linux-required needs a pinned Linux receipt: never skipped silently.
  const linuxFile = path.join(f.temp, "linux-required.json");
  fs.writeFileSync(linuxFile, JSON.stringify({ schema: 1, npm: [], crates: ["rightkit-a"] }));
  assert.throws(() => validateCandidate(clone(f.candidate), { linuxFile }), /asset pins has unexpected or missing keys/);
  const withLinux = clone(f.candidate); withLinux.assets.linux = { name: localAssetName(ID, "linux"), sha256: hex("1") };
  validateCandidate(withLinux, { linuxFile });
  assert.throws(() => validateCandidate(withLinux), /asset pins has unexpected or missing keys/);
});

test("an Actions candidate keeps its exact key list: no `source`, and a foreign source is not accepted as one", () => {
  const actions = { id: "cargo-a-1", sourceSha: SOURCE, runId: "7", runAttempt: "1", workflowId: "5", manifestSha256: hex("1"), registry: "crates", units: ["rightkit-a"], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, artifacts: { packed: { id: "1", digest: `sha256:${hex("1")}` }, macos: { id: "2", digest: `sha256:${hex("2")}`, receiptSha256: hex("3") }, windows: { id: "3", digest: `sha256:${hex("3")}`, receiptSha256: hex("4") } } };
  assert.equal(validateCandidate(clone(actions)).id, "cargo-a-1");
  assert.throws(() => validateCandidate({ ...clone(actions), source: "actions" }), /refused/);
});

test("download-and-verify refuses a wrong hash, size, redirect, extra or missing asset, and an unpublished release", async (t) => {
  for (const mutate of [
    (s) => withAsset(s, "macos", Buffer.from("tampered receipt")), (s) => { s.assetBytes.packed = Buffer.from("tampered ZIP bytes of same length!"); },
    (s) => { s.rows.find((r) => r.kind === "windows").size += 1; s.release.assets.find((a) => a.name === s.candidate.assets.windows.name).size += 1; },
    (s) => { s.redirectTo = "https://evil.example/asset"; }, (s) => { s.redirectTo = "http://release-assets.githubusercontent.com/asset"; },
    (s) => { s.release.assets.push({ id: 999, name: "extra.bin", size: 1, state: "uploaded" }); }, (s) => { s.release.assets.pop(); },
    (s) => { s.release.assets[0].state = "starter"; }, (s) => { s.release.prerelease = false; }, (s) => { s.release.draft = true; }, (s) => { s.release.tag_name = "candidate-x"; },
    (s) => { s.ref.object.sha = "d".repeat(40); }, (s) => { s.ref.object.type = "tag"; }, (s) => { s.ref.ref = "refs/tags/other"; },
    (s) => { s.comparison = { status: "diverged", merge_base_commit: { sha: SOURCE } }; }, (s) => { s.comparison = { status: "ahead", merge_base_commit: { sha: "e".repeat(40) } }; },
    (s) => { s.repository.default_branch = "dev"; }, (s) => { s.repository.full_name = "foreign/rightkit"; },
  ]) {
    const f = fixture(t, mutate); await assert.rejects(f.prepare(), /refused/); assert.equal(f.puts.length, 0);
    assert.equal(f.requests.filter((r) => r.url === CRATES_PUBLISH_URL).length, 0);
  }
});

test("a packed asset with other files than its manifest lists, or a manifest for another commit or candidate, is refused", async (t) => {
  for (const mutate of [
    (s) => { s.entries["extra.txt"] = Buffer.from("x"); }, (s) => { s.entries["manifest.json"] = Buffer.from(JSON.stringify({ ...s.manifest, commit: PROMOTION })); },
    (s) => { s.entries["manifest.json"] = Buffer.from(JSON.stringify({ ...s.manifest, candidateId: "other" })); }, (s) => { s.entries["manifest.json"] = Buffer.from(JSON.stringify({ ...s.manifest, source: "actions" })); },
    (s) => { s.entries["rightkit-a-0.1.0.crate"] = Buffer.from("other crate bytes"); },
  ]) {
    const f = fixture(t, (s) => { mutate(s); s.assetBytes.packed = Buffer.from(`ZIP ${Math.random()}`); s.candidate.assets.packed.sha256 = sha("sha256", s.assetBytes.packed); s.rows[0].size = s.assetBytes.packed.length; s.release.assets[0].size = s.assetBytes.packed.length; });
    await assert.rejects(f.prepare(), /refused/);
  }
});

test("approval is still required: missing, different, duplicate or unreviewed rows refuse before the registry", async (t) => {
  for (const mutate of [(s) => { s.approval.artifacts = []; }, (s) => { s.approval.artifacts[0].sha256 = "c".repeat(64); }, (s) => { s.approval.artifacts[0].reviewer = ""; }, (s) => { s.approval.artifacts[0].reviewer = null; }]) {
    const f = fixture(t, mutate); await assert.rejects(f.prepare(), /refused|reviewer|approv/); assert.equal(f.puts.length, 0);
  }
});

test("local receipts: every deviation from the pinned bytes, commit, hosts, scans and evidence refuses", async (t) => {
  const f = fixture(t); const manifest = f.manifest; const sha256 = f.manifestSha256; const unit = f.unit;
  const check = (mutateReceipts, options = {}) => {
    const dir = fs.mkdtempSync(path.join(f.temp, "rcpt-"));
    const docs = { macos: receiptFor("macos", unit, sha256), windows: receiptFor("windows", unit, sha256) }; mutateReceipts(docs);
    for (const [host, doc] of Object.entries(docs)) if (doc) fs.writeFileSync(path.join(dir, `${host}.json`), JSON.stringify(doc));
    return checkLocalReceipts({ manifest, manifestSha256: sha256, receiptsDir: dir, allow: options.allow ?? {}, candidate: { id: ID }, linuxRequired: options.linuxRequired ?? [] });
  };
  assert.deepEqual(check(() => {}), []);
  const refused = {
    "commit": (d) => { d.windows.commit = PROMOTION; }, "tree differs per host": (d) => { d.windows.tree = "d".repeat(40); }, "manifest hash": (d) => { d.macos.manifestSha256 = hex("1"); }, "candidate id": (d) => { d.macos.candidateId = "other"; },
    "windows packed other bytes": (d) => { d.windows.units[0].sha256 = hex("1"); d.windows.units[0].evidence.artifactSha256 = hex("1"); d.windows.units[0].evidence.runs[0].artifactSha256 = hex("1"); },
    "missing host": (d) => { delete d.windows; }, "host os": (d) => { d.windows.hostOs = "darwin"; }, "broker name": (d) => { d.macos.broker.name = "cargo"; }, "broker hash": (d) => { d.macos.broker.sha256 = "short"; }, "broker extra": (d) => { d.macos.broker.extra = 1; },
    "scans deferred": (d) => { d.windows.scans.deferred = ["clippy"]; }, "scans not passed": (d) => { d.macos.scans.passed = false; }, "scan row failed": (d) => { d.macos.scans.scans[0].status = "fail"; }, "scan wrong commit": (d) => { d.macos.scans.sourceCommit = PROMOTION; },
    "scan class A only": (d) => { d.macos.scans.classes = ["A"]; }, "scan scope": (d) => { d.macos.scans.scope.units = ["rightkit-b"]; }, "scan extra key": (d) => { d.macos.scans.extra = 1; },
    "bytes not verified": (d) => { d.macos.units[0].bytesVerified = false; }, "archive scan failed": (d) => { d.macos.units[0].archiveScan.status = "fail"; }, "status": (d) => { d.macos.units[0].status = "fail"; }, "no test status": (d) => { d.macos.units[0].status = "allowed-no-test"; },
    "pass with ignored": (d) => { d.macos.units[0].ignored = 1; d.macos.units[0].evidence.runs[0].ignored = 1; }, "executed zero": (d) => { d.macos.units[0].executed = 0; d.macos.units[0].evidence.runs[0].passed = 0; },
    "evidence host": (d) => { d.macos.units[0].evidence.runs[0].host.os = "win32"; }, "evidence policy": (d) => { d.macos.units[0].evidence.hostPolicy = "win32-only"; }, "evidence failed": (d) => { d.macos.units[0].evidence.runs[0].failed = 1; },
    "evidence missing": (d) => { delete d.macos.units[0].evidence; }, "evidence batch": (d) => { d.macos.units[0].evidence.runs[0].batch = {}; }, "evidence counts": (d) => { d.macos.units[0].evidence.runs[0].passed = 5; },
    "allowset widened": (d) => { d.macos.allowIgnored = ["rightkit-a"]; }, "unit missing": (d) => { d.macos.units = []; }, "unit extra": (d) => { d.macos.units.push({ ...d.macos.units[0], name: "rightkit-b" }); },
    "wrong kind": (d) => { d.macos.kind = "verify"; }, "qualifiedAt": (d) => { d.macos.qualifiedAt = "never"; }, "schema": (d) => { d.macos.schema = 2; }, "exclusion without allowset": (d) => { d.windows.units[0] = { name: unit.name, version: unit.version, file: unit.file, sha256: null, status: "allowed-exclusion", executed: 0, ignored: 0, skipped: 0 }; },
  };
  for (const [name, mutate] of Object.entries(refused)) assert.ok(check(mutate).length > 0, name);
  // A host that excludes the unit vouches for no bytes, but only when the candidate allowed the exclusion on both receipts.
  const excluded = (d) => { d.windows.units[0] = { name: unit.name, version: unit.version, file: unit.file, sha256: null, status: "allowed-exclusion", executed: 0, ignored: 0, skipped: 0 }; d.windows.allowHostExclusion = ["rightkit-a"]; d.macos.allowHostExclusion = ["rightkit-a"]; };
  assert.deepEqual(check(excluded, { allow: { hostExclusion: ["rightkit-a"] } }), []);
  // Linux-required units need a Linux receipt: missing means refused, never skipped.
  assert.match(check(() => {}, { linuxRequired: ["rightkit-a"] }).join("\n"), /exactly one local receipt from linux, found 0/);
  assert.deepEqual(localScanShapeProblems(scanRecord(["rightkit-a"]), { commit: SOURCE, units: ["rightkit-a"] }), []);
});

test("a receipt of another kind, an unreadable file or a foreign host in the receipts directory refuses", async (t) => {
  const f = fixture(t); const dir = fs.mkdtempSync(path.join(f.temp, "rcpt-"));
  for (const host of ["macos", "windows"]) fs.writeFileSync(path.join(dir, `${host}.json`), JSON.stringify(receiptFor(host, f.unit, f.manifestSha256)));
  const run = () => checkLocalReceipts({ manifest: f.manifest, manifestSha256: f.manifestSha256, receiptsDir: dir, candidate: { id: ID }, linuxRequired: [] });
  assert.deepEqual(run(), []);
  fs.writeFileSync(path.join(dir, "verify-linux.json"), JSON.stringify({ kind: "verify", host: "macos" })); assert.ok(run().length > 0); fs.rmSync(path.join(dir, "verify-linux.json"));
  fs.writeFileSync(path.join(dir, "junk.json"), "{not json"); assert.ok(run().length > 0); fs.rmSync(path.join(dir, "junk.json"));
  fs.writeFileSync(path.join(dir, "linux.json"), JSON.stringify(receiptFor("macos", f.unit, f.manifestSha256, { host: "linux", hostOs: "linux" }))); assert.match(run().join("\n"), /unexpected local receipt from "linux"/);
});

test("local admission verdicts: a held runtime proof, a failed unit, wrong receipts hash or foreign verifier never reach the credentialed job", async (t) => {
  const f0 = fixture(t); const base = f0.admission();
  for (const [name, mutate] of Object.entries({
    "held runtime proof": (a) => { a.units[0].runtimeFeatureProof = { classification: "always-on", listedOwner: true, required: true, status: "HOLD", failures: ["authenticated isolated runtime feature/workload proof missing"] }; },
    "qualified claim for a unit without trusted runtime coverage": (a) => { a.units[0].runtimeFeatureProof = { classification: "always-on", listedOwner: true, required: true, status: "qualified", failures: [], proof: { candidateId: ID, proofSha256: hex("1"), workloads: ["budget"] } }; },
    "failed unit": (a) => { a.units[0].ok = false; a.units[0].failures = ["x"]; }, "wrong archive": (a) => { a.units[0].archiveSha256 = hex("1"); }, "receipt hash": (a) => { a.receipts.windows = hex("1"); },
    "missing receipts host": (a) => { delete a.receipts.windows; }, "verifier": (a) => { a.verifierSha = SOURCE; }, "run": (a) => { a.runId = "1"; }, "policy": (a) => { a.units[0].policies.macos = "no-tests"; },
    "original": (a) => { a.original.sourceSha = PROMOTION; }, "extra key": (a) => { a.extra = 1; }, "kind": (a) => { a.kind = "semantic-admission"; }, "source hashes": (a) => { a.sourceHashes = {}; },
    "path hash": (a) => { a.sourceHashes = { "../x": hex("1") }; },
  })) {
    const f = fixture(t); const doc = clone(base); mutate(doc); f.setAdmission(doc);
    await assert.rejects(async () => { const prepared = await f.prepareOriginal(); await authenticateAdmission({ prepared, env: f.env, fetchImpl: f.fetchImpl, spawn: f.spawn }); }, /refused/, name);
  }
  const f = fixture(t); const prepared = await f.prepareOriginal();
  assert.throws(() => assertAuthenticatedContext(prepared.context, { env: { ...f.env, PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a" }, ...prepared, approvedFile: f.approvedFile }), /semantic admission is required/);
  await authenticateAdmission({ prepared, env: f.env, fetchImpl: f.fetchImpl, spawn: f.spawn });
  const actual = { env: { ...f.env, PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a", PUBLISH_ALLOW_HOST_EXCLUSION: "", PUBLISH_ALLOW_NO_TEST: "", PUBLISH_ALLOW_IGNORED: "", PUBLISH_ALLOW_MISSING_ASSETS: "" }, ...prepared, approvedFile: f.approvedFile };
  assertAuthenticatedContext(prepared.context, actual);
  fs.appendFileSync(path.join(prepared.receiptsDir, "windows", fs.readdirSync(path.join(prepared.receiptsDir, "windows"))[0]), " ");
  assert.throws(() => assertAuthenticatedContext(prepared.context, actual), /original native receipt changed/);
  assert.throws(() => assertAuthenticatedContext(prepared.context, { ...actual, env: { ...actual.env, PUBLISH_ALLOW_IGNORED: "rightkit-a" } }), /changed/);
});

test("tampering with the packed crate or the approval after admission refuses in dry and real mode", async (t) => {
  const f = fixture(t); const p = await f.prepare();
  fs.appendFileSync(path.join(p.packDir, "rightkit-a-0.1.0.crate"), "tampered");
  for (const dryRun of [true, false]) await assert.rejects(f.upload(p, dryRun), /changed after packing/);
  assert.equal(f.puts.length, 0);
  const g = fixture(t); const q = await g.prepare();
  fs.writeFileSync(g.approvedFile, JSON.stringify({ ...g.approval, artifacts: [] }));
  for (const dryRun of [true, false]) await assert.rejects(g.upload(q, dryRun), /sealed upload refused/);
  assert.equal(g.puts.length, 0);
});

test("checkLocalProvenance and selectCandidate work on committed local candidates only by exact ID", (t) => {
  const f = fixture(t);
  assert.equal(selectCandidate(f.approvedFile, ID).source, "local");
  assert.throws(() => selectCandidate(f.approvedFile, "https://example.test/x"), /URLs & paths/);
  assert.throws(() => selectCandidate(f.approvedFile, "missing-id"), /not committed exactly once/);
  checkLocalProvenance({ candidate: f.candidate, repository: f.repository, release: f.release, ref: f.ref, promotionSha: PROMOTION, comparison: f.comparison });
  assert.throws(() => checkLocalProvenance({ candidate: f.candidate, repository: f.repository, release: f.release, ref: f.ref, promotionSha: SOURCE, comparison: f.comparison }), /descend/);
});

// ---- a local candidate that pins its CI run (--with-ci) ----
const CI_RUN = "5150"; const CI_WORKFLOW_ID = "5";
const ciPin = (over = {}) => ({ workflowId: CI_WORKFLOW_ID, runId: CI_RUN, sourceSha: SOURCE, units: ["rightkit-a"], dispatchedAt: "2026-10-22T10:00:00Z", ...over });
const jobRow = (name, conclusion = "success", over = {}) => ({ name, status: "completed", conclusion, run_id: Number(CI_RUN), run_attempt: 1, head_sha: SOURCE, ...over });
const goodJobs = (linux = false) => [jobRow("guard"), jobRow("scans"), jobRow("plan"), jobRow("pack"), jobRow("verify (xcode-27, a)"), jobRow("verify (windows-2025, a)"), jobRow("verify-linux", linux ? "success" : "skipped")];
const ciRepo = { id: 99, full_name: "bogusyogi/rightkit" };
function ciApi({ status = "completed", conclusion = "success", jobs = goodJobs(), run = {}, workflow = {} } = {}) {
  return {
    workflow: { id: Number(CI_WORKFLOW_ID), path: ".github/workflows/publish.yml", state: "active", ...workflow },
    run: { id: Number(CI_RUN), run_attempt: 1, repository: ciRepo, head_repository: ciRepo, workflow_id: Number(CI_WORKFLOW_ID), path: ".github/workflows/publish.yml", event: "workflow_dispatch", head_branch: "main", head_sha: SOURCE, status, conclusion: status === "completed" ? conclusion : null, ...run },
    jobs: { total_count: jobs.length, jobs },
  };
}
const withCi = (s, pin = ciPin()) => { s.candidate.ci = pin; };
const proofDoc = (over = {}) => ({ runId: CI_RUN, runAttempt: 1, conclusion: "success", checkedAt: "2026-10-22T12:00:00Z", requiredUnits: [], failedJobs: [], ...over });
const heldRow = { classification: "always-on", listedOwner: true, required: true, status: "HOLD", failures: ["authenticated isolated runtime feature/workload proof missing"] };

test("a local candidate may pin its CI run: exact keys, same commit and units, a real time; allow.ciFailure needs the pin and a reason for every unit", (t) => {
  const f = fixture(t, (s) => withCi(s)); const good = validateCandidate(clone(f.candidate)); assert.deepEqual(good.ci, ciPin());
  const refused = {
    "extra key": (c) => { c.ci.extra = 1; }, "missing key": (c) => { delete c.ci.dispatchedAt; }, "no workflow": (c) => { delete c.ci.workflowId; },
    "other commit": (c) => { c.ci.sourceSha = PROMOTION; }, "zero run": (c) => { c.ci.runId = "0"; }, "text run": (c) => { c.ci.runId = "latest"; }, "negative workflow": (c) => { c.ci.workflowId = "-5"; },
    "other units": (c) => { c.ci.units = ["rightkit-b"]; }, "more units": (c) => { c.ci.units = ["rightkit-a", "rightkit-b"]; }, "no units": (c) => { c.ci.units = []; }, "duplicate units": (c) => { c.ci.units = ["rightkit-a", "rightkit-a"]; },
    "time": (c) => { c.ci.dispatchedAt = "yesterday"; }, "time without zone": (c) => { c.ci.dispatchedAt = "2026-10-22 10:00:00"; }, "not an object": (c) => { c.ci = "5150"; }, "null": (c) => { c.ci = null; },
    "ciFailure not a list": (c) => { c.allow.ciFailure = "rightkit-a"; }, "ciFailure empty": (c) => { c.allow.ciFailure = []; },
    "ciFailure bare unit": (c) => { c.allow.ciFailure = ["rightkit-a"]; }, "ciFailure no reason": (c) => { c.allow.ciFailure = [{ unit: "rightkit-a" }]; }, "ciFailure short reason": (c) => { c.allow.ciFailure = [{ unit: "rightkit-a", reason: "no" }]; },
    "ciFailure padded reason": (c) => { c.allow.ciFailure = [{ unit: "rightkit-a", reason: " flaky windows runner " }]; }, "ciFailure long reason": (c) => { c.allow.ciFailure = [{ unit: "rightkit-a", reason: "x".repeat(501) }]; },
    "ciFailure foreign unit": (c) => { c.allow.ciFailure = [{ unit: "rightkit-b", reason: "flaky windows runner" }]; }, "ciFailure twice": (c) => { const row = { unit: "rightkit-a", reason: "flaky windows runner" }; c.allow.ciFailure = [row, row]; },
    "ciFailure extra key": (c) => { c.allow.ciFailure = [{ unit: "rightkit-a", reason: "flaky windows runner", by: "me" }]; },
  };
  for (const [name, mutate] of Object.entries(refused)) { const c = clone(f.candidate); mutate(c); assert.throws(() => validateCandidate(c), /refused/, name); }
  const allowed = clone(f.candidate); allowed.allow.ciFailure = [{ unit: "rightkit-a", reason: "flaky windows runner, rerun is green" }]; assert.equal(validateCandidate(allowed).allow.ciFailure[0].unit, "rightkit-a");
  const noPin = clone(f.candidate); delete noPin.ci; noPin.allow.ciFailure = [{ unit: "rightkit-a", reason: "flaky windows runner" }]; assert.throws(() => validateCandidate(noPin), /needs a ci run pin/);
  const stray = clone(f.candidate); stray.allow.other = []; assert.throws(() => validateCandidate(stray), /allowsets has unexpected/);
  // An Actions candidate never carries one.
  assert.throws(() => validateCandidate({ id: "cargo-a-1", sourceSha: SOURCE, runId: "7", runAttempt: "1", workflowId: "5", manifestSha256: hex("1"), registry: "crates", units: ["rightkit-a"], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, artifacts: {}, ci: ciPin() }), /refused/);
  // A Linux-required unit is qualified on Linux by the pinned run, so a candidate that pins one has no Linux receipt asset; without the pin it still needs it.
  const linuxFile = path.join(f.temp, "linux-required.json"); fs.writeFileSync(linuxFile, JSON.stringify({ schema: 1, npm: [], crates: ["rightkit-a"] }));
  validateCandidate(clone(f.candidate), { linuxFile });
  const withLinuxAsset = clone(f.candidate); withLinuxAsset.assets.linux = { name: localAssetName(ID, "linux"), sha256: hex("1") };
  assert.throws(() => validateCandidate(withLinuxAsset, { linuxFile }), /asset pins has unexpected or missing keys/);
  const plain = clone(f.candidate); delete plain.ci; assert.throws(() => validateCandidate(plain, { linuxFile }), /asset pins has unexpected or missing keys/);
});

test("the CI run is checked like an Actions candidate's run: pinned workflow, run, commit, branch, event and repository; pending, success and failure with the failed jobs", (t) => {
  const f = fixture(t, (s) => withCi(s)); const base = { candidate: f.candidate, repository: f.repository };
  const check = (api, extra = {}) => checkCiRun({ ...base, ...api, ...extra });
  assert.deepEqual(check(ciApi()), { runId: CI_RUN, runAttempt: 1, conclusion: "success", failedJobs: [] });
  assert.deepEqual(check(ciApi({ status: "in_progress" })), { runId: CI_RUN, runAttempt: 1, conclusion: "pending", failedJobs: [] });
  assert.deepEqual(check(ciApi({ status: "queued" })).conclusion, "pending");
  const red = check(ciApi({ conclusion: "failure", jobs: [...goodJobs().slice(0, 4), jobRow("verify (xcode-27, a)"), jobRow("verify (windows-2025, a)", "failure"), jobRow("verify-linux", "skipped")] }));
  assert.deepEqual(red, { runId: CI_RUN, runAttempt: 1, conclusion: "failure", failedJobs: ["verify (windows-2025, a)"] });
  assert.deepEqual(check(ciApi({ conclusion: "cancelled", jobs: goodJobs() })).failedJobs, ["run cancelled"], "a cancelled run with no failed job still names why");
  assert.equal(check(ciApi({ conclusion: "timed_out" })).conclusion, "failure");
  for (const [name, api] of Object.entries({
    "other workflow id": ciApi({ workflow: { id: 6 } }), "workflow path": ciApi({ workflow: { path: ".github/workflows/other.yml" } }), "workflow disabled": ciApi({ workflow: { state: "disabled_manually" } }),
    "other run": ciApi({ run: { id: 5151 } }), "other commit": ciApi({ run: { head_sha: PROMOTION } }), "other branch": ciApi({ run: { head_branch: "dev" } }), "pushed not dispatched": ciApi({ run: { event: "push" } }),
    "foreign repository": ciApi({ run: { repository: { id: 100, full_name: "evil/rightkit" } } }), "foreign head repository": ciApi({ run: { head_repository: { id: 100, full_name: "evil/rightkit" } } }),
    "other run workflow": ciApi({ run: { workflow_id: 6 } }), "other run path": ciApi({ run: { path: ".github/workflows/other.yml" } }),
    "job of another commit": ciApi({ jobs: [...goodJobs().slice(0, 6), jobRow("verify-linux", "skipped", { head_sha: PROMOTION })] }), "job of another run": ciApi({ jobs: [...goodJobs().slice(0, 6), jobRow("verify-linux", "skipped", { run_id: 1 })] }),
    "incomplete jobs": { ...ciApi(), jobs: { total_count: 9, jobs: goodJobs() } }, "no jobs listing": { ...ciApi(), jobs: null },
    "success without the pack job": ciApi({ jobs: goodJobs().filter((job) => job.name !== "pack") }), "success with a failed verify": ciApi({ jobs: [...goodJobs().slice(0, 5), jobRow("verify (windows-2025, a)", "skipped"), goodJobs()[6]] }),
    "success with one verify": ciApi({ jobs: goodJobs().filter((job) => !/windows/.test(job.name)) }), "success with an unplanned linux job": ciApi({ jobs: goodJobs(true) }),
  })) assert.throws(() => check(api), /refused/, name);
  assert.throws(() => checkCiRun({ candidate: { ...f.candidate, ci: undefined }, repository: f.repository, ...ciApi() }), /pins no ci run/);
  assert.throws(() => check(ciApi(), { repository: { ...f.repository, default_branch: "dev" } }), /wrong repository/);
  // A Linux-required unit's run must carry a successful Linux job, and one that was skipped is not that proof.
  const linuxFile = path.join(f.temp, "linux-required.json"); fs.writeFileSync(linuxFile, JSON.stringify({ schema: 1, npm: [], crates: ["rightkit-a"] }));
  assert.equal(check(ciApi({ jobs: goodJobs(true) }), { linuxFile }).conclusion, "success");
  assert.throws(() => check(ciApi({ jobs: goodJobs(false) }), { linuxFile }), /Linux verify job must succeed/);
  assert.deepEqual(ciRequiredUnits({ candidate: f.candidate, linuxFile }), ["rightkit-a"]);
  assert.deepEqual(ciRequiredUnits({ candidate: f.candidate }), [], "no crate is Linux-required today");
});

test("the CI policy: pending proceeds on the local bytes, a failure is refused naming the failed jobs unless every unit is allowed with a reason, and a unit only CI can prove needs success", (t) => {
  const f = fixture(t, (s) => withCi(s)); const candidate = clone(f.candidate);
  const policy = (proof, over = {}) => ciPolicy({ candidate: over.candidate ?? candidate, proof: proofDoc(proof), requiredUnits: over.requiredUnits ?? [] });
  policy({}); policy({ conclusion: "pending" });
  assert.throws(() => policy({ conclusion: "failure", failedJobs: ["verify (windows-2025, a)", "pack"] }), /failed \(failed jobs: verify \(windows-2025, a\), pack\).*allow\.ciFailure/);
  const allowed = clone(candidate); allowed.allow.ciFailure = [{ unit: "rightkit-a", reason: "flaky windows runner, rerun is green" }];
  policy({ conclusion: "failure", failedJobs: ["pack"] }, { candidate: allowed });
  const two = clone(allowed); two.units = ["rightkit-a", "rightkit-b"]; assert.throws(() => policy({ conclusion: "failure", failedJobs: ["pack"] }, { candidate: two }), /every unit/, "a partial allowance is not an allowance");
  // A unit CI alone proves: success only; neither a pending run nor an allowance stands in for it.
  policy({}, { requiredUnits: ["rightkit-a"] });
  assert.throws(() => policy({ conclusion: "pending" }, { requiredUnits: ["rightkit-a"] }), /success is required for rightkit-a.*cannot prove/);
  assert.throws(() => policy({ conclusion: "failure", failedJobs: ["verify-linux"] }, { requiredUnits: ["rightkit-a"], candidate: allowed }), /failed jobs: verify-linux.*success is required for rightkit-a/);
});

test("ciProofFor reads the pinned run with the promotion's token, records the verdict, and applies the policy before any admission is written", async (t) => {
  const f = fixture(t, (s) => withCi(s)); const asked = [];
  const rig = (api) => async (url, options) => {
    const route = String(url).slice("https://api.github.com/repos/bogusyogi/rightkit/".length); asked.push(route);
    assert.equal(options.headers.authorization, "Bearer github-read-fixture");
    const docs = { "actions/workflows/publish.yml": api.workflow, [`actions/runs/${CI_RUN}`]: api.run, [`actions/runs/${CI_RUN}/attempts/1/jobs?per_page=100`]: api.jobs };
    if (!Object.hasOwn(docs, route) || docs[route] === null) throw new Error(`unexpected API route ${route}`);
    return { ok: true, status: 200, json: async () => docs[route] };
  };
  const proof = (api, over = {}) => ciProofFor({ candidate: over.candidate ?? f.candidate, repository: f.repository, env: f.env, fetchImpl: rig(api), now: () => "2026-10-22T12:30:00Z", linuxFile: over.linuxFile });
  const ok = await proof(ciApi());
  assert.deepEqual(ok, proofDoc({ checkedAt: "2026-10-22T12:30:00Z" })); assert.deepEqual(Object.keys(ok), ["runId", "runAttempt", "conclusion", "checkedAt", "requiredUnits", "failedJobs"]);
  assert.deepEqual(asked, ["actions/workflows/publish.yml", `actions/runs/${CI_RUN}`, `actions/runs/${CI_RUN}/attempts/1/jobs?per_page=100`]);
  asked.length = 0;
  const pending = await proof({ ...ciApi({ status: "in_progress" }), jobs: null });
  assert.equal(pending.conclusion, "pending"); assert.deepEqual(asked, ["actions/workflows/publish.yml", `actions/runs/${CI_RUN}`], "a run still in progress needs no job listing");
  const failedJobs = [...goodJobs().slice(0, 5), jobRow("verify (windows-2025, a)", "failure"), jobRow("verify-linux", "skipped")];
  await assert.rejects(proof(ciApi({ conclusion: "failure", jobs: failedJobs })), /ci run 5150 failed \(failed jobs: verify \(windows-2025, a\)\).*allow\.ciFailure/);
  const allowed = clone(f.candidate); allowed.allow.ciFailure = [{ unit: "rightkit-a", reason: "flaky windows runner, rerun is green" }];
  const red = await proof(ciApi({ conclusion: "failure", jobs: failedJobs }), { candidate: allowed });
  assert.deepEqual([red.conclusion, red.failedJobs], ["failure", ["verify (windows-2025, a)"]]);
  // A Linux-required unit: CI is the only Linux proof, so pending and failure are both refused, even with an allowance.
  const linuxFile = path.join(f.temp, "linux-required.json"); fs.writeFileSync(linuxFile, JSON.stringify({ schema: 1, npm: [], crates: ["rightkit-a"] }));
  await assert.rejects(proof({ ...ciApi({ status: "in_progress" }), jobs: null }, { linuxFile }), /success is required for rightkit-a/);
  await assert.rejects(proof(ciApi({ conclusion: "failure", jobs: failedJobs }), { linuxFile, candidate: allowed }), /success is required for rightkit-a/);
  assert.deepEqual((await proof(ciApi({ jobs: goodJobs(true) }), { linuxFile })).requiredUnits, ["rightkit-a"]);
  await assert.rejects(proof(ciApi({ jobs: goodJobs(false) }), { linuxFile }), /Linux verify job must succeed/);
  await assert.rejects(proof(ciApi({ run: { head_sha: PROMOTION } })), /refused/);
});

test("promotion admits a local candidate with a CI proof: pending and success proceed, the proof rides into the upload receipt, and every other shape never reaches the credentialed job", async (t) => {
  const linuxFile = path.join(fs.mkdtempSync(path.join(process.env.RIGHTKIT_TEST_SCRATCH ?? os.tmpdir(), "rk-linux-")), "linux-required.json");
  fs.writeFileSync(linuxFile, JSON.stringify({ schema: 1, npm: [], crates: ["rightkit-a"] })); t.after(() => fs.rmSync(path.dirname(linuxFile), { recursive: true, force: true }));
  const run = async (mutateAdmission, mutateState = () => {}) => {
    const f = fixture(t, (s) => { withCi(s); mutateState(s); });
    const doc = clone(f.admission()); mutateAdmission(doc, f); f.setAdmission(doc);
    const prepared = await f.prepareOriginal(); const admission = await authenticateAdmission({ prepared, env: f.env, fetchImpl: f.fetchImpl, spawn: f.spawn });
    return { f, prepared, admission };
  };
  const refusedWith = async (mutateAdmission, pattern, mutateState) => assert.rejects(run(mutateAdmission, mutateState), pattern);
  const sealed = await run(() => {});
  assert.deepEqual(sealed.admission.ciProof, proofDoc());
  const dry = await sealed.f.upload(sealed.prepared, true); const real = await sealed.f.upload(sealed.prepared, false);
  assert.deepEqual(real.ciProof, proofDoc(), "the publication receipt carries the proof the admission recorded"); assert.deepEqual(dry.ciProof, real.ciProof);
  assert.deepEqual(real.candidate.ci, ciPin()); assert.equal(sealed.f.puts.length, 1);
  const pending = await run((a) => { a.ciProof = proofDoc({ conclusion: "pending" }); });
  assert.equal((await pending.f.upload(pending.prepared, false)).ciProof.conclusion, "pending", "pending proceeds: the local bytes are the approved ones");
  // A failed run: refused, naming the jobs, unless the committed candidate allows it for every unit with a reason.
  const failure = (a) => { a.ciProof = proofDoc({ conclusion: "failure", failedJobs: ["verify (windows-2025, a)"] }); };
  await refusedWith(failure, /ci run 5150 failed \(failed jobs: verify \(windows-2025, a\)\)/);
  const allow = (s) => { s.candidate.allow.ciFailure = [{ unit: "rightkit-a", reason: "flaky windows runner, rerun is green" }]; };
  const tolerated = await run(failure, allow); assert.equal((await tolerated.f.upload(tolerated.prepared, false)).ciProof.conclusion, "failure");
  // A Linux-required unit: success or nothing, whatever the allowance says.
  const linux = (s) => { s.linuxFile = linuxFile; };
  const needed = (a) => { a.ciProof = proofDoc({ requiredUnits: ["rightkit-a"] }); };
  const linuxAdmission = await run(needed, linux); assert.deepEqual(linuxAdmission.admission.ciProof.requiredUnits, ["rightkit-a"]);
  assert.equal((await linuxAdmission.f.upload(linuxAdmission.prepared, false)).ciProof.conclusion, "success");
  await refusedWith((a) => { needed(a); a.ciProof.conclusion = "pending"; }, /success is required for rightkit-a/, linux);
  await refusedWith((a) => { failure(a); a.ciProof.requiredUnits = ["rightkit-a"]; }, /success is required for rightkit-a/, (s) => { linux(s); allow(s); });
  await refusedWith(() => {}, /other required units/, linux);
  await refusedWith((a) => { needed(a); }, /other required units/);
  // A runtime proof is never CI's to give: a held proof is refused with or without the pin.
  await refusedWith((a) => { a.units[0].runtimeFeatureProof = clone(heldRow); }, /local runtime feature proof failed/);
  for (const [name, mutate] of Object.entries({
    "no proof": (a) => { delete a.ciProof; }, "other run": (a) => { a.ciProof.runId = "5151"; }, "extra key": (a) => { a.ciProof.extra = 1; }, "missing key": (a) => { delete a.ciProof.failedJobs; },
    "unknown conclusion": (a) => { a.ciProof.conclusion = "cancelled"; }, "no time": (a) => { a.ciProof.checkedAt = "soon"; }, "bad attempt": (a) => { a.ciProof.runAttempt = 0; },
    "success naming failed jobs": (a) => { a.ciProof.failedJobs = ["pack"]; }, "failure naming none": (a) => { a.ciProof.conclusion = "failure"; },
    "proof is not an object": (a) => { a.ciProof = "success"; },
  })) await refusedWith(mutate, /refused/, undefined).catch((error) => { throw new Error(`${name}: ${error.message}`); });
  // A candidate that pins no run has no proof to carry, and a held proof stays a refusal for it.
  const plain = fixture(t); const doc = clone(plain.admission()); doc.ciProof = proofDoc(); plain.setAdmission(doc);
  await assert.rejects(async () => { const prepared = await plain.prepareOriginal(); await authenticateAdmission({ prepared, env: plain.env, fetchImpl: plain.fetchImpl, spawn: plain.spawn }); }, /refused/);
  const heldPlain = fixture(t); const heldDoc = clone(heldPlain.admission()); heldDoc.units[0].runtimeFeatureProof = clone(heldRow); heldPlain.setAdmission(heldDoc);
  await assert.rejects(async () => { const prepared = await heldPlain.prepareOriginal(); await authenticateAdmission({ prepared, env: heldPlain.env, fetchImpl: heldPlain.fetchImpl, spawn: heldPlain.spawn }); }, /refused/);
  const context = (await heldPlain.prepareOriginal()).context; assert.throws(() => ciProofOf(context), /semantic admission is required/);
});

test("a candidate that pins its CI run keeps the allowsets the receipts and the source loader know: ciFailure never reaches either", (t) => {
  const f = fixture(t, (s) => { withCi(s); s.candidate.allow.ciFailure = [{ unit: "rightkit-a", reason: "flaky windows runner, rerun is green" }]; });
  assert.deepEqual(localAllow(f.candidate), { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] });
  assert.deepEqual(localLinuxPolicy(f.candidate), { linuxRequired: [] }); assert.deepEqual(localLinuxPolicy({ ...f.candidate, ci: undefined }), {});
  assert.equal(selectCandidate(f.approvedFile, ID).ci.runId, CI_RUN);
});
