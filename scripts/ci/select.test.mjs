import assert from "node:assert/strict";
import test from "node:test";
import { changedConfigCrates, changedLockNames, parseLock, reach, selectByPaths, selectNpm, selectRust } from "./select.mjs";

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
    { name: "@rightkit/git", dir: "packages/git", deps: [] },
  ];
  assert.deepEqual(selectNpm({ changedPaths: ["packages/ui/src/a.ts"], packages }).test, ["packages/shell", "packages/ui"]);
  assert.deepEqual(selectNpm({ changedPaths: ["docs/x.md"], packages }).test, []);
  assert.equal(selectNpm({ changedPaths: ["pnpm-lock.yaml"], packages }).all, true);
  const suites = [{ id: "swift/A", paths: ["swift/A"] }, { id: "swift/AB", paths: ["swift/AB"] }];
  assert.deepEqual(selectByPaths({ changedPaths: ["swift/AB/x.swift"], suites }), ["swift/AB"]);
});
