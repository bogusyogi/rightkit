#!/usr/bin/env node
import childProcess from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { buildRequest, classifyAttemptFailure, deterministicFlags, materializeOutput, sha256 } from "./contextual_repair_core.mjs";
import { acquireOrAdoptRunLock, clearActiveServer, reapActiveServer, recordActiveServer, validateRuntimeHashes } from "./run_contextual_repair_overnight.mjs";

function valueAfter(flag, fallback = null) {
  const index = process.argv.indexOf(flag);
  return index === -1 ? fallback : process.argv[index + 1];
}

function listAfter(flag) {
  const value = valueAfter(flag, "");
  return value.split(/[\s,]+/).map((item) => item.trim()).filter(Boolean);
}

const defaultDataRoot = process.env.LOCALAPPDATA ?? path.join(os.homedir(), ".local", "share");
const runRoot = path.resolve(valueAfter("--run-root", path.join(defaultDataRoot, "HeardRightNext", "bakeoffs", "l1-local-contextual-repair-v1")));
const modelRoot = path.resolve(valueAfter("--model-root", process.platform === "win32" ? "E:\\models\\heardright-local-screening" : path.join(os.homedir(), "heardright-local-screening")));
const phase = valueAfter("--phase", "tune_a");
const candidateFilter = listAfter("--candidates");
const recipeFilter = listAfter("--recipes");
const limit = Number(valueAfter("--limit", "0"));

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function appendJsonl(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.appendFileSync(file, `${JSON.stringify(value)}\n`);
}

function fileHash(file) {
  return sha256(fs.readFileSync(file));
}

function loadControl() {
  const file = path.join(runRoot, "private", "control.json");
  if (!fs.existsSync(file)) throw new Error(`prepared control file is missing: ${file}`);
  return { file, value: readJson(file) };
}

function loadRecipes() {
  const file = path.join(runRoot, "candidate-visible", "candidate-recipes.json");
  if (!fs.existsSync(file)) throw new Error(`recipe snapshot is missing: ${file}`);
  return { file, value: readJson(file) };
}

function findCandidate(recipes, candidateId) {
  const candidate = recipes.candidates.find((item) => item.id === candidateId);
  if (!candidate) throw new Error(`unknown candidate: ${candidateId}`);
  return candidate;
}

function findRecipe(candidate, recipeId) {
  const recipe = candidate.recipes.find((item) => item.id === recipeId);
  if (!recipe) throw new Error(`unknown recipe ${recipeId} for ${candidate.id}`);
  return recipe;
}

function manifestFiles() {
  const root = path.join(runRoot, "candidate-visible", phase === "holdout" ? "holdout" : "tune", "manifests");
  if (!fs.existsSync(root)) throw new Error(`manifest root is missing: ${root}`);
  const files = [];
  for (const candidateDir of fs.readdirSync(root, { withFileTypes: true })) {
    if (!candidateDir.isDirectory()) continue;
    if (candidateFilter.length && !candidateFilter.includes(candidateDir.name)) continue;
    for (const entry of fs.readdirSync(path.join(root, candidateDir.name), { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith(`.${phase}.json`)) continue;
      const file = path.join(root, candidateDir.name, entry.name);
      const manifest = readJson(file);
      if (recipeFilter.length && !recipeFilter.includes(manifest.recipe_id)) continue;
      files.push(file);
    }
  }
  return files.sort();
}

function validateManifest(file, manifest, recipes) {
  if (manifest.phase !== phase) throw new Error(`manifest phase mismatch: ${file}`);
  const candidate = findCandidate(recipes, manifest.candidate_id);
  const recipe = findRecipe(candidate, manifest.recipe_id);
  if (recipe.output_mode !== manifest.output_mode) throw new Error(`output mode mismatch: ${file}`);
  if (!fs.existsSync(manifest.model_path)) throw new Error(`model file missing: ${manifest.model_path}`);
  if (fs.statSync(manifest.model_path).size !== manifest.model_bytes) throw new Error(`model byte size mismatch: ${manifest.candidate_id}`);
  for (const row of manifest.rows) {
    if (!row.row_id || !row.case_id || !row.input_sha256) throw new Error(`manifest row identity is incomplete: ${file}`);
    if (!Array.isArray(row.request?.messages) || row.request.messages.length < 1) throw new Error(`manifest request messages are missing: ${row.row_id}`);
    if (row.request.messages.some((message) => /\{[a-z_]+\}/.test(message.content))) throw new Error(`unrendered placeholder: ${row.row_id}`);
    if (row.request.stream !== false) throw new Error(`stream must stay false: ${row.row_id}`);
  }
  return { candidate, recipe };
}

function validateApproval(controlFile, recipesFile) {
  const approvalPath = valueAfter("--approval-ledger", path.join(runRoot, "private", "approval-ledger.json"));
  if (!fs.existsSync(approvalPath)) throw new Error(`live run requires an approval ledger: ${approvalPath}`);
  const approval = readJson(approvalPath);
  if (approval.approved !== true || !approval.approved_by || !approval.approved_at) throw new Error("approval ledger is not approved");
  if (approval.control_sha256 !== fileHash(controlFile)) throw new Error("approval control hash mismatch");
  if (approval.recipes_sha256 !== fileHash(recipesFile)) throw new Error("approval recipe hash mismatch");
  if (!Array.isArray(approval.permitted_phases) || !approval.permitted_phases.includes(phase)) throw new Error(`approval does not permit phase ${phase}`);
  return approval;
}

function validatePhaseSelection(manifest) {
  if (phase === "tune_a") return;
  if (phase === "tune_b") {
    const survivorPath = path.join(runRoot, "private", "survivors.stage_a.json");
    if (!fs.existsSync(survivorPath)) throw new Error("tune_b requires survivors.stage_a.json from the private scorer");
    const survivors = readJson(survivorPath).survivors ?? [];
    if (!survivors.some((row) => row.candidate_id === manifest.candidate_id && row.recipe_id === manifest.recipe_id)) {
      throw new Error(`${manifest.recipe_id} did not survive tune_a`);
    }
    return;
  }
  if (phase === "holdout") {
    const freezePath = path.join(runRoot, "private", "winners.freeze.json");
    if (!fs.existsSync(freezePath)) throw new Error("holdout requires winners.freeze.json");
    const frozen = readJson(freezePath);
    const winner = frozen.winners?.find((row) => row.candidate_id === manifest.candidate_id);
    if (!winner || winner.recipe_id !== manifest.recipe_id) throw new Error(`${manifest.recipe_id} is not the frozen winner for ${manifest.candidate_id}`);
    if (winner.recipe_sha256 !== sha256(JSON.stringify(findRecipe(findCandidate(loadRecipes().value, manifest.candidate_id), manifest.recipe_id)))) {
      throw new Error(`${manifest.recipe_id} changed after tuning freeze`);
    }
    return;
  }
  throw new Error(`unsupported phase: ${phase}`);
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolve(address.port));
    });
  });
}

function executablePath() {
  const explicit = valueAfter("--llama-server");
  if (explicit) return path.resolve(explicit);
  const suffix = process.platform === "win32" ? "llama-server.exe" : "llama-server";
  const lane = process.platform === "win32" ? path.join("runtimes", "llama.cpp", "b10069", "cuda-12.4") : path.join("runtimes", "llama.cpp", "b10069", "metal");
  return path.join(modelRoot, lane, suffix);
}

async function waitForHealth(port, child) {
  const started = Date.now();
  while (Date.now() - started < 120_000) {
    if (child.exitCode !== null) throw new Error(`llama-server exited early with ${child.exitCode}`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(1000) });
      if (response.ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error("llama-server health timeout");
}

function spawnServer(manifest, port) {
  const exe = executablePath();
  if (!fs.existsSync(exe)) throw new Error(`llama-server is missing: ${exe}`);
  const logDir = path.join(runRoot, "logs", manifest.candidate_id, manifest.recipe_id);
  fs.mkdirSync(logDir, { recursive: true });
  const stdout = fs.openSync(path.join(logDir, `${phase}.stdout.log`), "a");
  const stderr = fs.openSync(path.join(logDir, `${phase}.stderr.log`), "a");
  const args = [
    "-m", manifest.model_path,
    "--alias", manifest.candidate_id,
    "--host", "127.0.0.1",
    "--port", String(port),
    "-c", "8192",
    "-np", "1",
    "-ngl", "999",
    "--reasoning", "off",
    "--no-webui",
  ];
  const child = childProcess.spawn(exe, args, {
    detached: true,
    windowsHide: true,
    stdio: ["ignore", stdout, stderr],
  });
  recordActiveServer(runRoot, {
    pid: child.pid,
    candidate_id: manifest.candidate_id,
    recipe_id: manifest.recipe_id,
    phase,
    executable: exe,
  });
  child.unref();
  return { child, exe, args, stdout, stderr };
}

function stopServer(server) {
  if (!server) return;
  let terminated = false;
  try {
    if (process.platform === "win32") {
      childProcess.execFileSync("taskkill.exe", ["/PID", String(server.child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
    } else {
      process.kill(-server.child.pid, "SIGTERM");
    }
    terminated = true;
  } catch {
    try { process.kill(server.child.pid, 0); } catch { terminated = true; }
  }
  for (const descriptor of [server.stdout, server.stderr]) {
    try { fs.closeSync(descriptor); } catch {}
  }
  if (terminated) clearActiveServer(runRoot, server.child.pid);
}

const RESOURCE_PROBE_TIMEOUT_MS = 5_000;
function runResourceProbe(executable, args, options = {}) {
  return childProcess.execFileSync(executable, args, {
    timeout: RESOURCE_PROBE_TIMEOUT_MS,
    killSignal: "SIGKILL",
    ...options,
  });
}

function processWorkingSet(pid) {
  try {
    if (process.platform === "win32") {
      const script = `(Get-Process -Id ${Number(pid)} -ErrorAction Stop).WorkingSet64`;
      return Number(runResourceProbe("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true }).trim());
    }
    return Number(runResourceProbe("ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8" }).trim()) * 1024;
  } catch { return null; }
}

function gpuMemory(pid) {
  if (process.platform !== "win32") return null;
  try {
    const output = runResourceProbe("nvidia-smi.exe", ["--query-compute-apps=pid,used_gpu_memory", "--format=csv,noheader,nounits"], { encoding: "utf8", windowsHide: true });
    const row = output.split(/\r?\n/).map((line) => line.split(",").map((part) => part.trim())).find(([rowPid]) => Number(rowPid) === Number(pid));
    return row ? Number(row[1]) * 1024 * 1024 : null;
  } catch { return null; }
}

function systemResourceSnapshot(pid = null) {
  const snapshot = {
    captured_at: new Date().toISOString(),
    os_total_memory_bytes: os.totalmem(),
    os_free_memory_bytes: os.freemem(),
    process_working_set_bytes: pid ? processWorkingSet(pid) : null,
    accelerator_memory_bytes: pid ? gpuMemory(pid) : null,
    swapout_pages: null,
    pagefile_used_bytes: null,
    thermal: null,
  };
  try {
    if (process.platform === "darwin") {
      const vmStat = runResourceProbe("vm_stat", [], { encoding: "utf8" });
      const pageSize = Number(vmStat.match(/page size of (\d+) bytes/)?.[1] ?? 4096);
      const pageouts = Number(vmStat.match(/^Pageouts:\s+(\d+)\./m)?.[1] ?? 0);
      snapshot.swapout_pages = pageouts;
      snapshot.swapout_bytes = pageouts * pageSize;
      snapshot.thermal = runResourceProbe("pmset", ["-g", "therm"], { encoding: "utf8" }).trim().slice(0, 1000);
    } else if (process.platform === "win32") {
      const script = "$m=(Get-CimInstance Win32_PageFileUsage|Measure-Object CurrentUsage -Sum).Sum;if($null -eq $m){0}else{[int64]$m*1MB}";
      snapshot.pagefile_used_bytes = Number(runResourceProbe("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true }).trim());
    }
  } catch {}
  return snapshot;
}

function completedKeys(file, manifestSha256) {
  if (!fs.existsSync(file)) return new Set();
  const rows = fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  if (rows.some((row) => row.manifest_sha256 !== manifestSha256)) {
    throw new Error(`resume manifest hash mismatch in ${file}; archive the stale result file before continuing`);
  }
  return new Set(rows.filter((row) => row.status === "ok" || row.status === "rejected").map((row) => row.row_id));
}

async function runManifest(file, manifest, recipe) {
  validatePhaseSelection(manifest);
  const outputFile = path.join(runRoot, "results", phase, `${manifest.candidate_id}__${manifest.recipe_id}.jsonl`);
  const manifestSha256 = fileHash(file);
  const done = completedKeys(outputFile, manifestSha256);
  const rows = limit > 0 ? manifest.rows.slice(0, limit) : manifest.rows;
  const pending = rows.filter((row) => !done.has(row.row_id));
  if (!pending.length) return { manifest: file, status: "already_complete", rows: rows.length };
  const port = await freePort();
  const resources = { before_load: systemResourceSnapshot() };
  const coldStartAt = performance.now();
  const server = spawnServer(manifest, port);
  let coldStartMs;
  let consecutiveErrors = 0;
  try {
    await waitForHealth(port, server.child);
    coldStartMs = performance.now() - coldStartAt;
    resources.model_resident = systemResourceSnapshot(server.child.pid);
    for (const row of pending) {
      const beforeWorkingSet = processWorkingSet(server.child.pid);
      const beforeGpu = gpuMemory(server.child.pid);
      const started = performance.now();
      let rawOutput = null;
      let responseBody = null;
      let failureStage = "transport";
      try {
        const response = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
          method: "POST",
          headers: { "content-type": "application/json", "authorization": "Bearer local" },
          body: JSON.stringify(row.request),
          signal: AbortSignal.timeout(120_000),
        });
        const responseText = await response.text();
        const generationMs = performance.now() - started;
        responseBody = JSON.parse(responseText);
        if (!response.ok) throw new Error(`HTTP ${response.status}: ${JSON.stringify(responseBody).slice(0, 500)}`);
        failureStage = "assistant_output";
        rawOutput = responseBody?.choices?.[0]?.message?.content;
        if (typeof rawOutput !== "string") throw new Error("response has no assistant content");
        failureStage = "materialization";
        const materializeAt = performance.now();
        const sourceCase = readCaseForRow(row.case_id);
        const materialized = materializeOutput(sourceCase.transcript, rawOutput, recipe.output_mode);
        const materializationMs = performance.now() - materializeAt;
        failureStage = "validation";
        const validationAt = performance.now();
        const flags = deterministicFlags(sourceCase.transcript, materialized, sourceCase.context);
        const validationMs = performance.now() - validationAt;
        const cleanupWallMs = performance.now() - started;
        const afterWorkingSet = processWorkingSet(server.child.pid);
        const afterGpu = gpuMemory(server.child.pid);
        appendJsonl(outputFile, {
          schema: "heardright.contextual_repair_result.v1",
          phase,
          row_id: row.row_id,
          case_id: row.case_id,
          candidate_id: manifest.candidate_id,
          recipe_id: manifest.recipe_id,
          output_mode: recipe.output_mode,
          status: "ok",
          captured_at: new Date().toISOString(),
          cold_start_ms: coldStartMs,
          generation_ms: generationMs,
          materialization_ms: materializationMs,
          validation_ms: validationMs,
          cleanup_wall_ms: cleanupWallMs,
          process_working_set_bytes: Math.max(beforeWorkingSet ?? 0, afterWorkingSet ?? 0) || null,
          gpu_memory_bytes: Math.max(beforeGpu ?? 0, afterGpu ?? 0) || null,
          raw_output: rawOutput,
          materialized_output: materialized,
          mechanical_flags: flags,
          usage: responseBody.usage ?? null,
          timings: responseBody.timings ?? null,
          request_sha256: sha256(JSON.stringify(row.request)),
          manifest_sha256: manifestSha256,
        });
        consecutiveErrors = 0;
        coldStartMs = null;
      } catch (error) {
        const status = classifyAttemptFailure(failureStage);
        const elapsedMs = performance.now() - started;
        appendJsonl(outputFile, {
          schema: "heardright.contextual_repair_result.v1",
          phase,
          row_id: row.row_id,
          case_id: row.case_id,
          candidate_id: manifest.candidate_id,
          recipe_id: manifest.recipe_id,
          output_mode: recipe.output_mode,
          status,
          captured_at: new Date().toISOString(),
          elapsed_ms: elapsedMs,
          cleanup_wall_ms: status === "rejected" ? elapsedMs : null,
          error: String(error?.message ?? error),
          raw_output: rawOutput,
          materialized_output: null,
          mechanical_flags: status === "rejected" ? ["materialization_error"] : [],
          request_sha256: sha256(JSON.stringify(row.request)),
          manifest_sha256: manifestSha256,
        });
        if (status === "rejected") {
          consecutiveErrors = 0;
          coldStartMs = null;
        } else {
          consecutiveErrors += 1;
          if (consecutiveErrors >= 3) throw new Error("candidate circuit opened after three consecutive infrastructure failures");
        }
      }
    }
    resources.before_unload = systemResourceSnapshot(server.child.pid);
  } finally {
    stopServer(server);
    resources.after_unload = systemResourceSnapshot();
    const resourceFile = path.join(runRoot, "resources", phase, `${manifest.candidate_id}__${manifest.recipe_id}.json`);
    writeJson(resourceFile, {
      schema: "heardright.contextual_repair_resources.v1",
      phase,
      candidate_id: manifest.candidate_id,
      recipe_id: manifest.recipe_id,
      manifest_sha256: manifestSha256,
      ...resources,
      swapout_delta_pages: resources.before_unload?.swapout_pages != null && resources.model_resident?.swapout_pages != null
        ? resources.before_unload.swapout_pages - resources.model_resident.swapout_pages
        : null,
      pagefile_delta_bytes: resources.before_unload?.pagefile_used_bytes != null && resources.model_resident?.pagefile_used_bytes != null
        ? resources.before_unload.pagefile_used_bytes - resources.model_resident.pagefile_used_bytes
        : null,
    });
  }
  return { manifest: file, status: "complete", rows: rows.length, output: outputFile };
}

let caseCache = null;
function readCaseForRow(caseId) {
  if (!caseCache) {
    const visible = phase === "holdout"
      ? path.join(runRoot, "candidate-visible", "holdout", "cases.holdout.jsonl")
      : [path.join(runRoot, "candidate-visible", "tune", "cases.tune_a.jsonl"), path.join(runRoot, "candidate-visible", "tune", "cases.tune_b.jsonl")];
    const files = Array.isArray(visible) ? visible : [visible];
    caseCache = new Map(files.flatMap((file) => fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse)).map((row) => [row.case_id, row]));
  }
  const value = caseCache.get(caseId);
  if (!value) throw new Error(`case input missing: ${caseId}`);
  return value;
}

function prepareHoldout(recipes) {
  const freezePath = path.join(runRoot, "private", "winners.freeze.json");
  const sealedPath = path.join(runRoot, "private", "sealed-holdout.jsonl");
  if (!fs.existsSync(freezePath)) throw new Error("cannot prepare holdout before winners.freeze.json exists");
  const frozen = readJson(freezePath);
  const sealed = fs.readFileSync(sealedPath, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
  const holdoutRoot = path.join(runRoot, "candidate-visible", "holdout");
  fs.rmSync(holdoutRoot, { recursive: true, force: true });
  fs.mkdirSync(holdoutRoot, { recursive: true });
  const cases = sealed.map(({ private_reference: _private, ...visible }) => visible);
  fs.writeFileSync(path.join(holdoutRoot, "cases.holdout.jsonl"), `${cases.map(JSON.stringify).join("\n")}\n`);
  const manifests = [];
  for (const winner of frozen.winners ?? []) {
    const candidate = findCandidate(recipes, winner.candidate_id);
    const recipe = findRecipe(candidate, winner.recipe_id);
    if (winner.recipe_sha256 !== sha256(JSON.stringify(recipe))) throw new Error(`${winner.recipe_id} no longer matches the frozen hash`);
    const tuneManifestFile = path.join(runRoot, "candidate-visible", "tune", "manifests", candidate.id, `${recipe.id}.tune_a.json`);
    const tuneManifest = readJson(tuneManifestFile);
    const manifest = {
      ...tuneManifest,
      phase: "holdout",
      rows: cases.map((captured) => ({
        row_id: `${candidate.id}__${recipe.id}__${captured.case_id}`,
        case_id: captured.case_id,
        input_sha256: captured.input_sha256,
        request: buildRequest(candidate, recipe, captured),
      })),
    };
    const file = path.join(holdoutRoot, "manifests", candidate.id, `${recipe.id}.holdout.json`);
    writeJson(file, manifest);
    manifests.push({ candidate_id: candidate.id, recipe_id: recipe.id, rows: manifest.rows.length, file, sha256: fileHash(file) });
  }
  writeJson(path.join(runRoot, "private", "holdout-manifest-ledger.json"), { created_at: new Date().toISOString(), manifests });
  console.log(JSON.stringify({ status: "holdout_prepared_not_run", candidates: manifests.length, rows: manifests.reduce((sum, item) => sum + item.rows, 0), holdout_root: holdoutRoot }, null, 2));
}

async function main() {
  if (process.argv.includes("--resource-probe-smoke")) {
    const count = Number(valueAfter("--probe-count", "2"));
    if (!Number.isInteger(count) || count < 1 || count > 5) throw new Error("probe count must be an integer from 1 to 5");
    const samples = Array.from({ length: count }, () => systemResourceSnapshot(process.pid));
    console.log(JSON.stringify({ status: "resource_probe_smoke_ok", timeout_ms: RESOURCE_PROBE_TIMEOUT_MS, samples }));
    return;
  }
  const control = loadControl();
  const recipes = loadRecipes();
  if (control.value.recipes_sha256 !== fileHash(recipes.file)) throw new Error("prepared recipe snapshot hash mismatch");
  if (process.argv.includes("--prepare-holdout")) {
    prepareHoldout(recipes.value);
    return;
  }
  const files = manifestFiles();
  if (!files.length) throw new Error("no manifests match the requested filters");
  const plans = files.map((file) => {
    const manifest = readJson(file);
    const { candidate, recipe } = validateManifest(file, manifest, recipes.value);
    return {
      file,
      manifest,
      candidate,
      recipe,
      planned_rows: limit > 0 ? Math.min(limit, manifest.rows.length) : manifest.rows.length,
    };
  });
  if (process.argv.includes("--dry-run")) {
    console.log(JSON.stringify({
      status: "dry_run_no_generation",
      phase,
      run_root: runRoot,
      plans: plans.map((plan) => ({ candidate_id: plan.manifest.candidate_id, recipe_id: plan.manifest.recipe_id, output_mode: plan.recipe.output_mode, rows: plan.planned_rows, model_bytes: plan.manifest.model_bytes })),
    }, null, 2));
    return;
  }
  if (!process.argv.includes("--live")) throw new Error("choose --dry-run, --prepare-holdout, or --live");
  if (plans.length !== 1) throw new Error("live mode requires filters that resolve to exactly one candidate recipe manifest");
  validateApproval(control.file, recipes.file);
  validateRuntimeHashes(control.value, { nodeVersion: process.version, llamaServer: executablePath() });
  const gpuLock = acquireOrAdoptRunLock(runRoot);
  try {
    reapActiveServer(runRoot);
    const results = [];
    for (const plan of plans) results.push(await runManifest(plan.file, plan.manifest, plan.recipe));
    console.log(JSON.stringify({ status: "live_phase_complete", phase, results }, null, 2));
  } finally {
    reapActiveServer(runRoot);
    gpuLock.release();
  }
}

main().catch((error) => {
  console.error(error.stack ?? String(error));
  process.exitCode = 1;
});
