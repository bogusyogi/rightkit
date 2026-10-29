// Admission: the build's deterministic refusals as rows, run by the doctor and by the start of `right-release build` alike.
// A row is data, { id, tier, run(ctx) }:
//   files    reads the release config and the committed files only (no tool, no network, no credential)
//   host     asks this machine (tools, environment, signing prerequisites); injectable env and spawn
//   network  asks a registry
// `run` returns nothing (pass), an array of finding texts, or { notRun } (the probe cannot run on this host: never a pass),
// { na } (the row does not apply to this target) or { skipped } (another step runs it). { findings, notRun } is both at once: the
// row fails, and part of it also could not run. A throw is a finding, never a crash.
// The doctor and the build call the same rows with the same text, so a refusal found in one is found in the other; the build
// calls them before it takes a lock, writes a state directory, installs or starts a worker. See docs/PIPELINE-ARCHITECTURE.md.
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { expandPackageRecipeTarget } from "./package-recipe-targets.mjs";
import { isNativeIosTarget, resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { resolveProjectType } from "./project-type.mjs";
import { RightKitRegistryError } from "./rightkit-registry-resolver.mjs";
import { FILES_ROWS } from "./admission-files.mjs";
import { HOST_ROWS } from "./admission-host.mjs";

export const TIERS = Object.freeze(["files", "host", "network"]);

/** The registry finding, worded for the doctor and the build alike. */
export function describeRegistryError(error, app) {
  return error instanceof RightKitRegistryError
    ? `${app} ${error.recoverable ? "RECOVERABLE " : ""}${error.message}`
    : `RightKit registry resolution threw: ${error instanceof Error ? error.message : String(error)}`;
}

// The RightKit pins against the registries (rightkit-registry-resolver.mjs). `ctx.registry()` is the caller's resolver: the build
// hands it the same call it used to make after the lock, so a fresh build resolves once, before any side effect. The doctor
// resolves in its own top-level step (its package rules read the result), so it skips this row.
const NETWORK_ROWS = Object.freeze([{
  id: "rightkit-registry",
  tier: "network",
  doctorOwned: true,
  async run(ctx) {
    if (!ctx.registry) return { na: "no registry resolver was supplied" };
    try { ctx.out.registry = await ctx.registry(ctx); } catch (error) { return [describeRegistryError(error, ctx.config?.app ?? path.basename(ctx.root))]; }
    return undefined;
  },
}]);

export const ADMISSION_ROWS = Object.freeze([...FILES_ROWS, ...HOST_ROWS, ...NETWORK_ROWS]);

const memo = (fn) => { let box; return () => (box ??= { value: fn() }).value; };

function gitHead(repoRoot, spawn) {
  const result = spawn("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`git rev-parse HEAD failed: ${String(result.stderr ?? "").trim()}`);
  return String(result.stdout).trim();
}

/**
 * Run the rows of `tiers` over one app and platform. `target` is the expanded target the worker validates; without it the
 * target is expanded here and a failure is the `target` finding the doctor reports, with the rows that read it BLOCKED.
 * Returns { findings: [{ row, detail }], rows: [{ id, tier, status, detail }], notRun: [{ id, reason }], out }.
 * `mode` is "doctor" or "build": declared-checks-run executes only in the doctor (the build runs the checks after install).
 */
export async function runAdmission({
  root, repoRoot = root, configPath, config, platform, target, tiers = TIERS, env = process.env, spawn = spawnSync, registry,
  log = () => {}, hostPlatform = process.platform, mode = "build", skipChecks = false, skipDoctorOwned = false, commit,
  fs = { existsSync, readdirSync, readFileSync, statSync }, feed, rows = ADMISSION_ROWS, resolveInputs = resolveBuildInputs, releaseSource = null,
} = {}) {
  const findings = [];
  const results = [];
  const out = {};
  let targetProblem = null;
  if (!target) {
    const app = config?.app ?? "app";
    try {
      target = expandPackageRecipeTarget(config?.targets?.[platform], { platform, appRoot: root, config });
      if (!target) targetProblem = `${app} has no ${platform} release target`;
    } catch (error) {
      targetProblem = `${app} ${platform} ${error.message}`;
    }
  }
  const exists = fs.existsSync;
  const projectType = memo(() => resolveProjectType({ appRoot: root, config, target }));
  const ctx = {
    root, repoRoot, releaseSource, configPath, config, platform, target, env, spawn, registry, log, hostPlatform, mode, skipChecks, fs, exists, feed, out,
    // The target as the config declares it: the build controller judges a few rules on it, the worker on the expanded one.
    rawTarget: config?.targets?.[platform],
    projectType,
    nativeIos: memo(() => platform === "ios" && isNativeIosTarget({ appRoot: root, target })),
    commit: memo(() => commit ?? gitHead(repoRoot, spawn)),
  };
  // The canonical build-input inventory, resolved once: { ok, value } or { ok: false, error }.
  ctx.buildInputs = memo(() => resolveInputs(ctx));
  const failed = new Set();
  if (targetProblem && tiers.some((tier) => tier !== "network")) {
    findings.push({ row: "target", detail: targetProblem });
    results.push({ id: "target", tier: "files", status: "fail", detail: targetProblem });
    failed.add("target");
  }
  for (const row of rows) {
    if (!tiers.includes(row.tier)) continue;
    if (skipDoctorOwned && row.doctorOwned) continue;
    const blocker = [...(row.needsTarget === false ? [] : targetProblem ? ["target"] : []), ...(row.after ?? [])].find((id) => failed.has(id));
    if (blocker) {
      results.push({ id: row.id, tier: row.tier, status: "blocked", detail: `BLOCKED by ${blocker}` });
      failed.add(row.id);
      continue;
    }
    let outcome;
    try { outcome = await row.run(ctx); } catch (error) { outcome = [error instanceof Error ? error.message : `${row.id} threw a non-Error value: ${String(error)}`]; }
    const found = Array.isArray(outcome) ? outcome : Array.isArray(outcome?.findings) ? outcome.findings : [];
    if (found.length > 0) {
      for (const detail of found) findings.push({ row: row.id, detail });
      results.push({ id: row.id, tier: row.tier, status: "fail", detail: found.join("\n"), ...(outcome.notRun ? { alsoNotRun: outcome.notRun } : {}) });
      failed.add(row.id);
    } else if (outcome?.notRun) results.push({ id: row.id, tier: row.tier, status: "not-run", detail: outcome.notRun });
    else if (outcome?.na) results.push({ id: row.id, tier: row.tier, status: "n/a", detail: outcome.na });
    else if (outcome?.skipped) results.push({ id: row.id, tier: row.tier, status: "skipped", detail: outcome.skipped });
    else results.push({ id: row.id, tier: row.tier, status: "ok", detail: "" });
  }
  const notRun = results.flatMap((row) => (row.status === "not-run" ? [{ id: row.id, reason: row.detail }] : row.alsoNotRun ? [{ id: row.id, reason: row.alsoNotRun }] : []));
  return { findings, rows: results, notRun, out };
}

function resolveBuildInputs(ctx) {
  try {
    return { ok: true, value: resolveReleaseBuildInputs({ repoRoot: ctx.repoRoot, appRoot: ctx.root, configPath: ctx.configPath, config: ctx.config, target: ctx.target, env: ctx.env }) };
  } catch (error) {
    return { ok: false, error };
  }
}

const SYMBOL = { ok: "ok    ", fail: "FAIL  ", "not-run": "NOT RUN", "n/a": "n/a   ", skipped: "skipped", blocked: "BLOCKED" };

/** The rows as lines for a dry run or a verbose doctor: one per row, the finding text under a failed one. */
export function formatAdmission(result) {
  const counts = (status) => result.rows.filter((row) => row.status === status).length;
  const lines = [`admission: ${result.rows.length} rows, ${counts("fail")} failed, ${result.notRun.length} NOT RUN`];
  for (const row of result.rows) {
    const head = `  [${SYMBOL[row.status]}] ${row.id}`;
    lines.push(row.status === "ok" ? head : `${head}: ${row.detail.replaceAll("\n", "\n         ")}`);
    if (row.alsoNotRun) lines.push(`         NOT RUN: ${row.alsoNotRun.replaceAll("\n", "\n         ")}`);
  }
  return lines;
}

/** Lines naming the rows that could not run on this host. They are not passes. */
export function formatNotRun(result) {
  return result.notRun.map((row) => `NOT RUN ${row.id}: ${row.reason}`);
}
