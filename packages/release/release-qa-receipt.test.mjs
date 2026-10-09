import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, copyFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { computePolicyBundle, diffPolicyBundles, POLICY_BUNDLE_ENTRYPOINTS, QA_FLOOR_FILE, validatePolicyBundle } from "./ownership-policy-bundle.mjs";
import { ProtectedSecretMissingError } from "./protected-credentials.mjs";
import {
  assertInstalledQaAdmission, assertSourceQaAdmission, buildQaReceipt, dependencyClosure, evaluateRequiredChecks, loadQaFloor,
  qaReceiptPath, resolveQaReceiptKey, sealQaReceipt, sourceIdentity, validateAppQaConfig, writeQaReceipt, QaAdmissionError,
} from "./release-qa-receipt.mjs";
import { runQa } from "./release-qa-command.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const KEY = "unit-test-key";
const store = { read: () => KEY };
const BUNDLE = "b".repeat(64);
const SHA = (c) => c.repeat(64);

function sh(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function fixture() {
  const root = mkdtempSync(path.join(tmpdir(), "rk-qa-receipt-"));
  sh(root, ["init", "-q", "-b", "main"]);
  sh(root, ["config", "user.email", "t@example.com"]); sh(root, ["config", "user.name", "t"]);
  sh(root, ["remote", "add", "origin", "https://github.com/bogusyogi/example-app.git"]);
  writeFileSync(path.join(root, "Cargo.lock"), "lock-1\n");
  writeFileSync(path.join(root, ".gitignore"), ".right-release/\n");
  writeFileSync(path.join(root, "right-release.config.mjs"), "export default { app: 'demo', version: '1.0.0' };\n");
  sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "init"]);
  return root;
}

const qa = { version: "1", config: "rightkit-qa.toml", checks: { "smoke.launch": ["app.launch"], journey: ["journey.*"] } };
const passingEvidence = () => ({ schemaVersion: 4, runId: "r1", status: "passed", finishedAt: "2026-10-09T00:00:00Z", checks: [
  { name: "app.launch", status: "passed" }, { name: "journey.open-doc", status: "passed" }, { name: "journey.save", status: "passed" },
] });
const floorFixture = () => loadQaFloor(here);

function receiptFor(root, { stage = "installed", evidence = passingEvidence(), artifacts = [{ name: "demo.dmg", sha256: SHA("a"), sizeBytes: 3 }], sourceReceiptDigest = SHA("c"), bundle = BUNDLE, runnerName = "rightkit-qa", key = KEY } = {}) {
  const { floor, digest } = floorFixture();
  const source = sourceIdentity(root);
  const required = evaluateRequiredChecks({ floor, stage, qa, evidence });
  const receipt = sealQaReceipt(buildQaReceipt({
    stage, app: "demo", platform: "mac", releaseId: "demo-1.0.0-abcd1234", source, closure: dependencyClosure(root, source.commit),
    releaseConfigSha256: SHA("d"), qa, floor, floorDigest: digest, policyBundleDigest: bundle,
    runner: { basename: runnerName, path: "/x/rightkit-qa", sha256: SHA("e") }, evidence, evidenceSha256: SHA("f"), required,
    artifacts: stage === "installed" ? artifacts : [], sourceReceiptDigest: stage === "installed" ? sourceReceiptDigest : null,
  }), key);
  writeQaReceipt(qaReceiptPath({ repoRoot: root, releaseId: "demo-1.0.0-abcd1234", platform: "mac", stage }), receipt);
  return receipt;
}

const manifestFor = (root, overrides = {}) => ({
  releaseId: "demo-1.0.0-abcd1234", commit: sourceIdentity(root).commit,
  files: [{ name: "demo.dmg", sha256: SHA("a"), sizeBytes: 3 }], qa: { sourceReceiptDigest: SHA("c") }, ...overrides,
});
const gate = (root, manifest = manifestFor(root), extra = {}) => assertInstalledQaAdmission({
  repoRoot: root, manifest, platform: "mac", configName: "right-release.config.mjs", releaseConfigSha256: SHA("d"),
  credentialStore: store, policyBundleDigest: BUNDLE, floorDir: here, ...extra,
});

test("missing installed receipt refuses and names the exact QA command", () => {
  const root = fixture();
  try {
    assert.throws(() => gate(root), (error) => error instanceof QaAdmissionError
      && /no installed QA receipt/.test(error.message)
      && error.message.includes("right-release qa --stage installed --platform mac --release demo-1.0.0-abcd1234 --config right-release.config.mjs"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("passing artifact-bound receipt for the exact commit admits", () => {
  const root = fixture();
  try { receiptFor(root); assert.equal(gate(root).digest.length, 64); } finally { rmSync(root, { recursive: true, force: true }); }
});

test("stale commit refuses", () => {
  const root = fixture();
  try {
    receiptFor(root);
    writeFileSync(path.join(root, "new.txt"), "x"); sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "next"]);
    assert.throws(() => gate(root), /receipt is for commit/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("dependency lockfile digest change refuses", () => {
  const root = fixture();
  try {
    receiptFor(root);
    const stale = sourceIdentity(root);
    writeFileSync(path.join(root, "Cargo.lock"), "lock-2\n"); sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "deps"]);
    assert.throws(() => gate(root, manifestFor(root), { source: stale, closure: dependencyClosure(root, sourceIdentity(root).commit) }), /dependency closure/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("artifact hash mismatch refuses", () => {
  const root = fixture();
  try {
    receiptFor(root);
    const manifest = manifestFor(root, { files: [{ name: "demo.dmg", sha256: SHA("9"), sizeBytes: 3 }] });
    assert.throws(() => gate(root, manifest), /artifact hashes do not match the sealed artifacts/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("skipped or missing required check refuses even though the runner exited", () => {
  for (const checks of [
    [{ name: "app.launch", status: "passed" }, { name: "journey.save", status: "skipped" }],
    [{ name: "app.launch", status: "passed" }],
  ]) {
    const root = fixture();
    try {
      receiptFor(root, { evidence: { ...passingEvidence(), checks } });
      assert.throws(() => gate(root), /records a failed QA run/);
    } finally { rmSync(root, { recursive: true, force: true }); }
  }
  const { floor } = floorFixture();
  const results = evaluateRequiredChecks({ floor, stage: "installed", qa, evidence: { checks: [{ name: "app.launch", status: "passed" }] } });
  assert.deepEqual(results.map((r) => [r.id, r.status]), [["smoke.launch", "passed"], ["journey", "missing"]]);
});

test("forged receipts refuse: wrong key, edited body, wrong runner, unchained to the source receipt", () => {
  const root = fixture();
  try {
    receiptFor(root, { key: "attacker-key" });
    assert.throws(() => gate(root), /MAC is invalid/);
    const forged = receiptFor(root);
    const file = qaReceiptPath({ repoRoot: root, releaseId: "demo-1.0.0-abcd1234", platform: "mac", stage: "installed" });
    writeFileSync(file, JSON.stringify({ ...forged, artifacts: [{ name: "demo.dmg", sha256: SHA("9") }] }));
    assert.throws(() => gate(root, manifestFor(root, { files: [{ name: "demo.dmg", sha256: SHA("9"), sizeBytes: 3 }] })), /do not match its digest/);
    receiptFor(root, { runnerName: "my-fake-runner" });
    assert.throws(() => gate(root), /not produced by the rightkit-qa runner/);
    receiptFor(root);
    assert.throws(() => gate(root, manifestFor(root, { qa: { sourceReceiptDigest: SHA("1") } })), /not chained to the source QA receipt/);
    assert.throws(() => gate(root, manifestFor(root, { qa: undefined })), /not chained/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("a changed QA floor or policy bundle invalidates existing receipts", () => {
  const root = fixture();
  try {
    receiptFor(root);
    assert.throws(() => gate(root, manifestFor(root), { policyBundleDigest: "e".repeat(64) }), /policy bundle changed/);
    const floorDir = mkdtempSync(path.join(tmpdir(), "rk-qa-floor-"));
    const weaker = { ...JSON.parse(readFileSync(path.join(here, QA_FLOOR_FILE), "utf8")), version: 2 };
    writeFileSync(path.join(floorDir, QA_FLOOR_FILE), JSON.stringify(weaker));
    assert.throws(() => gate(root, manifestFor(root), { floorDir }), /QA floor changed/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("source gate: missing refuses with command, passing admits and returns the digest sealed into the manifest", () => {
  const root = fixture();
  try {
    const releaseId = `demo-1.0.0-${sourceIdentity(root).commit.slice(0, 8)}`;
    const opts = { repoRoot: root, releaseId, platform: "mac", configName: "right-release.config.mjs", releaseConfigSha256: SHA("d"), credentialStore: store, policyBundleDigest: BUNDLE, floorDir: here };
    assert.throws(() => assertSourceQaAdmission(opts), /no source QA receipt.*right-release qa --stage source --platform mac/s);
    const { floor, digest } = floorFixture();
    const source = sourceIdentity(root);
    const evidence = passingEvidence();
    const receipt = sealQaReceipt(buildQaReceipt({
      stage: "source", app: "demo", platform: "mac", releaseId, source, closure: dependencyClosure(root, source.commit), releaseConfigSha256: SHA("d"), qa, floor, floorDigest: digest,
      policyBundleDigest: BUNDLE, runner: { basename: "rightkit-qa", path: "/x", sha256: SHA("e") }, evidence, evidenceSha256: SHA("f"),
      required: evaluateRequiredChecks({ floor, stage: "source", qa, evidence }),
    }), KEY);
    writeQaReceipt(qaReceiptPath({ repoRoot: root, releaseId, platform: "mac", stage: "source" }), receipt);
    assert.deepEqual(assertSourceQaAdmission(opts), { digest: receipt.digest, floorDigest: digest });
    assert.throws(() => assertSourceQaAdmission({ ...opts, releaseConfigSha256: SHA("0") }), /release config changed/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("right-release qa writes the receipt through the runner path and fails closed when the runner fails", async () => {
  const root = fixture();
  try {
    writeFileSync(path.join(root, "right-release.config.mjs"), `export default { app: "demo", version: "1.0.0", qa: ${JSON.stringify(qa)} };\n`);
    writeFileSync(path.join(root, "rightkit-qa.toml"), "app = 'demo'\n");
    writeFileSync(path.join(root, "rightkit-qa"), "#!/bin/sh\n");
    sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "qa"]);
    const evidenceFile = path.join(mkdtempSync(path.join(tmpdir(), "rk-qa-ev-")), "evidence.json");
    const run = async (evidence, status) => {
      writeFileSync(evidenceFile, JSON.stringify(evidence));
      return runQa({ stage: "source", platform: "mac", configPath: path.join(root, "right-release.config.mjs"), deps: {
        skipPrimaryCheckout: true, quiet: true, credentialStore: store, policyBundleDigest: BUNDLE, runnerFile: path.join(root, "rightkit-qa"),
        spawnRunner: () => ({ status, stdout: `[qa] 3 passed, 0 failed, 0 skipped; evidence ${evidenceFile}\n`, stderr: "" }),
      } });
    };
    const ok = await run(passingEvidence(), 0);
    assert.equal(ok.receipt.status, "passed");
    assert.equal(ok.receipt.runner.basename, "rightkit-qa");
    assert.equal(ok.receipt.commit, sourceIdentity(root).commit);
    assert.deepEqual(ok.receipt.required.map((r) => r.id), ["smoke.launch", "journey"]);
    const bad = await run(passingEvidence(), 1);
    assert.equal(bad.receipt.status, "failed");
    const skipped = await run({ ...passingEvidence(), checks: [{ name: "app.launch", status: "passed" }, { name: "journey.a", status: "skipped" }] }, 0);
    assert.equal(skipped.receipt.status, "failed");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("app QA config cannot drop a floor-required check", () => {
  const { floor } = floorFixture();
  assert.deepEqual(validateAppQaConfig(qa, floor, "installed"), []);
  const dropped = { ...qa, checks: { "smoke.launch": ["app.launch"] } };
  assert.match(validateAppQaConfig(dropped, floor, "installed").join(";"), /journey/);
  assert.match(validateAppQaConfig(undefined, floor, "source").join(";"), /must declare qa/);
});

test("QA floor is part of the protected policy bundle: changing it changes the bundle digest", () => {
  assert.ok(POLICY_BUNDLE_ENTRYPOINTS.includes("release-qa-receipt.mjs"));
  const base = { scannerVersion: "1.0.0", manifest: { schema: 1 }, registry: null, allowlist: null, files: [] };
  const floor = JSON.parse(readFileSync(path.join(here, QA_FLOOR_FILE), "utf8"));
  const a = computePolicyBundle({ ...base, qaFloor: floor });
  const dropped = { ...floor, stages: { ...floor.stages, source: { required: ["smoke.launch"] } } };
  const b = computePolicyBundle({ ...base, qaFloor: dropped });
  assert.notEqual(a.digest, b.digest);
  assert.notEqual(a.qaFloorDigest, b.qaFloorDigest);
  assert.equal(diffPolicyBundles(a, b).qaFloorChanged, true);
  assert.equal(validatePolicyBundle(a).ok, true);
  assert.equal(computePolicyBundle({ ...base, qaFloor: floor }).digest, a.digest);
  // A bundle pinned before step 4 (no qaFloorDigest) stays well-formed but can never equal the running bundle.
  const { qaFloorDigest, digest, ...legacyParts } = a;
  assert.notEqual(legacyParts.scannerVersion, undefined);
  assert.ok(qaFloorDigest && digest);
});

test("receipt key comes from the protected store; the CI env fallback is limited to hosted CI", () => {
  const missing = { read() { throw new ProtectedSecretMissingError("qa-receipt-key", null); } };
  assert.throws(() => resolveQaReceiptKey({ env: { RIGHTKIT_QA_RECEIPT_KEY: "x" }, credentialStore: missing }), ProtectedSecretMissingError);
  assert.equal(resolveQaReceiptKey({ env: { GITHUB_ACTIONS: "true", RIGHTKIT_QA_RECEIPT_KEY: "ci-key" }, credentialStore: missing }), "ci-key");
});

test("dev lanes never reach the upload gate (install-app --dev, development lane); no manifest flag skips it", () => {
  for (const file of ["install-app.mjs", "development.mjs", "portable-development.mjs", "development-signing.mjs"]) {
    const source = readFileSync(path.join(here, file), "utf8");
    assert.doesNotMatch(source, /assertInstalledQaAdmission|assertSourceQaAdmission|upload-release|publish-update/, `${file} must stay outside the release QA gate`);
  }
  const gateSource = readFileSync(path.join(here, "release-qa-receipt.mjs"), "utf8");
  assert.doesNotMatch(gateSource, /manifest\.kind|manifest\.dev|process\.env\.RIGHT_RELEASE_(?:SKIP|DEV)/);
  for (const file of ["upload-release.mjs", "publish-update.mjs", "github-release.mjs"]) {
    assert.match(readFileSync(path.join(here, file), "utf8"), /assertInstalledQaAdmission/, `${file} must call the installed-QA gate`);
  }
  assert.match(readFileSync(path.join(here, "build-release.mjs"), "utf8"), /assertSourceQaAdmission/);
});
