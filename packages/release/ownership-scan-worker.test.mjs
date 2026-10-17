import { spawnSync } from "./background-process.mjs";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { catalogDigest, checkApp } from "./ownership.mjs";
import { sha256 } from "./ownership-approval.mjs";
import { ownershipVerdict } from "./ownership-verdict.mjs";
import { ownershipScanInputsForTest, scanOwnershipForTest } from "./ownership-scan-worker.mjs";
import { runScanWorker } from "./ownership-admission.mjs";

const identity = "https://github.com/test/repo";
const trust = { schema: 1, revokedKeyIds: [], verifiedTrackingEvidence: [], minimumScannerVersion: "2.0.0", minimumCatalogVersion: "1", repositories: [{ repositoryIdentity: identity, roots: ["."], visibility: "private" }] };
function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-worker-verdict-"));
  const manifestPath = path.join(root, "catalog.json");
  writeFileSync(manifestPath, JSON.stringify({ schema: 1, version: "1", capabilities: [] }));
  const state = { trust: structuredClone(trust), enrolled: true, pending: false, protection: { ok: true } };
  const inventory = () => ({ digest: sha256(readFileSync(path.join(root, "source.rs"))), complete: true, coverage: [], files: [] });
  writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
  const gitRunner = (_exe, args) => ({ status: 0, stdout: args.includes("remote") ? identity : "a".repeat(40) });
  const options = { manifestPath, trustLoader: () => state, inventory, gitRunner, policyDigest: () => sha256("scanner-v1"),
    scan: (_root, manifest) => ({ ok: true, findings: [], violations: [], problems: [], rows: [], covered: [], coverage: [], metadata: { repositoryIdentity: identity, scannerVersion: "2.4.0", catalogVersion: "1", catalogDigest: catalogDigest(manifest), contentDigests: [], coverage: [] } }) };
  return { root, options, state };
}

test("worker uses protected evidence & shared verdict instead of raw checkApp", () => {
  const f = fixture();
  try {
    const checked = scanOwnershipForTest(f.root, f.options);
    assert.equal(checked.verdict.admit, true);
    assert.deepEqual(checked.verdict, ownershipVerdict(checked.report, checked.trust));
    assert.equal(checked.evidence.registrationDigest, sha256(trust.repositories[0]));
    assert.equal(checked.evidence.floorOk, true);
    f.state.trust.repositories = [];
    assert.equal(scanOwnershipForTest(f.root, f.options).verdict.code, "RK_OWNERSHIP_INVALID_REGISTRATION");
    f.state.trust = { ...trust, minimumScannerVersion: "99.0.0" };
    assert.equal(scanOwnershipForTest(f.root, f.options).verdict.code, "RK_OWNERSHIP_POLICY_DOWNGRADE");
    f.state.trust = { ...trust, minimumCatalogVersion: "99" };
    assert.equal(scanOwnershipForTest(f.root, f.options).verdict.code, "RK_OWNERSHIP_POLICY_DOWNGRADE");
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("unenrolled hosts: pending enrollment is a warning and no longer short-circuits the verdict (#262)", () => {
  const f = fixture();
  try {
    f.state.enrolled = false; f.state.pending = true;
    const checked = scanOwnershipForTest(f.root, f.options);
    assert.equal(checked.report.enrollmentPending, true);
    assert.equal(checked.evidence.trustEnrolled, false);
    assert.notEqual(checked.verdict.code, "RK_OWNERSHIP_PENDING_ENROLLMENT");
    if (checked.verdict.admit) assert.match((checked.verdict.warnings ?? []).join(" "), /enrollment is pending/);
    f.state.trust = null; f.state.protection = { ok: false, reason: "external trust file is absent" };
    assert.notEqual(scanOwnershipForTest(f.root, f.options).verdict.code, "RK_OWNERSHIP_PENDING_ENROLLMENT");
    // checkApp inventories the tree with git, as the other ownership fixtures do (a non-git folder is unscanned).
    assert.equal(spawnSync("git", ["-C", f.root, "init", "-q"], { windowsHide: true }).status, 0);
    const direct = checkApp(f.root, { schema: 1, version: "1", capabilities: [] }, { trust: f.state });
    assert.equal(direct.enrollmentPending, true); assert.equal(direct.ok, true);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("protected evidence coverage-only failure reaches shared worker verdict", () => {
  const f = fixture();
  try {
    const scan = f.options.scan;
    f.options.scan = (...args) => { const report = scan(...args); report.coverage = report.metadata.coverage = [{ path: "native.bin", status: "unscanned", reason: "opaque" }]; return report; };
    assert.equal(scanOwnershipForTest(f.root, f.options).verdict.code, "RK_OWNERSHIP_INCOMPLETE_COVERAGE");
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("worker digests bind inventory, catalog, protected trust, approvals & scanner policy", () => {
  const f = fixture();
  try {
    const first = ownershipScanInputsForTest(f.root, f.options).inputs;
    assert.equal(first.contentDigest, f.options.inventory().digest);
    writeFileSync(path.join(f.root, "source.rs"), "fn changed() {}\n");
    assert.notEqual(ownershipScanInputsForTest(f.root, f.options).inputs.contentDigest, first.contentDigest);
    writeFileSync(f.options.manifestPath, JSON.stringify({ schema: 1, version: "2", capabilities: [] }));
    assert.notEqual(ownershipScanInputsForTest(f.root, f.options).inputs.catalogDigest, first.catalogDigest);
    f.state.trust.revokedKeyIds.push("new-revocation");
    assert.notEqual(ownershipScanInputsForTest(f.root, f.options).inputs.trustDigest, first.trustDigest);
    mkdirSync(path.join(f.root, ".rightkit"));
    writeFileSync(path.join(f.root, ".rightkit", "ownership-exceptions.json"), '{"exceptions":[]}');
    assert.notEqual(ownershipScanInputsForTest(f.root, f.options).inputs.approvalsDigest, first.approvalsDigest);
    f.options.policyDigest = () => sha256("scanner-v2");
    assert.notEqual(ownershipScanInputsForTest(f.root, f.options).inputs.scannerPolicyDigest, first.scannerPolicyDigest);
    f.state.trust.minimumScannerVersion = "2.1.0";
    assert.equal(ownershipScanInputsForTest(f.root, f.options).inputs.protectedFloors.minimumScannerVersion, "2.1.0");
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("production policy digest covers scanner module closure & handles canonical remotes", () => {
  const f = fixture();
  try {
    delete f.options.policyDigest;
    const runner = f.options.gitRunner;
    f.options.gitRunner = (exe, args, options) => args.includes("remote") ? { status: 0, stdout: "git@github.com:TEST/REPO.git" } : runner(exe, args, options);
    const first = ownershipScanInputsForTest(f.root, f.options);
    assert.equal(first.metadata.repositoryIdentity, identity);
    assert.match(first.inputs.scannerPolicyDigest, /^[a-f0-9]{64}$/);
    assert.deepEqual(ownershipScanInputsForTest(f.root, f.options).inputs, first.inputs);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("worker & admission verdict agree after optional scanner fields are serialized", () => {
  const f = fixture();
  try {
    const scan = f.options.scan;
    f.options.scan = (...args) => {
      const report = scan(...args);
      report.ok = false;
      report.findings = report.violations = [{ fingerprint: "browser.npm.playwright", capability: "browser-automation", package: "rightkit-browser", file: "package.json", line: 0,
        kind: "dependency", ecosystem: "npm", name: "playwright", alias: undefined, section: "dependencies", requirement: "1.50.0", features: [], defaultFeatures: null,
        source: "registry", resolvedVersion: null, text: "npm dependency playwright", local: false, provenance: "registry" }];
      return report;
    };
    const checked = scanOwnershipForTest(f.root, f.options);
    assert.equal(checked.verdict.code, "RK_OWNERSHIP_VIOLATION");
    assert.deepEqual(checked.verdict, ownershipVerdict(JSON.parse(JSON.stringify(checked.report)), checked.trust));
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("worker refuses mutated inputs across evidence scan", () => {
  const f = fixture();
  try {
    const scan = f.options.scan;
    f.options.scan = (...args) => { writeFileSync(path.join(f.root, "source.rs"), "fn edited_during_scan() {}\n"); return scan(...args); };
    const checked = scanOwnershipForTest(f.root, f.options);
    assert.equal(checked.code, "RK_OWNERSHIP_INPUTS_CHANGED");
    assert.match(checked.error, /changed during/);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("real broker worker transport returns structured fail-closed verdict or diagnostic", async () => {
  const f = fixture();
  try {
    const checked = await runScanWorker({ scannerRoot: path.dirname(fileURLToPath(import.meta.url)), scanRoot: f.root, timeoutMs: 10_000 });
    if (checked.report) {
      assert.equal(checked.report.ok, false);
      assert.equal(checked.verdict.admit, false);
      assert.deepEqual(checked.verdict, ownershipVerdict(checked.report, checked.trust));
    } else {
      assert.match(checked.error, /ownership|trust|git/i);
      assert.match(checked.code, /^RK_OWNERSHIP_/);
    }
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("inventory coverage changes during scanning fail closed even with identical content digest", () => {
  const f = fixture();
  try {
    let complete = true;
    const inventory = f.options.inventory, scan = f.options.scan;
    f.options.inventory = () => ({ ...inventory(), complete, coverage: complete ? [] : [{ path: "source.rs", status: "unscanned", reason: "unreadable" }] });
    f.options.scan = (...args) => { complete = false; return scan(...args); };
    assert.equal(scanOwnershipForTest(f.root, f.options).code, "RK_OWNERSHIP_INPUTS_CHANGED");
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});
