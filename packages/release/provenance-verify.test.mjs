import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ProvenanceError, brokerRequestIds, canonicalJson, carriedCallNonce, qualificationNonce, verifyProvenance } from "./provenance-verify.mjs";
import { makeSigner } from "./test-support/provenance-signer.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const JOB = "11111111-2222-4333-8444-555555555555";
const NONCE = "ab".repeat(32);
const refusal = (fn, code) => assert.throws(fn, (e) => e instanceof ProvenanceError && e.code === code, code);

test("canonical JSON matches the Rust writer: byte-order keys, no whitespace, minimal escapes", () => {
  assert.equal(canonicalJson({ b: 1, a: [true, null, "x"], "é": 2, "z": { y: 0, x: -3 } }), '{"a":[true,null,"x"],"b":1,"z":{"x":-3,"y":0},"é":2}');
  assert.equal(canonicalJson("quote\" back\\ nl\n tab\t nul\u0000 del\u007f unit\u001f é 😀 / <"), '"quote\\" back\\\\ nl\\u000a tab\\u0009 nul\\u0000 del\u007f unit\\u001f é 😀 / <"');
  // Above U+FFFF the UTF-8 byte order and the UTF-16 order differ: U+FF5E (3 bytes EF BD 9E) sorts before U+1F600 (F0 ...) in bytes.
  assert.equal(canonicalJson({ "😀": 1, "～": 2 }), '{"～":2,"😀":1}');
  assert.throws(() => canonicalJson({ n: 1.5 }), /safe integers/);
  assert.throws(() => canonicalJson({ n: 2 ** 60 }), /safe integers/);
  assert.throws(() => canonicalJson({ u: undefined }), /cannot carry/);
});

test("a receipt signed by the pinned key verifies and returns the signed payload", () => {
  const s = makeSigner();
  const receipt = s.receipt({ jobId: JOB, nonce: NONCE });
  const payload = verifyProvenance(receipt, s.pins, { jobId: JOB, nonce: NONCE });
  assert.equal(payload.jobId, JOB);
  assert.equal(verifyProvenance(receipt, s.pins, { jobId: JOB, nonce: NONCE, requestDigest: "7".repeat(64) }).nonce, NONCE);
});

test("every verify_receipt check fails closed: tampered payload, key, host, runtime, job, nonce, request digest, signature, envelope and pins", () => {
  const s = makeSigner();
  const other = makeSigner();
  const good = s.receipt({ jobId: JOB, nonce: NONCE });
  const expected = { jobId: JOB, nonce: NONCE };
  const clone = () => structuredClone(good);
  const edit = (fn) => { const r = clone(); fn(r); return r; };
  refusal(() => verifyProvenance(edit((r) => { r.provenance.payload.result.exitCode = 1; }), s.pins, expected), "signature");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.payload.request.workspace = "/elsewhere"; }), s.pins, expected), "signature");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.payload.keyId = "0".repeat(64); }), s.pins, expected), "key");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.signature.keyId = "0".repeat(64); }), s.pins, expected), "key");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.payload.host = "evil"; }), s.pins, expected), "signature");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.payload.nonce = "cd".repeat(32); }), s.pins, { jobId: JOB, nonce: "cd".repeat(32) }), "signature");
  // The signed values are right but this invocation expected others.
  refusal(() => verifyProvenance(good, { ...s.pins, host: "another-host" }, expected), "host");
  refusal(() => verifyProvenance(good, { ...s.pins, brokerDigest: "1".repeat(64) }, expected), "runtime");
  refusal(() => verifyProvenance(good, s.pins, { jobId: "other-job", nonce: NONCE }), "job");
  refusal(() => verifyProvenance(edit((r) => { r.id = "other-job"; }), s.pins, expected), "job");
  refusal(() => verifyProvenance(good, s.pins, { jobId: JOB, nonce: "cd".repeat(32) }), "nonce");
  refusal(() => verifyProvenance(good, s.pins, { ...expected, requestDigest: "6".repeat(64) }), "request");
  // A receipt signed by another key, or checked against another key's pin.
  refusal(() => verifyProvenance(other.receipt({ jobId: JOB, nonce: NONCE }), s.pins, expected), "key");
  refusal(() => verifyProvenance(good, other.pins, expected), "key");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.signature.value = r.provenance.signature.value.replace(/.$/, (c) => (c === "0" ? "1" : "0")); }), s.pins, expected), "signature");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.signature.value = "ab"; }), s.pins, expected), "signature");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.signature.value = "zz".repeat(64); }), s.pins, expected), "signature");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.signature.alg = "ES384"; }), s.pins, expected), "alg");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.schema = "rustright.provenance.v2"; }), s.pins, expected), "schema");
  refusal(() => verifyProvenance(edit((r) => { delete r.provenance; }), s.pins, expected), "legacy");
  refusal(() => verifyProvenance(edit((r) => { r.provenance.payload = "x"; }), s.pins, expected), "malformed");
  refusal(() => verifyProvenance(edit((r) => { delete r.provenance.signature; }), s.pins, expected), "malformed");
  // Pins must be self-consistent: the key id is the sha256 of the key bytes, and the key is an uncompressed P-256 point.
  refusal(() => verifyProvenance(good, { ...s.pins, keyId: "0".repeat(64) }, expected), "pin");
  refusal(() => verifyProvenance(good, { ...s.pins, publicKey: "xyz" }, expected), "pin");
  refusal(() => verifyProvenance(good, undefined, expected), "pin");
});

test("a pinned point that is not on the curve or not uncompressed is refused", async () => {
  const { createHash } = await import("node:crypto");
  const s = makeSigner();
  const good = s.receipt({ jobId: JOB, nonce: NONCE });
  const pinFor = (hex) => ({ ...s.pins, publicKey: hex, keyId: createHash("sha256").update(Buffer.from(hex, "hex")).digest("hex") });
  const offCurve = `04${"11".repeat(32)}${"22".repeat(32)}`;
  const forged = (hex) => { const r = structuredClone(good); const k = pinFor(hex).keyId; r.provenance.payload.keyId = k; r.provenance.signature.keyId = k; return r; };
  refusal(() => verifyProvenance(forged(offCurve), pinFor(offCurve), { jobId: JOB, nonce: NONCE }), "pin");
  const compressed = `02${s.pins.publicKey.slice(2, 66)}`;
  refusal(() => verifyProvenance(forged(compressed), pinFor(compressed), { jobId: JOB, nonce: NONCE }), "pin");
});

test("the qualification nonce binds commit, tree, unit and stage, as 64 lowercase hex", () => {
  const base = { commit: "a".repeat(40), tree: "b".repeat(40), unit: "npm:@rightkit/demo", stage: "compile" };
  const n = qualificationNonce(base);
  assert.match(n, /^[0-9a-f]{64}$/);
  assert.equal(n, qualificationNonce({ ...base }));
  for (const change of [{ commit: "c".repeat(40) }, { tree: "c".repeat(40) }, { unit: "cargo:rightkit-demo" }, { stage: "package" }]) assert.notEqual(qualificationNonce({ ...base, ...change }), n);
});

test("broker request ids are read from the client's `rustright: request <id>` lines only", () => {
  const out = `x\nrustright: request ${JOB}\n  rustright: request 99999999-2222-4333-8444-555555555555\nrustright: request AAAAAAAA-2222-4333-8444-555555555555\nrustright: request 99999999-2222-4333-8444-555555555556 extra\n`;
  assert.deepEqual(brokerRequestIds(out), [JOB, "99999999-2222-4333-8444-555555555556"]);
  assert.deepEqual(brokerRequestIds(undefined), []);
});

// The golden vector: a REAL receipt of the Mac's installed broker (rustright status 36efaa30-..., 2026-10-27) against the
// enrolled darwin pins committed in ci/rk-pipeline.json. Its signature verifying here proves the JS canonical JSON and ES256 check
// reproduce the Rust bytes exactly. An optional second fixture, RIGHTKIT_PROVENANCE_FIXTURE (a `rustright status` JSON file),
// extends the cross-check to any other host's receipt (enrolled pins for its host must be in ci/rk-pipeline.json).
const pipeline = JSON.parse(fs.readFileSync(path.join(HERE, "..", "..", "ci", "rk-pipeline.json"), "utf8"));
const goldenOf = (receipt) => {
  const payload = receipt.provenance.payload;
  const pins = Object.values(pipeline.attestation ?? {}).find((p) => p.host === payload.host);
  assert.ok(pins, `no enrolled pins for ${payload.host}`);
  return verifyProvenance(receipt, pins, { jobId: receipt.id, nonce: payload.nonce });
};
test("golden vector: a real broker receipt verifies against the pins it was signed under (recorded beside it), and a one-byte change to it does not", () => {
  const real = JSON.parse(fs.readFileSync(path.join(HERE, "provenance-real-receipt.fixture.json"), "utf8"));
  // The fixture was signed by the Mac key before its 2026-10-29 rotation (BR15): its pins are recorded beside it, so a rotation of the live pin
  // in ci/rk-pipeline.json cannot break the vector. The live pin is not the subject of this vector.
  const recorded = JSON.parse(fs.readFileSync(path.join(HERE, "provenance-real-receipt.pins.json"), "utf8"));
  const pins = { host: recorded.host, keyId: recorded.keyId, publicKey: recorded.publicKey, brokerDigest: recorded.brokerDigest };
  assert.equal(real.provenance.payload.host, pins.host);
  assert.equal(real.provenance.payload.keyId, pins.keyId, "the recorded key is the one that signed it");
  assert.equal(crypto.createHash("sha256").update(Buffer.from(pins.publicKey, "hex")).digest("hex"), pins.keyId, "the key id is the digest of the recorded public key");
  const golden = (receipt) => verifyProvenance(receipt, pins, { jobId: receipt.id, nonce: receipt.provenance.payload.nonce });
  const payload = golden(real);
  assert.equal(payload.result.status, "SUCCEEDED");
  const tampered = structuredClone(real); tampered.provenance.payload.result.exitCode = 1;
  refusal(() => golden(tampered), "signature");
  const moved = structuredClone(real); moved.provenance.payload.request.workspace += "x";
  refusal(() => golden(moved), "signature");
  // The same receipt against another key (the rotated live pin) is refused as unpinned: a pin cannot be taken from the receipt.
  const live = Object.values(pipeline.attestation ?? {}).find((p) => p.host === pins.host);
  if (live && live.keyId !== pins.keyId) refusal(() => verifyProvenance(real, live, { jobId: real.id, nonce: real.provenance.payload.nonce }), "key");
});

test("optional golden vector from RIGHTKIT_PROVENANCE_FIXTURE", { skip: !process.env.RIGHTKIT_PROVENANCE_FIXTURE }, () => {
  const doc = JSON.parse(fs.readFileSync(process.env.RIGHTKIT_PROVENANCE_FIXTURE, "utf8"));
  goldenOf(doc.receipt ?? doc);
});

test("golden vector: the carried-call nonce is sha256 of the domain, stage nonce, cwd and the canonical argv, and moves with each", () => {
  const stageNonce = "ab".repeat(32);
  const argv = ["build", "--locked", "--release", "-p", "rightkit-x"];
  // Computed independently of this module: sha256("rightkit-carried-call-v1\n<stage>\ncrates/rightkit-x\n[\"build\",...]").
  assert.equal(carriedCallNonce({ stageNonce, cwd: "crates/rightkit-x", argv }), "a0a721efd4536c1eeb017a97bb1c4405f5f832aa42c7424fb03c3dfe56bbf850");
  assert.equal(carriedCallNonce({ stageNonce, cwd: ".", argv: ["metadata", "--format-version", "1", "--no-deps"] }), "db6ad74456c39fbaf2a7da480ca8584ca981ca757ac2e8c695982e01bb80e429");
  const base = carriedCallNonce({ stageNonce, cwd: "crates/rightkit-x", argv });
  assert.notEqual(carriedCallNonce({ stageNonce: "cd".repeat(32), cwd: "crates/rightkit-x", argv }), base, "another stage nonce");
  assert.notEqual(carriedCallNonce({ stageNonce, cwd: "crates/rightkit-y", argv }), base, "another directory");
  assert.notEqual(carriedCallNonce({ stageNonce, cwd: "crates/rightkit-x", argv: [...argv, "--all"] }), base, "another argv");
  assert.notEqual(carriedCallNonce({ stageNonce, cwd: "crates/rightkit-x", argv }), stageNonce, "never the stage nonce itself");
  assert.match(base, /^[0-9a-f]{64}$/);
});
