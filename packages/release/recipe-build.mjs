#!/usr/bin/env node
// `right-release recipe-build --plan <json>` (bogusyogi/rightkit#345): the single prePackage command that a declarative
// winPackage / macPackage target expands to. release.mjs runs one prePackage command; a real Tauri app needs several
// steps before and after `tauri build` (stage ffmpeg / pdfium / sidecars, then the build, then for macOS embed a dylib
// before release.mjs signs). This runs them in order from a validated plan, with argv arrays and no shell, and fails
// closed on the first step that does not exit 0. It never signs; release.mjs signs what it leaves behind.
//
// Plan: { version: 1, steps: [{ phase: "prepare" | "build" | "postBuild", cmd, args?, cwd?, env? }] }
// `cwd` is relative to the app root (default the app root). Phases must appear in order prepare, build, postBuild, and
// exactly one build step is required.
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";

export const RECIPE_BUILD_PLAN_VERSION = 1;
export const RECIPE_PHASES = Object.freeze(["prepare", "build", "postBuild"]);

export class RecipeBuildError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "RecipeBuildError";
    this.code = code;
    this.details = details;
  }
}

const fail = (code, message, details) => { throw new RecipeBuildError(code, message, details); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const STEP_KEYS = new Set(["phase", "cmd", "args", "cwd", "env"]);

/** A relative path that stays inside the app root; returns the normalized form or undefined and pushes an error. */
function insideApp(value, label, errors) {
  if (typeof value !== "string" || !value.trim()) { errors.push(`${label} must be a non-empty relative path`); return undefined; }
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) { errors.push(`${label} must be relative and stay inside the app: ${value}`); return undefined; }
  return normalized;
}

/** Validate a plan strictly; every problem is reported at once as RECIPE_BUILD_PLAN_INVALID. Returns the normalized plan. */
export function validateRecipePlan(plan) {
  const errors = [];
  if (!isObject(plan)) fail("RECIPE_BUILD_PLAN_INVALID", "recipe plan must be an object");
  for (const key of Object.keys(plan)) if (!["version", "steps"].includes(key)) errors.push(`unknown key ${JSON.stringify(key)}`);
  if (plan.version !== RECIPE_BUILD_PLAN_VERSION) errors.push(`version must be ${RECIPE_BUILD_PLAN_VERSION}`);
  if (!Array.isArray(plan.steps) || !plan.steps.length) errors.push("steps must be a non-empty array");
  const steps = [];
  let lastPhase = -1;
  let builds = 0;
  for (const [index, step] of (Array.isArray(plan.steps) ? plan.steps : []).entries()) {
    const label = `steps[${index}]`;
    if (!isObject(step)) { errors.push(`${label} must be an object`); continue; }
    for (const key of Object.keys(step)) if (!STEP_KEYS.has(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} (allowed: ${[...STEP_KEYS].join(", ")})`);
    const phase = RECIPE_PHASES.indexOf(step.phase);
    if (phase < 0) errors.push(`${label}.phase must be one of ${RECIPE_PHASES.join(", ")}`);
    else if (phase < lastPhase) errors.push(`${label}: phase ${step.phase} comes after a later phase (order: ${RECIPE_PHASES.join(", ")})`);
    else lastPhase = phase;
    if (step.phase === "build") builds += 1;
    if (typeof step.cmd !== "string" || !step.cmd.trim()) errors.push(`${label}.cmd must be a non-empty string`);
    const args = step.args ?? [];
    if (!Array.isArray(args) || args.some((arg) => typeof arg !== "string")) errors.push(`${label}.args must be a list of strings`);
    const cwd = step.cwd === undefined ? undefined : insideApp(step.cwd, `${label}.cwd`, errors);
    if (step.env !== undefined && (!isObject(step.env) || Object.values(step.env).some((value) => typeof value !== "string"))) errors.push(`${label}.env must map names to strings`);
    steps.push({ phase: step.phase, cmd: step.cmd, args: Array.isArray(args) ? [...args] : [], ...(cwd ? { cwd } : {}), ...(isObject(step.env) ? { env: { ...step.env } } : {}) });
  }
  if (builds !== 1) errors.push(`exactly one build step is required, found ${builds}`);
  if (errors.length) fail("RECIPE_BUILD_PLAN_INVALID", `invalid recipe plan:\n${errors.map((line) => `- ${line}`).join("\n")}`, { errors });
  return { version: RECIPE_BUILD_PLAN_VERSION, steps };
}

// cmd.exe would reinterpret these in a shell launch; refuse rather than escape.
const SHELL_UNSAFE = /[&|<>^%"!\r\n]/;

/**
 * How to launch one step without a shell. `pnpm` runs through the pinned pnpm CLI (npm_execpath, set whenever the
 * release runs under pnpm). On Windows a non-.exe command is a .cmd shim that only a shell can start, so it is allowed
 * only when no argument could be reinterpreted by that shell.
 */
export function resolveLaunch({ cmd, args = [], platform = process.platform, env = process.env, execPath = process.execPath }) {
  if (cmd === "pnpm" && env.npm_execpath) return { command: execPath, args: [env.npm_execpath, ...args], shell: false };
  if (platform === "win32" && !/\.(?:exe|com)$/i.test(cmd)) {
    if ([cmd, ...args].some((value) => SHELL_UNSAFE.test(value))) fail("RECIPE_BUILD_LAUNCH_UNSAFE", `${cmd} needs a shell on Windows and its arguments contain characters a shell would reinterpret; run the release under pnpm (npm_execpath) or name an .exe`, { cmd });
    return { command: cmd, args, shell: true };
  }
  return { command: cmd, args, shell: false };
}

/**
 * Run the plan: prepare steps, the build step, then postBuild steps, strictly in order. Returns { steps: [{ phase, cmd,
 * status }] }. The first failure throws RECIPE_BUILD_STEP_FAILED and nothing after it runs.
 * Options: appRoot (absolute), env, platform, log, spawnSync (injected in tests).
 */
export function runRecipeBuild(rawPlan, { appRoot, env = process.env, platform = process.platform, log = () => {}, spawnSync = nodeSpawnSync } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("RECIPE_BUILD_PLAN_INVALID", "appRoot must be an absolute path");
  const plan = validateRecipePlan(rawPlan);
  const done = [];
  for (const [index, step] of plan.steps.entries()) {
    const launch = resolveLaunch({ cmd: step.cmd, args: step.args, platform, env });
    const cwd = path.resolve(appRoot, step.cwd ?? ".");
    log(`recipe-build: [${step.phase}] ${step.cmd} ${step.args.join(" ")}`.trimEnd());
    const result = spawnSync(launch.command, launch.args, { cwd, env: { ...env, ...(step.env ?? {}) }, stdio: "inherit", shell: launch.shell, windowsHide: true });
    if (result?.error || result?.status !== 0) {
      fail("RECIPE_BUILD_STEP_FAILED", `recipe-build ${step.phase} step ${index} (${step.cmd}) failed (${result?.error?.message ?? `exit ${result?.status ?? result?.signal ?? "unknown"}`}); later steps were not run`, { phase: step.phase, index, cmd: step.cmd, status: result?.status ?? null });
    }
    done.push({ phase: step.phase, cmd: step.cmd, status: 0 });
  }
  return { steps: done };
}

export function parseRecipeBuildArgs(argv) {
  const opts = { plan: undefined, planFile: undefined, appRoot: undefined };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--plan" || arg === "--plan-file" || arg === "--app-root") {
      const value = argv[i + 1];
      if (value === undefined || (arg !== "--plan" && value.startsWith("--"))) throw new Error(`${arg} needs a value`);
      opts[arg === "--plan" ? "plan" : arg === "--plan-file" ? "planFile" : "appRoot"] = value;
      i += 1;
    } else throw new Error(`unknown recipe-build argument: ${arg}`);
  }
  if ((opts.plan === undefined) === (opts.planFile === undefined)) throw new Error("recipe-build needs exactly one of --plan <json> or --plan-file <path>");
  return opts;
}

async function main() {
  const opts = parseRecipeBuildArgs(process.argv.slice(2));
  const appRoot = path.resolve(opts.appRoot ?? process.cwd());
  let text = opts.plan;
  if (text === undefined) {
    const { readFileSync } = await import("node:fs");
    text = readFileSync(path.resolve(appRoot, opts.planFile), "utf8");
  }
  let plan;
  try { plan = JSON.parse(text); } catch (error) { throw new RecipeBuildError("RECIPE_BUILD_PLAN_INVALID", `the plan is not JSON: ${error.message}`); }
  runRecipeBuild(plan, { appRoot, log: (line) => console.log(line) });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch((error) => { console.error(`right-release recipe-build: ${error.code ? `[${error.code}] ` : ""}${error.message}`); process.exitCode = 1; });
}
