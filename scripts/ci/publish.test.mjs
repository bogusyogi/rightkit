import assert from "node:assert/strict";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildCrateUploadMetadata, commandPack, commandVerify, crateIndexPath, crateSourceProblems, crateWorkspaceFromMetadata, parseAllowList, parseUnitNames, planPublication, readNpmWorkspace, satisfies } from "./publish.mjs";
import { buildCratePublishBody, checkNpmTarball, CRATES_PUBLISH_URL, childEnv, executeUpload, NPM_REGISTRY_URL, repositoryProblems } from "./upload.mjs";
import { parseYaml } from "./workflow-yaml.mjs";

const SHA = "a".repeat(40);
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const tmp = (label) => fs.mkdtempSync(path.join(os.tmpdir(), `rk-${label}-`));

// ---- requirements -----------------------------------------------------------------------------

test("Cargo requirements: bare is caret, 0.x narrows, ranges and exact pins hold", () => {
  assert.equal(satisfies("0.3.4", "0.3", "cargo"), true);
  assert.equal(satisfies("0.4.0", "0.3", "cargo"), false);
  assert.equal(satisfies("1.9.0", "1.2", "cargo"), true);
  assert.equal(satisfies("2.0.0", "1.2", "cargo"), false);
  assert.equal(satisfies("0.0.3", "0.0.3", "cargo"), true);
  assert.equal(satisfies("0.0.4", "0.0.3", "cargo"), false);
  assert.equal(satisfies("0.1.0", "=0.1.0", "cargo"), true);
  assert.equal(satisfies("0.1.1", "=0.1.0", "cargo"), false);
  assert.equal(satisfies("1.4.0", ">=1.2, <1.5", "cargo"), true);
  assert.equal(satisfies("1.5.0", ">=1.2, <1.5", "cargo"), false);
});

test("Cargo requirements: wildcard, tilde, comparison and comma-list boundaries follow Cargo", () => {
  const table = [
    // wildcard: 1.2.* is >=1.2.0, <1.3.0 (not the caret of 1.2)
    ["1.2.0", "1.2.*", true], ["1.2.9", "1.2.*", true], ["1.3.0", "1.2.*", false], ["1.9.0", "1.2.*", false], ["1.1.9", "1.2.*", false],
    ["1.0.0", "1.*", true], ["1.99.0", "1.*", true], ["2.0.0", "1.*", false], ["0.9.0", "1.*", false],
    ["0.0.0", "*", true], ["9.9.9", "*", true], ["1.2.9", "1.2.x", true], ["1.3.0", "1.2.X", false],
    // tilde
    ["1.2.3", "~1.2.3", true], ["1.2.2", "~1.2.3", false], ["1.2.9", "~1.2.3", true], ["1.3.0", "~1.2.3", false],
    ["1.2.9", "~1.2", true], ["1.3.0", "~1.2", false], ["1.9.9", "~1", true], ["2.0.0", "~1", false],
    // caret
    ["0.2.9", "^0.2", true], ["0.3.0", "^0.2", false], ["0.0.9", "^0.0", true], ["0.1.0", "^0.0", false],
    ["0.9.9", "^0", true], ["1.0.0", "^0", false], ["1.9.9", "^1.2.3", true], ["2.0.0", "^1.2.3", false], ["0.0.3", "^0.0.3", true], ["0.0.4", "^0.0.3", false],
    // exact and partial equality
    ["1.2.9", "=1.2", true], ["1.3.0", "=1.2", false], ["1.9.9", "=1", true], ["2.0.0", "=1", false],
    // comparison: partial versions widen the way Cargo's semver does
    ["1.2.9", ">1.2", false], ["1.3.0", ">1.2", true], ["1.9.9", ">1", false], ["2.0.0", ">1", true],
    ["1.1.9", "<1.2", true], ["1.2.0", "<1.2", false], ["1.2.9", "<=1.2", true], ["1.3.0", "<=1.2", false],
    ["1.2.0", ">=1.2", true], ["1.1.9", ">=1.2", false], ["1.2.3", ">=1.2.3", true], ["1.2.3", ">1.2.3", false],
    // comma lists, with and without spaces, and operators followed by a space
    ["1.2.5", ">=1.2.0, <1.2.5", false], ["1.2.4", ">=1.2.0,<1.2.5", true], ["1.2.4", ">= 1.2.0 , < 1.2.5", true], ["1.0.0", ">=1.2.0, <1.2.5", false],
    ["1.4.0", ">=1, <1.5, ~1.4", true], ["1.5.0", ">=1, <1.5, ~1.4", false],
  ];
  for (const [version, requirement, expected] of table) assert.equal(satisfies(version, requirement, "cargo"), expected, `${version} against ${requirement}`);
  for (const bad of ["1.*.3", "1.0.0-beta.1", "1.0 || 2.0", "^", ">=", "1.2.3.4"]) assert.throws(() => satisfies("1.0.0", bad, "cargo"), /unsupported/, bad);
  assert.equal(satisfies("1.0.0-beta.1", "*", "cargo"), false);
});

test("npm requirements: bare is exact, ranges join with spaces and ||", () => {
  assert.equal(satisfies("0.1.6", "0.1.6", "npm"), true);
  assert.equal(satisfies("0.1.7", "0.1.6", "npm"), false);
  assert.equal(satisfies("0.2.5", ">=0.2.0 <1", "npm"), true);
  assert.equal(satisfies("1.0.0", ">=0.2.0 <1", "npm"), false);
  assert.equal(satisfies("0.2.5", "^0.2.5", "npm"), true);
  assert.equal(satisfies("0.3.0", "^0.2.5", "npm"), false);
  assert.equal(satisfies("2.1.0", "^1.0.0 || ^2.0.0", "npm"), true);
  assert.equal(satisfies("1.9.0", "1.2.x", "npm"), false);
  assert.equal(satisfies("1.2.7", "1.2.x", "npm"), true);
  assert.equal(satisfies("0.1.0-beta.1", "*", "npm"), false);
  assert.throws(() => satisfies("1.0.0", "1.0.0-beta.1", "npm"), /unsupported/);
});

test("unit names are matched strictly", () => {
  assert.deepEqual(parseUnitNames("rightkit-fs  rightkit-jobs rightkit-fs", "crates"), ["rightkit-fs", "rightkit-jobs"]);
  assert.deepEqual(parseUnitNames("@rightkit/logs", "npm"), ["@rightkit/logs"]);
  for (const bad of ["", "  ", "rightkit-fs;rm", "$(id)", "Rightkit", "../x", "a`b`"]) assert.throws(() => parseUnitNames(bad, "crates"), /empty|valid/);
  assert.throws(() => parseUnitNames("@rightkit/logs --tag evil", "npm"), /valid/);
  assert.throws(() => parseUnitNames("a ".repeat(100), "crates"), /limit/);
  assert.equal(crateIndexPath("rightkit-fs"), "ri/gh/rightkit-fs");
  assert.equal(crateIndexPath("ab"), "2/ab");
  assert.deepEqual(parseAllowList("rightkit-a", "crates", ["rightkit-a", "rightkit-b"], "x"), ["rightkit-a"]);
  assert.throws(() => parseAllowList("rightkit-zzz", "crates", ["rightkit-a"], "allow_host_exclusion"), /allow_host_exclusion names "rightkit-zzz"/);
});

// ---- Cargo metadata -> workspace (findings 7 and 8) -------------------------------------------

const metaDep = (name, req, extra = {}) => ({ name, req, kind: null, optional: false, source: "registry+https://github.com/rust-lang/crates.io-index", rename: null, ...extra });
const metaPkg = (name, version, dependencies = [], extra = {}) => ({ id: `path+file:///ws/crates/${name}#${version}`, name, version, manifest_path: `/ws/crates/${name}/Cargo.toml`, publish: null, dependencies, ...extra });

test("cargo metadata is the dependency source: quoted keys, inherited and renamed dependencies, targets and dev edges all arrive", () => {
  const meta = {
    workspace_root: "/ws",
    packages: [
      metaPkg("rightkit-top", "0.3.0", [
        metaDep("rightkit-base", "^0.2", { source: null }), // `"rightkit-base" = { path = ".." , version = "0.2" }`: a quoted key
        metaDep("rightkit-inherited", "=0.1.0", { source: null }), // `rightkit-inherited.workspace = true`, resolved by Cargo
        metaDep("rightkit-real", "0.4", { source: null, rename: "alias" }), // `alias = { package = "rightkit-real", ... }`
        metaDep("rightkit-win", "0.1", { source: null, optional: true }),
        metaDep("rightkit-devonly", "*", { source: null, kind: "dev" }),
        metaDep("serde", "^1"),
      ]),
      metaPkg("rightkit-base", "0.2.0"), metaPkg("rightkit-inherited", "0.1.0"), metaPkg("rightkit-real", "0.4.2"), metaPkg("rightkit-win", "0.1.0"), metaPkg("rightkit-devonly", "0.1.0"),
      metaPkg("rightkit-private", "0.1.0", [], { publish: [] }),
      metaPkg("rightkit-other-registry", "0.1.0", [], { publish: ["corp"] }),
      metaPkg("rightkit-explicit", "0.1.0", [], { publish: ["crates-io"] }),
    ],
  };
  meta.workspace_members = meta.packages.map((p) => p.id);
  const ws = crateWorkspaceFromMetadata(meta);
  const top = ws.get("rightkit-top");
  assert.equal(top.dir, "crates/rightkit-top");
  const dep = (name) => top.deps.find((d) => d.name === name);
  assert.equal(dep("rightkit-base").spec, "^0.2");
  assert.equal(dep("rightkit-inherited").spec, "=0.1.0");
  assert.equal(dep("rightkit-real").rename, "alias");
  assert.equal(dep("rightkit-real").spec, "0.4");
  assert.equal(dep("rightkit-win").optional, true);
  assert.equal(dep("rightkit-devonly").dev, true);
  assert.equal(dep("rightkit-devonly").spec, null); // path-only dev-dependency
  assert.equal(dep("serde").path, false);
  assert.equal(ws.get("rightkit-private").private, true);
  assert.equal(ws.get("rightkit-other-registry").private, true);
  assert.equal(ws.get("rightkit-explicit").private, false);
  assert.throws(() => crateWorkspaceFromMetadata({ packages: [] }), /no packages/);
  assert.throws(() => crateWorkspaceFromMetadata({ workspace_root: "/ws", packages: [{ ...metaPkg("x", "1.0.0"), dependencies: [{ name: "y" }] }] }), /without a name or requirement/);
});

// ---- planning against a fixture workspace and a fake registry ---------------------------------

function fixture(redKeys = {}, rust = { "rightkit-win": { os: ["windows"] }, "rightkit-maconly": { os: ["macos"] } }) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-publish-test-"));
  fs.mkdirSync(path.join(root, "ci"), { recursive: true });
  fs.writeFileSync(path.join(root, "ci", "known-red.json"), JSON.stringify({ macos: redKeys, windows: {} }));
  fs.writeFileSync(path.join(root, "ci", "gate.config.json"), JSON.stringify({ rust }));
  return root;
}
const crate = (name, version, deps = [], extra = {}) => [name, { name, version, dir: `crates/${name}`, private: false, deps, ...extra }];
const dep = (name, spec, extra = {}) => ({ name, spec, dev: false, path: true, optional: false, ...extra });
function registry(entries) {
  return async (url) => {
    const hit = Object.entries(entries).find(([name]) => url.endsWith(`/${name}`));
    if (!hit) return { status: 404, ok: false, text: async () => "" };
    return { status: 200, ok: true, text: async () => hit[1].map((v) => JSON.stringify({ vers: v.vers, yanked: v.yanked === true, cksum: "ab" })).join("\n") };
  };
}

test("plan orders dependencies first and skips versions already published", async () => {
  const workspace = new Map([
    crate("rightkit-a", "0.1.0"),
    crate("rightkit-b", "0.2.0", [dep("rightkit-a", "0.1.0")]),
    crate("rightkit-c", "0.3.0", [dep("rightkit-b", "0.2")]),
    crate("rightkit-done", "1.0.0"),
  ]);
  const plan = await planPublication({ registry: "crates", unitsText: "rightkit-c rightkit-b rightkit-a rightkit-done", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-done": [{ vers: "1.0.0" }] }) });
  assert.deepEqual(plan.units.map((u) => u.name), ["rightkit-a", "rightkit-b", "rightkit-c"]);
  assert.deepEqual(plan.skipped, [{ name: "rightkit-done", version: "1.0.0" }]);
});

test("plan orders a renamed or inherited dependency edge: the dependent requested first cannot publish first", async () => {
  // metadata reports the real package name for `alias = { package = "rightkit-real" }`; the edge must use it.
  const workspace = new Map([
    crate("rightkit-top", "0.3.0", [dep("rightkit-real", "0.4", { rename: "alias" }), dep("rightkit-inherited", "=0.1.0")]),
    crate("rightkit-real", "0.4.2"),
    crate("rightkit-inherited", "0.1.0"),
  ]);
  const plan = await planPublication({ registry: "crates", unitsText: "rightkit-top rightkit-real rightkit-inherited", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) });
  assert.deepEqual(plan.units.map((u) => u.name), ["rightkit-real", "rightkit-inherited", "rightkit-top"]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-top", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /rightkit-real 0\.4 is not on crates\.io/);
});

test("plan applies Cargo wildcard semantics: 1.2.* does not accept 1.9.0", async () => {
  const workspace = (depVersion) => new Map([crate("rightkit-lib", depVersion), crate("rightkit-app", "0.1.0", [dep("rightkit-lib", "1.2.*")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-app rightkit-lib", dryRun: true, root: fixture(), workspace: workspace("1.9.0"), fetchImpl: registry({}) }), /rightkit-lib 1\.2\.\* is not on crates\.io and this request's 1\.9\.0 does not meet it/);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-app", dryRun: true, root: fixture(), workspace: workspace("1.9.0"), fetchImpl: registry({ "rightkit-lib": [{ vers: "1.9.0" }] }) }), /not on crates\.io/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-app rightkit-lib", dryRun: true, root: fixture(), workspace: workspace("1.2.7"), fetchImpl: registry({}) });
  assert.deepEqual(ok.units.map((u) => u.name), ["rightkit-lib", "rightkit-app"]);
});

test("plan refuses a dependency that is neither on the registry nor in the request", async () => {
  const workspace = new Map([crate("rightkit-a", "0.1.0"), crate("rightkit-b", "0.2.0", [dep("rightkit-a", "0.1.0")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /rightkit-a 0\.1\.0 is not on crates\.io/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-a": [{ vers: "0.1.5" }] }) });
  assert.deepEqual(ok.units.map((u) => u.name), ["rightkit-b"]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-a": [{ vers: "0.1.5", yanked: true }] }) }), /not on crates\.io/);
});

test("plan refuses private, publish=false, known-red, windows-only, path-only, git and unknown units", async () => {
  const workspace = new Map([
    crate("rightkit-private", "0.1.0", [], { private: true }),
    crate("rightkit-red", "0.1.0"),
    crate("rightkit-win", "0.1.0"),
    crate("rightkit-pathonly", "0.1.0", [dep("rightkit-red", null)]),
    crate("rightkit-pre", "0.1.0-beta.1"),
    crate("rightkit-git", "0.1.0", [dep("rightkit-red", "0.1", { source: "git+https://example.invalid/x" })]),
  ]);
  const root = fixture({ "rust:rightkit-red": "defect" });
  for (const [unit, pattern] of [
    ["rightkit-private", /publish = false/], ["rightkit-red", /known-red/], ["rightkit-win", /packed on the macOS runner/],
    ["rightkit-pathonly", /no version requirement/], ["rightkit-pre", /plain x\.y\.z/], ["rightkit-missing", /not a crate/], ["rightkit-git", /git dependency/],
  ]) {
    await assert.rejects(planPublication({ registry: "crates", unitsText: unit, dryRun: true, root, workspace, fetchImpl: registry({ "rightkit-red": [{ vers: "0.1.0" }] }) }), pattern, unit);
  }
});

test("a unit a host would skip, or an npm package with no tests, needs an explicit allow input", async () => {
  const workspace = new Map([crate("rightkit-maconly", "0.1.0"), crate("rightkit-plain", "0.1.0")]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-maconly", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /Windows verification would skip it; name it in allow_host_exclusion/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-maconly rightkit-plain", allowHostExclusionText: "rightkit-maconly", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) });
  assert.deepEqual(ok.allowHostExclusion, ["rightkit-maconly"]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-plain", allowHostExclusionText: "rightkit-plain", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /which no host excludes/);
  const pkg = (name, hasTest) => [name, { name, version: "0.1.0", dir: `packages/${name.split("/")[1]}`, private: false, hasTest, deps: [] }];
  const npm = new Map([pkg("@rightkit/ax", false), pkg("@rightkit/ui", true)]);
  const fetchImpl = async () => ({ status: 404, ok: false, text: async () => "" });
  await assert.rejects(planPublication({ registry: "npm", unitsText: "@rightkit/ax @rightkit/ui", dryRun: true, root: fixture(), workspace: npm, fetchImpl }), /@rightkit\/ax: package\.json has no test script/);
  const allowed = await planPublication({ registry: "npm", unitsText: "@rightkit/ax @rightkit/ui", allowNoTestText: "@rightkit/ax", dryRun: true, root: fixture(), workspace: npm, fetchImpl });
  assert.deepEqual(allowed.allowNoTest, ["@rightkit/ax"]);
  await assert.rejects(planPublication({ registry: "npm", unitsText: "@rightkit/ui", allowNoTestText: "@rightkit/ui", dryRun: true, root: fixture(), workspace: npm, fetchImpl }), /has a test script/);
});

test("plan detects dependency cycles and npm workspace specifiers", async () => {
  const cyc = new Map([crate("rightkit-a", "0.1.0", [dep("rightkit-b", "0.1.0")]), crate("rightkit-b", "0.1.0", [dep("rightkit-a", "0.1.0")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-a rightkit-b", dryRun: true, root: fixture(), workspace: cyc, fetchImpl: registry({}) }), /cycle/);
  const pkg = (name, version, deps = [], extra = {}) => [name, { name, version, dir: `packages/${name.split("/")[1]}`, private: false, hasTest: true, deps, ...extra }];
  const npm = new Map([
    pkg("@rightkit/base", "0.2.0"),
    pkg("@rightkit/top", "0.1.0", [{ name: "@rightkit/base", spec: "workspace:*", dev: false }, { name: "@rightkit/peer", spec: ">=0.1.0 <1", dev: false }]),
    pkg("@rightkit/peer", "0.1.0"),
  ]);
  const fetchImpl = async (url) => ({ status: 404, ok: false, text: async () => "", url });
  const plan = await planPublication({ registry: "npm", unitsText: "@rightkit/top @rightkit/peer @rightkit/base", dryRun: false, root: fixture(), workspace: npm, fetchImpl });
  assert.deepEqual(plan.units.map((u) => u.name), ["@rightkit/peer", "@rightkit/base", "@rightkit/top"]); // dependencies first, ties in the order requested
  await assert.rejects(planPublication({ registry: "npm", unitsText: "@rightkit/top", dryRun: false, root: fixture(), workspace: npm, fetchImpl }), /@rightkit\/base@0\.2\.0 \(workspace version\) is not on npm/);
});

// ---- npm provenance preconditions (finding 6) -------------------------------------------------

test("every public npm package names Orthic-Labs/rightkit and its own directory", () => {
  const root = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, "$1")), "..", "..");
  const workspace = readNpmWorkspace(root);
  assert.ok(workspace.size > 5);
  for (const unit of workspace.values()) {
    if (unit.private) continue;
    const manifest = JSON.parse(fs.readFileSync(path.join(root, unit.dir, "package.json"), "utf8"));
    assert.deepEqual(repositoryProblems(manifest, unit.dir), [], unit.name);
  }
});

test("repository problems name the missing, wrong-owner and wrong-directory cases", () => {
  const ok = { name: "@rightkit/x", repository: { type: "git", url: "git+https://github.com/Orthic-Labs/rightkit.git", directory: "packages/x" } };
  assert.deepEqual(repositoryProblems(ok, "packages/x"), []);
  assert.deepEqual(repositoryProblems({ ...ok, repository: { ...ok.repository, url: "https://github.com/Orthic-Labs/rightkit" } }, "packages/x"), []);
  assert.match(repositoryProblems({ name: "@rightkit/x" }, "packages/x")[0], /repository must be an object/);
  assert.match(repositoryProblems({ ...ok, repository: "github:Orthic-Labs/rightkit" }, "packages/x")[0], /repository must be an object/);
  assert.match(repositoryProblems({ ...ok, repository: { ...ok.repository, url: "git+https://github.com/bogusyogi/rightkit.git" } }, "packages/x").join(), /must name https:\/\/github\.com\/Orthic-Labs\/rightkit/);
  assert.match(repositoryProblems({ ...ok, repository: { ...ok.repository, directory: "packages/y" } }, "packages/x").join(), /directory/);
});

// ---- fixtures for sealed artifacts ------------------------------------------------------------

/** Create `<out>` as a gzip tar holding `rootName/<files>` using the real tar. */
function makeTgz(out, rootName, files) {
  const stage = tmp("stage");
  for (const [rel, content] of Object.entries(files)) {
    const full = path.join(stage, rootName, rel);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, content);
  }
  const r = spawnSync("tar", ["-czf", path.basename(out), "-C", stage, rootName], { cwd: path.dirname(out), encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  fs.rmSync(stage, { recursive: true, force: true });
}

const npmPackageJson = (name, dir, extra = {}) => ({ name, version: "0.1.0", main: "dist/index.js", exports: { ".": { import: "./dist/index.js" } }, repository: { type: "git", url: "git+https://github.com/Orthic-Labs/rightkit.git", directory: dir }, ...extra });

function sealNpm({ units = [["@rightkit/a", "packages/a"]], packageJson = npmPackageJson } = {}) {
  const dir = tmp("sealed");
  const manifest = { schema: 1, registry: "npm", commit: SHA, units: [] };
  for (const [name, unitDir] of units) {
    const file = `${name.replace(/^@/, "").replace("/", "-")}-0.1.0.tgz`;
    makeTgz(path.join(dir, file), "package", { "package.json": JSON.stringify(packageJson(name, unitDir)), "dist/index.js": "export {};\n" });
    const bytes = fs.readFileSync(path.join(dir, file));
    manifest.units.push({ name, version: "0.1.0", dir: unitDir, file, size: bytes.length, sha256: sha256(bytes), sha1: crypto.createHash("sha1").update(bytes).digest("hex"), integrity: `sha512-${crypto.createHash("sha512").update(bytes).digest("base64")}` });
  }
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify(manifest));
  return { dir, manifest };
}

function sealCrates(names = ["rightkit-a"]) {
  const dir = tmp("sealed-crates");
  const manifest = { schema: 1, registry: "crates", commit: SHA, units: [] };
  for (const name of names) {
    const file = `${name}-0.1.0.crate`; const metaFile = `${name}-0.1.0.upload.json`;
    makeTgz(path.join(dir, file), `${name}-0.1.0`, { "Cargo.toml": `[package]\nname = "${name}"\n`, "Cargo.toml.orig": `[package]\nname = "${name}"\n`, "src/lib.rs": "pub fn a() {}\n" });
    const meta = `${JSON.stringify({ name, vers: "0.1.0", deps: [], features: {} })}\n`;
    fs.writeFileSync(path.join(dir, metaFile), meta);
    const bytes = fs.readFileSync(path.join(dir, file));
    manifest.units.push({ name, version: "0.1.0", dir: `crates/${name}`, file, size: bytes.length, sha256: sha256(bytes), metaFile, metaSha256: sha256(Buffer.from(meta)) });
  }
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify(manifest));
  return { dir, manifest };
}

function receipts(packDir, manifest, { skip = [], override = {}, unitOverride = {} } = {}) {
  const dir = tmp("receipts");
  const manifestSha256 = sha256(fs.readFileSync(path.join(packDir, "manifest.json")));
  for (const host of ["macos", "windows"]) {
    if (skip.includes(host)) continue;
    fs.mkdirSync(path.join(dir, `publish-receipt-verify-${host}`), { recursive: true });
    const doc = {
      schema: 1, kind: "verify", host, registry: manifest.registry, commit: manifest.commit, manifestSha256, runId: "77", allowHostExclusion: [], allowNoTest: [],
      units: manifest.units.map((u) => ({ name: u.name, version: u.version, file: u.file, sha256: u.sha256, bytesVerified: true, status: "pass", ...unitOverride })),
      ...override,
    };
    fs.writeFileSync(path.join(dir, `publish-receipt-verify-${host}`, `verify-${host}.json`), JSON.stringify(doc));
  }
  return dir;
}

/** A spawn that records every call, answers the ones in `fake`, and runs tar for real. */
function spy(fake = {}) {
  const calls = [];
  const spawn = (command, args, options = {}) => {
    calls.push({ command, args: [...args], options });
    const handler = fake[command];
    if (handler) { const answer = handler(args, options, calls); if (answer !== undefined) return answer; }
    if (command === "tar" || command === "git") return spawnSync(command, args, options);
    throw new Error(`unexpected child process: ${command} ${args.join(" ")}`);
  };
  return { spawn, calls };
}

const actionsEnv = (extra = {}) => ({ GITHUB_ACTIONS: "true", GITHUB_REF: "refs/heads/main", GITHUB_REPOSITORY: "Orthic-Labs/rightkit", GITHUB_SHA: SHA, GITHUB_RUN_ID: "77", GITHUB_RUN_ATTEMPT: "1", PATH: process.env.PATH ?? "", RUNNER_TEMP: os.tmpdir(), PUBLISH_REGISTRY: "npm", PUBLISH_UNITS: "@rightkit/a", ...extra });
const oidc = { ACTIONS_ID_TOKEN_REQUEST_URL: "https://oidc.invalid/x", ACTIONS_ID_TOKEN_REQUEST_TOKEN: "oidc-secret" };
const npmFetch = (sealed) => async (url) => {
  const unit = sealed.manifest.units.find((u) => String(url).includes(u.name.replace("/", "%2F")));
  if (!unit) return { status: 404, ok: false, text: async () => "" };
  return { status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: unit.sha1, integrity: unit.integrity } }) };
};

// ---- the upload job: seal, receipts, dry run, command and environment construction ------------

test("upload (dry run) checks hashes and receipts, sends nothing, and refuses any credential", async () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const { spawn, calls } = spy();
  const lines = [];
  const base = { packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: true, spawn, fetchImpl: async () => { throw new Error("a dry run must not touch the network"); }, log: (l) => lines.push(l) };
  const env = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/b" });
  const doc = await executeUpload({ ...base, env });
  assert.equal(doc.dryRun, true);
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.published]), [["@rightkit/a", false], ["@rightkit/b", false]]);
  assert.ok(calls.every((c) => c.command === "tar"), "a dry run runs no registry client");
  assert.match(lines.join("\n"), /dry run: would run npm publish rightkit-a-0\.1\.0\.tgz --ignore-scripts --access public --provenance --registry https:\/\/registry\.npmjs\.org\//);
  for (const variable of ["NODE_AUTH_TOKEN", "CARGO_REGISTRY_TOKEN"]) await assert.rejects(executeUpload({ ...base, env: { ...env, [variable]: "tok" } }), /present in a dry run/);
  await assert.rejects(executeUpload({ ...base, env: { ...env, ...oidc } }), /OIDC token is present in a dry run/);
});

test("upload (real npm) builds the publish command from the sealed tarball and isolates the credential", async () => {
  const sealed = sealNpm();
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const outDir = tmp("out");
  const { spawn, calls } = spy({ npm: () => ({ status: 0, stdout: "+ @rightkit/a@0.1.0 with token npm-secret-token\n", stderr: "" }) });
  const lines = [];
  const env = actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc, GITHUB_TOKEN: "ghs_nope", CARGO_REGISTRY_TOKEN: "" , INPUT_UNITS: "x", SECRET_SAUCE: "hunter2", HOME: os.homedir() });
  const doc = await executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed), log: (l) => lines.push(l), pause: 1 });
  const publish = calls.find((c) => c.command === "npm");
  assert.ok(publish, "npm publish ran");
  const file = path.join(sealed.dir, sealed.manifest.units[0].file);
  assert.deepEqual(publish.args.slice(0, 7), ["publish", file, "--ignore-scripts", "--access", "public", "--provenance", "--registry"]);
  assert.equal(publish.args[7], NPM_REGISTRY_URL);
  assert.equal(publish.args[8], "--userconfig");
  assert.ok(!publish.args.some((a) => a.includes("npm-secret-token")), "the token is never on a command line");
  assert.equal(publish.options.env.NODE_AUTH_TOKEN, "npm-secret-token");
  assert.equal(publish.options.env.ACTIONS_ID_TOKEN_REQUEST_URL, oidc.ACTIONS_ID_TOKEN_REQUEST_URL);
  for (const leaked of ["SECRET_SAUCE", "INPUT_UNITS", "GITHUB_TOKEN", "CARGO_REGISTRY_TOKEN"]) assert.ok(!(leaked in publish.options.env), `${leaked} must not reach npm`);
  assert.ok(lines.join("\n").includes("<redacted>") && !lines.join("\n").includes("npm-secret-token"), "the token is redacted from output");
  assert.deepEqual(doc.receipts[0], { ...doc.receipts[0], unit: "@rightkit/a", published: true, registryVerified: true, sha256: sealed.manifest.units[0].sha256, commit: SHA, registry: "npm", dryRun: false });
  // credentials missing or crossed
  await assert.rejects(executeUpload({ env: actionsEnv({ ...oidc }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /NODE_AUTH_TOKEN is not set/);
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "t", CARGO_REGISTRY_TOKEN: "c", ...oidc }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /CARGO_REGISTRY_TOKEN must not be present/);
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "t" }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /needs id-token: write/);
});

test("upload refuses a registry that serves different bytes than the sealed tarball", async () => {
  const sealed = sealNpm();
  const { spawn } = spy({ npm: () => ({ status: 0, stdout: "", stderr: "" }) });
  const fetchImpl = async () => ({ status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: "0".repeat(40), integrity: "sha512-x" } }) });
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "t", ...oidc }), packDir: sealed.dir, receiptsDir: receipts(sealed.dir, sealed.manifest), outDir: tmp("out"), dryRun: false, spawn, fetchImpl, pause: 1 }), /serves different bytes/);
});

test("upload refuses when the sealed bytes, the commit or the receipts do not bind", async () => {
  const run = (sealed, receiptsDir, env = actionsEnv({ NODE_AUTH_TOKEN: "t", ...oidc })) => executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn: spy({ npm: () => ({ status: 0, stdout: "", stderr: "" }) }).spawn, fetchImpl: npmFetch(sealed), pause: 1 });
  // a tarball changed after packing
  let sealed = sealNpm();
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  fs.appendFileSync(path.join(sealed.dir, sealed.manifest.units[0].file), "tamper");
  await assert.rejects(run(sealed, receiptsDir), /sha256 does not match the manifest/);
  // an unlisted file rides along
  sealed = sealNpm();
  fs.writeFileSync(path.join(sealed.dir, "evil.sh"), "x");
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest)), /unlisted file in the sealed artifact: evil\.sh/);
  // a missing host
  sealed = sealNpm();
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { skip: ["windows"] })), /expected exactly one verify receipt from windows, found 0/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { skip: ["macos"] })), /from macos, found 0/);
  // a receipt for another manifest, commit, run, or other bytes
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { manifestSha256: "f".repeat(64) } })), /different manifest/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { commit: "b".repeat(40) } })), /is not the sealed commit/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { runId: "99" } })), /is not this run 77/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { sha256: "e".repeat(64) } })), /verified different bytes/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { bytesVerified: false } })), /not verified/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "fail" } })), /status "fail" is not a pass/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-exclusion" } })), /without allow_host_exclusion/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-no-test" } })), /not named in allow_no_test/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { allowNoTest: ["@rightkit/a"] } })), /different allow_no_test/);
  // wrong commit, branch, repository, unit list
  const ok = receipts(sealed.dir, sealed.manifest);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_SHA: "c".repeat(40), NODE_AUTH_TOKEN: "t", ...oidc })), /not the dispatched commit/);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_REF: "refs/heads/dev", NODE_AUTH_TOKEN: "t", ...oidc })), /refs\/heads\/main only/);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_REPOSITORY: "someone/rightkit", NODE_AUTH_TOKEN: "t", ...oidc })), /in Orthic-Labs\/rightkit only/);
  await assert.rejects(run(sealed, ok, actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/z", NODE_AUTH_TOKEN: "t", ...oidc })), /differ from the planned units/);
});

test("upload re-checks provenance preconditions in the sealed tarball in both modes", async () => {
  const wrong = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { repository: { type: "git", url: "git+https://github.com/bogusyogi/rightkit.git", directory: dir } }) });
  const missing = sealNpm({ packageJson: (name, dir) => { const j = npmPackageJson(name, dir); delete j.repository; return j; } });
  const workspaceDep = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { dependencies: { "@rightkit/b": "workspace:*" } }) });
  const noEntry = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { main: "dist/missing.js" }) });
  for (const [sealed, pattern] of [[wrong, /must name https:\/\/github\.com\/Orthic-Labs\/rightkit/], [missing, /repository must be an object/], [workspaceDep, /workspace:/], [noEntry, /dist\/missing\.js, which is not in the tarball/]]) {
    for (const dryRun of [true, false]) {
      const env = dryRun ? actionsEnv() : actionsEnv({ NODE_AUTH_TOKEN: "t", ...oidc });
      await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir: receipts(sealed.dir, sealed.manifest), outDir: tmp("out"), dryRun, spawn: spy().spawn, fetchImpl: async () => { throw new Error("no network"); } }), pattern);
    }
  }
});

test("upload (crates) PUTs the sealed bytes to the publish API with the token on that request only", async () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const requests = [];
  const fetchImpl = async (url, options = {}) => {
    requests.push({ url: String(url), options });
    if (String(url) === CRATES_PUBLISH_URL) return { status: 200, ok: true, text: async () => JSON.stringify({ warnings: {} }) };
    const name = String(url).split("/").pop();
    const unit = sealed.manifest.units.find((u) => u.name === name);
    return { status: 200, ok: true, text: async () => JSON.stringify({ vers: "0.1.0", cksum: unit.sha256 }) };
  };
  const { spawn, calls } = spy();
  const env = actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", CARGO_REGISTRY_TOKEN: "crate-secret-token" });
  const doc = await executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl, pause: 1 });
  const puts = requests.filter((r) => r.options.method === "PUT");
  assert.deepEqual(puts.map((r) => r.url), [CRATES_PUBLISH_URL, CRATES_PUBLISH_URL]);
  assert.equal(CRATES_PUBLISH_URL, "https://crates.io/api/v1/crates/new");
  for (const [i, unit] of sealed.manifest.units.entries()) {
    const expected = buildCratePublishBody(fs.readFileSync(path.join(sealed.dir, unit.metaFile), "utf8"), fs.readFileSync(path.join(sealed.dir, unit.file)));
    assert.ok(puts[i].options.body.equals(expected), "the body is the length-prefixed metadata and the sealed crate bytes");
    assert.equal(puts[i].options.headers.authorization, "crate-secret-token");
    assert.equal(puts[i].options.redirect, "error");
  }
  assert.ok(requests.filter((r) => r.options.method !== "PUT").every((r) => !JSON.stringify(r.options.headers).includes("crate-secret-token")), "no other request carries the token");
  assert.ok(calls.every((c) => c.command === "tar"), "no cargo, no compiler, no install: only tar for the integrity listing");
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.published, r.registryVerified]), [["rightkit-a", true, true], ["rightkit-b", true, true]]);
  // a refusal carries the status and never the token; a dry run sends nothing
  const refused = async () => ({ status: 403, ok: false, text: async () => "forbidden crate-secret-token" });
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl: refused }), (error) => /HTTP 403/.test(error.message) && !error.message.includes("crate-secret-token"));
  const dry = await executeUpload({ env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b" }), packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: true, spawn, fetchImpl: async () => { throw new Error("no network"); }, log: () => {} });
  assert.equal(dry.receipts.length, 2);
  await assert.rejects(executeUpload({ env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", NODE_AUTH_TOKEN: "n", CARGO_REGISTRY_TOKEN: "c" }), packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl }), /NODE_AUTH_TOKEN must not be present/);
});

test("child environments are an allow-list, never process.env wholesale", () => {
  const source = { PATH: "/bin", HOME: "/h", GITHUB_SHA: "s", RUNNER_TEMP: "/t", ACTIONS_ID_TOKEN_REQUEST_URL: "u", NODE_AUTH_TOKEN: "n", CARGO_REGISTRY_TOKEN: "c", GITHUB_TOKEN: "g", AWS_SECRET_ACCESS_KEY: "k", INPUT_UNITS: "x", npm_config_registry: "evil" };
  assert.deepEqual(Object.keys(childEnv(source)).sort(), ["ACTIONS_ID_TOKEN_REQUEST_URL", "GITHUB_SHA", "HOME", "PATH", "RUNNER_TEMP"]);
  assert.equal(childEnv(source, { NODE_AUTH_TOKEN: "n" }).NODE_AUTH_TOKEN, "n");
});

test("the crates.io publish body is length-prefixed metadata then the crate", () => {
  const meta = JSON.stringify({ name: "x", vers: "0.1.0" }); const crateBytes = Buffer.from([1, 2, 3, 4, 5]);
  const body = buildCratePublishBody(meta, crateBytes);
  assert.equal(body.readUInt32LE(0), Buffer.byteLength(meta));
  assert.equal(body.subarray(4, 4 + Buffer.byteLength(meta)).toString(), meta);
  assert.equal(body.readUInt32LE(4 + Buffer.byteLength(meta)), 5);
  assert.ok(body.subarray(8 + Buffer.byteLength(meta)).equals(crateBytes));
});

test("crate upload metadata comes from the packaged manifest as Cargo's NewCrate shape", () => {
  const meta = buildCrateUploadMetadata({
    name: "rightkit-a", version: "0.1.0", authors: ["A"], description: "d", license: "MIT", repository: "https://github.com/Orthic-Labs/rightkit", readme: "README.md", keywords: ["k"], categories: [], rust_version: "1.80",
    features: { default: ["x"], x: [] },
    dependencies: [
      { name: "rightkit-b", req: "^0.2", kind: null, optional: false, uses_default_features: true, features: [], target: null, registry: null, rename: null },
      { name: "serde", req: "^1", kind: "dev", optional: true, uses_default_features: false, features: ["derive"], target: "cfg(unix)", registry: null, rename: "serde1" },
    ],
  }, "# readme\n");
  assert.equal(meta.name, "rightkit-a"); assert.equal(meta.vers, "0.1.0"); assert.equal(meta.readme, "# readme\n"); assert.equal(meta.readme_file, "README.md");
  assert.deepEqual(meta.deps[0], { optional: false, default_features: true, name: "rightkit-b", features: [], version_req: "^0.2", target: null, kind: "normal", registry: null, explicit_name_in_toml: null });
  assert.deepEqual(meta.deps[1], { optional: true, default_features: false, name: "serde", features: ["derive"], version_req: "^1", target: "cfg(unix)", kind: "dev", registry: null, explicit_name_in_toml: "serde1" });
});

// ---- pack and verify with intercepted child processes -----------------------------------------

function npmWorkspaceRoot(names = ["a"]) {
  const root = tmp("ws");
  for (const n of names) {
    fs.mkdirSync(path.join(root, "packages", n), { recursive: true });
    fs.writeFileSync(path.join(root, "packages", n, "package.json"), JSON.stringify({ ...npmPackageJson(`@rightkit/${n}`, `packages/${n}`), scripts: { test: "x" } }));
  }
  return root;
}

test("pack (npm) runs pnpm pack per unit with no credential, seals the bytes, and refuses a bad manifest", () => {
  const root = npmWorkspaceRoot(["a", "b"]);
  const outRoot = tmp("run");
  let badRepo = false;
  const pnpm = (args) => {
    const unit = args[1].split("/").at(-1);
    const dest = args[args.indexOf("--pack-destination") + 1];
    makeTgz(path.join(dest, `rightkit-${unit}-0.1.0.tgz`), "package", { "package.json": JSON.stringify(npmPackageJson(`@rightkit/${unit}`, `packages/${unit}`, badRepo ? { repository: { type: "git", url: "git+https://github.com/bogusyogi/rightkit.git", directory: `packages/${unit}` } } : {})), "dist/index.js": "x" });
    return { status: 0 };
  };
  const { spawn, calls } = spy({ pnpm, git: () => ({ status: 0, stdout: `${SHA}\n` }) });
  const env = actionsEnv({ RUNNER_TEMP: outRoot, PUBLISH_UNITS: "@rightkit/a @rightkit/b" });
  const manifest = commandPack({ env, spawn, root });
  const packs = calls.filter((c) => c.command === "pnpm");
  assert.deepEqual(packs.map((c) => c.args.slice(0, 3)), [["--dir", "packages/a", "pack"], ["--dir", "packages/b", "pack"]]);
  assert.deepEqual(manifest.units.map((u) => u.name), ["@rightkit/a", "@rightkit/b"]);
  assert.equal(manifest.commit, SHA);
  const onDisk = JSON.parse(fs.readFileSync(path.join(outRoot, "rk-pack", "manifest.json"), "utf8"));
  assert.equal(onDisk.units[0].sha256, sha256(fs.readFileSync(path.join(outRoot, "rk-pack", onDisk.units[0].file))));
  assert.throws(() => commandPack({ env: { ...env, NODE_AUTH_TOKEN: "t" }, spawn, root }), /pack and verify must not receive secrets/);
  assert.throws(() => commandPack({ env: { ...env, ...oidc }, spawn, root }), /must not request id-token/);
  assert.throws(() => commandPack({ env: { ...env, GITHUB_REF: "refs/heads/x" }, spawn, root }), /refs\/heads\/main only/);
  badRepo = true;
  assert.throws(() => commandPack({ env, spawn, root }), /must name https:\/\/github\.com\/Orthic-Labs\/rightkit/);
});

test("pack (crates) runs one cargo package, reads the packaged manifest, and seals metadata beside the crate", () => {
  const root = tmp("ws");
  const target = tmp("target");
  const outRoot = tmp("run");
  const wsMeta = { workspace_root: root, target_directory: target, workspace_members: ["a", "b"], packages: [
    metaPkg("rightkit-a", "0.1.0", [], { id: "a", manifest_path: `${root}/crates/rightkit-a/Cargo.toml` }),
    metaPkg("rightkit-b", "0.1.0", [metaDep("rightkit-a", "^0.1", { source: null })], { id: "b", manifest_path: `${root}/crates/rightkit-b/Cargo.toml` }),
  ] };
  const cargo = (args) => {
    if (args[0] === "metadata" && args.includes("--manifest-path")) {
      const manifestPath = args[args.indexOf("--manifest-path") + 1];
      const name = path.basename(path.dirname(manifestPath)).replace(/-0\.1\.0$/, "");
      const deps = name === "rightkit-b" ? [metaDep("rightkit-a", "^0.1")] : [];
      return { status: 0, stdout: JSON.stringify({ packages: [{ name, version: "0.1.0", dependencies: deps, features: {}, readme: "README.md", authors: [], keywords: [], categories: [], manifest_path: manifestPath }] }) };
    }
    if (args[0] === "metadata") return { status: 0, stdout: JSON.stringify(wsMeta) };
    if (args[0] === "package") {
      fs.mkdirSync(path.join(target, "package"), { recursive: true });
      for (const name of ["rightkit-a", "rightkit-b"]) makeTgz(path.join(target, "package", `${name}-0.1.0.crate`), `${name}-0.1.0`, { "Cargo.toml": "x", "README.md": `# ${name}\n`, "src/lib.rs": "" });
      return { status: 0 };
    }
  };
  const { spawn, calls } = spy({ cargo, git: () => ({ status: 0, stdout: `${SHA}\n` }) });
  const env = actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", RUNNER_TEMP: outRoot });
  const manifest = commandPack({ env, spawn, root });
  const packageCalls = calls.filter((c) => c.command === "cargo" && c.args[0] === "package");
  assert.equal(packageCalls.length, 1);
  assert.deepEqual(packageCalls[0].args, ["package", "--locked", "-p", "rightkit-a", "-p", "rightkit-b"]);
  assert.equal(calls.filter((c) => c.command === "cargo" && c.args[0] === "publish").length, 0, "pack never publishes");
  const upload = JSON.parse(fs.readFileSync(path.join(outRoot, "rk-pack", manifest.units[1].metaFile), "utf8"));
  assert.equal(upload.name, "rightkit-b");
  assert.equal(upload.readme, "# rightkit-b\n");
  assert.deepEqual(upload.deps.map((d) => [d.name, d.version_req, d.kind]), [["rightkit-a", "^0.1", "normal"]]);
  assert.equal(manifest.units[1].metaSha256, sha256(fs.readFileSync(path.join(outRoot, "rk-pack", manifest.units[1].metaFile))));
  assert.throws(() => commandPack({ env: { ...env, CARGO_REGISTRY_TOKEN: "c" }, spawn, root }), /must not receive secrets/);
});

test("verify re-hashes the sealed bytes, requires gate evidence, and binds the receipt to the manifest", () => {
  const sealed = sealNpm();
  const runTemp = tmp("verify");
  const evidenceDir = path.join(runTemp, "rightkit-ci");
  fs.mkdirSync(evidenceDir, { recursive: true });
  const writeEvidence = (patch = {}, status = "pass") => fs.writeFileSync(path.join(evidenceDir, "unit-evidence.json"), JSON.stringify({ schema: 1, host: "windows", registry: "npm", sha: SHA, units: [{ name: "@rightkit/a", status, steps: [] }], ...patch }));
  writeEvidence();
  const env = actionsEnv({ RUNNER_TEMP: runTemp, PUBLISH_PACK_DIR: sealed.dir });
  const run = (over = {}, host = "windows") => commandVerify({ env: { ...env, ...over }, spawn: spy({ git: () => ({ status: 0, stdout: `${SHA}\n` }) }).spawn, host });
  const receipt = run();
  assert.equal(receipt.host, "windows");
  assert.equal(receipt.manifestSha256, sha256(fs.readFileSync(path.join(sealed.dir, "manifest.json"))));
  assert.deepEqual(receipt.units.map((u) => [u.name, u.status, u.bytesVerified]), [["@rightkit/a", "pass", true]]);
  assert.ok(fs.existsSync(path.join(runTemp, "verify-receipt", "verify-windows.json")));
  assert.throws(() => run({ NODE_AUTH_TOKEN: "t" }), /must not receive secrets/);
  assert.throws(() => run({}, "linux"), /macOS or Windows/);
  writeEvidence({ host: "macos" });
  assert.throws(() => run(), /gate evidence is for npm on macos/);
  writeEvidence({ units: [] });
  assert.throws(() => run(), /left no evidence for it/);
  writeEvidence({}, "fail");
  assert.throws(() => run(), /gate status "fail"/);
  writeEvidence();
  fs.appendFileSync(path.join(sealed.dir, sealed.manifest.units[0].file), "x");
  assert.throws(() => run(), /sha256 does not match the manifest/);
});

test("a sealed crate must equal the commit's source, byte for byte", () => {
  const dir = tmp("crate");
  const file = path.join(dir, "rightkit-a-0.1.0.crate");
  makeTgz(file, "rightkit-a-0.1.0", { "Cargo.toml": "generated", "Cargo.toml.orig": "[package]\nname=\"rightkit-a\"\n", "src/lib.rs": "pub fn a() {}\n", "README.md": "# a\n" });
  const unit = { name: "rightkit-a", version: "0.1.0", dir: "crates/rightkit-a" };
  const blobs = { "crates/rightkit-a/Cargo.toml": "[package]\nname=\"rightkit-a\"\n", "crates/rightkit-a/src/lib.rs": "pub fn a() {}\n", "README.md": "# a\n" };
  const gitFor = (overrides = {}) => (args) => {
    const key = args[2].slice(`${SHA}:`.length);
    const content = { ...blobs, ...overrides }[key];
    return content === undefined ? { status: 128, stdout: Buffer.alloc(0) } : { status: 0, stdout: Buffer.from(content) };
  };
  const check = (overrides) => crateSourceProblems({ file, unit, commit: SHA, scratchRoot: dir, spawn: spy({ git: gitFor(overrides) }).spawn });
  assert.deepEqual(check({}), []);
  assert.match(check({ "crates/rightkit-a/src/lib.rs": "pub fn evil() {}\n" }).join(), /src\/lib\.rs in the sealed crate does not match/);
  assert.match(check({ "crates/rightkit-a/Cargo.toml": "[package]\nname=\"other\"\n" }).join(), /Cargo\.toml\.orig/);
  assert.match(check({ "README.md": undefined }).join(), /README\.md/);
});

// ---- the workflow parser ----------------------------------------------------------------------

test("the workflow YAML subset parser reads structure and rejects what it cannot parse", () => {
  const doc = parseYaml(`# c
name: x
on:
  workflow_dispatch:
    inputs:
      list:
        options: [npm, "crates"]
jobs:
  a:
    needs: [b]
    if: \${{ x == 'y' }} # trailing
    steps:
      - uses: act/x@abc # note
        with:
          k: "v: w"
      - name: s
        run: |
          echo "# kept"
          if true; then
            exit 1
          fi
        env:
          E: \${{ secrets.T }}
      - run: single line
`);
  assert.deepEqual(Object.keys(doc.on), ["workflow_dispatch"]);
  assert.deepEqual(doc.on.workflow_dispatch.inputs.list.options, ["npm", "crates"]);
  assert.deepEqual(doc.jobs.a.needs, ["b"]);
  assert.equal(doc.jobs.a.if, "${{ x == 'y' }}");
  assert.equal(doc.jobs.a.steps[0].uses, "act/x@abc");
  assert.equal(doc.jobs.a.steps[0].with.k, "v: w");
  assert.equal(doc.jobs.a.steps[1].run, 'echo "# kept"\nif true; then\n  exit 1\nfi');
  assert.equal(doc.jobs.a.steps[1].env.E, "${{ secrets.T }}");
  assert.equal(doc.jobs.a.steps[2].run, "single line");
  for (const [bad, pattern] of [
    ["a:\n\tb: 1\n", /tab/], ["a: &x 1\n", /anchors/], ["a: 1\na: 2\n", /duplicate key/], ["a: one\n  two\n", /multi-line plain/],
    ["a: {b: 1}\n", /flow mappings/], ["a: [b, [c]]\n", /nested flow/], ["a: *x\n", /anchors/], ["a:\n  b: 1\n c: 2\n", /indent/],
  ]) assert.throws(() => parseYaml(bad), pattern, JSON.stringify(bad));
});

test("the real publish workflow parses, and it is the sealed-upload shape", () => {
  const text = fs.readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8");
  const doc = parseYaml(text);
  assert.deepEqual(Object.keys(doc.jobs), ["guard", "plan", "pack", "verify", "seal-check", "upload"]);
  assert.deepEqual([...doc.jobs.verify.strategy.matrix.os].sort(), ["windows-2025", "xcode-27"]);
  assert.equal(doc.jobs.upload.environment, "release");
});
