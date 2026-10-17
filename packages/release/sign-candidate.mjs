#!/usr/bin/env node
// right-release sign-candidate: sign the EXACT bytes of an unsigned release candidate instead of rebuilding from source
// (Orthic-Labs/rightkit#320, SwiftRight SR0 7a). The candidate job's artifact (MSIX / .exe on Windows, .app or loose executables on
// macOS) is the signing input. Steps: verify every file's sha256 against the candidate receipt (refuse on any mismatch, missing or
// unrecorded file), sign in place (Windows: Azure Artifact Signing through sign-windows.mjs; macOS: Developer ID through
// sign-dev-artifact), confirm each signed file changed, and write a receipt binding candidate sha -> signed sha. It never builds,
// notarizes or publishes, and a signing job calls it instead of `right-release build`.
//
// Candidate receipt (same shape as the dev artifact payload metadata): { schema: 1, files: [{ path, sha256 }] }, paths relative to --artifact.
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { readMsixSignerSubject } from "./msix-signer.mjs";
import { signDevArtifact, verifyDevArtifactPayload } from "./sign-dev-artifact.mjs";

export const SIGN_CANDIDATE_CONTRACT = "rightkit-sign-candidate-v1";
export const SIGN_CANDIDATE_SCHEMA = 1;

export class SignCandidateError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "SignCandidateError";
    this.code = code;
  }
}

const fail = (code, message) => { throw new SignCandidateError(code, message); };
const toPosix = (value) => value.split(path.sep).join("/");
const USAGE = "usage: right-release sign-candidate --platform win|mac --artifact <dir> --candidate-receipt <receipt.json> --out <binding.json> [--manifest <sign.json>] [--publisher <subject>] [--dry-run]";

const defaultSignWindows = ({ files, env, run }) => {
  const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "sign-windows.mjs");
  const tempDir = mkdtempSync(path.join(os.tmpdir(), "sign-candidate-"));
  try {
    const result = run(process.execPath, [script, "--receipt", path.join(tempDir, "authenticode.json"), ...files], { env, encoding: "utf8", windowsHide: true, shell: false });
    if (result.error || result.status !== 0) fail("SIGN_CANDIDATE_SIGNING_FAILED", `sign-windows failed: ${`${result.stdout ?? ""}\n${result.stderr ?? ""}`.trim() || result.error?.message}`);
  } finally { rmSync(tempDir, { recursive: true, force: true }); }
};

/**
 * Sign an existing candidate. Injectable for tests: `signWindows({ files })`, `signMac(options)` (defaults to signDevArtifact),
 * `evidenceOf`, `readSigner`. Returns the binding receipt (also written to `outPath`). With `dryRun` only the hash check runs.
 */
export function signCandidate({
  platform, artifact, receiptPath, outPath, manifestPath, publisher, dryRun = false,
  env = process.env, run = spawnSync, hostPlatform = process.platform,
  evidenceOf = fileEvidence, readSigner = readMsixSignerSubject,
  signWindows = (options) => defaultSignWindows({ ...options, env, run }),
  signMac = (options) => signDevArtifact(options),
} = {}) {
  if (!platform || !artifact || !receiptPath || !outPath) fail("SIGN_CANDIDATE_USAGE", USAGE);
  if (platform !== "win" && platform !== "mac") fail("SIGN_CANDIDATE_USAGE", `--platform must be win or mac: ${JSON.stringify(platform)}`);
  if (platform === "mac" && !manifestPath) fail("SIGN_CANDIDATE_USAGE", "--manifest <sign.json> is required for --platform mac (fixed identifier per file)");
  const root = path.resolve(artifact);
  if (!existsSync(root)) fail("SIGN_CANDIDATE_MISSING", `artifact directory does not exist: ${root}`);
  const receiptFile = path.resolve(receiptPath);
  if (!existsSync(receiptFile)) fail("SIGN_CANDIDATE_MISSING", `candidate receipt does not exist: ${receiptFile}`);

  // Bytes must be the qualified candidate: every recorded hash is checked before anything is signed.
  let candidate;
  try { candidate = verifyDevArtifactPayload(root, receiptFile); } catch (error) {
    fail(`SIGN_CANDIDATE_${String(error.code ?? "RECEIPT_INVALID").replace(/^SIGN_DEV_ARTIFACT_/, "")}`, error.message);
  }
  const candidateSha = new Map([...candidate.recorded].map(([file, item]) => [path.relative(root, file), item.sha256]));
  const receiptSha256 = evidenceOf(receiptFile).sha256;
  if (dryRun) return { schema: SIGN_CANDIDATE_SCHEMA, dryRun: true, platform, files: [...candidateSha].map(([rel, sha256]) => ({ path: toPosix(rel), candidateSha256: sha256 })) };
  if (platform === "win" && hostPlatform !== "win32") fail("SIGN_CANDIDATE_PLATFORM", "--platform win must run on Windows");
  if (platform === "mac" && hostPlatform !== "darwin") fail("SIGN_CANDIDATE_PLATFORM", "--platform mac must run on macOS");

  let files;
  let signing;
  if (platform === "win") {
    const absolute = [...candidateSha.keys()].map((rel) => path.resolve(root, rel));
    signWindows({ files: absolute });
    files = [...candidateSha].map(([rel, sha]) => {
      const signedSha256 = evidenceOf(path.resolve(root, rel)).sha256;
      if (signedSha256 === sha) fail("SIGN_CANDIDATE_UNCHANGED", `${toPosix(rel)} was not changed by signing`);
      let signer;
      if (/\.msix(bundle)?$/i.test(rel)) {
        signer = readSigner(path.resolve(root, rel));
        if (publisher && signer !== publisher) fail("SIGN_CANDIDATE_PUBLISHER_MISMATCH", `${toPosix(rel)} is signed as ${JSON.stringify(signer)}, expected ${JSON.stringify(publisher)} (a subject change changes the package family name)`);
      }
      return { path: toPosix(rel), candidateSha256: sha, signedSha256, ...(signer ? { signerSubject: signer } : {}) };
    });
    signing = "azure-artifact-signing";
  } else {
    // The candidate receipt doubles as the payload metadata, so work on a copy and keep the candidate receipt as submitted.
    const workDir = mkdtempSync(path.join(os.tmpdir(), "sign-candidate-"));
    try {
      const metadata = path.join(workDir, "payload.json");
      copyFileSync(receiptFile, metadata);
      const evidence = signMac({ artifact: root, manifestPath, metadataPath: metadata, evidencePath: path.join(workDir, "evidence.json"), env, run, platform: hostPlatform });
      const signedByPath = new Map(evidence.files.map((file) => [file.path, file]));
      files = [...candidateSha].map(([rel, sha]) => {
        const key = toPosix(rel);
        const record = signedByPath.get(key);
        if (record) {
          if (record.sha256Before !== sha) fail("SIGN_CANDIDATE_HASH_MISMATCH", `${key}: signing started from ${record.sha256Before}, candidate receipt says ${sha}`);
          return { path: key, candidateSha256: sha, signedSha256: record.sha256After, identifier: record.identifier, teamIdentifier: record.teamIdentifier };
        }
        const unchanged = evidenceOf(path.resolve(root, rel)).sha256;
        if (unchanged !== sha) fail("SIGN_CANDIDATE_HASH_MISMATCH", `${key} is not in the sign manifest but changed during signing`);
        return { path: key, candidateSha256: sha, signedSha256: unchanged, unsigned: true };
      });
      signing = `developer-id:${evidence.teamIdentifier}`;
    } finally { rmSync(workDir, { recursive: true, force: true }); }
  }

  const binding = {
    schema: SIGN_CANDIDATE_SCHEMA, contract: SIGN_CANDIDATE_CONTRACT, platform, signing,
    rebuilt: false, built: false, notarized: false, published: false,
    candidateReceiptSha256: receiptSha256, artifact: root, files,
  };
  const absoluteOut = path.resolve(outPath);
  mkdirSync(path.dirname(absoluteOut), { recursive: true });
  writeFileSync(absoluteOut, `${JSON.stringify(binding, null, 2)}\n`);
  return binding;
}

/** Validate a binding receipt: contract, no rebuild, every file bound candidate sha -> signed sha. Throws SIGN_CANDIDATE_BINDING_INVALID. */
export function validateCandidateBinding(binding, expected = {}) {
  const bad = (message) => fail("SIGN_CANDIDATE_BINDING_INVALID", `candidate binding receipt: ${message}`);
  if (!binding || binding.schema !== SIGN_CANDIDATE_SCHEMA || binding.contract !== SIGN_CANDIDATE_CONTRACT) bad("unrecognized contract");
  if (binding.rebuilt !== false) bad("signed bytes were rebuilt, not derived from the candidate");
  if (!Array.isArray(binding.files) || !binding.files.length) bad("no files");
  for (const file of binding.files) {
    for (const key of ["candidateSha256", "signedSha256"]) if (!/^[0-9a-f]{64}$/.test(file?.[key] ?? "")) bad(`${file?.path}.${key} must be a SHA-256`);
    if (!file.unsigned && file.candidateSha256 === file.signedSha256) bad(`${file.path} was not changed by signing`);
  }
  if (expected.platform && binding.platform !== expected.platform) bad(`platform ${binding.platform} is not ${expected.platform}`);
  if (expected.candidateSha256s) {
    const bound = new Set(binding.files.map((file) => file.candidateSha256));
    for (const sha of expected.candidateSha256s) if (!bound.has(sha)) bad(`candidate ${sha} is not bound`);
  }
  return binding;
}

export function main(args = process.argv.slice(2), options = {}) {
  const take = (flag) => {
    const index = args.indexOf(flag);
    if (index < 0) return undefined;
    const value = args[index + 1];
    if (!value || value.startsWith("--")) fail("SIGN_CANDIDATE_USAGE", `${flag} requires a value`);
    return value;
  };
  try {
    const result = signCandidate({
      platform: take("--platform"), artifact: take("--artifact"), receiptPath: take("--candidate-receipt"), outPath: take("--out"),
      manifestPath: take("--manifest"), publisher: take("--publisher"), dryRun: args.includes("--dry-run"), ...options,
    });
    console.log(result.dryRun ? `dry-run: candidate hashes verified; would sign ${result.files.map((file) => file.path).join(", ")}` : `signed ${result.files.length} candidate file(s) (${result.signing}); binding receipt written`);
  } catch (error) {
    console.error(`right-release sign-candidate: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main();
