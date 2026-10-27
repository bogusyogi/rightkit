import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { diskImageMountPoints, OWN_RIGHT_RELEASE, parseRecipeBuildArgs, RecipeBuildError, resolveLaunch, runRecipeBuild, runRecipeCleanup, validateRecipePlan, verifyUniversalSidecars, waitForStableArtifacts } from "./recipe-build.mjs";

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
  assert.deepEqual(calls.map((call) => call.command), ["node", process.execPath, process.execPath, "ditto"]);
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

// ---- built-in cleanup and artifact stability (bogusyogi/rightkit#345 gap reports) ----

/** An in-memory fake of the fs surface recipe-build uses; `tree` maps absolute paths to true (file or dir). */
const imagePlist = (mounts) => `<?xml version="1.0"?><plist version="1.0"><dict><key>images</key><array>${mounts.map((mount, index) => `<dict><key>image-path</key><string>/tmp/image${index}.dmg</string><key>system-entities</key><array><dict><key>dev-entry</key><string>/dev/disk${index}s1</string><key>mount-point</key><string>${mount}</string></dict></array></dict>`).join("")}</array></dict></plist>`;
function fakeFs(tree, volumes = []) {
  const present = new Set(tree);
  const removed = [];
  return {
    removed,
    fs: {
      existsSync: (file) => present.has(file),
      readdirSync: (dir) => (dir === "/Volumes" ? volumes : [...present].filter((entry) => path.dirname(entry) === dir).map((entry) => path.basename(entry))),
      rmSync: (file) => { present.delete(file); removed.push(file); },
    },
  };
}

test("plan cleanup and stability are validated strictly", () => {
  let error;
  try {
    validateRecipePlan({ version: 1, steps: [{ phase: "build", cmd: "x" }], cleanup: { remove: ["../x", 1], apps: { dir: "/abs", remove: "y", nope: 1 }, dmgVolumes: ["a/b"], extra: 1 }, stability: { files: [], intervalMs: 0 } });
  } catch (caught) { error = caught; }
  assert.equal(error.code, "RECIPE_BUILD_PLAN_INVALID");
  for (const needle of ["cleanup: unknown key \"extra\"", "cleanup.remove[0] must be relative", "cleanup.remove[1] must be a non-empty", "cleanup.apps.dir must be relative", "cleanup.apps.remove must be a boolean", "cleanup.apps: unknown key \"nope\"", "cleanup.dmgVolumes must be a list", "stability.files must be a non-empty", "stability.intervalMs must be a positive integer"]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
  const ok = validateRecipePlan({ version: 1, steps: [{ phase: "build", cmd: "x" }], cleanup: { remove: ["a/b.exe"], apps: { dir: "bundle/macos", unregister: true }, dmgVolumes: ["App"] }, stability: { files: ["a/b.exe"], checks: 3 } });
  assert.deepEqual(ok.cleanup, { remove: ["a/b.exe"], apps: { dir: "bundle/macos", remove: false, unregister: true }, dmgVolumes: ["App"] });
  assert.deepEqual(ok.stability, { files: ["a/b.exe"], checks: 3 });
});

test("plan paths reject the app root and its ancestors, and cleanup refuses a notarize.app at the app root", () => {
  for (const bad of [".", "./", "", "a/..", "./a/../.", "x/../.."]) {
    assert.throws(() => validateRecipePlan({ version: 1, steps: [{ phase: "build", cmd: "x" }], cleanup: { remove: [bad] } }), (error) => error.code === "RECIPE_BUILD_PLAN_INVALID", JSON.stringify(bad));
  }
  // A step may run at the app root; only paths that are removed, wiped or polled must be strictly inside it.
  assert.doesNotThrow(() => validateRecipePlan({ version: 1, steps: [{ phase: "build", cmd: "x", cwd: "." }] }));
  assert.throws(() => validateRecipePlan({ version: 1, steps: [{ phase: "build", cmd: "x" }], stability: { files: ["./"] } }), (error) => error.code === "RECIPE_BUILD_PLAN_INVALID");
  assert.throws(() => validateRecipePlan({ version: 1, steps: [{ phase: "build", cmd: "x" }], cleanup: { apps: { dir: ".", remove: true } } }), (error) => error.code === "RECIPE_BUILD_PLAN_INVALID");
  // Called directly, a notarize.app sitting at the app root is refused and nothing is removed.
  const stale = path.join(APP, "notarize.app");
  const { fs, removed } = fakeFs([stale]);
  assert.throws(() => runRecipeCleanup({ apps: { dir: ".", remove: true, unregister: false } }, { appRoot: APP, platform: "linux", fs }), (error) => error.code === "RECIPE_BUILD_CLEANUP_UNSAFE");
  assert.deepEqual(removed, []);
});

test("cleanup detaches stale DMG volumes, unregisters then removes stale apps, and removes stale files, all before the build", () => {
  const dir = path.join(APP, "bundle", "macos");
  const stale = path.join(dir, "Old.app");
  const rawExe = path.join(APP, "out", "demo.exe");
  const { fs, removed } = fakeFs([stale, path.join(dir, "notes.txt"), rawExe], ["App", "App 1", "App 2", "App Backup", "Other", "AppleSeed"]);
  const { spawnSync, calls } = fakeSpawn();
  const order = [];
  const tracking = (command, args, options) => { order.push(args[0] === "-u" || command === "hdiutil" ? "cleanup" : "step"); const run = spawnSync(command, args, options); return command === "hdiutil" && args[0] === "info" ? { ...run, stdout: imagePlist(["/Volumes/App", "/Volumes/App 1"]) } : run; };
  const result = runRecipeBuild({ ...plan(), cleanup: { dmgVolumes: ["App"], apps: { dir: "bundle/macos", remove: true, unregister: true }, remove: ["out/demo.exe", "out/missing.exe"] } }, { appRoot: APP, platform: "darwin", env: {}, spawnSync: tracking, fs });
  const cleanups = calls.filter((call) => call.command === "hdiutil" || call.args[0] === "-u");
  assert.deepEqual(cleanups.filter((call) => call.command === "hdiutil" && call.args[0] === "detach").map((call) => call.args), [["detach", "/Volumes/App"], ["detach", "/Volumes/App 1"]]);
  assert.deepEqual(calls.find((call) => call.command === "hdiutil").args, ["info", "-plist"]);
  const lsregister = cleanups.find((call) => call.args[0] === "-u");
  assert.ok(lsregister.command.endsWith("/lsregister"));
  assert.deepEqual(lsregister.args, ["-u", stale]);
  assert.deepEqual(removed, [stale, rawExe]);
  assert.deepEqual(result.cleaned.removed, [stale, rawExe]);
  assert.deepEqual(result.cleaned.unregistered, [stale]);
  assert.deepEqual(order.slice(0, 5), ["cleanup", "cleanup", "cleanup", "cleanup", "step"]);
});

test("cleanup skips the macOS-only parts elsewhere, keeps going when a detach fails, and never invokes a shell", () => {
  const { fs } = fakeFs([], ["App"]);
  const logs = [];
  const { spawnSync, calls } = fakeSpawn();
  runRecipeBuild({ ...plan(), cleanup: { dmgVolumes: ["App"], apps: { dir: "b", unregister: true } } }, { appRoot: APP, platform: "win32", env: { npm_execpath: "/pnpm.cjs" }, spawnSync, fs, log: (line) => logs.push(line) });
  assert.ok(!calls.some((call) => call.command === "hdiutil"));
  assert.ok(logs.some((line) => line.includes("dmgVolumes skipped")));
  const calls2 = [];
  const spawn2 = (command, args) => { calls2.push({ command, args }); return command === "hdiutil" && args[0] === "info" ? { status: 0, stdout: imagePlist(["/Volumes/App"]) } : { status: command === "hdiutil" ? 1 : 0 }; };
  const result = runRecipeBuild({ ...plan(), cleanup: { dmgVolumes: ["App"] } }, { appRoot: APP, platform: "darwin", env: {}, spawnSync: spawn2, fs, log: (line) => logs.push(line) });
  assert.deepEqual(result.cleaned.detached, []);
  assert.ok(logs.some((line) => line.includes("did not succeed")));
  // F2: plain detach first, -force only on the retry of a confirmed image mount.
  assert.deepEqual(calls2.filter((call) => call.command === "hdiutil" && call.args[0] === "detach").map((call) => call.args), [["detach", "/Volumes/App"], ["detach", "-force", "/Volumes/App"]]);
  assert.ok(calls2.length > 1, "the build still ran");
});


test("F2: only the exact volume name or '<name> <N>' that hdiutil reports as a disk image is detached; a real volume or lookalike is never touched", () => {
  const { fs } = fakeFs([], ["App", "App 2", "App Backup", "AppleSeed", "App 1x", "Macintosh HD"]);
  const calls = [];
  const logs = [];
  // App is a real (non-image) volume; App 2 is an image mount. hdiutil lists only image mounts, including an unrelated one.
  const spawn = (command, args) => { calls.push({ command, args }); return command === "hdiutil" && args[0] === "info" ? { status: 0, stdout: imagePlist(["/Volumes/App 2", "/Volumes/Other Image", "/Volumes/App Backup"]) } : { status: 0 }; };
  const result = runRecipeBuild({ ...plan(), cleanup: { dmgVolumes: ["App"] } }, { appRoot: APP, platform: "darwin", env: {}, spawnSync: spawn, fs, log: (line) => logs.push(line) });
  assert.deepEqual(calls.filter((call) => call.args[0] === "detach").map((call) => call.args), [["detach", "/Volumes/App 2"]]);
  assert.deepEqual(result.cleaned.detached, ["App 2"]);
  assert.ok(logs.some((line) => line.includes("/Volumes/App (not a mounted disk image)") || line.includes("leaves /Volumes/App ")));
  // hdiutil info failing detaches nothing.
  const none = [];
  const spawnFail = (command, args) => { none.push(args[0]); return { status: command === "hdiutil" && args[0] === "info" ? 1 : 0 }; };
  assert.deepEqual(runRecipeBuild({ ...plan(), cleanup: { dmgVolumes: ["App"] } }, { appRoot: APP, platform: "darwin", env: {}, spawnSync: spawnFail, fs }).cleaned.detached, []);
  assert.ok(!none.includes("detach"));
  assert.deepEqual([...diskImageMountPoints("<key>mount-point</key><string>/Volumes/Orphan</string>")], []);
});

test("artifact stability polling: waits for a stable size and mtime over two checks, one second apart", () => {
  const file = path.join(APP, "out", "demo.exe");
  const samples = [{ size: 10, mtimeMs: 1 }, { size: 20, mtimeMs: 2 }, { size: 30, mtimeMs: 3 }, { size: 30, mtimeMs: 3 }, { size: 30, mtimeMs: 3 }];
  let read = 0;
  const sleeps = [];
  const statSync = () => samples[Math.min(read++, samples.length - 1)];
  const logs = [];
  const files = waitForStableArtifacts({ files: ["out/demo.exe"] }, { appRoot: APP, statSync, sleep: (ms) => sleeps.push(ms), now: () => 0, log: (line) => logs.push(line) });
  assert.deepEqual(files, [file]);
  assert.deepEqual(sleeps, [1000, 1000, 1000, 1000]);
  assert.equal(logs.length, 1);

  const clock = { t: 0 };
  assert.throws(() => waitForStableArtifacts({ files: ["out/demo.exe"], timeoutMs: 5000 }, { appRoot: APP, statSync: () => { throw new Error("ENOENT"); }, sleep: (ms) => { clock.t += ms; }, now: () => clock.t }), { code: "RECIPE_BUILD_ARTIFACT_UNSTABLE" });
  let counter = 0;
  clock.t = 0;
  assert.throws(() => waitForStableArtifacts({ files: ["out/demo.exe"], timeoutMs: 5000 }, { appRoot: APP, statSync: () => ({ size: counter++, mtimeMs: 1 }), sleep: (ms) => { clock.t += ms; }, now: () => clock.t }), { code: "RECIPE_BUILD_ARTIFACT_UNSTABLE" });
  assert.deepEqual(waitForStableArtifacts(undefined, { appRoot: APP }), []);
});

test("stability runs right after the build step and before postBuild; an unstable artifact stops the run", () => {
  const { spawnSync, calls } = fakeSpawn();
  const stat = { size: 5, mtimeMs: 7 };
  const result = runRecipeBuild({ ...plan([{ phase: "postBuild", cmd: "ditto" }]), stability: { files: ["out/demo.exe"] } }, { appRoot: APP, platform: "linux", env: {}, spawnSync, statSync: () => stat, sleep: () => { calls.push({ command: "sleep" }); } });
  assert.deepEqual(calls.map((call) => call.command), ["node", process.execPath, "pnpm", "sleep", "sleep", "ditto"]);
  assert.deepEqual(result.stable, [path.join(APP, "out", "demo.exe")]);
  const clock = { t: 0 };
  const { spawnSync: spawn3, calls: calls3 } = fakeSpawn();
  assert.throws(() => runRecipeBuild({ ...plan([{ phase: "postBuild", cmd: "ditto" }]), stability: { files: ["out/demo.exe"], timeoutMs: 100 } }, { appRoot: APP, platform: "linux", env: {}, spawnSync: spawn3, statSync: () => { throw new Error("gone"); }, sleep: (ms) => { clock.t += ms; }, now: () => clock.t }), { code: "RECIPE_BUILD_ARTIFACT_UNSTABLE" });
  assert.ok(!calls3.some((call) => call.command === "ditto"));
});

// ---- right-release resolves to this package's own CLI; universal sidecar check; nativeHelpers section ----

test("a step's right-release (bare, `pnpm exec right-release`, `pnpm right-release`) launches this package's own CLI, never the app's copy", () => {
  const own = OWN_RIGHT_RELEASE;
  assert.ok(path.isAbsolute(own) && own.endsWith(path.join("cli", "right-release.mjs")));
  const env = { npm_execpath: "/pnpm.cjs" };
  for (const platform of ["darwin", "win32"]) {
    assert.deepEqual(resolveLaunch({ cmd: "right-release", args: ["stage-sidecar", "--target", "x"], platform, env, execPath: "/node" }), { command: "/node", args: [own, "stage-sidecar", "--target", "x"], shell: false });
    assert.deepEqual(resolveLaunch({ cmd: "pnpm", args: ["exec", "right-release", "stage-sidecar"], platform, env, execPath: "/node" }), { command: "/node", args: [own, "stage-sidecar"], shell: false });
    assert.deepEqual(resolveLaunch({ cmd: "pnpm", args: ["right-release", "web-build"], platform, env, execPath: "/node" }), { command: "/node", args: [own, "web-build"], shell: false });
  }
  // Other pnpm commands are unchanged.
  assert.deepEqual(resolveLaunch({ cmd: "pnpm", args: ["exec", "tauri", "build"], platform: "darwin", env, execPath: "/node" }), { command: "/node", args: ["/pnpm.cjs", "exec", "tauri", "build"], shell: false });
  // End to end through the runner, for prepare, build and postBuild steps.
  const { spawnSync, calls } = fakeSpawn();
  runRecipeBuild({ version: 1, steps: [
    { phase: "prepare", cmd: "right-release", args: ["stage-sidecar"] },
    { phase: "build", cmd: "pnpm", args: ["exec", "right-release", "web-build"] },
    { phase: "postBuild", cmd: "pnpm", args: ["exec", "right-release", "verify"] },
  ] }, { appRoot: APP, platform: "linux", env: {}, spawnSync });
  assert.ok(calls.every((call) => call.command === process.execPath && call.args[0] === OWN_RIGHT_RELEASE));
  assert.deepEqual(calls.map((call) => call.args[1]), ["stage-sidecar", "web-build", "verify"]);
});

const sidecarReceipt = (files, target = "universal-apple-darwin") => JSON.stringify({ schema: 1, target, files });
const verifySection = { receipt: "src-tauri/binaries/stage-sidecar.json", target: "universal-apple-darwin" };

test("verifySidecars runs after prepare and before the build; it needs both archs (and nothing else) in every universal externalBin", () => {
  const good = { name: "engine", path: "src-tauri/binaries/engine-universal-apple-darwin", externalBin: true };
  const calls = [];
  const spawnSync = (command, args) => { calls.push(command === "lipo" ? `lipo ${path.basename(args[1])}` : command); return { status: 0, stdout: "x86_64 arm64\n" }; };
  const fs = { readFileSync: () => sidecarReceipt([good, { name: "ffmpeg", path: "src-tauri/resources/runtime/ffmpeg", externalBin: false }]) };
  runRecipeBuild({ ...plan(), verifySidecars: verifySection }, { appRoot: APP, platform: "linux", env: {}, spawnSync, fs });
  assert.deepEqual(calls, ["node", process.execPath, "lipo engine-universal-apple-darwin", "pnpm"], "prepare, prepare, verify, then the build");
  const run = (files, lipoOut = "arm64 x86_64", target) => verifyUniversalSidecars(verifySection, { appRoot: APP, readFile: () => sidecarReceipt(files, target), spawnSync: () => ({ status: 0, stdout: lipoOut }) });
  assert.deepEqual(run([good]), ["src-tauri/binaries/engine-universal-apple-darwin"]);
  for (const [files, out, target, needle] of [
    [[good], "arm64", undefined, /exactly arm64 and x86_64/],
    [[good], "x86_64", undefined, /exactly arm64 and x86_64/],
    [[good], "arm64 x86_64 arm64e", undefined, /exactly arm64 and x86_64/],
    [[{ ...good, path: "src-tauri/binaries/engine-aarch64-apple-darwin" }], "arm64 x86_64", undefined, /not <name>-universal-apple-darwin/],
    [[good], "arm64 x86_64", "aarch64-apple-darwin", /staged for aarch64-apple-darwin/],
  ]) assert.throws(() => run(files, out, target), (error) => error.code === "RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL" && needle.test(error.message), String(needle));
  assert.throws(() => verifyUniversalSidecars(verifySection, { appRoot: APP, readFile: () => { throw new Error("ENOENT"); }, spawnSync: () => ({ status: 0 }) }), { code: "RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL", message: /cannot read the stage-sidecar receipt/ });
  assert.throws(() => verifyUniversalSidecars(verifySection, { appRoot: APP, readFile: () => sidecarReceipt([good]), spawnSync: () => ({ status: 1, stdout: "" }) }), { code: "RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL" });
  // A failing verification stops before the build step.
  const seen = [];
  assert.throws(() => runRecipeBuild({ ...plan(), verifySidecars: verifySection }, { appRoot: APP, platform: "linux", env: {}, spawnSync: (command, args) => { seen.push(args.join(" ")); return { status: 0, stdout: "arm64\n" }; }, fs: { readFileSync: () => sidecarReceipt([good]) } }), { code: "RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL" });
  assert.ok(!seen.some((line) => line.includes("exec tauri build")));
  assert.throws(() => validateRecipePlan({ ...plan(), verifySidecars: { receipt: "../x", target: "aarch64-apple-darwin", extra: 1 } }), (error) => /verifySidecars: unknown key "extra"/.test(error.message) && /target must be universal-apple-darwin/.test(error.message));
});

test("the nativeHelpers section is validated with the shared schema and runs after the build, before postBuild", () => {
  const helper = { name: "menu", package: "macos-native/Pkg", product: "Menu", kind: "executable", embed: "Contents/MacOS/menu", universal: true };
  const section = { app: "dist/Demo.app", helpers: [helper], configuration: "release" };
  assert.throws(() => validateRecipePlan({ ...plan(), nativeHelpers: { ...section, helpers: [{ ...helper, extra: 1 }], app: "/abs/Demo.app", forceArch: "ppc" } }), (error) => error.code === "RECIPE_BUILD_PLAN_INVALID" && /unknown key "extra"/.test(error.message) && /must be relative/.test(error.message) && /forceArch/.test(error.message));
  const BIN = "/bin-dir";
  const files = new Map([[`${APP}/dist/Demo.app/Contents/Info.plist`, 1], [`${APP}/macos-native/Pkg/Package.swift`, 1], [`${BIN}/Menu`, 1]]);
  const fs = { existsSync: (file) => files.has(file), mkdirSync() {}, copyFileSync: (from, to) => files.set(to, 1), chmodSync() {}, readFileSync: () => Buffer.from("x"), rmSync() {}, readFileSyncReceipt: undefined };
  const order = [];
  const spawnSync = (command, args) => {
    const tag = command === "swift" ? (args.includes("--show-bin-path") ? "swift-bin" : "swift-build") : command === "lipo" ? "lipo" : command === "ditto" ? "postBuild" : args.includes("tauri") ? "build" : "prepare";
    order.push(tag);
    return { status: 0, stdout: tag === "swift-bin" ? `${BIN}\n` : "arm64 x86_64\n" };
  };
  const result = runRecipeBuild({ ...plan([{ phase: "postBuild", cmd: "ditto", args: ["a", "b"] }]), nativeHelpers: section }, { appRoot: APP, platform: "linux", env: { npm_execpath: "/pnpm.cjs" }, spawnSync, fs });
  assert.deepEqual(order, ["prepare", "prepare", "build", "swift-build", "swift-bin", "lipo", "postBuild"]);
  assert.deepEqual(result.nativeHelpers.helpers.map((item) => item.name), ["menu"]);
});
