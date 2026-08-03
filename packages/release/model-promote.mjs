#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  buildRuntimeArtifactManifest,
  signRuntimeArtifactManifest,
} from "./runtime-artifact-manifest.mjs";
import { loadReleaseToken } from "./release-token.mjs";
import { registerRightAppsRelease } from "./rightapps-register.mjs";
import { assertCleanOverlay, loadBoundedAdminTrust, readVerificationDocument, verifyPromotionEvidence } from "./verification-ticket.mjs";

export const RUNTIME_ARTIFACT_TRUSTED_KEY_ID = "rightkit-runtime-artifacts-2026-07";
const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const UPLOAD = path.join(TOOL_ROOT, "upload-large.mjs");

export async function promoteRuntimeArtifact(input, deps = {}) {
  const metadata = deps.metadata ?? fileMetadata;
  const upload = deps.upload;
  const register = deps.register;
  const { config } = input;
  assertPromotionConfig(config);
  if (!input.verification) throw new Error("verification ticket, Mac and Windows receipts, and trusted promotion receipt are required");
  const adminTrust = deps.adminTrust ?? loadBoundedAdminTrust();
  verifyPromotionEvidence({
    ticket: input.verification.ticket,
    hostReceipts: input.verification.hostReceipts,
    promotionReceipt: input.verification.promotionReceipt,
    trustedKeys: adminTrust.ticketKeys,
    hostTrust: { hosts: adminTrust.hosts, keys: adminTrust.hostKeys },
    promotionTrustedKeys: adminTrust.ticketKeys,
    candidateCommit: input.verification.candidateCommit,
    candidateTree: input.verification.candidateTree,
    commandId: "right-release.model-promote",
  });
  if (input.authority !== "heardright") throw new Error("model promotion authority must be heardright");
  if (input.keyId !== RUNTIME_ARTIFACT_TRUSTED_KEY_ID) throw new Error(`untrusted runtime artifact key id: ${input.keyId}`);
  if (!input.privateKey) throw new Error("runtime artifact signing key is required");

  const [artifact, evidence] = await Promise.all([
    metadata(input.artifactFile),
    metadata(input.evidenceFile),
  ]);
  assertMetadata(artifact, config.artifact, "artifact");
  if (evidence.sha256 !== config.evidence.sha256) throw new Error("evidence SHA-256 does not match promotion config");

  const manifest = buildRuntimeArtifactManifest({
    ...config.manifest,
    filename: config.artifact.filename,
    sha256: artifact.sha256,
    sizeBytes: artifact.sizeBytes,
    promotion: {
      ...config.manifest.promotion,
      authorityAppKey: input.authority,
      evidenceSha256: evidence.sha256,
    },
  });
  const envelope = signRuntimeArtifactManifest(manifest, input.privateKey, input.keyId);
  const isR2 = manifest.distribution.delivery !== "bundled";
  const steps = isR2
    ? ["upload immutable object", "register signed envelope", "replace stable pointer"]
    : ["register signed envelope"];
  if (input.dryRun) return { envelope, steps };
  if (typeof register !== "function") throw new Error("RightApps registration boundary is required");
  if (isR2 && typeof upload !== "function") throw new Error("R2 upload boundary is required");

  if (isR2) {
    await upload({
      purpose: "object",
      file: input.artifactFile,
      key: manifest.object.r2Key,
      lane: manifest.distribution.delivery,
    });
  }
  await register(envelope);
  if (isR2) {
    await upload({
      purpose: "pointer",
      content: `${JSON.stringify(envelope)}\n`,
      key: manifest.pointerKey,
      lane: manifest.distribution.delivery,
    });
  }
  return { envelope, steps };
}

export function defaultWindowsRuntimeArtifactSigningKeyFile(env = process.env) {
  const appData = env.APPDATA || path.join(os.homedir(), "AppData", "Roaming");
  return path.join(appData, "RightKit", "runtime-artifact-signing-key.pem");
}

export async function loadRuntimeArtifactSigningKey({ env = process.env, platform = process.platform, explicitFile } = {}) {
  const direct = env.RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY;
  if (direct?.trim()) return direct.replace(/\\n/g, "\n");
  const configuredFile = explicitFile || env.RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE;
  if (configuredFile) return readNonemptySecret(path.resolve(configuredFile));
  if (platform === "win32") return readNonemptySecret(defaultWindowsRuntimeArtifactSigningKeyFile(env));
  throw new Error("RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY or RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE is required");
}

export function runtimeArtifactUploadEnv(source = process.env) {
  const env = { ...source };
  delete env.RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY;
  delete env.RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE;
  delete env.RIGHTAPPS_RELEASE_TOKEN;
  delete env.RIGHTAPPS_RELEASE_TOKEN_FILE;
  return env;
}

async function main() {
  if (process.argv.slice(2).some((arg) => arg === "--help" || arg === "-h")) {
    printHelp();
    return;
  }
  const args = parseArgs(process.argv.slice(2));
  const configPath = path.resolve(args.config);
  const configRoot = path.dirname(configPath);
  const raw = JSON.parse(await readFile(configPath, "utf8"));
  assertExactKeys(raw, ["artifact", "evidence", "manifest"], "promotion config");
  assertExactKeys(raw.artifact, ["file", "filename", "sha256", "sizeBytes"], "artifact");
  assertExactKeys(raw.evidence, ["file", "sha256"], "evidence");
  const config = {
    artifact: {
      filename: raw.artifact.filename,
      sha256: raw.artifact.sha256,
      sizeBytes: raw.artifact.sizeBytes,
    },
    evidence: { sha256: raw.evidence.sha256 },
    manifest: raw.manifest,
  };
  const privateKey = await loadRuntimeArtifactSigningKey({ explicitFile: args.signingKeyFile });
  const releaseToken = args.dryRun ? null : await loadReleaseToken();
  const runtime = {
    metadata: fileMetadata,
    upload: defaultUpload,
    register: (envelope) => registerRuntimeArtifact(envelope, releaseToken),
  };
  const result = await promoteRuntimeArtifact({
    authority: args.authority,
    keyId: args.keyId,
    privateKey,
    artifactFile: path.resolve(configRoot, raw.artifact.file),
    evidenceFile: path.resolve(configRoot, raw.evidence.file),
    config,
    dryRun: args.dryRun,
    verification: readVerificationEvidence(args, configRoot),
  }, runtime);
  const manifest = result.envelope.manifest;
  console.log(`${args.dryRun ? "[dry-run] " : ""}runtime artifact promotion validated`);
  console.log(`  app/kind: ${manifest.entitlement.appKey}/${manifest.artifactKind}`);
  console.log(`  target: ${manifest.target.os}/${manifest.target.arch}`);
  console.log(`  delivery: ${manifest.distribution.delivery}`);
  console.log(`  object sha256: ${manifest.object.sha256}`);
  console.log(`  pointer: ${manifest.pointerKey ?? "bundled"}`);
  console.log(`  order: ${result.steps.join(" -> ")}`);
}

function printHelp() {
  console.log(`right-release model promote --authority heardright --config <promotion.json> [options]

The JSON config has exactly:
  artifact: { file, filename, sha256, sizeBytes }
  evidence: { file, sha256 }
  manifest: { artifactKind, entitlement, distribution, target, versions, provenance,
              promotion: { promotionId, promotedAt } }
artifact.file and evidence.file resolve relative to the config; artifact.sha256,
artifact.sizeBytes, and evidence.sha256 must exactly match those files.

Options:
  --dry-run                 Validate real files and sign, without R2 or RightApps mutations
  --signing-key-file <pem>  Override the protected PEM path
  --key-id <id>             Must equal ${RUNTIME_ARTIFACT_TRUSTED_KEY_ID}
  --verification-ticket <file>
  --host-receipt <file>     Repeat for Mac and Windows receipts
  --promotion-receipt <file>

Signing key inputs (never printed): RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY or
RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE. Windows defaults to
%APPDATA%/RightKit/runtime-artifact-signing-key.pem. Live promotion also uses
the durable RIGHTAPPS_RELEASE_TOKEN boundary.`);
}

function parseArgs(args) {
  let authority = "";
  let config = "";
  let signingKeyFile;
  let keyId = process.env.RIGHTKIT_RUNTIME_ARTIFACT_KEY_ID || RUNTIME_ARTIFACT_TRUSTED_KEY_ID;
  let dryRun = false;
  let verificationTicket = "";
  let promotionReceipt = "";
  const hostReceipts = [];
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--authority") authority = args[++i] ?? "";
    else if (arg === "--config") config = args[++i] ?? "";
    else if (arg === "--signing-key-file") signingKeyFile = args[++i] ?? "";
    else if (arg === "--key-id") keyId = args[++i] ?? "";
    else if (arg === "--dry-run") dryRun = true;
    else if (arg === "--verification-ticket") verificationTicket = args[++i] ?? "";
    else if (arg === "--host-receipt") hostReceipts.push(args[++i] ?? "");
    else if (arg === "--promotion-receipt") promotionReceipt = args[++i] ?? "";
    else throw new Error(`unknown model promote argument: ${arg}`);
  }
  if (!config) throw new Error("model promote requires --config <path>");
  if (!authority) throw new Error("model promote requires --authority heardright");
  if (!verificationTicket || !promotionReceipt || hostReceipts.length === 0) throw new Error("model promote requires verification ticket, host receipts, and promotion receipt");
  return { authority, config, signingKeyFile, keyId, dryRun, verificationTicket, promotionReceipt, hostReceipts };
}

function readVerificationEvidence(args, cwd) {
  const candidateCommit = git(cwd, ["rev-parse", "HEAD"]);
  const candidateTree = git(cwd, ["rev-parse", "HEAD^{tree}"]);
  const ticket = readVerificationDocument(path.resolve(args.verificationTicket));
  const adminTrust = loadBoundedAdminTrust();
  assertCleanOverlay({ status: git(cwd, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]), ticket });
  return {
    ticket,
    hostReceipts: args.hostReceipts.map((file) => readVerificationDocument(path.resolve(file))),
    promotionReceipt: readVerificationDocument(path.resolve(args.promotionReceipt)),
    trustedKeys: adminTrust.ticketKeys,
    hostTrust: { hosts: adminTrust.hosts, keys: adminTrust.hostKeys },
    promotionTrustedKeys: adminTrust.ticketKeys,
    candidateCommit,
    candidateTree,
  };
}

function git(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed`);
  return result.stdout.trim();
}

async function fileMetadata(file) {
  const info = await stat(file);
  if (!info.isFile() || info.size <= 0) throw new Error(`promotion input is not a nonempty file: ${file}`);
  const hash = createHash("sha256");
  await new Promise((resolve, reject) => {
    const stream = createReadStream(file);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("error", reject);
    stream.on("end", resolve);
  });
  return { sha256: hash.digest("hex"), sizeBytes: info.size };
}

async function defaultUpload({ file, content, key, lane }) {
  const bucketAlias = lane === "private-r2" ? "private" : "public";
  const env = runtimeArtifactUploadEnv();
  if (file) return run(process.execPath, [UPLOAD, file, key, bucketAlias], env);
  const tempRoot = await mkdtemp(path.join(os.tmpdir(), "rightkit-runtime-pointer-"));
  const pointerFile = path.join(tempRoot, "manifest.json");
  try {
    await writeFile(pointerFile, content, { encoding: "utf8", mode: 0o600 });
    await run(process.execPath, [UPLOAD, pointerFile, key, bucketAlias], env);
  } finally {
    await rm(tempRoot, { recursive: true, force: true });
  }
}

async function registerRuntimeArtifact(envelope, releaseToken) {
  return registerRightAppsRelease("/v1/admin/apps/runtime-artifacts", envelope, { token: releaseToken });
}

function run(command, args, env = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: process.cwd(), env, stdio: "inherit", windowsHide: true });
    child.on("error", reject);
    child.on("exit", (code) => code === 0 ? resolve() : reject(new Error(`${command} exited ${code}`)));
  });
}

async function readNonemptySecret(file) {
  const value = await readFile(file, "utf8");
  if (!value.trim()) throw new Error(`runtime artifact signing key file is empty: ${file}`);
  return value;
}

function assertPromotionConfig(value) {
  assertExactKeys(value, ["artifact", "evidence", "manifest"], "promotion config");
  assertExactKeys(value.artifact, ["filename", "sha256", "sizeBytes"], "artifact");
  assertExactKeys(value.evidence, ["sha256"], "evidence");
  assertExactKeys(value.manifest, [
    "artifactKind", "entitlement", "distribution", "target", "versions", "provenance", "promotion",
  ], "manifest config");
  assertExactKeys(value.manifest.promotion, ["promotionId", "promotedAt"], "manifest config promotion");
}

function assertMetadata(actual, expected, label) {
  if (actual.sha256 !== expected.sha256) throw new Error(`${label} SHA-256 does not match promotion config`);
  if (actual.sizeBytes !== expected.sizeBytes) throw new Error(`${label} size does not match promotion config`);
}

function assertExactKeys(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  const allowed = new Set(expected);
  for (const key of Object.keys(value)) if (!allowed.has(key)) throw new Error(`${label} has unknown field ${key}`);
  for (const key of expected) if (!Object.hasOwn(value, key)) throw new Error(`${label}.${key} is required`);
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    console.error(`right-release model promote: ${error.message}`);
    process.exitCode = 1;
  });
}
