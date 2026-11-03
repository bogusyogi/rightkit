import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  createSyncRequest,
  pruneSyncedCheckpoint,
  secureTransportInvocation,
  shaFile,
  syncOnWindows,
  verifySyncReceipt,
} from "./checkpoint_lifecycle.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "heardright-checkpoint-life-"));
  const checkpoint = join(root, "run", "falcon", "epoch-1", "checkpoint");
  mkdirSync(checkpoint, { recursive: true });
  writeFileSync(join(checkpoint, "model.safetensors"), "weights");
  writeFileSync(join(checkpoint, "tokenizer.json"), "tokenizer");
  writeFileSync(join(checkpoint, "trainer_state.json"), "state");
  const manifest = join(root, "train.manifest.json");
  const evaluation = join(root, "evaluation.json");
  const request = join(root, "request.json");
  const receipt = join(root, "receipt.json");
  writeFileSync(manifest, JSON.stringify({ checkpoint_dir: checkpoint }));
  writeFileSync(evaluation, "{}");
  createSyncRequest({
    lane: "falcon",
    epoch: 1,
    checkpointManifest: manifest,
    evaluation,
    trainingSha256: "sha256:train",
    policyControlSha256: "sha256:policy",
    receiptPath: receipt,
    output: request,
  });
  return { root, checkpoint, request, receipt };
}

function fallbackFixture() {
  const root = mkdtempSync(join(tmpdir(), "heardright-checkpoint-life-"));
  const runRoot = join(root, "run", "falcon", "epoch-1");
  const checkpoint = join(runRoot, "checkpoint");
  const recovered = join(runRoot, "checkpoint-94");
  mkdirSync(checkpoint, { recursive: true });
  mkdirSync(recovered, { recursive: true });
  writeFileSync(join(checkpoint, "model.safetensors"), "weights");
  writeFileSync(join(checkpoint, "tokenizer.json"), "tokenizer");
  writeFileSync(join(recovered, "model.safetensors"), "weights");
  writeFileSync(join(recovered, "tokenizer.json"), "tokenizer");
  writeFileSync(join(recovered, "trainer_state.json"), "state");
  const manifest = join(root, "train.manifest.json");
  const evaluation = join(root, "evaluation.json");
  const request = join(root, "request.json");
  const receipt = join(root, "receipt.json");
  writeFileSync(manifest, JSON.stringify({ checkpoint_dir: checkpoint }));
  writeFileSync(evaluation, "{}");
  createSyncRequest({
    lane: "falcon",
    epoch: 1,
    checkpointManifest: manifest,
    evaluation,
    trainingSha256: "sha256:train",
    policyControlSha256: "sha256:policy",
    receiptPath: receipt,
    output: request,
  });
  return { root, manifest, request, receipt, recovered };
}

function legacyStateFixture() {
  const root = mkdtempSync(join(tmpdir(), "heardright-checkpoint-life-"));
  const checkpoint = join(root, "run", "falcon", "epoch-1", "checkpoint");
  mkdirSync(checkpoint, { recursive: true });
  writeFileSync(join(checkpoint, "model.safetensors"), "weights");
  writeFileSync(join(checkpoint, "tokenizer.json"), "tokenizer");
  writeFileSync(join(checkpoint, "trainer-state.json"), "state");
  const manifest = join(root, "train.manifest.json");
  const evaluation = join(root, "evaluation.json");
  const request = join(root, "request.json");
  const receipt = join(root, "receipt.json");
  writeFileSync(manifest, JSON.stringify({ checkpoint_dir: checkpoint }));
  writeFileSync(evaluation, "{}");
  createSyncRequest({
    lane: "falcon",
    epoch: 1,
    checkpointManifest: manifest,
    evaluation,
    trainingSha256: "sha256:train",
    policyControlSha256: "sha256:policy",
    receiptPath: receipt,
    output: request,
  });
  return { root, request };
}

test("Windows copy independently verifies and binds every required receipt field", () => {
  const item = fixture();
  try {
    syncOnWindows({
      requestPath: item.request,
      destinationRoot: join(item.root, "windows"),
      receiptPath: item.receipt,
      platform: "win32",
    });
    const receipt = verifySyncReceipt(item.request, item.receipt);
    assert.equal(receipt.verified_on_windows, true);
    assert.equal(receipt.dataset_and_policy_control_hashes.training, "sha256:train");
  } finally {
    rmSync(item.root, { recursive: true, force: true });
  }
});

test("sync request resolves a populated checkpoint-<step> directory when configured final weights lack trainer state", () => {
  const item = fallbackFixture();
  try {
    const request = JSON.parse(readFileSync(item.request, "utf8"));
    assert.equal(request.checkpoint_dir, item.recovered);
  } finally {
    rmSync(item.root, { recursive: true, force: true });
  }
});

test("sync request accepts the legacy trainer-state.json filename during transition", () => {
  const item = legacyStateFixture();
  try {
    assert.equal(
      JSON.parse(readFileSync(item.request, "utf8")).checkpoint_dir,
      join(item.root, "run", "falcon", "epoch-1", "checkpoint"),
    );
  } finally {
    rmSync(item.root, { recursive: true, force: true });
  }
});

test("remote prune requires a valid receipt, threshold, in-root target, and no protected path", () => {
  const item = fixture();
  try {
    assert.throws(
      () => pruneSyncedCheckpoint({
        runRoot: join(item.root, "run"),
        checkpointDir: item.checkpoint,
        requestPath: item.request,
        receiptPath: item.receipt,
        usedThresholdBytes: 0,
      }),
      /receipt is missing/u,
    );
    syncOnWindows({
      requestPath: item.request,
      destinationRoot: join(item.root, "windows"),
      receiptPath: item.receipt,
      platform: "win32",
    });
    assert.throws(
      () => pruneSyncedCheckpoint({
        runRoot: join(item.root, "run"),
        checkpointDir: item.checkpoint,
        requestPath: item.request,
        receiptPath: item.receipt,
        protectedPaths: [item.checkpoint],
        usedThresholdBytes: 0,
      }),
      /protected/u,
    );
    const result = pruneSyncedCheckpoint({
      runRoot: join(item.root, "run"),
      checkpointDir: item.checkpoint,
      requestPath: item.request,
      receiptPath: item.receipt,
      usedThresholdBytes: 0,
    });
    assert.equal(result.pruned, true);
  } finally {
    rmSync(item.root, { recursive: true, force: true });
  }
});

test("checkpoint copy transports cannot inherit stdin or wait on SSH prompts", () => {
  const scp = secureTransportInvocation("scp", [
    "-r",
    "vast:/workspace/checkpoint/.",
    "C:\\private\\checkpoint",
  ]);
  assert.deepEqual(scp.stdio, ["ignore", "inherit", "inherit"]);
  assert.equal(scp.args.includes("BatchMode=yes"), true);
  assert.equal(scp.args.includes("ConnectTimeout=15"), true);
});

test("checkpoint hashing streams files across multiple bounded chunks", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-checkpoint-hash-"));
  try {
    const path = join(root, "large.bin");
    const payload = Buffer.alloc(17 * 1024 * 1024 + 13, 0x5a);
    writeFileSync(path, payload);
    const expected = `sha256:${createHash("sha256").update(payload).digest("hex")}`;
    assert.equal(shaFile(path), expected);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("Windows sync reuses an already complete hash-matching destination after restart", () => {
  const item = fixture();
  try {
    const destinationRoot = join(item.root, "windows");
    syncOnWindows({
      requestPath: item.request,
      destinationRoot,
      receiptPath: item.receipt,
      platform: "win32",
    });
    rmSync(item.checkpoint, { recursive: true, force: true });
    const receipt = syncOnWindows({
      requestPath: item.request,
      destinationRoot,
      receiptPath: item.receipt,
      platform: "win32",
    });
    assert.equal(receipt.verified_on_windows, true);
  } finally {
    rmSync(item.root, { recursive: true, force: true });
  }
});
