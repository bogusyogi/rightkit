import assert from "node:assert/strict";
import test from "node:test";
import { changedConfigCrates, changedLockNames, parseLock, pnpmBuildClosureArgs, reach, selectByPaths, selectNpm, selectRust, selectUnits } from "./select.mjs";

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

test("test-result parsers, CI provisioners and their pinned inputs invalidate both registries", () => {
  for (const path of ["scripts/ci/test-results.mjs", "scripts/ci/provision-chrome.mjs", "scripts/ci/provision-ort.mjs", "ci/chrome-for-testing.json", "crates/rightkit-ffmpeg/catalog/native-libraries.json"]) {
    assert.equal(selectRust({ changedPaths: [path], members, lock }).all, true, `Rust full selection for ${path}`);
    assert.equal(selectNpm({ changedPaths: [path], packages: [] }).all, true, `npm full selection for ${path}`);
  }
});

test("npm selection follows dependents; path suites match by directory", () => {
  const packages = [
    { name: "@rightkit/ui", dir: "packages/ui", deps: [] },
    { name: "@rightkit/tauri-shell", dir: "packages/tauri-shell", deps: ["@rightkit/ui"] },
    { name: "@rightkit/ci", dir: "packages/ci", deps: [] },
  ];
  assert.deepEqual(selectNpm({ changedPaths: ["packages/ui/src/a.ts"], packages }).test, ["packages/tauri-shell", "packages/ui"]);
  assert.deepEqual(selectNpm({ changedPaths: ["docs/x.md"], packages }).test, []);
  assert.equal(selectNpm({ changedPaths: ["pnpm-lock.yaml"], packages }).all, true);
  const suites = [{ id: "swift/A", paths: ["swift/A"] }, { id: "swift/AB", paths: ["swift/AB"] }];
  assert.deepEqual(selectByPaths({ changedPaths: ["swift/AB/x.swift"], suites }), ["swift/AB"]);
});

test("explicit unit selection preserves named-unit order and keeps selected crates out of dependent checks", () => {
  assert.deepEqual(selectUnits({ registry: "crates", units: ["rightkit-b", "rightkit-a", "rightkit-b"], members, lock }).rust, { all: false, test: ["rightkit-b", "rightkit-a"], check: [] });
  const packages = [
    { name: "@rightkit/ui", dir: "packages/ui", scripts: { test: "vitest" }, deps: [] },
    { name: "@rightkit/tauri-shell", dir: "packages/tauri-shell", scripts: { test: "vitest" }, deps: ["@rightkit/ui"] },
  ];
  assert.deepEqual(selectUnits({ registry: "npm", units: ["@rightkit/ui"], packages }).npm, { all: false, test: ["packages/ui"] });
  assert.deepEqual(selectUnits({ registry: "npm", units: ["@rightkit/ui"], packages }).rust.test, []);
  assert.throws(() => selectUnits({ registry: "crates", units: ["nope"], members, lock }), /unknown crate/);
  assert.throws(() => selectUnits({ registry: "npm", units: ["@rightkit/nope"], packages }), /unknown npm package/);
  assert.throws(() => selectUnits({ registry: "crates", units: [], members }), /no units/);
  assert.throws(() => selectUnits({ registry: "pip", units: ["x"] }), /npm or crates/);
});

test("explicit crate selection tests named units and checks every transitive workspace dependent", () => {
  const graph = parseLock(`
[[package]]
name = "rightkit-logs"
version = "0.3.4"

[[package]]
name = "rightkit-crash"
version = "0.3.4"
dependencies = ["rightkit-logs"]

[[package]]
name = "rightkit-job-queue"
version = "0.4.1"
dependencies = ["rightkit-crash"]

[[package]]
name = "rightkit-tauri-shell"
version = "0.7.0"
dependencies = ["rightkit-job-queue"]

[[package]]
name = "rightkit-independent"
version = "0.1.0"
`);
  const chain = ["rightkit-logs", "rightkit-crash", "rightkit-job-queue", "rightkit-tauri-shell", "rightkit-independent"]
    .map((name) => ({ name, dir: `crates/${name}` }));
  const picked = selectUnits({ registry: "crates", units: ["rightkit-logs"], members: chain, lock: graph });
  assert.deepEqual(picked.rust, { all: false, test: ["rightkit-logs"], check: ["rightkit-crash", "rightkit-job-queue", "rightkit-tauri-shell"] });
  assert.ok(!picked.rust.test.includes("rightkit-crash"), "reverse dependents are checks, not full tests");
  assert.ok(!picked.rust.check.includes("rightkit-independent"), "unrelated workspace crates stay out of the request");
});

test("crate selection refuses incomplete graphs and host-restricted reverse checks", () => {
  assert.throws(() => selectUnits({ registry: "crates", units: ["rightkit-a"], members, host: "windows" }), /requires the parsed Cargo\.lock workspace graph/);
  assert.throws(() => selectUnits({ registry: "crates", units: ["rightkit-a"], members, lock: new Map(), host: "windows" }), /Cargo\.lock workspace graph is incomplete; missing member/);

  const chainLock = parseLock(`
[[package]]
name = "rightkit-logs"
version = "1"

[[package]]
name = "rightkit-crash"
version = "1"
dependencies = ["rightkit-logs"]
`);
  assert.throws(() => selectUnits({
    registry: "crates", units: ["rightkit-logs"],
    members: [{ name: "rightkit-logs", dir: "crates/rightkit-logs" }, { name: "rightkit-crash", dir: "crates/rightkit-crash" }],
    lock: chainLock, host: "windows", config: { rust: { "rightkit-crash": { os: ["macos"] } } },
  }), /reverse-dependent check would be skipped on windows: rightkit-crash/);
});

test("a unit that a host would skip fails the selection unless the request names it", () => {
  const config = { rust: { "rightkit-a": { os: ["macos"] }, "rightkit-b": {} } };
  const base = { registry: "crates", units: ["rightkit-a", "rightkit-b"], members, lock, config };
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
  const base = { registry: "crates", units: ["rightkit-a", "rightkit-b"], members, lock, assetGated };
  assert.throws(() => selectUnits(base), /rightkit-a need assets the publish workflow does not provision.*allow_missing_assets/);
  assert.deepEqual(selectUnits({ ...base, allowMissingAssets: ["rightkit-a"] }).rust.test, ["rightkit-a", "rightkit-b"]);
  assert.throws(() => selectUnits({ ...base, allowMissingAssets: ["rightkit-a", "rightkit-b"] }), /allow_missing_assets names rightkit-b, which ci\/asset-gated\.json does not list/);
  assert.throws(() => selectUnits({ ...base, allowMissingAssets: ["rightkit-a"], allowIgnored: ["rightkit-zzz"] }), /allow_ignored names rightkit-zzz, which are not units/);
  assert.throws(() => selectUnits({ ...base, allowMissingAssets: ["rightkit-a", "rightkit-zzz"] }), /allow_missing_assets names rightkit-zzz, which are not units/);
  assert.deepEqual(selectUnits({ registry: "crates", units: ["rightkit-b"], members, lock, allowIgnored: ["rightkit-b"] }).rust.test, ["rightkit-b"]);
});

test("the build closure tolerates a unit without a build script and keeps --if-present ahead of the script name", () => {
  const args = pnpmBuildClosureArgs(["@rightkit/release", "@rightkit/logs", "@rightkit/release"]);
  assert.deepEqual(args, ["--workspace-concurrency=1", "--if-present", "--filter", "@rightkit/release...", "--filter", "@rightkit/logs...", "run", "build"]);
  // pnpm parses options placed before the command; anything after the script name is forwarded to the script.
  assert.ok(args.indexOf("--if-present") < args.indexOf("run"));
  assert.equal(args.filter((a) => a === "--if-present").length, 1);
});
