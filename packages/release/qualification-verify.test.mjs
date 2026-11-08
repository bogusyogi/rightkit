// CI's publication admission (qualification-verify.mjs), driven with real ES256 signatures: a key pair generated here signs canonical payloads exactly as the
// broker does (rustright-protocol attestation.rs: ECDSA P-256 over "rustright-provenance-v1\n" + canonical JSON, r||s), and the verifier is called as publish-admission.mjs
// and unit-gate.mjs call it. `git` is a function over a fixed fake commit (as ci-compile-admission.test.mjs does): rev-parse, ci/rk-pipeline.json and ci/gate.config.json.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import fs from "node:fs";
import { ProvenanceError, canonicalJson, signedBytes, verifyProvenanceSignature } from "./receipt-signature.mjs";
import { verifyCiCompileReceipts, verifyQualificationReceipts } from "./qualification-verify.mjs";
import { expectedStageCommands } from "./stage-commands.mjs";
import { readSeamStageReceipts } from "./unit-gate.mjs";

const sha = "a".repeat(40), tree = "b".repeat(40), otherSha = "c".repeat(40), otherTree = "d".repeat(40);
const HOST = "mac.test", BROKER_DIGEST = "9".repeat(64);
const DATA = "/data/rightkit", SNAP = `${DATA}/snapshots/repokey01`, VERDICTS = `${DATA}/class-a-verdicts`;
const unit = { kind: "cargo", name: "rightkit-decimal" };
const config = JSON.parse(fs.readFileSync(new URL("../../ci/gate.config.json", import.meta.url), "utf8"));
const expected = expectedStageCommands({ kind: unit.kind, unit: unit.name, config, platform: "darwin" });

function makeKey({ host = HOST } = {}) {
  const { publicKey, privateKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const publicKeyHex = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  const keyId = createHash("sha256").update(Buffer.from(publicKeyHex, "hex")).digest("hex");
  return { privateKey, keyId, pin: { publicKey: publicKeyHex, keyId, host, brokerDigest: BROKER_DIGEST }, host };
}
const key = makeKey();

/** The broker's envelope: `payload` signed with `signer`'s key (the signature covers exactly the payload given). */
const envelope = (signer, payload) => ({
  schema: "rustright.provenance.v1", payload,
  signature: { alg: "ES256", keyId: signer.keyId, value: sign("sha256", signedBytes(payload), { key: signer.privateKey, dsaEncoding: "ieee-p1363" }).toString("hex") },
});

let counter = 0;
const nextId = () => `job-${String(++counter).padStart(4, "0")}`;
const workspaceOf = (row) => (row.cwd === "." ? SNAP : `${SNAP}/${row.cwd}`);
const verdictOf = (row, commit = sha) => `${VERDICTS}/class-a-${commit.slice(0, 12)}-full-${row.cwd === "." ? "root" : createHash("sha256").update(row.cwd).digest("hex").slice(0, 12)}.json`;

/** The signed payload of one seam receipt (rustright finish_impl.rs provenance_payload, gate-bound job: the flat `gate`, `testExecutables`, `outputDir`; optional members null). */
function seamPayload(row, { signer = key, jobId = nextId(), argv = row.argvs[0], gate = {}, result = {}, over = {} } = {}) {
  const workspace = workspaceOf(row);
  return {
    host: signer.host, broker: { version: "test", brokerDigest: BROKER_DIGEST, clientDigest: "8".repeat(64) }, keyId: signer.keyId, jobId,
    // The client mints the nonce internally; the verifier must neither know nor check it.
    nonce: `client-minted-${jobId}`, requestDigest: "7".repeat(64),
    request: { kind: "cargo", shim: "cargo", adapter: null, requestedArgv: argv, effectiveArgv: ["cargo", ...argv], workspace, workspaceRoot: workspace, envDigest: "6".repeat(64), manifestDigest: null, outputs: null, timeoutMs: null, transformations: [] },
    result: { status: "SUCCEEDED", exitCode: 0, signal: null, reap: null, postChecks: null, ...result },
    terminalAt: "2026-11-09T00:00:00.000Z",
    gate: { commit: sha, tree, snapshot: SNAP, classA: verdictOf(row), snapshotUnchanged: true, ...gate },
    testExecutables: null, testExecutablesReason: null, outputDir: `${DATA}/targets/repokey01`,
    ...over,
  };
}
const seamReceipt = (row, options = {}) => {
  const signer = options.signer ?? key;
  const payload = seamPayload(row, options);
  return { id: payload.jobId, provenance: envelope(signer, payload) };
};

/** One receipt per broker command of each stage (a `direct` row makes none, GC-A11). `mutate(stage, row, index)` may return a replacement receipt or options. */
function qualificationOf(build, stages = ["compile", "package"]) {
  const q = { schema: "rightkit.unit-qualification/1", unit, commit: sha, tree, stages: {} };
  for (const stage of stages) q.stages[stage] = { ok: true, receipts: expected[stage].filter((row) => row.direct !== true).map((row, index) => build(stage, row, index)) };
  return q;
}
const good = () => qualificationOf((stage, row) => seamReceipt(row));

const gitFor = ({ pins = { darwin: key.pin } } = {}) => (...args) => {
  if (args[0] === "rev-parse" && args[1] === `${sha}^{tree}`) return tree;
  if (args[0] === "show" && args[1] === `${sha}:ci/rk-pipeline.json`) return JSON.stringify({ attestation: pins });
  if (args[0] === "show" && args[1] === `${sha}:ci/gate.config.json`) return JSON.stringify(config);
  throw new Error(`unexpected git ${args.join(" ")}`);
};
const git = gitFor();
const verify = (q, options = {}) => verifyQualificationReceipts(JSON.stringify(q), { sha, git: options.git ?? git });
const refused = (q, pattern, options) => assert.throws(() => verify(q, options), (error) => { assert.match(error.message, /^publication admission refused: /); assert.match(error.message, pattern); return true; });
const count = (stage) => expected[stage].filter((row) => row.direct !== true).length;

test("a seam qualification verifies from the signed gate object and no caller nonce", () => {
  const verified = verify(good());
  assert.equal(verified.compile.length, count("compile"));
  assert.equal(verified.package.length, count("package"));
  assert.ok(verified.compile.every((r) => r.host === HOST && r.keyId === key.keyId && /^job-/.test(r.jobId)));
});

test("a seam receipt with a nonce no caller sent is accepted, and a tampered nonce is only a signature failure", () => {
  const q = qualificationOf((stage, row) => seamReceipt(row, { over: { nonce: "anything-at-all" } }));
  verify(q);
  const tampered = good();
  tampered.stages.compile.receipts[0].provenance.payload.nonce = "changed-after-signing";
  refused(tampered, /signature does not verify/);
});

test("a wrong signature is refused (payload changed after signing; another key; a non-hex value)", () => {
  const q = good();
  q.stages.package.receipts[0].provenance.payload.requestDigest = "5".repeat(64);
  refused(q, /does not verify against the enrolled pins.*signature does not verify/);
  const stranger = makeKey();
  refused(qualificationOf((stage, row) => seamReceipt(row, { signer: { ...stranger, keyId: key.keyId } })), /signature does not verify/);
  const bad = good();
  bad.stages.compile.receipts[0].provenance.signature.value = "zz".repeat(64);
  refused(bad, /signature is not hex/);
});

test("a host the commit's ci/rk-pipeline.json does not enroll is refused", () => {
  const other = makeKey({ host: "elsewhere.test" });
  refused(qualificationOf((stage, row) => seamReceipt(row, { signer: other })), /signed for host "elsewhere.test", which ci\/rk-pipeline.json does not enroll/);
});

test("a receipt signed for the enrolled host by another key than the pinned one is refused", () => {
  const impostor = makeKey({ host: HOST });
  refused(qualificationOf((stage, row) => seamReceipt(row, { signer: impostor })), /does not verify against the enrolled pins/);
});

test("a receipt of another commit or another tree is refused", () => {
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { gate: { commit: otherSha } } : {})), /was built from commit cccccccccccc, not the dispatched aaaaaaaaaaaa/);
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "package" ? { gate: { tree: otherTree } } : {})), /was built from tree dddddddddddd/);
});

test("a receipt whose snapshot changed during the job (snapshotUnchanged not true) is refused", () => {
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { gate: { snapshotUnchanged: false } } : {})), /did not leave the snapshot unchanged/);
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { gate: { snapshotUnchanged: "true" } } : {})), /did not leave the snapshot unchanged/);
});

test("a receipt with no gate evidence bound to this commit's Class A verdict is refused", () => {
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { gate: { classA: verdictOf(row, otherSha) } } : {})), /names the Class A verdict class-a-cccccccccccc/);
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { gate: { classA: "relative/verdict.json" } } : {})), /names no absolute snapshot or Class A verdict path/);
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { gate: { snapshot: null } } : {})), /names no absolute snapshot or Class A verdict path/);
});

test("receipts of two different snapshots are refused", () => {
  const other = "/data/rightkit/snapshots/repokey02";
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 1 && stage === "compile" ? { gate: { snapshot: other } } : {})), /another snapshot than the qualification's other jobs/);
  // The request's own directory must lie in the snapshot the gate object names, not merely agree across receipts.
  refused(qualificationOf((stage, row) => seamReceipt(row, { gate: { snapshot: other } })), /not the snapshot \/data\/rightkit\/snapshots\/repokey02 its gate evidence names/);
});

test("Class A verdicts in two different directories are refused", () => {
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "package" ? { gate: { classA: verdictOf(row).replace(VERDICTS, `${DATA}/elsewhere`) } } : {})), /another directory than the qualification's other jobs/);
});

test("a receipt that is none of the stage's commands is refused", () => {
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { argv: ["build", "--locked", "-p", "rightkit-decimal"] } : {})), /ran \["build","--locked","-p","rightkit-decimal"\], which is not \(or no longer\) a command of the compile stage/);
  // The right argv in another unit's directory is not this unit's command either.
  refused(qualificationOf((stage, row, i) => (i === 0 && stage === "compile" ? seamReceipt({ ...row, cwd: "crates/other-crate" }, { argv: row.argvs[0] }) : seamReceipt(row))), /which is not \(or no longer\) a command of the compile stage|not in one checkout|another workspace/);
});

test("a stage command no receipt covers is refused, naming it", () => {
  const q = good();
  q.stages.compile.receipts.pop();
  refused(q, /carries no signed receipt of the compile stage's cargo .* \(1 of \d+ required command\(s\) missing\)/);
  const none = good();
  none.stages.package.receipts = [];
  refused(none, /package stage carries no signed broker receipts/);
});

test("a job id used twice, within a stage or across stages, is refused", () => {
  const q = good();
  q.stages.compile.receipts.push(q.stages.compile.receipts[0]);
  refused(q, /appears more than once in the qualification/);
  const across = good();
  across.stages.package.receipts[0] = across.stages.compile.receipts[0];
  refused(across, /appears more than once in the qualification/);
});

test("a receipt whose signed jobId is not its own id is refused", () => {
  const q = good();
  q.stages.compile.receipts[0].id = "job-renamed";
  refused(q, /the receipt is for another job/);
});

test("a result that is not SUCCEEDED with exit 0 is refused", () => {
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { result: { status: "FAILED", exitCode: 101 } } : {})), /did not succeed \(FAILED, exit 101\)/);
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { result: { exitCode: 1 } } : {})), /did not succeed \(SUCCEEDED, exit 1\)/);
});

test("receipts of two hosts in one qualification are refused", () => {
  const second = makeKey({ host: "dell.test" });
  const pins = { darwin: key.pin, win32: second.pin };
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 1 && stage === "compile" ? { signer: second } : {})), /signed by the win32 host, another host than the qualification's other jobs \(darwin\)/, { git: gitFor({ pins }) });
});

test("a qualification for another tree, or with no unit, is refused", () => {
  refused({ ...good(), tree: otherTree }, /the qualification receipt is for tree dddddddddddd/);
  refused({ ...good(), unit: {} }, /names no unit/);
});

test("a seam receipt whose request ran outside the snapshot, or carries no workspace, is refused", () => {
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { over: { request: { ...seamPayload(row).request, workspace: `/elsewhere/${row.cwd}`, workspaceRoot: `/elsewhere/${row.cwd}` } } } : {})), /another workspace than the qualification's other jobs|not the snapshot/);
  refused(qualificationOf((stage, row, i) => seamReceipt(row, i === 0 && stage === "compile" ? { over: { request: { ...seamPayload(row).request, workspace: "" } } } : {})), /records no workspace/);
});

// ---- the deleted carrier-era shape (plan step 5): a receipt with no signed gate object admits nothing ----
test("a receipt with no signed gate object (the deleted carrier-era shape) is refused", () => {
  const noGate = (row) => { const { gate: _gate, ...rest } = seamPayload(row); return { id: rest.jobId, provenance: envelope(key, rest) }; };
  refused(qualificationOf((stage, row, i) => (i === 0 && stage === "compile" ? noGate(row) : seamReceipt(row))), /carries no signed gate object/);
});

// ---- the CI compile proof's receipts (one stage, an independently bound nonce that a seam receipt no longer carries) ----
test("verifyCiCompileReceipts accepts a seam compile stage and refuses a receipt of another commit", () => {
  const stageNonce = "e".repeat(64);
  const compileOnly = qualificationOf((stage, row) => seamReceipt(row), ["compile"]);
  const verified = verifyCiCompileReceipts(compileOnly, { sha, git, stageNonce });
  assert.equal(verified.compile.length, count("compile"));
  const wrong = qualificationOf((stage, row, i) => seamReceipt(row, i === 0 ? { gate: { commit: otherSha } } : {}), ["compile"]);
  assert.throws(() => verifyCiCompileReceipts(wrong, { sha, git, stageNonce }), /was built from commit/);
  assert.throws(() => verifyCiCompileReceipts(compileOnly, { sha, git, stageNonce: "nope" }), /CI compile nonce is invalid/);
});

// ---- the signature half (receipt-signature.mjs) ----
test("the signature half checks the job, the host, the broker and the algorithm", () => {
  assert.equal(ProvenanceError.name, "ProvenanceError");
  const row = expected.compile.find((r) => r.direct !== true);
  const receipt = seamReceipt(row);
  const pins = key.pin;
  assert.equal(verifyProvenanceSignature(receipt, pins, receipt.id).jobId, receipt.id);
  assert.throws(() => verifyProvenanceSignature(receipt, pins, "job-other"), (e) => e.code === "job");
  assert.throws(() => verifyProvenanceSignature(receipt, { ...pins, host: "other.test" }, receipt.id), (e) => e.code === "host");
  assert.throws(() => verifyProvenanceSignature(receipt, { ...pins, brokerDigest: "1".repeat(64) }, receipt.id), (e) => e.code === "runtime");
  assert.throws(() => verifyProvenanceSignature({ ...receipt, provenance: { ...receipt.provenance, signature: { ...receipt.provenance.signature, alg: "RS256" } } }, pins, receipt.id), (e) => e.code === "alg");
});

test("canonical JSON sorts keys by bytes and refuses what Rust would print differently", () => {
  assert.equal(canonicalJson({ b: 1, a: [true, null, "x\"\\\u0001"], "\u{1F600}": 2, "￿": 3 }), '{"a":[true,null,"x\\"\\\\\\u0001"],"b":1,"￿":3,"\u{1F600}":2}');
  assert.throws(() => canonicalJson({ n: 1.5 }), /safe integers/);
});

// ---- the producer: a seam stage's receipts are the files its ledger rows name ----
test("readSeamStageReceipts reads the receipt each verified seam ledger row names, checks its digest and tells stages apart by the ids seen before", async () => {
  const os = await import("node:os");
  const path = await import("node:path");
  const { appendLedger, UNSCOPED } = await import("./cargo-ledger.mjs");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-ledger-"));
  try {
    const rows = expected.compile.filter((r) => r.direct !== true);
    const files = rows.map((row, index) => {
      const receipt = { ...seamReceipt(row), terminalAt: "2026-11-09T00:00:00.000Z" };
      const file = path.join(dir, `receipt-${index}.json`);
      const text = `${JSON.stringify(receipt)}\n`;
      fs.writeFileSync(file, text);
      return { receipt, file, sha: createHash("sha256").update(text).digest("hex") };
    });
    const ledger = path.join(dir, "ledger");
    files.forEach((f, i) => appendLedger(ledger, UNSCOPED, { id: `call-${i}`, mode: "seam", shim: "cargo", cwd: SNAP, argv: rows[i].argvs[0], status: "verified", settled: true, requestId: f.receipt.id, receipt: f.file, receiptSha256: f.sha, outputDir: `${DATA}/targets/repokey01` }));
    // A failed row, a pending row and a carrier row contribute nothing.
    appendLedger(ledger, UNSCOPED, { id: "failed", mode: "seam", cwd: SNAP, argv: ["build"], status: "failed", settled: true });
    appendLedger(ledger, UNSCOPED, { id: "pending", mode: "seam", cwd: SNAP, argv: ["build"], status: "pending", settled: false });
    appendLedger(ledger, UNSCOPED, { id: "carrier", cwd: SNAP, argv: ["build"], status: "verified", requestId: "carrier-job" });
    const got = readSeamStageReceipts(ledger);
    assert.deepEqual(got.map((r) => r.id), files.map((f) => f.receipt.id));
    assert.deepEqual(Object.keys(got[0]).sort(), ["id", "provenance"]);
    // The projection is exactly what CI verifies.
    const q = { stages: { compile: { receipts: got } } };
    verifyCiCompileReceipts({ unit, tree, stages: q.stages }, { sha, git, stageNonce: "e".repeat(64) });
    // Rows seen before the stage are not this stage's.
    assert.deepEqual(readSeamStageReceipts(ledger, new Set(["call-0"])).map((r) => r.id), files.slice(1).map((f) => f.receipt.id));
    // A receipt that is not the file the ledger recorded fails the stage.
    fs.writeFileSync(files[0].file, "{}\n");
    assert.throws(() => readSeamStageReceipts(ledger), /sha256 differs/);
    // A missing receipt file fails it too.
    fs.rmSync(files[0].file);
    assert.throws(() => readSeamStageReceipts(ledger), /cannot be read/);
    // No ledger directory, no seam receipts.
    assert.deepEqual(readSeamStageReceipts(path.join(dir, "nothing")), []);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("Windows: the canonical \\\\?\\ snapshot and the submitted workspace are one checkout whatever the prefix, separators and case; a Mac compares case-sensitively", () => {
  const winKey = makeKey({ host: "dell.test" });
  const winExpected = expectedStageCommands({ kind: unit.kind, unit: unit.name, config, platform: "win32" });
  const winSnap = "\\\\?\\D:\\Rightkit\\Snapshots\\repokey01";
  const winWorkspace = (row) => (row.cwd === "." ? "D:\\rightkit\\snapshots\\repokey01" : `D:\\rightkit\\snapshots\\repokey01\\${row.cwd.replaceAll("/", "\\")}`);
  const q = { schema: "rightkit.unit-qualification/1", unit, commit: sha, tree, stages: {} };
  for (const stage of ["compile", "package"]) q.stages[stage] = { ok: true, receipts: winExpected[stage].filter((row) => row.direct !== true).map((row) => {
    const base = seamPayload(row, { signer: winKey });
    const payload = { ...base, request: { ...base.request, workspace: winWorkspace(row), workspaceRoot: winWorkspace(row) }, gate: { ...base.gate, snapshot: winSnap, classA: `\\\\?\\D:\\Rightkit\\class-a-verdicts\\${verdictOf(row).split("/").at(-1)}` } };
    return { id: payload.jobId, provenance: envelope(winKey, payload) };
  }) };
  const verified = verifyQualificationReceipts(JSON.stringify(q), { sha, git: gitFor({ pins: { win32: winKey.pin } }) });
  assert.equal(verified.compile.length, winExpected.compile.filter((row) => row.direct !== true).length);
  // darwin: a snapshot spelled with another case than the workspace is another directory
  const mac = qualificationOf((stage, row) => seamReceipt(row, { gate: { snapshot: SNAP.toUpperCase() } }));
  refused(mac, /is not the snapshot/);
});
