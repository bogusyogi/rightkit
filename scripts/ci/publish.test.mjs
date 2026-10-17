import assert from "node:assert/strict";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { buildCrateUploadMetadata, commandLedger, commandPack, commandVerify, crateIndexPath, crateSourceProblems, crateWorkspaceFromMetadata, ledgerEntries, parseAllowList, parseUnitNames, planPublication, readNpmWorkspace, satisfies } from "./publish.mjs";
import { buildCratePublishBody, checkNpmTarball, CRATES_PUBLISH_URL, childEnv, executeUpload, loadSealed, NPM_REGISTRY_URL, parseCratesResponse, repositoryProblems } from "./upload.mjs";
import { consumeCrates, consumeNpm, declaredEntries, npmCliCommand, skipImportList } from "./consume.mjs";
import { assertPublishDoc, MUTATIONS, proveMutations } from "./workflow-contract.mjs";
import { pinFor, recordPin } from "./provision-chrome.mjs";
import { parseYaml } from "./workflow-yaml.mjs";

// A directory for tar's -C. Git for Windows puts a GNU tar on PATH that reads a backslash as an escape, so
// "C:\\x\\y" is not found; both it and the system bsdtar accept forward slashes.
const tarDir = (dir) => (process.platform === "win32" ? dir.replaceAll("\\", "/") : dir);

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
  assert.deepEqual(parseUnitNames("rightkit-fs  rightkit-job-queue rightkit-fs", "crates"), ["rightkit-fs", "rightkit-job-queue"]);
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

test("every public npm package names bogusyogi/rightkit and its own directory", () => {
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
  const ok = { name: "@rightkit/x", repository: { type: "git", url: "git+https://github.com/bogusyogi/rightkit.git", directory: "packages/x" } };
  assert.deepEqual(repositoryProblems(ok, "packages/x"), []);
  assert.deepEqual(repositoryProblems({ ...ok, repository: { ...ok.repository, url: "https://github.com/bogusyogi/rightkit" } }, "packages/x"), []);
  assert.match(repositoryProblems({ name: "@rightkit/x" }, "packages/x")[0], /repository must be an object/);
  assert.match(repositoryProblems({ ...ok, repository: "github:bogusyogi/rightkit" }, "packages/x")[0], /repository must be an object/);
  assert.match(repositoryProblems({ ...ok, repository: { ...ok.repository, url: "git+https://github.com/someone-else/rightkit.git" } }, "packages/x").join(), /must name https:\/\/github\.com\/bogusyogi\/rightkit/);
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
  const r = spawnSync("tar", ["-czf", path.basename(out), "-C", tarDir(stage), rootName], { cwd: path.dirname(out), encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  fs.rmSync(stage, { recursive: true, force: true });
}

const npmPackageJson = (name, dir, extra = {}) => ({ name, version: "0.1.0", main: "dist/index.js", exports: { ".": { import: "./dist/index.js" } }, repository: { type: "git", url: "git+https://github.com/bogusyogi/rightkit.git", directory: dir }, ...extra });

function sealNpm({ units = [["@rightkit/a", "packages/a"]], packageJson = npmPackageJson, extraFiles = {} } = {}) {
  const dir = tmp("sealed");
  const manifest = { schema: 1, registry: "npm", commit: SHA, runId: "77", runAttempt: "1", units: [] };
  for (const [name, unitDir] of units) {
    const file = `${name.replace(/^@/, "").replace("/", "-")}-0.1.0.tgz`;
    makeTgz(path.join(dir, file), "package", { "package.json": JSON.stringify(packageJson(name, unitDir)), "dist/index.js": "export {};\n", ...extraFiles });
    const bytes = fs.readFileSync(path.join(dir, file));
    manifest.units.push({ name, version: "0.1.0", dir: unitDir, file, size: bytes.length, sha256: sha256(bytes), sha1: crypto.createHash("sha1").update(bytes).digest("hex"), integrity: `sha512-${crypto.createHash("sha512").update(bytes).digest("base64")}` });
  }
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify(manifest));
  return { dir, manifest };
}

function sealCrates(names = ["rightkit-a"]) {
  const dir = tmp("sealed-crates");
  const manifest = { schema: 1, registry: "crates", commit: SHA, runId: "77", runAttempt: "1", units: [] };
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
      schema: 1, kind: "verify", host, registry: manifest.registry, commit: manifest.commit, manifestSha256, runId: "77", runAttempt: "1", manifestRunId: manifest.runId, manifestRunAttempt: manifest.runAttempt,
      allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [],
      units: manifest.units.map((u) => ({ name: u.name, version: u.version, file: u.file, sha256: u.sha256, bytesVerified: true, status: "pass", executed: 3, ignored: 0, skipped: 0, consumer: { verified: true }, ...unitOverride })),
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

const actionsEnv = (extra = {}) => ({ GITHUB_ACTIONS: "true", GITHUB_REF: "refs/heads/main", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_SHA: SHA, GITHUB_RUN_ID: "77", GITHUB_RUN_ATTEMPT: "1", PATH: process.env.PATH ?? "", RUNNER_TEMP: os.tmpdir(), PUBLISH_REGISTRY: "npm", PUBLISH_UNITS: "@rightkit/a", ...extra });
const oidc = { ACTIONS_ID_TOKEN_REQUEST_URL: "https://oidc.invalid/x", ACTIONS_ID_TOKEN_REQUEST_TOKEN: "oidc-secret" };
// A registry that does not hold a unit until it is asked a second time (the first request is upload.mjs's pre-publish
// reconciliation); `already` names units that are on the registry from the start.
const npmFetch = (sealed, { already = [] } = {}) => {
  const asked = new Set();
  return async (url) => {
    const unit = sealed.manifest.units.find((u) => String(url).includes(u.name.replace("/", "%2F")));
    if (!unit) return { status: 404, ok: false, text: async () => "" };
    if (!already.includes(unit.name) && !asked.has(unit.name)) { asked.add(unit.name); return { status: 404, ok: false, text: async () => "" }; }
    return { status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: unit.sha1, integrity: unit.integrity } }) };
  };
};
/** A stateful fake npm: `npm publish` adds the tarball's unit (unless `failing` names it), the registry API serves what was added. */
function fakeNpm(sealed, { failing = [], present = {} } = {}) {
  const held = new Map(Object.entries(present));
  const publishes = [];
  const unitOf = (text) => sealed.manifest.units.find((u) => text.includes(u.file) || text.includes(u.name.replace("/", "%2F")));
  const { spawn } = spy({ npm: (args) => {
    const unit = unitOf(path.basename(args[1]));
    publishes.push(unit.name);
    if (failing.includes(unit.name)) return { status: 1, stdout: "", stderr: "E500" };
    held.set(unit.name, { shasum: unit.sha1, integrity: unit.integrity });
    return { status: 0, stdout: "", stderr: "" };
  } });
  const fetchImpl = async (url) => {
    const unit = unitOf(String(url));
    const dist = unit && held.get(unit.name);
    if (!dist) return { status: 404, ok: false, text: async () => "" };
    return { status: 200, ok: true, text: async () => JSON.stringify({ dist }) };
  };
  return { spawn, fetchImpl, publishes, held };
}

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
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", CARGO_REGISTRY_TOKEN: "crate-secret-token", ...oidc }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /CARGO_REGISTRY_TOKEN must not be present/);
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token" }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /needs id-token: write/);
});

test("upload refuses a registry that serves different bytes than the sealed tarball", async () => {
  const sealed = sealNpm();
  const { spawn } = spy({ npm: () => ({ status: 0, stdout: "", stderr: "" }) });
  const fetchImpl = async () => ({ status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: "0".repeat(40), integrity: "sha512-x" } }) });
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc }), packDir: sealed.dir, receiptsDir: receipts(sealed.dir, sealed.manifest), outDir: tmp("out"), dryRun: false, spawn, fetchImpl, pause: 1 }), /serves different bytes/);
});

test("upload refuses when the sealed bytes, the commit or the receipts do not bind", async () => {
  const run = (sealed, receiptsDir, env = actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })) => executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn: spy({ npm: () => ({ status: 0, stdout: "", stderr: "" }) }).spawn, fetchImpl: npmFetch(sealed), pause: 1 });
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
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { allowIgnored: ["@rightkit/a"] } })), /different allow_ignored/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { allowMissingAssets: ["@rightkit/a"] } })), /different allow_missing_assets/);
  // structured results: a pass needs executed tests and nothing ignored or skipped; an allowance must be requested
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { executed: 0 } })), /without a positive executed count/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { skipped: 2 } })), /status pass without a positive executed count and zero ignored\/skipped/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-skips", skipped: 2 } })), /not named in allow_missing_assets/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-skips", ignored: 2 } })), /not named in allow_ignored/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { consumer: { verified: false } } })), /not consumer-verified/);
  // the sealed artifact and the receipts bind to THIS run and its attempts
  const unbound = JSON.parse(fs.readFileSync(path.join(sealed.dir, "manifest.json"), "utf8"));
  const rewrite = (patch) => { const d = sealNpm(); const m = { ...JSON.parse(fs.readFileSync(path.join(d.dir, "manifest.json"), "utf8")), ...patch }; fs.writeFileSync(path.join(d.dir, "manifest.json"), JSON.stringify(m)); return { dir: d.dir, manifest: m }; };
  assert.equal(unbound.runId, "77");
  let other = rewrite({ runId: "78" });
  await assert.rejects(run(other, receipts(other.dir, other.manifest)), /manifest run 78 is not this run 77/);
  other = rewrite({ runId: undefined });
  await assert.rejects(run(other, receipts(other.dir, other.manifest)), /manifest\.runId is missing/);
  other = rewrite({ runAttempt: "2" });
  await assert.rejects(run(other, receipts(other.dir, other.manifest)), /manifest run attempt 2 is later than this attempt 1/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { runAttempt: "2" } })), /run attempt 2 is later than this attempt 1/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { runAttempt: "x" } })), /not a positive integer/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { manifestRunAttempt: "9" } })), /it verified manifest run 77\/9/);
  const rerun = rewrite({ runAttempt: "1" });
  await run(rerun, receipts(rerun.dir, rerun.manifest, { override: { runAttempt: "2" } }), actionsEnv({ GITHUB_RUN_ATTEMPT: "2", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })); // a re-run may reuse attempt 1's bytes
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest), actionsEnv({ GITHUB_RUN_ID: "", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /GITHUB_RUN_ID and GITHUB_RUN_ATTEMPT must be set/);
  const rerunReceipts = receipts(rerun.dir, rerun.manifest, { override: { runAttempt: "1" } });
  await run(rerun, rerunReceipts, actionsEnv({ GITHUB_RUN_ATTEMPT: "2", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc }));
  assert.deepEqual(loadSealed({ packDir: rerun.dir, registry: "npm", unitsText: "@rightkit/a", run: { runId: "77", runAttempt: "2" } }).problems, []);
  // wrong commit, branch, repository, unit list
  const ok = receipts(sealed.dir, sealed.manifest);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_SHA: "c".repeat(40), NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /not the dispatched commit/);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_REF: "refs/heads/dev", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /refs\/heads\/main only/);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_REPOSITORY: "someone/rightkit", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /in bogusyogi\/rightkit only/);
  await assert.rejects(run(sealed, ok, actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/z", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /differ from the planned units/);
});

test("upload re-checks provenance preconditions in the sealed tarball in both modes", async () => {
  const wrong = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { repository: { type: "git", url: "git+https://github.com/someone-else/rightkit.git", directory: dir } }) });
  const missing = sealNpm({ packageJson: (name, dir) => { const j = npmPackageJson(name, dir); delete j.repository; return j; } });
  const workspaceDep = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { dependencies: { "@rightkit/b": "workspace:*" } }) });
  const noEntry = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { main: "dist/missing.js" }) });
  for (const [sealed, pattern] of [[wrong, /must name https:\/\/github\.com\/bogusyogi\/rightkit/], [missing, /repository must be an object/], [workspaceDep, /workspace:/], [noEntry, /dist\/missing\.js, which is not in the tarball/]]) {
    for (const dryRun of [true, false]) {
      const env = dryRun ? actionsEnv() : actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc });
      await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir: receipts(sealed.dir, sealed.manifest), outDir: tmp("out"), dryRun, spawn: spy().spawn, fetchImpl: async () => { throw new Error("no network"); } }), pattern);
    }
  }
});

test("upload (crates) PUTs the sealed bytes to the publish API with the token on that request only", async () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const requests = [];
  // A fresh registry view per upload: the first index read of a crate (upload.mjs's reconciliation) finds nothing; later reads find it.
  const makeFetch = () => {
    const asked = new Set();
    return async (url, options = {}) => {
      requests.push({ url: String(url), options });
      if (String(url) === CRATES_PUBLISH_URL) return { status: 200, ok: true, text: async () => JSON.stringify({ warnings: {} }) };
      const name = String(url).split("/").pop();
      const unit = sealed.manifest.units.find((u) => u.name === name);
      if (!asked.has(name)) { asked.add(name); return { status: 404, ok: false, text: async () => "" }; }
      return { status: 200, ok: true, text: async () => JSON.stringify({ vers: "0.1.0", cksum: unit.sha256 }) };
    };
  };
  const fetchImpl = makeFetch();
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
  assert.equal(puts[0].options.headers.accept, "application/json", "the documented Accept header is sent");
  // HTTP 200 with an errors array is a failure, recorded as not published; warnings are surfaced; a non-JSON body is refused
  const answering = (body, status = 200, inner = null) => async (url, options = {}) => (String(url) === CRATES_PUBLISH_URL ? { status, ok: status < 400, text: async () => body } : (inner ??= makeFetch())(url, options));
  const published = [];
  const one = { env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", CARGO_REGISTRY_TOKEN: "crate-secret-token" }), packDir: sealed.dir, receiptsDir, dryRun: false, spawn, pause: 1 };
  await assert.rejects(executeUpload({ ...one, outDir: tmp("out"), fetchImpl: answering(JSON.stringify({ errors: [{ detail: "crate version `0.1.0` is already uploaded crate-secret-token" }] })) }), (error) => /with errors for rightkit-a@0\.1\.0; not recorded as published: crate version `0\.1\.0` is already uploaded/.test(error.message) && !error.message.includes("crate-secret-token"));
  const outErr = tmp("out");
  await assert.rejects(executeUpload({ ...one, outDir: outErr, fetchImpl: answering(JSON.stringify({ errors: [{ detail: "nope" }] })) }), /with errors/);
  const errRows = JSON.parse(fs.readFileSync(path.join(outErr, "publication-receipt.json"), "utf8")).receipts;
  assert.deepEqual(errRows.map((r) => [r.unit, r.published, r.registryVerified, r.outcome]), [["rightkit-a", false, false, "failed"]], "an errors body records a failed attempt, never a publication");
  await assert.rejects(executeUpload({ ...one, outDir: tmp("out"), fetchImpl: answering("<html>ok</html>") }), /not the documented JSON/);
  const warned = await executeUpload({ ...one, outDir: tmp("out"), log: (l) => published.push(l), fetchImpl: answering(JSON.stringify({ warnings: { invalid_categories: ["bogus"], invalid_badges: [], other: ["readme is empty"] } })) });
  assert.match(published.join("\n"), /warnings for rightkit-a@0\.1\.0: invalid_categories: bogus; other: readme is empty/);
  assert.deepEqual(warned.receipts[0].warnings, ["invalid_categories: bogus", "other: readme is empty"]);
  assert.deepEqual(parseCratesResponse('{"warnings":{"invalid_categories":[],"invalid_badges":[],"other":[]}}'), { errors: [], warnings: [], unreadable: false });
  assert.deepEqual(parseCratesResponse('{"errors":["plain"]}').errors, ["plain"]);
  assert.equal(parseCratesResponse("[]").unreadable, true);
  // a refusal carries the status and never the token; a dry run sends nothing
  const refused = answering("forbidden crate-secret-token", 403);
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl: refused }), (error) => /HTTP 403/.test(error.message) && !error.message.includes("crate-secret-token"));
  const dry = await executeUpload({ env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b" }), packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: true, spawn, fetchImpl: async () => { throw new Error("no network"); }, log: () => {} });
  assert.equal(dry.receipts.length, 2);
  await assert.rejects(executeUpload({ env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", NODE_AUTH_TOKEN: "npm-secret-token", CARGO_REGISTRY_TOKEN: "crate-secret-token" }), packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl }), /NODE_AUTH_TOKEN must not be present/);
});

// ---- a retry of failed jobs: partial publication is reconciled, never erased, never republished --

test("upload retry: the second unit failed, the retry starts from the earlier receipt and publishes only the rest", async () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env1 = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/b", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc });
  const out1 = tmp("out1");
  const first = fakeNpm(sealed, { failing: ["@rightkit/b"] });
  await assert.rejects(executeUpload({ env: env1, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: out1, dryRun: false, spawn: first.spawn, fetchImpl: first.fetchImpl, pause: 1, log: () => {} }), /npm publish failed for @rightkit\/b@0\.1\.0/);
  const after1 = JSON.parse(fs.readFileSync(path.join(out1, "publication-receipt.json"), "utf8"));
  assert.deepEqual(after1.receipts.map((r) => [r.unit, r.published, r.registryVerified, r.outcome, r.attempt]), [["@rightkit/a", true, true, "published", "1"], ["@rightkit/b", false, false, "failed", "1"]], "the failed attempt records the success AND the failure");
  // Attempt 2 of the same run: a is on the registry (held), b is not. The earlier receipt is the starting point.
  const out2 = tmp("out2");
  const second = fakeNpm(sealed, { present: Object.fromEntries([...first.held]) });
  const lines = [];
  const doc = await executeUpload({ env: { ...env1, GITHUB_RUN_ATTEMPT: "2" }, packDir: sealed.dir, receiptsDir, previousDir: out1, outDir: out2, dryRun: false, spawn: second.spawn, fetchImpl: second.fetchImpl, pause: 1, log: (l) => lines.push(l) });
  assert.deepEqual(second.publishes, ["@rightkit/b"], "only the unit not on the registry is published");
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.published, r.registryVerified, r.outcome, r.attempt]), [["@rightkit/a", true, true, "published-by-earlier-attempt", "2"], ["@rightkit/b", true, true, "published", "2"]]);
  assert.equal(doc.receipts[0].publishedBy, "earlier-attempt");
  assert.equal(doc.receipts[0].earlierAttempt, "1");
  assert.equal(doc.receipts[0].earlierOutcome, "published");
  assert.deepEqual(doc.priorAttempts, ["1"]);
  assert.ok(doc.history.some((r) => r.unit === "@rightkit/a" && r.attempt === "1" && r.outcome === "published"), "attempt 1's row is kept as history, not overwritten");
  assert.ok(doc.history.some((r) => r.unit === "@rightkit/b" && r.outcome === "failed"), "attempt 1's failure is kept");
  assert.match(lines.join("\n"), /@rightkit\/a@0\.1\.0 is already on npm with the sealed hash: published by an earlier attempt/);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(out2, "publication-receipt.json"), "utf8")), doc, "the receipt on disk is the one returned");
  // The ledger accepts what the retried receipt says.
  const ledger = ledgerEntries(doc);
  assert.deepEqual(ledger.entries.map((e) => e.name), ["@rightkit/a", "@rightkit/b"]);
});

test("upload retry: a version already on the registry with different bytes is refused before anything is sent", async () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/b", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc });
  const [a] = sealed.manifest.units;
  // b is first in nothing: a is absent, b has other bytes. Nothing may be published, not even a.
  const fake = fakeNpm(sealed, { present: { "@rightkit/b": { shasum: "0".repeat(40), integrity: "sha512-other" } } });
  const out = tmp("out");
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: out, dryRun: false, spawn: fake.spawn, fetchImpl: fake.fetchImpl, pause: 1, log: () => {} }), /npm already serves different bytes for @rightkit\/b@0\.1\.0/);
  assert.deepEqual(fake.publishes, [], "no publish ran");
  const rows = JSON.parse(fs.readFileSync(path.join(out, "publication-receipt.json"), "utf8")).receipts;
  assert.deepEqual(rows.map((r) => [r.unit, r.outcome]), [["@rightkit/b", "refused"]]);
  assert.equal(a.name, "@rightkit/a");
  // An unreadable registry answer is not permission to publish either.
  const down = { ...fake, fetchImpl: async () => ({ status: 503, ok: false, text: async () => "" }) };
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: tmp("out"), dryRun: false, spawn: down.spawn, fetchImpl: down.fetchImpl, pause: 1, log: () => {} }), /cannot reconcile @rightkit\/a@0\.1\.0 with the npm registry: HTTP 503/);
  assert.deepEqual(fake.publishes, []);
});

test("upload retry: refuses a previous receipt that is not this run's, or that says verified what the registry no longer serves", async () => {
  const sealed = sealNpm();
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env = { ...actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc }), GITHUB_RUN_ATTEMPT: "2" };
  const unit = sealed.manifest.units[0];
  const prior = (patch = {}, rows) => {
    const dir = tmp("prior");
    fs.writeFileSync(path.join(dir, "publication-receipt.json"), JSON.stringify({ schema: 1, registry: "npm", dryRun: false, commit: SHA, runId: "77", runAttempt: "1", priorAttempts: [], history: [], receipts: rows ?? [{ unit: unit.name, version: unit.version, published: true, registryVerified: true, outcome: "published", attempt: "1" }], ...patch }));
    return dir;
  };
  const go = (previousDir) => { const fake = fakeNpm(sealed); return executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir, outDir: tmp("out"), dryRun: false, spawn: fake.spawn, fetchImpl: fake.fetchImpl, pause: 1, log: () => {} }); };
  await assert.rejects(go(prior({ runId: "78" })), /previous attempt receipt refused: run 78 is not this run 77/);
  await assert.rejects(go(prior({ commit: "b".repeat(40) })), /previous attempt receipt refused: commit/);
  await assert.rejects(go(prior({ dryRun: true })), /not a real publication receipt/);
  await assert.rejects(go(prior({ registry: "crates" })), /registry crates is not npm/);
  await assert.rejects(go(prior({ runAttempt: "2" })), /attempt 2 is not earlier than this attempt 2/);
  await assert.rejects(go(prior({}, [{ unit: unit.name, version: unit.version, published: true, registryVerified: true, outcome: "published", attempt: "1" }])), /was recorded as published and verified by attempt 1, but the npm registry no longer serves it/);
  const garbage = tmp("prior"); fs.writeFileSync(path.join(garbage, "publication-receipt.json"), "not json");
  await assert.rejects(go(garbage), /previous attempt receipt is unreadable/);
  // No earlier receipt at all is a first attempt.
  const fresh = await go(tmp("none"));
  assert.equal(fresh.receipts[0].outcome, "published");
});

test("upload retry (crates): a crate already indexed with the sealed checksum is not PUT again; another checksum is refused", async () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env = actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", CARGO_REGISTRY_TOKEN: "crate-secret-token", GITHUB_RUN_ATTEMPT: "2" });
  const held = new Map([["rightkit-a", sealed.manifest.units[0].sha256]]);
  const puts = [];
  const fetchImpl = async (url, options = {}) => {
    if (String(url) === CRATES_PUBLISH_URL) { puts.push(options.body.length); held.set("rightkit-b", sealed.manifest.units[1].sha256); return { status: 200, ok: true, text: async () => JSON.stringify({ warnings: {} }) }; }
    const name = String(url).split("/").pop();
    return held.has(name) ? { status: 200, ok: true, text: async () => JSON.stringify({ vers: "0.1.0", cksum: held.get(name) }) } : { status: 404, ok: false, text: async () => "" };
  };
  const doc = await executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: tmp("out"), dryRun: false, spawn: spy().spawn, fetchImpl, pause: 1, log: () => {} });
  assert.equal(puts.length, 1, "only rightkit-b is sent");
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.outcome]), [["rightkit-a", "published-by-earlier-attempt"], ["rightkit-b", "published"]]);
  held.set("rightkit-a", "f".repeat(64)); puts.length = 0;
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: tmp("out"), dryRun: false, spawn: spy().spawn, fetchImpl, pause: 1, log: () => {} }), /crates\.io already serves different bytes for rightkit-a@0\.1\.0/);
  assert.equal(puts.length, 0);
});

// ---- the ledger command is strict -------------------------------------------------------------

test("ledger records each verified publication, reads the ledger back, and fails on any incomplete recording", async () => {
  const root = tmp("ledger-root");
  const versionsFile = path.join(root, "packages", "release", "rightkit-versions.json");
  const copyFile = path.join(root, "packages", "build-control-policy", "src", "ownership-session-versions.json");
  fs.mkdirSync(path.dirname(versionsFile), { recursive: true }); fs.mkdirSync(path.dirname(copyFile), { recursive: true });
  fs.writeFileSync(versionsFile, "{}\n"); fs.writeFileSync(copyFile, "{}\n");
  const receiptFile = path.join(tmp("ledger-receipt"), "publication-receipt.json");
  const rows = [{ unit: "@rightkit/a", version: "0.1.0", published: true, registryVerified: true }, { unit: "@rightkit/b", version: "0.1.0", published: true, registryVerified: false }];
  fs.writeFileSync(receiptFile, JSON.stringify({ schema: 1, registry: "npm", dryRun: false, receipts: rows }));
  const compareVersions = (a, b) => (a === b ? 0 : a < b ? -1 : 1);
  const writer = ({ strictSeen = [], breakCopy = false, skipWrite = false, fail = false } = {}) => ({
    compareVersions,
    recordPublished: (entry, options) => {
      strictSeen.push(options.strict);
      if (fail) { if (options.strict) throw new Error("write failed"); return; }
      if (skipWrite) return;
      const doc = JSON.parse(fs.readFileSync(options.file, "utf8")); (doc.registry ??= {})[entry.registry] ??= {}; doc.registry[entry.registry][entry.name] = entry.version;
      const text = `${JSON.stringify(doc)}\n`; fs.writeFileSync(options.file, text); if (!breakCopy) fs.writeFileSync(copyFile, text);
    },
  });
  const seen = [];
  await commandLedger([receiptFile], { root, sync: writer({ strictSeen: seen }) });
  assert.deepEqual(seen, [true], "the unverified unit is skipped and the strict option is always on");
  assert.equal(JSON.parse(fs.readFileSync(versionsFile, "utf8")).registry.npm["@rightkit/a"], "0.1.0");
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer({ fail: true }) }), /write failed/);
  fs.writeFileSync(versionsFile, "{}\n"); fs.writeFileSync(copyFile, "{}\n");
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer({ skipWrite: true }) }), /ledger is incomplete[\s\S]*npm @rightkit\/a@0\.1\.0 is not in the ledger/);
  fs.writeFileSync(versionsFile, "{}\n");
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer({ breakCopy: true }) }), /ownership-session-versions\.json differs from rightkit-versions\.json/);
  fs.writeFileSync(receiptFile, JSON.stringify({ schema: 1, registry: "npm", dryRun: false, receipts: [rows[1]] }));
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer() }), /nothing to record/);
  fs.writeFileSync(receiptFile, JSON.stringify({ schema: 1, registry: "npm", dryRun: true, receipts: rows }));
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer() }), /dry-run receipt is not a publication/);
  await assert.rejects(commandLedger([], { root, sync: writer() }), /usage/);
});

test("child environments are an allow-list, never process.env wholesale", () => {
  const source = { PATH: "/bin", HOME: "/h", GITHUB_SHA: "s", RUNNER_TEMP: "/t", ACTIONS_ID_TOKEN_REQUEST_URL: "u", NODE_AUTH_TOKEN: "npm-secret-token", CARGO_REGISTRY_TOKEN: "crate-secret-token", GITHUB_TOKEN: "g", AWS_SECRET_ACCESS_KEY: "k", INPUT_UNITS: "x", npm_config_registry: "evil" };
  assert.deepEqual(Object.keys(childEnv(source)).sort(), ["ACTIONS_ID_TOKEN_REQUEST_URL", "GITHUB_SHA", "HOME", "PATH", "RUNNER_TEMP"]);
  assert.equal(childEnv(source, { NODE_AUTH_TOKEN: "npm-secret-token" }).NODE_AUTH_TOKEN, "npm-secret-token");
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
    name: "rightkit-a", version: "0.1.0", authors: ["A"], description: "d", license: "MIT", repository: "https://github.com/bogusyogi/rightkit", readme: "README.md", keywords: ["k"], categories: [], rust_version: "1.80",
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
    makeTgz(path.join(dest, `rightkit-${unit}-0.1.0.tgz`), "package", { "package.json": JSON.stringify(npmPackageJson(`@rightkit/${unit}`, `packages/${unit}`, badRepo ? { repository: { type: "git", url: "git+https://github.com/someone-else/rightkit.git", directory: `packages/${unit}` } } : {})), "dist/index.js": "x" });
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
  assert.throws(() => commandPack({ env: { ...env, NODE_AUTH_TOKEN: "npm-secret-token" }, spawn, root }), /pack and verify must not receive secrets/);
  assert.throws(() => commandPack({ env: { ...env, ...oidc }, spawn, root }), /must not request id-token/);
  assert.throws(() => commandPack({ env: { ...env, GITHUB_REF: "refs/heads/x" }, spawn, root }), /refs\/heads\/main only/);
  badRepo = true;
  assert.throws(() => commandPack({ env, spawn, root }), /must name https:\/\/github\.com\/bogusyogi\/rightkit/);
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
  assert.throws(() => commandPack({ env: { ...env, CARGO_REGISTRY_TOKEN: "crate-secret-token" }, spawn, root }), /must not receive secrets/);
});

test("verify re-hashes the sealed bytes, requires gate evidence, and binds the receipt to the manifest", () => {
  const sealed = sealNpm();
  const runTemp = tmp("verify");
  const evidenceDir = path.join(runTemp, "rightkit-ci");
  fs.mkdirSync(evidenceDir, { recursive: true });
  const writeEvidence = (patch = {}, status = "pass") => fs.writeFileSync(path.join(evidenceDir, "unit-evidence.json"), JSON.stringify({ schema: 1, host: "windows", registry: "npm", sha: SHA, units: [{ name: "@rightkit/a", status, steps: [] }], ...patch }));
  writeEvidence();
  const env = actionsEnv({ RUNNER_TEMP: runTemp, PUBLISH_PACK_DIR: sealed.dir });
  let consumerProblems = [];
  const consume = { npm: ({ units }) => new Map(units.map((u) => [u.name, { problems: [...consumerProblems], report: { entries: 1, imported: [u.name] } }])), crates: () => { throw new Error("not used"); } };
  const run = (over = {}, host = "windows") => commandVerify({ env: { ...env, ...over }, spawn: spy({ git: () => ({ status: 0, stdout: `${SHA}\n` }) }).spawn, host, consume });
  const receipt = run();
  assert.equal(receipt.host, "windows");
  assert.equal(receipt.manifestRunId, "77");
  assert.equal(receipt.manifestRunAttempt, "1");
  assert.equal(receipt.runAttempt, "1");
  assert.equal(receipt.units[0].consumer.verified, true);
  consumerProblems = ["@rightkit/a: ./dist/index.js did not resolve or import: boom"];
  assert.throws(() => run(), /did not resolve or import: boom/);
  consumerProblems = [];
  assert.throws(() => run({ GITHUB_RUN_ID: "78" }), /manifest run 77 is not this run 78/);
  assert.throws(() => run({ GITHUB_RUN_ATTEMPT: "" }), /GITHUB_RUN_ATTEMPT is not set/);
  assert.equal(receipt.manifestSha256, sha256(fs.readFileSync(path.join(sealed.dir, "manifest.json"))));
  assert.deepEqual(receipt.units.map((u) => [u.name, u.status, u.bytesVerified]), [["@rightkit/a", "pass", true]]);
  assert.ok(fs.existsSync(path.join(runTemp, "verify-receipt", "verify-windows.json")));
  assert.throws(() => run({ NODE_AUTH_TOKEN: "npm-secret-token" }), /must not receive secrets/);
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
  assert.equal(doc.jobs.a.steps[1].run, 'echo "# kept"\nif true; then\n  exit 1\nfi\n', "a literal block keeps one trailing newline");
  assert.equal(parseYaml("a: |-\n  x\n  y\n\nb: |\n  z\n\n\nc: d\n").a, "x\ny");
  assert.equal(parseYaml("b: |\n  z\n\n\nc: d\n").b, "z\n");
  assert.equal(doc.jobs.a.steps[1].env.E, "${{ secrets.T }}");
  assert.equal(doc.jobs.a.steps[2].run, "single line");
  for (const [bad, pattern] of [
    ["a:\n\tb: 1\n", /tab/], ["a: &x 1\n", /anchors/], ["a: 1\na: 2\n", /duplicate key/], ["a: one\n  two\n", /multi-line plain/],
    ["a: {b: 1}\n", /flow mappings/], ["a: [b, [c]]\n", /nested flow/], ["a: *x\n", /anchors/], ["a:\n  b: 1\n c: 2\n", /indent/],
    // the review's example: an indentation indicator makes YAML keep the newlines a naive parser folds away
    ["run: >1\n  node scripts/ci/upload.mjs\n  run\n", /block scalar header ">1"/],
    ["run: >\n  a\n  b\n", /block scalar header ">"/], ["run: >-\n  a\n", /block scalar header/], ["run: >+\n  a\n", /block scalar header/],
    ["run: |2\n   a\n", /block scalar header "\|2"/], ["run: |1-\n  a\n", /block scalar header/], ["run: |+\n  a\n", /block scalar header/], ["run: |-2\n  a\n", /block scalar header/],
    ["a:\n  - |\n    x\n", /block scalar header/], ["a:\n  - >\n    x\n", /block scalar header/],
    ["&x a: 1\n", /cannot parse/], ["a:\n  - &x b\n", /anchors/], ["a: !!str b\n", /anchors, aliases and tags/], ["<<: *x\n", /anchors/],
    ["a: [b,\n  c]\n", /multi-line plain|flow sequence must close/], ["a: [b\n", /flow sequence must close/], ["a: \"b\n  c\"\n", /multi-line plain|unterminated/],
    ["a:\t1\n", /tabs are not allowed/], ["a:\n  b: |\n    x\n\ty\n", /tabs are not allowed/],
    ["a:\n  b: 1\n  b: 2\n", /duplicate key "b"/], ["a: 1\n---\nb: 2\n", /cannot parse/], ["? a\n: b\n", /cannot parse/], ["a:\n  - - b\n", /indicator/],
    ["a: |\n  x\n  \n    \n  y\n", /whitespace-only line/],
  ]) assert.throws(() => parseYaml(bad), pattern, JSON.stringify(bad));
});

test("the real publish workflow parses, and it is the sealed-upload shape", () => {
  const text = fs.readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8");
  const doc = parseYaml(text);
  assert.deepEqual(Object.keys(doc.jobs), ["guard", "plan", "sign-swap", "pack", "verify", "seal-check", "upload"]);
  assert.deepEqual([...doc.jobs.verify.strategy.matrix.os].sort(), ["windows-2025", "xcode-27"]);
  assert.equal(doc.jobs.upload.environment, "release");
});

// ---- the publish contract: structural conditions, every secret expression form ------------------

const realDoc = () => parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8"));

test("the real publish workflow holds the contract and every mutation is rejected", () => {
  const doc = realDoc();
  assert.equal(assertPublishDoc(doc), true);
  assert.deepEqual(proveMutations(doc), []);
  const names = MUTATIONS.map(([name]) => name);
  for (const required of ["upload condition gains || true after the expression", "upload condition gains || true inside the expression", "pack reads a bracket-form secret", "verify reads a double-quoted bracket-form secret", "verify reads toJSON(secrets)", "pack reads the bare secrets context", "plan calls secrets: inherit", "workflow-level env holds a secret"]) assert.ok(names.includes(required), required);
});

test("the contract compares conditions exactly and finds every secret expression", () => {
  const broken = (edit) => { const doc = structuredClone(realDoc()); edit(doc); return () => assertPublishDoc(doc); };
  assert.throws(broken((d) => { d.jobs.upload.if = `${d.jobs.upload.if} || true`; }), /upload's condition must be exactly/);
  assert.throws(broken((d) => { d.jobs.upload.if = `  ${d.jobs.upload.if.replace(" }}", " || true }}")}`; }), /upload's condition must be exactly/);
  assert.doesNotThrow(broken((d) => { d.jobs.upload.if = `  ${d.jobs.upload.if}  `; }), "surrounding whitespace is not a different condition");
  for (const form of ["${{ secrets.X }}", "${{ secrets['X'] }}", '${{ secrets["X"] }}', "${{ toJSON(secrets) }}", "${{ secrets }}", "${{ SECRETS.X }}", "${{ format('{0}', secrets.X) }}"]) {
    for (const job of ["guard", "plan", "sign-swap", "pack", "verify", "seal-check"]) {
      assert.throws(broken((d) => { const step = d.jobs[job].steps.at(-1); step.env = { ...(step.env ?? {}), T: form }; }), /secrets context|secret/, `${job}: ${form}`);
    }
  }
  assert.throws(broken((d) => { d.jobs.verify.secrets = "inherit"; }), /secrets/);
});

test("the privileged upload job is closed: env, shell, working directory, step list, with keys and job keys", () => {
  const broken = (edit) => { const doc = structuredClone(realDoc()); edit(doc.jobs.upload); return () => assertPublishDoc(doc); };
  const npm = 5; const crates = 6;
  for (const key of ["NODE_OPTIONS", "NPM_CONFIG_SCRIPT_SHELL", "npm_config_registry", "JAVA_TOOL_OPTIONS", "PATH", "BASH_ENV", "LD_PRELOAD"]) {
    assert.throws(broken((u) => { u.steps[npm].env[key] = "x"; }), /never allowed|exactly the env keys/, `${key} on the npm step`);
    assert.throws(broken((u) => { u.steps[crates].env[key] = "x"; }), /exactly the env keys/, `${key} on the crates step`);
  }
  assert.throws(broken((u) => { u.steps[npm].shell = "bash"; }), /may carry only the keys/);
  assert.throws(broken((u) => { u.steps[npm]["working-directory"] = "/tmp"; }), /may carry only the keys/);
  assert.throws(broken((u) => { u.steps.push({ name: "x", run: "node scripts/ci/upload.mjs run" }); }), /must have exactly 8 steps/);
  assert.throws(broken((u) => { u.steps[1].with.cache = "npm"; }), /exactly the with keys/);
  assert.throws(broken((u) => { u.steps[2].with["run-id"] = "1"; }), /exactly the with keys/);
  for (const key of ["container", "services", "defaults", "env", "strategy", "outputs", "concurrency"]) assert.throws(broken((u) => { u[key] = {}; }), /upload may carry only the keys|no job-level env/, key);
  assert.doesNotThrow(broken(() => {}));
});

test("the YAML parser reads the real workflow's run blocks as the spec does", () => {
  const doc = realDoc();
  const upload = doc.jobs.upload.steps.find((s) => s.name === "Upload to npm");
  assert.equal(upload.run, "node scripts/ci/upload.mjs run");
  assert.match(doc.jobs.guard.steps[0].run, /exit 1\nfi\n$/);
});

// ---- shipped-byte consumer verification ------------------------------------------------------

test("package.json entry points are enumerated from exports, main, types and bin", () => {
  const d = declaredEntries({ name: "@rightkit/x", main: "dist/index.js", types: "dist/index.d.ts", bin: { x: "./cli/x.mjs" }, exports: { ".": { types: "./dist/index.d.ts", import: "./dist/index.js" }, "./styles.css": "./styles.css", "./only-types": { types: "./t.d.ts" }, "./feature/*": "./dist/feature/*.js", "./package.json": "./package.json" } });
  assert.deepEqual(d.subpaths.map((s) => [s.spec, s.runtime, s.types]), [
    ["@rightkit/x", ["dist/index.js"], ["dist/index.d.ts"]], ["@rightkit/x/styles.css", ["styles.css"], []], ["@rightkit/x/only-types", [], ["t.d.ts"]], ["@rightkit/x/package.json", ["package.json"], []],
  ]);
  assert.deepEqual(d.patterns, ["./feature/*"]);
  assert.deepEqual(d.bins, ["cli/x.mjs"]);
  for (const f of ["dist/index.js", "dist/index.d.ts", "cli/x.mjs", "styles.css", "t.d.ts"]) assert.ok(d.files.includes(f), f);
  assert.deepEqual(declaredEntries({ name: "p", main: "lib/a.mjs" }).subpaths.map((s) => [s.spec, s.runtime]), [["p", ["lib/a.mjs"]]]);
  assert.equal(declaredEntries({ name: "p", bin: "cli.mjs" }).subpaths.length, 0);
  assert.equal(declaredEntries({ name: "p", exports: { import: "./i.js", require: "./r.cjs" } }).subpaths[0].runtime.length, 2, "a conditions-only exports object is the root entry");
  const declared = declaredEntries({ name: "p", exports: { ".": "./a.js" } });
  assert.deepEqual(skipImportList({ name: "p", rightkit: { publishCheck: { skipImport: ["."] } } }, declared), { list: ["."], problems: [] });
  assert.match(skipImportList({ name: "p", rightkit: { publishCheck: { skipImport: ["./nope"] } } }, declared).problems.join(), /does not export/);
  assert.match(skipImportList({ name: "p", rightkit: { publishCheck: { skipImport: "." } } }, declared).problems.join(), /array/);
  assert.deepEqual(skipImportList({ name: "p" }, declared), { list: [], problems: [] });
});

test("consumeNpm runs each installed bin's declared safe command and requires its exit code", () => {
  const withBin = (smoke, extra = {}) => sealNpm({ extraFiles: { "bin/tool.mjs": "console.log(1);\n" }, packageJson: (name, dir) => npmPackageJson(name, dir, { bin: { tool: "./bin/tool.mjs" }, ...(smoke ? { rightkit: { publishCheck: { binSmoke: smoke } } } : {}), ...extra }) });
  const run = (sealed, spawnFake) => consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn: spawnFake.spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" }).get("@rightkit/a");
  // declared and passing: the bin runs from the installed copy, with node, no shell, the declared arguments
  const good = consumerSpawn();
  const ok = run(withBin({ tool: ["--version"] }), good);
  assert.deepEqual(ok.problems, []);
  const call = good.calls.find((c) => c.bin);
  assert.deepEqual(call.args.slice(1), ["--version"]);
  assert.ok(call.args[0].includes(`node_modules${path.sep}@rightkit${path.sep}a${path.sep}bin${path.sep}tool.mjs`), "the INSTALLED copy runs");
  assert.notEqual(call.options.shell, true);
  assert.deepEqual(ok.report.binSmoke, [{ bin: "tool", args: ["--version"], status: 0 }]);
  // a bin that exits non-zero fails the unit, with its output
  const broken = run(withBin({ tool: ["--version"] }), consumerSpawn({ bin: () => ({ status: 1, stdout: "", stderr: "Error: Cannot find module 'x'" }) }));
  assert.match(broken.problems.join(), /installed bin tool --version exited 1 \(expected 0\): Error: Cannot find module 'x'/);
  // a bin with no declaration, a declaration for a bin that does not exist, and a malformed one all fail
  assert.match(run(withBin(null), consumerSpawn()).problems.join(), /declares bin tool but no rightkit\.publishCheck\.binSmoke/);
  assert.match(run(withBin({ tool: ["--version"], ghost: ["--help"] }), consumerSpawn()).problems.join(), /names ghost, which package\.json does not declare as a bin/);
  assert.match(run(withBin({ tool: "--version" }), consumerSpawn()).problems.join(), /no argument array for bin tool/);
  // a CLI whose only safe call is its usage message declares the exit code and the text it must print
  const usage = { tool: { args: [], exitCode: 64, output: "usage: tool" } };
  assert.deepEqual(run(withBin(usage), consumerSpawn({ bin: () => ({ status: 64, stdout: "", stderr: "usage: tool <x>\n" }) })).problems, []);
  assert.match(run(withBin(usage), consumerSpawn({ bin: () => ({ status: 0, stdout: "", stderr: "" }) })).problems.join(), /exited 0 \(expected 64\)/);
  assert.match(run(withBin(usage), consumerSpawn({ bin: () => ({ status: 64, stdout: "", stderr: "something else" }) })).problems.join(), /did not print "usage: tool"/);
  // a string-form bin is named after the package
  assert.deepEqual(declaredEntries({ name: "@rightkit/a", bin: "./bin/tool.mjs" }).binMap, { a: "bin/tool.mjs" });
});

test("npm CLI is run through node when it ships beside it, else through the shim", () => {
  assert.deepEqual(npmCliCommand({ execPath: "/n/bin/node", platform: "linux", exists: (p) => p.replaceAll("\\", "/") === "/n/lib/node_modules/npm/bin/npm-cli.js" }).prefix.length, 1);
  assert.deepEqual(npmCliCommand({ execPath: "C:\\n\\node.exe", platform: "win32", exists: () => false }), { command: "npm.cmd", prefix: [], shell: true });
  assert.deepEqual(npmCliCommand({ execPath: "/n/bin/node", platform: "darwin", exists: () => false }), { command: "npm", prefix: [], shell: false });
});

/** A fake npm + node: `install` unpacks each tarball into node_modules/<name>; the check script answers from `answer`. */
function consumerSpawn({ installStatus = 0, answer = (plan) => plan.items.map((i) => ({ spec: i.spec, ok: true, imported: i.import })), bin = () => ({ status: 0, stdout: "1.0.0\n", stderr: "" }) } = {}) {
  const calls = [];
  const spawn = (command, args, options = {}) => {
    calls.push({ command, args: [...args], options });
    if (command === "fake-npm") {
      if (installStatus) return { status: installStatus, stdout: "", stderr: "E404 not found" };
      for (const tgz of args.filter((a) => a.endsWith(".tgz"))) {
        const manifest = JSON.parse(spawnSync("tar", ["-xOzf", path.basename(tgz), "package/package.json"], { cwd: path.dirname(tgz), encoding: "utf8" }).stdout);
        const dest = path.join(options.cwd, "node_modules", ...manifest.name.split("/"));
        fs.mkdirSync(dest, { recursive: true });
        assert.equal(spawnSync("tar", ["-xzf", path.basename(tgz), "--strip-components=1", "-C", tarDir(dest)], { cwd: path.dirname(tgz) }).status, 0);
      }
      return { status: 0, stdout: "", stderr: "" };
    }
    if (command === "fake-node" && !path.basename(args[0]).startsWith("check-")) {
      // an installed bin run for its smoke command
      calls.at(-1).bin = true;
      return bin(path.basename(args[0]), args.slice(1));
    }
    if (command === "fake-node") {
      const plan = JSON.parse(fs.readFileSync(args[1], "utf8"));
      calls.at(-1).plan = plan;
      return { status: 0, stdout: `noise\nRK_CONSUME ${JSON.stringify(answer(plan))}\n`, stderr: "" };
    }
    throw new Error(`unexpected child process: ${command}`);
  };
  return { spawn, calls };
}

test("consumeNpm installs every sealed tarball together with --ignore-scripts and imports each entry from the installed copy", () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const { spawn, calls } = consumerSpawn();
  const result = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn, env: { PATH: "x" }, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  const install = calls.find((c) => c.command === "fake-npm");
  assert.deepEqual(install.args.slice(0, 5), ["install", "--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock"]);
  assert.deepEqual(install.args.slice(5).map((a) => path.basename(a)), sealed.manifest.units.map((u) => u.file), "one install of all tarballs, so they resolve each other");
  assert.equal(install.options.env.npm_config_userconfig.endsWith("npmrc"), true);
  for (const unit of sealed.manifest.units) {
    const r = result.get(unit.name);
    assert.deepEqual(r.problems, []);
    assert.deepEqual(r.report.imported, [unit.name]);
  }
  // an entry that throws, one that does not resolve, a missing file and a failed install are all problems
  const failing = consumerSpawn({ answer: (plan) => plan.items.map((i) => ({ spec: i.spec, ok: false, imported: false, error: "ReferenceError: window is not defined" })) });
  const bad = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn: failing.spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.match(bad.get("@rightkit/a").problems.join(), /@rightkit\/a did not resolve or import: ReferenceError: window is not defined/);
  const dead = consumerSpawn({ installStatus: 1 });
  const none = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn: dead.spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.match(none.get("@rightkit/b").problems.join(), /npm install --ignore-scripts of the sealed tarballs failed: E404/);
  const noFile = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { main: "dist/gone.js", exports: { ".": "./dist/index.js" } }) });
  const m = consumeNpm({ units: noFile.manifest.units, packDir: noFile.dir, scratchRoot: tmp("scratch"), spawn: consumerSpawn().spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.match(m.get("@rightkit/a").problems.join(), /dist\/gone\.js, which the installed package does not contain/);
});

test("a package declares skipImport for subpaths that need a DOM host; the entry is still resolved", () => {
  const sealed = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { rightkit: { publishCheck: { skipImport: ["."] } } }) });
  const { spawn, calls } = consumerSpawn();
  const result = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.deepEqual(result.get("@rightkit/a").problems, []);
  assert.deepEqual(result.get("@rightkit/a").report.resolvedOnly, ["@rightkit/a"]);
  assert.equal(calls.find((c) => c.command === "fake-node").plan.items[0].import, false);
});

test("consumeCrates checks the packaged crate, patches request siblings, and compares the sealed upload metadata", () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const root = tmp("crates-consume");
  const checks = [];
  let failB = false;
  const pathEnv = { PATH: process.env.PATH ?? "", ...(process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {}) };
  const pkgOf = (name) => ({ name, version: "0.1.0", dependencies: name === "rightkit-b" ? [{ name: "rightkit-a", req: "^0.1", kind: null, optional: false }] : [], features: {}, authors: [], keywords: [], categories: [], readme: null });
  const cargo = (args, options) => {
    if (args[0] === "metadata") { const name = path.basename(path.dirname(args[args.indexOf("--manifest-path") + 1])).replace(/-0\.1\.0$/, ""); return { status: 0, stdout: JSON.stringify({ packages: [pkgOf(name)] }) }; }
    checks.push({ args, options });
    return { status: failB && args.join(" ").includes("rightkit-b") ? 101 : 0, stdout: "", stderr: "error[E0432]: unresolved import" };
  };
  const { spawn } = spy({ cargo });
  // the sealed metadata is what pack would derive from the packaged manifest
  for (const unit of sealed.manifest.units) fs.writeFileSync(path.join(sealed.dir, unit.metaFile), `${JSON.stringify(buildCrateUploadMetadata(pkgOf(unit.name), null))}\n`);
  const result = consumeCrates({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: root, spawn, env: pathEnv, uploadMetadata: buildCrateUploadMetadata });
  assert.deepEqual([...result.values()].flatMap((r) => r.problems), []);
  assert.equal(checks.length, 2);
  const [a, b] = checks.map((c) => c.args);
  assert.deepEqual(a.slice(0, 2), ["check", "--manifest-path"], "no Cargo.lock in the fixture, so no --locked");
  assert.ok(b.includes("--config") && b.some((x) => /^patch\.crates-io\.rightkit-a\.path=".*rightkit-a-0\.1\.0"$/.test(x)), "the sibling of the same request is patched in from its sealed crate");
  assert.ok(!b.includes("--locked"));
  assert.equal(checks[0].options.env.CARGO_TARGET_DIR.includes("rk-consume-crates-"), true);
  // metadata drift, a failing check, and a host exclusion
  fs.writeFileSync(path.join(sealed.dir, sealed.manifest.units[0].metaFile), `${JSON.stringify({ name: "rightkit-a", vers: "0.1.0", deps: [] })}\n`);
  checks.length = 0; failB = true;
  const drift = consumeCrates({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: root, spawn, env: pathEnv, uploadMetadata: buildCrateUploadMetadata });
  assert.match(drift.get("rightkit-a").problems.join(), /sealed upload metadata .* does not match the packaged Cargo\.toml/);
  assert.match(drift.get("rightkit-b").problems.join(), /cargo check of the packaged crate failed/);
  checks.length = 0;
  const excluded = consumeCrates({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: root, spawn, env: pathEnv, excluded: ["rightkit-a", "rightkit-b"], uploadMetadata: buildCrateUploadMetadata });
  assert.equal(checks.length, 0);
  assert.equal(excluded.get("rightkit-a").report.checked, false);
});

test("a packaged Cargo.lock is honoured with --locked when no sibling needs patching", () => {
  const sealed = sealCrates(["rightkit-a"]);
  const dir = tmp("lock");
  makeTgz(path.join(dir, "rightkit-a-0.1.0.crate"), "rightkit-a-0.1.0", { "Cargo.toml": "x", "Cargo.lock": "# lock\n", "src/lib.rs": "" });
  const unit = { ...sealed.manifest.units[0], file: "rightkit-a-0.1.0.crate", metaFile: sealed.manifest.units[0].metaFile };
  fs.copyFileSync(path.join(sealed.dir, unit.metaFile), path.join(dir, unit.metaFile));
  const seen = [];
  const cargo = (args) => (args[0] === "metadata" ? { status: 0, stdout: JSON.stringify({ packages: [{ name: "rightkit-a", version: "0.1.0", dependencies: [], features: {}, authors: [], keywords: [], categories: [], readme: null }] }) } : (seen.push(args), { status: 0 }));
  const r = consumeCrates({ units: [unit], packDir: dir, scratchRoot: tmp("scratch"), spawn: spy({ cargo }).spawn, env: { PATH: process.env.PATH ?? "" }, uploadMetadata: () => ({ name: "rightkit-a", vers: "0.1.0", deps: [], features: {} }) });
  assert.deepEqual(r.get("rightkit-a").problems, []);
  assert.ok(seen[0].includes("--locked"));
  assert.equal(r.get("rightkit-a").report.locked, true);
});

// ---- assets, allow inputs and Chrome for Testing -------------------------------------------------

test("a unit that needs unprovisioned assets must be named in allow_missing_assets, and only such units may be", async () => {
  const root = fixture();
  fs.writeFileSync(path.join(root, "ci", "asset-gated.json"), JSON.stringify({ crates: { "rightkit-asr": "models" }, npm: { "@rightkit/qa": "checkouts" } }));
  const workspace = new Map([crate("rightkit-asr", "0.1.0"), crate("rightkit-plain", "0.1.0")]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-asr", dryRun: true, root, workspace, fetchImpl: registry({}) }), /rightkit-asr: its tests need assets this workflow does not provision \(models\); name it in allow_missing_assets/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-asr rightkit-plain", allowMissingAssetsText: "rightkit-asr", allowIgnoredText: "rightkit-plain", dryRun: true, root, workspace, fetchImpl: registry({}) });
  assert.deepEqual([ok.allowMissingAssets, ok.allowIgnored], [["rightkit-asr"], ["rightkit-plain"]]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-plain", allowMissingAssetsText: "rightkit-plain", dryRun: true, root, workspace, fetchImpl: registry({}) }), /allow_missing_assets names rightkit-plain, which ci\/asset-gated\.json does not list/);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-plain", allowIgnoredText: "rightkit-other", dryRun: true, root, workspace, fetchImpl: registry({}) }), /allow_ignored names .*rightkit-other.*not units of this request/);
});

test("ci/asset-gated.json lists every crate and package whose tests read RIGHTKIT_REQUIRE_ASSETS", () => {
  const repo = fileURLToPath(new URL("../..", import.meta.url));
  const gated = JSON.parse(fs.readFileSync(path.join(repo, "ci", "asset-gated.json"), "utf8"));
  const reads = (dir) => {
    const hits = [];
    const walk = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { if (["node_modules", "target", "dist", ".git"].includes(e.name)) continue; const full = path.join(d, e.name); if (e.isDirectory()) walk(full); else if (/\.(rs|mjs|ts|tsx|js)$/.test(e.name) && fs.readFileSync(full, "utf8").includes("RIGHTKIT_REQUIRE_ASSETS")) hits.push(full); } };
    walk(dir);
    return hits.length > 0;
  };
  for (const entry of fs.readdirSync(path.join(repo, "crates"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const toml = path.join(repo, "crates", entry.name, "Cargo.toml");
    if (!fs.existsSync(toml)) continue;
    const name = /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/ms.exec(fs.readFileSync(toml, "utf8"))?.[1];
    if (reads(path.join(repo, "crates", entry.name))) assert.ok(Object.hasOwn(gated.crates, name), `${name} reads RIGHTKIT_REQUIRE_ASSETS but is not in ci/asset-gated.json`);
  }
  for (const entry of fs.readdirSync(path.join(repo, "packages"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = path.join(repo, "packages", entry.name, "package.json");
    if (!fs.existsSync(manifest)) continue;
    const name = JSON.parse(fs.readFileSync(manifest, "utf8")).name;
    if (reads(path.join(repo, "packages", entry.name))) assert.ok(Object.hasOwn(gated.npm, name), `${name} reads RIGHTKIT_REQUIRE_ASSETS but is not in ci/asset-gated.json`);
  }
});

test("the Chrome for Testing pin: URL derives from the version; an empty sha256 can only be recorded in a dry run", async () => {
  const pins = { version: "130.0.6723.58", platforms: { win64: { sha256: "a".repeat(64) }, "mac-arm64": { sha256: "" } } };
  assert.deepEqual(pinFor(pins, "win64"), { url: "https://storage.googleapis.com/chrome-for-testing-public/130.0.6723.58/win64/chrome-win64.zip", sha256: "a".repeat(64) });
  assert.equal(pinFor(pins, "mac-arm64").sha256, "");
  assert.throws(() => pinFor({ version: "", platforms: {} }, "win64"), /no pinned version/);
  assert.throws(() => pinFor(pins, "linux64"), /no entry for linux64/);
  assert.throws(() => pinFor({ ...pins, platforms: { win64: { sha256: "XYZ" } } }, "win64"), /not 64 lowercase hex/);
  const body = Buffer.from("chrome bytes");
  const fetchImpl = async (url) => ({ ok: true, status: 200, body: (async function* () { yield body.subarray(0, 5); yield body.subarray(5); })(), url });
  const tmpDir = tmp("pin");
  const line = await recordPin({ version: pins.version, key: "mac-arm64", url: pinFor(pins, "mac-arm64").url, dryRun: true, fetchImpl, tmpDir });
  assert.equal(line, `CHROME_PIN mac-arm64 130.0.6723.58 ${sha256(body)} ${body.length}`);
  assert.equal(fs.existsSync(tmpDir), false, "the downloaded file is deleted");
  await assert.rejects(recordPin({ version: pins.version, key: "mac-arm64", url: "u", dryRun: false, fetchImpl: async () => { throw new Error("must not download"); }, tmpDir: tmp("pin") }), /not a dry run/);
  const real = JSON.parse(fs.readFileSync(new URL("../../ci/chrome-for-testing.json", import.meta.url), "utf8"));
  assert.deepEqual(Object.keys(real.platforms).sort(), ["mac-arm64", "win64"]);
});
