import assert from "node:assert/strict";
import test from "node:test";
import { changedConfigCrates, changedLockNames, parseLock, reach, selectByPaths, selectNpm, selectRust, selectUnits } from "./select.mjs";

const LOCK = `version = 4

[[package]]
name = "leaf"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aa"

[[package]]
name = "rightkit-a"
version = "0.1.0"
dependencies = [
 "leaf",
]

[[package]]
name = "rightkit-b"
version = "0.1.0"
dependencies = [
 "rightkit-a",
 "other 2.0.0 (registry+https://github.com/rust-lang/crates.io-index)",
]

[[package]]
name = "other"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "bb"

[[package]]
name = "rightkit-c"
version = "0.1.0"
`;
const members = [
  { name: "rightkit-a", dir: "crates/rightkit-a" },
  { name: "rightkit-b", dir: "crates/rightkit-b" },
  { name: "rightkit-c", dir: "crates/rightkit-c", extraPaths: ["contracts"] },
];
const lock = parseLock(LOCK);

test("lock graph reaches transitive dependencies by name", () => {
  assert.deepEqual([...reach(lock, "rightkit-b")].sort(), ["leaf", "other", "rightkit-a"]);
});

test("a changed crate is tested and its dependents are checked", () => {
  const picked = selectRust({ changedPaths: ["crates/rightkit-a/src/lib.rs"], members, lock });
  assert.deepEqual(picked, { all: false, test: ["rightkit-a"], check: ["rightkit-b"] });
});

test("changelog-only and evidence-only edits select nothing", () => {
  const picked = selectRust({ changedPaths: ["crates/rightkit-a/CHANGELOG.md", "crates/rightkit-a/runtime-budget.evidence/x.txt"], members, lock });
  assert.deepEqual(picked, { all: false, test: [], check: [] });
});

test("a crate directory that only shares a name prefix is not selected", () => {
  const picked = selectRust({ changedPaths: ["crates/rightkit-ab/src/lib.rs"], members, lock });
  assert.deepEqual(picked.test, []);
});

test("a changed third-party package selects every crate that compiles it", () => {
  const next = parseLock(LOCK.replace('checksum = "aa"', 'checksum = "cc"'));
  const picked = selectRust({ changedPaths: ["Cargo.lock"], members, lock: next, lockChanged: changedLockNames(lock, next) });
  assert.deepEqual(picked, { all: false, test: ["rightkit-a", "rightkit-b"], check: [] });
});

test("extra paths and config edits select their crate; the workspace manifest selects all", () => {
  assert.deepEqual(selectRust({ changedPaths: ["contracts/x.schema.json"], members, lock }).test, ["rightkit-c"]);
  assert.deepEqual(selectRust({ changedPaths: [], members, lock, configChanged: new Set(["rightkit-b"]) }).test, ["rightkit-b"]);
  assert.equal(selectRust({ changedPaths: ["Cargo.toml"], members, lock }).all, true);
  assert.deepEqual([...changedConfigCrates({ rust: { a: { os: ["macos"] } } }, { rust: { a: {}, b: {} } })].sort(), ["a", "b"]);
});

test("npm selection follows dependents; path suites match by directory", () => {
  const packages = [
    { name: "@rightkit/ui", dir: "packages/ui", deps: [] },
    { name: "@rightkit/shell", dir: "packages/shell", deps: ["@rightkit/ui"] },
    { name: "@rightkit/ci", dir: "packages/ci", deps: [] },
  ];
  assert.deepEqual(selectNpm({ changedPaths: ["packages/ui/src/a.ts"], packages }).test, ["packages/shell", "packages/ui"]);
  assert.deepEqual(selectNpm({ changedPaths: ["docs/x.md"], packages }).test, []);
  assert.equal(selectNpm({ changedPaths: ["pnpm-lock.yaml"], packages }).all, true);
  const suites = [{ id: "swift/A", paths: ["swift/A"] }, { id: "swift/AB", paths: ["swift/AB"] }];
  assert.deepEqual(selectByPaths({ changedPaths: ["swift/AB/x.swift"], suites }), ["swift/AB"]);
});

test("explicit unit selection names exactly the requested units, in order, with no dependents", () => {
  assert.deepEqual(selectUnits({ registry: "crates", units: ["rightkit-b", "rightkit-a", "rightkit-b"], members }).rust, { all: false, test: ["rightkit-b", "rightkit-a"], check: [] });
  const packages = [
    { name: "@rightkit/ui", dir: "packages/ui", scripts: { test: "vitest" }, deps: [] },
    { name: "@rightkit/shell", dir: "packages/shell", scripts: { test: "vitest" }, deps: ["@rightkit/ui"] },
  ];
  assert.deepEqual(selectUnits({ registry: "npm", units: ["@rightkit/ui"], packages }).npm, { all: false, test: ["packages/ui"] });
  assert.deepEqual(selectUnits({ registry: "npm", units: ["@rightkit/ui"], packages }).rust.test, []);
  assert.throws(() => selectUnits({ registry: "crates", units: ["nope"], members }), /unknown crate/);
  assert.throws(() => selectUnits({ registry: "npm", units: ["@rightkit/nope"], packages }), /unknown npm package/);
  assert.throws(() => selectUnits({ registry: "crates", units: [], members }), /no units/);
  assert.throws(() => selectUnits({ registry: "pip", units: ["x"] }), /npm or crates/);
});

test("a unit that a host would skip fails the selection unless the request names it", () => {
  const config = { rust: { "rightkit-a": { os: ["macos"] }, "rightkit-b": {} } };
  const base = { registry: "crates", units: ["rightkit-a", "rightkit-b"], members, config };
  assert.throws(() => selectUnits({ ...base, host: "windows" }), /rightkit-a \(restricted to macos\) would be skipped on windows/);
  const allowed = selectUnits({ ...base, host: "windows", allowHostExclusion: ["rightkit-a"] });
  assert.deepEqual(allowed.excluded, ["rightkit-a"]);
  assert.deepEqual(allowed.rust.test, ["rightkit-b"]);
  assert.deepEqual(selectUnits({ ...base, host: "macos" }).rust.test, ["rightkit-a", "rightkit-b"]);
  assert.throws(() => selectUnits({ ...base, host: "macos", allowHostExclusion: ["rightkit-zzz"] }), /not units of this request/);
  assert.throws(() => selectUnits({ ...base, host: "macos", allowNoTest: ["rightkit-a"] }), /npm packages only/);
});

test("an npm package without a test script fails the selection unless allow_no_test names it", () => {
  const packages = [
    { name: "@rightkit/ax", dir: "packages/ax", scripts: {}, deps: [] },
    { name: "@rightkit/ui", dir: "packages/ui", scripts: { test: "vitest" }, deps: [] },
  ];
  assert.throws(() => selectUnits({ registry: "npm", units: ["@rightkit/ax", "@rightkit/ui"], packages }), /@rightkit\/ax has no test script/);
  const ok = selectUnits({ registry: "npm", units: ["@rightkit/ax", "@rightkit/ui"], packages, allowNoTest: ["@rightkit/ax"] });
  assert.deepEqual(ok.noTest, ["@rightkit/ax"]);
  assert.deepEqual(ok.npm.test, ["packages/ui"]);
  assert.throws(() => selectUnits({ registry: "npm", units: ["@rightkit/ui"], packages, allowNoTest: ["@rightkit/ui"] }), /not units|has a test script/);
  assert.throws(() => selectUnits({ registry: "npm", units: ["@rightkit/ui"], packages, allowHostExclusion: ["@rightkit/ui"] }), /crates only/);
});

test("asset-gated units need allow_missing_assets, and allow_ignored / allow_missing_assets must name requested units", () => {
  const assetGated = { crates: { "rightkit-a": "models" }, npm: {} };
  const base = { registry: "crates", units: ["rightkit-a", "rightkit-b"], members, assetGated };
  assert.throws(() => selectUnits(base), /rightkit-a need assets the publish workflow does not provision.*allow_missing_assets/);
  assert.deepEqual(selectUnits({ ...base, allowMissingAssets: ["rightkit-a"] }).rust.test, ["rightkit-a", "rightkit-b"]);
  assert.throws(() => selectUnits({ ...base, allowMissingAssets: ["rightkit-a", "rightkit-b"] }), /allow_missing_assets names rightkit-b, which ci\/asset-gated\.json does not list/);
  assert.throws(() => selectUnits({ ...base, allowMissingAssets: ["rightkit-a"], allowIgnored: ["rightkit-zzz"] }), /allow_ignored names rightkit-zzz, which are not units/);
  assert.throws(() => selectUnits({ ...base, allowMissingAssets: ["rightkit-a", "rightkit-zzz"] }), /allow_missing_assets names rightkit-zzz, which are not units/);
  assert.deepEqual(selectUnits({ registry: "crates", units: ["rightkit-b"], members, allowIgnored: ["rightkit-b"] }).rust.test, ["rightkit-b"]);
});
