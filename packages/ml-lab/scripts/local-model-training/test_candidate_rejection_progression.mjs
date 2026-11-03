import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { candidateDisposition, executeDispositionActions, gemmaSourceDisposition, gemmaSourceDispositionFromFiles, resumeCheckpointFromManifest, runFamiliesSequentially, settleGemmaSourceRejection, summarizeRunStatus } from "./orchestrate.mjs";

const sha = (path) => `sha256:${createHash("sha256").update(readFileSync(path)).digest("hex")}`;
const attempt = (epoch, hardFailures, misses = 0) => ({ epoch, checkpoint_manifest: `checkpoint-${epoch}.json`, evaluation: `evaluation-${epoch}.json`, vector: [hardFailures, misses] });
const digest = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const gemmaCandidate = (name, vector) => ({ name, source: { id: `${name}/model`, revision: `${name}-revision` }, evaluation: { path: `${name}-evaluation.json`, sha256: digest(`${name}-evaluation`), receipt: `${name}-receipt.json`, receipt_sha256: digest(`${name}-receipt`), vector } });

test("both unsafe Gemma sources retain the least-unsafe hash-bound training source", () => {
  const stock = gemmaCandidate("stock", [2, 1]), grmr = gemmaCandidate("grmr", [1, 0]);
  const result = gemmaSourceDisposition({ stock, grmr });
  assert.equal(result.kind, "provisional_selection");
  assert.equal(result.status, "selected_least_unsafe_for_training");
  assert.equal(result.selected_candidate, "grmr");
  assert.deepEqual(result.selected_source, grmr.source);
  assert.deepEqual(result.evaluations.map((item) => item.vector), [[2, 1], [1, 0]]);
  assert.deepEqual(result.evaluations.map((item) => item.evaluation_sha256), [stock.evaluation.sha256, grmr.evaluation.sha256]);
  assert.deepEqual(result.evaluations.map((item) => item.receipt_sha256), [stock.evaluation.receipt_sha256, grmr.evaluation.receipt_sha256]);
});

test("epoch search continues after a hard-invalid checkpoint so later epochs remain eligible", () => {
  const source = readFileSync(new URL("./orchestrate.mjs", import.meta.url), "utf8");
  assert.doesNotMatch(source, /if \(!improved\) break;/u);
  assert.match(source, /priorManifest = epochManifest;/u);
});

test("epoch resume resolves the populated checkpoint child rather than its manifest parent", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-resume-checkpoint-"));
  try {
    const epoch = join(root, "full-epoch-1"), checkpoint = join(epoch, "checkpoint-94"), manifest = join(epoch, "train.manifest.json");
    mkdirSync(checkpoint, { recursive: true });
    writeFileSync(join(checkpoint, "trainer_state.json"), "{}");
    writeFileSync(manifest, JSON.stringify({ checkpoint_dir: join(epoch, "checkpoint") }));
    assert.equal(resumeCheckpointFromManifest(manifest), checkpoint);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("one safe Gemma source preserves normal selection", () => {
  const grmrOnly = gemmaSourceDisposition({ stock: gemmaCandidate("stock", [1, 0]), grmr: gemmaCandidate("grmr", [0, 3]) });
  assert.equal(grmrOnly.kind, "selection_required");
  assert.deepEqual(grmrOnly.safe_candidates, ["grmr"]);
  const stockSafe = gemmaSourceDisposition({ stock: gemmaCandidate("stock", [0, 4]), grmr: gemmaCandidate("grmr", [1, 0]) });
  assert.equal(stockSafe.kind, "selection_required");
  assert.deepEqual(stockSafe.safe_candidates, ["stock"]);
});

test("malformed Gemma source evidence remains fatal", () => {
  const malformed = gemmaCandidate("stock", [0, Number.NaN]);
  assert.throws(() => gemmaSourceDisposition({ stock: malformed, grmr: gemmaCandidate("grmr", [1, 0]) }), /malformed Gemma preflight evidence/);
});

test("Gemma file evidence binds exact evaluation artifacts and stage receipts", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-gemma-source-evidence-"));
  try {
    const candidate = (name, hardFailures, misses) => {
      const evaluationPath = join(root, `${name}-evaluation.json`), receiptPath = join(root, `${name}-receipt.json`);
      writeFileSync(evaluationPath, JSON.stringify({ aggregate: { safety: { protected_span_loss: hardFailures }, quality: { correction_misses: misses } } }));
      writeFileSync(receiptPath, JSON.stringify({ schema_version: "heardright.l1.stage_receipt.v1", id: `gemma:evaluate:${name}`, outputs: { [resolve(evaluationPath)]: sha(evaluationPath) } }));
      return { name, source: { id: `${name}/model`, revision: `${name}-revision` }, evaluation_path: evaluationPath, receipt_path: receiptPath };
    };
    const stock = candidate("stock", 2, 1), grmr = candidate("grmr", 1, 0), result = gemmaSourceDispositionFromFiles({ stock, grmr });
    assert.equal(result.status, "selected_least_unsafe_for_training");
    assert.deepEqual(result.evaluations.map((item) => item.evaluation_sha256), [sha(stock.evaluation_path), sha(grmr.evaluation_path)]);
    assert.deepEqual(result.evaluations.map((item) => item.receipt_sha256), [sha(stock.receipt_path), sha(grmr.receipt_path)]);
    writeFileSync(stock.evaluation_path, JSON.stringify({ aggregate: { safety: {}, quality: { correction_misses: 0 } } }));
    assert.throws(() => gemmaSourceDispositionFromFiles({ stock, grmr }), /Gemma preflight evaluation receipt is invalid/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("Gemma source rejection settles without train or export and other lanes continue", () => {
  const order = [], state = { lanes: { falcon: {}, granite: {}, gemma: {}, sotto: {} }, events: [] }, context = { outputDir: tmpdir(), state, persist() {} };
  const marker = { kind: "lane_skip", status: "rejected_no_safe_source", selected_source: null, evaluations: [] };
  let gemmaTrain = 0, gemmaExport = 0, cleanup = 0;
  const status = runFamiliesSequentially(context, ["falcon", "granite", "gemma", "sotto"].map((id) => ({ id, family: id })), marker, (_context, spec, binding) => {
    order.push(spec.id);
    if (spec.id === "gemma") {
      settleGemmaSourceRejection(context, spec, binding, { cleanup: () => { cleanup += 1; return { manifest: "gemma-cleanup.json", receipt: "gemma-release.json" }; }, train: () => { gemmaTrain += 1; }, exportCandidate: () => { gemmaExport += 1; } });
    } else state.lanes[spec.id].status = "viable_candidate";
  });
  assert.deepEqual(order, ["falcon", "granite", "gemma", "sotto"]);
  assert.equal(state.lanes.gemma.status, "rejected_no_safe_source");
  assert.equal(cleanup, 1); assert.equal(gemmaTrain, 0); assert.equal(gemmaExport, 0);
  assert.equal(status, "complete_with_rejections");
});

test("all attempted checkpoints hard-invalid rejects with null selection and evidence", () => {
  const attempts = [attempt(0, 2, 4), attempt(1, 1, 2)];
  const result = candidateDisposition({ attempts, selectedEpoch: 0, selectedManifest: "checkpoint-0.json", selectedEvaluation: "evaluation-0.json", selectedVector: [2, 4] }, { epoch_policy: {} });
  assert.equal(result.status, "rejected_no_safe_checkpoint");
  assert.equal(result.exportable, false);
  assert.equal(result.selected_epoch, null);
  assert.equal(result.checkpoint_manifest, null);
  assert.deepEqual(result.evidence.map((item) => item.hard_invalid), [true, true]);
});

test("safe stock baseline is non-exportable unless policy explicitly allows it", () => {
  const args = { attempts: [attempt(0, 0, 3), attempt(1, 1, 1)], selectedEpoch: 0, selectedManifest: "baseline.json", selectedEvaluation: "baseline-evaluation.json", selectedVector: [0, 3] };
  assert.equal(candidateDisposition(args, { epoch_policy: {} }).status, "no_trained_improvement");
  const allowed = candidateDisposition(args, { epoch_policy: { allow_stock_baseline_export: true } });
  assert.equal(allowed.status, "viable_candidate");
  assert.equal(allowed.stock_baseline_export, true);
});

test("non-exportable disposition never invokes export and always returns cleanup evidence", () => {
  let exports = 0, cleanups = 0;
  const result = executeDispositionActions({ status: "rejected_no_safe_checkpoint", exportable: false }, { exportCandidate: () => { exports += 1; }, cleanup: () => { cleanups += 1; return { manifest: "cleanup.manifest.json", receipt: "cleanup.receipt.json" }; } });
  assert.equal(exports, 0);
  assert.equal(cleanups, 1);
  assert.equal(result.exported, false);
  assert.equal(result.cleanup.receipt, "cleanup.receipt.json");
});

test("Falcon rejection does not prevent Granite and mixed outcomes complete with rejections", () => {
  const order = [], state = { lanes: { falcon: {}, granite: {} }, events: [] };
  const context = { outputDir: tmpdir(), state, persist() {} };
  const status = runFamiliesSequentially(context, [{ id: "falcon", family: "falcon" }, { id: "granite", family: "granite" }], null, (_context, spec) => {
    order.push(spec.id);
    state.lanes[spec.id].status = spec.id === "falcon" ? "rejected_no_safe_checkpoint" : "viable_candidate";
  });
  assert.deepEqual(order, ["falcon", "granite"]);
  assert.equal(status, "complete_with_rejections");
});

test("unsupported Granite full-weight backends do not exclude or block Gemma and Sotto", () => {
  const order = [], state = { lanes: { granite: {}, gemma: {}, sotto: {} }, events: [] };
  const context = { outputDir: tmpdir(), state, persist() {} };
  const status = runFamiliesSequentially(context, [{ id: "granite", family: "granite" }, { id: "gemma", family: "gemma" }, { id: "sotto", family: "sotto" }], null, (_context, spec) => {
    order.push(spec.id);
    state.lanes[spec.id].status = spec.id === "granite" ? "unsupported_for_full_weight_training" : "viable_candidate";
  });
  assert.deepEqual(order, ["granite", "gemma", "sotto"]);
  assert.equal(status, "complete_with_rejections");
});

test("all rejected produces completed_no_viable_candidates", () => {
  const lanes = { falcon: { status: "rejected_no_safe_checkpoint" }, granite: { status: "no_trained_improvement" } };
  assert.equal(summarizeRunStatus(lanes, ["falcon", "granite"]), "completed_no_viable_candidates");
});

test("durable rejected lane resumes idempotently from cleanup output receipt", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-rejected-resume-"));
  try {
    const cleanupDir = join(root, "falcon", "cleanup"), receipts = join(root, "stage-receipts"); mkdirSync(cleanupDir, { recursive: true }); mkdirSync(receipts);
    const manifest = join(cleanupDir, "cleanup.manifest.json"); writeFileSync(manifest, JSON.stringify({ action: "cleanup", gpu_free_bytes: 1 }));
    const receipt = join(receipts, "falcon--release-gpu.json"); writeFileSync(receipt, JSON.stringify({ outputs: { [resolve(manifest)]: sha(manifest) } }));
    let executions = 0, persists = 0;
    const state = { lanes: { falcon: { status: "rejected_no_safe_checkpoint", cleanup: { manifest, receipt } } }, events: [] };
    const context = { outputDir: root, state, persist() { persists += 1; } };
    const status = runFamiliesSequentially(context, [{ id: "falcon", family: "falcon" }], null, () => { executions += 1; });
    assert.equal(status, "completed_no_viable_candidates");
    assert.equal(executions, 0);
    assert.equal(persists, 1);
    assert.equal(state.events.at(-1).kind, "terminal_family_resumed");
    writeFileSync(manifest, JSON.stringify({ action: "cleanup", gpu_free_bytes: 0 }));
    runFamiliesSequentially(context, [{ id: "falcon", family: "falcon" }], null, () => { executions += 1; state.lanes.falcon.status = "rejected_no_safe_checkpoint"; });
    assert.equal(executions, 1, "tampered cleanup output must prevent terminal-lane skipping");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("durable Gemma source rejection resumes only while preflight and cleanup receipts remain valid", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-gemma-source-resume-"));
  try {
    const artifact = join(root, "gemma-preflight.manifest.json"), cleanup = join(root, "cleanup.manifest.json"), receipts = join(root, "stage-receipts"); mkdirSync(receipts, { recursive: true });
    writeFileSync(artifact, JSON.stringify({ status: "rejected_no_safe_source" })); writeFileSync(cleanup, JSON.stringify({ action: "cleanup" }));
    const preflightReceipt = join(receipts, "gemma--select-source.json"), cleanupReceipt = join(receipts, "gemma--release-gpu.json");
    writeFileSync(preflightReceipt, JSON.stringify({ outputs: { [resolve(artifact)]: sha(artifact) } })); writeFileSync(cleanupReceipt, JSON.stringify({ outputs: { [resolve(cleanup)]: sha(cleanup) } }));
    const state = { lanes: { gemma: { status: "rejected_no_safe_source", source_preflight: { artifact, artifact_sha256: sha(artifact), receipt: preflightReceipt, receipt_sha256: sha(preflightReceipt) }, cleanup: { manifest: cleanup, receipt: cleanupReceipt } } }, events: [] };
    const context = { outputDir: root, state, persist() {} }; let executions = 0;
    runFamiliesSequentially(context, [{ id: "gemma", family: "gemma" }], null, () => { executions += 1; state.lanes.gemma.status = "rejected_no_safe_source"; });
    assert.equal(executions, 0);
    writeFileSync(artifact, JSON.stringify({ status: "tampered" }));
    runFamiliesSequentially(context, [{ id: "gemma", family: "gemma" }], null, () => { executions += 1; state.lanes.gemma.status = "rejected_no_safe_source"; });
    assert.equal(executions, 1, "tampered Gemma preflight artifact must prevent terminal-lane skipping");
  } finally { rmSync(root, { recursive: true, force: true }); }
});
