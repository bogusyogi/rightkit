import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { createCommittedOwnershipEvidenceForTest, verifyOwnershipEvidenceForTest } from "./ownership-evidence.mjs";
import { iosOwnershipScope, normalizeScopeRoots, preflightOwnershipScope, projectYmlLocalPackagePaths } from "./ownership-scope.mjs";
import { expandIosPackageTarget } from "./package-recipe-targets.mjs";

function git(root, ...args) {
  const r = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
  assert.equal(r.status, 0, r.stderr);
  return r.stdout.trim();
}
const trusted = () => ({ enrolled: true, pending: false, protection: { ok: true }, trust: { schema: 1, revokedKeyIds: [], hosts: {}, verifiedTrackingEvidence: [], minimumScannerVersion: "0.0.0", minimumCatalogVersion: "0.0.0", provenance: [], repositories: [{ repositoryIdentity: "https://github.com/test/repo", roots: ["."], visibility: "private" }] } });
const YML = `name: App\npackages:\n  MeshLink:\n    path: ../swift/MeshLink\n  Remote:\n    url: https://example.invalid/x.git\n    from: 1.0.0\n  Inline: {path: "../swift/Inline"}\ntargets:\n  App:\n    path: Sources\n`;

function fixture() {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-scope-")));
  git(root, "init", "-q");
  git(root, "config", "user.email", "t@example.invalid");
  git(root, "config", "user.name", "t");
  git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
  for (const dir of ["ios", "swift/MeshLink", "swift/Inline", "ffi", "membrane", ".rightkit"]) mkdirSync(path.join(root, dir), { recursive: true });
  writeFileSync(path.join(root, "ios", "project.yml"), YML);
  writeFileSync(path.join(root, "ios", "App.swift"), "let a = 1\n");
  writeFileSync(path.join(root, "swift/MeshLink/Mesh.swift"), "let m = 1\n");
  writeFileSync(path.join(root, "swift/Inline/I.swift"), "let i = 1\n");
  writeFileSync(path.join(root, "ffi", "lib.rs"), "fn f() {}\n");
  writeFileSync(path.join(root, "membrane", "m.rs"), "fn o() { SCOPE_MARKER; }\n");
  writeFileSync(path.join(root, "catalog.json"), JSON.stringify({ schema: 1, version: "1", capabilities: [{ package: "sample", capability: "sample", tier: "block", fingerprints: [{ type: "source", id: "marker", pattern: "SCOPE_MARKER" }] }] }));
  return root;
}
const commit = (root) => { git(root, "add", "."); git(root, "commit", "-qm", "x"); };
const options = (root, scope) => ({ scanRoot: root, manifestPath: path.join(root, "catalog.json"), trustLoader: trusted, scope, committed: true, requireCleanCheckout: true });
const target = (extra = {}) => ({ iosPackage: { project: "ios/App.xcodeproj", projectSpec: "ios/project.yml", scheme: "App" }, ...extra });

test("project.yml local package paths ignore remote packages and other sections", () => {
  assert.deepEqual(projectYmlLocalPackagePaths(YML), ["../swift/MeshLink", "../swift/Inline"]);
});

test("default iOS scope: project dir, project.yml local packages, .rightkit; ownershipScope adds roots", () => {
  const root = fixture();
  try {
    assert.deepEqual(iosOwnershipScope({ repoRoot: root, appRoot: root, target: target() }), [".rightkit", "ios", "swift/Inline", "swift/MeshLink"]);
    assert.deepEqual(iosOwnershipScope({ repoRoot: root, appRoot: root, target: target({ ownershipScope: ["ffi", "ffi/", "ios"] }) }), [".rightkit", "ffi", "ios", "swift/Inline", "swift/MeshLink"]);
    const expanded = expandIosPackageTarget({ signed: true, artifacts: ["b/App.ipa"], ownershipScope: ["ffi"], iosPackage: { project: "ios/App.xcodeproj", projectSpec: "ios/project.yml", scheme: "App", teamId: "ABCDE12345", bundleId: "dev.x.app", ipa: "b/App.ipa", archive: "b/App.xcarchive" } });
    assert.deepEqual(iosOwnershipScope({ repoRoot: root, appRoot: root, target: expanded }), [".rightkit", "ffi", "ios", "swift/Inline", "swift/MeshLink"]);
    assert.equal(iosOwnershipScope({ repoRoot: root, appRoot: root, target: {} }), null);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("ownershipScope rejects absolute, escaping, missing, whole-repo and non-array values", () => {
  const root = fixture();
  try {
    for (const bad of [["/etc"], ["../x"], ["ffi/../../x"], ["nope"], ["."], [""], [3], "ffi"]) assert.throws(() => normalizeScopeRoots(bad, { repoRoot: root }), /ownershipScope/, JSON.stringify(bad));
    assert.throws(() => iosOwnershipScope({ repoRoot: root, appRoot: root, target: target({ ownershipScope: ["nope"] }) }), /not an existing directory/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("a finding outside scope is ignored, inside scope still fails, and verification fails when scope changes", () => {
  const root = fixture();
  try {
    commit(root);
    const whole = createCommittedOwnershipEvidenceForTest(root, options(root, null));
    assert.equal(whole.result.violations.length, 1);
    const scope = [".rightkit", "ios", "swift/MeshLink"];
    const scoped = createCommittedOwnershipEvidenceForTest(root, options(root, scope));
    assert.equal(scoped.ok, true);
    assert.equal(scoped.result.violations.length, 0);
    assert.deepEqual(scoped.evidence.ownershipScope, scope);
    assert.notEqual(scoped.evidence.contentDigest, whole.evidence.contentDigest);
    const same = verifyOwnershipEvidenceForTest(scoped.evidence, root, options(root, scope));
    assert.equal(same.ok, true, same.reason);
    const changed = verifyOwnershipEvidenceForTest(scoped.evidence, root, options(root, [...scope, "ffi"]));
    assert.equal(changed.ok, false);
    assert.match(changed.reason, /ownershipScope changed/);
    assert.match(verifyOwnershipEvidenceForTest(scoped.evidence, root, options(root, null)).reason, /ownershipScope changed/);
    assert.match(verifyOwnershipEvidenceForTest(whole.evidence, root, options(root, scope)).reason, /ownershipScope changed/);
    // the same marker inside scope fails
    const inside = createCommittedOwnershipEvidenceForTest(root, options(root, [...scope, "membrane"]));
    assert.equal(inside.ok, false);
    assert.equal(inside.result.violations.length, 1);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("an exception for a path outside scope is a stale warning, never a failure", () => {
  const root = fixture();
  try {
    writeFileSync(path.join(root, ".rightkit", "ownership-exceptions.json"), JSON.stringify({ exceptions: [{ capability: "sample", package: "sample", path: "membrane", reason: "r", owner: "o", date: "2026-10-19" }] }));
    commit(root);
    const scoped = createCommittedOwnershipEvidenceForTest(root, options(root, [".rightkit", "ios"]));
    assert.equal(scoped.ok, true);
    assert.ok(scoped.result.warnings.some((w) => /stale/.test(w)));
    const wide = createCommittedOwnershipEvidenceForTest(root, options(root, null));
    assert.equal(wide.result.covered.length, 1);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("a coverage gap outside scope is dropped, one inside scope still blocks", () => {
  const root = fixture();
  try {
    writeFileSync(path.join(root, "membrane", "big.c"), "x".repeat(2 * 1024 * 1024 + 10));
    writeFileSync(path.join(root, "ffi", "big.c"), "x".repeat(2 * 1024 * 1024 + 10));
    commit(root);
    assert.equal(createCommittedOwnershipEvidenceForTest(root, options(root, [".rightkit", "ios"])).ok, true);
    const inside = createCommittedOwnershipEvidenceForTest(root, options(root, [".rightkit", "ios", "ffi"]));
    assert.equal(inside.ok, false);
    assert.ok(inside.contentCoverage.some((c) => c.reason === "source-file-over-2MiB" && c.path === "ffi/big.c"));
    assert.ok(!inside.contentCoverage.some((c) => c.path.startsWith("membrane")));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("a scoped workspace member resolves against its ancestor workspace manifest and lockfile", () => {
  const root = fixture();
  try {
    writeFileSync(path.join(root, "Cargo.toml"), '[workspace]\nmembers = ["ffi"]\n[workspace.package]\nedition = "2021"\n');
    writeFileSync(path.join(root, "ffi", "Cargo.toml"), '[package]\nname = "ffi"\nversion = "0.1.0"\nedition.workspace = true\n');
    writeFileSync(path.join(root, "Cargo.lock"), 'version = 3\n\n[[package]]\nname = "ffi"\nversion = "0.1.0"\n');
    commit(root);
    const scoped = createCommittedOwnershipEvidenceForTest(root, options(root, [".rightkit", "ffi"]));
    assert.deepEqual(scoped.contentCoverage, []);
    assert.equal(scoped.ok, true);
    assert.ok(scoped.result.metadata.contentDigests.some((d) => d.path === "Cargo.lock"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("nativeAssembly workspaceRoot and cargo manifest directory are folded into the iOS scope", () => {
  const root = fixture();
  try {
    const scope = (nativeAssembly) => iosOwnershipScope({ repoRoot: root, appRoot: path.join(root, "ios"), target: { iosPackage: { project: "App.xcodeproj", projectSpec: "project.yml", scheme: "App" } }, nativeAssembly });
    assert.deepEqual(scope({ cargoManifest: "../ffi/Cargo.toml", workspaceRoot: "../membrane" }), [".rightkit", "ffi", "ios", "membrane", "swift/Inline", "swift/MeshLink"]);
    assert.deepEqual(scope({ cargoManifest: "../ffi/Cargo.toml" }), [".rightkit", "ffi", "ios", "swift/Inline", "swift/MeshLink"]);
    assert.deepEqual(scope({ cargoManifest: "../Cargo.toml", workspaceRoot: ".." }), [".rightkit", "ios", "swift/Inline", "swift/MeshLink"]);
    assert.throws(() => scope({ workspaceRoot: "../../elsewhere" }), /outside the repository/);
    assert.throws(() => scope({ workspaceRoot: "../missing" }), /not an existing directory/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("the scope is closed under local path dependencies: an unlisted sibling crate is pulled in and scanned", () => {
  const root = fixture();
  try {
    mkdirSync(path.join(root, "sibling", "src"), { recursive: true });
    writeFileSync(path.join(root, "ffi", "Cargo.toml"), '[package]\nname = "ffi"\nversion = "0.1.0"\n[dependencies]\nsibling = { path = "../sibling" }\n');
    writeFileSync(path.join(root, "sibling", "Cargo.toml"), '[package]\nname = "sibling"\nversion = "0.1.0"\n[dependencies]\nmembrane = { path = "../membrane" }\n');
    writeFileSync(path.join(root, "sibling", "src", "lib.rs"), "fn s() { SCOPE_MARKER; }\n");
    writeFileSync(path.join(root, "membrane", "Cargo.toml"), '[package]\nname = "membrane"\nversion = "0.1.0"\n');
    commit(root);
    const scope = [".rightkit", "ffi", "ios"];
    const checked = createCommittedOwnershipEvidenceForTest(root, options(root, scope));
    assert.deepEqual(checked.evidence.ownershipScope, scope);
    assert.deepEqual(checked.evidence.ownershipScopeClosed, [".rightkit", "ffi", "ios", "membrane", "sibling"]);
    assert.ok(checked.result.violations.some((v) => v.file === "sibling/src/lib.rs"));
    assert.ok(checked.result.violations.some((v) => v.file === "membrane/m.rs"));
    assert.equal(checked.ok, false);
    const same = verifyOwnershipEvidenceForTest(checked.evidence, root, options(root, scope));
    assert.ok(same.reason === undefined || !/ownershipScope/.test(same.reason), same.reason);
    assert.match(verifyOwnershipEvidenceForTest({ ...checked.evidence, ownershipScopeClosed: scope }, root, options(root, scope)).reason, /ownershipScopeClosed changed/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("a path dependency that leaves the repository is a blocking coverage gap in a scoped scan", () => {
  const root = fixture();
  try {
    writeFileSync(path.join(root, "ffi", "Cargo.toml"), '[package]\nname = "ffi"\nversion = "0.1.0"\n[dependencies]\nout = { path = "../../outside" }\n');
    commit(root);
    const checked = createCommittedOwnershipEvidenceForTest(root, options(root, [".rightkit", "ffi"]));
    assert.equal(checked.ok, false);
    assert.ok(checked.contentCoverage.some((c) => /local-dependency-outside-repository/.test(c.reason) && c.path === "ffi/Cargo.toml"), JSON.stringify(checked.contentCoverage));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

const NATIVE = { workspaceRoot: "ffi" };
const BIG = "x".repeat(2 * 1024 * 1024 + 10);
const doctorScope = (root, nativeAssembly = NATIVE) => preflightOwnershipScope({ repoRoot: root, appRoot: root, platform: "ios", target: target(), nativeAssembly });

test("doctor ownership passes a 2 MiB source file outside the iOS scope (rightkit#927; written, not run)", () => {
  const root = fixture();
  try {
    writeFileSync(path.join(root, "membrane", "big.c"), BIG);
    commit(root);
    const scope = doctorScope(root);
    assert.ok(!scope.includes("membrane"));
    assert.equal(createCommittedOwnershipEvidenceForTest(root, options(root, scope)).ok, true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("doctor ownership fails a 2 MiB source file inside the iOS scope with source-file-over-2MiB (rightkit#927; written, not run)", () => {
  const root = fixture();
  try {
    writeFileSync(path.join(root, "ffi", "big.c"), BIG);
    commit(root);
    const scope = doctorScope(root);
    assert.ok(scope.includes("ffi"));
    const checked = createCommittedOwnershipEvidenceForTest(root, options(root, scope));
    assert.equal(checked.ok, false);
    assert.ok(checked.contentCoverage.some((c) => c.reason === "source-file-over-2MiB" && c.path === "ffi/big.c"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("doctor and build assemble an identical ownership scope; non-iOS stays whole-repo (rightkit#927; written, not run)", () => {
  const root = fixture();
  try {
    const config = { nativeAssembly: NATIVE };
    const raw = target({ ownershipScope: ["membrane"] });
    const expanded = expandIosPackageTarget({ signed: true, artifacts: ["b/App.ipa"], ownershipScope: ["membrane"], iosPackage: { ...raw.iosPackage, teamId: "ABCDE12345", bundleId: "dev.x.app", ipa: "b/App.ipa", archive: "b/App.xcarchive" } });
    // The doctor passes config.targets[platform] (raw); the build passes its target (raw or expanded): same scope either way.
    const fromDoctor = preflightOwnershipScope({ repoRoot: root, appRoot: root, platform: "ios", target: raw, nativeAssembly: config.nativeAssembly });
    const fromBuild = preflightOwnershipScope({ repoRoot: root, appRoot: root, platform: "ios", target: expanded, nativeAssembly: config.nativeAssembly });
    assert.deepEqual(fromDoctor, fromBuild);
    assert.deepEqual(fromDoctor, iosOwnershipScope({ repoRoot: root, appRoot: root, target: raw, nativeAssembly: config.nativeAssembly }));
    assert.equal(preflightOwnershipScope({ repoRoot: root, appRoot: root, platform: "mac", target: raw, nativeAssembly: config.nativeAssembly }), null);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("doctor and build both route collectPreflight's ownershipScope through the one shared helper (rightkit#927; written, not run)", () => {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  for (const [file, call] of [["release.mjs", /const ownershipScope = preflightOwnershipScope\(\{ repoRoot, appRoot: root, platform, target: projectTarget, nativeAssembly: config\.nativeAssembly \}\);/], ["build-release.mjs", /const ownershipScope = preflightOwnershipScope\(\{ repoRoot, appRoot, platform, target, nativeAssembly: config\?\.nativeAssembly \}\);/]]) {
    const text = readFileSync(path.join(dir, file), "utf8");
    assert.match(text, call, file);
    assert.match(text, /collectPreflight\(\{\s*platform,\s*ownershipScope,/, file);
    if (file === "build-release.mjs") assert.doesNotMatch(text, /iosOwnershipScope\(/, `${file} must not assemble the scope itself`);
  }
});
