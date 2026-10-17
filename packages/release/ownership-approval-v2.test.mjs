import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import { canonicalJson, createDraft, keyIdForPublicKey, occurrenceHash, policyEntryDigest, sha256, validateRecord } from "./ownership-approval.mjs";
import * as approvalModule from "./ownership-approval.mjs";

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
// Exception signatures are retired (#262): the v2 vectors now only exercise the record schema (validateRecord). Vectors that were
// rejected by the removed verifyException (expiry window, future issue date, version floor) have no schema-level check and are skipped.
const VERIFY_ONLY = new Set(["91-day-expiry", "future-issued", "invalid-floor"]);
for (const [valid, entries] of [[true, vectors.valid], [false, vectors.invalid]]) for (const vector of entries) test(`schema v2 conformance: ${vector.name}`, { skip: !valid && VERIFY_ONLY.has(vector.name) }, () => {
  const problems = validateRecord(vector.record);
  assert.equal(problems.length === 0, valid, problems.join("; "));
  if (valid) assert.match(vector.keyId, /^(?:win|mac)-[a-f0-9]{64}$/);
});
test("verifyException is retired; a signed v2 record still passes the envelope schema", () => {
  assert.equal(approvalModule.verifyException, undefined);
  assert.deepEqual(validateRecord(signed(baseline)), []);
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
  const digest = policyEntryDigest(manifest, finding.fingerprint);
  manifest.capabilities[0].fingerprints[1].pattern = "OtherEvent";
  assert.equal(policyEntryDigest(manifest, finding.fingerprint), digest);
  manifest.capabilities[0].fingerprints[0].pattern = "OtherInput";
  assert.notEqual(policyEntryDigest(manifest, finding.fingerprint), digest);
  assert.throws(() => policyEntryDigest({ capabilities: [] }, finding.fingerprint), /catalog policy/);
});
test("malformed, missing, and unknown v2 fields fail closed", () => {
  for (const record of [null, {}, { ...baseline, signature: undefined }, { ...baseline, payload: null }, { ...baseline, payload: { ...payload, occurrences: [{ ...payload.occurrences[0], kind: "unknown" }] } }, { ...baseline, payload: { ...payload, occurrences: Array(257).fill(payload.occurrences[0]) } }]) assert.notDeepEqual(validateRecord(record), []);
  const mismatched = signed(baseline); mismatched.signature.keyId = `mac-${"a".repeat(64)}`;
  assert.match(validateRecord(mismatched).join(" "), /signature envelope/);
  assert.throws(() => createDraft({ ...payload, findings: undefined }), /findings must be an array/);
});
