import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { EventEmitter } from "node:events";
import { CommandSpecError, loadAppConfig, mergeNodeOptions, resolveInvocation, validateStep } from "./command-spec.mjs";
import { eventPrefix, parseTestArgs, planTests, runSuites } from "./test-command.mjs";

const appRoot = path.resolve("fixture-app");
const config = {
  tests: {
    eventPrefix: "e2e",
    runners: {
      vitest: { node: "node_modules/vitest/vitest.mjs", nodeFlags: ["--no-experimental-webstorage"], nodeOptions: ["--no-experimental-webstorage"] },
    },
    suites: [
      { name: "tooling", runner: "vitest", args: ["run", "scripts/tooling.test.mjs", "--maxWorkers=1"] },
      { name: "runtime-config", node: "scripts/runtime.test.mjs", nodeFlags: ["--test"] },
      { name: "toolbar", cmd: "node", args: ["scripts/toolbar.mjs"] },
    ],
  },
};

test("command-spec: one executable form, no unknown keys, app-relative paths only", () => {
  assert.doesNotThrow(() => validateStep({ node: "a.mjs", args: ["x"], nodeFlags: ["--f"], cwd: "sub", env: { A: "1" } }, "step"));
  assert.throws(() => validateStep({ cmd: "x", node: "a.mjs" }, "step"), /both cmd and node/);
  assert.throws(() => validateStep({ cmd: "x", shell: true }, "step"), /unknown key/);
  assert.throws(() => validateStep({ node: "../a.mjs" }, "step"), /stay inside the app/);
  assert.throws(() => validateStep({ cmd: "x", cwd: "/abs" }, "step"), /stay inside the app/);
  assert.throws(() => validateStep({ cmd: "x", args: [1] }, "step"), /list of strings/);
  assert.throws(() => validateStep({ cmd: "x", env: { A: 1 } }, "step"), /object of strings/);
  assert.throws(() => validateStep("x", "step"), CommandSpecError);
});

test("command-spec: node steps run through this Node, flags first, NODE_OPTIONS merged once", () => {
  const out = resolveInvocation({ node: "s/run.mjs", args: ["a"], nodeFlags: ["--f"], nodeOptions: ["--o"], env: { K: "v" } }, { appRoot, baseEnv: { NODE_OPTIONS: "--o --p" }, extraArgs: ["b"] });
  assert.equal(out.cmd, process.execPath);
  assert.deepEqual(out.args, ["--f", path.join(appRoot, "s/run.mjs"), "a", "b"]);
  assert.equal(out.env.NODE_OPTIONS, "--o --p");
  assert.equal(out.env.K, "v");
  assert.equal(mergeNodeOptions("", ["--x"]), "--x");
  assert.equal(mergeNodeOptions("--a", ["--x", "--a"]), "--a --x");
  const plain = resolveInvocation({ cmd: "pnpm", args: ["exec", "x"], cwd: "w" }, { appRoot, baseEnv: {} });
  assert.deepEqual([plain.cmd, plain.args, plain.cwd], ["pnpm", ["exec", "x"], path.join(appRoot, "w")]);
  assert.equal(resolveInvocation({ cmd: "node", args: ["-v"] }, { appRoot, baseEnv: {} }).cmd, process.execPath);
});

test("planTests merges a runner into a suite and keeps declared order", () => {
  const plan = planTests(config, { appRoot });
  assert.deepEqual(plan.map((item) => item.name), ["tooling", "runtime-config", "toolbar"]);
  const tooling = resolveInvocation(plan[0].step, { appRoot, baseEnv: {} });
  assert.deepEqual(tooling.args, ["--no-experimental-webstorage", path.join(appRoot, "node_modules/vitest/vitest.mjs"), "run", "scripts/tooling.test.mjs", "--maxWorkers=1"]);
  assert.equal(tooling.env.NODE_OPTIONS, "--no-experimental-webstorage");
  assert.deepEqual(resolveInvocation(plan[1].step, { appRoot, baseEnv: {} }).args, ["--test", path.join(appRoot, "scripts/runtime.test.mjs")]);
  assert.equal(eventPrefix(config), "e2e");
  assert.equal(eventPrefix({ tests: {} }), "test");
  assert.throws(() => eventPrefix({ tests: { eventPrefix: "E2E!" } }), /eventPrefix/);
});

test("planTests selects by name, runs a runner with passthrough, and refuses ambiguous input", () => {
  assert.deepEqual(planTests(config, { appRoot, suites: ["toolbar", "tooling"] }).map((item) => item.name), ["tooling", "toolbar"]);
  const direct = planTests(config, { appRoot, runner: "vitest", passthrough: ["run", "--project", "unit"] });
  assert.deepEqual(resolveInvocation(direct[0].step, { appRoot, baseEnv: {}, extraArgs: direct[0].extraArgs }).args.slice(-3), ["run", "--project", "unit"]);
  const one = planTests(config, { appRoot, suites: ["toolbar"], passthrough: ["--extra"] });
  assert.deepEqual(one[0].extraArgs, ["--extra"]);
  assert.throws(() => planTests(config, { appRoot, suites: ["nope"] }), /unknown suite: nope/);
  assert.throws(() => planTests(config, { appRoot, runner: "nope" }), /unknown runner/);
  assert.throws(() => planTests(config, { appRoot, suites: ["toolbar", "tooling"], passthrough: ["x"] }), /exactly one/);
  assert.throws(() => planTests(config, { appRoot, runner: "vitest", suites: ["toolbar"] }), /not both/);
  assert.throws(() => planTests({}, { appRoot }), /declares no tests/);
  assert.throws(() => planTests({ tests: { suites: [] } }, { appRoot }), /non-empty/);
  assert.throws(() => planTests({ tests: { suites: [{ name: "a" }] } }, { appRoot }), /needs cmd, node/);
  assert.throws(() => planTests({ tests: { suites: [{ name: "a", cmd: "x" }, { name: "a", cmd: "y" }] } }, { appRoot }), /duplicate suite name/);
  assert.throws(() => planTests({ tests: { suites: [{ name: "a", runner: "r", cmd: "x" }] } }, { appRoot }), /undeclared runner/);
  assert.throws(() => planTests({ tests: { suites: [{ cmd: "x" }] } }, { appRoot }), /name must be set/);
  assert.throws(() => planTests({ tests: { suites: [{ name: "a", cmd: "x" }], extra: 1 } }, { appRoot }), /unknown key/);
});

test("journeys expand into chunked suites that a group name selects together", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-release-test-command-"));
  try {
    mkdirSync(path.join(dir, "scripts"), { recursive: true });
    writeFileSync(path.join(dir, "scripts", "journeys.json"), JSON.stringify({ journeys: ["a", "b", "c", "d", "e"] }));
    const cfg = { tests: { suites: [
      { name: "unit", cmd: "node", args: ["u.mjs"] },
      { name: "journeys", node: "scripts/qa.mjs", args: ["--keep-going"], journeys: { file: "scripts/journeys.json", chunk: 2, flag: "--journey" } },
    ] } };
    const plan = planTests(cfg, { appRoot: dir });
    assert.deepEqual(plan.map((item) => item.name), ["unit", "journeys-1", "journeys-2", "journeys-3"]);
    assert.deepEqual(plan[1].step.args, ["--keep-going", "--journey", "a,b"]);
    assert.deepEqual(plan[3].step.args, ["--keep-going", "--journey", "e"]);
    assert.deepEqual(planTests(cfg, { appRoot: dir, suites: ["journeys"] }).map((item) => item.name), ["journeys-1", "journeys-2", "journeys-3"]);
    assert.deepEqual(planTests(cfg, { appRoot: dir, suites: ["journeys-2"] }).map((item) => item.name), ["journeys-2"]);
    const bad = (journeys) => () => planTests({ tests: { suites: [{ name: "j", node: "x.mjs", journeys }] } }, { appRoot: dir });
    assert.throws(bad({ file: "scripts/journeys.json", chunk: 0, flag: "--journey" }), /positive integer/);
    assert.throws(bad({ file: "scripts/journeys.json", chunk: 2, flag: "journey" }), /flag/);
    assert.throws(bad({ file: "scripts/missing.json", chunk: 2, flag: "--journey" }), /cannot be read/);
    assert.throws(bad({ file: "scripts/journeys.json", key: "nope", chunk: 2, flag: "--journey" }), /list of names/);
    assert.throws(bad({ file: "scripts/journeys.json", chunk: 2, flag: "--journey", extra: 1 }), /unknown key/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("parseTestArgs reads suites, runner, flags and the -- passthrough", () => {
  assert.deepEqual(parseTestArgs(["--suite", "a,b", "--suite", "c", "--list", "--json", "--config", "x.mjs"]), { config: "x.mjs", suites: ["a", "b", "c"], runner: undefined, list: true, json: true, passthrough: [] });
  assert.deepEqual(parseTestArgs(["--runner", "vitest", "--", "run", "--project", "unit"]).passthrough, ["run", "--project", "unit"]);
  assert.throws(() => parseTestArgs(["--suite"]), /needs a value/);
  assert.throws(() => parseTestArgs(["--nope"]), /unknown test argument/);
});

function fakeSpawn(exitCodes, calls) {
  return (cmd, args, options) => {
    calls.push({ cmd, args, options });
    const child = new EventEmitter();
    const code = exitCodes.shift();
    setImmediate(() => (code instanceof Error ? child.emit("error", code) : code === "signal" ? child.emit("exit", null, "SIGTERM") : child.emit("exit", code, null)));
    return child;
  };
}

test("runSuites runs in order, emits events, stops at the first failure and returns its code", async () => {
  const plan = planTests(config, { appRoot });
  const calls = [];
  const logs = [];
  const ok = await runSuites(plan, { appRoot, prefix: "e2e", env: {}, spawnFn: fakeSpawn([0, 0, 0], calls), log: (line) => logs.push(JSON.parse(line)) });
  assert.equal(ok.exitCode, 0);
  assert.equal(calls.length, 3);
  assert.deepEqual(logs.filter((e) => e.event === "e2e_suite_started").map((e) => e.suite), ["tooling", "runtime-config", "toolbar"]);
  assert.equal(logs.filter((e) => e.event === "e2e_suite_completed").length, 3);
  assert.equal(calls[0].options.stdio, "inherit");
  assert.equal(calls[0].options.cwd, appRoot);

  const failCalls = [];
  const failLogs = [];
  const failed = await runSuites(plan, { appRoot, env: {}, spawnFn: fakeSpawn([0, 3], failCalls), log: (line) => failLogs.push(JSON.parse(line)) });
  assert.equal(failed.exitCode, 3);
  assert.equal(failCalls.length, 2, "the third suite never starts");
  assert.deepEqual(failed.results.map((r) => r.exitCode), [0, 3]);
  assert.equal(failLogs[0].event, "test_suite_started");

  await assert.rejects(() => runSuites(plan, { appRoot, env: {}, spawnFn: fakeSpawn(["signal"], []), log: () => {} }), /stopped by SIGTERM/);
  await assert.rejects(() => runSuites(plan, { appRoot, env: {}, spawnFn: fakeSpawn([new Error("ENOENT")], []), log: () => {} }), /ENOENT/);
});

test("command-spec: loadAppConfig loads right-release.config.json from the working directory and names the app root", async () => {
  const root = realpathSync(mkdtempSync(path.join(os.tmpdir(), "command-spec-json-")));
  const previous = process.cwd();
  try {
    writeFileSync(path.join(root, "right-release.config.json"), JSON.stringify({ schema: 1, app: "demo", tests: { suites: [{ name: "unit", cmd: "node", args: ["--test"] }] } }));
    process.chdir(root);
    const loaded = await loadAppConfig();
    assert.equal(loaded.config.app, "demo");
    assert.equal(loaded.config.tests.suites[0].name, "unit");
    assert.equal(loaded.appRoot, root);
    assert.equal(loaded.configPath, path.join(root, "right-release.config.json"));
    assert.equal(parseTestArgs([]).config, "right-release.config.json", "the CLI default follows the same rule");
    assert.deepEqual((await loadAppConfig(path.join(root, "right-release.config.json"))).config, loaded.config);
    writeFileSync(path.join(root, "right-release.config.json"), '{ "schema": 1, "app": "demo", "unknown": true }');
    await assert.rejects(loadAppConfig(), (error) => error.code === "RELEASE_CONFIG_KEY");
  } finally { process.chdir(previous); rmSync(root, { recursive: true, force: true }); }
});

test("command-spec: loadAppConfig still imports a legacy right-release.config.mjs when it is the only config", async () => {
  const root = realpathSync(mkdtempSync(path.join(os.tmpdir(), "command-spec-legacy-")));
  const previous = process.cwd();
  try {
    writeFileSync(path.join(root, "right-release.config.mjs"), 'export default { schema: 1, app: "legacy" };\n');
    process.chdir(root);
    assert.equal((await loadAppConfig()).config.app, "legacy");
    assert.equal(parseTestArgs([]).config, "right-release.config.mjs");
  } finally { process.chdir(previous); rmSync(root, { recursive: true, force: true }); }
});
