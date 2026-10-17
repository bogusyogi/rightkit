import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { parseRecipeBuildArgs, RecipeBuildError, resolveLaunch, runRecipeBuild, validateRecipePlan } from "./recipe-build.mjs";

const APP = path.resolve("/rk-recipe-app");
const plan = (extra = []) => ({
  version: 1,
  steps: [
    { phase: "prepare", cmd: "node", args: ["scripts/stage.mjs"] },
    { phase: "prepare", cmd: "right-release", args: ["stage-sidecar"], cwd: "." },
    { phase: "build", cmd: "pnpm", args: ["exec", "tauri", "build"], cwd: "src-tauri" },
    ...extra,
  ],
});

/** A fake spawnSync recording each launch; `statusFor(index)` picks the exit code. */
function fakeSpawn(statusFor = () => 0) {
  const calls = [];
  const spawnSync = (command, args, options) => { calls.push({ command, args, options }); return { status: statusFor(calls.length - 1) }; };
  return { spawnSync, calls };
}

test("runs prepare, build, then postBuild in order, with argv, the app-root cwd and no shell", () => {
  const { spawnSync, calls } = fakeSpawn();
  const logs = [];
  const result = runRecipeBuild(plan([{ phase: "postBuild", cmd: "ditto", args: ["a.dylib", "b.dylib"] }]), { appRoot: APP, env: { npm_execpath: "/pnpm.cjs", KEEP: "1" }, platform: "linux", spawnSync, log: (line) => logs.push(line) });
  assert.deepEqual(result.steps.map((step) => step.phase), ["prepare", "prepare", "build", "postBuild"]);
  assert.deepEqual(calls.map((call) => call.command), ["node", "right-release", process.execPath, "ditto"]);
  assert.deepEqual(calls[2].args, ["/pnpm.cjs", "exec", "tauri", "build"]);
  assert.equal(calls[2].options.cwd, path.join(APP, "src-tauri"));
  assert.equal(calls[0].options.cwd, APP);
  assert.ok(calls.every((call) => call.options.shell === false && call.options.windowsHide === true && call.options.env.KEEP === "1"));
  assert.equal(logs.length, 4);
});

test("fails closed on the first non-zero exit and runs nothing after it", () => {
  for (const failAt of [0, 2, 3]) {
    const { spawnSync, calls } = fakeSpawn((index) => (index === failAt ? 9 : 0));
    let error;
    try { runRecipeBuild(plan([{ phase: "postBuild", cmd: "ditto" }]), { appRoot: APP, platform: "linux", env: {}, spawnSync }); } catch (caught) { error = caught; }
    assert.ok(error instanceof RecipeBuildError);
    assert.equal(error.code, "RECIPE_BUILD_STEP_FAILED");
    assert.equal(error.details.index, failAt);
    assert.equal(calls.length, failAt + 1);
  }
  const spawnError = () => ({ error: new Error("ENOENT"), status: null });
  assert.throws(() => runRecipeBuild(plan(), { appRoot: APP, platform: "linux", env: {}, spawnSync: spawnError }), { code: "RECIPE_BUILD_STEP_FAILED", message: /ENOENT/ });
});

test("the plan is validated strictly and every problem is reported at once", () => {
  let error;
  try {
    validateRecipePlan({ version: 2, extra: 1, steps: [
      { phase: "postBuild", cmd: "a" },
      { phase: "prepare", cmd: "b", args: "x" },
      { phase: "build", cmd: "", cwd: "../out" },
      { phase: "build", cmd: "c", env: { A: 1 }, shell: true },
      { phase: "nope", cmd: "d" },
    ] });
  } catch (caught) { error = caught; }
  assert.equal(error.code, "RECIPE_BUILD_PLAN_INVALID");
  for (const needle of ["unknown key \"extra\"", "version must be 1", "comes after a later phase", "args must be a list of strings", "cmd must be a non-empty string", "must be relative and stay inside the app", "env must map names to strings", "unknown key \"shell\"", "phase must be one of", "exactly one build step"]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
  assert.throws(() => validateRecipePlan({ version: 1, steps: [] }), { code: "RECIPE_BUILD_PLAN_INVALID" });
  assert.throws(() => runRecipeBuild(plan(), { appRoot: "relative", spawnSync: fakeSpawn().spawnSync }), { code: "RECIPE_BUILD_PLAN_INVALID" });
});

test("launch resolution: pnpm through npm_execpath, Windows shims only with shell-safe arguments", () => {
  assert.deepEqual(resolveLaunch({ cmd: "pnpm", args: ["exec"], platform: "win32", env: { npm_execpath: "C:/p/pnpm.cjs" }, execPath: "C:/node.exe" }), { command: "C:/node.exe", args: ["C:/p/pnpm.cjs", "exec"], shell: false });
  assert.deepEqual(resolveLaunch({ cmd: "tool.exe", args: ["--config", "{\"a\":1}"], platform: "win32", env: {} }), { command: "tool.exe", args: ["--config", "{\"a\":1}"], shell: false });
  assert.equal(resolveLaunch({ cmd: "pnpm", args: ["exec", "tauri"], platform: "win32", env: {} }).shell, true);
  assert.throws(() => resolveLaunch({ cmd: "pnpm", args: ["--config", "{\"a\":1}"], platform: "win32", env: {} }), { code: "RECIPE_BUILD_LAUNCH_UNSAFE" });
  assert.equal(resolveLaunch({ cmd: "pnpm", args: ["--config", "{\"a\":1}"], platform: "darwin", env: {} }).shell, false);
});

test("recipe-build CLI arguments", () => {
  assert.deepEqual(parseRecipeBuildArgs(["--plan", "{}"]), { plan: "{}", planFile: undefined, appRoot: undefined });
  assert.deepEqual(parseRecipeBuildArgs(["--plan-file", ".right-release/p.json", "--app-root", "/a"]), { plan: undefined, planFile: ".right-release/p.json", appRoot: "/a" });
  assert.throws(() => parseRecipeBuildArgs([]), /exactly one/);
  assert.throws(() => parseRecipeBuildArgs(["--plan", "{}", "--plan-file", "x"]), /exactly one/);
  assert.throws(() => parseRecipeBuildArgs(["--plan"]), /needs a value/);
  assert.throws(() => parseRecipeBuildArgs(["--bogus"]), /unknown/);
});
