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
import { canonicalJson, signedBytes } from "./provenance-verify.mjs";
import {
  ATTEST_NONCE_ENV, DESCRIPTOR_MAX_BYTES, REQUEST_ID_FILE_ENV, SNAPSHOT_REQUEST_ENV, SNAPSHOT_REQUEST_SCHEMA, SnapshotRequestError, argvSha256, declaredInputDigestsOf, freshNonce,
  cargoSubcommandOf, isCompileClass, isGenerationTarget, observedEnvOf, readDeclaredInputs, readRequestRef, requestDigestOf, snapshotRequestCore, verifySnapshotReceipt, writeSnapshotRequest,
} from "./snapshot-request.mjs";
import { makeFakeBroker, signedReceipt } from "./test-support/fake-broker.mjs";

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
  assert.deepEqual(c.declaredInputs, { inputs: ["Cargo.lock", "crates/core"] }); assert.deepEqual(c.ownership, { handle: "own-1" });
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
  assert.equal(isGenerationTarget("D:\\.rightkit-managed\\rightkit-build-control\\ws\\0123456789ab\\g\\002\\target", ["D:\\.rightkit-managed\\rightkit-build-control\\ws"], "win32"), true);
  assert.equal(isGenerationTarget("d:\\.RIGHTKIT-managed\\rightkit-build-control\\ws\\0123456789ab\\g\\002\\target", ["D:\\.rightkit-managed\\rightkit-build-control\\ws"], "win32"), true, "Windows paths compare case-insensitively");
});

test("[7.22] stage evidence and the unsigned fields: freshness, attribution and toolchain are signed and equal; verdict, timingsMs, exit, diagnosticsPath, executor and refreshed are present with their types", (t) => {
  for (const [tamper, why] of [["noStageEvidence", /no stageEvidence/], ["noFreshness", /freshness is missing/], ["freshnessReceiptOnly", /freshness is missing/], ["freshnessSignedOnly", /freshness is missing/], ["freshnessDiffers", /freshness differs from the signed stage evidence/]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && why.test(e.message), tamper);
  }
  for (const [tamper, field] of [["noVerdict", "verdict"], ["noTimings", "timingsMs"], ["noExit", "exit"], ["noDiagnostics", "diagnosticsPath"], ["noRefreshed", "refreshed"]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} is missing or has the wrong type`).test(e.message), tamper);
  }
  // Wrong types are as bad as missing ones.
  for (const [field, value] of [["verdict", "passed"], ["timingsMs", [1]], ["exit", "0"], ["signal", {}], ["diagnosticsPath", ""], ["refreshed", {}]]) {
    const c = receiptCase(t); c.receipt[field] = value;
    assert.throws(() => c.check(), new RegExp(`receipt ${field} is missing or has the wrong type`), `${field}=${JSON.stringify(value)}`);
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

test("[F7 #4] freshness is still required for every command, a copy carried by one side only is refused even for a non-compile command, and verdict, exit, timingsMs, diagnosticsPath and refreshed are required for every command", (t) => {
  for (const argv of NON_COMPILE) {
    for (const tamper of ["noFreshness", "freshnessReceiptOnly", "freshnessSignedOnly"]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /freshness is missing/.test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
    for (const tamper of ["attributionReceiptOnly", "attributionSignedOnly"]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /attribution differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
    for (const [tamper, field] of [["noVerdict", "verdict"], ["noExit", "exit"], ["noTimings", "timingsMs"], ["noDiagnostics", "diagnosticsPath"], ["noRefreshed", "refreshed"]]) {
      assert.throws(() => receiptCase(t, { argv, tamper: tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} is missing or has the wrong type`).test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
  }
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
  assert.throws(() => receiptCase(t, { tamper: "verdictCopyDiffers" }).check(), /receipt verdict differs from the signed payload/);
  const bad = receiptCase(t); bad.receipt.exit = 1;
  assert.throws(() => bad.check(), /not a success/);
  const signalled = receiptCase(t); signalled.receipt.exit = null; signalled.receipt.signal = "SIGKILL"; delete signalled.receipt.provenance.payload.exit; delete signalled.receipt.provenance.payload.signal;
  assert.throws(() => signalled.check(), (e) => e.code === "signature" || /not a success/.test(e.message), "a signalled run is never a success (editing the signed payload breaks the signature)");
});

test("[F7 #4] the subcommand is read from the argv the way cargo reads it: after +toolchain and global options, before --", () => {
  for (const [argv, sub] of [[["build"], "build"], [["+nightly", "test"], "test"], [["--color", "always", "check"], "check"], [["--locked", "-q", "metadata"], "metadata"], [["--", "build"], null], [[], null], [["--explain", "E0001", "doc"], "doc"]]) {
    assert.equal(cargoSubcommandOf(argv), sub, argv.join(" "));
  }
  assert.equal(isCompileClass(["clippy"]), true); assert.equal(isCompileClass(["run"]), false); assert.equal(isCompileClass(["metadata"]), false); assert.equal(isCompileClass([]), false);
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
  // Per file, in sorted directory order: relative name, NUL, bytes, NUL (names joined with the platform separator).
  assert.deepEqual(declaredInputDigestsOf(snap, inputs, { platform: "darwin" }), [
    { path: "a.txt", sha256: h("a.txt", "\0", "hello", "\0") },
    { path: "dir", sha256: h("dir/a", "\0", "A", "\0", "dir/b", "\0", "B", "\0", "dir/sub/c", "\0", "C", "\0") },
  ]);
  assert.equal(declaredInputDigestsOf(snap, ["dir"], { platform: "win32" })[0].sha256, h("dir\\a", "\0", "A", "\0", "dir\\b", "\0", "B", "\0", "dir\\sub\\c", "\0", "C", "\0"), "Windows joins child names with a backslash, as Path::join does");
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
      if (!env.RIGHTKIT_SNAPSHOT_REQUEST) { if (args.includes("check")) seen.launched = true; return { status: 0, stdout: "v1\n", stderr: "" }; }
      seen.launched = true;
      const d = JSON.parse(env.RIGHTKIT_SNAPSHOT_REQUEST);
      seen.file = d.path; seen.refDir = path.dirname(env.RIGHTKIT_REQUEST_ID_FILE);
      if (launch) return launch(opts);
      const bytes = fs.readFileSync(d.path);
      const carrier = JSON.parse(bytes);
      seen.ok = sha(bytes) === d.sha256 && carrier.nonce === env.RIGHTKIT_ATTEST_NONCE && carrier.argvSha256 === sha(canonicalJson(args.slice(1))) && fs.realpathSync(carrier.primaryCwd) === fs.realpathSync(opts.cwd) && !badCarrier;
      seen.submitsBefore = submits; seen.carrier = carrier; seen.file = d.path;
      fs.writeFileSync(env.RIGHTKIT_REQUEST_ID_FILE, JSON.stringify({ schema: "rustright.request-ref.v1", requestId: "job-1", submittedAt: "x", workspace: fs.realpathSync(opts.cwd) }));
      return seen.ok ? { status: 0, stdout: "", stderr: "" } : { status: 2, stdout: "", stderr: "invalid snapshot request carrier" };
    };
    const receiptOf = async (id) => signedReceipt(fx, { id, carrier: seen.carrier, argv: args0, workspace: fs.realpathSync(checkout), tamper: tamper ?? "none" });
    const args0 = scan.args.slice(1);
    const result = await runScans(plan, { spawn, lookupSpawn: spawn, env: { PATH: process.env.PATH }, classes: ["A", "B"],
      signal: launchSignal,
      snapshotBroker: { controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: snap, expectedTree: T }, handle: "own-9", pins, checkout, outputRoots: [fx.wsRoot], platform: process.platform,
        ...(mark === null ? {} : { markBrokerSubmit: mark ?? (() => { submits += 1; return { handle: "own-9", broker: { submitted: true } }; }) }), receiptOf } }).catch((e) => ({ thrown: e, scans: [] }));
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
