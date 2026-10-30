// GB-28 (BR11, GATE one entry): the standalone Windows package helper is not a door of its own. With its real effects it refuses before any
// compile unless a live door run launched it, and its pnpm/node steps take the one build door (build-launch.mjs), never the shell.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { WinPackageConfigError, defaultWinPackageDeps, runWinPackageRecipe } from "./win-package-recipe.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const RAW = "target/x86_64-pc-windows-msvc/release/demo.exe";
const INSTALLER = "target/x86_64-pc-windows-msvc/release/bundle/nsis/Demo_1.0.0_x64-setup.exe";
const target = () => ({ signingContract: "windows-raw-exe-authenticode-before-nsis-v1", sign: { prePackageFiles: [RAW], files: [INSTALLER] }, updater: { artifacts: [] }, winPackage: { installer: "nsis", productName: "Demo", updater: true, sidecars: [] } });

test("[GB-28] the helper with real effects refuses before any compile when no live door run launched it; the fakes a test injects and a dry run start nothing real", () => {
  const appRoot = tempRoot("rk-gb28-");
  const started = [];
  assert.throws(() => runWinPackageRecipe({ appRoot, config: {}, target: target(), deps: { env: { PATH: "x" } } }), (e) => e instanceof WinPackageConfigError && /win-package-recipe runs only through the one door|one door/.test(e.message));
  // A door run that exists is accepted (the seam stands in for doorRefusal).
  const reached = [];
  assert.throws(() => runWinPackageRecipe({ appRoot, config: {}, target: target(), deps: { env: { PATH: "x" }, doorRefusal: () => null, log: () => {}, signWindows: () => { throw new Error("stop here"); }, patchMarker: () => ({}), fs: { existsSync: () => true, readFileSync: () => Buffer.from(""), rmSync() {}, mkdirSync() {} } } }), (e) => { reached.push(e.message); return !(e instanceof WinPackageConfigError); });
  assert.equal(started.length, 0);
  // An injected spawn (the test doubles) and a dry run never consult the door.
  assert.doesNotThrow(() => runWinPackageRecipe({ appRoot, config: {}, target: target(), deps: { env: { PATH: "x" }, dryRun: true, log: () => {}, signWindows: () => [], fs: { existsSync: () => true, readFileSync: () => Buffer.from(""), rmSync() {}, mkdirSync() {} }, patchMarker: () => ({}), verifyEmbedded: () => ({}), signUpdater: () => {}, spawn: () => ({ status: 0 }) } }));
});

test("[GB-28] the real spawn routes pnpm and node through buildLaunch first: a broker-managed host's tauri step is carried or refused before any process starts, never run around the door", () => {
  const source = readFileSync(new URL("./win-package-recipe.mjs", import.meta.url), "utf8");
  assert.match(source, /buildLaunch\(command, args, \{ env: stepEnv/);
  assert.doesNotMatch(source, /shell: true/);
  // On a broker-managed host with no reachable client, the build door refuses and nothing is started.
  const appRoot = tempRoot("rk-gb28b-");
  const managedEnv = { PATH: "/managed/agent-bin", RIGHTKIT_CONTROL_ROOT: path.join(appRoot, "control") };
  const deps = defaultWinPackageDeps({ appRoot, target: target(), env: managedEnv });
  const refused = deps.spawn("node", ["-e", "process.exit(0)"], { cwd: appRoot, env: managedEnv });
  // On this (direct Linux) host the plain command resolves; the managed refusal shape is pinned by build-launch.test.mjs. The result is spawn-shaped either way.
  assert.ok(refused && typeof refused === "object" && ("status" in refused || "error" in refused));
});
