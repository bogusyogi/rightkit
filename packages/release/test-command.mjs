#!/usr/bin/env node
// `right-release test [--config <file>] [--suite <name>[,<name>]]... [--runner <name>] [--list] [--json] [-- <args>]`
// runs the app's declared test suites from its release config (`right-release.config.json`), one after another, and stops at the first failure.
//
//   tests: {
//     eventPrefix: "e2e",                       // optional; events are `<prefix>_suite_started` / `<prefix>_suite_completed`
//     runners: {                                // optional named bases a suite or a `--runner` call builds on
//       vitest: { node: "node_modules/vitest/vitest.mjs", nodeFlags: ["--no-experimental-webstorage"], nodeOptions: ["--no-experimental-webstorage"] },
//     },
//     suites: [
//       { name: "unit", runner: "vitest", args: ["run", "--project", "unit"] },
//       { name: "runtime-config", node: "scripts/test-runtime-config-e2e.test.mjs", nodeFlags: ["--test"] },
//       // Expands to journeys-1, journeys-2, ...: the listed journeys run `chunk` at a time in separate processes.
//       { name: "journeys", node: "scripts/qa-browser-functional.mjs", args: ["--keep-going"],
//         journeys: { file: "scripts/qa-e2e-journeys.json", key: "journeys", chunk: 12, flag: "--journey" } },
//     ],
//   }
//
// A step is `{ cmd | node, args?, cwd?, env?, nodeFlags?, nodeOptions? }` (see command-spec.mjs); a suite adds `name`,
// optionally `runner` (its arguments and flags are appended to the runner's) and `journeys`. Everything runs without a shell
// from the app root. `--suite journeys` selects every chunk of that group. `--runner vitest -- run --project unit` runs
// the runner once with the arguments after `--` (the watch or one-off case); `-- <args>` after `--suite` is allowed when
// exactly one suite is selected. Exit code: 0 when every selected suite passed, else the failing suite's exit code.
import { existsSync, readFileSync } from "node:fs";
import { buildLaunch } from "./build-launch.mjs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawn } from "./background-process.mjs";
import { CommandSpecError, loadAppConfig, resolveInvocation, validateStep } from "./command-spec.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";

const fail = (message) => { throw new CommandSpecError(message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const PREFIX = /^[a-z][a-z0-9_]*$/;
const uniq = (...lists) => [...new Set(lists.flatMap((list) => list ?? []))];

export function parseTestArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), suites: [], runner: undefined, list: false, json: false, passthrough: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--") { opts.passthrough = argv.slice(i + 1); break; }
    if (arg === "--config" || arg === "--suite" || arg === "--runner" || arg === "--platform") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      if (arg === "--suite") opts.suites.push(...value.split(",").filter(Boolean));
      else if (arg === "--platform") opts.platform = value;
      else opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--list") opts.list = true;
    else if (arg === "--json") opts.json = true;
    else throw new Error(`unknown test argument: ${arg}`);
  }
  return opts;
}

function mergeRunner(runner, entry) {
  const { name: _name, runner: _runner, journeys: _journeys, ...own } = entry;
  return {
    ...runner,
    ...own,
    args: [...(runner?.args ?? []), ...(own.args ?? [])],
    nodeFlags: uniq(runner?.nodeFlags, own.nodeFlags),
    nodeOptions: uniq(runner?.nodeOptions, own.nodeOptions),
    env: { ...(runner?.env ?? {}), ...(own.env ?? {}) },
    // A suite that names its own executable replaces the runner's, whichever form each uses.
    ...(runner?.node !== undefined && own.cmd !== undefined ? { node: undefined } : {}),
    ...(runner?.cmd !== undefined && own.node !== undefined ? { cmd: undefined } : {}),
  };
}

function expandJourneys(spec, label, appRoot) {
  if (!isObject(spec)) fail(`${label}.journeys must be an object`);
  const unknown = Object.keys(spec).filter((key) => !["file", "key", "chunk", "flag"].includes(key));
  if (unknown.length) fail(`${label}.journeys has unknown key(s): ${unknown.join(", ")} (allowed: file, key, chunk, flag)`);
  if (typeof spec.file !== "string" || !spec.file.trim()) fail(`${label}.journeys.file must be a relative JSON path`);
  if (!Number.isInteger(spec.chunk) || spec.chunk < 1) fail(`${label}.journeys.chunk must be a positive integer`);
  if (typeof spec.flag !== "string" || !spec.flag.startsWith("--")) fail(`${label}.journeys.flag must be a flag such as --journey`);
  let data;
  try { data = JSON.parse(readFileSync(path.resolve(appRoot, spec.file), "utf8")); }
  catch (error) { fail(`${label}.journeys.file cannot be read: ${error.message}`); }
  const list = (spec.key ?? "journeys") === "" ? data : data?.[spec.key ?? "journeys"];
  if (!Array.isArray(list) || list.some((item) => typeof item !== "string" || item.includes(","))) fail(`${label}.journeys: ${spec.file} must hold a list of names without commas under "${spec.key ?? "journeys"}"`);
  const chunks = [];
  for (let index = 0; index < list.length; index += spec.chunk) chunks.push(list.slice(index, index + spec.chunk));
  return chunks.map((names) => [spec.flag, names.join(",")]);
}

/**
 * The ordered suites a run would execute: `[{ name, group, step, extraArgs }]`. `step` is merged with its runner and
 * validated; journey chunks are already expanded. Pure apart from reading a declared journeys file.
 */
export function planTests(config, { suites = [], runner, passthrough = [], appRoot }) {
  const tests = config?.tests;
  if (!isObject(tests)) fail("the config declares no tests");
  const unknown = Object.keys(tests).filter((key) => !["suites", "runners", "eventPrefix"].includes(key));
  if (unknown.length) fail(`tests has unknown key(s): ${unknown.join(", ")} (allowed: suites, runners, eventPrefix)`);
  const runners = tests.runners ?? {};
  if (!isObject(runners)) fail("tests.runners must be an object");
  for (const [name, spec] of Object.entries(runners)) {
    if (!NAME.test(name)) fail(`tests.runners.${name}: names use letters, digits, . _ -`);
    validateStep(spec, `tests.runners.${name}`);
  }
  if (runner !== undefined) {
    if (!runners[runner]) fail(`unknown runner: ${runner} (declared: ${Object.keys(runners).join(", ") || "none"})`);
    if (suites.length) fail("use --runner or --suite, not both");
    const step = mergeRunner(runners[runner], {});
    return [{ name: runner, group: runner, step, extraArgs: [...passthrough] }];
  }
  if (!Array.isArray(tests.suites) || tests.suites.length === 0) fail("tests.suites must be a non-empty list");
  const planned = [];
  const seen = new Set();
  tests.suites.forEach((entry, index) => {
    const label = `tests.suites[${index}]`;
    validateStep(entry, label, ["name", "runner", "journeys"]);
    if (typeof entry.name !== "string" || !NAME.test(entry.name)) fail(`${label}.name must be set (letters, digits, . _ -)`);
    if (entry.runner !== undefined && !runners[entry.runner]) fail(`${label}.runner names an undeclared runner: ${entry.runner}`);
    const base = mergeRunner(entry.runner === undefined ? undefined : runners[entry.runner], entry);
    if (base.cmd === undefined && base.node === undefined) fail(`${label} needs cmd, node or a runner that has one`);
    const variants = entry.journeys === undefined
      ? [{ name: entry.name, extra: [] }]
      : expandJourneys(entry.journeys, label, appRoot).map((extra, chunk) => ({ name: `${entry.name}-${chunk + 1}`, extra }));
    for (const variant of variants) {
      if (seen.has(variant.name)) fail(`${label}: duplicate suite name ${variant.name}`);
      seen.add(variant.name);
      planned.push({ name: variant.name, group: entry.name, step: { ...base, args: [...base.args, ...variant.extra] }, extraArgs: [] });
    }
  });
  let selected = planned;
  if (suites.length) {
    selected = planned.filter((item) => suites.includes(item.name) || suites.includes(item.group));
    const missing = suites.filter((name) => !planned.some((item) => item.name === name || item.group === name));
    if (missing.length) fail(`unknown suite: ${missing.join(", ")} (declared: ${[...new Set(planned.map((item) => item.group))].join(", ")})`);
  }
  if (passthrough.length) {
    if (selected.length !== 1) fail("arguments after -- need exactly one selected suite (or use --runner)");
    selected = [{ ...selected[0], extraArgs: [...passthrough] }];
  }
  return selected;
}

export function eventPrefix(config) {
  const prefix = config?.tests?.eventPrefix ?? "test";
  if (typeof prefix !== "string" || !PREFIX.test(prefix)) fail("tests.eventPrefix must be lower-case letters, digits and underscores");
  return prefix;
}

/**
 * Run the planned suites in order; stop at the first non-zero exit. `spawnFn` and `log` are injectable.
 * Resolves `{ exitCode, results: [{ suite, exitCode, durationMs }] }`; a spawn error or a signal rejects.
 */
export async function runSuites(plan, { appRoot, prefix = "test", env = process.env, spawnFn = spawn, log = console.log, door = buildLaunch } = {}) {
  const results = [];
  for (const item of plan) {
    const invocation = resolveInvocation(item.step, { appRoot, baseEnv: env, extraArgs: item.extraArgs });
    log(JSON.stringify({ event: `${prefix}_suite_started`, suite: item.name }));
    const started = Date.now();
    const exitCode = await new Promise((resolve, reject) => {
      // A test suite goes through the one door (build-launch.mjs): a broker job on a managed host, which builds it under the app's claim.
      const target = door(invocation.cmd, invocation.args, { env: invocation.env, cwd: invocation.cwd, exists: existsSync });
      const child = spawnFn(target.command, target.args, { cwd: invocation.cwd, env: invocation.env, stdio: "inherit" });
      child.once("error", reject);
      child.once("exit", (code, signal) => {
        if (signal) reject(new Error(`${item.name} stopped by ${signal}`));
        else resolve(code ?? 1);
      });
    });
    const durationMs = Date.now() - started;
    log(JSON.stringify({ event: `${prefix}_suite_completed`, suite: item.name, exitCode, durationMs }));
    results.push({ suite: item.name, exitCode, durationMs });
    if (exitCode !== 0) return { exitCode, results };
  }
  return { exitCode: 0, results };
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseTestArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  // `--platform ios`: the app's declared XCUITest journeys (development.targets.ios.uiTests) on its dev device or simulator.
  if (opts.platform !== undefined) {
    if (opts.platform !== "ios") throw new Error(`--platform ${opts.platform}: only ios has platform UI test journeys; other suites are declared in tests.suites`);
    if (opts.suites.length || opts.runner || opts.passthrough.length) throw new Error("--platform ios runs the declared journeys; it takes no --suite, --runner or arguments");
    const { runDevelopment } = await import("./development.mjs");
    const result = runDevelopment({ root: appRoot, platform: "ios", config, uiTests: true, dryRun: opts.list });
    if (opts.list) console.log(JSON.stringify(result.plan ?? result, null, 2));
    return 0;
  }
  const plan = planTests(config, { suites: opts.suites, runner: opts.runner, passthrough: opts.passthrough, appRoot });
  if (opts.list) {
    const view = plan.map((item) => ({ suite: item.name, ...resolveInvocation(item.step, { appRoot, baseEnv: {}, extraArgs: item.extraArgs }), env: undefined }));
    if (opts.json) console.log(JSON.stringify(view, null, 2));
    else for (const item of view) console.log(`${item.suite}: ${[item.cmd, ...item.args].join(" ")}`);
    return 0;
  }
  const { exitCode } = await runSuites(plan, { appRoot, prefix: eventPrefix(config) });
  return exitCode;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release test: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
