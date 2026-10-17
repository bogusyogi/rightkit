#!/usr/bin/env node
// `right-release recipe-build --plan <json>` (bogusyogi/rightkit#345): the single prePackage command that a declarative
// winPackage / macPackage target expands to. release.mjs runs one prePackage command; a real Tauri app needs several
// steps before and after `tauri build` (stage ffmpeg / pdfium / sidecars, then the build, then for macOS embed a dylib
// before release.mjs signs). This runs them in order from a validated plan, with argv arrays and no shell, and fails
// closed on the first step that does not exit 0. It never signs; release.mjs signs what it leaves behind.
//
// Plan: { version: 1, steps: [{ phase: "prepare" | "build" | "postBuild", cmd, args?, cwd?, env? }], cleanup?, stability? }
// `cwd` is relative to the app root (default the app root). Phases must appear in order prepare, build, postBuild, and
// exactly one build step is required.
//
// Built-in steps (no app script needed), all paths relative to the app root:
//   cleanup:   { remove?: [file or dir], apps?: { dir, remove?: bool, unregister?: bool }, dmgVolumes?: [name prefix] }
//              runs BEFORE the first step. `remove` deletes stale artifacts (a stale raw EXE or installer must never be
//              mistaken for this build's output). `apps` finds stale `*.app` bundles in `dir`, unregisters them with
//              `lsregister -u` and/or deletes them. `dmgVolumes` detaches /Volumes/<name> and /Volumes/<name> <N> (the macOS
//              duplicate suffix) only when `hdiutil info -plist` shows the volume is a mounted disk image; `-force` is used
//              only on retry. The macOS-only parts (apps unregister, dmgVolumes) are skipped off darwin.
//   stability: { files: [file], intervalMs?, checks?, timeoutMs? } runs right AFTER the build step: each file must exist
//              and keep the same size and mtime over `checks` consecutive polls `intervalMs` apart (defaults 2 and 1000 ms,
//              timeout 120 s), so a hung or still-writing Tauri process cannot hand a half-written EXE to signing.
import { existsSync as nodeExistsSync, readFileSync, readdirSync as nodeReaddirSync, rmSync as nodeRmSync, statSync as nodeStatSync } from "node:fs";
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
const PLAN_KEYS = new Set(["version", "steps", "cleanup", "stability"]);
const CLEANUP_KEYS = new Set(["remove", "apps", "dmgVolumes"]);
const APPS_KEYS = new Set(["dir", "remove", "unregister"]);
const STABILITY_KEYS = new Set(["files", "intervalMs", "checks", "timeoutMs"]);
const LSREGISTER = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
const unknownKeys = (value, allowed, label, errors) => { for (const key of Object.keys(value)) if (!allowed.has(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} (allowed: ${[...allowed].join(", ")})`); };
const positiveInt = (value) => Number.isInteger(value) && value > 0;
const badVolumePrefix = (item) => typeof item !== "string" || !item.trim() || item === "." || item === ".." || item.includes("/") || item.includes("\\") || item.includes(":") || item.includes("\0");

/** A relative path that stays inside the app root; returns the normalized form or undefined and pushes an error. */
function insideApp(value, label, errors, { strict = false } = {}) {
  if (typeof value !== "string" || !value.trim()) { errors.push(`${label} must be a non-empty relative path`); return undefined; }
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) { errors.push(`${label} must be relative and stay inside the app: ${value}`); return undefined; }
  const resolved = path.posix.normalize(normalized);
  if (strict && (resolved === "." || resolved === "./" || resolved === "")) { errors.push(`${label} must name a path strictly inside the app, not the app root: ${JSON.stringify(value)}`); return undefined; }
  if (resolved === "/" || resolved.startsWith("../") || resolved === "..") { errors.push(`${label} must name a path strictly inside the app, not the app root or an ancestor: ${JSON.stringify(value)}`); return undefined; }
  return normalized;
}

function validateCleanup(value, errors) {
  if (value === undefined) return undefined;
  if (!isObject(value)) { errors.push("cleanup must be an object"); return undefined; }
  unknownKeys(value, CLEANUP_KEYS, "cleanup", errors);
  const out = {};
  if (value.remove !== undefined) {
    if (!Array.isArray(value.remove)) errors.push("cleanup.remove must be a list of relative paths");
    else out.remove = value.remove.map((item, index) => insideApp(item, `cleanup.remove[${index}]`, errors, { strict: true })).filter(Boolean);
  }
  if (value.apps !== undefined) {
    if (!isObject(value.apps)) errors.push("cleanup.apps must be an object");
    else {
      unknownKeys(value.apps, APPS_KEYS, "cleanup.apps", errors);
      for (const key of ["remove", "unregister"]) if (value.apps[key] !== undefined && typeof value.apps[key] !== "boolean") errors.push(`cleanup.apps.${key} must be a boolean`);
      const dir = insideApp(value.apps.dir, "cleanup.apps.dir", errors, { strict: true });
      if (dir) out.apps = { dir, remove: value.apps.remove === true, unregister: value.apps.unregister === true };
    }
  }
  if (value.dmgVolumes !== undefined) {
    if (!Array.isArray(value.dmgVolumes) || value.dmgVolumes.some(badVolumePrefix)) errors.push("cleanup.dmgVolumes must be a list of non-empty volume name prefixes (no path separators)");
    else out.dmgVolumes = [...value.dmgVolumes];
  }
  return out;
}

function validateStability(value, errors) {
  if (value === undefined) return undefined;
  if (!isObject(value)) { errors.push("stability must be an object"); return undefined; }
  unknownKeys(value, STABILITY_KEYS, "stability", errors);
  for (const key of ["intervalMs", "checks", "timeoutMs"]) if (value[key] !== undefined && !positiveInt(value[key])) errors.push(`stability.${key} must be a positive integer`);
  if (!Array.isArray(value.files) || !value.files.length) { errors.push("stability.files must be a non-empty list of relative paths"); return undefined; }
  const files = value.files.map((item, index) => insideApp(item, `stability.files[${index}]`, errors, { strict: true })).filter(Boolean);
  return { files, ...(value.intervalMs === undefined ? {} : { intervalMs: value.intervalMs }), ...(value.checks === undefined ? {} : { checks: value.checks }), ...(value.timeoutMs === undefined ? {} : { timeoutMs: value.timeoutMs }) };
}

/** Validate a plan strictly; every problem is reported at once as RECIPE_BUILD_PLAN_INVALID. Returns the normalized plan. */
export function validateRecipePlan(plan) {
  const errors = [];
  if (!isObject(plan)) fail("RECIPE_BUILD_PLAN_INVALID", "recipe plan must be an object");
  for (const key of Object.keys(plan)) if (!PLAN_KEYS.has(key)) errors.push(`unknown key ${JSON.stringify(key)}`);
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
  const cleanup = validateCleanup(plan.cleanup, errors);
  const stability = validateStability(plan.stability, errors);
  if (errors.length) fail("RECIPE_BUILD_PLAN_INVALID", `invalid recipe plan:\n${errors.map((line) => `- ${line}`).join("\n")}`, { errors });
  return { version: RECIPE_BUILD_PLAN_VERSION, steps, ...(cleanup ? { cleanup } : {}), ...(stability ? { stability } : {}) };
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

const defaultSleep = (ms) => { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms); };
const defaultFs = { existsSync: nodeExistsSync, readdirSync: nodeReaddirSync, rmSync: nodeRmSync };

/**
 * Mount points of mounted disk images in `hdiutil info -plist` output: a mount-point counts only when it follows an
 * `image-path` key (so it belongs to an image). Anything unparsable yields an empty set, so nothing is detached.
 */
export function diskImageMountPoints(plistText) {
  const mounts = new Set();
  const segments = String(plistText).split("<key>image-path</key>").slice(1);
  for (const segment of segments) {
    for (const match of segment.matchAll(/<key>mount-point<\/key>\s*<string>([^<]*)<\/string>/g)) mounts.add(match[1].replaceAll("&amp;", "&"));
  }
  return mounts;
}

/**
 * Stale-artifact cleanup, run before the build. Returns what it did: { removed, unregistered, detached }.
 * A failing detach or unregister is logged and skipped: the build's own gates decide pass or fail.
 * Options: appRoot, platform, log, spawnSync, fs ({ existsSync, readdirSync, rmSync }), volumesDir.
 */
export function runRecipeCleanup(cleanup, { appRoot, platform = process.platform, log = () => {}, spawnSync = nodeSpawnSync, fs = defaultFs, volumesDir = "/Volumes" } = {}) {
  const result = { removed: [], unregistered: [], detached: [] };
  if (!cleanup) return result;
  const tool = (cmd, args) => {
    const run = spawnSync(cmd, args, { stdio: "inherit", windowsHide: true });
    const ok = !(run?.error || run?.status !== 0);
    if (!ok) log(`recipe-build: cleanup ${cmd} ${args.join(" ")} did not succeed (${run?.error?.message ?? `exit ${run?.status ?? "unknown"}`}); continuing`);
    return ok;
  };
  if (cleanup.dmgVolumes?.length && platform !== "darwin") log("recipe-build: cleanup dmgVolumes skipped (not macOS)");
  else {
    // Only a volume named exactly `<name>` or `<name> <N>` (the macOS duplicate suffix) that `hdiutil info -plist` reports as
    // a mounted disk image is detached; a real disk, a network share or a similarly named folder is never touched.
    const candidatesFor = (names) => {
      const wanted = cleanup.dmgVolumes ?? [];
      return names.filter((entry) => wanted.some((volName) => entry === volName || (entry.startsWith(`${volName} `) && /^[0-9]+$/.test(entry.slice(volName.length + 1))))).sort();
    };
    let names = [];
    try { names = fs.readdirSync(volumesDir); } catch { names = []; }
    const candidates = candidatesFor(names);
    if (candidates.length) {
      const info = spawnSync("hdiutil", ["info", "-plist"], { encoding: "utf8", windowsHide: true });
      const imageMounts = info?.error || info?.status !== 0 ? null : diskImageMountPoints(String(info.stdout ?? ""));
      if (!imageMounts) log("recipe-build: cleanup dmgVolumes skipped (hdiutil info -plist failed, so no volume is confirmed to be a disk image)");
      else {
        for (const name of candidates) {
          const volume = path.posix.join(volumesDir, name);
          if (!imageMounts.has(volume)) { log(`recipe-build: cleanup leaves ${volume} (not a mounted disk image)`); continue; }
          log(`recipe-build: cleanup detach ${volume}`);
          // No -force first; -force only on retry, and only for a volume confirmed to be an image mount.
          if (tool("hdiutil", ["detach", volume]) || tool("hdiutil", ["detach", "-force", volume])) result.detached.push(name);
        }
      }
    }
  }
  if (cleanup.apps) {
    const dir = path.resolve(appRoot, cleanup.apps.dir);
    let entries = [];
    try { entries = fs.readdirSync(dir).filter((entry) => entry.toLowerCase().endsWith(".app")).sort(); } catch { entries = []; }
    for (const entry of entries) {
      const stale = path.join(dir, entry);
      const root = path.resolve(appRoot);
      const rel = path.relative(stale, root);
      if (rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel)) || (dir === root && entry.toLowerCase() === "notarize.app")) {
        fail("RECIPE_BUILD_CLEANUP_UNSAFE", `cleanup refuses ${stale}: it is, or contains, the app root ${root}`);
      }
      if (cleanup.apps.unregister) {
        if (platform !== "darwin") log("recipe-build: cleanup lsregister skipped (not macOS)");
        else if (tool(LSREGISTER, ["-u", stale])) result.unregistered.push(stale);
      }
      if (cleanup.apps.remove) {
        log(`recipe-build: cleanup remove stale app ${stale}`);
        fs.rmSync(stale, { recursive: true, force: true });
        result.removed.push(stale);
      }
    }
  }
  for (const relative of cleanup.remove ?? []) {
    const stale = path.resolve(appRoot, relative);
    if (!fs.existsSync(stale)) continue;
    log(`recipe-build: cleanup remove stale ${stale}`);
    fs.rmSync(stale, { recursive: true, force: true });
    result.removed.push(stale);
  }
  return result;
}

/**
 * Wait until every file exists and keeps the same size and mtime over `checks` consecutive polls `intervalMs` apart.
 * Throws RECIPE_BUILD_ARTIFACT_UNSTABLE when that does not happen within `timeoutMs`. Returns the absolute files.
 * Options: appRoot, log, statSync, sleep(ms), now().
 */
export function waitForStableArtifacts(stability, { appRoot, log = () => {}, statSync = nodeStatSync, sleep = defaultSleep, now = Date.now } = {}) {
  if (!stability) return [];
  const { intervalMs = 1000, checks = 2, timeoutMs = 120000 } = stability;
  const signature = (file) => { try { const stat = statSync(file); return `${stat.size}:${stat.mtimeMs}`; } catch { return null; } };
  const files = stability.files.map((relative) => path.resolve(appRoot, relative));
  const deadline = now() + timeoutMs;
  let previous = files.map(signature);
  let stable = 0;
  while (stable < checks) {
    if (now() >= deadline) fail("RECIPE_BUILD_ARTIFACT_UNSTABLE", `artifact missing or still changing after ${timeoutMs} ms: ${files.join(", ")}`, { files });
    sleep(intervalMs);
    const current = files.map(signature);
    stable = current.every((value, index) => value !== null && value === previous[index]) ? stable + 1 : 0;
    previous = current;
  }
  log(`recipe-build: artifacts stable (${files.length} file(s), ${checks} check(s) ${intervalMs} ms apart)`);
  return files;
}

/**
 * Run the plan: cleanup, prepare steps, the build step, stability polling, then postBuild steps, strictly in order.
 * Returns { steps: [{ phase, cmd, status }], cleaned, stable }. The first failure throws RECIPE_BUILD_STEP_FAILED and
 * nothing after it runs.
 * Options: appRoot (absolute), env, platform, log, spawnSync, fs, sleep, now, statSync (injected in tests).
 */
export function runRecipeBuild(rawPlan, { appRoot, env = process.env, platform = process.platform, log = () => {}, spawnSync = nodeSpawnSync, fs, sleep, now, statSync } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("RECIPE_BUILD_PLAN_INVALID", "appRoot must be an absolute path");
  const plan = validateRecipePlan(rawPlan);
  const done = [];
  const cleaned = runRecipeCleanup(plan.cleanup, { appRoot, platform, log, spawnSync, ...(fs ? { fs } : {}) });
  let stable = [];
  for (const [index, step] of plan.steps.entries()) {
    const launch = resolveLaunch({ cmd: step.cmd, args: step.args, platform, env });
    const cwd = path.resolve(appRoot, step.cwd ?? ".");
    log(`recipe-build: [${step.phase}] ${step.cmd} ${step.args.join(" ")}`.trimEnd());
    const result = spawnSync(launch.command, launch.args, { cwd, env: { ...env, ...(step.env ?? {}) }, stdio: "inherit", shell: launch.shell, windowsHide: true });
    if (result?.error || result?.status !== 0) {
      fail("RECIPE_BUILD_STEP_FAILED", `recipe-build ${step.phase} step ${index} (${step.cmd}) failed (${result?.error?.message ?? `exit ${result?.status ?? result?.signal ?? "unknown"}`}); later steps were not run`, { phase: step.phase, index, cmd: step.cmd, status: result?.status ?? null });
    }
    done.push({ phase: step.phase, cmd: step.cmd, status: 0 });
    if (step.phase === "build") stable = waitForStableArtifacts(plan.stability, { appRoot, log, ...(sleep ? { sleep } : {}), ...(now ? { now } : {}), ...(statSync ? { statSync } : {}) });
  }
  return { steps: done, cleaned, stable };
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

/**
 * The commands a build command really runs. A declarative winPackage / macPackage wraps the Tauri build as
 * `node recipe-build.mjs --plan <JSON>` (or `--plan-file <path>`), which hides each step's `--features` inside one JSON
 * argument. The forbidden-feature scan and the cargo-tree closure must see every step, so this returns
 * `[command, ...every prepare / build / postBuild step as { cmd, args, env? }]` (nested wrappers are expanded too), and
 * `[command]` for any other command. A wrapper whose plan cannot be read or validated throws (fail closed).
 * Options: root (the app root, for --plan-file and --app-root), readFile (injected in tests).
 */
export function expandRecipeBuildCommands(command, { root = process.cwd(), readFile = readFileSync, depth = 0 } = {}) {
  if (command === undefined || command === null) return [];
  const parts = typeof command === "string"
    ? (command.match(/"[^"]*"|'[^']*'|[^\s"']+/g) ?? []).map((token) => token.replace(/^["']|["']$/g, ""))
    : [command?.cmd, ...(command?.args ?? [])].filter((value) => value !== undefined).map(String);
  const at = parts.findIndex((part) => path.basename(part.replaceAll("\\", "/")) === "recipe-build.mjs" || part === "recipe-build");
  if (at < 0) return [command];
  const rest = parts.slice(at + 1);
  if (!rest.some((arg) => arg === "--plan" || arg === "--plan-file")) return [command];
  if (depth >= 3) fail("RECIPE_BUILD_PLAN_INVALID", "recipe-build plans are nested too deeply to scan");
  let plan;
  try {
    const opts = parseRecipeBuildArgs(rest);
    const text = opts.plan ?? readFile(path.resolve(path.resolve(root, opts.appRoot ?? "."), opts.planFile), "utf8");
    plan = validateRecipePlan(JSON.parse(text));
  } catch (error) {
    if (error instanceof RecipeBuildError) throw error;
    fail("RECIPE_BUILD_PLAN_INVALID", `cannot scan the recipe-build command (${error.message}); every step must be visible to the forbidden-feature scan`);
  }
  const steps = plan.steps.map(({ cmd, args, env }) => ({ cmd, args, ...(env ? { env } : {}) }));
  return [command, ...steps.flatMap((step) => expandRecipeBuildCommands(step, { root, readFile, depth: depth + 1 }))];
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
