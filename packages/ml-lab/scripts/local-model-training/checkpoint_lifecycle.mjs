#!/usr/bin/env node
import { createHash } from "node:crypto";
import {
  closeSync,
  cpSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const shaBuffer = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const HASH_BUFFER_BYTES = 8 * 1024 * 1024;
export function shaFile(path) {
  const digest = createHash("sha256");
  const buffer = Buffer.allocUnsafe(HASH_BUFFER_BYTES);
  const handle = openSync(path, "r");
  try {
    for (;;) {
      const bytes = readSync(handle, buffer, 0, buffer.length, null);
      if (bytes === 0) break;
      digest.update(buffer.subarray(0, bytes));
    }
  } finally {
    closeSync(handle);
  }
  return `sha256:${digest.digest("hex")}`;
}
const canonicalHash = (value) => shaBuffer(Buffer.from(JSON.stringify(value)));
const atomic = (path, value) => {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.tmp-${process.pid}`;
  writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`);
  renameSync(temporary, path);
};
const fail = (message) => { throw new Error(`Checkpoint lifecycle: ${message}`); };
const TRAINER_STATE_FILENAMES = new Set(["trainer_state.json", "trainer-state.json"]);
const TRANSPORT_OPTIONS = [
  "-o", "BatchMode=yes",
  "-o", "ConnectTimeout=15",
  "-o", "ServerAliveInterval=15",
  "-o", "ServerAliveCountMax=2",
];

export function secureTransportInvocation(command, args) {
  return {
    args: command === "ssh" || command === "scp" ? [...TRANSPORT_OPTIONS, ...args] : args,
    stdio: ["ignore", "inherit", "inherit"],
  };
}

function runTransport(command, args) {
  const invocation = secureTransportInvocation(command, args);
  return spawnSync(command, invocation.args, { stdio: invocation.stdio });
}

function describeCheckpointCandidate(candidateManifest) {
  const weights = candidateManifest.files.filter((file) => /(^|\/)(model.*\.(safetensors|bin)|.*weights.*\.(safetensors|bin))$/iu.test(file.path));
  const tokenizer = candidateManifest.files.filter((file) => /(tokenizer|chat_template|special_tokens_map|tokenizer_config)/iu.test(file.path));
  const trainerState = candidateManifest.files.filter((file) => TRAINER_STATE_FILENAMES.has(basename(file.path)));
  return { weights, tokenizer, trainerState };
}

function sortedCheckpointFallbacks(baseDir) {
  if (!existsSync(baseDir) || !statSync(baseDir).isDirectory()) return [];
  const parent = dirname(baseDir);
  const children = readdirSync(parent, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && /^checkpoint(?:-\d+)?$/u.test(entry.name))
    .map((entry) => resolve(parent, entry.name));
  const parsed = (path) => {
    const match = /checkpoint-(\d+)$/u.exec(path);
    return match ? Number.parseInt(match[1], 10) : Number.NEGATIVE_INFINITY;
  };
  children.sort((left, right) => parsed(right) - parsed(left));
  return children;
}

function resolveCheckpointDir({ lane, epoch, checkpointDir }) {
  const manifestDir = checkpointDir = resolve(checkpointDir);
  const candidates = [];
  if (existsSync(manifestDir) && statSync(manifestDir).isDirectory()) candidates.push(manifestDir);
  for (const candidate of sortedCheckpointFallbacks(manifestDir)) {
    if (!candidates.includes(candidate)) candidates.push(candidate);
  }
  if (!candidates.length) fail(`checkpoint directory is missing for ${lane} epoch ${epoch}: ${checkpointDir}`);
  for (const candidate of candidates) {
    const manifest = fileManifest(candidate);
    const { weights, tokenizer, trainerState } = describeCheckpointCandidate(manifest);
    if (!weights.length || !tokenizer.length || trainerState.length !== 1) continue;
    return { checkpointDir: candidate, manifest, weights, tokenizer, trainerState };
  }
  fail(`checkpoint directory does not contain the required model/tokenizer/trainer-state payload for ${lane} epoch ${epoch}; checked ${candidates.join(", ")}`);
}

export function fileManifest(root) {
  root = resolve(root);
  const files = [];
  const walk = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = resolve(current, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) {
        files.push({
          path: relative(root, path).split(sep).join("/"),
          bytes: statSync(path).size,
          sha256: shaFile(path),
        });
      }
    }
  };
  walk(root);
  files.sort((left, right) => left.path.localeCompare(right.path));
  return {
    root,
    files,
    bytes: files.reduce((sum, file) => sum + file.bytes, 0),
    tree_sha256: canonicalHash(files),
  };
}

export function createSyncRequest({
  lane,
  epoch,
  checkpointManifest,
  evaluation,
  trainingSha256,
  policyControlSha256,
  receiptPath,
  output,
}) {
  const action = JSON.parse(readFileSync(checkpointManifest, "utf8"));
  const resolved = resolveCheckpointDir({ lane, epoch, checkpointDir: action.checkpoint_dir ?? "" });
  const { checkpointDir, manifest, weights, tokenizer, trainerState } = resolved;
  const request = {
    schema_version: "heardright.l1.checkpoint_sync_request.v1",
    lane,
    epoch,
    checkpoint_dir: checkpointDir,
    checkpoint_manifest: resolve(checkpointManifest),
    manifest_sha256: shaFile(checkpointManifest),
    all_weight_file_hashes: weights.map(({ path, bytes, sha256 }) => ({ path, bytes, sha256 })),
    tokenizer_and_template_hash: canonicalHash(tokenizer),
    trainer_state_hash: trainerState[0].sha256,
    dataset_and_policy_control_hashes: {
      training: trainingSha256,
      policy_control: policyControlSha256,
    },
    development_evaluation: resolve(evaluation),
    development_evaluation_receipt_hash: shaFile(evaluation),
    checkpoint_tree: manifest,
    remote_receipt_path: resolve(receiptPath),
    created_at: new Date().toISOString(),
  };
  atomic(output, request);
  return request;
}

function copyRemote(request, remote, destination) {
  if (!/^[A-Za-z0-9_.@-]+$/u.test(remote)) fail("remote SSH target contains unsupported characters");
  if (!/^\/[A-Za-z0-9_./-]+$/u.test(request.checkpoint_dir)) fail("remote checkpoint path contains unsupported characters");
  mkdirSync(destination, { recursive: true });
  const result = runTransport("scp", ["-r", `${remote}:${request.checkpoint_dir}/.`, destination]);
  if (result.error || result.status !== 0) fail(`scp checkpoint copy failed with status ${result.status ?? "launch-error"}`);
}

export function syncOnWindows({
  requestPath,
  destinationRoot,
  receiptPath,
  remote = null,
  platform = process.platform,
}) {
  if (platform !== "win32") fail("sync receipt must be produced on Windows");
  const request = JSON.parse(readFileSync(requestPath, "utf8"));
  const destination = resolve(destinationRoot, request.lane, `epoch-${request.epoch}`, "checkpoint");
  let received = existsSync(destination) ? fileManifest(destination) : null;
  if (
    received?.tree_sha256 !== request.checkpoint_tree.tree_sha256
    || received.bytes !== request.checkpoint_tree.bytes
  ) {
    if (existsSync(destination)) rmSync(destination, { recursive: true, force: true });
    if (remote) copyRemote(request, remote, destination);
    else cpSync(request.checkpoint_dir, destination, { recursive: true });
    received = fileManifest(destination);
  }
  if (
    received.tree_sha256 !== request.checkpoint_tree.tree_sha256
    || received.bytes !== request.checkpoint_tree.bytes
  ) {
    fail("Windows checkpoint copy failed independent tree verification");
  }
  const receipt = {
    schema_version: "heardright.l1.windows_checkpoint_sync_receipt.v1",
    verified_on_windows: true,
    request_sha256: shaFile(requestPath),
    lane: request.lane,
    epoch: request.epoch,
    destination,
    destination_tree_sha256: received.tree_sha256,
    destination_bytes: received.bytes,
    manifest_sha256: request.manifest_sha256,
    all_weight_file_hashes: request.all_weight_file_hashes,
    tokenizer_and_template_hash: request.tokenizer_and_template_hash,
    trainer_state_hash: request.trainer_state_hash,
    dataset_and_policy_control_hashes: request.dataset_and_policy_control_hashes,
    development_evaluation_receipt_hash: request.development_evaluation_receipt_hash,
    verified_at: new Date().toISOString(),
  };
  atomic(receiptPath, receipt);
  if (remote) {
    if (!/^\/[A-Za-z0-9_./-]+$/u.test(request.remote_receipt_path)) {
      fail("remote receipt path contains unsupported characters");
    }
    const temporary = `${request.remote_receipt_path}.tmp`;
    let result = runTransport("scp", [receiptPath, `${remote}:${temporary}`]);
    if (result.error || result.status !== 0) fail(`receipt upload failed with status ${result.status ?? "launch-error"}`);
    result = runTransport("ssh", [remote, "mv", "--", temporary, request.remote_receipt_path]);
    if (result.error || result.status !== 0) fail(`atomic remote receipt activation failed with status ${result.status ?? "launch-error"}`);
  }
  return receipt;
}

export function verifySyncReceipt(requestPath, receiptPath) {
  if (!existsSync(receiptPath)) fail(`Windows sync receipt is missing: ${receiptPath}`);
  const request = JSON.parse(readFileSync(requestPath, "utf8"));
  const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
  if (
    receipt.schema_version !== "heardright.l1.windows_checkpoint_sync_receipt.v1"
    || receipt.verified_on_windows !== true
    || receipt.request_sha256 !== shaFile(requestPath)
    || receipt.destination_tree_sha256 !== request.checkpoint_tree.tree_sha256
    || receipt.destination_bytes !== request.checkpoint_tree.bytes
  ) {
    fail("Windows sync receipt does not bind the requested checkpoint");
  }
  for (const field of [
    "manifest_sha256",
    "tokenizer_and_template_hash",
    "trainer_state_hash",
    "development_evaluation_receipt_hash",
  ]) {
    if (receipt[field] !== request[field]) fail(`Windows sync receipt mismatch: ${field}`);
  }
  if (
    canonicalHash(receipt.all_weight_file_hashes) !== canonicalHash(request.all_weight_file_hashes)
    || canonicalHash(receipt.dataset_and_policy_control_hashes)
      !== canonicalHash(request.dataset_and_policy_control_hashes)
  ) {
    fail("Windows sync receipt hash collections do not match request");
  }
  return receipt;
}

export function logicalBytes(root) {
  return fileManifest(root).bytes;
}

export function pruneSyncedCheckpoint({
  runRoot,
  checkpointDir,
  requestPath,
  receiptPath,
  protectedPaths = [],
  usedThresholdBytes = 45 * 1024 ** 3,
}) {
  const root = resolve(runRoot);
  const target = resolve(checkpointDir);
  if (!(target + sep).startsWith(root + sep)) fail("refusing to prune outside the run root");
  if (
    protectedPaths.map((path) => resolve(path))
      .some((path) => path === target || path.startsWith(target + sep) || target.startsWith(path + sep))
  ) {
    fail("refusing to prune a protected rollback or selected checkpoint");
  }
  verifySyncReceipt(requestPath, receiptPath);
  const used = logicalBytes(root);
  if (used < usedThresholdBytes) {
    return { pruned: false, used_bytes: used, threshold_bytes: usedThresholdBytes };
  }
  rmSync(target, { recursive: true, force: true });
  return {
    pruned: true,
    used_bytes_before: used,
    threshold_bytes: usedThresholdBytes,
    removed: target,
  };
}

function option(argv, name) {
  const index = argv.indexOf(name);
  return index < 0 ? undefined : argv[index + 1];
}

if (
  process.argv[1]
  && !process.execArgv.includes("-e")
  && (
    process.platform === "win32"
      ? basename(process.argv[1]).toLowerCase() === basename(fileURLToPath(import.meta.url)).toLowerCase()
      : basename(process.argv[1]) === basename(fileURLToPath(import.meta.url))
  )
) {
  try {
    const argv = process.argv.slice(2);
    const action = option(argv, "--action");
    if (action !== "sync-windows") fail("CLI currently supports --action sync-windows");
    const request = resolve(option(argv, "--request") ?? fail("--request is required"));
    const destination = resolve(option(argv, "--destination-root") ?? fail("--destination-root is required"));
    const receipt = resolve(option(argv, "--receipt") ?? fail("--receipt is required"));
    const result = syncOnWindows({
      requestPath: request,
      destinationRoot: destination,
      receiptPath: receipt,
      remote: option(argv, "--remote"),
    });
    process.stdout.write(`${JSON.stringify({ status: "ok", receipt, destination: result.destination })}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
