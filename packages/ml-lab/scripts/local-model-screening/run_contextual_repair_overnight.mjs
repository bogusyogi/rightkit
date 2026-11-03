#!/usr/bin/env node
import childProcess from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptFile = fileURLToPath(import.meta.url);
const scriptDir = path.dirname(scriptFile);

export const OVERNIGHT_STEPS = Object.freeze([
  { id: "tune_a", kind: "gpu", phase: "tune_a" },
  { id: "score_tune_a", kind: "cpu" },
  { id: "report_tune_a", kind: "cpu" },
  { id: "tune_b", kind: "gpu", phase: "tune_b" },
  { id: "freeze_winners", kind: "cpu" },
  { id: "report_tune_b", kind: "cpu" },
  { id: "prepare_holdout", kind: "cpu" },
  { id: "holdout", kind: "gpu", phase: "holdout" },
  { id: "report_holdout", kind: "cpu" },
  { id: "verify_complete", kind: "cpu" },
]);

function valueAfter(flag, fallback = null) {
  const index = process.argv.indexOf(flag);
  return index === -1 ? fallback : process.argv[index + 1];
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function readJsonl(file) {
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function appendEvent(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.appendFileSync(file, `${JSON.stringify({ at: new Date().toISOString(), ...value })}\n`);
}

function fileHash(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function pidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

export function acquireRunLock(runRoot, pid = process.pid) {
  const lockFile = path.join(runRoot, "private", "overnight-run.lock.json");
  fs.mkdirSync(path.dirname(lockFile), { recursive: true });
  if (fs.existsSync(lockFile)) {
    let existing = null;
    try { existing = readJson(lockFile); } catch {}
    if (pidAlive(existing?.pid)) throw new Error(`overnight run already active as PID ${existing.pid}`);
    fs.rmSync(lockFile, { force: true });
  }
  const token = crypto.randomUUID();
  const descriptor = fs.openSync(lockFile, "wx");
  fs.writeFileSync(descriptor, `${JSON.stringify({ pid, token, started_at: new Date().toISOString() }, null, 2)}\n`);
  fs.closeSync(descriptor);
  let released = false;
  return {
    file: lockFile,
    token,
    adopted: false,
    release() {
      if (released) return;
      released = true;
      try {
        if (readJson(lockFile).token === token) fs.rmSync(lockFile, { force: true });
      } catch {}
    },
  };
}

export function acquireOrAdoptRunLock(runRoot, pid = process.pid, token = process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN) {
  if (!token) return acquireRunLock(runRoot, pid);
  const lockFile = path.join(runRoot, "private", "overnight-run.lock.json");
  if (!fs.existsSync(lockFile)) throw new Error("shared GPU lock token was provided but the lock file is missing");
  const existing = readJson(lockFile);
  if (existing.token !== token) throw new Error("shared GPU lock token mismatch");
  if (!pidAlive(existing.pid)) throw new Error(`shared GPU lock owner PID ${existing.pid} is not active`);
  return { file: lockFile, token, adopted: true, release() {} };
}

function activeServerFile(runRoot) {
  return path.join(runRoot, "private", "active-llama-server.json");
}

export function recordActiveServer(runRoot, state) {
  if (!Number.isInteger(state?.pid) || state.pid <= 0) throw new Error("active llama-server state needs a positive PID");
  const file = activeServerFile(runRoot);
  writeJson(file, { schema: "heardright.active_llama_server.v1", started_at: new Date().toISOString(), ...state });
  return file;
}

export function clearActiveServer(runRoot, pid = null) {
  const file = activeServerFile(runRoot);
  if (!fs.existsSync(file)) return;
  if (pid != null) {
    let current = null;
    try { current = readJson(file); } catch {}
    if (Number(current?.pid) !== Number(pid)) return;
  }
  fs.rmSync(file, { force: true });
}

function terminateProcessTree(pid) {
  if (!pidAlive(pid)) return;
  if (process.platform === "win32") {
    childProcess.execFileSync("taskkill.exe", ["/PID", String(pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
  } else {
    process.kill(-pid, "SIGTERM");
  }
}

export function reapActiveServer(runRoot) {
  const file = activeServerFile(runRoot);
  if (!fs.existsSync(file)) return { reaped: false, pid: null };
  let state = null;
  try { state = readJson(file); } catch {}
  const pid = Number(state?.pid);
  try { if (Number.isInteger(pid) && pid > 0) terminateProcessTree(pid); } catch {}
  if (Number.isInteger(pid) && pid > 0 && pidAlive(pid)) return { reaped: false, pid, still_alive: true };
  clearActiveServer(runRoot, Number.isInteger(pid) ? pid : null);
  return { reaped: true, pid: Number.isInteger(pid) ? pid : null };
}

export function validateOvernightApproval(approval, controlSha256, recipesSha256) {
  if (approval?.approved !== true || !approval.approved_by || !approval.approved_at) {
    throw new Error("overnight run requires an approved ledger with approver and timestamp");
  }
  if (approval.control_sha256 !== controlSha256) throw new Error("approval control hash mismatch");
  if (approval.recipes_sha256 !== recipesSha256) throw new Error("approval recipe hash mismatch");
  for (const phase of ["tune_a", "tune_b", "holdout"]) {
    if (!approval.permitted_phases?.includes(phase)) throw new Error(`approval does not permit ${phase}`);
  }
}

export function validateRuntimeHashes(control, active = {}) {
  if (!Array.isArray(control?.runtime_files) || !control.runtime_files.length) {
    throw new Error("approved control has no runtime file hashes");
  }
  if (active.nodeVersion && control.node_version !== active.nodeVersion) {
    throw new Error(`unapproved Node runtime: ${active.nodeVersion}`);
  }
  if (active.llamaServer) {
    const approved = control.runtime_files.find((entry) => entry.role === "llama_server");
    if (!approved || path.resolve(approved.file) !== path.resolve(active.llamaServer)) {
      throw new Error(`unapproved llama-server: ${active.llamaServer}`);
    }
  }
  for (const entry of control.runtime_files) {
    if (!entry?.file || !entry?.sha256 || !fs.existsSync(entry.file)) {
      throw new Error(`approved runtime file is missing: ${entry?.file ?? "unknown"}`);
    }
    if (fileHash(entry.file) !== entry.sha256) throw new Error(`runtime hash mismatch: ${entry.file}`);
  }
}

export function phaseCoverage(manifestRows, resultRows) {
  const latest = new Map();
  for (const row of resultRows) latest.set(row.row_id, row);
  const terminal = new Set(["ok", "rejected"]);
  const missingOrFailed = manifestRows
    .filter((row) => !terminal.has(latest.get(row.row_id)?.status))
    .map((row) => row.row_id);
  const completedRows = manifestRows.map((row) => latest.get(row.row_id)).filter((row) => terminal.has(row?.status));
  return {
    expected: manifestRows.length,
    completed: completedRows.length,
    ok: completedRows.filter((row) => row.status === "ok").length,
    rejected: completedRows.filter((row) => row.status === "rejected").length,
    missing_or_failed: missingOrFailed,
  };
}

function selectionKeys(runRoot, phase) {
  if (phase === "tune_a") return null;
  const file = phase === "tune_b"
    ? path.join(runRoot, "private", "survivors.stage_a.json")
    : path.join(runRoot, "private", "winners.freeze.json");
  const rows = phase === "tune_b" ? readJson(file).survivors : readJson(file).winners;
  return new Set((rows ?? []).map((row) => `${row.candidate_id}__${row.recipe_id}`));
}

function phaseManifests(runRoot, phase, candidateFilter = []) {
  const root = path.join(runRoot, "candidate-visible", phase === "holdout" ? "holdout" : "tune", "manifests");
  const allowed = selectionKeys(runRoot, phase);
  const selectedCandidates = new Set(candidateFilter);
  const manifests = [];
  for (const candidateDir of fs.readdirSync(root, { withFileTypes: true }).filter((entry) => entry.isDirectory())) {
    if (selectedCandidates.size && !selectedCandidates.has(candidateDir.name)) continue;
    for (const name of fs.readdirSync(path.join(root, candidateDir.name)).filter((entry) => entry.endsWith(`.${phase}.json`))) {
      const file = path.join(root, candidateDir.name, name);
      const manifest = readJson(file);
      const key = `${manifest.candidate_id}__${manifest.recipe_id}`;
      if (!allowed || allowed.has(key)) manifests.push({ file, manifest, key });
    }
  }
  return manifests.sort((a, b) => a.key.localeCompare(b.key));
}

function verifyPhase(runRoot, phase) {
  const failures = [];
  let expected = 0;
  let completed = 0;
  let ok = 0;
  let rejected = 0;
  const manifests = phaseManifests(runRoot, phase);
  for (const { manifest, key } of manifests) {
    const resultFile = path.join(runRoot, "results", phase, `${key}.jsonl`);
    const coverage = phaseCoverage(manifest.rows, readJsonl(resultFile));
    expected += coverage.expected;
    completed += coverage.completed;
    ok += coverage.ok;
    rejected += coverage.rejected;
    const resourceFile = path.join(runRoot, "resources", phase, `${key}.json`);
    if (coverage.missing_or_failed.length || !fs.existsSync(resourceFile)) {
      failures.push({ key, missing_or_failed: coverage.missing_or_failed.length, resource_file: fs.existsSync(resourceFile) });
    }
  }
  return { phase, manifests: manifests.length, expected, completed, ok, rejected, failures };
}

function childArgs(base, options) {
  const result = [...base, "--run-root", options.runRoot];
  if (options.approval) result.push("--approval-ledger", options.approval);
  if (options.modelRoot) result.push("--model-root", options.modelRoot);
  if (options.llamaServer) result.push("--llama-server", options.llamaServer);
  return result;
}

function runChild(label, file, args, eventFile, allowFailure = false) {
  appendEvent(eventFile, { event: "step_start", step: label, command: file, args });
  const result = childProcess.spawnSync(process.execPath, [file, ...args], {
    cwd: scriptDir,
    stdio: "inherit",
    windowsHide: true,
  });
  const status = result.status ?? 1;
  appendEvent(eventFile, { event: "step_end", step: label, status, signal: result.signal ?? null });
  if (status !== 0 && !allowFailure) throw new Error(`${label} failed with exit code ${status}`);
  return status;
}

function runGpuPhase(phase, options, scripts, eventFile) {
  const args = childArgs(["--live", "--phase", phase], options);
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    const status = runChild(`${phase}_attempt_${attempt}`, scripts.phase, args, eventFile, true);
    const coverage = verifyPhase(options.runRoot, phase);
    appendEvent(eventFile, { event: "phase_coverage", attempt, process_status: status, ...coverage });
    if (status === 0 && coverage.failures.length === 0 && coverage.expected === coverage.completed) return coverage;
  }
  const coverage = verifyPhase(options.runRoot, phase);
  throw new Error(`${phase} incomplete after three resumable attempts: ${JSON.stringify(coverage.failures.slice(0, 10))}`);
}

function verifyComplete(options) {
  const phases = ["tune_a", "tune_b", "holdout"].map((phase) => verifyPhase(options.runRoot, phase));
  const recipes = readJson(path.join(options.runRoot, "candidate-visible", "candidate-recipes.json"));
  const winners = readJson(path.join(options.runRoot, "private", "winners.freeze.json")).winners ?? [];
  const reportFile = path.join(options.runRoot, "reports", "holdout.summary.json");
  if (phases.some((phase) => phase.failures.length || phase.expected !== phase.completed)) throw new Error("final phase coverage is incomplete");
  if (winners.length !== recipes.candidates.length) throw new Error(`winner count mismatch: ${winners.length}/${recipes.candidates.length}`);
  if (!fs.existsSync(reportFile)) throw new Error("holdout report is missing");
  const report = readJson(reportFile);
  const summary = {
    schema: "heardright.contextual_repair_overnight_complete.v1",
    status: "complete",
    completed_at: new Date().toISOString(),
    control_sha256: fileHash(path.join(options.runRoot, "private", "control.json")),
    recipes_sha256: fileHash(path.join(options.runRoot, "candidate-visible", "candidate-recipes.json")),
    candidates: recipes.candidates.length,
    winners: winners.length,
    phases,
    cloud_cleanup_stage_wall_ms: report.cloud_cleanup_stage_wall_ms,
    holdout_report: reportFile,
    human_review_status: report.summaries?.every((row) => row.final_quality_complete) ? "complete" : "pending_blind_review",
  };
  const file = path.join(options.runRoot, "reports", "overnight.complete.json");
  writeJson(file, summary);
  return { file, summary };
}

function dryRun(options, scripts) {
  const recipes = readJson(path.join(options.runRoot, "candidate-visible", "candidate-recipes.json"));
  const requested = options.candidates.length ? options.candidates : recipes.candidates.slice(0, 3).map((candidate) => candidate.id);
  const known = new Set(recipes.candidates.map((candidate) => candidate.id));
  for (const candidate of requested) if (!known.has(candidate)) throw new Error(`unknown candidate: ${candidate}`);
  const status = childProcess.spawnSync(process.execPath, [
    scripts.phase,
    "--dry-run",
    "--phase", "tune_a",
    "--run-root", options.runRoot,
    "--candidates", requested.join(","),
  ], { cwd: scriptDir, stdio: "inherit", windowsHide: true }).status ?? 1;
  if (status !== 0) throw new Error(`tune_a dry-run failed with exit code ${status}`);
  const selected = recipes.candidates.filter((candidate) => requested.includes(candidate.id));
  console.log(JSON.stringify({
    status: "overnight_dry_run_no_generation",
    candidates: selected.length,
    recipes: selected.reduce((sum, candidate) => sum + candidate.recipes.length, 0),
    gpu_concurrency: 1,
    sequence: OVERNIGHT_STEPS.map((step) => step.id),
    live_requires_approved_phases: ["tune_a", "tune_b", "holdout"],
  }));
}

async function main() {
  const defaultDataRoot = process.env.LOCALAPPDATA ?? path.join(os.homedir(), ".local", "share");
  const options = {
    runRoot: path.resolve(valueAfter("--run-root", path.join(defaultDataRoot, "HeardRightNext", "bakeoffs", "l1-local-contextual-repair-v1"))),
    approval: valueAfter("--approval-ledger"),
    modelRoot: valueAfter("--model-root"),
    llamaServer: valueAfter("--llama-server"),
    candidates: valueAfter("--candidates", "").split(/[\s,]+/).filter(Boolean),
  };
  options.approval ??= path.join(options.runRoot, "private", "approval-ledger.json");
  const scripts = {
    phase: path.join(scriptDir, "run_contextual_repair_phase.mjs"),
    runner: path.join(scriptDir, "run_contextual_repair_screen.mjs"),
    scorer: path.join(scriptDir, "score_contextual_repair_screen.mjs"),
  };
  for (const file of Object.values(scripts)) if (!fs.existsSync(file)) throw new Error(`required script missing: ${file}`);

  if (process.argv.includes("--dry-run")) return dryRun(options, scripts);
  if (!process.argv.includes("--live")) throw new Error("choose --dry-run or --live");
  if (options.candidates.length) throw new Error("live overnight runs cannot filter candidates");

  const controlFile = path.join(options.runRoot, "private", "control.json");
  const recipesFile = path.join(options.runRoot, "candidate-visible", "candidate-recipes.json");
  const control = readJson(controlFile);
  validateOvernightApproval(readJson(options.approval), fileHash(controlFile), fileHash(recipesFile));
  validateRuntimeHashes(control, { nodeVersion: process.version, llamaServer: options.llamaServer });

  const lock = acquireRunLock(options.runRoot);
  const priorLockToken = process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN;
  process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN = lock.token;
  reapActiveServer(options.runRoot);
  const eventFile = path.join(options.runRoot, "logs", "overnight.events.jsonl");
  appendEvent(eventFile, { event: "overnight_start", pid: process.pid, gpu_concurrency: 1 });
  try {
    runGpuPhase("tune_a", options, scripts, eventFile);
    runChild("score_tune_a", scripts.scorer, ["--stage-a", "--run-root", options.runRoot], eventFile);
    runChild("report_tune_a", scripts.scorer, ["--report", "--phase", "tune_a", "--run-root", options.runRoot], eventFile);
    runGpuPhase("tune_b", options, scripts, eventFile);
    runChild("freeze_winners", scripts.scorer, ["--freeze-winners", "--run-root", options.runRoot], eventFile);
    runChild("report_tune_b", scripts.scorer, ["--report", "--phase", "tune_b", "--run-root", options.runRoot], eventFile);
    runChild("prepare_holdout", scripts.runner, ["--prepare-holdout", "--run-root", options.runRoot], eventFile);
    runGpuPhase("holdout", options, scripts, eventFile);
    runChild("report_holdout", scripts.scorer, ["--report", "--phase", "holdout", "--run-root", options.runRoot], eventFile);
    const completed = verifyComplete(options);
    appendEvent(eventFile, { event: "overnight_complete", report: completed.file, status: completed.summary.status });
    console.log(JSON.stringify({ status: "overnight_complete", report: completed.file, gpu_concurrency: 1 }));
  } catch (error) {
    appendEvent(eventFile, { event: "overnight_failed", error: String(error?.stack ?? error) });
    throw error;
  } finally {
    reapActiveServer(options.runRoot);
    if (priorLockToken === undefined) delete process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN;
    else process.env.RIGHTKIT_ML_GPU_RUN_LOCK_TOKEN = priorLockToken;
    lock.release();
  }
}

if (path.resolve(process.argv[1] ?? "") === path.resolve(scriptFile)) {
  main().catch((error) => {
    console.error(error.stack ?? String(error));
    process.exitCode = 1;
  });
}
