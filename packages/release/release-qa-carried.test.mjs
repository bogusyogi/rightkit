// Orthic-Labs/rightkit#183: the signing job reads the source QA receipt carried from the QA job.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { admitSourceQa } from "./release-qa-audit.mjs";
import {
  assertSourceQaAdmission, buildQaReceipt, carriedSourceReceiptPath, dependencyClosure, evaluateRequiredChecks, loadQaFloor,
  QA_CARRIED_RECEIPT_ENV, sealQaReceipt, sourceIdentity, writeQaReceipt,
} from "./release-qa-receipt.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const KEY = "unit-test-key";
const store = { read: () => KEY };
const BUNDLE = "b".repeat(64);
const SHA = (c) => c.repeat(64);
const qa = { version: "1", config: "rightkit-qa.toml", checks: { "smoke.launch": ["app.launch"], journey: ["journey.*"] } };
const evidence = () => ({ schemaVersion: 4, runId: "r1", status: "passed", finishedAt: "2026-10-09T00:00:00Z", checks: [
  { name: "app.launch", status: "passed" }, { name: "journey.open-doc", status: "passed" }, { name: "journey.save", status: "passed" },
] });

function sh(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function fixture() {
  const root = mkdtempSync(path.join(tmpdir(), "rk-qa-carried-"));
  sh(root, ["init", "-q", "-b", "main"]);
  sh(root, ["config", "user.email", "t@example.com"]); sh(root, ["config", "user.name", "t"]);
  sh(root, ["remote", "add", "origin", "https://github.com/bogusyogi/example-app.git"]);
  writeFileSync(path.join(root, "Cargo.lock"), "lock-1\n");
  writeFileSync(path.join(root, ".gitignore"), ".right-release/\n");
  sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "init"]);
  return root;
}

function sourceReceipt(root, releaseId, platform = "mac") {
  const { floor, digest } = loadQaFloor(here);
  const source = sourceIdentity(root);
  return sealQaReceipt(buildQaReceipt({
    stage: "source", app: "demo", platform, releaseId, source, closure: dependencyClosure(root, source.commit), releaseConfigSha256: SHA("d"), qa, floor, floorDigest: digest,
    policyBundleDigest: BUNDLE, runner: { basename: "rightkit-qa", path: "/x", sha256: SHA("e") }, evidence: evidence(), evidenceSha256: SHA("f"),
    required: evaluateRequiredChecks({ floor, stage: "source", qa, evidence: evidence() }),
  }), KEY);
}

const gateOpts = (root, releaseId, env) => ({
  repoRoot: root, releaseId, platform: "mac", configName: "right-release.config.mjs", releaseConfigSha256: SHA("d"),
  credentialStore: store, policyBundleDigest: BUNDLE, floorDir: here, env,
});

test("carriedSourceReceiptPath: unset or absent is null; a file or a directory resolves to the receipt", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-carry-dir-"));
  try {
    assert.equal(carriedSourceReceiptPath({}), null);
    assert.equal(carriedSourceReceiptPath({ [QA_CARRIED_RECEIPT_ENV]: path.join(dir, "nope.json") }), null);
    assert.equal(carriedSourceReceiptPath({ [QA_CARRIED_RECEIPT_ENV]: dir }), null);
    const file = path.join(dir, "source.receipt.json");
    writeFileSync(file, "{}");
    assert.equal(carriedSourceReceiptPath({ [QA_CARRIED_RECEIPT_ENV]: dir }), file);
    assert.equal(carriedSourceReceiptPath({ [QA_CARRIED_RECEIPT_ENV]: file }), file);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("signing job with no local receipt records passed from the carried receipt (qa.status passed, digest sealed)", () => {
  const root = fixture();
  const carry = mkdtempSync(path.join(tmpdir(), "rk-qa-carry-"));
  try {
    const releaseId = `demo-1.0.0-${sourceIdentity(root).commit.slice(0, 8)}`;
    const receipt = sourceReceipt(root, releaseId);
    writeQaReceipt(path.join(carry, "source.receipt.json"), receipt);
    const warnings = [];
    const sealed = admitSourceQa(gateOpts(root, releaseId, { [QA_CARRIED_RECEIPT_ENV]: carry }), { warn: (line) => warnings.push(line) });
    assert.equal(sealed.status, "passed");
    assert.equal(sealed.sourceReceiptDigest, receipt.digest);
    assert.deepEqual(warnings, []);
    // Same job without the carried receipt records missing, as before.
    const missing = admitSourceQa(gateOpts(root, releaseId, {}), { warn: () => {} });
    assert.equal(missing.status, "missing");
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(carry, { recursive: true, force: true }); }
});

test("carried receipt is verified like a local one: another commit, another release id, another platform and a tampered body are rejected", () => {
  const root = fixture();
  const carry = mkdtempSync(path.join(tmpdir(), "rk-qa-carry-"));
  try {
    const releaseId = `demo-1.0.0-${sourceIdentity(root).commit.slice(0, 8)}`;
    const env = { [QA_CARRIED_RECEIPT_ENV]: carry };
    const file = path.join(carry, "source.receipt.json");

    writeQaReceipt(file, sourceReceipt(root, "demo-1.0.0-deadbeef"));
    assert.throws(() => assertSourceQaAdmission(gateOpts(root, releaseId, env)), /carried receipt is for demo-1\.0\.0-deadbeef/);

    writeQaReceipt(file, sourceReceipt(root, releaseId, "win"));
    assert.throws(() => assertSourceQaAdmission(gateOpts(root, releaseId, env)), /carried receipt is for/);

    writeQaReceipt(file, { ...sourceReceipt(root, releaseId), commit: "0".repeat(40) });
    assert.throws(() => assertSourceQaAdmission(gateOpts(root, releaseId, env)), /do not match its digest/);

    writeQaReceipt(file, sourceReceipt(root, releaseId));
    writeFileSync(path.join(root, "next.txt"), "x\n");
    sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "next"]);
    assert.throws(() => assertSourceQaAdmission(gateOpts(root, releaseId, env)), /receipt is for commit/);
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(carry, { recursive: true, force: true }); }
});

test("a wrong MAC key at the signing job rejects the carried receipt", () => {
  const root = fixture();
  const carry = mkdtempSync(path.join(tmpdir(), "rk-qa-carry-"));
  try {
    const releaseId = `demo-1.0.0-${sourceIdentity(root).commit.slice(0, 8)}`;
    writeQaReceipt(path.join(carry, "source.receipt.json"), sourceReceipt(root, releaseId));
    assert.throws(() => assertSourceQaAdmission({ ...gateOpts(root, releaseId, { [QA_CARRIED_RECEIPT_ENV]: carry }), credentialStore: { read: () => "another-key" } }), /MAC is invalid/);
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(carry, { recursive: true, force: true }); }
});
