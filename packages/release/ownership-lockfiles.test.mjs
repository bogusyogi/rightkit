import assert from "node:assert/strict";
import test from "node:test";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { checkApp, checkCommittedAppForTest, evaluate, scanApp, SCANNER_VERSION } from "./ownership.mjs";
import { canonicalJson, createDraft, keyIdForPublicKey, occurrenceHash, policyEntryDigest, sha256 } from "./ownership-approval.mjs";
import { ownershipVerdict } from "./ownership-verdict.mjs";
import { transitiveDependencies } from "./ownership-lockfiles.mjs";

const catalog = { schema: 1, version: "1", capabilities: [
  { package: "@rightkit/browser", capability: "browser", fingerprints: [{ id: "browser.npm.banned", type: "dependency", ecosystem: "npm", name: "puppeteer-core", tier: "block", policy: "banned" }] },
  { package: "rightkit-browser", capability: "browser", fingerprints: [{ id: "browser.cargo.banned", type: "dependency", ecosystem: "cargo", name: "banned-crate", tier: "block" }] },
  { package: "@rightkit/advice", capability: "advice", fingerprints: [{ id: "advice.npm.library", type: "dependency", ecosystem: "npm", name: "advice", tier: "advise" }] },
] };
function fixture(files, run) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-transitive-"));
  const git = (...args) => { const out = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true }); assert.equal(out.status, 0, out.stderr); return out.stdout.trim(); };
  try {
    git("init", "-q"); git("remote", "add", "origin", "https://github.com/test/transitive.git");
    for (const [rel, text] of Object.entries(files)) { mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); writeFileSync(path.join(root, rel), text); }
    return run(root, git);
  } finally { rmSync(root, { recursive: true, force: true }); }
}
const json = JSON.stringify;
const npmManifest = (deps = { wrapper: "1.0.0" }, name = "app") => json({ name, version: "1.0.0", dependencies: deps });
function npmLock(wrappers = ["wrapper"], version = 3) {
  return json({ lockfileVersion: version, packages: {
    "": { name: "app", version: "1.0.0", dependencies: Object.fromEntries(wrappers.map((name) => [name, "1.0.0"])) },
    ...Object.fromEntries(wrappers.map((name) => [`node_modules/${name}`, { version: "1.0.0", dependencies: { "puppeteer-core": "24.1.0", advice: "1.0.0" } }])),
    "node_modules/puppeteer-core": { version: "24.1.0", resolved: "https://registry.npmjs.org/puppeteer-core/-/puppeteer-core-24.1.0.tgz" },
    "node_modules/advice": { version: "1.0.0" },
    "node_modules/unreachable": { version: "1.0.0" },
  } });
}
const transitive = (root, manifest = catalog) => scanApp(root, manifest, { trust: null }).filter((f) => f.kind === "transitive-dependency");

for (const version of [2, 3]) test(`npm lock v${version}: third-party transitive path, version & source; advise stays manifest-only`, () => {
  fixture({ "package.json": npmManifest(), "package-lock.json": npmLock(["wrapper"], version) }, (root) => {
    const found = transitive(root); assert.equal(found.length, 1);
    assert.deepEqual(found[0].dependencyPath, ["app", "wrapper", "puppeteer-core"]);
    assert.equal(found[0].resolvedVersion, "24.1.0"); assert.equal(found[0].lockfile, "package-lock.json");
    assert.equal(found[0].file, "package-lock.json"); assert.match(found[0].source, /registry.npmjs.org/);
    assert.equal(found[0].occurrenceHash, occurrenceHash(found[0]));
    assert.deepEqual(scanApp(root, catalog, { trust: null }).coverage, []);
  });
});
test("direct banned dependency retains manifest finding & hash without duplicate transitive", () => {
  fixture({ "package.json": npmManifest({ "puppeteer-core": "24.1.0" }), "package-lock.json": npmLock(["puppeteer-core"]) }, (root) => {
    const found = scanApp(root, catalog, { trust: null }); assert.equal(found.length, 1);
    assert.equal(found[0].kind, "dependency"); assert.equal(found[0].file, "package.json"); assert.equal(found[0].resolvedVersion, "24.1.0");
    assert.equal(found[0].occurrenceHash, occurrenceHash(found[0]));
  });
});
test("only catalog owner routes are skipped; mixed routes still report", () => {
  for (const wrappers of [["@rightkit/browser"], ["@rightkit/browser", "wrapper"]]) fixture({ "package.json": npmManifest(Object.fromEntries(wrappers.map((n) => [n, "1.0.0"]))), "package-lock.json": npmLock(wrappers) }, (root) => {
    const found = transitive(root); assert.equal(found.length, wrappers.length - 1);
    if (found.length) assert.deepEqual(found[0].dependencyPath, ["app", "wrapper", "puppeteer-core"]);
  });
});
test("app-declared fake owner cannot suppress transitive findings", () => {
  fixture({ "package.json": npmManifest({ "@rightkit/fake": "1.0.0" }), "package-lock.json": npmLock(["@rightkit/fake"]), "rightkit-ownership.json": json({ capabilities: [{ package: "@rightkit/fake", capability: "browser", fingerprints: [] }] }) }, (root) => {
    assert.deepEqual(transitive(root)[0].dependencyPath, ["app", "@rightkit/fake", "puppeteer-core"]);
  });
});
test("catalog internal owners also form authenticated boundaries", () => {
  fixture({ "package.json": npmManifest(), "package-lock.json": npmLock() }, (root) => assert.equal(transitive(root, { ...catalog, internalOwners: [{ package: "wrapper" }] }).length, 0));
});
test("app naming itself as catalog owner grants no skip", () => {
  fixture({ "package.json": npmManifest(undefined, "@rightkit/browser"), "package-lock.json": npmLock() }, (root) => assert.equal(transitive(root).length, 1));
  const lock = JSON.parse(npmLock(["@rightkit/browser"]));
  lock.packages["node_modules/@rightkit/browser"].resolved = "file:vendor/browser";
  fixture({ "package.json": npmManifest({ "@rightkit/browser": "1.0.0" }), "package-lock.json": json(lock) }, (root) => assert.equal(transitive(root).length, 1));
});

function pnpmLock(version) {
  const key = version === 6 ? "/" : "", body = `  '${key}wrapper@1.0.0':\n    dependencies:\n      puppeteer-core: 24.1.0\n      advice: 1.0.0\n  '${key}puppeteer-core@24.1.0': {}\n  '${key}advice@1.0.0': {}\n`;
  return `lockfileVersion: '${version}.0'\nimporters:\n  .:\n    dependencies:\n      wrapper:\n        specifier: 1.0.0\n        version: 1.0.0\npackages:\n${version === 6 ? body : "  wrapper@1.0.0: {resolution: {integrity: sha512-test}}\n  puppeteer-core@24.1.0: {}\n  advice@1.0.0: {}\nsnapshots:\n" + body}`;
}
for (const version of [6, 9]) test(`pnpm v${version}: importers & resolved package graph`, () => {
  fixture({ "package.json": npmManifest(), "pnpm-lock.yaml": pnpmLock(version) }, (root) => {
    const found = transitive(root); assert.equal(found.length, 1);
    assert.deepEqual(found[0].dependencyPath, ["app", "wrapper", "puppeteer-core"]);
    assert.equal(found[0].resolvedVersion, "24.1.0"); assert.deepEqual(scanApp(root, catalog, { trust: null }).coverage, []);
  });
});
test("pnpm workspace importers, links, aliases & peer-suffixed snapshots resolve from own members", () => {
  const lock = `lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      child:\n        version: link:apps/child\n  apps/child:\n    dependencies:\n      browser-alias:\n        version: wrapper@1.0.0(peer@2.0.0)\npackages:\n  wrapper@1.0.0: {}\n  puppeteer-core@24.1.0: {}\nsnapshots:\n  'wrapper@1.0.0(peer@2.0.0)':\n    dependencies:\n      puppeteer-core: 24.1.0\n  puppeteer-core@24.1.0: {}\n`;
  fixture({ "package.json": npmManifest({ child: "workspace:*" }), "apps/child/package.json": npmManifest({ "browser-alias": "npm:wrapper@1.0.0" }, "child"), "pnpm-lock.yaml": lock }, (root) => {
    const found = transitive(root);
    assert.deepEqual(found.map((f) => f.dependencyPath), [["child", "wrapper", "puppeteer-core"], ["app", "child", "wrapper", "puppeteer-core"]]);
    assert.ok(found.every((f) => f.lockfile === "pnpm-lock.yaml"));
    assert.ok(!scanApp(root, catalog, { trust: null }).coverage.some((c) => c.reason.startsWith("lockfile-")));
  });
});
test("npm nested installs resolve nearest package version & workspace links", () => {
  const lock = JSON.parse(npmLock());
  lock.packages["node_modules/puppeteer-core"].version = "23.0.0";
  lock.packages["node_modules/wrapper/node_modules/puppeteer-core"] = { version: "24.1.0" };
  fixture({ "package.json": npmManifest(), "package-lock.json": json(lock) }, (root) => assert.equal(transitive(root)[0].resolvedVersion, "24.1.0"));
  lock.packages[""] = { name: "app", dependencies: { child: "file:apps/child" } };
  lock.packages["node_modules/child"] = { link: true, resolved: "apps/child" };
  lock.packages["apps/child"] = { name: "child", version: "1.0.0", dependencies: { wrapper: "1.0.0" } };
  fixture({ "package.json": npmManifest({ child: "file:apps/child" }), "apps/child/package.json": npmManifest(undefined, "child"), "package-lock.json": json(lock) }, (root) => {
    assert.ok(transitive(root).some((f) => f.dependencyPath.join("/") === "app/child/wrapper/puppeteer-core"));
  });
});

const cargoManifest = '[package]\nname="app"\nversion="1.0.0"\n[dependencies]\nwrapper="1.0.0"\n';
function cargoLock(wrapper = "wrapper") {
  return `version=4\n[[package]]\nname="app"\nversion="1.0.0"\ndependencies=["${wrapper}"]\n[[package]]\nname="${wrapper}"\nversion="1.0.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\ndependencies=["banned-crate 2.0.0"]\n[[package]]\nname="banned-crate"\nversion="2.0.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n[[package]]\nname="banned-crate"\nversion="1.0.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n`;
}
test("Cargo package references disambiguate versions & catalog owner routes skip", () => {
  fixture({ "Cargo.toml": cargoManifest, "Cargo.lock": cargoLock() }, (root) => {
    const found = transitive(root); assert.equal(found.length, 1); assert.equal(found[0].resolvedVersion, "2.0.0");
    assert.deepEqual(found[0].dependencyPath, ["app", "wrapper", "banned-crate"]); assert.deepEqual(scanApp(root, catalog, { trust: null }).coverage, []);
  });
  fixture({ "Cargo.toml": cargoManifest.replace("wrapper", "rightkit-browser"), "Cargo.lock": cargoLock("rightkit-browser") }, (root) => assert.equal(transitive(root).length, 0));
});
test("Cargo workspace member uses ancestor lock & does not start at third-party packages", () => {
  fixture({ "Cargo.toml": '[workspace]\nmembers=["engine"]\n', "engine/Cargo.toml": cargoManifest, "Cargo.lock": cargoLock() }, (root) => {
    assert.deepEqual(transitive(root)[0].dependencyPath, ["app", "wrapper", "banned-crate"]);
    assert.ok(scanApp(root, catalog, { trust: null }).metadata.contentDigests.some((d) => d.path === "Cargo.lock"));
  });
});

for (const [name, content, reason] of [
  ["package-lock.json", "{", "lockfile-parse-failed"],
  ["Cargo.lock", "[[package]\n", "lockfile-parse-failed"],
  ["pnpm-lock.yaml", "lockfileVersion: '9.0'\nimporters: [broken\n", "lockfile-parse-failed"],
  ["package-lock.json", '{"lockfileVersion":1,"dependencies":{}}', "lockfile-unsupported-format"],
  ["pnpm-lock.yaml", "lockfileVersion: '5.4'\nimporters: {}\npackages: {}\n", "lockfile-unsupported-format"],
  ["yarn.lock", '"wrapper@1.0.0":\n  version "1.0.0"\n', "lockfile-unsupported-format"],
]) test(`${name} invalid/unsupported graph records typed incomplete coverage: ${reason}`, () => {
  fixture({ [name === "Cargo.lock" ? "Cargo.toml" : "package.json"]: name === "Cargo.lock" ? cargoManifest : npmManifest(), [name]: content }, (root) => {
    const found = scanApp(root, catalog, { trust: null });
    assert.ok(found.coverage.some((c) => c.path === name && c.status === "unscanned" && c.reason === reason), json(found.coverage));
  });
});
test("missing lockfiles & unresolved graph references fail coverage", () => {
  fixture({ "package.json": npmManifest(), "Cargo.toml": cargoManifest }, (root) => assert.equal(scanApp(root, catalog, { trust: null }).coverage.filter((c) => c.reason === "lockfile-missing").length, 2));
  const lock = JSON.parse(npmLock()); delete lock.packages["node_modules/puppeteer-core"];
  fixture({ "package.json": npmManifest(), "package-lock.json": json(lock) }, (root) => assert.ok(scanApp(root, catalog, { trust: null }).coverage.some((c) => c.reason === "lockfile-unresolved-dependency")));
});
test("an unresolved sibling importer keeps valid-root findings & records incomplete coverage", () => {
  fixture({ "package.json": npmManifest(), "vendor/package.json": npmManifest(undefined, "vendor"), "pnpm-lock.yaml": pnpmLock(9) }, (root) => {
    assert.equal(transitive(root).length, 1);
    assert.ok(scanApp(root, catalog, { trust: null }).coverage.some((c) => c.reason === "lockfile-root-unresolved" && c.detail === "vendor/package.json"));
  });
});
test("ignored locks cannot silently escape scanner input coverage", () => {
  fixture({ ".gitignore": "package-lock.json\n", "package.json": npmManifest(), "package-lock.json": npmLock() }, (root) => {
    const found = scanApp(root, catalog, { trust: null });
    assert.equal(found.filter((f) => f.kind === "transitive-dependency").length, 1);
    assert.ok(found.coverage.some((c) => c.path === "package-lock.json" && c.reason === "lockfile-not-in-inventory"));
  });
});
test("cycles terminate & distinct dependency routes remain separate with shortest first", () => {
  const lock = JSON.parse(npmLock(["a", "b"])); lock.packages["node_modules/a"].dependencies.b = "1.0.0"; lock.packages["node_modules/b"].dependencies.a = "1.0.0";
  fixture({ "package.json": npmManifest({ a: "1.0.0", b: "1.0.0" }), "package-lock.json": json(lock) }, (root) => {
    const found = transitive(root); assert.equal(found.length, 4);
    assert.equal(new Set(found.map(occurrenceHash)).size, 4); assert.equal(found[0].dependencyPath.length, 3);
  });
});
test("a plain exception entry covers the capability; route A and B hash differently (name/version/source/path bind hashes) (#262)", () => {
  fixture({ "package.json": npmManifest({ a: "1.0.0", b: "1.0.0" }), "package-lock.json": npmLock(["a", "b"]) }, (root) => {
    const found = transitive(root), [a, b] = found, identity = "https://github.com/test/transitive";
    assert.notEqual(occurrenceHash(a), occurrenceHash(b));
    for (const change of [{ resolvedVersion: "25.0.0" }, { source: "git+https://example.com/browser.git#rev" }, { name: "changed", dependencyPath: [...a.dependencyPath.slice(0, -1), "changed"] }]) assert.notEqual(occurrenceHash(a), occurrenceHash({ ...a, ...change }));
    const record = { capability: a.capability, package: a.package, fingerprint: a.fingerprint, path: a.file, reason: "migration", owner: "app", date: "2026-10-17", index: 0 };
    const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" }), mac = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
    const trust = { schema: 1, minimumScannerVersion: "2", minimumCatalogVersion: "1", revokedKeyIds: [], provenance: [], repositories: [{ repositoryIdentity: identity, roots: ["."], visibility: "public" }], verifiedTrackingEvidence: [], hosts: { windows: { keyId: keyIdForPublicKey("windows", pair.publicKey), publicKeyPem: pair.publicKey.export({ type: "spki", format: "pem" }) }, macos: { keyId: keyIdForPublicKey("macos", mac.publicKey), publicKeyPem: mac.publicKey.export({ type: "spki", format: "pem" }) } } };
    const options = { manifest: catalog, repositoryIdentity: identity, scannerVersion: SCANNER_VERSION, catalogVersion: "1" };
    const result = evaluate(found, [record], options); assert.deepEqual(result.problems, []); assert.deepEqual(result.covered.map((f) => f.dependencyPath), found.filter((f) => f.fingerprint === a.fingerprint && f.file === a.file).map((f) => f.dependencyPath));
    const narrowed = evaluate(found, [{ ...record, fingerprint: "unrelated.fingerprint" }], options); assert.equal(narrowed.covered.length, 0); assert.ok(narrowed.warnings.some((w) => /stale/.test(w)));
    const checked = JSON.parse(json(checkApp(root, catalog, { trust })));
    assert.equal(ownershipVerdict(checked, trust).code, "RK_OWNERSHIP_VIOLATION");
    writeFileSync(path.join(root, "package-lock.json"), "{");
    assert.equal(ownershipVerdict(JSON.parse(json(checkApp(root, catalog, { trust }))), trust).code, "RK_OWNERSHIP_INCOMPLETE_COVERAGE");
  });
});
test("committed scanner uses lockfile blobs & ignores worktree tampering", () => {
  fixture({ "package.json": npmManifest(), "package-lock.json": npmLock() }, (root, git) => {
    git("config", "user.name", "test"); git("config", "user.email", "test@example.invalid"); git("add", "."); git("commit", "-qm", "fixture");
    writeFileSync(path.join(root, "package-lock.json"), "{}");
    const result = checkCommittedAppForTest(root, catalog, { trust: { enrolled: false, pending: true, trust: null } });
    assert.deepEqual(result.findings[0].dependencyPath, ["app", "wrapper", "puppeteer-core"]);
    assert.deepEqual(result.coverage, []);
  });
});

// Workspace membership decides which manifests must resolve to a lock root.
function membershipScan(files) {
  const entries = Object.entries(files).map(([rel, text]) => ({ rel, bytes: Buffer.from(text) }));
  const manifests = entries.filter((file) => /(?:^|\/)(?:Cargo\.toml|package\.json)$/.test(file.rel));
  const coverage = [], digests = [];
  const result = transitiveDependencies("/unused", { files: entries, committed: true }, manifests, new Set(), coverage, digests, []);
  return { coverage, info: result.info };
}
const crateManifest = (name) => `[package]\nname="${name}"\nversion="1.0.0"\n`;
const cargoWorkspaceFiles = {
  "Cargo.toml": '[workspace]\nmembers=["crates/*"]\nexclude=["crates/skip"]\n',
  "Cargo.lock": 'version=4\n[[package]]\nname="a"\nversion="1.0.0"\n',
  "crates/a/Cargo.toml": crateManifest("a"),
  "crates/b/Cargo.toml": crateManifest("b"),
  "crates/skip/Cargo.toml": crateManifest("skip"),
  "fixtures/f/Cargo.toml": crateManifest("fixture"),
};
const unresolvedGaps = (coverage) => coverage.filter((c) => c.reason === "lockfile-root-unresolved").map((c) => c.detail).sort();
const outsideInfo = (info) => info.filter((i) => i.reason === "manifest-outside-workspace").map((i) => i.detail).sort();

test("Cargo: a workspace member present in the lock resolves with no gap and no info", () => {
  const { coverage, info } = membershipScan(cargoWorkspaceFiles);
  assert.ok(!unresolvedGaps(coverage).includes("crates/a/Cargo.toml"));
  assert.ok(!outsideInfo(info).includes("crates/a/Cargo.toml"));
});
test("Cargo: a manifest outside the workspace is info, never a coverage gap", () => {
  const { coverage, info } = membershipScan(cargoWorkspaceFiles);
  assert.ok(outsideInfo(info).includes("fixtures/f/Cargo.toml"));
  assert.ok(!unresolvedGaps(coverage).includes("fixtures/f/Cargo.toml"));
  assert.ok(info.every((entry) => entry.status === "info"));
});
test("Cargo: a path under workspace.exclude is info, never a coverage gap", () => {
  const { coverage, info } = membershipScan(cargoWorkspaceFiles);
  assert.ok(outsideInfo(info).includes("crates/skip/Cargo.toml"));
  assert.ok(!unresolvedGaps(coverage).includes("crates/skip/Cargo.toml"));
});
test("Cargo: a member missing from the lock is still a coverage gap", () => {
  const { coverage, info } = membershipScan(cargoWorkspaceFiles);
  assert.deepEqual(unresolvedGaps(coverage), ["crates/b/Cargo.toml"]);
  assert.ok(!outsideInfo(info).includes("crates/b/Cargo.toml"));
});
test("Cargo: a path dependency inside the workspace directory is a member, so a missing lock entry stays a gap", () => {
  const { coverage } = membershipScan({
    "Cargo.toml": '[package]\nname="root"\nversion="1.0.0"\n[dependencies]\nhelper={path="tools/helper"}\n[workspace]\nmembers=[]\n',
    "Cargo.lock": 'version=4\n[[package]]\nname="root"\nversion="1.0.0"\n',
    "tools/helper/Cargo.toml": crateManifest("helper"),
  });
  assert.deepEqual(unresolvedGaps(coverage), ["tools/helper/Cargo.toml"]);
});
test("Cargo: with no workspace definition every manifest stays a root and an unresolved one is a gap", () => {
  const { coverage, info } = membershipScan({ "Cargo.toml": crateManifest("a"), "Cargo.lock": 'version=4\n[[package]]\nname="a"\nversion="1.0.0"\n', "sub/Cargo.toml": crateManifest("sub") });
  assert.deepEqual(unresolvedGaps(coverage), ["sub/Cargo.toml"]);
  assert.deepEqual(info, []);
});

const pnpmWorkspaceFiles = {
  "pnpm-workspace.yaml": "packages:\n  - packages/*\n  # parked\n  - '!packages/parked'\n\noverrides:\n  x: 1.0.0\n",
  "pnpm-lock.yaml": "lockfileVersion: '9.0'\nimporters:\n  .: {}\n  packages/a: {}\npackages: {}\nsnapshots: {}\n",
  "package.json": json({ name: "root" }),
  "packages/a/package.json": json({ name: "a" }),
  "packages/missing/package.json": json({ name: "missing" }),
  "packages/parked/package.json": json({ name: "parked" }),
  "tools/x/package.json": json({ name: "x" }),
};
test("pnpm: importers resolve, negated and unlisted manifests are info, a member missing from the lock is a gap", () => {
  const { coverage, info } = membershipScan(pnpmWorkspaceFiles);
  assert.deepEqual(unresolvedGaps(coverage), ["packages/missing/package.json"]);
  assert.deepEqual(outsideInfo(info), ["packages/parked/package.json", "tools/x/package.json"]);
});
test("pnpm: without pnpm-workspace.yaml every manifest stays a root and an unresolved one is a gap", () => {
  const { "pnpm-workspace.yaml": _omitted, ...files } = pnpmWorkspaceFiles;
  const { coverage, info } = membershipScan(files);
  assert.deepEqual(unresolvedGaps(coverage), ["packages/missing/package.json", "packages/parked/package.json", "tools/x/package.json"]);
  assert.deepEqual(info, []);
});
