#!/usr/bin/env node
// `right-release web-build [--config <file>] [--dry-run]` (also `right-release build --web`): the web frontend build
// alone, with no Tauri, signing or packaging. It runs the config's `web.build` steps in order, without a shell, from
// the app root, and stops at the first failure.
//
//   web: {
//     build: [
//       { node: "node_modules/typescript/bin/tsc", nodeFlags: ["--disable-warning=DEP0205"] },
//       { node: "node_modules/vite/bin/vite.js", args: ["build"], nodeFlags: ["--disable-warning=DEP0205"] },
//     ],
//   }
//
// A step is `{ cmd | node, args?, cwd?, env?, nodeFlags?, nodeOptions? }` (see command-spec.mjs). `--dry-run` prints the
// resolved commands and runs nothing. Exit code: 0, or the failing step's exit code.
//
// The door (gate-stages.mjs requireDoorRun) is checked before the config is imported: a .mjs config is code, so even a dry run
// runs only under a live door run. The exported runner checks it again, so no in-process caller spawns a step around it.
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { existsSync } from "node:fs";
import { buildLaunch } from "./build-launch.mjs";
import { CommandSpecError, loadAppConfig, resolveInvocation, validateStep } from "./command-spec.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";
import { requireDoorRun } from "./gate-stages.mjs";
import { doorContextFor } from "./source-split.mjs";

export function parseWebBuildArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), dryRun: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error("--config needs a value");
      opts.config = value;
      i += 1;
    } else if (arg === "--dry-run") opts.dryRun = true;
    else throw new Error(`unknown web-build argument: ${arg}`);
  }
  return opts;
}

/** The validated `web.build` steps of a config. */
export function planWebBuild(config) {
  const steps = config?.web?.build;
  if (!Array.isArray(steps) || steps.length === 0) throw new CommandSpecError("the config declares no web.build steps");
  const unknown = Object.keys(config.web).filter((key) => key !== "build");
  if (unknown.length) throw new CommandSpecError(`web has unknown key(s): ${unknown.join(", ")} (allowed: build)`);
  return steps.map((step, index) => validateStep(step, `web.build[${index}]`));
}

/**
 * The door refusal for a web build of the app at `appRoot`, or null under a live door run. Outside a git repository nothing can be
 * bound to a commit: refused. `spawnSyncFn` runs git; `requireDoor` is gate-stages.mjs requireDoorRun.
 */
export function webBuildDoorRefusal({ appRoot, env = process.env, spawnSyncFn = spawnSync, requireDoor = requireDoorRun }) {
  // Inside a source split (the door's snapshot of a commit) the git context is the door run's checkout and commit, not the cwd's.
  let context;
  try { context = doorContextFor({ cwd: appRoot, env, run: spawnSyncFn }); } catch (error) { return `right-release web-build: ${error.code ?? "RK_SOURCE_SPLIT"}: ${error.message}`; }
  if (!context) return "right-release web-build runs only inside a git repository, under a door run";
  return requireDoor({ env, commonDir: context.commonDir, commit: context.commit, command: "right-release web-build" });
}

function doorError(refusal) { const error = new Error(refusal); error.code = "RK_DOOR"; return error; }

/**
 * Run the steps in order under a live door run; `spawnSyncFn`, `requireDoor` and `log` are injectable. Returns the exit code. The
 * door is checked here as well as in main: this is the only place a step is spawned.
 */
export function runWebBuild(steps, { appRoot, env = process.env, dryRun = false, spawnSyncFn = spawnSync, requireDoor = requireDoorRun, log = console.log, door = buildLaunch } = {}) {
  const refusal = webBuildDoorRefusal({ appRoot, env, spawnSyncFn, requireDoor });
  if (refusal) throw doorError(refusal);
  for (const step of steps) {
    const invocation = resolveInvocation(step, { appRoot, baseEnv: env });
    log(`${dryRun ? "[dry-run] " : ""}${[invocation.cmd, ...invocation.args].join(" ")}`);
    if (dryRun) continue;
    // A web build is a build: through the one door (build-launch.mjs), a broker job on a managed host.
    const target = door(invocation.cmd, invocation.args, { env: invocation.env, stepEnv: step.env, cwd: invocation.cwd, exists: existsSync });
    const result = spawnSyncFn(target.command, target.args, { cwd: invocation.cwd, env: target.env ? { ...invocation.env, ...target.env } : invocation.env, stdio: "inherit" });
    if (result.error) throw result.error;
    if (result.status !== 0) return result.status ?? 1;
  }
  return 0;
}

export async function main(argv = process.argv.slice(2), { env = process.env, requireDoor = requireDoorRun, spawnSyncFn = spawnSync, loadConfig = loadAppConfig, log = console.log } = {}) {
  const opts = parseWebBuildArgs(argv);
  // A web build is a compile: like right-release build, the direct entry runs only under a door run (rightkit-gate --to compile runs
  // class A first). The door is checked before the config is imported, since a .mjs config is code; a dry run is no exception.
  const appRoot = path.dirname(path.resolve(opts.config));
  const refusal = webBuildDoorRefusal({ appRoot, env, spawnSyncFn, requireDoor });
  if (refusal) throw doorError(refusal);
  const { config } = await loadConfig(opts.config);
  return runWebBuild(planWebBuild(config), { appRoot, env, dryRun: opts.dryRun, spawnSyncFn, requireDoor, log });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release web-build: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
