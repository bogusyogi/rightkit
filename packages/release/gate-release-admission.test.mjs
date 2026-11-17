// Admission of a gate-package release: real ES256 signatures, real hashes, no broker and no network.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { admitGateRelease } from "./gate-release-admission.mjs";
import { NN_ALL_IDS, CONTRACT_RECEIPT_SCHEMA, CONTRACT_SIGNED_STEPS, archiveIdentity } from "./gate-acceptance.mjs";
import { contractJobNonce } from "./gate-seam.mjs";
import { signedBytes } from "./receipt-signature.mjs";

const sha = (b) => createHash("sha256").update(b).digest("hex");
function makeKey(host, brokerDigest) {
  const { publicKey, privateKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const publicKeyHex = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  const keyId = sha(Buffer.from(publicKeyHex, "hex"));
  return { privateKey, pin: { publicKey: publicKeyHex, keyId, host, brokerDigest } };
}
const BROKER = "d".repeat(64); const HOST = "BOGUS-DELL";
const key = makeKey(HOST, BROKER);
const RUN = "1".repeat(64); const SEALED = "2".repeat(64); const COMMIT = "c".repeat(40);
const archiveBytes = Buffer.from("the packed gate archive"); const archive = archiveIdentity(archiveBytes);
const hashes = { contractSha256: "a".repeat(64), budgetsSha256: "b".repeat(64), runnerSha256: "e".repeat(64), runnerFiles: [{ name: "gate-acceptance.mjs", sha256: "e".repeat(64) }] };
const sealed = { signedSteps: [...CONTRACT_SIGNED_STEPS] };
const pipeline = { attestation: { win32: key.pin } };

/** One signed broker job of the run: its receipt bytes and its entry in the contract receipt. */
function job(step, n, { signer = key, nonce, over = {}, payloadOver = {} } = {}) {
  const requestId = `job-${n}`; const call = String(n).repeat(32).slice(0, 32);
  const payload = { jobId: requestId, host: signer.pin.host, keyId: signer.pin.keyId, broker: { brokerDigest: signer.pin.brokerDigest }, nonce: nonce ?? contractJobNonce({ runId: RUN, sealedSha256: SEALED, step, call }), ...payloadOver };
  const receipt = { id: requestId, provenance: { schema: "rustright.provenance.v1", payload, signature: { alg: "ES256", keyId: signer.pin.keyId, value: sign("sha256", signedBytes(payload), { key: signer.privateKey, dsaEncoding: "ieee-p1363" }).toString("hex") } } };
  const bytes = Buffer.from(JSON.stringify(receipt));
  return { bytes, entry: { step, call, nonce: payload.nonce, requestId, receiptSha256: sha(bytes), argv: ["cargo", "clippy"], status: 0, ...over } };
}
function fixture(jobs, receiptOver = {}) {
  const bundle = new Map(jobs.map((j) => [j.entry.requestId, j.bytes]));
  const receipt = { schema: CONTRACT_RECEIPT_SCHEMA, host: "dell", commit: COMMIT, gate: { kind: "candidate", version: "0.6.58", sealedSha256: SEALED, archiveSha256: archive.sha256, archiveIntegrity: archive.integrity },
    broker: { runtimeDigest: BROKER, runtimeVersion: "v", enrolledDigest: BROKER }, runId: RUN, jobs: jobs.map((j) => j.entry), jobReceipts: "job-receipts", signedSteps: [], nnBasis: "runner-claim",
    ...hashes, verdict: "PASS 42/42", ok: true, nn: Object.fromEntries(NN_ALL_IDS.map((id) => [id, "pass"])), uncheckedClauses: [], ...receiptOver };
  const readJob = (id) => { if (!bundle.has(id)) throw Object.assign(new Error("absent"), { code: "ENOENT" }); return bundle.get(id); };
  return { receipt, readJob, bundle };
}
const admit = (f, over = {}) => admitGateRelease({ receipt: f.receipt, readJob: f.readJob, archiveBytes, commit: COMMIT, pipeline, sealed, ...hashes, ...over });
const goodJobs = () => [job("S11:check", 3), job("S11:compile", 4), job("S12:compile__crate_edit_", 5)];

test("a gate release is admitted when the receipt, the archive, the committed host and every signed job agree", () => {
  const r = admit(fixture(goodJobs()));
  assert.deepEqual(r.problems, []); assert.equal(r.ok, true);
  assert.deepEqual(r.facts, { host: "dell", sealedSha256: SEALED, archiveSha256: archive.sha256, jobsListed: 3, jobsVerified: 3, signedSteps: ["S11", "S12"], nnBasis: "runner-claim" });
  // A job the broker refused before it existed is listed, proves nothing, and is not counted.
  const withRefused = fixture(goodJobs()); withRefused.receipt.jobs.push({ step: "S7:neg", call: "9".repeat(32), nonce: "0".repeat(64), requestId: null, receiptSha256: null, argv: [], status: 125 });
  const r2 = admit(withRefused); assert.equal(r2.ok, true); assert.equal(r2.facts.jobsListed, 4); assert.equal(r2.facts.jobsVerified, 3);
});

test("the archive, the commit, the receipt and the host must be the ones in question", () => {
  const has = (r, re) => assert.match(r.problems.join(" | "), re);
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

test("the run's broker jobs: none listed, a step without one, and every way one job can fail to verify", () => {
  const has = (r, re) => { assert.equal(r.ok, false); assert.match(r.problems.join(" | "), re); };
  has(admit(fixture([])), /lists no broker job at all: nothing in it is signed/);
  has(admit(fixture(goodJobs(), { jobs: undefined })), /jobs is missing/);
  has(admit(fixture(goodJobs(), { runId: undefined })), /carries no run id/);
  // A step that cannot be a cache hit shows no verified job: only S11 jobs, or only refused ones in S12.
  has(admit(fixture([job("S11:check", 3)])), /step S12 cannot be served from a cache and shows no verified broker job/);
  has(admit(fixture(goodJobs()), { sealed: {} }), /names no steps that must show signed jobs/);
  // The receipt cannot choose the steps: its own signedSteps field is ignored.
  has(admit(fixture([job("S11:check", 3)], { signedSteps: ["S11", "S12"] })), /step S12 cannot be served/);
  // One job at a time.
  const one = (bad) => fixture([job("S11:check", 3), bad, job("S12:compile", 5)]);
  { const f = one(job("S11:compile", 4)); f.bundle.delete("job-4"); has(admit(f), /job job-4 \(S11:compile\): its signed receipt is not in the bundle \(ENOENT\)/); }
  { const f = one(job("S11:compile", 4)); f.bundle.set("job-4", Buffer.from('{"id":"job-4","edited":true}')); has(admit(f), /not the one listed \(sha256 differs\)/); }
  has(admit(one(job("S11:compile", 4, { signer: makeKey(HOST, BROKER) }))), /job job-4 .*not signed by the pinned key/);
  has(admit(one(job("S11:compile", 4, { signer: { ...key, pin: { ...key.pin, host: "OTHER-HOST" } } }))), /job job-4 .*signed for another host/);
  has(admit(one(job("S11:compile", 4, { signer: { ...key, pin: { ...key.pin, brokerDigest: "7".repeat(64) } } }))), /job job-4 .*signed by another broker runtime/);
  has(admit(one(job("S11:compile", 4, { payloadOver: { jobId: "job-999" } }))), /job job-4 .*for another job/);
  // A real signed job of another run, of another gate, or of another step, passed off as this one's.
  has(admit(one(job("S11:compile", 4, { nonce: contractJobNonce({ runId: "9".repeat(64), sealedSha256: SEALED, step: "S11:compile", call: "4".repeat(32) }) }))), /nonce the broker signed is not this run's/);
  has(admit(one(job("S11:compile", 4, { nonce: contractJobNonce({ runId: RUN, sealedSha256: "9".repeat(64), step: "S11:compile", call: "4".repeat(32) }) }))), /nonce the broker signed is not this run's/);
  has(admit(one(job("S11:compile", 4, { over: { step: "S12:compile" } }))), /nonce the broker signed is not this run's/);
  has(admit(one(job("S11:compile", 4, { over: { requestId: "../x" } }))), /cannot name a file/);
  // A tampered payload under a real signature.
  { const f = one(job("S11:compile", 4)); const j = JSON.parse(String(f.bundle.get("job-4"))); j.provenance.payload.exitCode = 0; const b = Buffer.from(JSON.stringify(j)); f.bundle.set("job-4", b); f.receipt.jobs[1].receiptSha256 = sha(b); has(admit(f), /job job-4 .*signature does not verify/); }
});

test("the real command: admitted only with every input present, and refused by name otherwise", () => {
  const script = fileURLToPath(new URL("./gate-release-admission.mjs", import.meta.url));
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-adm-"));
  try {
    const run = (...args) => { const r = spawnSync(process.execPath, [script, ...args], { encoding: "utf8", timeout: 60000, windowsHide: true }); return { code: r.status, text: `${r.stdout}${r.stderr}` }; };
    const none = run(); assert.equal(none.code, 1); assert.match(none.text, /--receipt <contract-test receipt> is required/); assert.match(none.text, /--archive .* is required/); assert.match(none.text, /--commit <40-hex release commit> is required/);
    assert.equal(run("--nope").code, 64);
    // A receipt for another archive, read from disk with its bundle folder, against this checkout: refused, and it names the archive.
    const f = fixture(goodJobs());
    const receiptFile = path.join(dir, "contract-test-receipt.dell.json"); fs.writeFileSync(receiptFile, JSON.stringify(f.receipt));
    fs.mkdirSync(path.join(dir, "job-receipts")); for (const [id, bytes] of f.bundle) fs.writeFileSync(path.join(dir, "job-receipts", `${id}.json`), bytes);
    const archiveFile = path.join(dir, "pkg.tgz"); fs.writeFileSync(archiveFile, "other bytes");
    const pipelineFile = path.join(dir, "rk-pipeline.json"); fs.writeFileSync(pipelineFile, JSON.stringify(pipeline));
    const refused = run("--receipt", receiptFile, "--archive", archiveFile, "--commit", COMMIT, "--pipeline", pipelineFile);
    assert.equal(refused.code, 1); assert.match(refused.text, /gate-release-admission REFUSED/); assert.match(refused.text, /the archive in question is/);
    assert.doesNotMatch(refused.text, /its signed receipt is not in the bundle/, "the bundle beside the receipt was read");
    // A receipt that names another folder for its job receipts is refused before anything is read from it.
    fs.writeFileSync(receiptFile, JSON.stringify({ ...f.receipt, jobReceipts: "../elsewhere" }));
    assert.match(run("--receipt", receiptFile, "--archive", archiveFile, "--commit", COMMIT, "--pipeline", pipelineFile).text, /names its job receipts folder as "\.\.\/elsewhere"/);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
