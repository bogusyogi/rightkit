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
import { SIGNING_REFUSAL_CODES, adHocReason, identifierOverrideReason, isProductBuildStep, productTargets, signingRefusalHost, suiteSigningRefusals } from "./suite-signing-policy.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const directExecutor = () => ({ kind: "direct", cargo: ["cargo"] });
const BUILD = { name: "build-app", cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle"] };
const SIGN = { name: "sign-app", sign: { files: [{ from: "build-app", path: "debug/rightapp" }] } };
const QA = { name: "qa", cmd: "node", args: ["qa.mjs"] };
const TAURI_CONF = JSON.stringify({ productName: "RightApp", identifier: "app.rightsuite.rightapp" });
const CARGO = '[package]\nname = "rightapp"\nversion = "0.1.0"\n';
const files = { "src-tauri/tauri.conf.json": TAURI_CONF, "src-tauri/Cargo.toml": CARGO };
const readFile = (rel) => files[rel] ?? null;
const refusals = (config, over = {}) => suiteSigningRefusals({ config, platform: "darwin", to: "test", readFile, ...over });
const codes = (list) => list.map((item) => item.code);

// ---- what counts -------------------------------------------------------------------------------------------------------------------

test("a product build: tauri build and bundle, and a cargo build of the tauri package or a sidecar package; a test build is not one", () => {
  const targets = productTargets({ config: { sidecars: { sidecars: [{ kind: "cargo-bin", package: "rightapp-engine", manifestPath: "crates/engine/Cargo.toml" }] } }, readFile });
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
    { to: "dev", config: { development: { targets: { mac: { build: [], install: { kind: "tauri-devid", bundleId: "app.other" } } } } }, code: "RK_SUITE_IDENTIFIER_MISMATCH", only: "darwin" },
    { to: "dev", config: { development: { targets: { win: { build: [], install: { kind: "tauri" } } } } }, code: "RK_DEV_WIN_UNSIGNED_INSTALL", only: "win32" },
  ];
  assert.deepEqual([...new Set(configs.map((c) => c.code))].sort(), [...SIGNING_REFUSAL_CODES].sort(), "every code is exercised");
  const plan = (platform, env, { to, config }) => routePlan({ platform, env, resolveExecutor: directExecutor, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "" }, { to, stages: [], devTest: true, executor: directExecutor(), appConfig: { config, readFile } });
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
  assert.equal(signingRefusalHost("win32", { GITHUB_ACTIONS: "true" }), false);
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
    { to: "dev", extra: ["--platform", "mac"], config: { development: { targets: { mac: { build: [], install: { kind: "tauri-devid", bundleId: "app.other" } } } } }, code: "RK_SUITE_IDENTIFIER_MISMATCH", hosts: ["darwin", "win32"] },
    { to: "dev", extra: ["--platform", "win"], config: { development: { targets: { win: { build: [], install: { kind: "tauri" } } } } }, code: "RK_DEV_WIN_UNSIGNED_INSTALL", hosts: ["darwin", "win32"] },
  ];
  assert.deepEqual(cases.map((c) => c.code).sort(), [...SIGNING_REFUSAL_CODES].sort());
  for (const item of cases) {
    const r = repoWith(t, { "right-release.config.json": JSON.stringify({ schema: 1, app: "x", ...item.config }), ...files });
    const reached = [];
    const run = async (args, extra) => {
      const out = []; const err = [];
      const stops = { acquireOwnership: () => { reached.push("ownership"); throw Object.assign(new Error("stopped at the ownership"), { code: "busy" }); }, syncSnapshot: () => { throw new Error("must not sync"); }, runCheck: async () => 1, spawnInherit: () => ({ status: 1 }) };
      const code = await main(args, { cwd: r.repo, env: r.env, spawn: (c, a, o) => (c === "git" ? spawnSync(c, a, { ...o, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "x" }), snapshotRoot: path.join(r.dir, "snaps"), resolveExecutor: directExecutor, out: (m) => out.push(m), err: (m) => err.push(m), ...stops, ...extra });
      return { code, out, err: err.join("\n") };
    };
    const blocked = (err) => /BLOCKED: (RK_[A-Z0-9_]+):/.exec(err)?.[1] ?? null;
    for (const [platform, hosted] of [["darwin", false], ["win32", false], ["linux", false], ["darwin", true], ["win32", true]]) {
      const env = hosted ? { ...r.env, GITHUB_ACTIONS: "true" } : r.env;
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
const hostEnforces = (process.platform === "darwin" || process.platform === "win32") && process.env.GITHUB_ACTIONS !== "true";

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
  const ci = await runSuites([], { appRoot: loaded.appRoot, config: loaded.config, platform: "darwin", env: { GITHUB_ACTIONS: "true" }, spawnFn: never, door: never, log: () => {}, errLog: never });
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
