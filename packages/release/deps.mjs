#!/usr/bin/env node
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { existsSync } from "node:fs";

const args = process.argv.slice(2);
const opts = {
  config: "right-release.config.mjs",
  mode: "check",
  latest: false,
};

for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === "--") continue;
  else if (arg === "--config") opts.config = args[++i];
  else if (arg === "--check") opts.mode = "check";
  else if (arg === "--audit") opts.mode = "audit";
  else if (arg === "--update") opts.mode = "update";
  else if (arg === "--latest") opts.latest = true;
  else if (arg === "-h" || arg === "--help") usage(0);
  else usage(2, `unknown argument: ${arg}`);
}

const configPath = path.resolve(opts.config);
const config = (await import(pathToFileURL(configPath))).default;
if (config?.schema !== 1) fail(`unsupported config schema: ${config?.schema ?? "<missing>"} (expected 1)`);
if (config.packageManager !== "pnpm") fail(`deps tool expects pnpm, got ${config.packageManager}`);

const root = path.dirname(configPath);
const workdirs = [...new Set([config.workdir ?? ".", ...(config.deps?.workdirs ?? [])])];
const resolvedWorkdirs = workdirs.map((rel) => ({ rel, cwd: path.resolve(root, rel) }));
const missingLocks = resolvedWorkdirs.filter(({ cwd }) => !existsSync(path.join(cwd, "pnpm-lock.yaml")));
if (missingLocks.length > 0) {
  for (const { rel, cwd } of missingLocks) {
    console.error(`right-deps: configured workdir ${rel} is missing ${path.join(cwd, "pnpm-lock.yaml")}`);
  }
  fail("dependency check refused: every configured workdir must have a pnpm-lock.yaml");
}

console.log(`right-deps: ${config.app} ${opts.mode}`);
const failures = [];
for (const { rel, cwd } of resolvedWorkdirs) {
  const workspaceArgs = cwd === root ? [] : ["--ignore-workspace"];
  if (opts.mode === "check") {
    const status = run("pnpm", ["outdated", ...workspaceArgs], cwd, { allowFailure: true });
    if (status == null) failures.push(`${rel}: pnpm outdated could not start`);
  } else if (opts.mode === "audit") {
    const status = run("pnpm", ["audit", ...workspaceArgs], cwd, { allowFailure: true });
    if (status !== 0) failures.push(`${rel}: pnpm audit exited ${status ?? "without status"}`);
  }
  else if (opts.mode === "update") {
    run("pnpm", ["update", ...workspaceArgs, ...(opts.latest ? ["--latest"] : [])], cwd, { ignoreScripts: true });
    run("pnpm", ["install", ...workspaceArgs, "--lockfile-only", "--ignore-scripts"], cwd, { ignoreScripts: true });
  }
}
if (failures.length > 0) {
  for (const failure of failures) console.error(`right-deps: ${failure}`);
  fail(`${failures.length} dependency workdir(s) failed`);
}

function usage(code, message) {
  if (message) console.error(`right-deps: ${message}\n`);
  console.error("usage: node deps.mjs [--config right-release.config.mjs] [--check|--audit|--update] [--latest]");
  process.exit(code);
}

function fail(message) {
  console.error(`right-deps: ${message}`);
  process.exit(1);
}

function run(cmd, runArgs, cwd, options = {}) {
  console.log(`\n> ${cmd} ${runArgs.join(" ")}  (${cwd})`);
  const env = options.ignoreScripts
    ? { ...process.env, npm_config_ignore_scripts: "true", PNPM_CONFIG_IGNORE_SCRIPTS: "true" }
    : process.env;
  const executable = process.platform === "win32" ? (process.env.ComSpec ?? "cmd.exe") : cmd;
  const executableArgs = process.platform === "win32"
    ? ["/d", "/s", "/c", [cmd, ...runArgs].join(" ")]
    : runArgs;
  const result = spawnSync(executable, executableArgs, { cwd, env, stdio: "inherit" });
  if (result.status !== 0 && !options.allowFailure) process.exit(result.status ?? 1);
  if (result.status !== 0 && options.allowFailure) {
    console.log(`right-deps: ${cmd} exited ${result.status}; continuing so all workdirs report`);
  }
  return result.status;
}
