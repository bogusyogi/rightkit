import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import { canonicalJson, createDraft, keyIdForPublicKey, occurrenceHash, policyEntryDigest, sha256, validateRecord, verifyException } from "./ownership-approval.mjs";

const vectors = JSON.parse(readFileSync(new URL("../../ownership-approval/conformance/v2-vectors.json", import.meta.url), "utf8"));
const finding = { fingerprint: vectors.valid[0].record.payload.fingerprint, file: "src/browser.ts", kind: "source", lineText: "import puppeteer from 'puppeteer';", ordinal: 0 };
const keys = Object.fromEntries(["windows", "macos"].map((host) => {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" }), publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  return [host, { ...pair, publicKeyPem, keyId: keyIdForPublicKey(host, publicKeyPem) }];
}));
const payload = { ...vectors.valid[0].record.payload, occurrences: [{ path: finding.file, kind: "source", hash: occurrenceHash(finding) }] };
const baseline = { schema: 2, payload, signature: null };
const trust = { schema: 1, minimumScannerVersion: "2", minimumCatalogVersion: "1", revokedKeyIds: [], provenance: [],
  repositories: [{ repositoryIdentity: payload.repositoryIdentity, roots: ["."], visibility: "public" }],
  verifiedTrackingEvidence: [sha256({ repositoryIdentity: payload.repositoryIdentity, requestId: payload.requestId, trackingRef: payload.trackingRef })],
  hosts: Object.fromEntries(Object.entries(keys).map(([host, key]) => [host, { keyId: key.keyId, publicKeyPem: key.publicKeyPem }])) };
const context = { repositoryIdentity: payload.repositoryIdentity, policyDigest: payload.policyDigest, scannerVersion: "2.4.0", catalogVersion: "1.4.0", findings: [finding], today: "2026-10-08", trust };
function signed(record) {
  const host = record.signature?.host ?? "windows", key = keys[host];
  return { ...record, signature: { host, keyId: key.keyId, value: sign("sha256", Buffer.from(canonicalJson(record.payload)), key.privateKey).toString("base64") } };
}
for (const [valid, entries] of [[true, vectors.valid], [false, vectors.invalid]]) for (const vector of entries) test(`schema v2 conformance: ${vector.name}`, () => {
  const problems = validateRecord(vector.record);
  const result = problems.length ? { ok: false, problems } : verifyException(vector.record, { ...context, trust: null });
  assert.equal(result.problems.length === 1 && result.problems[0] === "pending: external ownership trust is not enrolled", valid, result.problems.join("; "));
  if (valid) {
    assert.match(vector.keyId, /^(?:win|mac)-[a-f0-9]{64}$/);
    assert.deepEqual(verifyException(signed(vector.record), { ...context, findings: vector.findings }), { ok: true, problems: [] });
  }
});
test("valid signed v2 record is accepted", () => {
  assert.deepEqual(verifyException(signed(baseline), context), { ok: true, problems: [] });
});

test("draft defaults to issued + 90 days and carries exact occurrence triples", () => {
  const record = createDraft({ ...payload, expires: undefined, findings: [finding] });
  assert.equal(record.schema, 2);
  assert.equal(record.signature, null);
  assert.equal(record.payload.expires, "2027-01-06");
  assert.deepEqual(record.payload.occurrences, payload.occurrences);
  assert.deepEqual(validateRecord(record), []);
});
test("full line hash ignores display and line number; late edits change identity", () => {
  const lineText = " SendInput(); " + "x".repeat(300), base = { ...finding, lineText, text: lineText.trim().slice(0, 160), line: 4 };
  assert.equal(occurrenceHash(base), occurrenceHash({ ...base, line: 90, text: "different display" }));
  assert.notEqual(occurrenceHash(base), occurrenceHash({ ...base, lineText: lineText.slice(0, 200) + "y" + lineText.slice(201) }));
  assert.notEqual(occurrenceHash(base), occurrenceHash({ ...base, ordinal: 1 }));
  assert.throws(() => occurrenceHash({ ...base, lineText: undefined }), /full lineText/);
});
test("dependency declaration fields and resolved version bind identity", () => {
  const base = { fingerprint: "browser.npm.puppeteer", file: "package.json", kind: "dependency", ecosystem: "npm", name: "puppeteer", alias: null, section: "dependencies", requirement: "^24", features: ["b", "a", "a"], defaultFeatures: null, source: "registry", resolvedVersion: "24.1.0" };
  assert.equal(occurrenceHash(base), occurrenceHash({ ...base, features: ["a", "b"] }));
  for (const change of [{ requirement: "^25" }, { features: ["a"] }, { source: "git+https://github.com/example/browser#main" }, { resolvedVersion: "24.2.0" }, { section: "devDependencies" }, { alias: "browser" }]) assert.notEqual(occurrenceHash(base), occurrenceHash({ ...base, ...change }));
  assert.throws(() => occurrenceHash({ ...base, source: undefined }), /malformed dependency/);
});
test("per-entry policy digest survives other fingerprint edits and rejects own policy edits", () => {
  const manifest = { capabilities: [{ capability: "input", package: "rightkit-qa-driver", tier: "block", fingerprints: [{ id: finding.fingerprint, type: "source", pattern: "SendInput" }, { id: "control.macos.cgevent", type: "source", pattern: "CGEvent" }] }] };
  const digest = policyEntryDigest(manifest, finding.fingerprint), record = signed({ ...baseline, payload: { ...payload, policyDigest: digest } });
  manifest.capabilities[0].fingerprints[1].pattern = "OtherEvent";
  assert.deepEqual(verifyException(record, { ...context, policyDigest: policyEntryDigest(manifest, finding.fingerprint) }), { ok: true, problems: [] });
  manifest.capabilities[0].fingerprints[0].pattern = "OtherInput";
  assert.match(verifyException(record, { ...context, policyDigest: policyEntryDigest(manifest, finding.fingerprint) }).problems.join(" "), /policy digest/);
  assert.throws(() => policyEntryDigest({ capabilities: [] }, finding.fingerprint), /catalog policy/);
});
test("approval floors precede policy, occurrences, and signature checks", () => {
  const record = signed(baseline);
  for (const change of [{ scannerVersion: "1.9.9" }, { catalogVersion: "1.3.9" }]) assert.match(verifyException(record, { ...context, policyDigest: undefined, ...change }).problems.join(" "), /below approval minimum/);
  const altered = { ...record, signature: { ...record.signature, value: "AQ==" } };
  assert.match(verifyException(altered, { ...context, findings: [{ ...finding, lineText: "changed" }] }).problems.join(" "), /occurrence changed/);
});
test("only listed path/hash pairs are covered; additional occurrences stay available", () => {
  const record = signed(baseline);
  assert.equal(verifyException(record, { ...context, findings: [finding, { ...finding, ordinal: 1 }] }).ok, true);
  assert.equal(verifyException(record, { ...context, findings: [{ ...finding, file: "src/other.rs" }] }).ok, false);
});
test("malformed, missing, and unknown v2 fields fail closed", () => {
  for (const record of [null, {}, { ...baseline, signature: undefined }, { ...baseline, payload: null }, { ...baseline, payload: { ...payload, occurrences: [{ ...payload.occurrences[0], kind: "unknown" }] } }, { ...baseline, payload: { ...payload, occurrences: Array(257).fill(payload.occurrences[0]) } }]) assert.equal(verifyException(record, context).ok, false);
  const mismatched = signed(baseline); mismatched.signature.keyId = `mac-${"a".repeat(64)}`;
  assert.match(verifyException(mismatched, context).problems.join(" "), /signature envelope/);
  assert.throws(() => createDraft({ ...payload, findings: undefined }), /findings must be an array/);
});
