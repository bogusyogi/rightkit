import assert from "node:assert/strict";
import test from "node:test";
import { reverseDependents } from "../../packages/release/crate-workspaces.mjs";
import { changedConfigCrates, parseLock, pnpmBuildClosureArgs, reach, selectByPaths, selectNpm, selectRust, selectUnits } from "./select.mjs";

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
// Path dependencies between the crates' own workspaces (crate-workspaces.mjs pathDependencies): b -> a.
const deps = new Map([["rightkit-a", new Set()], ["rightkit-b", new Set(["rightkit-a"])], ["rightkit-c", new Set()]]);
const dependents = (names) => reverseDependents(names, deps);

test("lock graph reaches transitive dependencies by name", () => {
  assert.deepEqual([...reach(lock, "rightkit-b")].sort(), ["leaf", "other", "rightkit-a"]);
});

test("a changed crate is tested and its dependents are checked", () => {
  const picked = selectRust({ changedPaths: ["crates/rightkit-a/src/lib.rs"], members, dependents });
  assert.deepEqual(picked, { all: false, test: ["rightkit-a"], check: ["rightkit-b"] });
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-b/src/lib.rs"], members, dependents }), { all: false, test: ["rightkit-b"], check: [] }, "a dependency is not reached by its dependent's change");
});

test("changelog-only and evidence-only edits select nothing", () => {
  const picked = selectRust({ changedPaths: ["crates/rightkit-a/CHANGELOG.md", "crates/rightkit-a/runtime-budget.evidence/x.txt"], members, dependents });
  assert.deepEqual(picked, { all: false, test: [], check: [] });
});

test("a crate directory that only shares a name prefix is not selected", () => {
  const picked = selectRust({ changedPaths: ["crates/rightkit-ab/src/lib.rs"], members, dependents });
  assert.deepEqual(picked.test, []);
});

test("each crate has its own lock: a lock or manifest change selects that crate and its path dependents only", () => {
  // crates/rightkit-a/Cargo.lock holds leaf; rightkit-b's lock is a separate file, so a leaf bump in a's lock never selects b as a test.
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-a/Cargo.lock"], members, dependents }), { all: false, test: ["rightkit-a"], check: ["rightkit-b"] });
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-b/Cargo.lock", "crates/rightkit-c/Cargo.toml"], members, dependents }), { all: false, test: ["rightkit-b", "rightkit-c"], check: [] });
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-c/deny.toml"], members, dependents }), { all: false, test: ["rightkit-c"], check: [] });
});

test("per-crate selection: transitive path dependents are checked, never tested; no root manifest or lock exists to select everything", () => {
  const chain = ["rightkit-x", "rightkit-y", "rightkit-z", "rightkit-solo"].map((name) => ({ name, dir: `crates/${name}` }));
  const chainDeps = new Map([["rightkit-x", new Set()], ["rightkit-y", new Set(["rightkit-x"])], ["rightkit-z", new Set(["rightkit-y"])], ["rightkit-solo", new Set()]]);
  const reach2 = (names) => reverseDependents(names, chainDeps);
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-x/src/lib.rs"], members: chain, dependents: reach2 }), { all: false, test: ["rightkit-x"], check: ["rightkit-y", "rightkit-z"] });
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-x/src/lib.rs", "crates/rightkit-y/src/lib.rs"], members: chain, dependents: reach2 }), { all: false, test: ["rightkit-x", "rightkit-y"], check: ["rightkit-z"] });
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-solo/src/lib.rs"], members: chain, dependents: reach2 }), { all: false, test: ["rightkit-solo"], check: [] });
  for (const root of ["Cargo.toml", "Cargo.lock"]) assert.equal(selectRust({ changedPaths: [root], members: chain, dependents: reach2 }).all, false, `${root} is no longer a global input`);
  assert.throws(() => selectRust({ changedPaths: ["rust-toolchain.toml"], members: chain, dependents: reach2 }), /Rust global input.*name bounded units/);
});

test("vendor/citationberg selects rightkit-typeset and its dependents (shared input listed explicitly)", () => {
  const typeset = [{ name: "rightkit-typeset", dir: "crates/rightkit-typeset" }, { name: "rightkit-docs", dir: "crates/rightkit-docs" }, { name: "rightkit-fs", dir: "crates/rightkit-fs" }];
  const typesetDeps = new Map([["rightkit-typeset", new Set()], ["rightkit-docs", new Set(["rightkit-typeset"])], ["rightkit-fs", new Set()]]);
  assert.deepEqual(selectRust({ changedPaths: ["vendor/citationberg/src/lib.rs"], members: typeset, dependents: (n) => reverseDependents(n, typesetDeps) }), { all: false, test: ["rightkit-typeset"], check: ["rightkit-docs"] });
  assert.deepEqual(selectRust({ changedPaths: ["vendor/tauri-runtime-wry/src/lib.rs"], members: typeset, dependents: (n) => reverseDependents(n, typesetDeps) }), { all: false, test: [], check: [] });
});

test("extra paths and config edits select their crate; global inputs require explicit selection", () => {
  assert.deepEqual(selectRust({ changedPaths: ["contracts/x.schema.json"], members, dependents }).test, ["rightkit-c"]);
  assert.deepEqual(selectRust({ changedPaths: [], members, dependents, configChanged: new Set(["rightkit-b"]) }).test, ["rightkit-b"]);
  assert.throws(() => selectRust({ changedPaths: ["rust-toolchain.toml"], members, dependents }), /Rust global input.*name bounded units/);
  assert.deepEqual([...changedConfigCrates({ rust: { a: { os: ["macos"] } } }, { rust: { a: {}, b: {} } })].sort(), ["a", "b"]);
});

test("global gate, parser, provisioner and pinned input edits cannot silently select both registries", () => {
  const packages = [{ name: "@rightkit/example", dir: "packages/example", deps: [] }];
  for (const path of ["scripts/ci/gate.mjs", "scripts/ci/select.mjs", "scripts/ci/ci-plan.mjs", "scripts/ci/test-results.mjs", "scripts/ci/provision-chrome.mjs", "scripts/ci/provision-ort.mjs", "ci/chrome-for-testing.json", "crates/rightkit-ffmpeg/catalog/native-libraries.json"]) {
    assert.throws(() => selectRust({ changedPaths: [path], members, dependents }), /Rust global input.*--all-units-exception/, `Rust refusal for ${path}`);
    assert.throws(() => selectNpm({ changedPaths: [path], packages }), /npm global input.*--all-units-exception/, `npm refusal for ${path}`);
    assert.deepEqual(selectRust({ changedPaths: [path], members, dependents, all: true }), { all: true, test: members.map((m) => m.name), check: [] });
    assert.deepEqual(selectNpm({ changedPaths: [path], packages, all: true }), { all: true, test: ["packages/example"] });
  }
});

test("only shared CI workflow and setup action changes require explicit global scope", () => {
  const packages = [{ name: "@rightkit/example", dir: "packages/example", deps: [] }];
  for (const path of [
    ".github/workflows/publish.yml",
    ".github/workflows/publish-reviewed.yml",
    ".github/workflows/reviewed-registry-readback.yml",
    ".github/workflows/qualify-fs.yml",
  ]) {
    assert.equal(selectRust({ changedPaths: [path], members, dependents }).all, false, `Rust selection for ${path}`);
    assert.equal(selectNpm({ changedPaths: [path], packages }).all, false, `npm selection for ${path}`);
  }
  for (const path of [".github/workflows/ci.yml", ".github/actions/rk-setup/action.yml"]) {
    assert.throws(() => selectRust({ changedPaths: [path], members, dependents }), /Rust global input.*name bounded units/, `Rust refusal for ${path}`);
    assert.throws(() => selectNpm({ changedPaths: [path], packages }), /npm global input.*name bounded units/, `npm refusal for ${path}`);
  }
  assert.deepEqual(selectRust({ changedPaths: ["crates/rightkit-a/src/lib.rs"], members, dependents }), { all: false, test: ["rightkit-a"], check: ["rightkit-b"] });
  assert.deepEqual(selectNpm({ changedPaths: ["packages/example/src/index.ts"], packages }), { all: false, test: ["packages/example"] });
});

test("npm selection follows dependents; path suites match by directory", () => {
  const packages = [
    { name: "@rightkit/ui", dir: "packages/ui", deps: [] },
    { name: "@rightkit/tauri-shell", dir: "packages/tauri-shell", deps: ["@rightkit/ui"] },
    { name: "@rightkit/ci", dir: "packages/ci", deps: [] },
  ];
  assert.deepEqual(selectNpm({ changedPaths: ["packages/ui/src/a.ts"], packages }).test, ["packages/tauri-shell", "packages/ui"]);
  assert.deepEqual(selectNpm({ changedPaths: ["docs/x.md"], packages }).test, []);
  assert.throws(() => selectNpm({ changedPaths: ["pnpm-lock.yaml"], packages }), /npm global input.*name bounded units/);
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

test("publication selection tests named units without reverse workspace dependents", () => {
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
  assert.deepEqual(picked.rust, { all: false, test: ["rightkit-logs"], check: [] });
  assert.ok(!picked.rust.test.includes("rightkit-crash"), "reverse dependents stay outside publication tests");
  assert.ok(!picked.rust.check.includes("rightkit-independent"), "unrelated workspace crates stay out of the request");
});

test("crate selection validates requested graph entries without blocking on host-restricted dependents", () => {
  assert.throws(() => selectUnits({ registry: "crates", units: ["rightkit-a"], members, host: "windows" }), /requires the per-crate lock graph/);
  assert.throws(() => selectUnits({ registry: "crates", units: ["rightkit-a"], members, lock: new Map(), host: "windows" }), /crate lock graph is incomplete; no crates\/<x>\/Cargo\.lock lists member/);

  const chainLock = parseLock(`
[[package]]
name = "rightkit-logs"
version = "1"

[[package]]
name = "rightkit-crash"
version = "1"
dependencies = ["rightkit-logs"]
`);
  const selected = selectUnits({
    registry: "crates", units: ["rightkit-logs"],
    members: [{ name: "rightkit-logs", dir: "crates/rightkit-logs" }, { name: "rightkit-crash", dir: "crates/rightkit-crash" }],
    lock: chainLock, host: "windows", config: { rust: { "rightkit-crash": { os: ["macos"] } } },
  });
  assert.deepEqual(selected.rust, { all: false, test: ["rightkit-logs"], check: [] });
  chainLock.delete("rightkit-crash");
  assert.deepEqual(selectUnits({ registry: "crates", units: ["rightkit-logs"], members: [{ name: "rightkit-logs" }, { name: "rightkit-crash" }], lock: chainLock }).rust.check, []);
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

test("a package diff produces build filters only for selected packages and their dependency closures", () => {
  const packages = [
    { name: "@rightkit/base", dir: "packages/base", deps: [] },
    { name: "@rightkit/client", dir: "packages/client", deps: ["@rightkit/base"] },
    { name: "@rightkit/unrelated", dir: "packages/unrelated", deps: [] },
  ];
  const selected = selectNpm({ changedPaths: ["packages/client/src/index.js"], packages });
  assert.deepEqual(selected, { all: false, test: ["packages/client"] });
  const args = pnpmBuildClosureArgs(selected.test.map((dir) => packages.find((p) => p.dir === dir)?.name));
  assert.deepEqual(args, ["--workspace-concurrency=1", "--if-present", "--filter", "@rightkit/client...", "run", "build"]);
  assert.ok(!args.includes("-r"), "a diff never requests a recursive whole-workspace build");
  assert.throws(() => pnpmBuildClosureArgs([undefined]), /unsafe package name/, "missing package mappings fail closed");
});
