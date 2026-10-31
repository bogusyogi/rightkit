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

test("runSuites runs in order and emits events; a failure no longer hides the independent suites after it (GB-44); a signal still cancels the whole run", async () => {
  const plan = planTests(config, { appRoot });
  const calls = [];
  const logs = [];
  const ok = await runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, prefix: "e2e", env: {}, spawnFn: fakeSpawn([0, 0, 0], calls), log: (line) => logs.push(JSON.parse(line)) });
  assert.equal(ok.exitCode, 0);
  assert.equal(calls.length, 3);
  assert.deepEqual(logs.filter((e) => e.event === "e2e_suite_started").map((e) => e.suite), ["tooling", "runtime-config", "toolbar"]);
  assert.equal(logs.filter((e) => e.event === "e2e_suite_completed").length, 3);
  assert.equal(calls[0].options.stdio, "inherit");
  assert.equal(calls[0].options.cwd, appRoot);

  const failCalls = [];
  const failLogs = [];
  const failed = await runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn([0, 3, 0], failCalls), log: (line) => failLogs.push(JSON.parse(line)), errLog: () => {} });
  assert.equal(failed.exitCode, 3, "the run ends with the first failing code");
  assert.equal(failCalls.length, 3, "the independent third suite still runs");
  assert.deepEqual(failed.results.map((r) => r.exitCode), [0, 3, 0]);
  assert.equal(failLogs[0].event, "test_suite_started");

  await assert.rejects(() => runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn(["signal"], []), log: () => {} }), /stopped by SIGTERM/);
  const errored = await runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn([new Error("ENOENT"), 0, 0], []), log: () => {}, errLog: () => {} });
  assert.equal(errored.exitCode, 1, "a suite that cannot start is a failed suite, recorded, not a rejection of the whole run");
  assert.match(errored.results[0].error, /ENOENT/);
  assert.deepEqual(errored.results.map((r) => r.exitCode), [1, 0, 0]);
});

test("[GB-44] first suite fails, an unrelated second fails, a third needs the first's output, a fourth is independent: both failures and the pass are recorded, the dependent is skipped with its reason, the run is non-zero", async () => {
  const cfg = { tests: { suites: [
    { name: "build", cmd: "node", args: ["build.mjs"] },
    { name: "unrelated", cmd: "node", args: ["unrelated.mjs"] },
    { name: "needs-build", cmd: "node", args: ["consumer.mjs"], requires: ["build"] },
    { name: "independent", cmd: "node", args: ["independent.mjs"] },
  ] } };
  const plan = planTests(cfg, { appRoot });
  assert.deepEqual(plan.map((p) => p.requires ?? null), [null, null, ["build"], null]);
  const calls = []; const errs = []; const logs = [];
  const result = await runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn([2, 5, 0], calls), log: (l) => logs.push(JSON.parse(l)), errLog: (l) => errs.push(l) });
  assert.deepEqual(calls.map((c) => path.basename(c.args[0])), ["build.mjs", "unrelated.mjs", "independent.mjs"], "the dependent never started");
  assert.deepEqual(result.results.map((r) => [r.suite, r.exitCode, r.skipped === true]), [["build", 2, false], ["unrelated", 5, false], ["needs-build", null, true], ["independent", 0, false]]);
  assert.match(result.results[2].reason, /"build" did not pass/);
  assert.ok(logs.some((e) => e.event === "test_suite_skipped" && e.suite === "needs-build"));
  assert.equal(result.exitCode, 2, "the aggregate terminal outcome is non-zero (the first failing code)");
  assert.match(errs.join("\n"), /needs-build: skipped/);
  assert.throws(() => planTests({ tests: { suites: [{ name: "a", cmd: "node", requires: ["later"] }, { name: "later", cmd: "node" }] } }, { appRoot }), /requires must be a list of earlier suite names/);
});

test("[GB-44] a failed sign step signs nothing and no product consumer runs after it, but suites before it and independent ones keep their diagnostics; a sign whose build failed is skipped, not attempted", async () => {
  const signing = (name, from) => ({ name, sign: { files: [{ from, path: "debug/rightapp" }] } });
  const cfg = { tests: { suites: [
    { name: "build", cmd: "node", args: ["build.mjs"] },
    { name: "lint", cmd: "node", args: ["lint.mjs"] },
    signing("sign", "build"),
    { name: "e2e", cmd: "node", args: ["e2e.mjs"], env: { RIGHTKIT_QA_APP_BINARY: "$signed" } },
    { name: "docs", cmd: "node", args: ["docs.mjs"], requires: [] },
  ] } };
  const plan = planTests(cfg, { appRoot });
  const calls = []; const signed = [];
  // 1. The build fails: the sign is skipped (never attempted), the consumer is skipped, lint and docs still run.
  const buildFails = await runSuites(plan, { door: (c, a) => ({ command: c, args: a }), appRoot, env: {}, spawnFn: fakeSpawn([1, 0, 0], calls), log: () => {}, errLog: () => {}, signStep: () => { signed.push("attempted"); throw new Error("must not sign"); } });
  assert.deepEqual(calls.map((c) => path.basename(c.args[0])), ["build.mjs", "lint.mjs", "docs.mjs"]);
  assert.deepEqual(signed, []);
  assert.deepEqual(buildFails.results.map((r) => [r.suite, r.skipped === true ? "skipped" : r.exitCode]), [["build", 1], ["lint", 0], ["sign", "skipped"], ["e2e", "skipped"], ["docs", 0]]);
  assert.equal(buildFails.exitCode, 1);
  // 2. The build passes but the sign fails: the consumer never launches; independent suites still run.
  const calls2 = [];
  const { SuiteSigningError } = await import("./suite-signing-policy.mjs");
  const signFails = await runSuites(plan, { door: (c, a) => ({ command: c, args: a }), appRoot, env: {}, spawnFn: fakeSpawn([0, 0, 0], calls2), log: () => {}, errLog: () => {}, signStep: () => { throw new SuiteSigningError("SIGN_FAILED", "codesign refused"); } });
  assert.deepEqual(calls2.map((c) => path.basename(c.args[0])), ["build.mjs", "lint.mjs", "docs.mjs"], "no product consumer after a failed sign");
  assert.deepEqual(signFails.results.map((r) => [r.suite, r.skipped === true ? "skipped" : r.exitCode]), [["build", 0], ["lint", 0], ["sign", 1], ["e2e", "skipped"], ["docs", 0]]);
  assert.equal(signFails.exitCode, 1);
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

test("[GB-79] a planned `rightkit-qa journeys run` suite fails at planning with the named capability before ANY suite runs (no earlier build is wasted, nothing is skipped, an empty journey set is never green)", async () => {
  const { QA_JOURNEYS_UNSUPPORTED, qaJourneysRunStep } = await import("./test-command.mjs");
  const cfg = { tests: { suites: [
    { name: "build", cmd: "node", args: ["build.mjs"] },
    { name: "journeys", cmd: "rightkit-qa", args: ["journeys", "run", "--app", "x"] },
  ] } };
  const plan = planTests(cfg, { appRoot });
  const calls = []; const errs = [];
  const result = await runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn([0, 0], calls), log: () => {}, errLog: (l) => errs.push(l), snapshotStage: true, qaCapability: () => false });
  assert.equal(result.exitCode, 1);
  assert.deepEqual(calls, [], "the earlier build suite never ran: planning refused first");
  const directCalls = [];
  const direct = await runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn([0, 0], directCalls), log: () => {}, errLog: () => {}, snapshotStage: false, qaCapability: () => false });
  assert.equal(direct.exitCode, 0);
  assert.equal(directCalls.length, 2, "outside a gate-carried snapshot stage the suite starts as before");
  assert.match(errs.join("\n"), new RegExp(`\\[${QA_JOURNEYS_UNSUPPORTED}\\] journeys:.*journeys-prebuilt-input`));
  assert.equal(qaJourneysRunStep({ cmd: "rightkit-qa", args: ["run-tests"] }), false);
  assert.equal(qaJourneysRunStep({ cmd: "/x/rightkit-qa", args: ["journeys", "run"] }), true);
  assert.equal(qaJourneysRunStep({ cmd: "rightkit-qa", args: ["journeys"] }, ["run"]), true, "passthrough arguments count");
  assert.equal(qaJourneysRunStep({ cmd: "rightkit-qa", args: ["journeys", "receipt", "--root", "."] }), false, "reading a receipt compiles nothing");
  // With the capability (rightkit-qa implements the prebuilt journey input) the same plan starts both suites; the door carries the build.
  const capableCalls = [];
  const capable = await runSuites(plan, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn([0, 0], capableCalls), log: () => {}, errLog: () => {}, snapshotStage: true, qaCapability: () => true });
  assert.equal(capable.exitCode, 0);
  assert.equal(capableCalls.length, 2, "with journeys-prebuilt-input both suites start");
  // A step that names its own listing, attestation or plan flag is an input the gate did not verify: refused at planning, whatever the capability.
  for (const flag of [["--from-cargo-json", "/x/l.jsonl"], ["--prebuilt-attestation", "/x/a.json"], ["--print-build-plan"]]) {
    const own = planTests({ tests: { suites: [{ name: "build", cmd: "node", args: ["build.mjs"] }, { name: "journeys", cmd: "rightkit-qa", args: ["journeys", "run", ...flag] }] } }, { appRoot });
    const ownCalls = []; const ownErrs = [];
    const refused = await runSuites(own, { door: (command, args) => ({ command, args }), appRoot, env: {}, spawnFn: fakeSpawn([0, 0], ownCalls), log: () => {}, errLog: (l) => ownErrs.push(l), snapshotStage: true, qaCapability: () => true });
    assert.equal(refused.exitCode, 1, flag.join(" "));
    assert.deepEqual(ownCalls, [], `${flag[0]}: refused before any suite ran`);
    assert.match(ownErrs.join("\n"), new RegExp(`${flag[0]} is the gate's own`));
  }
});
