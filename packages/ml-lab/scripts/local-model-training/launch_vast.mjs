#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { main as bootstrap } from "./bootstrap_vast.mjs";
import { validateSmokeGateReceipt } from "./finalize_smoke_gate.mjs";

const ROOT = dirname(fileURLToPath(import.meta.url));
export const RECOVERED_CPYTHON = "/workspace/heardright-runtime-cpython311/bin/python3";
const value = (argv, name) => { const index = argv.indexOf(name); return index < 0 ? undefined : argv[index + 1]; };
const fail = (message) => { throw new Error(`Vast launcher: ${message}`); };

export function parseLaunch(argv, { packetDir = resolve(ROOT, "..", "..") } = {}) {
  const profile = value(argv, "--profile"), authorization = value(argv, "--authorization"), output = value(argv, "--output-dir");
  if (!["smoke", "full"].includes(profile)) fail("--profile must be smoke or full");
  if (!authorization || !output) fail("--authorization and --output-dir are required");
  const authPath = resolve(authorization);
  if (!existsSync(authPath)) fail(`authorization is missing: ${authPath}`);
  const approval = JSON.parse(readFileSync(authPath, "utf8"));
  if (approval.approved !== true || approval.scope !== "heardright_l1_four_family_training" || approval.profile !== profile) fail("authorization must approve the exact scope and profile");
  if (typeof approval.offer_id !== "string" || !approval.offer_id.trim() || !(approval.hourly_rate_usd > 0)) fail("authorization must bind the exact Vast offer and hourly rate");
  let smokeGate = null;
  if (profile === "full") {
    if (!approval.smoke_gate_receipt || !/^sha256:[0-9a-f]{64}$/.test(approval.smoke_gate_receipt_sha256 ?? "")) fail("full authorization requires smoke_gate_receipt and smoke_gate_receipt_sha256");
    smokeGate = validateSmokeGateReceipt(approval.smoke_gate_receipt, packetDir);
    if (smokeGate.sha256 !== approval.smoke_gate_receipt_sha256) fail("full authorization smoke gate receipt hash mismatch");
  }
  const data = resolve(packetDir, "data"), bundle = resolve(packetDir), privateOutput = resolve(output);
  const runtimeDir = process.env.RIGHTKIT_ML_RUNTIME_DIR || "/workspace/heardright-v15-shared-runtime";
  const training = resolve(data, profile === "smoke" ? "smoke-2.jsonl" : "training-1500.jsonl"), evaluation = resolve(data, profile === "smoke" ? "smoke-2.jsonl" : "development-100.jsonl");
  const preflight = resolve(data, "gemma-preflight-30.jsonl");
  for (const path of [training, evaluation, ...(profile === "full" ? [preflight] : [])]) if (!existsSync(path)) fail(`frozen data is missing: ${path}`);
  const python = process.env.RIGHTKIT_ML_PYTHON || RECOVERED_CPYTHON;
  const orchestratorArgs = ["--live", "--profile", profile, "--python", python, "--live-authorization", authPath, "--output-dir", resolve(privateOutput, "run"), "--training-input", training, "--eval-input", evaluation, "--evaluation-policy", resolve(bundle, "evaluation-policy.v2.json"), "--evaluation-policy-control", resolve(bundle, "evaluation-policy-control.v1.json")];
  if (profile === "full") orchestratorArgs.push("--gemma-preflight-input", preflight);
  if (argv.includes("--include-opt-in-candidates")) orchestratorArgs.push("--include-opt-in-candidates");
  return { profile, authorization: authPath, output: privateOutput, runtimeDir, smokeGate, orchestratorArgs };
}

export async function main(argv = process.argv.slice(2)) {
  const launch = parseLaunch(argv);
  const instanceId = process.env.CONTAINER_ID;
  if (!instanceId) fail("CONTAINER_ID is required so the paid-run watchdog can stop billing");
  const guard = spawnSync(launch.orchestratorArgs[launch.orchestratorArgs.indexOf("--python") + 1], [
    resolve(ROOT, "paid_run_watchdog.py"),
    "--install-supervisor",
    "--run-root", resolve(launch.output, "run"),
    "--command-token", launch.output,
    "--instance-id", instanceId,
  ], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  if (guard.error || guard.status !== 0) fail(`paid-run watchdog installation failed: ${guard.stderr || guard.error?.message || guard.status}`);
  process.stdout.write(guard.stdout);
  await bootstrap(["--output-dir", launch.runtimeDir, "--llama-cpp-dir", resolve(launch.runtimeDir, "tools", "llama.cpp"), "--orchestrator", resolve(ROOT, "orchestrate.mjs"), "--invoke-orchestrator", "--orchestrator-args-json", JSON.stringify(launch.orchestratorArgs)]);
}

if (process.argv[1] && !process.execArgv.includes("-e") && (process.platform === "win32" ? basename(process.argv[1]).toLowerCase() === basename(fileURLToPath(import.meta.url)).toLowerCase() : basename(process.argv[1]) === basename(fileURLToPath(import.meta.url)))) main().catch((error) => { process.stderr.write(`${error.stack || error.message}\n`); process.exitCode = 1; });
