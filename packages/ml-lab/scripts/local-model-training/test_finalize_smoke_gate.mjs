import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import test from "node:test";
import { evaluateSmokeOutput, finalize, finalizeRecovery, validateSmokeGateReceipt } from "./finalize_smoke_gate.mjs";

const LANES = ["falcon", "granite", "gemma", "sotto"];
const shaBuffer = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const sha = (path) => shaBuffer(readFileSync(path));
const writeJson = (path, value) => { mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`); };
const receiptName = (id) => `${id.replaceAll(":", "--")}.json`;
const resign = (receipt) => { const { signature: _signature, ...payload } = receipt; return { ...payload, signature: { algorithm: "sha256", payload_sha256: shaBuffer(Buffer.from(JSON.stringify(payload))) } }; };

function buildPacket(root) {
  const packet = resolve(root, "packet"), scripts = resolve(packet, "scripts", "local-model-training"), data = resolve(packet, "data");
  mkdirSync(scripts, { recursive: true }); mkdirSync(data, { recursive: true });
  writeJson(resolve(packet, "control.json"), { schema_version: "test.control.v1", runtime_files: {} });
  writeJson(resolve(scripts, "model-recipes.json"), { schema_version: "test.recipes.v1", families: {} });
  writeFileSync(resolve(scripts, "requirements.vast-cu126.pins.txt"), "torch==test\n");
  writeFileSync(resolve(scripts, "orchestrate.mjs"), "export const version = 'old';\n");
  writeFileSync(resolve(data, "smoke-2.jsonl"), "{\"id\":1}\n{\"id\":2}\n");
  const files = [resolve(packet, "control.json"), resolve(scripts, "model-recipes.json"), resolve(scripts, "requirements.vast-cu126.pins.txt"), resolve(scripts, "orchestrate.mjs"), resolve(data, "smoke-2.jsonl")].map((path) => ({ path: relative(packet, path).replaceAll("\\", "/"), bytes: statSync(path).size, sha256: sha(path) }));
  writeJson(resolve(packet, "bundle-manifest.json"), { schema_version: "test.bundle.v1", control_sha256: sha(resolve(packet, "control.json")), files });
  return packet;
}

function rebuildPacketManifest(packet) {
  const prior = JSON.parse(readFileSync(resolve(packet, "bundle-manifest.json"), "utf8"));
  const paths = prior.files.map((item) => item.path);
  prior.control_sha256 = sha(resolve(packet, "control.json"));
  prior.files = paths.map((path) => {
    const local = resolve(packet, ...path.split("/"));
    return { path, bytes: statSync(local).size, sha256: sha(local) };
  });
  writeJson(resolve(packet, "bundle-manifest.json"), prior);
}

function stage(run, id) {
  const output = resolve(run, "outputs", `${id.replaceAll(":", "--")}.json`), receipt = resolve(run, "stage-receipts", receiptName(id));
  writeJson(output, { id, status: "ok" });
  writeJson(receipt, { schema_version: "heardright.l1.stage_receipt.v1", id, outputs: { [output]: sha(output) } });
  return { id, output, receipt, receipt_sha256: sha(receipt) };
}

function fixture({ unsupportedLane = null, noSafeGemma = false } = {}) {
  const root = mkdtempSync(join(tmpdir(), "heardright-smoke-gate-")), packet = buildPacket(root), smoke = resolve(root, "smoke"), run = resolve(smoke, "run"), runtime = resolve(smoke, "runtime");
  mkdirSync(run, { recursive: true }); mkdirSync(runtime, { recursive: true });
  const runtimeReceipt = resolve(runtime, "vast-runtime-receipt.json"); writeJson(runtimeReceipt, { schema_version: "test.runtime.v1", status: "ready" });
  const events = [], lanes = {};
  for (const laneId of LANES) {
    const sourceRejected = laneId === "gemma" && noSafeGemma;
    const ids = sourceRejected ? ["gemma:prepare:stock", "gemma:evaluate:stock", "gemma:prepare:grmr", "gemma:evaluate:grmr", "gemma:select-source", "gemma:release-gpu"] : [`${laneId}:backend-preflight:full_parameter`, `${laneId}:release-gpu`];
    if (!sourceRejected && laneId !== unsupportedLane) ids.splice(1, 0, `${laneId}:evaluate:baseline`, `${laneId}:train:full_parameter:epoch-1`, `${laneId}:evaluate:full_parameter:epoch-1`);
    const receipts = Object.fromEntries(ids.map((id) => [id, stage(run, id)]));
    for (const id of ids) events.push({ kind: "stage_started", id }, { kind: "stage_completed", id });
    const backend = receipts[`${laneId}:backend-preflight:full_parameter`], cleanup = receipts[`${laneId}:release-gpu`];
    lanes[laneId] = {
      status: sourceRejected ? "rejected_no_safe_source" : laneId === unsupportedLane ? "unsupported_for_full_weight_training" : "rejected_no_safe_checkpoint",
      stages: ids,
      ...(backend ? { backend_preflight: { receipt: backend.receipt, receipt_sha256: backend.receipt_sha256 } } : {}),
      cleanup: { receipt: cleanup.receipt, receipt_sha256: cleanup.receipt_sha256 },
    };
    if (sourceRejected) {
      const selected = receipts["gemma:select-source"], stock = receipts["gemma:evaluate:stock"], grmr = receipts["gemma:evaluate:grmr"];
      lanes[laneId].source_preflight = { status: "rejected_no_safe_source", selected_source: null, artifact: selected.output, artifact_sha256: sha(selected.output), receipt: selected.receipt, receipt_sha256: selected.receipt_sha256, evaluations: [
        { candidate: "stock", vector: [2, 1], evaluation_artifact: stock.output, evaluation_sha256: sha(stock.output), evaluation_receipt: stock.receipt, receipt_sha256: stock.receipt_sha256 },
        { candidate: "grmr", vector: [1, 0], evaluation_artifact: grmr.output, evaluation_sha256: sha(grmr.output), evaluation_receipt: grmr.receipt, receipt_sha256: grmr.receipt_sha256 },
      ] };
    }
  }
  const state = {
    schema_version: "heardright.l1.orchestrator_state.v4", status: "completed_no_viable_candidates", mode: "live", profile: "smoke",
    active_stage: null, active_lock: null, lane_order: LANES, lanes, events,
    packet_binding: { manifest_sha256: sha(resolve(packet, "bundle-manifest.json")), control_sha256: sha(resolve(packet, "control.json")) },
    dataset_binding: { training_sha256: sha(resolve(packet, "data", "smoke-2.jsonl")), evaluation_sha256: sha(resolve(packet, "data", "smoke-2.jsonl")) },
    training_contract: { mode: "full", lora: false },
    recipes_sha256: sha(resolve(packet, "scripts", "local-model-training", "model-recipes.json")),
    runtime_binding: { receipt: runtimeReceipt, receipt_sha256: sha(runtimeReceipt), requirements_sha256: sha(resolve(packet, "scripts", "local-model-training", "requirements.vast-cu126.pins.txt")) },
  };
  const statePath = resolve(run, "state.json"); writeJson(statePath, state);
  return { root, packet, smoke, run, statePath, state, cleanup: () => rmSync(root, { recursive: true, force: true }) };
}

function withFixture(callback, options) {
  const item = fixture(options);
  try { return callback(item); } finally { item.cleanup(); }
}

test("completed_no_viable_candidates passes infrastructure and remains quality-neutral", () => withFixture(({ smoke, packet }) => {
  const result = evaluateSmokeOutput(smoke, packet);
  assert.equal(result.state.status, "completed_no_viable_candidates");
  assert.equal(result.lanes.every((lane) => lane.model_quality_success === false && lane.infrastructure_supported_rejection === true), true);
}));

test("explicit unsupported backend may terminate after preflight and cleanup", () => withFixture(({ smoke, packet }) => {
  const result = evaluateSmokeOutput(smoke, packet);
  assert.equal(result.lanes.find((lane) => lane.lane === "gemma").disposition, "unsupported_for_full_weight_training");
}, { unsupportedLane: "gemma" }));

test("Gemma no-safe-source rejection passes with preflight and cleanup but no train or export", () => withFixture(({ smoke, packet }) => {
  const result = evaluateSmokeOutput(smoke, packet), gemma = result.lanes.find((lane) => lane.lane === "gemma");
  assert.equal(gemma.disposition, "rejected_no_safe_source");
  assert.equal(gemma.required_stage_receipts.some(({ id }) => /gemma:(?:train|export|backend-preflight)/u.test(id)), false);
  assert.deepEqual(gemma.required_stage_receipts.map(({ id }) => id), ["gemma:prepare:stock", "gemma:evaluate:stock", "gemma:prepare:grmr", "gemma:evaluate:grmr", "gemma:select-source", "gemma:release-gpu"]);
}, { noSafeGemma: true }));

test("finalizer emits a signed packet-bound receipt", () => withFixture(({ root, smoke, packet }) => {
  const target = resolve(root, "gate.json"), generated = finalize(smoke, packet, target), validated = validateSmokeGateReceipt(target, packet);
  assert.equal(generated.status, "infrastructure_pass");
  assert.equal(validated.sha256, generated.sha256);
}));

test("a recovery seal permits only hash-enumerated operational patches and preserves the original authorization receipt", () => withFixture(({ root, smoke, packet }) => {
  const parentReceipt = resolve(root, "parent-gate.json");
  finalize(smoke, packet, parentReceipt);
  const parentSha = sha(parentReceipt);
  const target = resolve(root, "target-packet");
  cpSync(packet, target, { recursive: true });
  const orchestrator = resolve(target, "scripts", "local-model-training", "orchestrate.mjs");
  writeFileSync(orchestrator, "export const version = 'resume-contract-fix';\n");
  const watchdog = resolve(target, "scripts", "local-model-training", "paid_run_watchdog.py");
  writeFileSync(watchdog, "print('fail-closed billing guard')\n");
  const targetManifest = JSON.parse(readFileSync(resolve(target, "bundle-manifest.json"), "utf8"));
  targetManifest.files.push({ path: "scripts/local-model-training/paid_run_watchdog.py", bytes: statSync(watchdog).size, sha256: sha(watchdog) });
  writeJson(resolve(target, "bundle-manifest.json"), targetManifest);
  const controlPath = resolve(target, "control.json");
  const control = JSON.parse(readFileSync(controlPath, "utf8"));
  control.runtime_files["orchestrate.mjs"] = sha(orchestrator);
  writeJson(controlPath, control);
  rebuildPacketManifest(target);
  const recoveryReceipt = resolve(target, "authorization", "recovery-smoke-gate-receipt.json");
  const recovered = finalizeRecovery(parentReceipt, packet, target, recoveryReceipt, "resume argument contract");
  const validated = validateSmokeGateReceipt(parentReceipt, target);
  assert.equal(sha(parentReceipt), parentSha);
  assert.equal(validated.recovery_receipt_sha256, recovered.sha256);
  assert.deepEqual(JSON.parse(readFileSync(recoveryReceipt, "utf8")).changed_files.map((item) => item.path), [
    "control.json",
    "scripts/local-model-training/orchestrate.mjs",
    "scripts/local-model-training/paid_run_watchdog.py",
  ]);
}));

test("tampered stage output is rejected", () => withFixture(({ smoke, packet, run }) => {
  writeJson(resolve(run, "outputs", "falcon--evaluate--baseline.json"), { tampered: true });
  assert.throws(() => evaluateSmokeOutput(smoke, packet), /stage output hash mismatch/);
}));

test("missing required stage receipt is rejected", () => withFixture(({ smoke, packet, run }) => {
  rmSync(resolve(run, "stage-receipts", receiptName("granite:evaluate:baseline")));
  assert.throws(() => evaluateSmokeOutput(smoke, packet), /stage receipt is missing/);
}));

test("partial lane roster is rejected", () => withFixture(({ smoke, packet, statePath, state }) => {
  delete state.lanes.sotto; state.lane_order = state.lane_order.slice(0, -1); writeJson(statePath, state);
  assert.throws(() => evaluateSmokeOutput(smoke, packet), /exactly the requested default-four lanes/);
}));

test("wrong packet binding is rejected", () => withFixture(({ smoke, packet, statePath, state }) => {
  state.packet_binding.manifest_sha256 = `sha256:${"0".repeat(64)}`; writeJson(statePath, state);
  assert.throws(() => evaluateSmokeOutput(smoke, packet), /different packet or control/);
}));

test("non-terminal overall status is rejected", () => withFixture(({ smoke, packet, statePath, state }) => {
  state.status = "running"; writeJson(statePath, state);
  assert.throws(() => evaluateSmokeOutput(smoke, packet), /not an accepted terminal infrastructure state/);
}));

test("active stage is rejected", () => withFixture(({ smoke, packet, statePath, state }) => {
  state.active_stage = { id: "falcon:train:full_parameter:epoch-2" }; writeJson(statePath, state);
  assert.throws(() => evaluateSmokeOutput(smoke, packet), /active stage, lock, or system\/stage failure/);
}));

test("LoRA evidence is rejected", () => withFixture(({ smoke, packet, statePath, state }) => {
  state.training_contract.lora = true; writeJson(statePath, state);
  assert.throws(() => evaluateSmokeOutput(smoke, packet), /no-LoRA full-weight contract/);
}));

test("tampered signed receipt is rejected", () => withFixture(({ root, smoke, packet }) => {
  const target = resolve(root, "gate.json"); finalize(smoke, packet, target);
  const receipt = JSON.parse(readFileSync(target, "utf8")); receipt.status = "tampered"; writeJson(target, receipt);
  assert.throws(() => validateSmokeGateReceipt(target, packet), /signature or schema is invalid/);
}));

test("re-signed target packet tamper is rejected", () => withFixture(({ root, smoke, packet }) => {
  const target = resolve(root, "gate.json"); finalize(smoke, packet, target);
  const receipt = JSON.parse(readFileSync(target, "utf8")); receipt.target_full_packet.manifest_sha256 = `sha256:${"0".repeat(64)}`; writeJson(target, resign(receipt));
  assert.throws(() => validateSmokeGateReceipt(target, packet), /different target packet/);
}));

test("re-signed source evidence tamper is rejected", () => withFixture(({ root, smoke, packet }) => {
  const target = resolve(root, "gate.json"); finalize(smoke, packet, target);
  const receipt = JSON.parse(readFileSync(target, "utf8")); receipt.source_smoke_packet.smoke_dataset_sha256 = `sha256:${"0".repeat(64)}`; writeJson(target, resign(receipt));
  assert.throws(() => validateSmokeGateReceipt(target, packet), /source evidence hashes do not match/);
}));

const V11_EVIDENCE = "C:\\Users\\adrds\\Downloads\\heardright-l1-vast-training-20260722-private-fullft-v11-smoke-final-evidence\\extracted";
const V11_PACKET = "C:\\Users\\adrds\\Downloads\\heardright-l1-vast-training-20260722-private-fullft-v11";
const V13_PACKET = "C:\\Users\\adrds\\Downloads\\heardright-l1-vast-training-20260722-private-fullft-v13";
test("exact archived v11 positive evidence passes the infrastructure bridge", { skip: !existsSync(V11_EVIDENCE) || !existsSync(V11_PACKET) }, () => {
  withFixture(({ packet: syntheticTarget }) => {
    const target = existsSync(V13_PACKET) ? V13_PACKET : syntheticTarget;
    const result = evaluateSmokeOutput(V11_EVIDENCE, target);
    assert.equal(result.sourcePacket.manifest_sha256, "sha256:03772bca5a1272d10155fdd79082244c8d6b215034cee47b5dc8bd90707c7ab0");
    assert.notEqual(result.targetPacket.manifest_sha256, result.sourcePacket.manifest_sha256);
    assert.equal(result.lanes.length, 4);
  });
});
