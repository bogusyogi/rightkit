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
  ATTEST_NONCE_ENV, DESCRIPTOR_MAX_BYTES, REQUEST_ID_FILE_ENV, SNAPSHOT_REQUEST_ENV, SNAPSHOT_REQUEST_SCHEMA, SnapshotRequestError, argvSha256, freshNonce,
  observedEnvOf, readRequestRef, requestDigestOf, snapshotRequestCore, verifySnapshotReceipt, writeSnapshotRequest,
} from "./snapshot-request.mjs";

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

test("observedEnv: only the managed Rust variables the client records (case-insensitive on Windows)", () => {
  assert.deepEqual(observedEnvOf({ PATH: "/x", CARGO_TARGET_DIR: "/t", RUSTFLAGS: "-D warnings", HOME: "/h" }, "darwin"), { CARGO_TARGET_DIR: "/t", RUSTFLAGS: "-D warnings" });
  assert.deepEqual(observedEnvOf({ Cargo_Home: "C:\\c", Path: "x" }, "win32"), { CARGO_HOME: "C:\\c" });
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

test("receipt: a signed snapshot receipt verifies; tampered copies, payload, trees, handle or a missing evidence are refused", (t) => {
  const { made, argv, cwd, snapshot } = request(t);
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const sec1 = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  const pins = { publicKey: sec1, keyId: sha(Buffer.from(sec1, "hex")), host: "host-1", brokerDigest: "bd" };
  const jobId = "11111111-1111-4111-8111-111111111111";
  const evidence = {
    requestId: jobId, brokerVersion: "rustright-x", executor: "host-1", commit: made.carrier.commit,
    snapshot: { path: snapshot, treeAtStart: T, treeAtExit: T }, declaredInputDigests: [], ownership: { handle: "own-1" }, outputDir: "/checkout/target",
  };
  const make = (mutate = (p) => p) => {
    const payload = mutate({ host: "host-1", broker: { brokerDigest: "bd" }, keyId: pins.keyId, jobId, nonce: made.carrier.nonce,
      requestDigest: requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, carrier: made.carrier })), snapshotEvidence: structuredClone(evidence) });
    const value = crypto.sign("sha256", signedBytes(payload), { key: privateKey, dsaEncoding: "ieee-p1363" }).toString("hex");
    return { id: jobId, ...structuredClone(payload.snapshotEvidence ?? {}), provenance: { schema: "rustright.provenance.v1", payload, signature: { alg: "ES256", keyId: pins.keyId, value } } };
  };
  const check = (receipt, over = {}) => verifySnapshotReceipt(receipt, { pins, jobId, carrier: made.carrier, argv, workspace: cwd, ...over });
  assert.equal(check(make()).evidence.outputDir, "/checkout/target");
  // The receipt's copies must equal the signed evidence (RustRight's own rule).
  for (const field of ["snapshot", "outputDir", "commit", "declaredInputDigests", "ownership"]) {
    const r = make(); r[field] = "tampered";
    assert.throws(() => check(r), (e) => e.code === "evidence", field);
  }
  // Editing the signed payload breaks the signature.
  const r = make(); r.provenance.payload.snapshotEvidence.snapshot.treeAtExit = "f".repeat(40);
  assert.throws(() => check(r), (e) => e.code === "signature");
  // Signed, but the snapshot changed during the build, or another gate's handle, or another commit: refused.
  assert.throws(() => check(make((p) => { p.snapshotEvidence.snapshot.treeAtExit = "f".repeat(40); return p; })), /not .* throughout/);
  assert.throws(() => check(make((p) => { p.snapshotEvidence.ownership = { handle: "other" }; return p; })), /handle is not this gate's/);
  assert.throws(() => check(make((p) => { p.snapshotEvidence.commit = { sha: "d".repeat(40), tree: T }; return p; })), /signed commit is not/);
  assert.throws(() => check(make((p) => { delete p.snapshotEvidence; return p; })), /carries no snapshotEvidence/);
  // A different argv, workspace or nonce than this request: the request digest no longer matches.
  assert.throws(() => check(make(), { argv: ["build"] }), (e) => e.code === "request");
  assert.throws(() => check(make(), { workspace: "/elsewhere" }), (e) => e.code === "request");
  assert.throws(() => check(make(), { pins: { ...pins, host: "other" } }), (e) => e.code === "host");
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
  const root = tmp(t);
  const checkout = path.join(root, "checkout"); const snap = path.join(root, "data", "snapshots", "0123456789abcdef");
  fs.mkdirSync(checkout, { recursive: true }); fs.mkdirSync(snap, { recursive: true });
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const sec1 = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  const pins = { publicKey: sec1, keyId: sha(Buffer.from(sec1, "hex")), host: "host-1", brokerDigest: "bd" };
  const scan = { id: "check:core", class: "B", tool: "rustright", args: ["cargo", "check", "-p", "core", "--locked"], root: "cargoLive", env: {}, command: "rustright cargo check -p core --locked" };
  // Class B runs only after class A ran in the same call: one passing class A row.
  const classA = { id: "a:noop", class: "A", tool: "node", args: ["-e", "0"], root: "live", env: {}, command: "node -e 0" };
  const plan = { roots: { contentRoot: snap, liveRoot: snap, cargoLiveRoot: checkout }, scans: [classA, scan], notes: [], deferred: [] };
  // A fake client that applies RustRight's reader rules, records the request reference, and keeps what it saw.
  const run = async ({ tamper = null, badCarrier = false } = {}) => {
    const seen = { submitsBefore: 0 }; let submits = 0;
    const spawn = (command, args, opts = {}) => {
      const env = opts.env ?? {};
      // Tool probes and the class A row: anything that is not the carried request.
      if (!env.RIGHTKIT_SNAPSHOT_REQUEST) return { status: 0, stdout: "v1\n", stderr: "" };
      const d = JSON.parse(env.RIGHTKIT_SNAPSHOT_REQUEST);
      const bytes = fs.readFileSync(d.path);
      const carrier = JSON.parse(bytes);
      seen.ok = sha(bytes) === d.sha256 && carrier.nonce === env.RIGHTKIT_ATTEST_NONCE && carrier.argvSha256 === sha(canonicalJson(args.slice(1))) && fs.realpathSync(carrier.primaryCwd) === fs.realpathSync(opts.cwd) && !badCarrier;
      seen.submitsBefore = submits; seen.carrier = carrier; seen.file = d.path;
      fs.writeFileSync(env.RIGHTKIT_REQUEST_ID_FILE, JSON.stringify({ schema: "rustright.request-ref.v1", requestId: "job-1", submittedAt: "x", workspace: fs.realpathSync(opts.cwd) }));
      return seen.ok ? { status: 0, stdout: "", stderr: "" } : { status: 2, stdout: "", stderr: "invalid snapshot request carrier" };
    };
    const receiptOf = async (id) => {
      const c = seen.carrier;
      const evidence = { requestId: id, brokerVersion: "v", executor: "host-1", commit: c.commit, snapshot: { path: c.snapshot.path, treeAtStart: T, treeAtExit: T }, declaredInputDigests: [], ownership: c.ownership, outputDir: path.join(checkout, "target") };
      const payload = { host: "host-1", broker: { brokerDigest: "bd" }, keyId: pins.keyId, jobId: id, nonce: c.nonce,
        requestDigest: requestDigestOf(snapshotRequestCore({ argv: args0, workspace: fs.realpathSync(path.join(checkout)), carrier: c })), snapshotEvidence: evidence };
      tamper?.(payload);
      const value = crypto.sign("sha256", signedBytes(payload), { key: privateKey, dsaEncoding: "ieee-p1363" }).toString("hex");
      return { id, ...structuredClone(payload.snapshotEvidence), provenance: { schema: "rustright.provenance.v1", payload, signature: { alg: "ES256", keyId: pins.keyId, value } } };
    };
    const args0 = scan.args.slice(1);
    const result = await runScans(plan, { spawn, lookupSpawn: spawn, env: { PATH: process.env.PATH }, classes: ["A", "B"],
      snapshotBroker: { controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: snap, expectedTree: T }, handle: "own-9", pins, markBrokerSubmit: () => { submits += 1; }, receiptOf } });
    return { row: result.scans.find((r) => r.id === "check:core"), seen, submits };
  };
  const good = await run();
  assert.equal(good.seen.ok, true, `the client accepted the carrier (${JSON.stringify(good.row)})`);
  assert.equal(good.submits, 1); assert.equal(good.seen.submitsBefore, 1, "the ownership recorded the broker submit before the request ran");
  assert.equal(good.row.status, "pass", JSON.stringify(good.row));
  assert.deepEqual(good.row.snapshotEvidence, { requestId: "job-1", outputDir: path.join(checkout, "target"), treeAtStart: T, treeAtExit: T });
  assert.equal(fs.existsSync(good.seen.file), false, "the carrier file is removed after the request");
  // The broker built from a snapshot that changed, or for another handle: the row fails even though cargo exited 0.
  const moved = await run({ tamper: (p) => { p.snapshotEvidence.snapshot.treeAtExit = "f".repeat(40); } });
  assert.equal(moved.row.status, "fail"); assert.equal(moved.row.code, "SNAPSHOT_EVIDENCE"); assert.match(moved.row.detail, /throughout/);
  const other = await run({ tamper: (p) => { p.snapshotEvidence.ownership = { handle: "someone-else" }; } });
  assert.equal(other.row.code, "SNAPSHOT_EVIDENCE");
  const legacy = await run({ tamper: (p) => { delete p.snapshotEvidence; } });
  assert.equal(legacy.row.code, "SNAPSHOT_EVIDENCE"); assert.match(legacy.row.detail, /no snapshotEvidence/);
});
