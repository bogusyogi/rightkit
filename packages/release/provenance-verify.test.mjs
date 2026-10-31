import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { CAUSE_EVIDENCE_SCHEMA, CAUSE_UNEXPLAINED, CAUSE_UNKNOWN, EXPLAINED_CAUSES, ProvenanceError, brokerRequestIds, canonicalJson, carriedCallNonce, qualificationNonce, requireExplainedCauses, verifyCauseEvidence, verifyProvenance } from "./provenance-verify.mjs";
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

// ---- RA-03: the cause evidence a broker signs inside a compile's freshness (rustright.cause-evidence.v1, cargo_evidence.rs) ----
// The freshness objects below are the compact forms `compact_freshness` signs (the keys of `Classification::compact`) for each state a real Cargo log and census can be
// in (freshness_vectors.rs derives them from real log text). The verifier is a pure function of the object: no broker, no signer.
const cause = (over = {}) => ({
  schema: CAUSE_EVIDENCE_SCHEMA, state: "complete", verdict: "explained", basis: "reasons", unavailable: null, gaps: [], historyMigrationNeeded: false,
  counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 0 }, unexplainedUnits: [], unexplainedUnitsTruncated: 0, uncoveredUnits: [], uncoveredUnitsTruncated: 0, ...over,
});
const rowCounts = (over = {}) => ({ rows: 1, rowsDropped: 0, units: 1, unexplained: 0, uncovered: 0, ...over });
const unread = (reason, over = {}) => cause({ state: "unavailable", basis: "none", verdict: "unknown", unavailable: reason, gaps: ["log-not-read"], ...over });
const census = (over = {}) => ({ available: true, unitsAtStart: 5, fresh: 5, notFresh: 0, parseErrors: 0, ...over });
const evidenceRefusal = (fn, why) => assert.throws(fn, (e) => e instanceof ProvenanceError && e.code === "evidence" && why.test(e.message), why.source);
const KEPT_UNIT = "app v0.1.0 (lib:app) [build]";
const FORMS = {
  sourceEdit: { freshness: census({ notFresh: 1, causeEvidence: cause({ counts: rowCounts() }), unexplainedCount: 0, causeCounts: { "source-or-dependency-changed": 1 } }), verdict: "explained", count: 0 },
  everyExplainedCause: { freshness: census({ notFresh: 8, causeEvidence: cause({ counts: { rows: 8, rowsDropped: 0, units: 8, unexplained: 0, uncovered: 0 } }), unexplainedCount: 0, causeCounts: Object.fromEntries(EXPLAINED_CAUSES.map((name) => [name, 1])) }), verdict: "explained", count: 0 },
  freshQuiet: { freshness: census({ invalidationReasonsUnavailable: "quiet-root", causeEvidence: cause({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }), unexplainedCount: 0 }), verdict: "explained", count: 0 },
  nothingCompiled: { freshness: { available: false, reason: "nothing-compiled", causeEvidence: cause({ state: "unavailable", basis: "nothing-compiled", unavailable: "not-read" }), unexplainedCount: 0 }, verdict: "explained", count: 0 },
  removedFingerprint: { freshness: census({ notFresh: 1, causeEvidence: cause({ verdict: "unexplained", counts: rowCounts({ unexplained: 1 }), unexplainedUnits: [KEPT_UNIT] }), unexplainedCount: 1, causeCounts: { "fingerprint-missing": 1 } }), verdict: "unexplained", count: 1 },
  quietRebuilt: { freshness: census({ notFresh: 1, invalidationReasonsUnavailable: "quiet-root", causeEvidence: unread("quiet-root"), unexplainedCount: null }), verdict: "unknown", count: null },
  callerLogged: { freshness: census({ notFresh: 2, causeEvidence: unread("caller-cargo-log"), unexplainedCount: null }), verdict: "unknown", count: null },
  testRun: { freshness: census({ notFresh: 3, causeEvidence: unread("root-executes-artifacts"), unexplainedCount: null }), verdict: "unknown", count: null },
  outputDropped: { freshness: census({ notFresh: 1, causeEvidence: cause({ state: "incomplete", verdict: "unknown", gaps: ["output-dropped"] }), unexplainedCount: null, causeCounts: {} }), verdict: "unknown", count: null },
};

test("[RA-03] the schema and the set of explaining causes are pinned to the broker's", () => {
  assert.equal(CAUSE_EVIDENCE_SCHEMA, "rustright.cause-evidence.v1");
  assert.deepEqual([...EXPLAINED_CAUSES], ["first-build", "toolchain-changed", "source-or-dependency-changed", "env-rustflags-profile-features-or-target-changed", "generation-change", "authorized-eviction", "new-unit", "new-namespace"]);
  assert.deepEqual([CAUSE_UNKNOWN, CAUSE_UNEXPLAINED], ["cause-unknown", "cause-unexplained"]);
});

test("[RA-03] verifyCauseEvidence reads every state a real log and census can be in, and the unexplained count is null exactly when the verdict is unknown", () => {
  for (const [name, { freshness, verdict, count }] of Object.entries(FORMS)) {
    const read = verifyCauseEvidence(structuredClone(freshness));
    assert.equal(read.verdict, verdict, name);
    assert.equal(read.unexplainedCount, count, name);
    assert.equal(read.schema, CAUSE_EVIDENCE_SCHEMA, name);
  }
  // The returned evidence is a copy: editing it never changes the signed object it was read from.
  const f = structuredClone(FORMS.removedFingerprint.freshness);
  verifyCauseEvidence(f).unexplainedUnits.push("forged"); assert.deepEqual(f.causeEvidence.unexplainedUnits, [KEPT_UNIT]);
});

test("[RA-03] requireExplainedCauses accepts only an explained verdict: genuinely fresh quiet work passes; a removed fingerprint and every unread or incomplete log are typed refusals", () => {
  for (const name of ["sourceEdit", "everyExplainedCause", "freshQuiet", "nothingCompiled"]) assert.equal(requireExplainedCauses(structuredClone(FORMS[name].freshness)).verdict, "explained", name);
  assert.throws(() => requireExplainedCauses(FORMS.removedFingerprint.freshness), (e) => e instanceof ProvenanceError && e.code === CAUSE_UNEXPLAINED && e.message.includes(KEPT_UNIT));
  for (const name of ["quietRebuilt", "callerLogged", "testRun", "outputDropped"]) {
    assert.throws(() => requireExplainedCauses(FORMS[name].freshness), (e) => e instanceof ProvenanceError && e.code === CAUSE_UNKNOWN, name);
  }
  assert.throws(() => requireExplainedCauses(FORMS.quietRebuilt.freshness), (e) => e.code === CAUSE_UNKNOWN && /quiet-root/.test(e.message) && /never read as no unexplained unit/.test(e.message));
  assert.throws(() => requireExplainedCauses(FORMS.outputDropped.freshness), (e) => e.code === CAUSE_UNKNOWN && /output-dropped/.test(e.message));
});

test("[RA-03] absent, null, foreign-schema and malformed cause evidence is refused as evidence, never read as the schema this gate knows", () => {
  const base = FORMS.sourceEdit.freshness;
  const without = (key) => { const f = structuredClone(base); delete f[key]; return f; };
  const withCause = (over) => ({ ...structuredClone(base), causeEvidence: over });
  for (const f of [undefined, null, "fresh", [], 3]) evidenceRefusal(() => verifyCauseEvidence(f), /no freshness object/);
  evidenceRefusal(() => verifyCauseEvidence(without("causeEvidence")), /carries no causeEvidence/);
  evidenceRefusal(() => verifyCauseEvidence(withCause(null)), /carries no causeEvidence/);
  for (const bad of ["x", 1, [], true]) evidenceRefusal(() => verifyCauseEvidence(withCause(bad)), /is not an object/);
  const noSchema = structuredClone(base.causeEvidence); delete noSchema.schema;
  for (const foreign of [noSchema, { ...base.causeEvidence, schema: "rustright.cause-evidence.v2" }, { ...base.causeEvidence, schema: "rustright.cause-evidence.v0" }, { ...base.causeEvidence, schema: null }, { ...base.causeEvidence, schema: "" }]) {
    evidenceRefusal(() => verifyCauseEvidence(withCause(foreign)), /does not read/);
  }
  for (const key of ["state", "verdict", "basis", "historyMigrationNeeded", "unavailable", "gaps", "counts", "unexplainedUnits", "unexplainedUnitsTruncated", "uncoveredUnits", "uncoveredUnitsTruncated"]) {
    const broken = structuredClone(base.causeEvidence); delete broken[key];
    assert.throws(() => verifyCauseEvidence(withCause(broken)), (e) => e instanceof ProvenanceError && e.code === "evidence", `missing ${key}`);
  }
  for (const field of ["rows", "rowsDropped", "units", "unexplained", "uncovered"]) {
    const broken = structuredClone(base.causeEvidence); delete broken.counts[field];
    evidenceRefusal(() => verifyCauseEvidence(withCause(broken)), /counts needs/);
  }
});

test("[RA-03] every incoherence between state, verdict, basis, gaps, counts, the census and the sibling counts is refused", () => {
  const f = (over, extra = {}) => ({ ...census(), unexplainedCount: 0, causeCounts: {}, ...extra, causeEvidence: cause(over) });
  const cases = [
    ["verdict explained over an unexplained unit", f({ counts: rowCounts({ unexplained: 1 }), unexplainedUnits: ["x"] }, { unexplainedCount: 1, causeCounts: { unexplained: 1 } }), /verdict explained disagrees/],
    ["verdict explained over a gap", f({ state: "incomplete", gaps: ["output-dropped"] }), /verdict explained disagrees/],
    ["verdict unexplained over no unit", f({ verdict: "unexplained" }), /verdict unexplained disagrees/],
    ["state complete over a gap", f({ verdict: "unknown", gaps: ["output-dropped"] }, { unexplainedCount: null }), /state complete disagrees/],
    ["state incomplete over no gap", f({ state: "incomplete" }), /state incomplete disagrees/],
    ["reasons read with a reason they were not", f({ unavailable: "quiet-root" }), /names an unavailability reason/],
    ["units above rows", f({ counts: rowCounts({ units: 2 }) }, { causeCounts: { "first-build": 1 } }), /not nested/],
    ["unexplained above units", f({ verdict: "unexplained", counts: rowCounts({ unexplained: 2 }), unexplainedUnits: ["x", "y"] }, { unexplainedCount: 2, causeCounts: { unexplained: 1 } }), /not nested/],
    ["dropped rows without the truncation gap", f({ counts: { rows: 0, rowsDropped: 3, units: 0, unexplained: 0, uncovered: 0 } }), /reasons-truncated gap disagree/],
    ["the truncation gap without dropped rows", f({ state: "incomplete", verdict: "unknown", gaps: ["reasons-truncated"] }, { unexplainedCount: null }), /reasons-truncated gap disagree/],
    ["an uncovered unit without its gap", f({ counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 1 }, uncoveredUnits: ["b v0.1.0"] }), /rebuilt-unit-without-reason gap disagree/],
    ["unit labels that do not add up to the count", f({ counts: rowCounts({ unexplained: 2 }), verdict: "unexplained", unexplainedUnits: ["x"], unexplainedUnitsTruncated: 0 }, { unexplainedCount: 2, causeCounts: { unexplained: 2 } }), /do not add up/],
    ["unread reasons in a reading state", f({ basis: "none", verdict: "unknown", unavailable: "quiet-root", gaps: ["log-not-read"] }, { unexplainedCount: null }), /state complete is not unavailable/],
    ["unread reasons with no reason", f(unread("quiet-root", { unavailable: null }), { unexplainedCount: null }), /states no reason why/],
    ["unread reasons with counts", f({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root", counts: rowCounts() }), /counts reasons that were never read/],
    ["basis none that is explained", f({ state: "unavailable", basis: "none", unavailable: "quiet-root" }), /basis none is the typed unknown/],
    ["basis none without the log-not-read gap", f(unread("quiet-root", { gaps: ["census-unavailable"] }), { unexplainedCount: null }), /basis none is the typed unknown/],
    ["census-nothing-rebuilt with a gap", f({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root", gaps: ["output-dropped"] }), /explained verdict with no gap/],
    ["census-nothing-rebuilt over rebuilt units", f({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }, { notFresh: 2 }), /does not prove it/],
    ["census-nothing-rebuilt over an unavailable census", f({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }, { available: false }), /does not prove it/],
    ["census-nothing-rebuilt over parse errors", f({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }, { parseErrors: 1 }), /does not prove it/],
    ["census-nothing-rebuilt over a truncated package list", f({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }, { notFreshPackagesTruncated: 2 }), /does not prove it/],
    ["nothing-compiled over an available census", f({ state: "unavailable", basis: "nothing-compiled", unavailable: "not-read" }), /nothing was compiled/],
    ["nothing-compiled under another reason", f({ state: "unavailable", basis: "nothing-compiled", unavailable: "not-read" }, { available: false, reason: "timeout" }), /nothing was compiled/],
    ["an unknown verdict counted as zero", f(unread("quiet-root"), { unexplainedCount: 0 }), /unexplainedCount \(0\) is not the projection/],
    ["a verdict with no sibling count", { ...f({}), unexplainedCount: undefined }, /unexplainedCount \(absent\)/],
    ["a sibling count that is not the unexplained count", f({}, { unexplainedCount: 1 }), /unexplainedCount \(1\) is not the projection/],
    ["cause counts missing for read reasons", { ...f({}), causeCounts: undefined }, /causeCounts is missing/],
    ["cause counts that are not counts", f({}, { causeCounts: { "first-build": -1 } }), /causeCounts is missing/],
    ["cause counts that do not add up to the rows", f({}, { causeCounts: { "first-build": 2 } }), /add up to 2/],
    ["an unexplained cause behind an explained verdict", f({ counts: rowCounts() }, { causeCounts: { "fingerprint-missing": 1 } }), /cannot give 0 unexplained/],
    ["an unknown cause name behind an explained verdict", f({ counts: rowCounts() }, { causeCounts: { "made-up-cause": 1 } }), /cannot give 0 unexplained/],
    ["an unexplained verdict whose causes all explain", f({ verdict: "unexplained", counts: rowCounts({ unexplained: 1 }), unexplainedUnits: ["x"] }, { unexplainedCount: 1, causeCounts: { "first-build": 1 } }), /cannot give 1 unexplained/],
  ];
  for (const [name, freshness, why] of cases) evidenceRefusal(() => verifyCauseEvidence(freshness), why);
  assert.ok(cases.length > 30);
});
