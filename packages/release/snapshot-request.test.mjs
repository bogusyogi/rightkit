// The gate's half of a broker snapshot build (snapshot-request.mjs; joint plan step 4d). The carrier is checked against every rule
// RustRight's reader applies (rustright-cli shims.rs snapshot_carrier at 5c59b9b); the receipt verifier against a receipt signed with
// a real P-256 key the way rustright-protocol attestation.rs signs and checks it.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { canonicalJson, signedBytes, verifyAttributionDetail } from "./provenance-verify.mjs";
import {
  ATTEST_NONCE_ENV, DESCRIPTOR_MAX_BYTES, REQUEST_ID_FILE_ENV, RESULT_EVIDENCE_FIELDS, SNAPSHOT_REQUEST_ENV, SNAPSHOT_REQUEST_SCHEMA, SnapshotRequestError, argvSha256, declaredInputDigestsOf, freshNonce,
  BROKER_COMPILE_COMMANDS, CARGO_COMMAND_ALIASES, cargoSubcommandOf, isCallHandleOf, isCompileClass, isLockBound, isPackageObservation, lockedArgv, isGenerationTarget, isInsideGenerationTarget, GENERATION_LEAVES, nextCallHandle, ownershipOfHandle, observedEnvOf, readDeclaredInputs, readRequestRef, requestDigestOf, snapshotRequestCore, verifySnapshotEvidenceIdentity, verifySnapshotReceipt, writeSnapshotRequest,
} from "./snapshot-request.mjs";
import { makeFakeBroker, signedReceipt } from "./test-support/fake-broker.mjs";
import { loadVector } from "./test-support/vectors.mjs";

const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-snapreq-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const sha = (s) => crypto.createHash("sha256").update(s).digest("hex");
const C = "c".repeat(40); const T = "e".repeat(40);

function request(t, over = {}) {
  const root = tmp(t);
  const snapshot = path.join(root, "data", "snapshots", "0123456789abcdef"); fs.mkdirSync(snapshot, { recursive: true });
  const cwd = path.join(root, "checkout"); fs.mkdirSync(cwd);
  const argv = ["test", "-p", "core", "--locked"];
  const made = writeSnapshotRequest({ controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: snapshot, expectedTree: T }, declaredInputs: ["Cargo.lock", "crates/core"], handle: "own-1", primaryCwd: cwd, argv, nonce: "a".repeat(64), requestIdFile: path.join(root, "req.json"), ...over });
  return { root, snapshot, cwd, argv, made };
}

test("canonical JSON is RustRight's: its own test vector (attestation.rs canonical_json_sorts_keys_by_bytes_and_escapes_minimally)", () => {
  assert.equal(canonicalJson({ b: 1, a: [true, null, 'x"y\\z\n'], B: { d: 2, c: "é" } }), '{"B":{"c":"é","d":2},"a":[true,null,"x\\"y\\\\z\\u000a"],"b":1}');
});

test("carrier: the file and descriptor pass every rule RustRight's reader applies", (t) => {
  const { root, snapshot, cwd, argv, made } = request(t);
  const dir = path.join(root, "control", "snapshot-requests");
  // Under <control root>/snapshot-requests, a real directory and a regular file, owner-only on POSIX.
  assert.equal(path.dirname(made.file), dir);
  assert.ok(fs.lstatSync(dir).isDirectory() && !fs.lstatSync(dir).isSymbolicLink());
  if (process.platform !== "win32") { assert.equal(fs.statSync(made.file).mode & 0o777, 0o600); assert.equal(fs.statSync(dir).mode & 0o777, 0o700); }
  // Descriptor: {path, sha256} only, at most 4096 bytes, sha256 of the exact bytes.
  const d = JSON.parse(made.descriptor);
  assert.deepEqual(Object.keys(d).sort(), ["path", "sha256"]);
  assert.ok(Buffer.byteLength(made.descriptor) <= DESCRIPTOR_MAX_BYTES);
  assert.equal(d.sha256, sha(fs.readFileSync(made.file)));
  // Carrier: exactly the reader's fields (deny_unknown_fields, camelCase), the commit tree equal to the expected tree, nonce = the env's.
  const c = JSON.parse(fs.readFileSync(made.file, "utf8"));
  assert.deepEqual(Object.keys(c).sort(), ["argvSha256", "commit", "declaredInputs", "nonce", "ownership", "primaryCwd", "schema", "snapshot"]);
  assert.equal(c.schema, SNAPSHOT_REQUEST_SCHEMA);
  assert.deepEqual(c.commit, { sha: C, tree: T }); assert.deepEqual(c.snapshot, { path: snapshot, expectedTree: T });
  assert.deepEqual(c.declaredInputs, { inputs: ["Cargo.lock", "crates/core"] }); assert.match(c.ownership.handle, /^own-1#[A-Za-z0-9._-]+$/, "the carrier names a per-call sub-handle of the ownership"); assert.equal(isCallHandleOf(c.ownership.handle, "own-1"), true);
  assert.equal(c.primaryCwd, cwd);
  assert.equal(c.argvSha256, sha(canonicalJson(argv)), "sha256 of the canonical cargo argv, without cargo");
  assert.equal(c.nonce, made.env[ATTEST_NONCE_ENV]);
  assert.deepEqual(Object.keys(made.env).sort(), [ATTEST_NONCE_ENV, REQUEST_ID_FILE_ENV, SNAPSHOT_REQUEST_ENV].sort());
  assert.equal(made.env[SNAPSHOT_REQUEST_ENV], made.descriptor);
});

test("carrier: anything the reader would refuse is refused before a file is written", (t) => {
  const bad = [
    [{ commit: { sha: "abc", tree: T } }, /full lowercase object ids/],
    [{ snapshot: { path: "/x", expectedTree: "f".repeat(40) } }, /expectedTree/],
    [{ snapshot: { path: "relative", expectedTree: T } }, /absolute path/],
    [{ handle: " " }, /handle/],
    [{ primaryCwd: "rel" }, /primaryCwd/],
    [{ nonce: "short" }, /nonce/],
    [{ nonce: "A".repeat(64) }, /lowercase/],
    [{ declaredInputs: ["/abs"] }, /relative/],
  ];
  for (const [over, why] of bad) {
    const root = tmp(t);
    assert.throws(() => writeSnapshotRequest({ controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: "/snap", expectedTree: T }, handle: "h", primaryCwd: root, argv: ["build"], nonce: "a".repeat(64), ...over }),
      (e) => e instanceof SnapshotRequestError && why.test(e.message), JSON.stringify(over));
    assert.equal(fs.existsSync(path.join(root, "control", "snapshot-requests")) && fs.readdirSync(path.join(root, "control", "snapshot-requests")).length > 0, false);
  }
  // A snapshot-requests that is a link is never written through.
  const root = tmp(t); fs.mkdirSync(path.join(root, "control")); fs.mkdirSync(path.join(root, "elsewhere"));
  fs.symlinkSync(path.join(root, "elsewhere"), path.join(root, "control", "snapshot-requests"));
  assert.throws(() => writeSnapshotRequest({ controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: "/snap", expectedTree: T }, handle: "h", primaryCwd: root, argv: ["build"] }), /not a real directory/);
  assert.equal(freshNonce().length, 64); assert.match(freshNonce(), /^[0-9a-f]{64}$/); assert.notEqual(freshNonce(), freshNonce());
});

test("observedEnv: the managed Rust variables as the client records them, matched case-insensitively, spelling kept", (t) => {
  assert.deepEqual(observedEnvOf({ PATH: "/x", CARGO_TARGET_DIR: "/t", RUSTFLAGS: "-D warnings", HOME: "/h" }, "darwin"), { CARGO_TARGET_DIR: "/t", RUSTFLAGS: "-D warnings" });
  // RustRight keeps the key as captured (shims.rs capture_managed_rust_environment: key.clone()), on every platform.
  assert.deepEqual(observedEnvOf({ Cargo_Home: "C:\\c", Path: "x" }, "win32"), { Cargo_Home: "C:\\c" });
  assert.deepEqual(observedEnvOf({ cargo_target_dir: "/t" }, "linux"), { cargo_target_dir: "/t" });
  assert.deepEqual(observedEnvOf({ CARGO_HOME: "/a", cargo_home: "/b" }, "linux"), { CARGO_HOME: "/a", cargo_home: "/b" }, "POSIX names that differ by case are two variables");
  // Windows hands the child one of several case variants, the first in sorted order; only that one is observed.
  assert.deepEqual(observedEnvOf({ Cargo_Home: "/b", CARGO_HOME: "/a" }, "win32"), { CARGO_HOME: "/a" });
  // The spelling is bound by the request digest: an uppercased copy would not match what the broker signed.
  const { made, argv, cwd } = request(t);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, observedEnv: observedEnvOf({ Cargo_Home: "C:\\c" }, "win32"), carrier: made.carrier })),
    requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, observedEnv: { CARGO_HOME: "C:\\c" }, carrier: made.carrier })));
});

test("request digest: the extended core binds every field the broker adds for a snapshot request", (t) => {
  const { made, argv, cwd } = request(t);
  const base = requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, carrier: made.carrier }));
  assert.match(base, /^[0-9a-f]{64}$/);
  const core = snapshotRequestCore({ argv, workspace: cwd, carrier: made.carrier });
  assert.deepEqual(Object.keys(core).sort(), ["adapter", "commit", "declaredInputs", "env", "inputManifest", "nonce", "observedEnv", "outputs", "ownership", "requestedArgv", "shim", "snapshot", "workspace"]);
  for (const change of [
    (c) => ({ ...c, commit: { ...c.commit, sha: "d".repeat(40) } }), (c) => ({ ...c, snapshot: { ...c.snapshot, path: "/other" } }),
    (c) => ({ ...c, declaredInputs: { inputs: [] } }), (c) => ({ ...c, ownership: { handle: "other" } }), (c) => ({ ...c, nonce: "b".repeat(64) }),
  ]) assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, carrier: change(made.carrier) })), base);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv: [...argv, "-q"], workspace: cwd, carrier: made.carrier })), base);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: `${cwd}/x`, carrier: made.carrier })), base);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, observedEnv: { RUSTFLAGS: "x" }, carrier: made.carrier })), base);
});

// ---- row 7.22: every returned-evidence field is required and matches the request; the signature covers the snapshot evidence ----
const JOB = "11111111-1111-4111-8111-111111111111";
function receiptCase(t, { tamper = "none", inputs = ["gen/in.txt"], carrierInputs = inputs ?? [], over = {}, mutate = null, argv = ["test", "-p", "a", "--locked"] } = {}) {
  const fx = makeFakeBroker(t, { inputs });
  const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: carrierInputs, handle: fx.handle, primaryCwd: fx.checkout, argv, nonce: "a".repeat(64) });
  const receipt = signedReceipt(fx, { id: JOB, carrier: made.carrier, argv, workspace: fx.checkout, tamper, mutate });
  const check = (extra = {}) => verifySnapshotReceipt(receipt, { pins: fx.pins, jobId: JOB, carrier: made.carrier, argv, workspace: fx.checkout, checkout: fx.checkout, outputRoots: [fx.wsRoot], platform: process.platform, ...over, ...extra });
  return { fx, made, receipt, check, argv };
}

test("[7.22] a fully formed receipt verifies, with and without declared inputs", (t) => {
  for (const inputs of [["gen/in.txt"], null]) {
    const c = receiptCase(t, { inputs });
    const { evidence, payload } = c.check();
    assert.equal(evidence.outputDir, c.fx.outputDir);
    assert.equal(payload.result.status, "SUCCEEDED");
    assert.deepEqual(evidence.declaredInputDigests.map((d) => d.path), inputs ?? []);
  }
});

test("a broker verdict after a clean run (FAILED, exit 0) is refused and the message carries the receipt's unsigned error for diagnosis", (t) => {
  const c = receiptCase(t, { tamper: "failed" });
  c.receipt.error = "RR_UNEXPLAINED_COLD: 1 rebuilt unit(s) have no explained cause";
  assert.throws(() => c.check(), (e) => e.code === "result" && /signed FAILED \(exit \d+; the receipt's unsigned error: RR_UNEXPLAINED_COLD/.test(e.message) && /not SUCCEEDED with exit 0/.test(e.message));
});

test("[7.22] each refusal: provenance, signature, pins, request, job, nonce, signed outcome and snapshot evidence", (t) => {
  const cases = [
    // [tamper, error code, message pattern]
    ["noProvenance", "legacy", /no provenance envelope/], ["editedAfterSigning", "signature", /does not verify/], ["signedByOtherKey", "signature", /does not verify/],
    ["otherHost", "host", /another host/], ["otherBrokerDigest", "runtime", /another broker runtime/], ["otherJob", "job", /another job/], ["otherNonce", "nonce", /nonce/],
    ["requestOtherArgv", "request", /request digest/],
    ["failed", "result", /signed FAILED/], ["noResult", "result", /no result/],
    ["noSnapshotEvidence", "evidence", /no snapshotEvidence/], ["receiptCopyDiffers", "evidence", /outputDir differs/],
    ["treeAtExit", "evidence", /throughout/], ["treeAtStart", "evidence", /throughout/], ["otherHandle", "evidence", /handle is not this gate's/], ["otherCommit", "evidence", /signed commit is not/],
    ["otherSnapshotPath", "evidence", /signed snapshot/], ["otherRequestId", "evidence", /request id/],
    ["noBrokerVersion", "evidence", /brokerVersion differs/], ["noExecutor", "evidence", /executor differs/],
  ];
  for (const [tamper, code, why] of cases) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === code && why.test(e.message), `${tamper}: expected ${code} ${why}`);
  }
});

test("[7.22] declared inputs: the signed digests must be the gate's own digest of the snapshot's declared inputs, from the manifest it carried", (t) => {
  for (const tamper of ["digestWrong", "digestsDropped"]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && /declaredInputDigests are not the gate's own digest/.test(e.message), tamper);
  }
  // A declared input changed in the snapshot after the broker digested it: the gate's own digest no longer matches.
  const moved = receiptCase(t);
  fs.writeFileSync(path.join(moved.fx.snapshot, "gen", "in.txt"), "changed after the build\n");
  assert.throws(() => moved.check(), /declaredInputDigests are not the gate's own digest/);
  // The manifest in the snapshot lists inputs the request did not carry.
  const other = receiptCase(t, { carrierInputs: [] });
  assert.throws(() => other.check(), /declared inputs are not the ones this request carried/);
  // The checkout's own manifest is never read: a different one there changes nothing.
  const c = receiptCase(t);
  fs.mkdirSync(path.join(c.fx.checkout, ".rightkit"), { recursive: true });
  fs.writeFileSync(path.join(c.fx.checkout, ".rightkit", "compile-inputs.json"), JSON.stringify({ inputs: ["Cargo.toml"] }));
  assert.equal(c.check().evidence.declaredInputDigests.length, 1);
});

test("[7.22] output directory: absolute, outside the snapshot and the checkout, and a managed generation target", (t) => {
  for (const [tamper, why] of [["outputInsideSnapshot", /inside the snapshot/], ["outputInsideCheckout", /inside the checkout/], ["outputOutsideWs", /not a managed generation target/], ["outputNotTarget", /not a managed generation target/], ["outputRelative", /no absolute output directory/]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && why.test(e.message), tamper);
  }
  const c = receiptCase(t);
  // The gate must know where outputs may go: without a checkout and a managed root nothing can be judged, so it refuses.
  assert.throws(() => c.check({ outputRoots: [] }), /no checkout or managed output root/);
  assert.throws(() => c.check({ checkout: undefined }), /no checkout or managed output root/);
  assert.throws(() => c.check({ outputRoots: [path.join(c.fx.root, "another-ws")] }), /not a managed generation target/);
  const wsRoot = c.fx.wsRoot;
  for (const [out, ok] of [[`${wsRoot}/0123456789ab/g/002/target`, true], [`${wsRoot}/0123456789ab/g/1000/target`, true], [`${wsRoot}/0123456789abcdef/g/002/target`, false], [`${wsRoot}/0123456789a/g/002/target`, false], [`${wsRoot}/0123456789AB/g/002/target`, false], [`${wsRoot}/0123456789ab/g/2/target`, false], [`${wsRoot}/zzzzzzzzzzzz/g/002/target`, false],
    [`${wsRoot}/0123456789ab/g/002/target/debug`, false], [`${wsRoot}/0123456789ab/002/target`, false], [`${wsRoot}/../outside/0123456789ab/g/002/target`, false], [`${wsRoot}`, false]]) {
    assert.equal(isGenerationTarget(out, [wsRoot], process.platform), ok, out);
  }
  // The leaf is per generation (rustright-state lib.rs:52-62, :422-461): `target` or `t`, on either platform; any other leaf is refused.
  const winWs = "D:\\.rightkit-managed\\rightkit-build-control\\ws"; const winGen = `${winWs}\\0123456789ab\\g\\002`;
  for (const leaf of ["t", "target"]) {
    assert.equal(isGenerationTarget(`${winGen}\\${leaf}`, [winWs], "win32"), true, `win32 ${leaf}`);
    assert.equal(isGenerationTarget(`${wsRoot}/0123456789ab/g/002/${leaf}`, [wsRoot], "darwin"), true, `darwin ${leaf}`);
    assert.equal(isGenerationTarget(`${wsRoot}/0123456789ab/g/002/${leaf}`, [wsRoot], "linux"), true, `linux ${leaf}`);
  }
  for (const leaf of ["T", "tt", "debug", "targets", "release", ""]) {
    if (leaf !== "T") assert.equal(isGenerationTarget(`${winGen}\\${leaf}`, [winWs], "win32"), false, `win32 ${leaf}`); // Windows paths are case-insensitive
    assert.equal(isGenerationTarget(`${wsRoot}/0123456789ab/g/002/${leaf}`, [wsRoot], "linux"), false, `linux ${leaf}`);
  }
  assert.equal(isGenerationTarget("d:\\.RIGHTKIT-managed\\rightkit-build-control\\ws\\0123456789ab\\g\\002\\t", [winWs], "win32"), true, "Windows paths compare case-insensitively");
  assert.equal(isGenerationTarget("E:\\elsewhere\\0123456789ab\\g\\002\\t", [winWs], "win32"), false, "outside the managed root");
  assert.equal(isGenerationTarget(`${wsRoot}/../outside/0123456789ab/g/002/t`, [wsRoot], "linux"), false, "outside the managed root");
  assert.deepEqual([...GENERATION_LEAVES], ["target", "t"]);
});

test("[7.22] a nested compile's directory is inside a generation target with either leaf on either platform, and nowhere else", () => {
  const winWs = "D:\\ws"; const gen = `${winWs}\\0123456789ab\\g\\001`;
  for (const leaf of ["t", "target"]) {
    assert.equal(isInsideGenerationTarget(`${gen}\\${leaf}`, [winWs], "win32"), true, leaf);
    assert.equal(isInsideGenerationTarget(`${gen}\\${leaf}\\debug\\x`, [winWs], "win32"), true, leaf);
    assert.equal(isInsideGenerationTarget(`/w/ws/0123456789ab/g/001/${leaf}/debug`, ["/w/ws"], "linux"), true, leaf);
    assert.equal(isInsideGenerationTarget(`/w/ws/0123456789ab/g/001/${leaf}`, ["/w/ws"], "darwin"), true, leaf);
  }
  assert.equal(isInsideGenerationTarget(`${gen}\\debug\\x`, [winWs], "win32"), false);
  assert.equal(isInsideGenerationTarget("/w/ws/0123456789ab/g/001/other/x", ["/w/ws"], "linux"), false);
  assert.equal(isInsideGenerationTarget("/elsewhere/0123456789ab/g/001/t/x", ["/w/ws"], "linux"), false);
});

test("[7.22] lease handles: each carrier names its own `<ownership>#<n>`; the bare handle, another ownership's and a malformed one are refused; receipts bind the exact sub-handle", (t) => {
  const a = nextCallHandle("own-1"); const b = nextCallHandle("own-1");
  assert.notEqual(a, b); assert.equal(isCallHandleOf(a, "own-1"), true); assert.equal(ownershipOfHandle(a), "own-1");
  for (const bad of ["own-1", "own-1#", "own-10#1", "own-1#a b", "own-1##1", undefined, 7]) assert.equal(isCallHandleOf(bad, "own-1"), false, String(bad));
  assert.equal(isCallHandleOf("own-1#1", "own-1"), true);
  assert.throws(() => request(t, { callHandle: "own-1" }), /not a sub-handle/);
  assert.throws(() => request(t, { callHandle: "other#1" }), /not a sub-handle/);
  assert.equal(request(t, { callHandle: "own-1#fixed-1" }).made.carrier.ownership.handle, "own-1#fixed-1");
  assert.notEqual(request(t).made.carrier.ownership.handle, request(t).made.carrier.ownership.handle, "each call gets a fresh one");
  // The receipt must sign exactly the sub-handle the gate sent: another sub-handle of the same ownership, or the bare handle, fails.
  assert.doesNotThrow(() => receiptCase(t).check());
  for (const tamper of ["otherSubHandle", "bareHandle", "otherHandle"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && /handle is not this gate's/.test(e.message), tamper);
});

test("[7.22] stage evidence and the unsigned fields: freshness, attribution and toolchain are signed and equal; timingsMs, exit, diagnosticsPath, executor are present with their types, and the retired verdict and refreshed are refused", (t) => {
  for (const [tamper, why] of [["noStageEvidence", /no stageEvidence/], ["noFreshness", /freshness is missing/], ["freshnessReceiptOnly", /freshness is missing/], ["freshnessSignedOnly", /freshness is missing/], ["freshnessDiffers", /freshness differs from the signed stage evidence/]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && why.test(e.message), tamper);
  }
  for (const [tamper, field] of [["noTimings", "timingsMs"], ["noExit", "exit"], ["noDiagnostics", "diagnosticsPath"]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} is missing or has the wrong type`).test(e.message), tamper);
  }
  // Wrong types are as bad as missing ones.
  for (const [field, value] of [["timingsMs", [1]], ["exit", "0"], ["signal", {}], ["diagnosticsPath", ""]]) {
    const c = receiptCase(t); c.receipt[field] = value;
    assert.throws(() => c.check(), new RegExp(`receipt ${field} is missing or has the wrong type`), `${field}=${JSON.stringify(value)}`);
  }
  for (const [tamper, field] of [["retiredVerdict", "verdict"], ["retiredRefreshed", "refreshed"]]) {
    assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt carries the retired field ${field}`).test(e.message), tamper);
  }
});

// ---- F7 re-decision #4: attribution and toolchain only for compile-class commands; the rest for every carried command ----
const NON_COMPILE = [["metadata", "--format-version", "1", "--offline"], ["fetch", "--locked"], ["fmt", "--check"], ["tree", "--locked"], ["--frozen", "pkgid"], ["version"]];
const COMPILE = [["check", "--locked"], ["clippy", "--", "-D", "warnings"], ["build"], ["doc", "--no-deps"], ["rustc", "--", "-Zx"], ["test", "-p", "a"], ["+stable", "--locked", "build"], ["--color", "always", "test"], ["b"], ["t"]];

test("[F7 #4] a non-compile command (metadata, fetch --locked, fmt --check, tree --locked) verifies without attribution and toolchain, with or without them present", (t) => {
  for (const argv of NON_COMPILE) {
    for (const tamper of ["freshnessOnly", "none"]) {
      const c = receiptCase(t, { argv, tamper });
      assert.equal(c.check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} / ${tamper}`);
    }
  }
});

test("[F7 #4] a compile-class command (check, clippy, build, doc, rustc, test, and their aliases) fails without attribution or without toolchain", (t) => {
  for (const argv of COMPILE) {
    assert.equal(receiptCase(t, { argv }).check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} verifies with both`);
    for (const [tamper, name] of [["noAttribution", "attribution"], ["noToolchain", "toolchain"], ["freshnessOnly", "attribution"]]) {
      const c = receiptCase(t, { argv, tamper });
      assert.throws(() => c.check(), (e) => e.code === "evidence" && new RegExp(`${name} is missing from the signed stage evidence or the receipt`).test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
  }
});

test("[#1311] a read-only carried command's receipt passes with freshness (or the whole census) absent on both sides", (t) => {
  for (const argv of NON_COMPILE) {
    for (const tamper of ["noFreshness", "freshnessOnly", "none"]) {
      assert.equal(receiptCase(t, { argv, tamper }).check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} / ${tamper}`);
    }
    // The whole census absent on both sides: a read-only metadata job has none.
    assert.equal(receiptCase(t, { argv, tamper: "noStageCensus" }).check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} / no census`);
  }
});

test("[#1311] a compile-class receipt without a census fails; freshness on one side only fails (either side); a tampered freshness fails; the parity check is unchanged for read-only commands", (t) => {
  for (const argv of COMPILE) {
    assert.throws(() => receiptCase(t, { argv, tamper: "noStageCensus" }).check(), (e) => e.code === "evidence" && /is missing from the signed stage evidence or the receipt/.test(e.message), `${argv.join(" ")} / no census`);
    assert.throws(() => receiptCase(t, { argv, tamper: "noFreshness" }).check(), (e) => e.code === "evidence" && /freshness is missing from the signed stage evidence or the receipt/.test(e.message), `${argv.join(" ")} / no freshness on either side`);
    for (const tamper of ["freshnessReceiptOnly", "freshnessSignedOnly"]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /freshness is missing/.test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
  }
  for (const argv of NON_COMPILE) {
    // Present on the receipt side only (the signed stage evidence has none).
    assert.throws(() => receiptCase(t, { argv, tamper: "freshnessSignedOnly" }).check(), (e) => e.code === "evidence" && /receipt freshness differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / receipt side only`);
    // Present on the signed side only (the receipt dropped it).
    assert.throws(() => receiptCase(t, { argv, tamper: "freshnessReceiptOnly" }).check(), (e) => e.code === "evidence" && /receipt freshness differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / signed side only`);
    assert.throws(() => receiptCase(t, { argv, tamper: "freshnessDiffers" }).check(), (e) => e.code === "evidence" && /receipt freshness differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / tampered`);
    for (const tamper of ["attributionReceiptOnly", "attributionSignedOnly"]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /attribution differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
    for (const [tamper, field] of [["noExit", "exit"], ["noTimings", "timingsMs"], ["noDiagnostics", "diagnosticsPath"]]) {
      assert.throws(() => receiptCase(t, { argv, tamper: tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} is missing or has the wrong type`).test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
    for (const [tamper, field] of [["retiredVerdict", "verdict"], ["retiredRefreshed", "refreshed"]]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt carries the retired field ${field}`).test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
  }
});

test("[#1311] a nested adapter job's compile children are still validated when the job signs no freshness, attribution or toolchain", (t) => {
  const JOBID = JOB; const argv = ["exec", "tauri", "build"];
  const child = (fx, over = {}) => ({ requestId: "22222222-2222-4222-8222-222222222222", parentRequestId: JOBID, shim: "cargo", argv: ["build", "--locked"], cwd: path.join(fx.snapshot, "native"), exitCode: 0, status: "SUCCEEDED", outputDir: path.join(fx.outputDir, "nested", "native"), ...over });
  const run = (tamper, over = {}) => {
    const fx = makeFakeBroker(t, { inputs: ["gen/in.txt"] });
    const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: ["gen/in.txt"], handle: fx.handle, primaryCwd: fx.checkout, argv, nonce: "a".repeat(64) });
    const receipt = signedReceipt(fx, { id: JOBID, carrier: made.carrier, argv, workspace: fx.checkout, tamper, adapter: "rightkit-pnpm", children: [child(fx, over)] });
    return () => verifySnapshotReceipt(receipt, { pins: fx.pins, jobId: JOBID, carrier: made.carrier, argv, workspace: fx.checkout, checkout: fx.checkout, outputRoots: [fx.wsRoot], platform: process.platform, adapter: "rightkit-pnpm" });
  };
  const ok = run("freshnessOnly")();
  assert.equal(ok.nestedChildren.length, 1, "the adapter job verifies and its compile child is carried");
  assert.equal(run("noFreshness")().nestedChildren.length, 1, "no freshness on either side is fine for an adapter job");
  assert.throws(run("childFailed"), (e) => e.code === "nested" && /exited 1, not 0/.test(e.message));
  assert.throws(run("none", { cwd: "/elsewhere" }), (e) => e.code === "nested" && /outside the snapshot/.test(e.message));
  assert.throws(run("nestedCopyDiffers"), (e) => e.code === "nested" && /differs from the signed list/.test(e.message));
});

// ---- RR's receipt shapes (item 4 row 4.18, answers at rustright HEAD f4cf1170) ----
test("[RR 4.18] attribution: `{available:false, reason}` is accepted and recorded when the reason is a non-empty string; no reason, or not an object, is refused", (t) => {
  const ok = receiptCase(t, { tamper: "attributionUnavailable" }).check();
  assert.deepEqual(ok.attribution, { available: false, reason: "cargo-json-message-format-not-requested" }, "recorded for the consumer, which must treat available:false as FAILED where per-package attribution is needed");
  assert.equal(receiptCase(t).check().attribution.available, true);
  assert.throws(() => receiptCase(t, { tamper: "attributionNoReason" }).check(), (e) => e.code === "evidence" && /unavailable and names no reason/.test(e.message));
  assert.throws(() => receiptCase(t, { tamper: "attributionNotObject" }).check(), (e) => e.code === "evidence" && /not \{available/.test(e.message));
});

test("[RR 4.18] toolchain is `<toolchain>|cargo:<sha256>|rustc:<sha256>`, and is returned for the record; another shape is refused", (t) => {
  assert.match(receiptCase(t).check().toolchain, /^stable\|cargo:1{64}\|rustc:2{64}$/);
  for (const tamper of ["toolchainObject", "toolchainNoHashes"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && /toolchain is not <toolchain>\|cargo:<sha256>\|rustc:<sha256>/.test(e.message), tamper);
});

test("[RR 4.18] top-level fields: diagnosticsPath may be null, timingsMs needs all six keys (each a number or null), exit may be null only with a signal and a null exit without one is not a success, and a receipt copy that differs from the signed one is refused", (t) => {
  assert.equal(receiptCase(t, { tamper: "diagnosticsNull" }).check().payload.result.status, "SUCCEEDED");
  assert.equal(receiptCase(t, { tamper: "timingsNulls" }).check().payload.result.status, "SUCCEEDED");
  assert.throws(() => receiptCase(t, { tamper: "timingsMissingKey" }).check(), /receipt timingsMs is missing or has the wrong type/);
  assert.throws(() => receiptCase(t, { tamper: "exitNullNoSignal" }).check(), /exit null and signal null are not a success/);
  const bad = receiptCase(t); bad.receipt.exit = 1;
  assert.throws(() => bad.check(), /not a success/);
  const signalled = receiptCase(t); signalled.receipt.exit = null; signalled.receipt.signal = "SIGKILL"; delete signalled.receipt.provenance.payload.resultEvidence.exit; delete signalled.receipt.provenance.payload.resultEvidence.signal;
  assert.throws(() => signalled.check(), (e) => e.code === "signature" || /not a success/.test(e.message), "a signalled run is never a success (editing the signed payload breaks the signature)");
});

test("[L1] the signed result fields are compared where the broker signs them (payload.resultEvidence): a missing, partial or disagreeing resultEvidence fails, a matching one passes", (t) => {
  assert.equal(receiptCase(t).check().payload.result.status, "SUCCEEDED");
  assert.deepEqual(Object.keys(receiptCase(t).check().payload.resultEvidence).sort(), [...RESULT_EVIDENCE_FIELDS].sort());
  assert.equal(Object.hasOwn(receiptCase(t).check().payload, "exit"), false, "the fake signs them as the real broker does: not at the top level");
  for (const tamper of ["noResultEvidence", "resultEvidenceTopLevelOnly", "resultEvidenceEmpty"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && /no resultEvidence|differs from the signed result evidence/.test(e.message), tamper);
  for (const [tamper, field] of [["resultEvidenceNoExit", "exit"], ["resultEvidenceNoSignal", "signal"], ["resultEvidenceNoTimings", "timingsMs"], ["resultEvidenceNoDiagnostics", "diagnosticsPath"],
    ["resultEvidenceExitDiffers", "exit"], ["resultEvidenceSignalDiffers", "signal"], ["resultEvidenceTimingsDiffer", "timingsMs"], ["resultEvidenceDiagnosticsDiffers", "diagnosticsPath"]]) {
    assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} differs from the signed result evidence`).test(e.message), tamper);
  }
  assert.throws(() => receiptCase(t, { tamper: "resultEvidenceRetiredVerdict" }).check(), (e) => e.code === "evidence" && /retired field verdict/.test(e.message));
  // Added field: signed but unnamed by the protocol (in resultEvidence, or as a timingsMs key both sides carry).
  assert.throws(() => receiptCase(t, { tamper: "resultEvidenceAddedField" }).check(), (e) => e.code === "evidence" && /unknown field extra/.test(e.message), "added signed field");
  assert.throws(() => receiptCase(t, { tamper: "resultEvidenceTimingsAddedKey" }).check(), (e) => e.code === "evidence" && /unknown key bogus/.test(e.message), "added timings key");
  // Dropped on the receipt side while still signed, and present on the receipt while unsigned (one-sided either way), and dropped on both sides.
  for (const field of RESULT_EVIDENCE_FIELDS) {
    const dropReceipt = receiptCase(t); delete dropReceipt.receipt[field];
    assert.throws(() => dropReceipt.check(), (e) => e.code === "evidence", `receipt drops ${field}, the signature still carries it`);
    const dropBoth = receiptCase(t); delete dropBoth.receipt[field]; delete dropBoth.receipt.provenance.payload.resultEvidence[field];
    assert.throws(() => dropBoth.check(), (e) => e.code === "evidence" || e.code === "signature", `both drop ${field}`);
  }
  // The gate still reads the signed status and exit code: a signed FAILED never passes however the copies agree.
  assert.throws(() => receiptCase(t, { tamper: "failed" }).check(), (e) => e.code === "result");
  // A receipt copy edited after the fact (the signature still verifies the payload) disagrees with resultEvidence.
  for (const [field, value] of [["timingsMs", { admission: 1, queue: 0, bind: 1, cargo: 3, afterExitBind: 1, diagnostics: 1 }], ["diagnosticsPath", "/diag/elsewhere.json"]]) {
    const c = receiptCase(t); c.receipt[field] = value;
    assert.throws(() => c.check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} differs from the signed result evidence`).test(e.message), field);
  }
});

test("[F7 #4] the subcommand is read from the argv the way cargo reads it: after +toolchain and global options, before --", () => {
  for (const [argv, sub] of [[["build"], "build"], [["+nightly", "test"], "test"], [["--color", "always", "check"], "check"], [["--locked", "-q", "metadata"], "metadata"], [["--", "build"], null], [[], null], [["--explain", "E0001", "doc"], "doc"]]) {
    assert.equal(cargoSubcommandOf(argv), sub, argv.join(" "));
  }
  assert.equal(isCompileClass(["clippy"]), true); assert.equal(isCompileClass(["run"]), true); assert.equal(isCompileClass(["metadata"]), false); assert.equal(isCompileClass([]), false);
});

test("[GB-02] the compile table is the broker's: pinned to rustright b84ae605 command_policy.rs cargo_capability (:581 fix, :597 run, :611 package, :615 publish, :619 the BUILD list)", () => {
  // The broker's own spellings, copied from the cited lines at b84ae605 (git show b84ae605:crates/rustright-policy/src/command_policy.rs): a change on either side shows here.
  const brokerBuildList = ["build", "check", "test", "bench", "clippy", "doc", "rustc", "rustdoc"]; // :619
  const brokerOthers = ["fix", "run", "package", "publish"]; // :581, :597, :611, :615
  assert.deepEqual([...BROKER_COMPILE_COMMANDS].sort(), [...brokerBuildList, ...brokerOthers].sort());
  for (const command of [...brokerBuildList, "fix", "run", "publish"]) assert.equal(isCompileClass([command]), true, command);
  assert.equal(isCompileClass(["+nightly", command0()]), true);
  // package compiles unless it is exactly the observation listing the broker admits (build_rules.rs observation_only_cargo).
  assert.equal(isCompileClass(["package", "-p", "a"]), true);
  assert.equal(isCompileClass(["package", "-p", "a", "--list"]), true, "an unlocked listing is not the observation shape");
  assert.equal(isCompileClass(["package", "-p", "a", "--list", "--locked"]), false);
  assert.equal(isCompileClass(["package", "--package=a", "--list", "--locked", "--allow-dirty"]), false);
  for (const argv of [["package", "-p=a", "--list", "--locked"], ["package", "-p", "a", "-p", "b", "--list", "--locked"], ["package", "--list", "--locked"], ["package", "-p", "a", "--list", "--locked", "--no-verify"], ["+nightly", "package", "-p", "a", "--list", "--locked"], ["package", "-p", "a", "--list", "--locked", "--", "x"], ["package", "-p", "-q", "--list", "--locked"]]) {
    assert.equal(isPackageObservation(argv), false, argv.join(" "));
    assert.equal(isCompileClass(argv), true, argv.join(" "));
  }
  // Not compiles: the read-only and write classes.
  for (const argv of [["metadata"], ["metadata", "--no-deps"], ["fetch", "--locked"], ["fmt", "--check"], ["tree", "--locked"], ["--version"], ["deny", "check"], ["audit"], ["about", "generate"], []]) assert.equal(isCompileClass(argv), false, argv.join(" "));
  // The one-letter aliases are the gate's stricter reading (the broker lists none): each reads as its command.
  for (const [alias, command] of Object.entries(CARGO_COMMAND_ALIASES)) assert.equal(isCompileClass([alias]), isCompileClass([command]), alias);
  // lockedArgv derives from the same table: every compile command is locked, nothing else is.
  for (const command of [...brokerBuildList, "fix", "run", "publish"]) assert.deepEqual(lockedArgv([command, "-x"]), [command, "--locked", "-x"], command);
  assert.deepEqual(lockedArgv(["package", "-p", "a"]), ["package", "--locked", "-p", "a"]);
  assert.deepEqual(lockedArgv(["package", "-p", "a", "--list", "--locked"]), ["package", "-p", "a", "--list", "--locked"]);
  for (const argv of [["metadata"], ["fmt", "--check"], ["fetch"]]) assert.deepEqual(lockedArgv(argv), argv);
  for (const argv of [["build"], ["rustdoc"], ["package", "-p", "a"], ["bench"]]) assert.equal(isLockBound(argv), isCompileClass(argv));
});
const command0 = () => "build";

test("[GB-02] every compile command's receipt must carry freshness, attribution and toolchain: dropping a field from BOTH the signed evidence and the receipt fails for each; a read-only command passes without them", (t) => {
  for (const argv of [["bench"], ["run"], ["rustdoc"], ["package", "-p", "a"], ["fix"], ["publish"], ["clippy"], ["build"]]) {
    for (const field of ["freshness", "attribution", "toolchain"]) {
      const c = receiptCase(t, { argv });
      delete c.receipt[field]; delete c.receipt.provenance.payload.stageEvidence[field];
      assert.throws(() => c.check(), (e) => e.code === "evidence" || e.code === "signature", `${argv.join(" ")} without ${field}`);
    }
  }
});

test("[7.22] declared inputs are read from the snapshot and digested as the broker digests them (check_admission.rs snapshot_inputs)", (t) => {
  const root = tmp(t);
  const snap = path.join(root, "snap"); fs.mkdirSync(path.join(snap, ".rightkit"), { recursive: true });
  assert.deepEqual(readDeclaredInputs(snap), [], "no manifest: an empty declared set");
  const manifest = (v) => fs.writeFileSync(path.join(snap, ".rightkit", "compile-inputs.json"), typeof v === "string" ? v : JSON.stringify(v));
  manifest({ inputs: ["a.txt", "dir"] });
  fs.writeFileSync(path.join(snap, "a.txt"), "hello"); fs.mkdirSync(path.join(snap, "dir", "sub"), { recursive: true });
  fs.writeFileSync(path.join(snap, "dir", "b"), "B"); fs.writeFileSync(path.join(snap, "dir", "a"), "A"); fs.writeFileSync(path.join(snap, "dir", "sub", "c"), "C");
  const inputs = readDeclaredInputs(snap);
  assert.deepEqual(inputs, ["a.txt", "dir"]);
  const h = (...parts) => crypto.createHash("sha256").update(Buffer.concat(parts.map((p) => (typeof p === "string" ? Buffer.from(p) : p)))).digest("hex");
  // Per file, in sorted directory order: relative name, NUL, bytes, NUL (names joined with `/` on every platform, GB-49).
  assert.deepEqual(declaredInputDigestsOf(snap, inputs, { platform: "darwin" }), [
    { path: "a.txt", sha256: h("a.txt", "\0", "hello", "\0") },
    { path: "dir", sha256: h("dir/a", "\0", "A", "\0", "dir/b", "\0", "B", "\0", "dir/sub/c", "\0", "C", "\0") },
  ]);
  assert.equal(declaredInputDigestsOf(snap, ["dir"], { platform: "win32" })[0].sha256, h("dir/a", "\0", "A", "\0", "dir/b", "\0", "B", "\0", "dir/sub/c", "\0", "C", "\0"), "GB-49: RustRight now hashes names with `/` on Windows too (input_name), so the gate no longer joins with a backslash there");
  // A malformed or unsafe manifest is refused before any request.
  for (const [body, why] of [["not json", /not JSON/], [{ inputs: "x" }, /list of paths/], [{ inputs: [1] }, /non-empty relative path/], [{ inputs: ["/abs"] }, /plain relative path/], [{ inputs: ["../x"] }, /plain relative path/],
    [{ inputs: ["a/./b"] }, /plain relative path/], [{ inputs: ["a//b"] }, /plain relative path/], [{ inputs: [".git/config"] }, /inside \.git/], [{ inputs: [".rightkit/compile-inputs.json"] }, /declaration file itself/], [{ inputs: [".rightkit"] }, /declaration file itself/]]) {
    manifest(body);
    assert.throws(() => readDeclaredInputs(snap), (e) => e instanceof SnapshotRequestError && e.code === "declared-inputs" && why.test(e.message), JSON.stringify(body));
  }
  // A link anywhere is refused, a missing input too (creating a link needs a privilege on Windows: those cases run where it is held).
  const link = (target, at) => { try { fs.symlinkSync(target, at); return true; } catch { return false; } };
  if (link(path.join(snap, "a.txt"), path.join(snap, "dir", "link"))) {
    assert.throws(() => declaredInputDigestsOf(snap, ["dir"]), /holds a link/);
    fs.rmSync(path.join(snap, "dir", "link"));
  }
  assert.throws(() => declaredInputDigestsOf(snap, ["missing"]), /ENOENT/);
  if (link(path.join(snap, "dir"), path.join(snap, "viaLink"))) assert.throws(() => declaredInputDigestsOf(snap, ["viaLink"]), /runs through a link/);
  fs.mkdirSync(path.join(root, "real")); fs.rmSync(path.join(snap, ".rightkit", "compile-inputs.json"));
  fs.writeFileSync(path.join(root, "real", "m.json"), JSON.stringify({ inputs: [] }));
  if (link(path.join(root, "real", "m.json"), path.join(snap, ".rightkit", "compile-inputs.json"))) assert.throws(() => readDeclaredInputs(snap), /runs through a link/, "a manifest that is a link is refused");
});

test("request reference: the client's rustright.request-ref.v1, or a refusal", (t) => {
  const root = tmp(t); const f = path.join(root, "ref.json");
  fs.writeFileSync(f, JSON.stringify({ schema: "rustright.request-ref.v1", requestId: "r-1", submittedAt: "2026-10-29T00:00:00Z", workspace: root }));
  assert.equal(readRequestRef(f).requestId, "r-1");
  fs.writeFileSync(f, JSON.stringify({ schema: "other", requestId: "r-1", workspace: root }));
  assert.throws(() => readRequestRef(f), /not a rustright.request-ref.v1/);
  assert.throws(() => readRequestRef(path.join(root, "missing.json")), /no request reference/);
});

test("runScans: a broker cargo request builds the snapshot under its own carrier and passes only on the broker's verified evidence", async (t) => {
  const { runScans } = await import("./deterministic-scans.mjs");
  const fx = makeFakeBroker(t);
  const { root, checkout, snapshot: snap, pins } = fx;
  const scan = { id: "check:core", class: "B", tool: "rustright", args: ["cargo", "check", "-p", "core", "--locked"], root: "cargoLive", env: {}, command: "rustright cargo check -p core --locked" };
  // Class B runs only after class A ran in the same call: one passing class A row.
  const classA = { id: "a:noop", class: "A", tool: "node", args: ["-e", "0"], root: "live", env: {}, command: "node -e 0" };
  const plan = { roots: { contentRoot: snap, liveRoot: snap, cargoLiveRoot: checkout }, scans: [classA, scan], notes: [], deferred: [] };
  // A fake client that applies RustRight's reader rules, records the request reference, and keeps what it saw.
  const run = async ({ tamper = null, badCarrier = false, mark, launch = null } = {}) => {
    const seen = { submitsBefore: 0, launched: false }; let submits = 0;
    const spawn = (command, args, opts = {}) => {
      const env = opts.env ?? {};
      // Tool probes and the class A row: anything that is not the carried request.
      if (!env.RUSTRIGHT_SNAPSHOT_REQUEST) { if (args.includes("check")) seen.launched = true; return { status: 0, stdout: "v1\n", stderr: "" }; }
      seen.launched = true;
      const d = JSON.parse(env.RUSTRIGHT_SNAPSHOT_REQUEST);
      seen.file = d.path; seen.refDir = path.dirname(env.RUSTRIGHT_REQUEST_ID_FILE);
      if (launch) return launch(opts);
      const bytes = fs.readFileSync(d.path);
      const carrier = JSON.parse(bytes);
      seen.ok = sha(bytes) === d.sha256 && carrier.nonce === env.RUSTRIGHT_ATTEST_NONCE && carrier.argvSha256 === sha(canonicalJson(args.slice(1))) && fs.realpathSync(carrier.primaryCwd) === fs.realpathSync(opts.cwd) && !badCarrier;
      seen.submitsBefore = submits; seen.carrier = carrier; seen.file = d.path;
      fs.writeFileSync(env.RUSTRIGHT_REQUEST_ID_FILE, JSON.stringify({ schema: "rustright.request-ref.v1", requestId: "job-1", submittedAt: "x", workspace: fs.realpathSync(opts.cwd) }));
      return seen.ok ? { status: 0, stdout: "", stderr: "" } : { status: 2, stdout: "", stderr: "invalid snapshot request carrier" };
    };
    const receiptOf = async (id) => signedReceipt(fx, { id, carrier: seen.carrier, argv: args0, workspace: fs.realpathSync(checkout), tamper: tamper ?? "none" });
    const args0 = scan.args.slice(1);
    const result = await runScans(plan, { spawn, lookupSpawn: spawn, env: { PATH: process.env.PATH }, classes: ["A", "B"],
      signal: launchSignal,
      snapshotBroker: { controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: snap, expectedTree: T }, handle: "own-9", pins, checkout, outputRoots: [fx.wsRoot], platform: process.platform,
        ...(mark === null ? {} : { markBrokerSubmit: mark ?? ((h) => { assert.ok(isCallHandleOf(h, "own-9"), `a broker submit is recorded for a sub-handle <handle>#<id>, not ${JSON.stringify(h ?? null)}`); submits += 1; return { handle: "own-9", broker: { submitted: true, handles: [h] } }; }) }), receiptOf } }).catch((e) => ({ thrown: e, scans: [] }));
    return { row: result.scans.find((r) => r.id === "check:core"), result, seen, submits };
  };
  let launchSignal;
  const good = await run();
  assert.equal(good.seen.ok, true, `the client accepted the carrier (${JSON.stringify(good.row)})`);
  assert.equal(good.submits, 1); assert.equal(good.seen.submitsBefore, 1, "the ownership recorded the broker submit before the request ran");
  assert.equal(good.row.status, "pass", JSON.stringify(good.row));
  assert.deepEqual(good.row.snapshotEvidence, { requestId: "job-1", outputDir: fx.outputDir, treeAtStart: T, treeAtExit: T });
  assert.equal(fs.existsSync(good.seen.file), false, "the carrier file is removed after the request");
  // The broker built from a snapshot that changed, or for another handle: the row fails even though cargo exited 0.
  const moved = await run({ tamper: "treeAtExit" });
  assert.equal(moved.row.status, "fail"); assert.equal(moved.row.code, "SNAPSHOT_EVIDENCE"); assert.match(moved.row.detail, /throughout/);
  const other = await run({ tamper: "otherHandle" });
  assert.equal(other.row.code, "SNAPSHOT_EVIDENCE");
  const legacy = await run({ tamper: "noSnapshotEvidence" });
  assert.equal(legacy.row.code, "SNAPSHOT_EVIDENCE"); assert.match(legacy.row.detail, /no snapshotEvidence/);
  // cargo exited 0, but the broker authentically signed a failure, or signed no result: the row fails.
  const failed = await run({ tamper: "failed" });
  assert.equal(failed.row.status, "fail"); assert.equal(failed.row.code, "SNAPSHOT_EVIDENCE"); assert.match(failed.row.detail, /signed FAILED/);
  const silent = await run({ tamper: "noResult" });
  assert.equal(silent.row.code, "SNAPSHOT_EVIDENCE"); assert.match(silent.row.detail, /no result/);
  // The submit is recorded before launch, or nothing launches: no hook, a hook that throws, a mark that did not land.
  for (const [mark, why] of [
    [null, /no build-ownership submit hook/],
    [() => { throw new Error("build ownership is no longer held"); }, /no longer held/],
    [() => ({ handle: "own-9", broker: {} }), /did not record the broker submit/],
    [() => ({ handle: "someone-else", broker: { submitted: true } }), /did not record the broker submit/],
  ]) {
    const refused = await run({ mark });
    assert.equal(refused.row.status, "fail"); assert.equal(refused.row.code, "SNAPSHOT_REQUEST"); assert.match(refused.row.detail, why);
    assert.equal(refused.seen.launched, false, "the cargo request was never launched");
    assert.deepEqual(fs.readdirSync(path.join(root, "control", "snapshot-requests")), [], "no carrier is left behind");
  }
  // Every exit after the carrier exists removes it and the request reference: a thrown spawn, a cancel, a timeout.
  const gone = (seen) => { assert.ok(seen.file && seen.refDir); assert.equal(fs.existsSync(seen.file), false, seen.file); assert.equal(fs.existsSync(seen.refDir), false, seen.refDir); };
  const thrown = await run({ launch: () => { throw new Error("spawn exploded"); } });
  assert.equal(thrown.row.status, "fail"); assert.equal(thrown.row.code, "SCAN_ERROR"); assert.match(thrown.row.detail, /spawn exploded/); gone(thrown.seen);
  const ac = new AbortController(); launchSignal = ac.signal;
  const cancelled = await run({ launch: () => { ac.abort(); return { status: null, error: Object.assign(new Error("aborted"), { code: "ABORT_ERR" }) }; } });
  assert.equal(cancelled.row.code, "CANCELLED"); gone(cancelled.seen);
  launchSignal = undefined;
  const late = await run({ launch: () => ({ status: null, error: Object.assign(new Error("timed out"), { code: "ETIMEDOUT" }) }) });
  assert.equal(late.row.code, "TIMEOUT"); gone(late.seen);
  assert.deepEqual(fs.readdirSync(path.join(root, "control", "snapshot-requests")), []);
});

test("[7.22] a sibling directory whose name merely begins with two dots is not inside the snapshot or the checkout", (t) => {
  const c = receiptCase(t);
  // `<checkout>/..x/target` is inside the checkout; `<root>/..snap-sibling` is not inside the snapshot (a path-prefix check would say both).
  const out = (dir) => signedReceiptWithOutput(c, dir);
  assert.throws(() => out(path.join(c.fx.checkout, "..x", "target")).check(), /inside the checkout/);
  assert.throws(() => out(path.join(c.fx.snapshot, "..x", "target")).check(), /inside the snapshot/);
  assert.equal(isGenerationTarget(path.join(c.fx.wsRoot, "..0123456789a", "g", "002", "target"), [c.fx.wsRoot], process.platform), false);
});
function signedReceiptWithOutput(c, outputDir) {
  const receipt = signedReceipt(c.fx, { id: JOB, carrier: c.made.carrier, argv: c.argv, workspace: c.fx.checkout, tamper: "none", mutate: null });
  // Re-sign with the chosen output directory (signed and copied consistently).
  const evidence = { ...receipt.provenance.payload.snapshotEvidence, outputDir };
  const rebuilt = signedReceipt({ ...c.fx, outputDir }, { id: JOB, carrier: c.made.carrier, argv: c.argv, workspace: c.fx.checkout });
  void evidence;
  return { check: () => verifySnapshotReceipt(rebuilt, { pins: c.fx.pins, jobId: JOB, carrier: c.made.carrier, argv: c.argv, workspace: c.fx.checkout, checkout: c.fx.checkout, outputRoots: [c.fx.wsRoot], platform: process.platform }) };
}

test("[B9] the filesystem-free evidence identity: commit, handle, request id, snapshot path and the tree at start and exit, in the gate's order", () => {
  const commit = { sha: "a".repeat(40), tree: "b".repeat(40) };
  const evidence = { requestId: "job-1", commit, snapshot: { path: "/s/x", treeAtStart: commit.tree, treeAtExit: commit.tree }, ownership: { handle: "h" } };
  const ok = (over = {}, expect = {}) => verifySnapshotEvidenceIdentity({ ...evidence, ...over }, { commit, jobId: "job-1", ...expect });
  assert.deepEqual(ok(), evidence.snapshot, "handle and snapshot path are checked only when the caller knows them");
  assert.deepEqual(ok({}, { handle: "h", snapshotPath: "/s/x" }), evidence.snapshot);
  const refuses = (over, expect, pattern) => assert.throws(() => ok(over, expect), (e) => e instanceof SnapshotRequestError && e.code === "evidence" && pattern.test(e.message));
  refuses({ commit: { ...commit, sha: "c".repeat(40) } }, {}, /commit is not the one/);
  refuses({ commit: { ...commit, tree: "c".repeat(40) } }, {}, /commit is not the one/);
  refuses({}, { handle: "other" }, /ownership handle/);
  refuses({ requestId: "job-2" }, {}, /request id is not this job/);
  refuses({ requestId: undefined }, {}, /request id is not this job/);
  refuses({}, { snapshotPath: "/s/y" }, /signed snapshot \/s\/x is not \/s\/y/);
  refuses({ snapshot: { ...evidence.snapshot, treeAtStart: "c".repeat(40) } }, {}, /throughout/);
  refuses({ snapshot: { ...evidence.snapshot, treeAtExit: "c".repeat(40) } }, {}, /throughout/);
  refuses({ snapshot: undefined }, {}, /throughout/);
  assert.throws(() => verifySnapshotEvidenceIdentity(undefined, { commit, jobId: "job-1" }), /no snapshotEvidence/);
  assert.equal(ok({}, { snapshotPath: "/s/X", samePath: (a, b) => a.toLowerCase() === b.toLowerCase() }) === evidence.snapshot, true, "the gate compares real paths through its own comparator");
});

// ---- GB-02: the evidence a receipt owes is the broker's own compile classification (RustRight's cargo-shapes vector, `compile` column) ----
test("[GB-02] for every compile form of the vector, removing both copies of freshness, attribution or toolchain is refused; a read-only form legitimately carries none", (t) => {
  const rows = loadVector("cargo-shapes.fixtures.json").rows.filter((r) => !r.refused);
  assert.ok(rows.some((r) => r.compile) && rows.some((r) => !r.compile));
  for (const row of rows) {
    const argv = row.normalizedArgv;
    if (row.compile) {
      for (const tamper of ["noFreshness", "noAttribution", "noToolchain", "noStageCensus"]) {
        assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /is missing from the signed stage evidence or the receipt/.test(e.message), `${row.name} / ${tamper}`);
      }
      assert.equal(receiptCase(t, { argv }).check().payload.result.status, "SUCCEEDED", `${row.name}: the full evidence verifies`);
    } else {
      assert.equal(receiptCase(t, { argv, tamper: "noStageCensus" }).check().payload.result.status, "SUCCEEDED", `${row.name}: absence is valid for a form that compiles nothing`);
      assert.throws(() => receiptCase(t, { argv, tamper: "attributionReceiptOnly" }).check(), (e) => e.code === "evidence", `${row.name}: one-sided evidence still fails`);
    }
  }
});

// ---- GB-10 / GB-11: the gate verifies a compile receipt with the strict verifier ----
test("[GB-10] a receipt the strict verifier refuses never verifies: no or one-sided or malformed BR19 evidence, signed-only and receipt-only result evidence", (t) => {
  assert.equal(receiptCase(t).check().payload.result.status, "SUCCEEDED");
  for (const tamper of ["noBr19Evidence", "br19ReceiptOnly", "br19SignedOnly", "br19ProfileDiffers", "br19ResourcesNoPriority", "br19InventedZero", "br19DigestNotHex"]) {
    assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence", tamper);
  }
  // A timing phase missing on BOTH sides: equal, but not the contract's six phases.
  assert.throws(() => receiptCase(t, { mutate: (p) => { delete p.resultEvidence.timingsMs.queue; } }).check(), (e) => e.code === "evidence");
});

// ---- GB-66: the toolchain identity the broker could not build is a typed unavailable state ----
test("[GB-66] an UNKNOWN toolchain is a typed unavailable identity, never evidence; an empty component or a malformed UNKNOWN is still refused; a compile refused for it names the broker's code", (t) => {
  assert.equal(receiptCase(t).check().toolchain.startsWith("stable|cargo:"), true, "a full identity verifies");
  assert.throws(() => receiptCase(t, { tamper: "toolchainUnknown" }).check(), (e) => e.code === "toolchain-unavailable" && /RK_TOOLCHAIN_IDENTITY_UNAVAILABLE: UNKNOWN, cargo is not registered/.test(e.message));
  assert.throws(() => receiptCase(t, { tamper: "toolchainUnavailableFailed" }).check(), (e) => e.code === "toolchain-unavailable" && /did not run this compile.*rustc changed since registration/.test(e.message));
  for (const tamper of ["toolchainUnknownMalformed", "toolchainEmptyName", "toolchainNoHashes", "toolchainObject"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence", tamper);
  // A read-only form carries no toolchain at all and is not made to.
  assert.equal(receiptCase(t, { argv: ["metadata", "--no-deps"], tamper: "noStageCensus" }).check().toolchain, null);
});

// ---- GB-82: the fixture broker's receipts are the production forms ----
test("[GB-82] the fixture signs compact attribution with a detail file the production verifier accepts, an unavailable census fails the compile, and a nonzero exit is a signed FAILED", (t) => {
  const c = receiptCase(t);
  const { attribution } = c.check();
  assert.equal(attribution.units, 1); assert.equal(Object.hasOwn(attribution, "rows"), false, "no manufactured full row set in the signed receipt");
  const bytes = fs.readFileSync(attribution.detail.path);
  assert.deepEqual(verifyAttributionDetail(c.receipt, bytes).failedUnits, 0);
  // Unavailable census: typed unavailable evidence that fails a compile; without a reason, or not an object, it is refused as evidence.
  assert.throws(() => receiptCase(t, { tamper: "censusUnavailable" }).check(), (e) => e.code === "result" && /the broker signed FAILED/.test(e.message));
  for (const tamper of ["censusUnavailableNoReason", "freshnessNotObject"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence", tamper);
  // A read-only form that carries no census is unaffected.
  assert.equal(receiptCase(t, { argv: ["metadata", "--no-deps"], tamper: "noStageCensus" }).check().payload.result.status, "SUCCEEDED");
  // A depth-2 nested child keeps its lineage (its parent is the earlier child, not the root).
  const fx = makeFakeBroker(t);
  const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: [], handle: fx.handle, primaryCwd: fx.checkout, argv: ["run", "x"], nonce: "a".repeat(64) });
  const child = (requestId, parentRequestId) => ({ requestId, parentRequestId, shim: "cargo", argv: ["build"], cwd: fx.snapshot, exitCode: 0, status: "SUCCEEDED", outputDir: path.join(fx.outputDir, "nested", requestId) });
  const receipt = signedReceipt(fx, { id: JOB, carrier: made.carrier, argv: ["run", "x"], workspace: fx.checkout, adapter: "rightkit-pnpm", children: [child("c1", "root"), child("c2", "c1")] });
  assert.deepEqual(receipt.nestedChildren.map((n) => [n.requestId, n.parentRequestId]), [["c1", JOB], ["c2", "c1"]]);
  const verified = verifySnapshotReceipt(receipt, { pins: fx.pins, jobId: JOB, carrier: made.carrier, argv: ["run", "x"], workspace: fx.checkout, adapter: "rightkit-pnpm", checkout: fx.checkout, outputRoots: [fx.wsRoot], platform: process.platform });
  assert.equal(verified.nestedChildren.length, 2);
});
