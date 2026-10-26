import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { main, parseWebBuildArgs, planWebBuild, runWebBuild, webBuildDoorRefusal } from "./web-build.mjs";

const gitStub = (common = ".git") => (cmd, args) => cmd === "git" ? { status: 0, stdout: args[0] === "rev-parse" && args[1] === "HEAD" ? "a".repeat(40) : common } : { status: 0 };
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
    appRoot, env: {}, requireDoor: () => null, log: (line) => logs.push(line),
    spawnSyncFn: (cmd, args, options) => { if (cmd === "git") return gitStub()(cmd, args); calls.push({ cmd, args, options }); return { status: 0 }; },
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
  const code = runWebBuild(planWebBuild(config), { appRoot, env: {}, requireDoor: () => null, log: () => {}, spawnSyncFn: (...call) => { if (call[0] === "git") return gitStub()(...call); calls.push(call); return { status: 2 }; } });
  assert.equal(code, 2);
  assert.equal(calls.length, 1);
  assert.equal(runWebBuild(planWebBuild(config), { appRoot, env: {}, requireDoor: () => null, log: () => {}, spawnSyncFn: (cmd, args) => cmd === "git" ? gitStub()(cmd, args) : { status: null } }), 1);
  assert.throws(() => runWebBuild(planWebBuild(config), { appRoot, env: {}, requireDoor: () => null, log: () => {}, spawnSyncFn: (cmd, args) => cmd === "git" ? gitStub()(cmd, args) : { error: new Error("ENOENT") } }), /ENOENT/);
});

test("--dry-run prints the commands and runs nothing", () => {
  const logs = [];
  const code = runWebBuild(planWebBuild(config), { appRoot, env: {}, dryRun: true, requireDoor: () => null, log: (line) => logs.push(line), spawnSyncFn: (cmd, args) => { if (cmd === "git") return gitStub()(cmd, args); throw new Error("must not spawn"); } });
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


test("the door is checked before the config is imported: a refused web build never evaluates the config, dry run included (Astra, third pass)", async () => {
  let imported = 0;
  const loadConfig = async () => { imported += 1; return { config, appRoot }; };
  for (const argv of [["--config", path.join(appRoot, "right-release.config.mjs")], ["--config", path.join(appRoot, "right-release.config.mjs"), "--dry-run"]]) {
    await assert.rejects(main(argv, { env: {}, requireDoor: () => "no door", spawnSyncFn: gitStub(), loadConfig }), (e) => e.code === "RK_DOOR" && /no door/.test(e.message));
  }
  assert.equal(imported, 0, "the config was never imported");
  // Outside a git repository nothing binds to a commit: refused before the import as well.
  await assert.rejects(main(["--config", path.join(appRoot, "c.mjs")], { env: {}, requireDoor: () => null, spawnSyncFn: () => ({ status: 128, stdout: "" }), loadConfig }), /only inside a git repository/);
  assert.equal(imported, 0);
  // Under a live door run the config is imported and the steps run from the app root; the door receives the app's git dir and HEAD.
  const seen = [];
  const code = await main(["--config", path.join(appRoot, "right-release.config.mjs"), "--dry-run"], {
    env: { RK_GATE_RUN: "x" }, spawnSyncFn: gitStub(), loadConfig, log: () => {},
    requireDoor: ({ commonDir, commit, command }) => { seen.push({ commonDir, commit, command }); return null; },
  });
  assert.equal(code, 0);
  assert.equal(imported, 1);
  assert.deepEqual(seen[0], { commonDir: path.join(appRoot, ".git"), commit: "a".repeat(40), command: "right-release web-build" });
});

test("the exported runner refuses outside a door run too: no in-process caller spawns a step around the door (Astra, third pass)", () => {
  assert.throws(() => runWebBuild(planWebBuild(config), { appRoot, env: {}, spawnSyncFn: gitStub(), log: () => {} }), (e) => e.code === "RK_DOOR" && /one door/.test(e.message));
  let spawned = 0;
  assert.throws(() => runWebBuild(planWebBuild(config), { appRoot, env: {}, requireDoor: () => "shut", spawnSyncFn: (cmd) => { if (cmd !== "git") spawned += 1; return { status: 0, stdout: ".git" }; }, log: () => {} }), /shut/);
  assert.equal(spawned, 0);
  assert.equal(webBuildDoorRefusal({ appRoot, env: {}, spawnSyncFn: () => ({ status: 128, stdout: "" }) }), "right-release web-build runs only inside a git repository, under a door run");
});
