// rightkit#1303: the gate's signing refusals. They are `routePlan`'s (so `rightkit-gate --to <stage> --route` answers what a real run does), they read the
// commit's own config from the object database, and they apply on the Mac and the Dell only: Linux builds sign nothing and hosted CI is exempt.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { loadAppConfig } from "./command-spec.mjs";
import { runDevelopment } from "./development.mjs";
import { runSuites, main as testMain, planTests } from "./test-command.mjs";
import { SuiteSigningError } from "./suite-signing-policy.mjs";
import { ROUTE_REFUSAL_CODES, devTestPlatform, main, routeAppConfig, routePlan } from "./bin/rightkit-gate.mjs";
import { SIGNING_REFUSAL_CODES, adHocReason, classifyStepArtifacts, identifierOverrideReason, isProductBuildStep, planSigningRefusals, productTargets, signingRefusalHost, suiteSigningRefusals } from "./suite-signing-policy.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const directExecutor = () => ({ kind: "direct", cargo: ["cargo"] });
const BUILD = { name: "build-app", cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle"] };
const SIGN = { name: "sign-app", sign: { files: [{ from: "build-app", path: "debug/rightapp" }] } };
const QA = { name: "qa", cmd: "node", args: ["qa.mjs"] };
const TAURI_CONF = JSON.stringify({ productName: "RightApp", identifier: "app.rightsuite.rightapp" });
const CARGO = '[package]\nname = "rightapp"\nversion = "0.1.0"\n';
const files = { "src-tauri/tauri.conf.json": TAURI_CONF, "src-tauri/Cargo.toml": CARGO };
// The commit's Rust tests, as the policy reads them (app-relative path -> text): the sidecar crate's e2e test launches its own bin.
const tree = { "src-tauri/tests/ui.rs": "#[test] fn ui() {}", "crates/engine/tests/e2e.rs": 'let bin = env!("CARGO_BIN_EXE_rightapp-engine");', "crates/engine/tests/unit.rs": "#[test] fn u() {}" };
const readFile = (rel) => files[rel] ?? tree[rel] ?? null;
const listFiles = (dir) => Object.keys(tree).filter((name) => name.startsWith(`${dir}/`));
const refusals = (config, over = {}) => suiteSigningRefusals({ config, platform: "darwin", to: "test", readFile, listFiles, ...over });
const codes = (list) => list.map((item) => item.code);

// ---- what counts -------------------------------------------------------------------------------------------------------------------

test("a product build: tauri build and bundle, and a cargo build of the tauri package or a sidecar package; a test build is not one", () => {
  const targets = productTargets({ config: { sidecars: { sidecars: [{ kind: "cargo-bin", package: "rightapp-engine", manifestPath: "crates/engine/Cargo.toml" }] } }, readFile, listFiles });
  const is = (step) => isProductBuildStep(step, targets);
  assert.equal(is({ cmd: "pnpm", args: ["exec", "tauri", "build"] }), true);
  assert.equal(is({ cmd: "pnpm", args: ["exec", "tauri", "bundle", "--bundles", "nsis"] }), true);
  assert.equal(is({ cmd: "pnpm", args: ["exec", "tauri", "build", "--config", { $json: { bundle: {} } }] }), true);
  assert.equal(is({ cmd: "cargo-tauri", args: ["build"] }), true);
  assert.equal(is({ cmd: "node", args: ["node_modules/@tauri-apps/cli/tauri.js", "build", "--no-bundle"] }), true, "HeardRight runs the Tauri CLI by its script");
  assert.equal(is({ node: "node_modules/@tauri-apps/cli/tauri.js", args: ["build"] }), true);
  assert.equal(is({ cmd: "node", args: ["scripts/other.js", "build"] }), false);
  assert.equal(is({ cmd: "pnpm", args: ["exec", "tauri", "info"] }), false);
  assert.equal(is({ cmd: "cargo", args: ["build", "-p", "rightapp"] }), true, "the tauri package, named from its manifest");
  assert.equal(is({ cmd: "rustright", args: ["cargo", "build", "--package=rightapp"] }), true);
  assert.equal(is({ cmd: "cargo", args: ["build", "-p", "rightapp-engine"] }), true, "a sidecar package");
  assert.equal(is({ cmd: "cargo", args: ["build", "--manifest-path", "crates/engine/Cargo.toml"] }), true);
  assert.equal(is({ cmd: "cargo", args: ["build", "-p", "some-helper"] }), false);
  assert.equal(is({ cmd: "cargo", args: ["build"] }), true, "no package named: the workspace defaults, which include the product");
  assert.equal(is({ cmd: "cargo", args: ["test", "--no-run", "-p", "rightapp"] }), false);
  assert.equal(is({ cmd: "cargo", args: ["clippy"] }), false);
  assert.equal(is({ cmd: "node", args: ["x.mjs"] }), false);
  assert.equal(is({ node: "scripts/x.mjs" }), false);
});

test("ad hoc: sign-qa-bundle and RIGHTKIT_DEV_SIGNING, in a command, an argument or the env", () => {
  assert.match(adHocReason({ node: "scripts/sign-qa-bundle.mjs" }), /sign-qa-bundle/);
  assert.match(adHocReason({ cmd: "right-release", args: ["sign-qa-bundle", "--app", "x"] }), /sign-qa-bundle/);
  assert.match(adHocReason({ cmd: "x", env: { RIGHTKIT_DEV_SIGNING: "ci-adhoc" } }), /RIGHTKIT_DEV_SIGNING/);
  assert.match(adHocReason({ cmd: "env", args: ["RIGHTKIT_DEV_SIGNING=ci-adhoc", "x"] }), /RIGHTKIT_DEV_SIGNING/);
  assert.equal(adHocReason({ cmd: "pnpm", args: ["exec", "tauri", "build"] }), null);
});

test("an identifier override: inline JSON, a typed $json, and a config file that is read from the commit", () => {
  const step = (value) => ({ cmd: "pnpm", args: ["exec", "tauri", "build", "--config", value] });
  assert.match(identifierOverrideReason(step('{"identifier":"x.y"}')), /sets the bundle identifier/);
  assert.match(identifierOverrideReason(step({ $json: { identifier: "x.y" } })), /sets the bundle identifier/);
  assert.equal(identifierOverrideReason(step({ $json: { bundle: { createUpdaterArtifacts: false } } })), null);
  assert.match(identifierOverrideReason(step("src-tauri/tauri.dev.conf.json"), (rel) => (rel === "src-tauri/tauri.dev.conf.json" ? '{"identifier":"x.y"}' : null)), /sets the bundle identifier/);
  assert.equal(identifierOverrideReason(step("src-tauri/tauri.dev.conf.json"), () => '{"productName":"X"}'), null);
  assert.match(identifierOverrideReason(step("src-tauri/missing.json"), () => null), /cannot be read/, "a file that cannot be read cannot be shown clean");
  assert.match(identifierOverrideReason(step("{not json"), () => null), /does not parse/);
  assert.match(identifierOverrideReason({ cmd: "pnpm", args: ["exec", "tauri", "build", "--config=" + '{"identifier":"x"}'] }), /sets the bundle identifier/);
});

// ---- the refusals ------------------------------------------------------------------------------------------------------------------

test("a suite that builds a product needs a LATER sign step that names it", () => {
  assert.deepEqual(codes(refusals({ tests: { suites: [BUILD, QA] } })), ["RK_SUITE_UNSIGNED_BUILD"]);
  assert.deepEqual(refusals({ tests: { suites: [BUILD, SIGN, QA] } }), []);
  // A sign step before the build, or naming another suite, does not cover it.
  assert.deepEqual(codes(refusals({ tests: { suites: [SIGN, BUILD, QA] } })), ["RK_SUITE_UNSIGNED_BUILD"]);
  assert.deepEqual(codes(refusals({ tests: { suites: [BUILD, { name: "sign-other", sign: { files: [{ from: "elsewhere", path: "a" }] } }, QA] } })), ["RK_SUITE_UNSIGNED_BUILD"]);
  // A runner's command counts: the build can hide in a named runner.
  assert.deepEqual(codes(refusals({ tests: { runners: { tauri: { cmd: "pnpm", args: ["exec", "tauri"] } }, suites: [{ name: "build-app", runner: "tauri", args: ["build"] }, QA] } })), ["RK_SUITE_UNSIGNED_BUILD"]);
  // A suite that builds nothing of the product needs nothing.
  assert.deepEqual(refusals({ tests: { suites: [{ name: "unit", cmd: "pnpm", args: ["exec", "vitest", "run"] }, { name: "t", cmd: "cargo", args: ["test", "-p", "rightapp"] }] } }), []);
  assert.deepEqual(refusals({}), []);
  assert.deepEqual(refusals({ tests: { suites: [] } }), []);
});

test("ad hoc signing, an identifier override and a mismatched mac bundle id are refused, each with its code", () => {
  assert.deepEqual(codes(refusals({ tests: { suites: [{ name: "x", node: "scripts/sign-qa-bundle.mjs" }] } })), ["RK_SUITE_ADHOC_SIGNING"]);
  assert.deepEqual(codes(refusals({ tests: { suites: [{ name: "x", cmd: "node", args: ["a.mjs"], env: { RIGHTKIT_DEV_SIGNING: "ci-adhoc" } }] } })), ["RK_SUITE_ADHOC_SIGNING"]);
  const override = { name: "build-app", cmd: "pnpm", args: ["exec", "tauri", "build", "--config", { $json: { identifier: "app.dev" } }] };
  assert.deepEqual(codes(refusals({ tests: { suites: [override, SIGN] } })), ["RK_SUITE_IDENTIFIER_OVERRIDE"]);
  const dev = (install, build = []) => ({ development: { targets: { mac: { build, install } } } });
  assert.deepEqual(codes(refusals(dev({ kind: "tauri-devid", bundleId: "app.other" }), { to: "dev" })), ["RK_SUITE_IDENTIFIER_MISMATCH"]);
  assert.deepEqual(refusals(dev({ kind: "tauri-devid", bundleId: "app.rightsuite.rightapp" }), { to: "dev" }), []);
  assert.deepEqual(codes(refusals(dev({ kind: "tauri", productName: "X", signing: "ci-adhoc" }), { to: "dev" })), ["RK_SUITE_ADHOC_SIGNING"]);
  const overrideBuild = { cmd: "pnpm", args: ["exec", "tauri", "build", "--config", '{"identifier":"x.y"}'] };
  assert.deepEqual(codes(refusals(dev({ kind: "tauri-devid" }, [overrideBuild]), { to: "dev" })), ["RK_SUITE_IDENTIFIER_OVERRIDE"]);
  assert.deepEqual(codes(refusals(dev({ kind: "tauri-devid" }, [{ cmd: "node", args: ["sign-qa-bundle.mjs"] }]), { to: "dev" })), ["RK_SUITE_ADHOC_SIGNING"]);
  // A missing tauri.conf.json cannot prove the bundle id: refused as a mismatch, not passed.
  assert.deepEqual(codes(refusals(dev({ kind: "tauri-devid", bundleId: "app.x" }), { to: "dev", readFile: () => null })), ["RK_SUITE_IDENTIFIER_MISMATCH"]);
});

test("the Windows `tauri` install kind is refused on a Windows host; tauri-authenticode is not", () => {
  const win = (kind) => ({ development: { targets: { win: { build: [], install: { kind, productName: "X" } } } } });
  assert.deepEqual(codes(refusals(win("tauri"), { platform: "win32", to: "dev" })), ["RK_DEV_WIN_UNSIGNED_INSTALL"]);
  assert.deepEqual(refusals(win("tauri-authenticode"), { platform: "win32", to: "dev" }), []);
  // The refusal follows the platform asked for, not a target the run does not touch.
  assert.deepEqual(refusals(win("tauri"), { platform: "darwin", to: "dev" }), []);
  assert.deepEqual(codes(refusals(win("tauri"), { platform: "darwin", to: "dev", devPlatform: "win" })), ["RK_DEV_WIN_UNSIGNED_INSTALL"]);
  assert.deepEqual(refusals(win("tauri"), { platform: "win32", to: "test" }), [], "a test run installs nothing");
});

test("each refusal fires on darwin and win32 and on neither linux nor hosted CI", () => {
  const configs = [
    { to: "test", config: { tests: { suites: [BUILD, QA] } }, code: "RK_SUITE_UNSIGNED_BUILD" },
    { to: "test", config: { tests: { suites: [{ name: "x", node: "sign-qa-bundle.mjs" }] } }, code: "RK_SUITE_ADHOC_SIGNING" },
    { to: "test", config: { tests: { suites: [{ ...BUILD, args: [...BUILD.args, "--config", '{"identifier":"a.b"}'] }, SIGN] } }, code: "RK_SUITE_IDENTIFIER_OVERRIDE" },
    { to: "test", config: { ...ENGINE, tests: { suites: [{ name: "build-all", cmd: "cargo", args: ["build"] }, { name: "sign-shell", sign: { files: [{ from: "build-all", path: "debug/rightapp" }] } }, { name: "e2e", cmd: "cargo", args: ["test", "-p", "rightapp-engine", "--test", "e2e"] }] } }, code: "RK_SUITE_LAUNCH_UNBOUND" },
    { to: "dev", config: { development: { targets: { mac: { build: [], install: { kind: "tauri-devid", bundleId: "app.other" } } } } }, code: "RK_SUITE_IDENTIFIER_MISMATCH", only: "darwin" },
    { to: "dev", config: { development: { targets: { win: { build: [], install: { kind: "tauri" } } } } }, code: "RK_DEV_WIN_UNSIGNED_INSTALL", only: "win32" },
    { to: "dev", config: { development: { targets: { mac: { build: [], install: { kind: "command", cmd: "true" } } } } }, code: "RK_DEV_UNSIGNED_INSTALL", only: "darwin" },
    { to: "test", config: { sidecars: { sidecars: [{ package: "no-manifest" }] }, tests: { suites: [{ name: "t", cmd: "cargo", args: ["test", "-p", "no-manifest"] }] } }, code: "RK_SUITE_UNCLASSIFIED_ARTIFACT" },
  ];
  assert.deepEqual([...new Set(configs.map((c) => c.code))].sort(), [...SIGNING_REFUSAL_CODES].sort(), "every code is exercised");
  const plan = (platform, env, { to, config }) => routePlan({ platform, env, resolveExecutor: directExecutor, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "" }, { to, stages: [], devTest: true, executor: directExecutor(), appConfig: { config, readFile, listFiles } });
  for (const item of configs) {
    for (const platform of ["darwin", "win32"]) {
      const answer = plan(platform, {}, item);
      if (item.only && item.only !== platform) { assert.equal(answer.refusal, null, `${item.code} on ${platform}`); continue; }
      assert.equal(answer.route, "snapshot");
      assert.equal(answer.refusal?.code, item.code, `${item.code} on ${platform}`);
      assert.equal(answer.refusal.stage, item.to);
    }
    assert.equal(plan("linux", {}, item).refusal, null, `${item.code} on linux`);
    for (const platform of ["darwin", "win32", "linux"]) {
      const hosted = plan(platform, { GITHUB_ACTIONS: "true" }, item);
      assert.equal(hosted.route, "hosted");
      assert.equal(hosted.refusal, null, `${item.code} on hosted ${platform}`);
    }
  }
  assert.equal(signingRefusalHost("darwin"), true);
  assert.equal(signingRefusalHost("win32"), true);
  assert.equal(signingRefusalHost("linux"), false);
  const runner = { GITHUB_ACTIONS: "true", CI: "true", RUNNER_ENVIRONMENT: "github-hosted" };
  assert.equal(signingRefusalHost("win32", runner), false);
  assert.equal(signingRefusalHost("darwin", runner), false);
  // L1: the hosted exemption is the whole three-variable runner test, never one forgeable variable.
  assert.equal(signingRefusalHost("win32", { GITHUB_ACTIONS: "true" }), true);
  assert.equal(signingRefusalHost("darwin", { ...runner, RUNNER_ENVIRONMENT: "self-hosted" }), true);
  assert.equal(signingRefusalHost("darwin", { ...runner, CI: undefined }), true);
});

test("routePlan: no config, a clean config and a stage that is not dev or test never refuse for signing; an earlier refusal keeps its place", () => {
  const base = { platform: "darwin", env: {}, resolveExecutor: directExecutor, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "" };
  const bad = { tests: { suites: [BUILD, QA] } };
  assert.equal(routePlan(base, { to: "test", stages: [], devTest: true, executor: directExecutor() }).refusal, null);
  assert.equal(routePlan(base, { to: "test", stages: [], devTest: true, executor: directExecutor(), appConfig: { config: { tests: { suites: [BUILD, SIGN, QA] } }, readFile } }).refusal, null);
  assert.equal(routePlan(base, { to: "package", stages: ["check", "compile", "package"], devTest: false, executor: directExecutor(), appConfig: { config: bad, readFile } }).refusal, null);
  assert.equal(routePlan({ ...base, env: { TAURI_CONFIG: "{}" } }, { to: "test", stages: [], devTest: true, executor: directExecutor(), appConfig: { config: bad, readFile } }).refusal.code, "RK_TAURI_CONFIG_OVERRIDE");
});

test("the route's code list is the base codes plus exactly the codes the signing refusals can return", () => {
  const source = fs.readFileSync(path.join(HERE, "suite-signing-policy.mjs"), "utf8");
  const at = source.indexOf("export function suiteSigningRefusals");
  const body = source.slice(at, source.indexOf("\n}\n", at));
  assert.deepEqual([...new Set([...body.matchAll(/\b(RK_[A-Z0-9_]+)\b/g)].map((m) => m[1]))].sort(), [...SIGNING_REFUSAL_CODES].sort());
  for (const code of SIGNING_REFUSAL_CODES) assert.ok(ROUTE_REFUSAL_CODES.includes(code), code);
});

// ---- the resolved plan (Oracle H1) and the artifact rule -----------------------------------------------------------------------------

const ENGINE = { sidecars: { sidecars: [{ kind: "cargo-bin", package: "rightapp-engine", manifestPath: "crates/engine/Cargo.toml" }] } };
const RUNNERS = { cargo: { cmd: "cargo" }, pnpm: { cmd: "pnpm" } };
const PLAN_CONFIG = { ...ENGINE, tests: { runners: RUNNERS, suites: [BUILD, SIGN, QA] } };
const planOf = (options, config = PLAN_CONFIG) => planTests(config, { appRoot: os.tmpdir(), ...options });
const planRefusals = (plan, over = {}) => planSigningRefusals({ config: PLAN_CONFIG, plan, platform: "darwin", env: {}, readFile, listFiles, ...over });

test("H1: runner mode, a lone build suite and passthrough are judged on the plan that runs; the four Oracle reproductions and HeardRight's are refused", () => {
  const cargoBuild = planOf({ runner: "cargo", passthrough: ["build", "-p", "rightapp-engine"] });
  assert.deepEqual(codes(planRefusals(cargoBuild)), ["RK_SUITE_UNSIGNED_BUILD"], "--runner cargo -- build -p <sidecar>");
  assert.deepEqual(codes(planRefusals(planOf({ runner: "cargo", passthrough: ["build"] }))), ["RK_SUITE_UNSIGNED_BUILD"], "a bare cargo build");
  assert.deepEqual(codes(planRefusals(planOf({ runner: "pnpm", passthrough: ["exec", "tauri", "build"] }))), ["RK_SUITE_UNSIGNED_BUILD"], "--runner pnpm -- exec tauri build");
  assert.deepEqual(codes(planRefusals(planOf({ suites: ["build-app"] }))), ["RK_SUITE_UNSIGNED_BUILD"], "--suite build-app alone");
  assert.deepEqual(codes(planRefusals(planOf({ suites: ["build-app"], passthrough: ["--config", '{"identifier":"com.other"}'] }))).sort(), ["RK_SUITE_IDENTIFIER_OVERRIDE", "RK_SUITE_UNSIGNED_BUILD"], "--suite build-app -- --config {identifier}");
  // HeardRight's own shape: its cargo runner building its engine sidecar.
  const hr = { sidecars: { sidecars: [{ kind: "cargo-bin", package: "heardright-engine", manifestPath: "crates/engine/Cargo.toml" }] }, tests: { runners: RUNNERS, suites: [BUILD, SIGN] } };
  assert.deepEqual(codes(planSigningRefusals({ config: hr, plan: planOf({ runner: "cargo", passthrough: ["build", "-p", "heardright-engine"] }, hr), platform: "darwin", env: {}, readFile, listFiles })), ["RK_SUITE_UNSIGNED_BUILD"]);
  // Ad hoc signing smuggled in by passthrough.
  assert.deepEqual(codes(planRefusals(planOf({ suites: ["qa"], passthrough: ["--sign", "sign-qa-bundle"] }))), ["RK_SUITE_ADHOC_SIGNING"]);
  // A properly planned build + sign passes, whole or as a named group; Linux and hosted CI are exempt.
  assert.deepEqual(planRefusals(planOf({})), []);
  assert.deepEqual(planRefusals(planOf({ suites: ["build-app", "sign-app"] })), []);
  assert.deepEqual(planRefusals(planOf({ runner: "cargo", passthrough: ["build"] }), { platform: "linux" }), []);
  assert.deepEqual(planRefusals(planOf({ runner: "cargo", passthrough: ["build"] }), { env: { GITHUB_ACTIONS: "true", CI: "true", RUNNER_ENVIRONMENT: "github-hosted" } }), []);
  // runSuites refuses it before anything spawns.
  const spawned = [];
  return runSuites(cargoBuild, { appRoot: os.tmpdir(), config: PLAN_CONFIG, platform: "darwin", env: {}, spawnFn: (...args) => { spawned.push(args); throw new Error("must not spawn"); }, errLog: () => {}, log: () => {} }).then((result) => {
    assert.equal(result.exitCode, 1);
    assert.equal(spawned.length, 0);
  });
});

test("artifacts, not verbs: a product bin is a product, in-place lib tests and explicit examples are harnesses, a test that launches CARGO_BIN_EXE_ needs an earlier sign", () => {
  const targets = productTargets({ config: ENGINE, readFile, listFiles });
  const kind = (...args) => { const found = classifyStepArtifacts({ cmd: "cargo", args }, targets); return found.unknown.length ? "unknown" : found.launches ? "launches" : found.product ? "product" : "harness"; };
  // HeardRight's Case4: three lib harnesses and six explicitly selected examples pass.
  for (const args of [["test", "-p", "rightapp", "--lib"], ["test", "-p", "rightapp-engine", "--lib"], ["test", "--lib", "-p", "rightapp-engine", "--no-run"]]) assert.equal(kind(...args), "harness", args.join(" "));
  for (let i = 1; i <= 6; i += 1) assert.equal(kind("run", "-p", "rightapp-engine", "--example", `measure${i}`), "harness", `example ${i}`);
  assert.equal(kind("build", "-p", "rightapp-engine", "--example", "m"), "harness");
  assert.equal(kind("test", "-p", "rightapp"), "harness", "a product package whose tests never launch its bin");
  assert.equal(kind("test", "-p", "other-crate"), "harness");
  // The engine e2e launches its own bin: product consumption, with `--no-run` no exception.
  assert.equal(kind("test", "-p", "rightapp-engine"), "launches");
  assert.equal(kind("test", "-p", "rightapp-engine", "--no-run"), "launches");
  assert.equal(kind("test", "-p", "rightapp-engine", "--test", "e2e"), "launches");
  assert.equal(kind("test"), "launches", "no package named covers every product package");
  assert.equal(kind("test", "--manifest-path", "crates/engine/Cargo.toml"), "launches");
  assert.equal(kind("build", "-p", "rightapp-engine"), "product");
  assert.equal(kind("run", "-p", "rightapp-engine"), "launches");
  // Words after `--` go to the program: they never select or exclude packages.
  assert.equal(kind("run", "-p", "rightapp-engine", "--", "--exclude", "rightapp-engine"), "launches");
  assert.equal(kind("build", "-p", "rightapp-engine", "--", "--exclude", "rightapp-engine"), "product");
  assert.equal(kind("test", "-p", "rightapp", "--", "-p", "rightapp-engine"), "harness", "a program argument names no package");
  assert.equal(kind("clippy", "-p", "rightapp-engine"), "harness", "check, clippy and doc produce no executable");
  // The verb list is gone: `test --no-run` is no escape, and a harness-only verb does not make a product.
  const hr = (suites) => ({ ...ENGINE, tests: { suites } });
  const e2e = { name: "e2e", cmd: "cargo", args: ["test", "-p", "rightapp-engine", "--test", "e2e"] };
  const lib = { name: "lib", cmd: "cargo", args: ["test", "-p", "rightapp-engine", "--lib"] };
  const buildEngine = { name: "build-engine", cmd: "cargo", args: ["build", "-p", "rightapp-engine"] };
  const signEngine = { name: "sign-engine", sign: { files: [{ from: "build-engine", path: "debug/rightapp-engine" }] } };
  assert.deepEqual(codes(refusals(hr([e2e]))), ["RK_SUITE_UNSIGNED_BUILD"], "the e2e that launches CARGO_BIN_EXE unsigned is refused");
  assert.deepEqual(refusals(hr([lib])), [], "a lib harness runs in place");
  assert.deepEqual(codes(refusals(hr([e2e, buildEngine, signEngine]))), ["RK_SUITE_UNSIGNED_BUILD"], "a sign step AFTER the launch does not cover it");
  assert.deepEqual(refusals(hr([buildEngine, signEngine, e2e])), [], "build, sign, then the test passes");
  // Launch binding: the sign step must name the executable the suite launches, built by a suite of that package, at exactly the path its layout leaves
  // it: <profile>/<bin> for a build with no --target (what cargo writes), <triple>/<profile>/<bin> with one.
  const buildAll = { name: "build-all", cmd: "cargo", args: ["build"] };
  const signOf = (from, file) => ({ name: "sign-x", sign: { files: [{ from, path: file }] } });
  const signShell = signOf("build-all", "debug/rightapp");
  const signEngineDebug = signOf("build-all", "debug/rightapp-engine");
  const signWinEngine = signOf("build-all", "debug/rightapp-engine.exe");
  assert.deepEqual(codes(refusals(hr([buildAll, signShell, e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "a sign step that signs only the shell does not cover the engine the test launches");
  assert.match(refusals(hr([buildAll, signShell, e2e]))[0].message, /rightapp-engine:rightapp-engine/);
  assert.deepEqual(refusals(hr([buildAll, signEngineDebug, e2e])), [], "a sign step that covers the engine passes");
  assert.deepEqual(refusals(hr([buildAll, signShell, signEngineDebug, e2e])), [], "any earlier sign step may cover it");
  assert.deepEqual(refusals(hr([buildAll, signWinEngine, e2e])), [], "<profile>/<bin>.exe");
  assert.deepEqual(codes(refusals(hr([lib, signOf("lib", "debug/rightapp-engine"), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "a file of a suite that builds nothing is no cover");
  // 3. The path shape, the triple and the profile must match what the build produced and what the launch uses.
  const buildRelease = { name: "build-all", cmd: "cargo", args: ["build", "--release"] };
  assert.deepEqual(codes(refusals(hr([buildRelease, signOf("build-all", "release/rightapp-engine"), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "a release build signed, a debug cargo test launched");
  assert.deepEqual(refusals(hr([buildRelease, signOf("build-all", "release/rightapp-engine"), { ...e2e, args: [...e2e.args, "--release"] }]), []), [], "release build, release launch");
  assert.deepEqual(codes(refusals(hr([buildAll, signOf("build-all", "anything/rightapp-engine"), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "an arbitrary directory is not the profile directory");
  assert.deepEqual(codes(refusals(hr([buildAll, signOf("build-all", "rightapp-engine"), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "a bare file name");
  assert.deepEqual(codes(refusals(hr([buildAll, signOf("build-all", "a/b/debug/rightapp-engine"), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "more segments than the layout has");
  assert.deepEqual(codes(refusals(hr([buildAll, signOf("build-all", "aarch64-apple-darwin/debug/rightapp-engine"), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "a build with no --target writes no triple directory");
  const TRIPLE = "x86_64-unknown-freebsd";
  const buildTarget = { name: "build-all", cmd: "cargo", args: ["build", "--target", TRIPLE] };
  const e2eTarget = { ...e2e, args: [...e2e.args, "--target", TRIPLE] };
  assert.deepEqual(refusals(hr([buildTarget, signOf("build-all", `${TRIPLE}/debug/rightapp-engine`), e2eTarget])), [], "the triple of the build is the triple of the launch");
  assert.deepEqual(codes(refusals(hr([buildTarget, signOf("build-all", `${TRIPLE}/debug/rightapp-engine`), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], "built for another triple than the launch runs");
  assert.deepEqual(codes(refusals(hr([buildTarget, signOf("build-all", "debug/rightapp-engine"), e2eTarget]))), ["RK_SUITE_LAUNCH_UNBOUND"], "a --target build is under the triple directory");
  // No host-triple equivalence, in either direction: signing target/<triple>/debug/<bin> does not cover the target/debug/<bin> a no-target launch runs.
  for (const host of [TRIPLE, "aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]) {
    const built = { name: "build-all", cmd: "cargo", args: ["build", "--target", host] };
    assert.deepEqual(codes(refusals(hr([built, signOf("build-all", `${host}/debug/rightapp-engine`), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], `build --target ${host}, launch with no --target`);
    assert.deepEqual(codes(refusals(hr([buildAll, signEngineDebug, { ...e2e, args: [...e2e.args, "--target", host] }]))), ["RK_SUITE_LAUNCH_UNBOUND"], `build with no --target, launch --target ${host}`);
    assert.deepEqual(refusals(hr([built, signOf("build-all", `${host}/debug/rightapp-engine`), { ...e2e, args: [...e2e.args, "--target", host] }])), [], `the same --target ${host} on both`);
  }
  const buildProfile ={ name: "build-all", cmd: "cargo", args: ["build", "--profile", "qa"] };
  assert.deepEqual(refusals(hr([buildProfile, signOf("build-all", "qa/rightapp-engine"), { ...e2e, args: [...e2e.args, "--profile", "qa"] }])), [], "--profile X lands in X");
  assert.deepEqual(codes(refusals(hr([buildProfile, signOf("build-all", "debug/rightapp-engine"), e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"]);
  // 1. A shared test module (tests/common/mod.rs) launches another bin for every target, so `--test e2e` cannot narrow it away.
  const sharedTree = { ...tree, "crates/engine/tests/e2e.rs": 'mod common;\nlet bin = env!("CARGO_BIN_EXE_rightapp-engine");', "crates/engine/tests/common/mod.rs": 'pub fn h() { let _ = env!("CARGO_BIN_EXE_rightapp-helper"); }' };
  const shared = (suites) => refusals(hr(suites), { readFile: (rel) => files[rel] ?? sharedTree[rel] ?? null, listFiles: (dir) => Object.keys(sharedTree).filter((name) => name.startsWith(`${dir}/`)) });
  const sharedRefusal = shared([buildAll, signEngineDebug, e2e]);
  assert.deepEqual(codes(sharedRefusal), ["RK_SUITE_LAUNCH_UNBOUND"]);
  assert.match(sharedRefusal[0].message, /rightapp-helper/);
  assert.deepEqual(shared([buildAll, signOf("build-all", "debug/rightapp-helper"), signEngineDebug, e2e]), [], "both bins signed");
  // C. No narrowing by `--test`: every bin any test source of the package launches counts for every cargo test of it.
  const twoBins = { ...tree, "crates/engine/tests/other.rs": 'let bin = env!("CARGO_BIN_EXE_rightapp-helper");' };
  const wide = (suites) => refusals(hr(suites), { readFile: (rel) => files[rel] ?? twoBins[rel] ?? null, listFiles: (dir) => Object.keys(twoBins).filter((name) => name.startsWith(`${dir}/`)) });
  assert.deepEqual(codes(wide([buildAll, signEngineDebug, e2e])), ["RK_SUITE_LAUNCH_UNBOUND"], "--test e2e does not narrow away what tests/other.rs launches");
  assert.match(wide([buildAll, signEngineDebug, e2e])[0].message, /rightapp-helper/);
  assert.deepEqual(wide([buildAll, signOf("build-all", "debug/rightapp-helper"), signEngineDebug, e2e]), [], "both bins signed");
  // ...and a [[test]] path outside tests/ is a test source too.
  const outside = { ...tree, "crates/engine/Cargo.toml": '[package]\nname = "rightapp-engine"\n\n[[test]]\nname = "extra"\npath = "extras/launch.rs"\n', "crates/engine/extras/launch.rs": 'let bin = env!("CARGO_BIN_EXE_rightapp-helper");' };
  const outsideRefusals = refusals(hr([buildAll, signEngineDebug, e2e]), { readFile: (rel) => files[rel] ?? outside[rel] ?? null, listFiles: (dir) => Object.keys(outside).filter((name) => name.startsWith(`${dir}/`)) });
  assert.deepEqual(codes(outsideRefusals), ["RK_SUITE_LAUNCH_UNBOUND"], "a [[test]] path outside tests/");
  assert.match(outsideRefusals[0].message, /rightapp-helper/);
  assert.deepEqual(codes(wide([buildAll, signEngineDebug, { name: "all", cmd: "cargo", args: ["test", "-p", "rightapp-engine"] }])), ["RK_SUITE_LAUNCH_UNBOUND"], "the whole package also launches the helper");
  // A. `--exclude` removes a package from a workspace-wide build.
  for (const args of [["build", "--workspace", "--exclude", "rightapp-engine"], ["build", "--workspace", "--exclude=rightapp-engine"], ["build", "--workspace", "--exclude", "other", "--exclude", "rightapp-engine"]]) {
    assert.deepEqual(codes(refusals(hr([{ name: "build-all", cmd: "cargo", args }, signEngineDebug, e2e]))), ["RK_SUITE_LAUNCH_UNBOUND"], args.join(" "));
  }
  assert.deepEqual(refusals(hr([{ name: "build-all", cmd: "cargo", args: ["build", "--workspace", "--exclude", "rightapp"] }, signEngineDebug, e2e])), [], "excluding another package still builds the engine");
  // 2. A tauri build builds only the shell's package: a sign file naming it does not cover the engine.
  const shell = { name: "shell", cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle", "--debug"] };
  assert.deepEqual(codes(refusals(hr([shell, signOf("shell", "debug/rightapp-engine"), { name: "all", cmd: "cargo", args: ["test", "-p", "rightapp-engine"] }]))), ["RK_SUITE_LAUNCH_UNBOUND"], "the tauri build does not build the engine");
  assert.deepEqual(refusals(hr([shell, signOf("shell", "debug/rightapp"), buildAll, signEngineDebug, e2e])), [], "the shell signed from its tauri build, the engine from its own build");
  // 4. A launch whose bin name is not literal needs every declared bin of the package signed.
  const concatTree = { ...files, "crates/engine/Cargo.toml": '[package]\nname = "rightapp-engine"\n\n[[bin]]\nname = "rightapp-engine"\npath = "src/main.rs"\n\n[[bin]]\nname = "unrelated-tool"\npath = "src/tool.rs"\n', "crates/engine/tests/e2e.rs": 'let bin = env!(concat!("CARGO_BIN_EXE_", "rightapp-engine"));' };
  const concat = (suites, world = concatTree) => refusals(hr(suites), { readFile: (rel) => world[rel] ?? null, listFiles: (dir) => Object.keys(world).filter((name) => name.startsWith(`${dir}/`)) });
  const unrelated = concat([buildAll, signOf("build-all", "debug/unrelated-tool"), e2e]);
  assert.deepEqual(codes(unrelated), ["RK_SUITE_LAUNCH_UNBOUND"]);
  assert.match(unrelated[0].message, /rightapp-engine:rightapp-engine/);
  assert.deepEqual(codes(concat([buildAll, signEngineDebug, e2e])), ["RK_SUITE_LAUNCH_UNBOUND"], "the other declared bin is still unsigned");
  assert.deepEqual(concat([buildAll, { name: "sign-x", sign: { files: [{ from: "build-all", path: "debug/rightapp-engine" }, { from: "build-all", path: "debug/unrelated-tool" }] } }, e2e]), [], "every declared bin signed");
  // B. The manifest is read with a real TOML parser: single-quoted names and inline bin arrays declare bins too.
  for (const [form, manifest] of [["single-quoted names", "[package]\nname = 'rightapp-engine'\n\n[[bin]]\nname = 'rightapp-engine'\n\n[[bin]]\nname = 'unrelated-tool'\n"], ["an inline bin array", 'bin = [{ name = "rightapp-engine" }, { name = "unrelated-tool", path = "src/t.rs" }]\n\n[package]\nname = "rightapp-engine"\n']]) {
    const formed = concat([buildAll, signEngineDebug, e2e], { ...concatTree, "crates/engine/Cargo.toml": manifest });
    assert.deepEqual(codes(formed), ["RK_SUITE_LAUNCH_UNBOUND"], form);
    assert.match(formed[0].message, /rightapp-engine:unrelated-tool/, form);
  }
  assert.deepEqual(codes(concat([buildAll, signEngineDebug, e2e], { ...concatTree, "crates/engine/Cargo.toml": "[package\nname = " })), ["RK_SUITE_UNCLASSIFIED_ARTIFACT"], "a manifest that does not parse cannot be shown not to launch a bin");
  assert.deepEqual(codes(concat([buildAll, signEngineDebug, e2e], { ...concatTree, "crates/engine/Cargo.toml": null })), ["RK_SUITE_LAUNCH_UNBOUND"], "declared bins that cannot be read are never covered");
  // A sign step that names a suite AFTER the launch covers nothing, in the config and in the plan alike.
  assert.ok(codes(refusals(hr([signEngineDebug, e2e, buildAll]))).includes("RK_SUITE_LAUNCH_UNBOUND"));
  // The resolved plan is judged the same way.
  const planItem = (suite) => ({ name: suite.name, group: suite.name, step: suite.sign ? undefined : { cmd: suite.cmd, args: suite.args ?? [] }, extraArgs: [], ...(suite.sign ? { sign: suite.sign } : {}) });
  const planned = (suites, over = {}) => planSigningRefusals({ config: hr(suites), plan: suites.map(planItem), platform: "darwin", readFile, listFiles, ...over });
  assert.deepEqual(codes(planned([buildAll, signShell, e2e])), ["RK_SUITE_LAUNCH_UNBOUND"]);
  assert.deepEqual(planned([buildAll, signEngineDebug, e2e]), []);
  assert.deepEqual(codes(planned([buildRelease, signOf("build-all", "release/rightapp-engine"), e2e])), ["RK_SUITE_LAUNCH_UNBOUND"]);
  assert.deepEqual(codes(planned([shell, signOf("shell", "debug/rightapp-engine"), { name: "all", cmd: "cargo", args: ["test", "-p", "rightapp-engine"] }])), ["RK_SUITE_LAUNCH_UNBOUND"]);
  assert.deepEqual(codes(planned([buildAll, signEngineDebug, e2e], { readFile: (rel) => files[rel] ?? sharedTree[rel] ?? null, listFiles: (dir) => Object.keys(sharedTree).filter((name) => name.startsWith(`${dir}/`)) })), ["RK_SUITE_LAUNCH_UNBOUND"]);
  assert.ok(SIGNING_REFUSAL_CODES.includes("RK_SUITE_LAUNCH_UNBOUND"));
});

test("an artifact that cannot be classified is refused before it runs", () => {
  const bare = { sidecars: { sidecars: [{ kind: "cargo-bin", package: "mystery" }] }, tests: { runners: RUNNERS, suites: [{ name: "t", cmd: "cargo", args: ["test", "-p", "mystery"] }] } };
  assert.deepEqual(codes(refusals(bare)), ["RK_SUITE_UNCLASSIFIED_ARTIFACT"], "no manifest path: its tests cannot be found");
  assert.deepEqual(codes(refusals({ ...ENGINE, tests: { suites: [{ name: "t", cmd: "cargo", args: ["test", "-p", "rightapp-engine"] }] } }, { listFiles: null })), ["RK_SUITE_UNCLASSIFIED_ARTIFACT"], "no way to list the tests");
  assert.deepEqual(codes(refusals({ ...ENGINE, tests: { suites: [{ name: "t", cmd: "cargo", args: ["test", "-p", "rightapp-engine"] }] } }, { listFiles: () => null })), ["RK_SUITE_UNCLASSIFIED_ARTIFACT"], "a tests directory that cannot be read");
  assert.deepEqual(codes(refusals({ ...ENGINE, tests: { suites: [{ name: "t", cmd: "cargo", args: ["test", "-p", "rightapp-engine"] }] } }, { readFile: () => null })), ["RK_SUITE_UNCLASSIFIED_ARTIFACT"], "a test file that cannot be read");
  assert.deepEqual(codes(planRefusals(planOf({ runner: "cargo", passthrough: ["test", "-p", "mystery"] }, bare), { config: bare })), ["RK_SUITE_UNCLASSIFIED_ARTIFACT"]);
});

test("a sidecar package with no manifestPath is found through the workspace members (globs, exclude); missing or ambiguous stays unclassified", () => {
  const virtual = (extra = {}) => {
    const tree = {
      "Cargo.toml": '[workspace]\nmembers = [\n  "src-tauri",\n  "crates/*",\n]\nexclude = ["crates/skipped"]\n',
      "src-tauri/Cargo.toml": CARGO,
      "crates/engine/Cargo.toml": '[package]\nname = "heardright-engine"\n',
      "crates/engine/tests/e2e_engine.rs": 'let exe = env!("CARGO_BIN_EXE_heardright-engine");',
      "crates/helper/Cargo.toml": '[package]\nname = "helper"\n',
      "crates/skipped/Cargo.toml": '[package]\nname = "heardright-engine"\n',
      ...extra,
    };
    return { read: (rel) => tree[rel] ?? null, list: (dir) => Object.keys(tree).filter((name) => name.startsWith(dir === "." ? "" : `${dir}/`)) };
  };
  const config = { sidecars: { sidecars: [{ kind: "cargo-bin", package: "heardright-engine" }] } };
  const kindOf = (world, ...args) => {
    const targets = productTargets({ config, readFile: world.read, listFiles: world.list });
    const found = classifyStepArtifacts({ cmd: "cargo", args }, targets);
    return { dir: targets.dirs.get("heardright-engine"), kind: found.unknown.length ? "unknown" : found.launches ? "launches" : found.product ? "product" : "harness" };
  };
  const world = virtual();
  assert.deepEqual(kindOf(world, "test", "-p", "heardright-engine"), { dir: "crates/engine", kind: "launches" }, "resolved through crates/*, its e2e test launches the bin");
  assert.deepEqual(kindOf(world, "test", "-p", "heardright-engine", "--lib"), { dir: "crates/engine", kind: "harness" });
  assert.deepEqual(codes(refusals({ ...config, tests: { suites: [{ name: "e2e", cmd: "cargo", args: ["test", "-p", "heardright-engine", "--test", "e2e_engine"] }] } }, { readFile: world.read, listFiles: world.list })), ["RK_SUITE_UNSIGNED_BUILD"]);
  // Two members with the package's name (here the excluded one is counted out; drop the exclude and it is ambiguous).
  const ambiguous = virtual({ "Cargo.toml": '[workspace]\nmembers = ["src-tauri", "crates/*"]\n' });
  assert.equal(kindOf(ambiguous, "test", "-p", "heardright-engine").kind, "unknown");
  // Not a member at all.
  const missing = virtual({ "Cargo.toml": '[workspace]\nmembers = ["src-tauri"]\n' });
  assert.equal(kindOf(missing, "test", "-p", "heardright-engine").kind, "unknown");
  // No workspace root and no lister: unresolved.
  assert.equal(kindOf({ read: () => null, list: () => [] }, "test", "-p", "heardright-engine").kind, "unknown");
});

test("the dev win path hands the Authenticode installer the app root and the release config", () => {
  const source = fs.readFileSync(path.join(HERE, "development.mjs"), "utf8");
  const call = /installTauriAuthenticode\(\{([^]*?)deps: \{/.exec(source);
  assert.ok(call, "installTauriAuthenticode is called");
  assert.match(call[1], /appRoot: root/);
  assert.match(call[1], /releaseConfig: config/);
});

test("M2: a Mac or Dell dev target needs a signed install: command, portable, none, --build-only and stage entries are refused", () => {
  const mac = (install, extra = {}) => ({ development: { targets: { mac: { build: [], ...(install ? { install } : {}), ...extra } } } });
  assert.deepEqual(codes(refusals(mac({ kind: "command", cmd: "true" }), { to: "dev" })), ["RK_DEV_UNSIGNED_INSTALL"]);
  assert.deepEqual(codes(refusals(mac({ kind: "portable" }), { to: "dev" })), ["RK_DEV_UNSIGNED_INSTALL"]);
  assert.deepEqual(codes(refusals(mac(null), { to: "dev" })), ["RK_DEV_UNSIGNED_INSTALL"], "no install at all");
  assert.deepEqual(codes(refusals(mac({ kind: "tauri-devid" }, { stage: [{ from: "a", to: "b" }] }), { to: "dev" })), ["RK_DEV_UNSIGNED_INSTALL"], "stage copies the raw build");
  assert.deepEqual(codes(refusals(mac({ kind: "tauri-devid" }), { to: "dev", buildOnly: true })), ["RK_DEV_UNSIGNED_INSTALL"], "--build-only signs nothing");
  assert.deepEqual(refusals(mac({ kind: "tauri-devid" }), { to: "dev" }), []);
  assert.deepEqual(refusals(mac({ kind: "command", cmd: "true" }), { to: "dev", platform: "linux" }), [], "Linux builds sign nothing");
  assert.deepEqual(refusals({ development: { targets: { ios: { build: [] } } } }, { to: "dev", devPlatform: "ios" }), [], "iOS and Android are not Mac or Dell product installs");
});

test("M2: the desktop-app template's dev targets pass the policy on both hosts", () => {
  const template = JSON.parse(fs.readFileSync(path.join(HERE, "..", "..", "desktop-app-template", "right-release.config.json"), "utf8"));
  const tauriConf = JSON.stringify({ identifier: "app.rightsuite.rightapp" });
  const read = (rel) => (rel === "src-tauri/tauri.conf.json" ? tauriConf : rel === "src-tauri/Cargo.toml" ? CARGO : null);
  assert.deepEqual(suiteSigningRefusals({ config: template, platform: "darwin", to: "dev", devPlatform: "mac", readFile: read, listFiles: () => [] }), []);
  assert.deepEqual(suiteSigningRefusals({ config: template, platform: "win32", to: "dev", devPlatform: "win", readFile: read, listFiles: () => [] }), []);
});

// ---- the commit's own bytes, and the probe and a real run deciding alike ---------------------------------------------------------------

const git = (cwd, ...a) => spawnSync("git", a, { cwd, encoding: "utf8" });

function repoWith(t, repoFiles) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sign-route-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t");
  for (const [rel, body] of Object.entries(repoFiles)) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c");
  const commit = git(repo, "rev-parse", "HEAD").stdout.trim();
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  for (const k of ["GITHUB_ACTIONS", "TAURI_CONFIG", "RIGHTKIT_BUILD_OWNERSHIP", "RIGHTKIT_BUILD_OWNERSHIP_FILE"]) delete env[k];
  return { dir, repo, commit, env };
}

test("routeAppConfig reads the COMMIT's config and files, never the checkout's", (t) => {
  const r = repoWith(t, { "right-release.config.json": JSON.stringify({ tests: { suites: [BUILD] } }), "src-tauri/tauri.conf.json": TAURI_CONF, "apps/x/right-release.config.json": "{}" });
  const gitIn = (...a) => git(r.repo, ...a);
  // The checkout is edited after the commit: the route still judges the commit.
  fs.writeFileSync(path.join(r.repo, "right-release.config.json"), JSON.stringify({ tests: { suites: [] } }));
  const read = routeAppConfig({ git: gitIn, commit: r.commit, appDir: ".", platform: "win" });
  assert.equal(read.config.tests.suites.length, 1);
  assert.equal(read.devPlatform, "win");
  assert.equal(JSON.parse(read.readFile("src-tauri/tauri.conf.json")).identifier, "app.rightsuite.rightapp");
  assert.equal(read.readFile("src-tauri/none.json"), null);
  assert.equal(routeAppConfig({ git: gitIn, commit: r.commit, appDir: "apps/x" }).config && true, true);
  assert.equal(routeAppConfig({ git: gitIn, commit: r.commit, appDir: "apps/none" }), null);
  assert.equal(routeAppConfig({ git: gitIn, commit: null }), null);
  assert.equal(devTestPlatform(["--to", "dev", "--platform", "win"]), "win");
  assert.equal(devTestPlatform(["--to", "dev", "--platform", "mac", "--", "--platform", "win"]), "win", "a platform after -- wins, as runDevTest hands it on");
  assert.equal(devTestPlatform(["--to", "test"]), null);
});

test("parity: `--route` and a real run refuse with the same signing code on darwin and win32, and neither refuses on linux or hosted CI", async (t) => {
  const cases = [
    { to: "test", extra: [], config: { tests: { suites: [BUILD, QA] } }, code: "RK_SUITE_UNSIGNED_BUILD", hosts: ["darwin", "win32"] },
    { to: "test", extra: [], config: { tests: { suites: [{ name: "x", cmd: "node", args: ["a.mjs"], env: { RIGHTKIT_DEV_SIGNING: "ci-adhoc" } }] } }, code: "RK_SUITE_ADHOC_SIGNING", hosts: ["darwin", "win32"] },
    { to: "test", extra: [], config: { tests: { suites: [{ ...BUILD, args: [...BUILD.args, "--config", '{"identifier":"a.b"}'] }, SIGN] } }, code: "RK_SUITE_IDENTIFIER_OVERRIDE", hosts: ["darwin", "win32"] },
    { to: "test", extra: [], config: { ...ENGINE, tests: { suites: [{ name: "build-all", cmd: "cargo", args: ["build"] }, { name: "sign-shell", sign: { files: [{ from: "build-all", path: "debug/rightapp" }] } }, { name: "e2e", cmd: "cargo", args: ["test", "-p", "rightapp-engine", "--test", "e2e"] }] } }, code: "RK_SUITE_LAUNCH_UNBOUND", hosts: ["darwin", "win32"] },
    { to: "dev", extra: ["--platform", "mac"], config: { development: { targets: { mac: { build: [], install: { kind: "tauri-devid", bundleId: "app.other" } } } } }, code: "RK_SUITE_IDENTIFIER_MISMATCH", hosts: ["darwin", "win32"] },
    { to: "dev", extra: ["--platform", "win"], config: { development: { targets: { win: { build: [], install: { kind: "tauri" } } } } }, code: "RK_DEV_WIN_UNSIGNED_INSTALL", hosts: ["darwin", "win32"] },
    { to: "dev", extra: ["--platform", "mac"], config: { development: { targets: { mac: { build: [], install: { kind: "command", cmd: "true" } } } } }, code: "RK_DEV_UNSIGNED_INSTALL", hosts: ["darwin", "win32"] },
    { to: "test", extra: [], config: { sidecars: { sidecars: [{ package: "no-manifest" }] }, tests: { suites: [{ name: "t", cmd: "cargo", args: ["test", "-p", "no-manifest"] }] } }, code: "RK_SUITE_UNCLASSIFIED_ARTIFACT", hosts: ["darwin", "win32"] },
  ];
  assert.deepEqual(cases.map((c) => c.code).sort(), [...SIGNING_REFUSAL_CODES].sort());
  for (const item of cases) {
    const r = repoWith(t, { "right-release.config.json": JSON.stringify({ schema: 1, app: "x", ...item.config }), ...files, ...tree });
    const reached = [];
    const run = async (args, extra) => {
      const out = []; const err = [];
      const stops = { acquireOwnership: () => { reached.push("ownership"); throw Object.assign(new Error("stopped at the ownership"), { code: "busy" }); }, syncSnapshot: () => { throw new Error("must not sync"); }, runCheck: async () => 1, spawnInherit: () => ({ status: 1 }) };
      const code = await main(args, { cwd: r.repo, env: r.env, spawn: (c, a, o) => (c === "git" ? spawnSync(c, a, { ...o, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "x" }), snapshotRoot: path.join(r.dir, "snaps"), resolveExecutor: directExecutor, out: (m) => out.push(m), err: (m) => err.push(m), ...stops, ...extra });
      return { code, out, err: err.join("\n") };
    };
    const blocked = (err) => /BLOCKED: (RK_[A-Z0-9_]+):/.exec(err)?.[1] ?? null;
    for (const [platform, hosted] of [["darwin", false], ["win32", false], ["linux", false], ["darwin", true], ["win32", true]]) {
      const env = hosted ? { ...r.env, GITHUB_ACTIONS: "true", CI: "true", RUNNER_ENVIRONMENT: "github-hosted" } : r.env;
      const probe = await run(["--to", item.to, "--route", ...item.extra], { platform, env });
      assert.equal(probe.code, 0, probe.err);
      const answer = JSON.parse(probe.out.find((l) => l.startsWith("{")));
      const real = await run(["--to", item.to, ...item.extra], { platform, env });
      const expected = !hosted && item.hosts.includes(platform) ? item.code : null;
      const label = `${item.code} ${platform}${hosted ? " hosted" : ""}`;
      assert.equal(answer.refusal?.code ?? null, expected, `${label}: probe ${JSON.stringify(answer.refusal)}`);
      assert.equal(blocked(real.err), expected, `${label}: the real run printed ${real.err.slice(0, 300)}`);
      if (expected) assert.match(real.err, new RegExp(`BLOCKED: ${expected}: `));
    }
    assert.ok(reached.length > 0, "an admitted run goes on to its ownership");
  }
});

// ---- a legacy .mjs config is enforced at run time (the route probe reads JSON configs only) -------------------------------------------------

/** An app directory with a legacy right-release.config.mjs and the tauri files the policy reads. */
function mjsApp(t, config) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-mjs-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  fs.mkdirSync(path.join(dir, "src-tauri"), { recursive: true });
  fs.writeFileSync(path.join(dir, "src-tauri", "tauri.conf.json"), TAURI_CONF);
  fs.writeFileSync(path.join(dir, "src-tauri", "Cargo.toml"), CARGO);
  const file = path.join(dir, "right-release.config.mjs");
  fs.writeFileSync(file, `export default ${JSON.stringify(config)};\n`);
  return { dir, file };
}
const never = () => assert.fail("nothing may be spawned or built");
const hostEnforces = (process.platform === "darwin" || process.platform === "win32") && !(process.env.GITHUB_ACTIONS === "true" && process.env.CI === "true" && process.env.RUNNER_ENVIRONMENT === "github-hosted");

test(".mjs config: right-release test refuses an unsigned product build before any suite runs (loaded config, runSuites, and the real CLI main on a Mac or Dell)", async (t) => {
  const app = mjsApp(t, { tests: { suites: [BUILD, QA] } });
  const loaded = await loadAppConfig(app.file);
  const errors = []; const logs = [];
  const plan = planTests(loaded.config, { appRoot: loaded.appRoot });
  const result = await runSuites(plan, { appRoot: loaded.appRoot, config: loaded.config, platform: "darwin", env: {}, spawnFn: never, door: never, log: (line) => logs.push(line), errLog: (line) => errors.push(line) });
  assert.equal(result.exitCode, 1);
  assert.deepEqual(result.results, []);
  assert.deepEqual(logs, [], "no suite started");
  assert.match(errors.join("\n"), /\[RK_SUITE_UNSIGNED_BUILD\]/);
  // The same config, signed, runs.
  const signed = await loadAppConfig(mjsApp(t, { tests: { suites: [BUILD, SIGN] } }).file);
  assert.deepEqual(suiteSigningRefusals({ config: signed.config, platform: "darwin", to: "test", readFile }), []);
  // Hosted CI and Linux are exempt: same config, no refusal.
  const exempt = await runSuites([], { appRoot: loaded.appRoot, config: loaded.config, platform: "linux", env: {}, spawnFn: never, door: never, log: () => {}, errLog: never });
  assert.equal(exempt.exitCode, 0);
  const ci = await runSuites([], { appRoot: loaded.appRoot, config: loaded.config, platform: "darwin", env: { GITHUB_ACTIONS: "true", CI: "true", RUNNER_ENVIRONMENT: "github-hosted" }, spawnFn: never, door: never, log: () => {}, errLog: never });
  assert.equal(ci.exitCode, 0);
  // The real CLI entry, from the config file, on a host the policy applies to.
  if (hostEnforces) {
    const written = []; const original = console.error;
    console.error = (line) => written.push(String(line));
    try { assert.equal(await testMain(["--config", app.file]), 1); } finally { console.error = original; }
    assert.match(written.join("\n"), /\[RK_SUITE_UNSIGNED_BUILD\]/);
  }
});

test(".mjs config: ad hoc signing and an identifier override are refused by the test runner too", async (t) => {
  for (const [suite, wanted] of [[{ name: "x", node: "scripts/sign-qa-bundle.mjs" }, "RK_SUITE_ADHOC_SIGNING"], [{ ...BUILD, args: ["exec", "tauri", "build", "--config", '{"identifier":"x.y"}'] }, "RK_SUITE_IDENTIFIER_OVERRIDE"]]) {
    const app = mjsApp(t, { tests: { suites: [suite, SIGN] } });
    const loaded = await loadAppConfig(app.file);
    const errors = [];
    const result = await runSuites([], { appRoot: loaded.appRoot, config: loaded.config, platform: "darwin", env: {}, spawnFn: never, door: never, log: () => {}, errLog: (line) => errors.push(line) });
    assert.equal(result.exitCode, 1);
    assert.match(errors.join("\n"), new RegExp(`\\[${wanted}\\]`));
  }
});

test(".mjs config: right-release dev refuses an ad hoc dev target before it builds (real runDevelopment, a Mac host)", async (t) => {
  if (process.platform !== "darwin") { t.skip("the mac dev target runs on a Mac"); return; }
  const config = { development: { targets: { mac: { build: [{ cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle"] }], install: { kind: "tauri", productName: "RightApp", targetTriple: "aarch64-apple-darwin", signing: "ci-adhoc" } } } } };
  const app = mjsApp(t, config);
  const loaded = await loadAppConfig(app.file);
  let caught;
  try { runDevelopment({ root: loaded.appRoot, platform: "mac", config: loaded.config, env: {}, hostPlatform: "darwin", run: never, door: never }); } catch (error) { caught = error; }
  assert.ok(caught instanceof SuiteSigningError, String(caught?.stack ?? caught));
  assert.equal(caught.code, "RK_SUITE_ADHOC_SIGNING");
  // An identifier override in the dev build is refused the same way.
  const override = mjsApp(t, { development: { targets: { mac: { build: [{ cmd: "pnpm", args: ["exec", "tauri", "build", "--config", '{"identifier":"x.y"}'] }], install: { kind: "tauri-devid", productName: "RightApp", targetTriple: "aarch64-apple-darwin", destination: "/Applications/RightApp.app" } } } } });
  const loadedOverride = await loadAppConfig(override.file);
  let second;
  try { runDevelopment({ root: loadedOverride.appRoot, platform: "mac", config: loadedOverride.config, env: {}, hostPlatform: "darwin", run: never, door: never }); } catch (error) { second = error; }
  assert.equal(second?.code, "RK_SUITE_IDENTIFIER_OVERRIDE", String(second?.stack ?? second));
});

test(".mjs config: the real CLI entries (child processes) refuse it, the suite runner and the dev runner alike, and build nothing", (t) => {
  if (process.platform !== "darwin") { t.skip("the policy applies on a Mac or a Dell"); return; }
  const env = { ...process.env }; delete env.GITHUB_ACTIONS;
  const cli = (file, ...args) => spawnSync(process.execPath, [path.join(HERE, file), ...args], { encoding: "utf8", env });
  const suiteApp = mjsApp(t, { tests: { suites: [BUILD, QA] } });
  const ran = cli("test-command.mjs", "--config", suiteApp.file);
  assert.equal(ran.status, 1, ran.stderr);
  assert.match(ran.stderr, /right-release test: \[RK_SUITE_UNSIGNED_BUILD\]/);
  assert.doesNotMatch(ran.stdout, /suite_started/, "no suite started");
  const devApp = mjsApp(t, { development: { targets: { mac: { build: [{ cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle"] }], install: { kind: "tauri", productName: "RightApp", targetTriple: "aarch64-apple-darwin", signing: "ci-adhoc" } } } } });
  const dev = cli("development.mjs", "--platform", "mac", "--config", devApp.file);
  assert.equal(dev.status, 1, dev.stderr);
  assert.match(dev.stderr, /right-release dev: \[RK_SUITE_ADHOC_SIGNING\]/);
});
