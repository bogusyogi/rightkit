// Admission of a gate-package release: real ES256 signatures, real hashes, no broker and no network.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { admitGateRelease, admitFromFiles, gateReleaseGuard, GATE_PACKAGE, GATE_RECEIPT_ENV } from "./gate-release-admission.mjs";
import { NN_ALL_IDS, CONTRACT_RECEIPT_SCHEMA, CONTRACT_SIGNED_LAUNCHES, archiveIdentity, archiveOfSeal } from "./gate-acceptance.mjs";
import { contractJobNonce } from "./gate-seam.mjs";
import { claimSignedBytes, signedBytes, verifyPublicationClaim, PUBLICATION_CLAIM_SCHEMA } from "./receipt-signature.mjs";

const sha = (b) => createHash("sha256").update(b).digest("hex");
function makeKey(host, brokerDigest) {
  const { publicKey, privateKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const publicKeyHex = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  return { privateKey, pin: { publicKey: publicKeyHex, keyId: sha(Buffer.from(publicKeyHex, "hex")), host, brokerDigest } };
}
const BROKER = "d".repeat(64); const HOST = "BOGUS-DELL";
const key = makeKey(HOST, BROKER);
const RUN = "1".repeat(64); const COMMIT = "c".repeat(40); const FIXTURE = "f".repeat(40); const EDIT1 = "a".repeat(40); const EDIT2 = "b".repeat(40);
const archiveBytes = Buffer.from("the packed gate archive"); const archive = archiveIdentity(archiveBytes);
// The sealed gate: its source record names the archive, its manifest lists that record, and the seal is the manifest's sha256.
const sourceOf = (over = {}) => Buffer.from(JSON.stringify({ schema: "rustright.trusted-gate-source/1", package: "@rightkit/release", version: "0.6.58", integrity: archive.integrity, sha256: archive.sha256, origin: "ci-archive", ...over }));
const manifestOf = (source, extra = "") => Buffer.from(`./node_modules/@rightkit/release/package.json\tfile\t-\t${"3".repeat(64)}\n./trusted-gate-source.json\tfile\t-\t${sha(source)}\n${extra}`);
const sourceBytes = sourceOf(); const manifestBytes = manifestOf(sourceBytes); const SEALED = sha(manifestBytes);
const hashes = { contractSha256: "a".repeat(64), budgetsSha256: "b".repeat(64), runnerSha256: "e".repeat(64), runnerFiles: [{ name: "gate-acceptance.mjs", sha256: "e".repeat(64) }] };
const sealed = { signedLaunches: CONTRACT_SIGNED_LAUNCHES.map((l) => ({ label: l.label, commands: [...l.commands], exitCode: l.exitCode, commit: l.commit })) };
const pipeline = { attestation: { win32: key.pin } };

/** One broker job of the run as it is handed over: { id, publication } signed by `signer`, and its entry in the contract receipt. */
function job(step, n, { signer = key, nonce, sealedSha256 = SEALED, over = {}, payloadOver = {}, command = "clippy", exitCode = 0, commit = EDIT1 } = {}) {
  const requestId = `job-${n}`; const call = String(n).repeat(32).slice(0, 32);
  const payload = { schema: PUBLICATION_CLAIM_SCHEMA, keyId: signer.pin.keyId, host: signer.pin.host, broker: { brokerDigest: signer.pin.brokerDigest, version: "rustright-x" }, jobId: requestId,
    nonce: nonce ?? contractJobNonce({ runId: RUN, sealedSha256, step, call }), gate: { commit, tree: "9".repeat(40) }, result: { status: exitCode === 0 ? "succeeded" : "failed", exitCode }, command, payloadSha256: "5".repeat(64), ...payloadOver };
  const doc = { id: requestId, publication: { schema: PUBLICATION_CLAIM_SCHEMA, payload, signature: { alg: "ES256", keyId: signer.pin.keyId, value: sign("sha256", claimSignedBytes(payload), { key: signer.privateKey, dsaEncoding: "ieee-p1363" }).toString("hex") } } };
  const bytes = Buffer.from(JSON.stringify(doc));
  return { bytes, doc, entry: { step, call, nonce: payload.nonce, requestId, receiptSha256: "f".repeat(64), claimSha256: sha(bytes), argv: ["cargo", command], status: exitCode, ...over } };
}
function fixture(jobs, receiptOver = {}) {
  const bundle = new Map(jobs.map((j) => [j.entry.requestId, j.bytes]));
  const receipt = { schema: CONTRACT_RECEIPT_SCHEMA, host: "dell", commit: COMMIT, fixtureCommit: FIXTURE, gate: { kind: "candidate", version: "0.6.58", sealedSha256: SEALED, archiveSha256: archive.sha256, archiveIntegrity: archive.integrity },
    broker: { runtimeDigest: BROKER, runtimeVersion: "v", enrolledDigest: BROKER }, runId: RUN, jobs: jobs.map((j) => j.entry), jobReceipts: "job-receipts", jobClaims: "job-claims", gateSeal: "gate-seal", signedSteps: [], nnBasis: "runner-claim",
    ...hashes, verdict: "PASS 42/42", ok: true, nn: Object.fromEntries(NN_ALL_IDS.map((id) => [id, "pass"])), uncheckedClauses: [], ...receiptOver };
  const readJob = (id) => { if (!bundle.has(id)) throw Object.assign(new Error("absent"), { code: "ENOENT" }); return bundle.get(id); };
  return { receipt, readJob, bundle };
}
const admit = (f, over = {}) => admitGateRelease({ receipt: f.receipt, readJob: f.readJob, archiveBytes, commit: COMMIT, pipeline, sealed, manifestBytes, sourceBytes, ...hashes, ...over });
// A real compile launch sends several jobs on one commit; a warm launch elsewhere sends one.
const goodJobs = () => [job("S2:compile", 2, { commit: FIXTURE }), job("S12:crate", 3), job("S12:crate", 4, { command: "test" }), job("S12:manifest", 5, { commit: EDIT2 }), job("S12:manifest", 6, { command: "doc", commit: EDIT2 })];
const has = (r, re) => { assert.equal(r.ok, false); assert.match(r.problems.join(" | "), re); };

test("a gate release is admitted when the receipt, the archive, the committed host and every signed claim agree", () => {
  const r = admit(fixture(goodJobs()));
  assert.deepEqual(r.problems, []); assert.equal(r.ok, true);
  assert.deepEqual(r.facts, { host: "dell", version: "0.6.58", sealedSha256: SEALED, archiveSha256: archive.sha256, jobsListed: 5, jobsVerified: 5, signedSteps: ["S12", "S2"], nnBasis: "runner-claim", launchesProven: ["S12:crate", "S12:manifest"] });
  // A job the broker refused before it existed is listed, proves nothing, and is not counted.
  const withRefused = fixture(goodJobs()); withRefused.receipt.jobs.push({ step: "S7:neg", call: "9".repeat(32), nonce: "0".repeat(64), requestId: null, receiptSha256: null, claimSha256: null, argv: [], status: 125 });
  const r2 = admit(withRefused); assert.equal(r2.ok, true); assert.equal(r2.facts.jobsListed, 6); assert.equal(r2.facts.jobsVerified, 5);
});

test("the archive, the commit, the receipt and the host must be the ones in question", () => {
  has(admit(fixture(goodJobs()), { archiveBytes: Buffer.from("other bytes") }), /is for the archive [0-9a-f]{64}, the archive in question is/);
  has(admit(fixture(goodJobs()), { commit: "d".repeat(40) }), /receipt is for commit c{40}, the release is d{40}/);
  has(admit(fixture(goodJobs(), { gate: { kind: "installed", sealedSha256: SEALED } })), /not a candidate's/);
  has(admit(fixture(goodJobs(), { verdict: "NOT PASSED", ok: false })), /is not a PASS/);
  has(admit(fixture(goodJobs(), { nn: { NN01: "pass" } })), /not the contract's \d+/);
  has(admit(fixture(goodJobs(), { uncheckedClauses: undefined })), /does not state which operative clauses/);
  has(admit(fixture(goodJobs(), { contractSha256: "0".repeat(64) })), /contract changed/);
  has(admit(fixture(goodJobs(), { budgetsSha256: "0".repeat(64) })), /other budgets/);
  has(admit(fixture(goodJobs(), { runnerFiles: [] })), /runner files are not this runner's/);
  // The host: unknown, not in the committed list, or running a broker that is not the committed one (loop matrix C9).
  has(admit(fixture(goodJobs(), { host: "linux" })), /not a host this repository names/);
  has(admit(fixture(goodJobs()), { pipeline: { attestation: {} } }), /no complete attestation\.win32/);
  has(admit(fixture(goodJobs(), { broker: { runtimeDigest: "9".repeat(64) } })), /made with the broker 9{64}, this host runs d{64}/);
});

test("the publication claim: only the small signed statement is accepted, with exactly its fields, under its own signing line", () => {
  const j = job("S12:crate", 3);
  assert.equal(verifyPublicationClaim(j.doc, key.pin, "job-3").command, "clippy");
  const code = (fn) => { try { fn(); return null; } catch (e) { return e.code; } };
  // A full envelope's bytes signed by the same key do not verify as a claim: the first signed line differs.
  { const p = j.doc.publication.payload; const asEnvelope = { id: "job-3", publication: { schema: PUBLICATION_CLAIM_SCHEMA, payload: p, signature: { alg: "ES256", keyId: key.pin.keyId, value: sign("sha256", signedBytes(p), { key: key.privateKey, dsaEncoding: "ieee-p1363" }).toString("hex") } } };
    assert.equal(code(() => verifyPublicationClaim(asEnvelope, key.pin, "job-3")), "signature"); }
  // A claim carrying anything beyond the ten fields is refused even under a valid signature: nothing private can ride along.
  { const extra = job("S12:crate", 3, { payloadOver: { outputDir: "D:\\private" } }); assert.equal(code(() => verifyPublicationClaim(extra.doc, key.pin, "job-3")), "malformed");
    try { verifyPublicationClaim(extra.doc, key.pin, "job-3"); } catch (e) { assert.match(e.message, /carries fields it may not: outputDir/); } }
  assert.equal(code(() => verifyPublicationClaim({ id: "job-3" }, key.pin, "job-3")), "legacy");
  assert.equal(code(() => verifyPublicationClaim(j.doc, makeKey(HOST, BROKER).pin, "job-3")), "key");
  assert.equal(code(() => verifyPublicationClaim(j.doc, { ...key.pin, host: "OTHER" }, "job-3")), "host");
  assert.equal(code(() => verifyPublicationClaim(j.doc, { ...key.pin, brokerDigest: "7".repeat(64) }, "job-3")), "runtime");
  assert.equal(code(() => verifyPublicationClaim(j.doc, key.pin, "job-9")), "job");
  { const t = JSON.parse(JSON.stringify(j.doc)); t.publication.payload.result.exitCode = 1; assert.equal(code(() => verifyPublicationClaim(t, key.pin, "job-3")), "signature"); }
  // Admission: a whole receipt or a full envelope in the bundle is refused; a job whose broker made no claim cannot be checked here.
  const one = (bad) => fixture([...goodJobs().filter((x) => x.entry.requestId !== bad.entry.requestId), bad]);
  { const f = one(job("S12:crate", 3)); const whole = Buffer.from(JSON.stringify({ ...JSON.parse(String(f.bundle.get("job-3"))), provenance: { payload: { br19Evidence: { environment: { dropped: ["SOME_NAME"] } } } } })); f.bundle.set("job-3", whole); f.receipt.jobs.find((x) => x.requestId === "job-3").claimSha256 = sha(whole); has(admit(f), /holds more than the claim \(only id and publication may be handed over\)/); }
  has(admit(one(job("S12:crate", 3, { over: { claimSha256: null } }))), /job job-3 \(S12:crate\): it carries no publication claim \(its broker made none\)/);
});

test("the run's broker jobs: none listed, and every way one job can fail to verify", () => {
  has(admit(fixture([])), /lists no broker job at all: nothing in it is signed/);
  has(admit(fixture(goodJobs(), { jobs: undefined })), /jobs is missing/);
  has(admit(fixture(goodJobs(), { runId: undefined })), /carries no run id/);
  const one = (bad) => fixture([...goodJobs().filter((x) => x.entry.requestId !== bad.entry.requestId), bad]);
  { const f = fixture(goodJobs()); f.bundle.delete("job-4"); has(admit(f), /job job-4 \(S12:crate\): its claim is not in the bundle \(ENOENT\)/); }
  { const f = fixture(goodJobs()); f.bundle.set("job-4", Buffer.from('{"id":"job-4","edited":true}')); has(admit(f), /not the one listed \(sha256 differs\)/); }
  has(admit(one(job("S12:crate", 4, { signer: makeKey(HOST, BROKER) }))), /job job-4 .*not signed by the pinned key/);
  has(admit(one(job("S12:crate", 4, { signer: { ...key, pin: { ...key.pin, host: "OTHER-HOST" } } }))), /job job-4 .*signed for another host/);
  has(admit(one(job("S12:crate", 4, { signer: { ...key, pin: { ...key.pin, brokerDigest: "7".repeat(64) } } }))), /job job-4 .*signed by another broker runtime/);
  has(admit(one(job("S12:crate", 4, { payloadOver: { jobId: "job-999" } }))), /job job-4 .*for another job/);
  // A real signed job of another run, of another gate, or of another launch, passed off as this one's.
  has(admit(one(job("S12:crate", 4, { nonce: contractJobNonce({ runId: "9".repeat(64), sealedSha256: SEALED, step: "S12:crate", call: "4".repeat(32) }) }))), /nonce the broker signed is not this run's/);
  has(admit(one(job("S12:crate", 4, { sealedSha256: "9".repeat(64) }))), /nonce the broker signed is not this run's/);
  has(admit(one(job("S2:compile", 4, { over: { step: "S12:crate" } }))), /nonce the broker signed is not this run's/);
  has(admit(one(job("S12:crate", 4, { over: { requestId: "../x" } }))), /cannot name a file/);
});

test("the sealed launches: each must show its own kind of work, its expected result and one commit made for it; the receipt cannot choose them", () => {
  // A launch that cannot be a cache hit shows no verified job.
  has(admit(fixture(goodJobs().filter((j) => !j.entry.step.startsWith("S12:manifest")))), /launch S12:manifest cannot be served from a cache and shows no verified broker job/);
  has(admit(fixture(goodJobs()), { sealed: {} }), /seals no launches that must show signed jobs/);
  // The receipt's own lists are ignored: it cannot name its launches or steps as proven.
  has(admit(fixture(goodJobs().filter((j) => !j.entry.step.startsWith("S12:manifest")), { signedSteps: ["S12"], launchesProven: ["S12:crate", "S12:manifest"] })), /launch S12:manifest cannot be served/);
  // A signed job of this run filed under the launch's label, but not that launch's kind of work: a metadata call is not a compile.
  has(admit(fixture([job("S12:crate", 3, { command: "metadata" }), job("S12:manifest", 5, { commit: EDIT2 })])), /launch S12:crate: none of its jobs is one of clippy, test, build, check, doc \(found: metadata\)/);
  // A compile that failed: the broker signed a non-zero exit.
  has(admit(fixture([job("S12:crate", 3), job("S12:crate", 4, { command: "test", exitCode: 101 }), job("S12:manifest", 5, { commit: EDIT2 })])), /launch S12:crate: job job-4 \(S12:crate\) ended with exit 101, the launch requires 0/);
  // Not on a commit made for the launch: on the fixture commit, on two commits, or on none.
  has(admit(fixture([job("S12:crate", 3, { commit: FIXTURE }), job("S12:manifest", 5, { commit: EDIT2 })])), /launch S12:crate: its jobs ran on the fixture commit, not on a commit made for this launch/);
  has(admit(fixture([job("S12:crate", 3), job("S12:crate", 4, { commit: EDIT2 }), job("S12:manifest", 5, { commit: EDIT2 })])), /launch S12:crate: its jobs name 2 different commits/);
  has(admit(fixture([job("S12:crate", 3, { payloadOver: { gate: null } }), job("S12:manifest", 5, { commit: EDIT2 })])), /launch S12:crate: job job-3 \(S12:crate\) names no commit/);
  // A launch sealed to run on the fixture commit is held to it.
  const fixed = { signedLaunches: [{ label: "S2:compile", commands: ["clippy"], exitCode: 0, commit: "fixture" }] };
  assert.equal(admit(fixture(goodJobs()), { sealed: fixed }).ok, true);
  has(admit(fixture([job("S2:compile", 2, { commit: EDIT1 })]), { sealed: fixed }), /launch S2:compile: its jobs did not run on the fixture commit the receipt states/);
});

test("the archive is tied to the tested gate by the seal itself, not by what the receipt says", () => {
  assert.deepEqual(archiveOfSeal({ sealedSha256: SEALED, manifestBytes, sourceBytes }), { ok: true, problems: [], archive: { sha256: archive.sha256, integrity: archive.integrity, version: "0.6.58" } });
  has(admit(fixture(goodJobs()), { manifestBytes: null, sourceBytes: null }), /nothing ties the archive to the seal but the receipt's own statement/);
  // A receipt that claims the archive in question for a gate that was really installed from another archive. Its claim matches; the seal does not.
  const otherSource = sourceOf({ sha256: "9".repeat(64) });
  has(admit(fixture(goodJobs()), { sourceBytes: otherSource }), /seal to archive: the source record handed over is not the one under the seal/);
  // The same forgery done consistently (a manifest listing the other record) no longer hashes to the tested seal, which every signed nonce carries.
  has(admit(fixture(goodJobs()), { sourceBytes: otherSource, manifestBytes: manifestOf(otherSource) }), /seal to archive: the manifest handed over hashes to [0-9a-f]{64}, the tested gate's seal is/);
  // A gate really sealed from another archive, tested for real: every signature verifies for that seal, and the archive in question is refused.
  { const m = manifestOf(otherSource); const seal2 = sha(m);
    const jobs = [job("S12:crate", 3, { sealedSha256: seal2 }), job("S12:manifest", 5, { sealedSha256: seal2, commit: EDIT2 })];
    const f = fixture(jobs, { gate: { kind: "candidate", version: "0.6.58", sealedSha256: seal2, archiveSha256: archive.sha256, archiveIntegrity: archive.integrity } });
    has(admit(f, { manifestBytes: m, sourceBytes: otherSource }), /seal to archive: the tested gate was installed from the archive 9{64}, the archive in question is/); }
  has(admit(fixture(goodJobs()), { manifestBytes: Buffer.from("./a\tfile\t-\t" + "3".repeat(64) + "\n") }), /lists trusted-gate-source\.json 0 time\(s\)/);
  assert.match(archiveOfSeal({ sealedSha256: SEALED, manifestBytes: manifestOf(sourceBytes, `./trusted-gate-source.json\tfile\t-\t${sha(sourceBytes)}\n`), sourceBytes }).problems.join(" | "), /2 time\(s\)/);
  assert.match(archiveOfSeal({ sealedSha256: SEALED, manifestBytes: Buffer.from("./trusted-gate-source.json\tdir\t-\t-\n"), sourceBytes }).problems.join(" | "), /not a file with a sha256/);
  for (const bad of [{ schema: "x/1" }, { package: "@rightkit/other" }, { integrity: "md5-x" }]) { const b = sourceOf(bad); assert.match(archiveOfSeal({ sealedSha256: sha(manifestOf(b)), manifestBytes: manifestOf(b), sourceBytes: b }).problems.join(" | "), /is not a rustright\.trusted-gate-source\/1/, JSON.stringify(bad)); }
  { const b = sourceOf({ version: "0.6.57" }); const m = manifestOf(b); const seal2 = sha(m);
    const jobs = [job("S12:crate", 3, { sealedSha256: seal2 }), job("S12:manifest", 5, { sealedSha256: seal2, commit: EDIT2 })];
    has(admit(fixture(jobs, { gate: { kind: "candidate", version: "0.6.58", sealedSha256: seal2, archiveSha256: archive.sha256, archiveIntegrity: archive.integrity } }), { manifestBytes: m, sourceBytes: b }), /source record is for version 0\.6\.57, the receipt says 0\.6\.58/); }
});

test("the real command and the publication entries: refused by name without an admitting receipt; a record is written only when admitted", async () => {
  const script = fileURLToPath(new URL("./gate-release-admission.mjs", import.meta.url));
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-adm-"));
  try {
    const run = (...args) => { const r = spawnSync(process.execPath, [script, ...args], { encoding: "utf8", timeout: 60000, windowsHide: true }); return { code: r.status, text: `${r.stdout}${r.stderr}` }; };
    const none = run(); assert.equal(none.code, 1); assert.match(none.text, /--receipt <contract-test receipt> is required/); assert.match(none.text, /--archive .* is required/); assert.match(none.text, /--commit <40-hex release commit> is required/);
    assert.equal(run("--nope").code, 64);
    // A receipt for another archive, read from disk with its folders, against this checkout: refused, it names the archive, writes no record.
    const f = fixture(goodJobs());
    const receiptFile = path.join(dir, "contract-test-receipt.dell.json"); fs.writeFileSync(receiptFile, JSON.stringify(f.receipt));
    fs.mkdirSync(path.join(dir, "job-claims")); for (const [id, bytes] of f.bundle) fs.writeFileSync(path.join(dir, "job-claims", `${id}.json`), bytes);
    fs.mkdirSync(path.join(dir, "gate-seal")); fs.writeFileSync(path.join(dir, "gate-seal", "manifest.txt"), manifestBytes); fs.writeFileSync(path.join(dir, "gate-seal", "trusted-gate-source.json"), sourceBytes);
    const archiveFile = path.join(dir, "pkg.tgz"); fs.writeFileSync(archiveFile, "other bytes");
    const pipelineFile = path.join(dir, "rk-pipeline.json"); fs.writeFileSync(pipelineFile, JSON.stringify(pipeline));
    const recFile = path.join(dir, "admission.json");
    const refused = run("--receipt", receiptFile, "--archive", archiveFile, "--commit", COMMIT, "--pipeline", pipelineFile, "--record", recFile);
    assert.equal(refused.code, 1); assert.match(refused.text, /gate-release-admission REFUSED/); assert.match(refused.text, /the archive in question is/);
    assert.doesNotMatch(refused.text, /its claim is not in the bundle|nothing ties the archive/, "the folders beside the receipt were read");
    assert.equal(fs.existsSync(recFile), false, "a refused admission writes no record");
    fs.writeFileSync(receiptFile, JSON.stringify({ ...f.receipt, jobClaims: "../elsewhere" }));
    assert.match(run("--receipt", receiptFile, "--archive", archiveFile, "--commit", COMMIT, "--pipeline", pipelineFile).text, /names its job claims folder as "\.\.\/elsewhere"/);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }

  // The guard a publish entry calls.
  assert.equal(GATE_PACKAGE, "@rightkit/release"); assert.equal(GATE_RECEIPT_ENV, "RK_GATE_CONTRACT_RECEIPT");
  let asked = 0; const never = () => { asked += 1; return { ok: false, problems: ["x"] }; };
  assert.deepEqual(gateReleaseGuard({ name: "@rightkit/theme", archiveFile: "/p/t.tgz", commit: COMMIT, env: {}, admit: never }), { required: false }); assert.equal(asked, 0);
  for (const env of [{}, { RK_GATE_CONTRACT_RECEIPT: "" }, null]) assert.throws(() => gateReleaseGuard({ name: GATE_PACKAGE, archiveFile: "/p/r.tgz", commit: COMMIT, env, admit: never }), /is not published without a passing contract-test receipt for this exact archive \(GATE-CONTRACT NN31\): RK_GATE_CONTRACT_RECEIPT names no receipt/);
  assert.equal(asked, 0);
  const seen = [];
  assert.throws(() => gateReleaseGuard({ name: GATE_PACKAGE, archiveFile: "/p/r.tgz", commit: COMMIT, env: { RK_GATE_CONTRACT_RECEIPT: "/h/receipt.json" }, admit: (a) => { seen.push(a); return { ok: false, problems: ["launch S12:crate shows no verified broker job"] }; } }), /does not admit this archive \(1 problem\(s\)\): launch S12:crate shows no verified broker job/);
  assert.deepEqual(seen[0], { receiptFile: "/h/receipt.json", archiveFile: "/p/r.tgz", commit: COMMIT, pipelineFile: null });
  assert.deepEqual(gateReleaseGuard({ name: GATE_PACKAGE, archiveFile: "/p/r.tgz", commit: COMMIT, env: { RK_GATE_CONTRACT_RECEIPT: "/h/receipt.json" }, admit: () => ({ ok: true, problems: [], facts: { host: "dell" } }) }), { required: true, facts: { host: "dell" } });
  const missing = admitFromFiles({ receiptFile: path.join(os.tmpdir(), "rk-no-such-receipt.json"), archiveFile: path.join(os.tmpdir(), "rk-no-such.tgz"), commit: COMMIT });
  assert.equal(missing.ok, false); assert.match(missing.problems.join(" | "), /the receipt .* cannot be read \(ENOENT\)/);
  assert.match(admitFromFiles({ receiptFile: "x", archiveFile: "y", commit: "short" }).problems.join(" | "), /no 40-hex release commit was given/);

  // Each actual entry, by its source (they are inline scripts with no seam to run them in a test; this is order in source, not a run).
  const src = (rel) => fs.readFileSync(new URL(rel, import.meta.url), "utf8");
  const upload = src("../../scripts/ci/upload.mjs");
  const guardAt = upload.indexOf("const refusal = gateAdmissionProblem({ unit, commit: manifest.commit, env });");
  assert.ok(guardAt > 0, "scripts/ci/upload.mjs requires the admission record for the sealed unit");
  assert.doesNotMatch(upload, /from "\.\.\/\.\.\/packages\//); assert.doesNotMatch(upload, /import\("\.\.\/\.\.\/packages\//);
  assert.ok(guardAt < upload.indexOf('if (dryRun) { log(`dry run: would run npm'), "before the dry-run branch"); assert.ok(guardAt < upload.indexOf('const result = spawn("npm", args,'), "before npm is started");
  const local = src("./publish-local-npm.mjs");
  const localAt = local.indexOf("gateReleaseGuard({ name: o.pkg, archiveFile: path.join(work, tgz), commit })");
  assert.ok(localAt > 0 && localAt < local.indexOf("const token = readToken();") && localAt < local.indexOf('run("npm", ["publish", path.join(work, tgz)'), "publish-local-npm.mjs calls the guard on the packed file before the token and before npm");
  const owner = src("../../.github/workflows/publish-npm-owner.yml");
  const refuseAt = owner.indexOf('if [[ "$PACKAGE" == "packages/release" ]]; then echo "::error::packages/release');
  assert.ok(refuseAt > 0 && refuseAt < owner.indexOf("          npm publish --provenance --access public --ignore-scripts"), "the hand-started workflow refuses packages/release before it publishes");
});

test("the upload entry reads an admission record: required for the gate package, bound to the exact sealed unit", async () => {
  const { gateAdmissionProblem, GATE_ADMISSION_RECORD_ENV, GATE_ADMISSION_RECORD_SCHEMA } = await import("../../scripts/ci/upload.mjs");
  const unit = { name: "@rightkit/release", version: "0.6.58", sha256: archive.sha256 };
  const record = (over = {}) => JSON.stringify({ schema: GATE_ADMISSION_RECORD_SCHEMA, ok: true, unit: unit.name, version: unit.version, archiveSha256: unit.sha256, commit: COMMIT, host: "dell", ...over });
  const check = (text, u = unit, env = { [GATE_ADMISSION_RECORD_ENV]: "/t/admission.json" }) => gateAdmissionProblem({ unit: u, commit: COMMIT, env, readFile: () => { if (text === null) throw Object.assign(new Error("absent"), { code: "ENOENT" }); return text; } });
  assert.equal(gateAdmissionProblem({ unit: { name: "@rightkit/theme", version: "1.0.0", sha256: "0".repeat(64) }, commit: COMMIT, env: {}, readFile: () => { throw new Error("must not read"); } }), null);
  assert.equal(check(record()), null);
  assert.match(check(record(), unit, {}), /is not uploaded without a passing contract-test admission .* names no admission record/);
  assert.match(check(null), /cannot be read \(ENOENT\)/); assert.match(check("not json"), /cannot be read/);
  assert.match(check(record({ ok: false })), /not a passing rightkit\.gate-release-admission\/1/); assert.match(check(record({ schema: "other/1" })), /not a passing/);
  assert.match(check(record({ archiveSha256: "9".repeat(64) })), /is for the archive 9{64}, the sealed archive is/);
  assert.match(check(record({ version: "0.6.57" })), /is for @rightkit\/release@0\.6\.57, the sealed unit is @rightkit\/release@0\.6\.58/);
  assert.match(check(record({ commit: "d".repeat(40) })), /is for commit d{40}, the sealed commit is c{40}/);
});
