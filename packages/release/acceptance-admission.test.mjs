// Admission of a gate release judged by the twelve-line result (rightkit#1514 step 9; design table D:/scratch/loop-review/twelve-line-step9/design-table.md).
// Real ES256 signatures, real hashes, no broker and no network. One admitted case, every row of the decision table refused, and the effect proven: a refused result
// throws before the publish entry starts npm, an admitted one lets exactly one publish follow.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { admitAcceptanceResult, ACCEPTANCE_RESULT_SCHEMA, JUDGE_FILES, judgeFileHashes } from "./acceptance-admission.mjs";
import { admitFromFiles, gateReleaseGuard, GATE_PACKAGE } from "./gate-release-admission.mjs";
import { readAcceptanceLines, CONTRACT_FILE } from "./acceptance-lines.mjs";
import { archiveIdentity } from "./gate-acceptance.mjs";
import { claimSignedBytes, PUBLICATION_CLAIM_SCHEMA } from "./receipt-signature.mjs";

const sha = (b) => createHash("sha256").update(b).digest("hex");
function makeKey(host, brokerDigest) {
  const { publicKey, privateKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const publicKeyHex = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  return { privateKey, pin: { publicKey: publicKeyHex, keyId: sha(Buffer.from(publicKeyHex, "hex")), host, brokerDigest } };
}
const BROKER = "d".repeat(64); const HOST = "BOGUS-DELL";
const key = makeKey(HOST, BROKER); const other = makeKey(HOST, BROKER);
const COMMIT = "c".repeat(40); const TREE = "9".repeat(40); const RUN = "1".repeat(64);
const archiveBytes = Buffer.from("the packed gate archive"); const archive = archiveIdentity(archiveBytes);
const sourceOf = (over = {}) => Buffer.from(JSON.stringify({ schema: "rustright.trusted-gate-source/1", package: "@rightkit/release", version: "0.6.62", integrity: archive.integrity, sha256: archive.sha256, origin: "ci-artifact", ...over }));
const manifestOf = (source) => Buffer.from(`./node_modules/@rightkit/release/package.json\tfile\t-\t${"3".repeat(64)}\n./trusted-gate-source.json\tfile\t-\t${sha(source)}\n`);
const sourceBytes = sourceOf(); const manifestBytes = manifestOf(sourceBytes); const SEALED = sha(manifestBytes);
const pipeline = { attestation: { win32: key.pin } };
const contractText = fs.readFileSync(CONTRACT_FILE, "utf8"); const LINES = readAcceptanceLines(contractText);
const judgeFiles = judgeFileHashes();

/** One broker job as handed over: { id, publication }, signed by `signer`, with the commit and tree the broker signed. */
function claim(n, { signer = key, commit = COMMIT, tree = TREE, command = "clippy", extra = {} } = {}) {
  const jobId = `job-${n}`;
  const payload = { schema: PUBLICATION_CLAIM_SCHEMA, keyId: signer.pin.keyId, host: signer.pin.host, broker: { brokerDigest: signer.pin.brokerDigest, version: "rustright-x" }, jobId, nonce: String(n).repeat(64).slice(0, 64), gate: { commit, tree }, result: { status: "SUCCEEDED", exitCode: 0 }, command, payloadSha256: "5".repeat(64), ...extra };
  const doc = { id: jobId, publication: { schema: PUBLICATION_CLAIM_SCHEMA, payload, signature: { alg: "ES256", keyId: signer.pin.keyId, value: sign("sha256", claimSignedBytes(payload), { key: signer.privateKey, dsaEncoding: "ieee-p1363" }).toString("hex") } } };
  const bytes = Buffer.from(JSON.stringify(doc));
  return { jobId, bytes, entry: { requestId: jobId, claimSha256: sha(bytes) } };
}
function fixture(over = {}, claims = [claim(1), claim(2)]) {
  const bundle = new Map(claims.map((c) => [c.jobId, c.bytes]));
  const result = { schema: ACCEPTANCE_RESULT_SCHEMA, runId: RUN, commit: COMMIT, tree: TREE, host: "dell", kind: "compile", startedAt: "2026-11-21T05:00:00.000Z", endedAt: "2026-11-21T05:10:00.000Z",
    gate: { path: "D:\\gates\\sealed\\bin\\rightkit-gate.mjs", sealedSha256: SEALED }, contractSha256: sha(contractText), judgeFiles: { ...judgeFiles },
    lines: LINES.map((l) => ({ id: l.id, text: l.text, verdict: "pass" })), pass: true, jobs: claims.map((c) => c.entry), jobClaims: "job-claims", gateSeal: "gate-seal", ...over };
  const readJob = (id) => { if (!bundle.has(id)) throw Object.assign(new Error("absent"), { code: "ENOENT" }); return bundle.get(id); };
  return { result, readJob, bundle };
}
const admit = (f, over = {}) => admitAcceptanceResult({ result: f.result, readJob: f.readJob, archiveBytes, commit: COMMIT, tree: TREE, pipeline, contractText, contractSha256: sha(contractText), judgeFiles, manifestBytes, sourceBytes, ...over });
const refused = (r, re) => { assert.equal(r.ok, false, "must be refused"); assert.match(r.problems.join(" | "), re); };

test("9.1 a gate release is admitted when the twelve-line result, the archive, the committed host and every signed claim agree", () => {
  const r = admit(fixture());
  assert.deepEqual(r.problems, []); assert.equal(r.ok, true);
  assert.deepEqual(r.facts, { host: "dell", version: "0.6.62", sealedSha256: SEALED, archiveSha256: archive.sha256, jobsListed: 2, jobsVerified: 2, signedSteps: [], nnBasis: "twelve-line-judge" });
  assert.deepEqual(JUDGE_FILES, ["acceptance-lines.mjs", "acceptance-lines.test.mjs", "acceptance-launch.mjs"]);
});

test("9.2 every row of the decision table refuses (table 1 to 9)", () => {
  // 1: the schema
  refused(admit(fixture({ schema: "rightkit.something-else/1" })), /schema/);
  refused(admit({ result: null, readJob: () => { throw new Error("x"); }, archiveBytes, commit: COMMIT, tree: TREE, pipeline, contractText, contractSha256: sha(contractText), judgeFiles, manifestBytes, sourceBytes }), /schema/);
  // 2: the twelve lines, as the checkout's table has them
  refused(admit(fixture({ lines: LINES.slice(0, 11).map((l) => ({ id: l.id, text: l.text, verdict: "pass" })) })), /twelve lines/);
  refused(admit(fixture({ lines: [...LINES].reverse().map((l) => ({ id: l.id, text: l.text, verdict: "pass" })) })), /twelve lines/);
  refused(admit(fixture({ lines: LINES.map((l, i) => ({ id: l.id, text: i === 4 ? `${l.text} Reworded.` : l.text, verdict: "pass" })) })), /NN05.*text/);
  // 3: every verdict pass
  for (const verdict of ["fail", "not measured", "PASS", undefined]) refused(admit(fixture({ lines: LINES.map((l, i) => ({ id: l.id, text: l.text, verdict: i === 2 ? verdict : "pass" })) })), /NN03.*(fail|not measured|PASS|undefined)|NN03/);
  refused(admit(fixture({ pass: false })), /pass/);
  refused(admit(fixture({ pass: true, lines: LINES.map((l, i) => ({ id: l.id, text: l.text, verdict: i === 0 ? "not measured" : "pass" })) })), /NN01/);
  // 4: judged by this checkout's judge
  refused(admit(fixture({ judgeFiles: { ...judgeFiles, "acceptance-lines.mjs": "0".repeat(64) } })), /judge file acceptance-lines\.mjs/);
  refused(admit(fixture({ judgeFiles: { "acceptance-lines.mjs": judgeFiles["acceptance-lines.mjs"] } })), /judge file acceptance-launch\.mjs/);
  // 5: the contract text
  refused(admit(fixture({ contractSha256: "0".repeat(64) })), /contract/);
  // 6: the release commit and its tree
  refused(admit(fixture({ commit: "d".repeat(40) })), /commit/);
  refused(admit(fixture({ tree: "8".repeat(40) })), /tree/);
  refused(admit(fixture(), { tree: null }), /no tree/);
  // 7: a host this repository trusts
  refused(admit(fixture({ host: "somewhere" })), /not a host this repository names/);
  refused(admit(fixture(), { pipeline: { attestation: {} } }), /no complete attestation/);
  // 8: the seal accounts for the archive
  refused(admit(fixture({ gate: { path: "x", sealedSha256: null } })), /seal/);
  refused(admit(fixture({ gate: { path: "x", sealedSha256: "0".repeat(64) } })), /seal to archive/);
  refused(admit(fixture(), { archiveBytes: Buffer.from("another archive") }), /the archive in question is/);
  refused(admit(fixture(), { manifestBytes: null, sourceBytes: null }), /manifest and source record/);
  // 9: the broker's claims
  refused(admit(fixture({ jobs: [] }, [])), /no broker job/);
  refused(admit(fixture({ jobs: undefined })), /jobs/);
  { const f = fixture(); f.bundle.delete("job-2"); refused(admit(f), /job-2.*not in the bundle/); }
  { const f = fixture(); f.result.jobs[0].claimSha256 = "0".repeat(64); refused(admit(f), /job-1.*not the one listed/); }
  refused(admit(fixture({}, [claim(1), claim(2, { signer: other })])), /job-2/);
  refused(admit(fixture({}, [claim(1), claim(2, { commit: "e".repeat(40) })])), /job-2.*commit/);
  refused(admit(fixture({}, [claim(1), claim(2, { tree: "7".repeat(40) })])), /job-2.*tree/);
  { const c = claim(3); const doc = JSON.parse(c.bytes); doc.extra = "more than the claim"; const bytes = Buffer.from(JSON.stringify(doc)); const f = fixture({}, [claim(1)]); f.bundle.set("job-3", bytes); f.result.jobs.push({ requestId: "job-3", claimSha256: sha(bytes) }); refused(admit(f), /job-3.*more than the claim/); }
  refused(admit(fixture({ jobs: [{ requestId: "../escape", claimSha256: "0".repeat(64) }] })), /cannot name a file/);
});

test("9.4 the result record is built from the launcher's record, the judge's verdict and the broker's signed claims, and the admission accepts it", async () => {
  const { buildAcceptanceResult, claimOf, sealedOfGatePath } = await import("./acceptance-result.mjs");
  const c1 = claim(1); const c2 = claim(2);
  // a broker receipt as the broker keeps it: provenance.payload.jobId and a top-level publication claim
  const receiptOf = (c) => ({ provenance: { payload: { jobId: c.jobId } }, publication: JSON.parse(c.bytes).publication });
  const taken = claimOf(receiptOf(c1)); assert.equal(taken.id, "job-1"); assert.deepEqual(JSON.parse(taken.bytes), JSON.parse(c1.bytes)); assert.equal(taken.sha256, sha(taken.bytes));
  assert.equal(claimOf({ provenance: { payload: { jobId: "../x" } }, publication: {} }), null); assert.equal(claimOf({ provenance: { payload: { jobId: "job-9" } } }), null);
  assert.equal(sealedOfGatePath(`D:\\.rightkit-managed\\trusted-gates\\${SEALED}\\node_modules\\@rightkit\\release\\bin\\rightkit-gate.mjs`), SEALED);
  assert.equal(sealedOfGatePath(`D:\\.rightkit-managed\\trusted-gates\\candidates\\${SEALED}\\node_modules\\@rightkit\\release\\bin\\rightkit-gate.mjs`), SEALED);
  assert.equal(sealedOfGatePath("D:\\Claude\\rightkit\\packages\\release\\bin\\rightkit-gate.mjs"), null);
  const launch = { runId: RUN, commit: COMMIT, kind: "compile", startedAt: "2026-11-21T05:00:00.000Z", endedAt: "2026-11-21T05:10:00.000Z", fingerprint: { platform: "win32" }, gate: { path: `D:\\.rightkit-managed\\trusted-gates\\${SEALED}\\node_modules\\@rightkit\\release\\bin\\rightkit-gate.mjs` } };
  const verdict = { lines: LINES.map((l) => ({ id: l.id, text: l.text, verdict: "pass", why: "x" })), pass: true };
  const record = buildAcceptanceResult({ launch, verdict, contractText, judgeFiles, tree: TREE, claims: [taken, claimOf(receiptOf(c2))] });
  assert.equal(record.schema, ACCEPTANCE_RESULT_SCHEMA); assert.equal(record.host, "dell"); assert.equal(record.gate.sealedSha256, SEALED); assert.equal(record.jobs.length, 2);
  const f = fixture({}, [c1, c2]); const r = admit({ ...f, result: record });
  assert.deepEqual(r.problems, []); assert.equal(r.ok, true);
  // a run that did not pass builds a record the admission refuses
  const notPassed = buildAcceptanceResult({ launch, verdict: { lines: verdict.lines.map((l, i) => ({ ...l, verdict: i === 3 ? "not measured" : "pass" })), pass: false }, contractText, judgeFiles, tree: TREE, claims: [taken, claimOf(receiptOf(c2))] });
  refused(admit(f, { result: notPassed }), /NN04/);
});

test("9.3 the effect: a refused result throws before the publish entry starts npm; an admitted one lets exactly one publish follow", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-acc-adm-"));
  try {
    const f = fixture();
    const write = (result) => { fs.writeFileSync(path.join(dir, "acceptance-result.json"), JSON.stringify(result)); fs.rmSync(path.join(dir, "job-claims"), { recursive: true, force: true }); fs.mkdirSync(path.join(dir, "job-claims")); for (const [id, bytes] of f.bundle) fs.writeFileSync(path.join(dir, "job-claims", `${id}.json`), bytes); fs.mkdirSync(path.join(dir, "gate-seal"), { recursive: true }); fs.writeFileSync(path.join(dir, "gate-seal", "manifest.txt"), manifestBytes); fs.writeFileSync(path.join(dir, "gate-seal", "trusted-gate-source.json"), sourceBytes); };
    const archiveFile = path.join(dir, "pkg.tgz"); fs.writeFileSync(archiveFile, archiveBytes);
    const pipelineFile = path.join(dir, "rk-pipeline.json"); fs.writeFileSync(pipelineFile, JSON.stringify(pipeline));
    const env = { RK_GATE_CONTRACT_RECEIPT: path.join(dir, "acceptance-result.json") };
    let publishes = 0; const publish = () => { publishes += 1; };
    const enter = () => { gateReleaseGuard({ name: GATE_PACKAGE, archiveFile, commit: COMMIT, env, pipelineFile, admit: (a) => admitFromFiles({ ...a, treeOf: () => TREE }) }); publish(); };
    // refused: one line not measured
    write({ ...f.result, lines: LINES.map((l, i) => ({ id: l.id, text: l.text, verdict: i === 3 ? "not measured" : "pass" })), pass: false });
    assert.throws(enter, /is not published: its contract-test receipt does not admit this archive/); assert.equal(publishes, 0, "no publish after a refusal");
    // refused: the archive in question is another one
    write(f.result); fs.writeFileSync(archiveFile, "another archive");
    assert.throws(enter, /the archive in question is/); assert.equal(publishes, 0);
    // admitted: the real archive, every line pass
    fs.writeFileSync(archiveFile, archiveBytes); enter(); assert.equal(publishes, 1, "exactly one publish after an admission");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
