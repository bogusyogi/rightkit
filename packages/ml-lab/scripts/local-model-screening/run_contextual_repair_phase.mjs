#!/usr/bin/env node
import childProcess from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { acquireOrAdoptRunLock, reapActiveServer, validateRuntimeHashes } from "./run_contextual_repair_overnight.mjs";
import { sha256 } from "./contextual_repair_core.mjs";

function valueAfter(flag, fallback = null) {
  const index = process.argv.indexOf(flag);
  return index === -1 ? fallback : process.argv[index + 1];
}

const dataRoot = process.env.LOCALAPPDATA ?? path.join(os.homedir(), ".local", "share");
const runRoot = path.resolve(valueAfter("--run-root", path.join(dataRoot, "HeardRightNext", "bakeoffs", "l1-local-contextual-repair-v1")));
const phase = valueAfter("--phase", "tune_a");
const runner = path.join(path.dirname(fileURLToPath(new URL(import.meta.url))), "run_contextual_repair_screen.mjs");
const approval = valueAfter("--approval-ledger", path.join(runRoot, "private", "approval-ledger.json"));
const modelRoot = valueAfter("--model-root");
const requestedLlamaServer = valueAfter("--llama-server");
let effectiveLlamaServer = requestedLlamaServer;

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function eligibleManifests() {
  const root = path.join(runRoot, "candidate-visible", phase === "holdout" ? "holdout" : "tune", "manifests");
  if (!fs.existsSync(root)) throw new Error(`manifest root missing: ${root}`);
  let allowed = null;
  if (phase === "tune_b") {
    const file = path.join(runRoot, "private", "survivors.stage_a.json");
    if (!fs.existsSync(file)) throw new Error("tune_b needs survivors.stage_a.json");
    allowed = new Set((readJson(file).survivors ?? []).map((row) => `${row.candidate_id}__${row.recipe_id}`));
  } else if (phase === "holdout") {
    const file = path.join(runRoot, "private", "winners.freeze.json");
    if (!fs.existsSync(file)) throw new Error("holdout needs winners.freeze.json");
    allowed = new Set((readJson(file).winners ?? []).map((row) => `${row.candidate_id}__${row.recipe_id}`));
  }
  const rows = [];
  for (const candidateDir of fs.readdirSync(root, { withFileTypes: true }).filter((entry) => entry.isDirectory())) {
    for (const name of fs.readdirSync(path.join(root, candidateDir.name)).filter((entry) => entry.endsWith(`.${phase}.json`))) {
      const manifest = readJson(path.join(root, candidateDir.name, name));
      if (!allowed || allowed.has(`${manifest.candidate_id}__${manifest.recipe_id}`)) rows.push({ candidate_id: manifest.candidate_id, recipe_id: manifest.recipe_id, rows: manifest.rows.length });
    }
  }
  return rows.sort((a, b) => a.candidate_id.localeCompare(b.candidate_id) || a.recipe_id.localeCompare(b.recipe_id));
}

function runOne(plan, dryRun) {
  const args = [
    runner,
    dryRun ? "--dry-run" : "--live",
    "--run-root", runRoot,
    "--phase", phase,
    "--candidates", plan.candidate_id,
    "--recipes", plan.recipe_id,
  ];
  if (!dryRun) args.push("--approval-ledger", approval);
  if (modelRoot) args.push("--model-root", modelRoot);
  if (effectiveLlamaServer) args.push("--llama-server", effectiveLlamaServer);
  const limit = valueAfter("--limit");
  if (limit) args.push("--limit", limit);
  return childProcess.spawnSync(process.execPath, args, { cwd: path.dirname(runner), stdio: "inherit", windowsHide: true }).status ?? 1;
}

function main() {
  if (!["tune_a", "tune_b", "holdout"].includes(phase)) throw new Error(`invalid phase: ${phase}`);
  const dryRun = process.argv.includes("--dry-run");
  if (!dryRun && !process.argv.includes("--live")) throw new Error("choose --dry-run or --live");
  let gpuLock = null;
  let priorLockToken;
  if (!dryRun) {
    const controlFile = path.join(runRoot, "private", "control.json");
    const recipesFile = path.join(runRoot, "candidate-visible", "candidate-recipes.json");
    const control = readJson(controlFile);
    const approvedRuntime = control.runtime_files?.find((entry) => entry.role === "llama_server")?.file;
    effectiveLlamaServer = requestedLlamaServer ? path.resolve(requestedLlamaServer) : approvedRuntime;
    const ledger = readJson(approval);
    if (ledger.approved !== true || !ledger.approved_by || !ledger.approved_at) throw new Error("approval ledger is not approved");
    if (ledger.control_sha256 !== sha256(fs.readFileSync(controlFile))) throw new Error("approval control hash mismatch");
    if (ledger.recipes_sha256 !== sha256(fs.readFileSync(recipesFile))) throw new Error("approval recipe hash mismatch");
    if (!ledger.permitted_phases?.includes(phase)) throw new Error(`approval does not permit phase ${phase}`);
    validateRuntimeHashes(control, { nodeVersion: process.version, llamaServer: effectiveLlamaServer });
    gpuLock = acquireOrAdoptRunLock(runRoot);
    priorLockToken = process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN;
    process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN = gpuLock.token;
    reapActiveServer(runRoot);
  }
  try {
  let plans = eligibleManifests();
  const candidates = (valueAfter("--candidates", "")).split(/[\s,]+/).filter(Boolean);
  if (candidates.length) plans = plans.filter((plan) => candidates.includes(plan.candidate_id));
  const recipes = (valueAfter("--recipes", "")).split(/[\s,]+/).filter(Boolean);
  if (recipes.length) plans = plans.filter((plan) => recipes.includes(plan.recipe_id));
  if (!plans.length) throw new Error("no eligible manifests");
  let consecutiveFailures = 0;
  for (const [index, plan] of plans.entries()) {
    console.log(JSON.stringify({ event: "phase_plan_start", phase, index: index + 1, total: plans.length, ...plan, dry_run: dryRun }));
    const status = runOne(plan, dryRun);
    if (status === 0) consecutiveFailures = 0;
    else consecutiveFailures += 1;
    if (consecutiveFailures >= 3) throw new Error("phase circuit opened after three consecutive manifest failures");
  }
  console.log(JSON.stringify({ status: dryRun ? "phase_dry_run_no_generation" : "phase_complete", phase, plans: plans.length }));
  } finally {
    if (gpuLock) {
      reapActiveServer(runRoot);
      if (priorLockToken === undefined) delete process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN;
      else process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN = priorLockToken;
      gpuLock.release();
    }
  }
}

try { main(); } catch (error) {
  console.error(error.stack ?? String(error));
  process.exitCode = 1;
}
