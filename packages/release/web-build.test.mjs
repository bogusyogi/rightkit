import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { parseWebBuildArgs, planWebBuild, runWebBuild } from "./web-build.mjs";

const appRoot = path.resolve("fixture-app");
const config = {
  web: {
    build: [
      { node: "node_modules/typescript/bin/tsc", nodeFlags: ["--disable-warning=DEP0205"] },
      { node: "node_modules/vite/bin/vite.js", args: ["build"], nodeFlags: ["--disable-warning=DEP0205"] },
    ],
  },
};

test("web.build steps resolve to node invocations from the app root, in order", () => {
  const calls = [];
  const logs = [];
  const code = runWebBuild(planWebBuild(config), {
    appRoot, env: {}, log: (line) => logs.push(line),
    spawnSyncFn: (cmd, args, options) => { calls.push({ cmd, args, options }); return { status: 0 }; },
  });
  assert.equal(code, 0);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].cmd, process.execPath);
  assert.deepEqual(calls[0].args, ["--disable-warning=DEP0205", path.join(appRoot, "node_modules/typescript/bin/tsc")]);
  assert.deepEqual(calls[1].args, ["--disable-warning=DEP0205", path.join(appRoot, "node_modules/vite/bin/vite.js"), "build"]);
  assert.equal(calls[1].options.cwd, appRoot);
  assert.equal(calls[1].options.stdio, "inherit");
  assert.equal(logs.length, 2);
});

test("a failing step stops the build and its exit code is returned; a spawn error throws", () => {
  const calls = [];
  const code = runWebBuild(planWebBuild(config), { appRoot, env: {}, log: () => {}, spawnSyncFn: (...call) => { calls.push(call); return { status: 2 }; } });
  assert.equal(code, 2);
  assert.equal(calls.length, 1);
  assert.equal(runWebBuild(planWebBuild(config), { appRoot, env: {}, log: () => {}, spawnSyncFn: () => ({ status: null }) }), 1);
  assert.throws(() => runWebBuild(planWebBuild(config), { appRoot, env: {}, log: () => {}, spawnSyncFn: () => ({ error: new Error("ENOENT") }) }), /ENOENT/);
});

test("--dry-run prints the commands and runs nothing", () => {
  const logs = [];
  const code = runWebBuild(planWebBuild(config), { appRoot, env: {}, dryRun: true, log: (line) => logs.push(line), spawnSyncFn: () => { throw new Error("must not spawn"); } });
  assert.equal(code, 0);
  assert.equal(logs.length, 2);
  assert.ok(logs.every((line) => line.startsWith("[dry-run] ")));
});

test("planWebBuild refuses a missing, empty or malformed section", () => {
  assert.throws(() => planWebBuild({}), /no web\.build steps/);
  assert.throws(() => planWebBuild({ web: { build: [] } }), /no web\.build steps/);
  assert.throws(() => planWebBuild({ web: { build: [{ cmd: "x", shell: true }] } }), /unknown key/);
  assert.throws(() => planWebBuild({ web: { build: [{ cmd: "x" }], other: 1 } }), /unknown key/);
  assert.deepEqual(parseWebBuildArgs(["--config", "c.mjs", "--dry-run"]), { config: "c.mjs", dryRun: true });
  assert.throws(() => parseWebBuildArgs(["--nope"]), /unknown web-build argument/);
});
