import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { crateIndexPath, parseCargoManifest, parseUnitNames, planPublication, satisfies } from "./publish.mjs";

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

test("npm requirements: bare is exact, ranges join with spaces and ||", () => {
  assert.equal(satisfies("0.1.6", "0.1.6", "npm"), true);
  assert.equal(satisfies("0.1.7", "0.1.6", "npm"), false);
  assert.equal(satisfies("0.2.5", ">=0.2.0 <1", "npm"), true);
  assert.equal(satisfies("1.0.0", ">=0.2.0 <1", "npm"), false);
  assert.equal(satisfies("0.2.5", "^0.2.5", "npm"), true);
  assert.equal(satisfies("0.3.0", "^0.2.5", "npm"), false);
  assert.equal(satisfies("2.1.0", "^1.0.0 || ^2.0.0", "npm"), true);
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
});

test("Cargo manifest reader finds publish, versions, target and table dependencies", () => {
  const manifest = parseCargoManifest(`[package]
name = "rightkit-x"
version = "0.2.0" # current
publish = false

[dependencies]
rightkit-a = { version = "0.3", path = "../rightkit-a", optional = true }
serde.workspace = true
plain = "1.0" # comment

[target.'cfg(unix)'.dependencies]
rightkit-b = { path = "../rightkit-b", version = "0.1.0" }

[dev-dependencies]
rightkit-c = { path = "../rightkit-c" }

[dependencies.rightkit-d]
version = "2"
path = "../rightkit-d"
`);
  assert.equal(manifest.name, "rightkit-x");
  assert.equal(manifest.version, "0.2.0");
  assert.equal(manifest.publishable, false);
  const byName = Object.fromEntries(manifest.deps.map((d) => [d.name, d]));
  assert.equal(byName["rightkit-a"].spec, "0.3");
  assert.equal(byName["rightkit-a"].optional, true);
  assert.equal(byName.plain.spec, "1.0");
  assert.equal(byName["rightkit-b"].spec, "0.1.0");
  assert.equal(byName["rightkit-c"].dev, true);
  assert.equal(byName["rightkit-c"].spec, null);
  assert.equal(byName["rightkit-d"].spec, "2");
  assert.throws(() => parseCargoManifest('[package]\nname = "rightkit-y"\nversion = "1.0.0"\n[dependencies]\nrightkit-z = {\n version = "1" }\n'), /multi-line/);
});

// ---- planning against a fixture workspace and a fake registry ---------------------------------

function fixture(redKeys = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-publish-test-"));
  fs.mkdirSync(path.join(root, "ci"), { recursive: true });
  fs.writeFileSync(path.join(root, "ci", "known-red.json"), JSON.stringify({ macos: redKeys, windows: {} }));
  fs.writeFileSync(path.join(root, "ci", "gate.config.json"), JSON.stringify({ rust: { "rightkit-win": { os: ["windows"] } } }));
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

test("plan refuses a dependency that is neither on the registry nor in the request", async () => {
  const workspace = new Map([crate("rightkit-a", "0.1.0"), crate("rightkit-b", "0.2.0", [dep("rightkit-a", "0.1.0")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /rightkit-a 0\.1\.0 is not on crates\.io/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-a": [{ vers: "0.1.5" }] }) });
  assert.deepEqual(ok.units.map((u) => u.name), ["rightkit-b"]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-a": [{ vers: "0.1.5", yanked: true }] }) }), /not on crates\.io/);
});

test("plan refuses private, publish=false, known-red, windows-only, path-only and unknown units", async () => {
  const workspace = new Map([
    crate("rightkit-private", "0.1.0", [], { private: true }),
    crate("rightkit-red", "0.1.0"),
    crate("rightkit-win", "0.1.0"),
    crate("rightkit-pathonly", "0.1.0", [dep("rightkit-red", null)]),
    crate("rightkit-pre", "0.1.0-beta.1"),
  ]);
  const root = fixture({ "rust:rightkit-red": "defect" });
  for (const [unit, pattern] of [
    ["rightkit-private", /publish = false/], ["rightkit-red", /known-red/], ["rightkit-win", /macOS runner/],
    ["rightkit-pathonly", /no version requirement/], ["rightkit-pre", /plain x\.y\.z/], ["rightkit-missing", /not a crate/],
  ]) {
    await assert.rejects(planPublication({ registry: "crates", unitsText: unit, dryRun: true, root, workspace, fetchImpl: registry({}) }), pattern, unit);
  }
});

test("plan detects dependency cycles and npm workspace specifiers", async () => {
  const cyc = new Map([crate("rightkit-a", "0.1.0", [dep("rightkit-b", "0.1.0")]), crate("rightkit-b", "0.1.0", [dep("rightkit-a", "0.1.0")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-a rightkit-b", dryRun: true, root: fixture(), workspace: cyc, fetchImpl: registry({}) }), /cycle/);
  const pkg = (name, version, deps = [], extra = {}) => [name, { name, version, dir: `packages/${name.split("/")[1]}`, private: false, deps, ...extra }];
  const npm = new Map([
    pkg("@rightkit/base", "0.2.0"),
    pkg("@rightkit/top", "0.1.0", [{ name: "@rightkit/base", spec: "workspace:*", dev: false }, { name: "@rightkit/peer", spec: ">=0.1.0 <1", dev: false }]),
    pkg("@rightkit/peer", "0.1.0"),
  ]);
  const fetchImpl = async (url) => ({ status: 404, ok: false, text: async () => "" , url });
  const plan = await planPublication({ registry: "npm", unitsText: "@rightkit/top @rightkit/peer @rightkit/base", dryRun: false, root: fixture(), workspace: npm, fetchImpl });
  assert.deepEqual(plan.units.map((u) => u.name), ["@rightkit/peer", "@rightkit/base", "@rightkit/top"]); // dependencies first, ties in the order requested
  await assert.rejects(planPublication({ registry: "npm", unitsText: "@rightkit/top", dryRun: false, root: fixture(), workspace: npm, fetchImpl }), /@rightkit\/base@0\.2\.0 \(workspace version\) is not on npm/);
});
