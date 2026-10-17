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
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { CommandSpecError, loadAppConfig, resolveInvocation, validateStep } from "./command-spec.mjs";

export function parseWebBuildArgs(argv) {
  const opts = { config: "right-release.config.mjs", dryRun: false };
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

/** Run the steps in order; `spawnSyncFn` and `log` are injectable. Returns the exit code. */
export function runWebBuild(steps, { appRoot, env = process.env, dryRun = false, spawnSyncFn = spawnSync, log = console.log } = {}) {
  for (const step of steps) {
    const invocation = resolveInvocation(step, { appRoot, baseEnv: env });
    log(`${dryRun ? "[dry-run] " : ""}${[invocation.cmd, ...invocation.args].join(" ")}`);
    if (dryRun) continue;
    const result = spawnSyncFn(invocation.cmd, invocation.args, { cwd: invocation.cwd, env: invocation.env, stdio: "inherit" });
    if (result.error) throw result.error;
    if (result.status !== 0) return result.status ?? 1;
  }
  return 0;
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseWebBuildArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  return runWebBuild(planWebBuild(config), { appRoot, dryRun: opts.dryRun });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release web-build: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
