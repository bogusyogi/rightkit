import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { validateOwnershipFloor, validateTrustRegistry } from "./ownership-approval.mjs";
import { checkApp, checkCommittedAppForTest, dependencyAllowlistPolicy, MANIFEST_FILE, SCANNER_VERSION } from "./ownership.mjs";
import { ALLOWLIST_FILE, diffAllowlists, evaluateDependency, parseEntry, sourceKind, validateAllowlist } from "./ownership-dependency-allowlist.mjs";
import { computePolicyBundle, diffPolicyBundles, POLICY_BUNDLE_ENTRYPOINTS, scannerSourceFiles, trustComparison, validatePolicyBundle } from "./ownership-policy-bundle.mjs";
import { raiseFloorsToInstalledRelease } from "./ownership-key-trust.mjs";

const here = import.meta.dirname;
const catalog = JSON.parse(readFileSync(path.join(here, MANIFEST_FILE), "utf8"));
const IDENTITY = "https://github.com/org/repo";
const allowlist = { schema: 1, global: ["npm:typescript"], repositories: [{ repositoryIdentity: IDENTITY, dependencies: ["npm:lodash", "npm:puppeteer", "cargo:serde", "cargo:gitdep#git"] }] };

// ---- allowlist semantics ---------------------------------------------------------------------------
test("allowlist file grammar and validation", () => {
  assert.deepEqual(parseEntry("npm:@scope/pkg#git"), { ecosystem: "npm", name: "@scope/pkg", kind: "git" });
  assert.deepEqual(parseEntry("cargo:serde"), { ecosystem: "cargo", name: "serde", kind: "registry" });
  for (const bad of ["serde", "pip:x", "npm:", "npm:x#svn", "npm:a b", 5]) assert.equal(parseEntry(bad), null, String(bad));
  assert.equal(validateAllowlist(allowlist).ok, true);
  assert.equal(validateAllowlist({ ...allowlist, schema: 2 }).ok, false);
  assert.equal(validateAllowlist({ ...allowlist, extra: 1 }).ok, false);
  assert.equal(validateAllowlist({ ...allowlist, global: ["npm:x", "npm:x"] }).ok, false);
  assert.equal(validateAllowlist({ ...allowlist, repositories: [{ repositoryIdentity: "https://github.com/o/r.git", dependencies: [] }] }).ok, false);
  assert.equal(sourceKind("registry"), "registry");
  assert.equal(sourceKind("path:../x"), "path");
  assert.equal(sourceKind("git+https://x/y#abc"), "git");
});

test("allowlist decisions: baseline, global, RightKit and path allowed; new, wrong-kind and other-repository denied", () => {
  const ask = (dependency, identity = IDENTITY) => evaluateDependency(allowlist, identity, dependency).allowed;
  assert.equal(ask({ ecosystem: "npm", name: "lodash" }), true);
  assert.equal(ask({ ecosystem: "npm", name: "typescript" }), true);
  assert.equal(ask({ ecosystem: "npm", name: "@rightkit/app-update" }), true);
  assert.equal(ask({ ecosystem: "cargo", name: "anything", kind: "path" }), true);
  assert.equal(ask({ ecosystem: "cargo", name: "gitdep", kind: "git" }), true);
  assert.equal(ask({ ecosystem: "cargo", name: "serde", kind: "git" }), false);
  assert.equal(ask({ ecosystem: "cargo", name: "serde", kind: "any" }), true);
  assert.equal(ask({ ecosystem: "npm", name: "leftpad" }), false);
  assert.equal(ask({ ecosystem: "npm", name: "lodash" }, "https://github.com/org/other"), false);
  assert.equal(ask({ ecosystem: "npm", name: "typescript" }, "https://github.com/org/other"), true);
});

test("allowlist diff reports global and per-repository additions and removals", () => {
  const next = { schema: 1, global: ["npm:typescript", "npm:vite"], repositories: [{ repositoryIdentity: IDENTITY, dependencies: ["npm:lodash", "npm:new"] }, { repositoryIdentity: "https://github.com/org/two", dependencies: ["cargo:a"] }] };
  const diff = diffAllowlists(allowlist, next);
  assert.deepEqual(diff.global, { added: ["npm:vite"], removed: [] });
  const main = diff.repositories.find((r) => r.repositoryIdentity === IDENTITY);
  assert.deepEqual([main.added, main.removed], [["npm:new"], ["cargo:gitdep#git", "cargo:serde", "npm:puppeteer"]]);
  assert.equal(diff.repositories.find((r) => r.repositoryIdentity === "https://github.com/org/two").baseline, "added");
  assert.equal(diffAllowlists(allowlist, allowlist).changed, false);
});

test("shipped allowlist is valid, covers every registered repository and never records block-tier or RightKit deps", () => {
  const shipped = JSON.parse(readFileSync(path.join(here, ALLOWLIST_FILE), "utf8"));
  assert.deepEqual(validateAllowlist(shipped).problems, []);
  const registry = JSON.parse(readFileSync(path.join(here, "ownership-repositories.json"), "utf8"));
  for (const { repositoryIdentity } of registry.repositories) assert.ok(shipped.repositories.some((e) => e.repositoryIdentity === repositoryIdentity), repositoryIdentity);
  const blocked = new Set(catalog.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "dependency" && (f.tier ?? c.tier ?? "block") === "block").map((f) => `${f.ecosystem}:${f.name}`)));
  for (const entry of shipped.repositories) for (const item of entry.dependencies) {
    const parsed = parseEntry(item);
    assert.ok(!blocked.has(`${parsed.ecosystem}:${parsed.name}`), `${entry.repositoryIdentity} records block-tier ${item}`);
    assert.ok(!/^(?:@rightkit\/|rightkit-)/.test(parsed.name), `${entry.repositoryIdentity} records RightKit package ${item}`);
  }
});

// ---- policy bundle ---------------------------------------------------------------------------------
test("policy bundle digest covers scanner version, catalog, registry, allowlist and scanner source digests", () => {
  const files = [{ path: "ownership.mjs", sha256: "a".repeat(64) }, { path: "ownership-verdict.mjs", sha256: "b".repeat(64) }];
  const registry = { schema: 1, repositories: [] };
  const base = computePolicyBundle({ manifest: catalog, registry, allowlist, scannerVersion: "2.6.0", files });
  assert.equal(validatePolicyBundle(base).ok, true);
  assert.equal(computePolicyBundle({ manifest: catalog, registry, allowlist, scannerVersion: "2.6.0", files }).digest, base.digest, "deterministic");
  const variants = {
    scannerVersion: { scannerVersion: "2.6.1" },
    catalog: { manifest: { ...catalog, version: `${catalog.version}.1` } },
    registry: { registry: { schema: 1, repositories: [{ repositoryIdentity: IDENTITY, roots: ["."], visibility: "private" }] } },
    allowlist: { allowlist: { ...allowlist, global: [...allowlist.global, "npm:evil"] } },
    scannerSource: { files: [files[0], { ...files[1], sha256: "c".repeat(64) }] },
    scannerSourceAdded: { files: [...files, { path: "new.mjs", sha256: "d".repeat(64) }] },
  };
  for (const [name, change] of Object.entries(variants)) assert.notEqual(computePolicyBundle({ manifest: catalog, registry, allowlist, scannerVersion: "2.6.0", files, ...change }).digest, base.digest, name);
  assert.equal(validatePolicyBundle({ ...base, digest: "0".repeat(64) }).ok, false);
});

test("scanner source closure pins enforcement code (and only code) from the entry points", () => {
  const files = scannerSourceFiles().map((f) => f.path);
  for (const expected of ["ownership.mjs", "ownership-evidence.mjs", "ownership-verdict.mjs", "ownership-approval.mjs", "ownership-lockfiles.mjs", "ownership-key-trust.mjs", "ownership-dependency-allowlist.mjs", "ownership-policy-bundle.mjs"]) assert.ok(files.includes(expected), expected);
  assert.ok(files.some((f) => f.startsWith("node_modules/smol-toml/")), "pinned third-party parser");
  for (const data of [MANIFEST_FILE, "ownership-repositories.json", ALLOWLIST_FILE]) assert.ok(!files.includes(data), `${data} is pinned by its own digest`);
  assert.ok(!files.some((f) => f.endsWith(".test.mjs")));
  assert.ok(POLICY_BUNDLE_ENTRYPOINTS.every((name) => files.includes(name)));
});

test("bundle diff names changed, added and removed scanner files and changed data digests", () => {
  const a = computePolicyBundle({ manifest: catalog, registry: null, allowlist: null, scannerVersion: "2.6.0", files: [{ path: "a.mjs", sha256: "1".repeat(64) }, { path: "b.mjs", sha256: "2".repeat(64) }] });
  const b = computePolicyBundle({ manifest: catalog, registry: null, allowlist, scannerVersion: "2.7.0", files: [{ path: "a.mjs", sha256: "9".repeat(64) }, { path: "c.mjs", sha256: "3".repeat(64) }] });
  const diff = diffPolicyBundles(a, b);
  assert.deepEqual([diff.filesChanged, diff.filesAdded, diff.filesRemoved], [["a.mjs"], ["c.mjs"], ["b.mjs"]]);
  assert.deepEqual(diff.scannerVersion, { from: "2.6.0", to: "2.7.0" });
  assert.equal(diff.allowlistChanged, true);
  assert.equal(diff.catalogChanged, false);
});

// ---- protected trust: floor, validation, pinning, comparison ----------------------------------------
const trustBase = { schema: 1, revokedKeyIds: [], verifiedTrackingEvidence: [], minimumScannerVersion: "1.0.0", minimumCatalogVersion: "1.0.0", provenance: [], repositories: [{ repositoryIdentity: IDENTITY, roots: ["."], visibility: "private" }] };

test("floor refuses a different running bundle; trust without a pinned bundle stays unpinned", () => {
  const bundle = computePolicyBundle({ manifest: catalog, registry: null, allowlist, scannerVersion: SCANNER_VERSION, files: [] });
  const inputs = { scannerVersion: SCANNER_VERSION, catalogVersion: "9", catalogDigest: "x" };
  assert.equal(validateOwnershipFloor({ ...trustBase, policyBundle: bundle }, { ...inputs, policyBundleDigest: bundle.digest }).ok, true);
  const refused = validateOwnershipFloor({ ...trustBase, policyBundle: bundle }, { ...inputs, policyBundleDigest: "f".repeat(64) });
  assert.equal(refused.ok, false);
  assert.match(refused.problems.join(), /policy bundle .*differs from the protected pinned bundle/);
  assert.equal(validateOwnershipFloor(trustBase, { ...inputs, policyBundleDigest: "f".repeat(64) }).ok, true, "absent policyBundle is unpinned");
  assert.equal(validateOwnershipFloor({ ...trustBase, policyBundle: bundle }, inputs).ok, true, "callers that do not compute the bundle are unaffected");
});

test("trust validation accepts legacy trust and rejects malformed bundle or allowlist", () => {
  const bundle = computePolicyBundle({ manifest: catalog, registry: null, allowlist, scannerVersion: SCANNER_VERSION, files: [] });
  assert.equal(validateTrustRegistry(trustBase).ok, true);
  assert.equal(validateTrustRegistry({ ...trustBase, policyBundle: bundle, dependencyAllowlist: allowlist }).ok, true);
  assert.match(validateTrustRegistry({ ...trustBase, policyBundle: { ...bundle, digest: "zz" } }).problems.join(), /policy bundle is malformed/);
  assert.match(validateTrustRegistry({ ...trustBase, dependencyAllowlist: { schema: 1, global: ["bad"], repositories: [] } }).problems.join(), /allowlist is malformed/);
});

test("trust-sync pins the bundle and copies the proposed allowlist; malformed proposals and older releases leave trust untouched", () => {
  const manifest = { ...catalog, version: "99.0.0" };
  const registry = { schema: 1, repositories: trustBase.repositories };
  const pinned = raiseFloorsToInstalledRelease(trustBase, { manifest, scannerVersion: SCANNER_VERSION, policy: { allowlist, registry } });
  assert.equal(pinned.policyBundle.digest, computePolicyBundle({ manifest, registry, allowlist, scannerVersion: SCANNER_VERSION }).digest);
  assert.deepEqual(pinned.dependencyAllowlist, allowlist);
  assert.equal(validateTrustRegistry(pinned).ok, true);
  assert.equal(raiseFloorsToInstalledRelease(trustBase, { manifest, scannerVersion: SCANNER_VERSION, policy: { allowlist: { schema: 1 }, registry } }), trustBase);
  const newer = { ...trustBase, minimumCatalogVersion: "100.0.0" };
  assert.equal(raiseFloorsToInstalledRelease(newer, { manifest, scannerVersion: SCANNER_VERSION, policy: { allowlist, registry } }), newer);
});

test("trust comparison ignores the per-host checkout path and nothing else", () => {
  const trust = { ...trustBase, keyAuthority: { repositoryIdentity: IDENTITY, checkoutPath: "/Volumes/D/claude" }, policyBundle: computePolicyBundle({ manifest: catalog, registry: null, allowlist, scannerVersion: SCANNER_VERSION, files: [] }), dependencyAllowlist: allowlist };
  const mac = trustComparison(trust);
  assert.equal(trustComparison({ ...trust, keyAuthority: { repositoryIdentity: IDENTITY, checkoutPath: "D:\\Claude" } }).digest, mac.digest);
  assert.notEqual(trustComparison({ ...trust, dependencyAllowlist: { ...allowlist, global: ["npm:evil"] } }).digest, mac.digest);
  assert.notEqual(trustComparison({ ...trust, minimumScannerVersion: "0.0.1" }).digest, mac.digest);
  assert.notEqual(trustComparison({ ...trust, policyBundle: { ...trust.policyBundle, digest: "e".repeat(64) } }).digest, mac.digest);
  assert.equal(mac.components.policyBundle, trust.policyBundle.digest);
});

// ---- scanner behavior ------------------------------------------------------------------------------
function repo(files, origin = `${IDENTITY}.git`) {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rk-allowlist-")));
  assert.equal(spawnSync("git", ["-C", root, "init", "-q"], { windowsHide: true }).status, 0);
  if (origin) assert.equal(spawnSync("git", ["-C", root, "remote", "add", "origin", origin], { windowsHide: true }).status, 0);
  for (const [rel, text] of Object.entries(files)) { mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); writeFileSync(path.join(root, rel), text); }
  return root;
}
const pkg = (dependencies) => JSON.stringify({ name: "app", version: "1.0.0", dependencies });
const trustState = ({ pending = false, withAllowlist = true } = {}) => ({ enrolled: !pending, pending, trust: { ...trustBase, ...(withAllowlist ? { dependencyAllowlist: allowlist } : {}) } });
const notAllowlisted = (result) => result.violations.filter((f) => f.fingerprint === "dependency-not-allowlisted");

test("scan: allowlisted dependencies pass; a new dependency is a blocking dependency-not-allowlisted finding", () => {
  const clean = repo({ "package.json": pkg({ lodash: "4", typescript: "5", "@rightkit/app-update": "1", shared: "workspace:*" }) });
  const dirty = repo({ "package.json": pkg({ lodash: "4", leftpad: "1" }) });
  try {
    const ok = checkApp(clean, catalog, { trust: trustState() });
    assert.deepEqual(notAllowlisted(ok), []);
    assert.deepEqual(ok.violations, []);
    const bad = checkApp(dirty, catalog, { trust: trustState() });
    const [finding] = notAllowlisted(bad);
    assert.equal(notAllowlisted(bad).length, 1);
    assert.equal(finding.name, "leftpad");
    assert.equal(finding.file, "package.json");
    assert.equal(finding.tier, "block");
    assert.match(finding.text, /propose it there and Adrian activates it via trust-sync/);
    assert.equal(bad.ok, false);
  } finally { rmSync(clean, { recursive: true, force: true }); rmSync(dirty, { recursive: true, force: true }); }
});

test("scan: block-tier capability dependencies stay blocking even when allowlisted, without a duplicate allowlist finding", () => {
  const root = repo({ "package.json": pkg({ puppeteer: "24" }) });
  try {
    const result = checkApp(root, catalog, { trust: trustState() });
    assert.equal(result.ok, false);
    assert.ok(result.violations.some((f) => f.name === "puppeteer" && f.package === "rightkit-browser" && f.tier === "block"));
    assert.deepEqual(notAllowlisted(result), []);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("scan: pending enrollment makes the allowlist finding advisory", () => {
  const root = repo({ "package.json": pkg({ leftpad: "1" }) });
  try {
    const result = checkApp(root, catalog, { trust: trustState({ pending: true }) });
    assert.equal(result.enrollmentPending, true);
    const [finding] = notAllowlisted(result);
    assert.equal(finding.tier, "advise");
    assert.equal(result.ok, false, "pending enrollment still refuses protected evidence");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("scan: before trust-sync only the shipped proposal applies, advisory, and only for repositories it has a baseline for", () => {
  const root = repo({ "package.json": pkg({ leftpad: "1" }) });
  const stranger = repo({ "package.json": pkg({ leftpad: "1" }) }, "https://github.com/org/unlisted.git");
  try {
    const unsynced = trustState({ withAllowlist: false });
    const advisory = checkApp(root, catalog, { trust: unsynced, shippedAllowlist: allowlist });
    assert.equal(notAllowlisted(advisory)[0].tier, "advise");
    assert.equal(advisory.violations.some((f) => f.tier === "block"), false);
    assert.deepEqual(notAllowlisted(checkApp(stranger, catalog, { trust: unsynced, shippedAllowlist: allowlist })), []);
    assert.equal(dependencyAllowlistPolicy(unsynced.trust, IDENTITY, { shipped: allowlist }).source, "shipped-proposal");
    assert.equal(dependencyAllowlistPolicy(trustState().trust, IDENTITY, { shipped: allowlist }).source, "protected-trust");
    assert.equal(dependencyAllowlistPolicy(trustState().trust, "https://github.com/org/unregistered", { shipped: null }), null);
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(stranger, { recursive: true, force: true }); }
});

test("scan: cargo git dependencies need an explicit #git entry and a missing baseline denies everything for registered repositories", () => {
  const root = repo({ "Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n[dependencies]\nserde = "1"\ngitdep = { git = "https://example.com/g" }\nsib = { path = "sib" }\n', "sib/Cargo.toml": '[package]\nname = "sib"\nversion = "0.1.0"\n' });
  try {
    const result = checkApp(root, catalog, { trust: trustState() });
    assert.deepEqual(notAllowlisted(result), []);
    const bare = { ...trustState(), trust: { ...trustState().trust, dependencyAllowlist: { schema: 1, global: [], repositories: [] } } };
    assert.deepEqual(notAllowlisted(checkApp(root, catalog, { trust: bare })).map((f) => f.name).sort(), ["gitdep", "serde"]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("committed scan applies the same default-deny allowlist", () => {
  const root = repo({ "package.json": pkg({ lodash: "4", leftpad: "1" }) });
  try {
    for (const args of [["add", "."], ["-c", "user.email=t@example.invalid", "-c", "user.name=t", "commit", "-qm", "base"]]) assert.equal(spawnSync("git", ["-C", root, ...args], { windowsHide: true }).status, 0);
    const result = checkCommittedAppForTest(root, catalog, { trust: trustState() });
    assert.deepEqual(notAllowlisted(result).map((f) => f.name), ["leftpad"]);
    assert.equal(result.ok, false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
