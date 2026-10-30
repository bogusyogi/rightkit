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
//   stage:     { pre?: [{ from: app-relative, to: absolute }], post?: [{ from: absolute, to: app-relative, fresh?: bool }] }
//              the broker-managed target directory shape (bogusyogi/rightkit#924, managed-target-stage.mjs): `pre` copies
//              app files into the real target directory right before the build step (the signed raw EXE, which tauri bundle
//              reads from there); `post` waits for each real output to be stable (the stability settings, or their defaults), then copies it (a missing output skips the wait and fails the copy) to its app-relative location right after the build, and with
//              `fresh` the absolute source is deleted before the build step so a missing output cannot be a leftover. Every
//              copy deletes a stale destination first, is atomic, and is verified by sha256; a missing source fails the step.
//              `pre` never creates the broker-side parent directory. `verify: [{ app, real }]` requires the real file to equal the
//              signed app file before and after the build step; `resolve: { manifest, expected }` re-runs cargo metadata and
//              requires the planned target directory before and after it (a temporary bridge until the broker exposes artifacts).
import { existsSync as nodeExistsSync, readFileSync, readdirSync as nodeReaddirSync, rmSync as nodeRmSync, statSync as nodeStatSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";
import { buildLaunch } from "./build-launch.mjs";
import { resolveTargetRoot as cargoResolveTargetRoot } from "./cargo-target.mjs";
import { buildAndEmbedNativeHelpers, validateNativeHelpers } from "./native-helpers.mjs";
import { lockedTauriArgs } from "./tauri-locked.mjs";
import { assertTargetDirUnchanged, removeFreshSources, stageArtifacts, verifySameContent } from "./managed-target-stage.mjs";

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
const PLAN_KEYS = new Set(["version", "steps", "cleanup", "stability", "stage", "verifySidecars", "nativeHelpers", "boundTargets"]);
const VERIFY_SIDECARS_KEYS = new Set(["receipt", "target"]);
const NATIVE_HELPERS_KEYS = new Set(["app", "helpers", "version", "forceArch", "configuration"]);
const UNIVERSAL_TRIPLE = "universal-apple-darwin";
const STAGE_KEYS = new Set(["pre", "post", "verify", "resolve"]);
const STAGE_ENTRY_KEYS = new Set(["from", "to", "fresh"]);
const CLEANUP_KEYS = new Set(["remove", "apps", "dmgVolumes"]);
const APPS_KEYS = new Set(["dir", "remove", "unregister"]);
const STABILITY_KEYS = new Set(["files", "intervalMs", "checks", "timeoutMs"]);
const LSREGISTER = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
const unknownKeys = (value, allowed, label, errors) => { for (const key of Object.keys(value)) if (!allowed.has(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} (allowed: ${[...allowed].join(", ")})`); };
const positiveInt = (value) => Number.isInteger(value) && value > 0;
const badVolumePrefix = (item) => typeof item !== "string" || !item.trim() || item === "." || item === ".." || item.includes("/") || item.includes("\\") || item.includes(":") || item.includes("\0");

/**
 * A relative path that stays inside the app root; returns the normalized form or undefined and pushes an error.
 * `bound` (a door split, row 7.04c R-T2): the absolute Cargo target directories the plan binds. A path strictly inside one of them is also
 * accepted, absolute and without `..`: the snapshot has no target directory, so a split's build output is addressed there. Any other
 * absolute path stays refused.
 */
function insideApp(value, label, errors, { strict = false, bound = [] } = {}) {
  if (typeof value !== "string" || !value.trim()) { errors.push(`${label} must be a non-empty relative path`); return undefined; }
  const normalized = value.replaceAll("\\", "/");
  if (bound.length && (path.isAbsolute(value) || /^[A-Za-z]:[\\/]/.test(value))) {
    const parts = normalized.split("/");
    const absolute = path.resolve(value);
    const under = !parts.includes("..") && bound.some((dir) => { const relative = path.relative(dir, absolute); return relative !== "" && relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative); });
    if (!under) { errors.push(`${label} is an absolute path outside every bound Cargo target (${bound.join(", ")}): ${value}`); return undefined; }
    return normalized;
  }
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) { errors.push(`${label} must be relative and stay inside the app: ${value}`); return undefined; }
  const resolved = path.posix.normalize(normalized);
  if (strict && (resolved === "." || resolved === "./" || resolved === "")) { errors.push(`${label} must name a path strictly inside the app, not the app root: ${JSON.stringify(value)}`); return undefined; }
  if (resolved === "/" || resolved.startsWith("../") || resolved === "..") { errors.push(`${label} must name a path strictly inside the app, not the app root or an ancestor: ${JSON.stringify(value)}`); return undefined; }
  return normalized;
}

/** `boundTargets`: the absolute Cargo target directories of a door split (row 7.04c). Absent outside a split. */
function validateBoundTargets(value, errors) {
  if (value === undefined) return undefined;
  if (!Array.isArray(value) || !value.length || value.some((dir) => typeof dir !== "string" || !path.isAbsolute(dir) || dir.replaceAll("\\", "/").split("/").includes(".."))) {
    errors.push("boundTargets must be a non-empty list of absolute directories without ..");
    return undefined;
  }
  return value.map((dir) => path.resolve(dir));
}

function validateCleanup(value, errors, bound = []) {
  if (value === undefined) return undefined;
  if (!isObject(value)) { errors.push("cleanup must be an object"); return undefined; }
  unknownKeys(value, CLEANUP_KEYS, "cleanup", errors);
  const out = {};
  if (value.remove !== undefined) {
    if (!Array.isArray(value.remove)) errors.push("cleanup.remove must be a list of relative paths");
    else out.remove = value.remove.map((item, index) => insideApp(item, `cleanup.remove[${index}]`, errors, { strict: true, bound })).filter(Boolean);
  }
  if (value.apps !== undefined) {
    if (!isObject(value.apps)) errors.push("cleanup.apps must be an object");
    else {
      unknownKeys(value.apps, APPS_KEYS, "cleanup.apps", errors);
      for (const key of ["remove", "unregister"]) if (value.apps[key] !== undefined && typeof value.apps[key] !== "boolean") errors.push(`cleanup.apps.${key} must be a boolean`);
      const dir = insideApp(value.apps.dir, "cleanup.apps.dir", errors, { strict: true, bound });
      if (dir) out.apps = { dir, remove: value.apps.remove === true, unregister: value.apps.unregister === true };
    }
  }
  if (value.dmgVolumes !== undefined) {
    if (!Array.isArray(value.dmgVolumes) || value.dmgVolumes.some(badVolumePrefix)) errors.push("cleanup.dmgVolumes must be a list of non-empty volume name prefixes (no path separators)");
    else out.dmgVolumes = [...value.dmgVolumes];
  }
  return out;
}

function validateStability(value, errors, bound = []) {
  if (value === undefined) return undefined;
  if (!isObject(value)) { errors.push("stability must be an object"); return undefined; }
  unknownKeys(value, STABILITY_KEYS, "stability", errors);
  for (const key of ["intervalMs", "checks", "timeoutMs"]) if (value[key] !== undefined && !positiveInt(value[key])) errors.push(`stability.${key} must be a positive integer`);
  if (!Array.isArray(value.files) || !value.files.length) { errors.push("stability.files must be a non-empty list of relative paths"); return undefined; }
  const files = value.files.map((item, index) => insideApp(item, `stability.files[${index}]`, errors, { strict: true, bound })).filter(Boolean);
  return { files, ...(value.intervalMs === undefined ? {} : { intervalMs: value.intervalMs }), ...(value.checks === undefined ? {} : { checks: value.checks }), ...(value.timeoutMs === undefined ? {} : { timeoutMs: value.timeoutMs }) };
}

function validateStage(value, errors) {
  if (value === undefined) return undefined;
  if (!isObject(value)) { errors.push("stage must be an object"); return undefined; }
  unknownKeys(value, STAGE_KEYS, "stage", errors);
  const out = {};
  const absolute = (item, label) => {
    if (typeof item !== "string" || !path.isAbsolute(item) || item.split(/[\\/]/).includes("..")) { errors.push(`${label} must be an absolute path without ..: ${JSON.stringify(item)}`); return undefined; }
    return item;
  };
  for (const [side, appKey, outerKey] of [["pre", "from", "to"], ["post", "to", "from"]]) {
    if (value[side] === undefined) continue;
    if (!Array.isArray(value[side])) { errors.push(`stage.${side} must be a list of { from, to }`); continue; }
    out[side] = value[side].map((entry, index) => {
      const label = `stage.${side}[${index}]`;
      if (!isObject(entry)) { errors.push(`${label} must be an object`); return undefined; }
      unknownKeys(entry, STAGE_ENTRY_KEYS, label, errors);
      if (entry.fresh !== undefined && typeof entry.fresh !== "boolean") errors.push(`${label}.fresh must be a boolean`);
      if (side === "pre" && entry.fresh !== undefined) errors.push(`${label}.fresh is only valid in stage.post`);
      const inner = insideApp(entry[appKey], `${label}.${appKey}`, errors, { strict: true });
      const outer = absolute(entry[outerKey], `${label}.${outerKey}`);
      if (!inner || !outer) return undefined;
      return side === "pre" ? { from: inner, to: outer } : { from: outer, to: inner, ...(entry.fresh === true ? { fresh: true } : {}) };
    }).filter(Boolean);
  }
  if (value.verify !== undefined) {
    if (!Array.isArray(value.verify)) errors.push("stage.verify must be a list of { app, real }");
    else out.verify = value.verify.map((entry, index) => {
      const label = `stage.verify[${index}]`;
      if (!isObject(entry)) { errors.push(`${label} must be an object`); return undefined; }
      unknownKeys(entry, new Set(["app", "real"]), label, errors);
      const app = insideApp(entry.app, `${label}.app`, errors, { strict: true });
      const real = absolute(entry.real, `${label}.real`);
      return app && real ? { app, real } : undefined;
    }).filter(Boolean);
  }
  if (value.resolve !== undefined) {
    if (!isObject(value.resolve)) errors.push("stage.resolve must be { manifest, expected }");
    else {
      unknownKeys(value.resolve, new Set(["manifest", "expected"]), "stage.resolve", errors);
      const manifest = absolute(value.resolve.manifest, "stage.resolve.manifest");
      const expected = absolute(value.resolve.expected, "stage.resolve.expected");
      if (manifest && expected) out.resolve = { manifest, expected };
    }
  }
  return out;
}

function validateVerifySidecars(value, errors) {
  if (value === undefined) return undefined;
  if (!isObject(value)) { errors.push("verifySidecars must be { receipt, target }"); return undefined; }
  unknownKeys(value, VERIFY_SIDECARS_KEYS, "verifySidecars", errors);
  if (value.target !== UNIVERSAL_TRIPLE) errors.push(`verifySidecars.target must be ${UNIVERSAL_TRIPLE}`);
  const receipt = insideApp(value.receipt, "verifySidecars.receipt", errors, { strict: true });
  return receipt ? { receipt, target: UNIVERSAL_TRIPLE } : undefined;
}

function validateNativeHelpersSection(value, errors, bound = []) {
  if (value === undefined) return undefined;
  if (!isObject(value)) { errors.push("nativeHelpers must be { app, helpers, version?, forceArch?, configuration? }"); return undefined; }
  unknownKeys(value, NATIVE_HELPERS_KEYS, "nativeHelpers", errors);
  const app = insideApp(value.app, "nativeHelpers.app", errors, { strict: true, bound });
  if (app && !app.replace(/\/+$/, "").endsWith(".app")) errors.push("nativeHelpers.app must name the .app bundle");
  let helpers;
  try { helpers = validateNativeHelpers(value.helpers, { label: "nativeHelpers.helpers" }); } catch (error) { errors.push(...(error.details?.errors ?? [error.message])); }
  if (value.forceArch !== undefined && !["arm64", "x86_64"].includes(value.forceArch)) errors.push("nativeHelpers.forceArch must be arm64 or x86_64");
  if (value.configuration !== undefined && !["release", "debug"].includes(value.configuration)) errors.push("nativeHelpers.configuration must be release or debug");
  if (value.version !== undefined && (typeof value.version !== "string" || !/^\d+(\.\d+){0,2}$/.test(value.version))) errors.push("nativeHelpers.version must be 1-3 numeric parts");
  return app && helpers ? { app, helpers, ...(value.version === undefined ? {} : { version: value.version }), ...(value.forceArch ? { forceArch: value.forceArch } : {}), ...(value.configuration ? { configuration: value.configuration } : {}) } : undefined;
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
  const boundTargets = validateBoundTargets(plan.boundTargets, errors);
  const cleanup = validateCleanup(plan.cleanup, errors, boundTargets ?? []);
  const stability = validateStability(plan.stability, errors, boundTargets ?? []);
  const stage = validateStage(plan.stage, errors);
  const verifySidecars = validateVerifySidecars(plan.verifySidecars, errors);
  const nativeHelpers = validateNativeHelpersSection(plan.nativeHelpers, errors, boundTargets ?? []);
  if (boundTargets && stage && (stage.pre?.length || stage.post?.length)) errors.push("a plan that binds Cargo targets (a door split) plans no stage copies: the build output is read and signed in place (rightkit#924 copies are zero)");
  if (errors.length) fail("RECIPE_BUILD_PLAN_INVALID", `invalid recipe plan:\n${errors.map((line) => `- ${line}`).join("\n")}`, { errors });
  return { version: RECIPE_BUILD_PLAN_VERSION, steps, ...(boundTargets ? { boundTargets } : {}), ...(cleanup ? { cleanup } : {}), ...(stability ? { stability } : {}), ...(stage ? { stage } : {}), ...(verifySidecars ? { verifySidecars } : {}), ...(nativeHelpers ? { nativeHelpers } : {}) };
}

/** This package's own right-release entry: the door's RightKit, whatever the app's node_modules holds. */
export const OWN_RIGHT_RELEASE = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
// cmd.exe would reinterpret these in a shell launch; refuse rather than escape.
const SHELL_UNSAFE = /[&|<>^%"!\r\n]/;

/**
 * How to launch one step without a shell. `pnpm` runs through the pinned pnpm CLI (npm_execpath, set whenever the
 * release runs under pnpm). On Windows a non-.exe command is a .cmd shim that only a shell can start, so it is allowed
 * only when no argument could be reinterpreted by that shell.
 */
export function resolveLaunch({ cmd, args = [], platform = process.platform, env = process.env, execPath = process.execPath, ownRightRelease = OWN_RIGHT_RELEASE }) {
  // A step's `right-release ...` (also `pnpm exec right-release ...` and `pnpm right-release ...`) runs THIS package's own CLI, never the
  // app's node_modules copy that `pnpm exec` would put first on PATH (the same rule as development.mjs resolveCommand).
  if (cmd === "right-release") return { command: execPath, args: [ownRightRelease, ...args], shell: false };
  if (cmd === "pnpm" && ((args[0] === "exec" && args[1] === "right-release") || args[0] === "right-release")) return { command: execPath, args: [ownRightRelease, ...args.slice(args[0] === "exec" ? 2 : 1)], shell: false };
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
 * Returns { steps: [{ phase, cmd, status }], cleaned, stable, staged }. The first failure throws RECIPE_BUILD_STEP_FAILED and
 * nothing after it runs.
 * Options: appRoot (absolute), env, platform, log, spawnSync, fs, sleep, now, statSync (injected in tests).
 */
export function runRecipeBuild(rawPlan, { appRoot, env = process.env, platform = process.platform, log = () => {}, spawnSync = nodeSpawnSync, fs, sleep, now, statSync, resolveTargetRoot = cargoResolveTargetRoot, door = buildLaunch } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("RECIPE_BUILD_PLAN_INVALID", "appRoot must be an absolute path");
  const plan = validateRecipePlan(rawPlan);
  const done = [];
  const cleaned = runRecipeCleanup(plan.cleanup, { appRoot, platform, log, spawnSync, ...(fs ? { fs } : {}) });
  let stable = [];
  let staged = [];
  let nativeHelpers = null;
  const stagePost = plan.stage?.post ?? [];
  const resolveEntry = (entry, appKey) => ({ ...entry, [appKey]: path.resolve(appRoot, entry[appKey]) });
  for (const [index, step] of plan.steps.entries()) {
    const verifyReal = (what) => { for (const entry of plan.stage?.verify ?? []) verifySameContent({ app: path.resolve(appRoot, entry.app), real: entry.real }, { what, ...(fs ? { fs } : {}) }); };
    const checkDir = (what) => { if (plan.stage?.resolve) assertTargetDirUnchanged(plan.stage.resolve, resolveTargetRoot, { what }); };
    if (step.phase === "build" && plan.stage) {
      checkDir("before the build step");
      removeFreshSources(stagePost, { ...(fs ? { fs } : {}), log });
      staged = stageArtifacts((plan.stage.pre ?? []).map((entry) => resolveEntry(entry, "from")), { ...(fs ? { fs } : {}), log, requireParent: true });
      // Immediately before the step that embeds the real EXE: it must be exactly the signed in-app copy.
      verifyReal("after the copy-back, before the build step");
    }
    if (step.phase === "build" && plan.verifySidecars) verifyUniversalSidecars(plan.verifySidecars, { appRoot, spawnSync, env, log, ...(fs?.readFileSync ? { readFile: fs.readFileSync } : {}) });
    const stepArgs = lockedTauriArgs(step.args, env);
    const launch = resolveLaunch({ cmd: step.cmd, args: stepArgs, platform, env });
    const cwd = path.resolve(appRoot, step.cwd ?? ".");
    const stepEnv = { ...env, ...(step.env ?? {}) };
    log(`recipe-build: [${step.phase}] ${step.cmd} ${step.args.join(" ")}`.trimEnd());
    // The release stage goes through the same one door as dev and test (build-launch.mjs; rightkit#1301, one model): on a broker host a pnpm,
    // node or cargo step is a broker job (in a snapshot stage a carried one, with the Tauri CLI's compiles nested under it), a snapshot stage's
    // `install` step is skipped, and this package's own right-release runs as it is. Where the door adds nothing (a direct host, a step it
    // passes through unchanged) the step runs as resolveLaunch decided.
    const own = launch.command === process.execPath && launch.args[0] === OWN_RIGHT_RELEASE;
    const routed = own ? null : door(step.cmd, stepArgs, { env: stepEnv, platform, cwd, exists: (fs?.existsSync ?? nodeExistsSync) });
    const passthrough = !routed || (routed.command === step.cmd && routed.args.length === stepArgs.length && routed.args.every((a, i) => a === stepArgs[i]));
    const run = passthrough ? launch : { command: routed.command, args: routed.args, shell: false };
    const result = spawnSync(run.command, run.args, { cwd, env: { ...stepEnv, ...(!passthrough && routed.env ? routed.env : {}) }, stdio: "inherit", shell: run.shell, windowsHide: true });
    if (result?.error || result?.status !== 0) {
      fail("RECIPE_BUILD_STEP_FAILED", `recipe-build ${step.phase} step ${index} (${step.cmd}) failed (${result?.error?.message ?? `exit ${result?.status ?? result?.signal ?? "unknown"}`}); later steps were not run`, { phase: step.phase, index, cmd: step.cmd, status: result?.status ?? null });
    }
    done.push({ phase: step.phase, cmd: step.cmd, status: 0 });
    if (step.phase === "build" && plan.stage) { verifyReal("after the build step"); checkDir("after the build step"); }
    // The REAL outputs must stop changing before anything is copied: a half-written EXE is never staged.
    if (step.phase === "build" && stagePost.length) waitForStableArtifacts({ ...(plan.stability ?? {}), files: stagePost.map((entry) => entry.from).filter((file) => (fs?.existsSync ?? nodeExistsSync)(file)) }, { appRoot, log, ...(sleep ? { sleep } : {}), ...(now ? { now } : {}), ...(statSync ? { statSync } : {}) });
    if (step.phase === "build" && stagePost.length) staged.push(...stageArtifacts(stagePost.map((entry) => resolveEntry(entry, "to")), { ...(fs ? { fs } : {}), log }));
    if (step.phase === "build") stable = waitForStableArtifacts(plan.stability, { appRoot, log, ...(sleep ? { sleep } : {}), ...(now ? { now } : {}), ...(statSync ? { statSync } : {}) });
    // Native Swift helpers are built and embedded into the built .app AFTER the app exists and BEFORE postBuild and signing.
    if (step.phase === "build" && plan.nativeHelpers) {
      const section = plan.nativeHelpers;
      nativeHelpers = buildAndEmbedNativeHelpers({ appRoot, app: path.resolve(appRoot, section.app), helpers: section.helpers, version: section.version, forceArch: section.forceArch, configuration: section.configuration ?? "release", env, spawn: spawnSync, log, ...(fs ? { fs } : {}) });
    }
  }
  return { steps: done, cleaned, stable, staged, nativeHelpers };
}

/**
 * After the prepare steps of a universal macOS build: the stage-sidecar receipt must be for universal-apple-darwin and every
 * externalBin file it lists must be named `<name>-universal-apple-darwin` (the name Tauri's universal build looks for) and hold
 * exactly arm64 and x86_64 (`lipo -archs`). Anything else throws RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL before the Tauri build starts.
 * Options: appRoot, spawnSync, readFile, env, log.
 */
export function verifyUniversalSidecars(section, { appRoot, spawnSync = nodeSpawnSync, readFile = readFileSync, env = process.env, log = () => {} } = {}) {
  const receiptFile = path.resolve(appRoot, section.receipt);
  let receipt;
  try { receipt = JSON.parse(readFile(receiptFile, "utf8")); } catch (error) { fail("RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL", `cannot read the stage-sidecar receipt ${section.receipt} (${error.message}); stageSidecars must run before the build`); }
  if (!isObject(receipt) || !Array.isArray(receipt.files)) fail("RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL", `${section.receipt} is not a stage-sidecar receipt`);
  if (receipt.target !== section.target) fail("RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL", `${section.receipt} was staged for ${receipt.target}, not ${section.target}`);
  const checked = [];
  for (const file of receipt.files.filter((item) => item?.externalBin === true)) {
    if (typeof file.path !== "string" || !file.path.endsWith(`-${UNIVERSAL_TRIPLE}`)) fail("RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL", `externalBin ${file.name} is staged as ${file.path}, not <name>-${UNIVERSAL_TRIPLE}`);
    const absolute = path.resolve(appRoot, file.path);
    if (path.relative(appRoot, absolute).startsWith("..")) fail("RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL", `externalBin ${file.name} path escapes the app: ${file.path}`);
    const result = spawnSync("lipo", ["-archs", absolute], { cwd: appRoot, env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    const archs = String(result?.stdout ?? "").trim().split(/\s+/).filter(Boolean).sort();
    if (result?.error || result?.status !== 0 || archs.join(" ") !== "arm64 x86_64") fail("RECIPE_BUILD_SIDECAR_NOT_UNIVERSAL", `externalBin ${file.path} must hold exactly arm64 and x86_64, lipo -archs reports ${JSON.stringify(archs.join(" "))}${result?.error ? ` (${result.error.message})` : ""}`, { file: file.path, archs });
    checked.push(file.path);
  }
  log(`recipe-build: universal sidecars verified (${checked.length} externalBin file(s))`);
  return checked;
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
