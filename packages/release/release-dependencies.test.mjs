import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { assertReleaseDependencies, forbiddenCrateFindings } from "./release-dependencies.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const denied = ["rightkit-record", "rightkit-automation", "rightkit-hotkeys", "rightkit-overlay", "rightkit-agent-tools", "rightkit-qa", "rightkit-qa-driver"];
const config = { releaseDependencies: { forbiddenCrates: denied } };
const target = { cargoTarget: "aarch64-apple-darwin" };
function fixture() {
  const root = tempRoot("release-dependencies-");
  mkdirSync(path.join(root, "src-tauri"));
  writeFileSync(path.join(root, "src-tauri/Cargo.toml"), '[package]\nname = "app"\nversion = "0.1.0"\n');
  return root;
}
const ok = (stdout = "app v0.1.0\nserde v1.0.0\n") => ({ status: 0, stdout, stderr: "" });

test("exact Cargo names match transitive, duplicate, path & multiple-version entries", () => {
  const text = "app v0.1.0\r\nrightkit-record-helper v1.0.0\r\nrightkit-record v0.2.0 (/tmp/record)\r\nrightkit-record v0.3.0 (*)\r\nrightkit-qa v0.6.0\r\n";
  assert.deepEqual(forbiddenCrateFindings(text, denied), ["rightkit-qa", "rightkit-record"]);
  assert.deepEqual(forbiddenCrateFindings("rightkit-record-helper v1.0\nnot-rightkit-record v1.0\n", denied), []);
});

test("unconfigured apps skip without invoking Cargo", () => {
  assert.deepEqual(assertReleaseDependencies({ run: () => assert.fail("Cargo invoked") }), { skipped: "no releaseDependencies policy" });
});

test("uses locked normal-only graph, prefix none, manifest & selected release target", () => {
  const root = fixture();
  const result = assertReleaseDependencies({ root, config, target, run: (cmd, args, options) => {
    assert.equal(cmd, "cargo");
    assert.equal(args[args.indexOf("-e") + 1], "normal");
    assert.equal(args[args.indexOf("--prefix") + 1], "none");
    assert.equal(args[args.indexOf("--manifest-path") + 1], path.join(root, "src-tauri/Cargo.toml"));
    assert.equal(args[args.indexOf("--target") + 1], target.cargoTarget);
    assert.ok(args.includes("--locked"));
    assert.equal(options.cwd, root);
    assert.ok(!args.includes("--features"));
    return ok();
  } });
  assert.equal(result.trees, 1);
  assert.equal(result.target, target.cargoTarget);
});

test("reports every forbidden crate with target context", () => {
  assert.throws(() => assertReleaseDependencies({ root: fixture(), config, target, run: () => ok(denied.map((crate) => `${crate} v1.0.0`).join("\n")) }), (error) => {
    for (const crate of denied) assert.ok(error.message.includes(`${crate} is in the release dependency tree (${target.cargoTarget})`));
    return true;
  });
});

test("forbidden lists belong to each app", () => {
  const root = fixture();
  const run = () => ok("app v1.0.0\nrightkit-record v0.2.0\n");
  assert.doesNotThrow(() => assertReleaseDependencies({ root, target, config: { releaseDependencies: { forbiddenCrates: ["rightkit-qa"] } }, run }));
  assert.throws(() => assertReleaseDependencies({ root, target, config, run }), /rightkit-record/);
});

test("follows package scripts & checks default/no-default feature sets separately", () => {
  const root = fixture();
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ scripts: { ship: "pnpm run native", native: "cargo build --no-default-features -Fexport" } }));
  const calls = [];
  const result = assertReleaseDependencies({ root, config: { ...config, releaseSwitches: { features: ["shell"] }, releaseDependencies: { ...config.releaseDependencies, features: ["codec"] } }, target,
    commands: [{ cmd: "pnpm", args: ["run", "ship"] }, "cargo build"], run: (cmd, args) => { calls.push(args); return ok(); } });
  assert.equal(result.trees, 2);
  assert.equal(calls[0][calls[0].indexOf("--features") + 1], "codec,export,shell");
  assert.ok(calls[0].includes("--no-default-features"));
  assert.ok(!calls[1].includes("--no-default-features"));
  assert.throws(() => assertReleaseDependencies({ root, config, target, commands: ["cargo build --no-default-features", "cargo build"],
    run: (cmd, args) => ok(args.includes("--no-default-features") ? "app v1.0.0\n" : "app v1.0.0\nrightkit-record v0.1.0\n") }), /rightkit-record/);
});

test("deduplicates equivalent feature sets & forwards all-features", () => {
  const calls = [];
  assertReleaseDependencies({ root: fixture(), config, target, commands: ["cargo build --features b,a", "cargo build -Fa,b", "cargo build --all-features"],
    run: (cmd, args) => { calls.push(args); return ok(); } });
  assert.equal(calls.length, 2);
  assert.ok(calls[1].includes("--all-features"));
});

test("policy noDefaultFeatures forwards to every graph", () => {
  assertReleaseDependencies({ root: fixture(), config: { releaseDependencies: { forbiddenCrates: [], noDefaultFeatures: true } }, target,
    run: (cmd, args) => { assert.ok(args.includes("--no-default-features")); return ok(); } });
});

test("bad policy or declared manifest fails before Cargo", () => {
  const root = fixture();
  for (const policy of [null, [], {}, { forbiddenCrates: "record" }, { forbiddenCrates: [""] }, { forbiddenCrates: ["record.*"] }, { forbiddenCrates: [1] },
    { forbiddenCrates: [], features: "demo" }, { forbiddenCrates: [], noDefaultFeatures: "true" }, { forbiddenCrates: [], manifest: "missing/Cargo.toml" }]) {
    assert.throws(() => assertReleaseDependencies({ root, config: { releaseDependencies: policy }, target, run: () => assert.fail("Cargo invoked") }));
  }
});

test("missing manifest, failed Cargo, spawn error & empty tree fail closed", () => {
  assert.throws(() => assertReleaseDependencies({ root: tempRoot("no-manifest-"), config, target }), /manifest-missing/);
  for (const result of [{ status: 101, stderr: "lock needs update" }, { status: null, error: new Error("ENOENT") }, { status: 0, stdout: "  " }, { status: 0, stdout: "warning: no packages" }]) {
    assert.throws(() => assertReleaseDependencies({ root: fixture(), config, target, run: () => result }), /release dependencies/);
  }
});

test("explicit alternate manifest overrides fallback", () => {
  const root = fixture();
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "engine"\nversion = "0.1.0"\n');
  assertReleaseDependencies({ root, config: { releaseDependencies: { forbiddenCrates: [], manifest: "Cargo.toml" } }, target,
    run: (cmd, args) => { assert.equal(args[args.indexOf("--manifest-path") + 1], path.join(root, "Cargo.toml")); return ok(); } });
});


test("absolute manifest paths resolve without falling back to src-tauri", () => {
  const root = fixture();
  const manifest = path.join(root, "Cargo.toml");
  writeFileSync(manifest, '[package]\nname = "engine"\nversion = "0.1.0"\n');
  assertReleaseDependencies({ root, config: { releaseDependencies: { forbiddenCrates: [], manifest } }, target,
    run: (cmd, args) => { assert.equal(args[args.indexOf("--manifest-path") + 1], manifest); return ok(); } });
});
