#!/usr/bin/env node
import assert from "node:assert/strict";
import childProcess from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDir = path.dirname(fileURLToPath(new URL(import.meta.url)));
const supervisorPath = path.join(scriptDir, "run_contextual_repair_overnight.mjs");
const windowsLauncherPath = path.join(scriptDir, "start_contextual_repair_overnight_windows.ps1");
const candidateRunnerPath = path.join(scriptDir, "run_contextual_repair_screen.mjs");
const phaseRunnerPath = path.join(scriptDir, "run_contextual_repair_phase.mjs");

assert.ok(fs.existsSync(supervisorPath), "overnight supervisor is missing");
assert.ok(fs.existsSync(windowsLauncherPath), "detached Windows overnight launcher is missing");
const candidateRunnerSource = fs.readFileSync(candidateRunnerPath, "utf8");
const phaseRunnerSource = fs.readFileSync(phaseRunnerPath, "utf8");
assert.match(candidateRunnerSource, /detached:\s*true/, "llama-server must use an independent process group");
assert.match(candidateRunnerSource, /windowsHide:\s*true/, "llama-server must stay hidden on Windows");
assert.match(candidateRunnerSource, /taskkill\.exe[\s\S]*?\/T[\s\S]*?\/F/, "llama-server process-tree cleanup is missing");
assert.match(candidateRunnerSource, /RESOURCE_PROBE_TIMEOUT_MS\s*=\s*5_000/, "resource probes need a five-second bound");
assert.match(candidateRunnerSource, /function runResourceProbe[\s\S]*?timeout:\s*RESOURCE_PROBE_TIMEOUT_MS[\s\S]*?killSignal:\s*"SIGKILL"/, "resource probe timeout helper is incomplete");
const resourceProbeSection = candidateRunnerSource.slice(
  candidateRunnerSource.indexOf("function processWorkingSet"),
  candidateRunnerSource.indexOf("function completedKeys"),
);
assert.doesNotMatch(resourceProbeSection, /childProcess\.execFileSync/, "resource probes must use the bounded helper");
assert.match(candidateRunnerSource, /acquireOrAdoptRunLock/, "standalone candidate runner must honor the shared GPU lock");
assert.match(candidateRunnerSource, /validateRuntimeHashes/, "standalone candidate runner must validate frozen runtime hashes");
assert.match(phaseRunnerSource, /acquireOrAdoptRunLock/, "standalone phase runner must honor the shared GPU lock");
assert.match(phaseRunnerSource, /validateRuntimeHashes/, "standalone phase runner must validate frozen runtime hashes");

const probeSmoke = childProcess.spawnSync(process.execPath, [candidateRunnerPath, "--resource-probe-smoke", "--probe-count", "2"], {
  encoding: "utf8",
  timeout: 30_000,
  windowsHide: true,
});
assert.equal(probeSmoke.status, 0, probeSmoke.stderr);
const probeSmokeResult = JSON.parse(probeSmoke.stdout.trim().split(/\r?\n/).at(-1));
assert.equal(probeSmokeResult.status, "resource_probe_smoke_ok");
assert.equal(probeSmokeResult.samples.length, 2);

const {
  OVERNIGHT_STEPS,
  acquireOrAdoptRunLock,
  acquireRunLock,
  clearActiveServer,
  phaseCoverage,
  recordActiveServer,
  validateOvernightApproval,
  validateRuntimeHashes,
} = await import(pathToFileURL(supervisorPath).href);

assert.deepEqual(OVERNIGHT_STEPS.map((step) => step.id), [
  "tune_a",
  "score_tune_a",
  "report_tune_a",
  "tune_b",
  "freeze_winners",
  "report_tune_b",
  "prepare_holdout",
  "holdout",
  "report_holdout",
  "verify_complete",
]);

const approval = {
  approved: true,
  approved_by: "Adrian",
  approved_at: "2026-07-21T00:00:00.000Z",
  control_sha256: "control",
  recipes_sha256: "recipes",
  permitted_phases: ["tune_a", "tune_b", "holdout"],
};
assert.doesNotThrow(() => validateOvernightApproval(approval, "control", "recipes"));
assert.throws(
  () => validateOvernightApproval({ ...approval, permitted_phases: ["tune_a", "tune_b"] }, "control", "recipes"),
  /holdout/,
);

assert.equal(typeof validateRuntimeHashes, "function", "runtime hash validator is missing");
const runtimeFile = path.join(os.tmpdir(), `heardright-runtime-${process.pid}.txt`);
const alternateRuntimeFile = path.join(os.tmpdir(), `heardright-runtime-alternate-${process.pid}.txt`);
fs.writeFileSync(runtimeFile, "approved runtime\n");
fs.writeFileSync(alternateRuntimeFile, "alternate runtime\n");
const runtimeControl = { node_version: process.version, runtime_files: [{ role: "llama_server", file: runtimeFile, sha256: crypto.createHash("sha256").update(fs.readFileSync(runtimeFile)).digest("hex") }] };
assert.doesNotThrow(() => validateRuntimeHashes(runtimeControl, { nodeVersion: process.version, llamaServer: runtimeFile }));
assert.throws(
  () => validateRuntimeHashes(runtimeControl, { nodeVersion: process.version, llamaServer: alternateRuntimeFile }),
  /unapproved llama-server/,
);
fs.writeFileSync(runtimeFile, "changed runtime\n");
assert.throws(() => validateRuntimeHashes(runtimeControl), /runtime hash mismatch/);
fs.rmSync(runtimeFile, { force: true });
fs.rmSync(alternateRuntimeFile, { force: true });

const coverage = phaseCoverage(
  [{ row_id: "a" }, { row_id: "b" }],
  [
    { row_id: "a", status: "error" },
    { row_id: "a", status: "ok" },
    { row_id: "b", status: "ok" },
  ],
);
assert.deepEqual(coverage, { expected: 2, completed: 2, ok: 2, rejected: 0, missing_or_failed: [] });
assert.deepEqual(
  phaseCoverage([{ row_id: "a" }, { row_id: "b" }], [{ row_id: "a", status: "ok" }]).missing_or_failed,
  ["b"],
);
assert.deepEqual(
  phaseCoverage([{ row_id: "a" }], [{ row_id: "a", status: "rejected" }]),
  { expected: 1, completed: 1, ok: 0, rejected: 1, missing_or_failed: [] },
  "a deterministic quality rejection is a terminal attempt and must not be regenerated",
);

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "heardright-overnight-test-"));
const first = acquireRunLock(tempRoot, process.pid);
assert.equal(typeof first.token, "string");
assert.throws(() => acquireRunLock(tempRoot, process.pid), /already active/);
const adopted = acquireOrAdoptRunLock(tempRoot, process.pid, first.token);
assert.equal(adopted.adopted, true);
adopted.release();
assert.ok(fs.existsSync(first.file), "an adopted child must not release its parent's lock");
first.release();
assert.doesNotThrow(() => acquireRunLock(tempRoot, process.pid).release());
assert.equal(typeof recordActiveServer, "function", "active server PID recorder is missing");
assert.equal(typeof clearActiveServer, "function", "active server PID clearer is missing");
const activeState = recordActiveServer(tempRoot, { pid: 123456789, candidate_id: "candidate", recipe_id: "recipe", executable: "llama-server" });
assert.ok(fs.existsSync(activeState));
clearActiveServer(tempRoot, 123456789);
assert.ok(!fs.existsSync(activeState));
fs.rmSync(tempRoot, { recursive: true, force: true });

console.log(JSON.stringify({ status: "ok", steps: OVERNIGHT_STEPS.length, sequential_gpu_jobs: true, resumable: true }));
