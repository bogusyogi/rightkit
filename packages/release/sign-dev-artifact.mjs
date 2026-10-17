#!/usr/bin/env node
// right-release sign-dev-artifact: Developer ID sign an EXISTING, already-qualified CI dev artifact in place (Orthic-Labs/rightkit#257).
// It never builds, notarizes, publishes or installs. Steps: verify the artifact's recorded hashes, sign each manifest file with its fixed
// codesign identifier (mac-sign-core signMacosEntries, the provisioned Developer ID only), verify strictly, read back Identifier and
// TeamIdentifier, refresh the payload metadata, and write evidence JSON.
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { MacosDevelopmentSigningError, resolveMacosDevelopmentSigning } from "./development-signing.mjs";
import { SIGN_MANIFEST_SCHEMA, parseSignManifest, signMacosEntries } from "./mac-sign-core.mjs";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { MACOS_PORTABLE_SIGNING_CONTRACT } from "./macos-signing-contract.mjs";
import { runMacCodesignCommands } from "./macos-signing-session.mjs";

export const DEV_ARTIFACT_PAYLOAD_SCHEMA = 1;
export const DEV_ARTIFACT_EVIDENCE_SCHEMA = 1;
export const DEFAULT_PAYLOAD_METADATA = "payload.json";

export class SignDevArtifactError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "SignDevArtifactError";
    this.code = code;
  }
}

const fail = (code, message) => { throw new SignDevArtifactError(code, message); };
const toPosix = (value) => value.split(path.sep).join("/");

/** A path relative to `base` that stays inside it: no absolute path, no `..`, not empty. Returns the absolute path. */
function contained(base, relative, label) {
  if (typeof relative !== "string" || !relative || path.isAbsolute(relative) || path.win32.isAbsolute(relative) || relative.split(/[\\/]/).includes("..")) {
    fail("SIGN_DEV_ARTIFACT_PATH_INVALID", `${label} must be a relative path inside the artifact directory: ${JSON.stringify(relative)}`);
  }
  return path.resolve(base, relative);
}

/**
 * Sign manifest for this command: `{ schema: 1, entries: [{ file, identifier, entitlements?, options?, hardenedRuntime? }] }`.
 * `file` is relative to the artifact directory (a loose executable or the installer .app); `identifier` is required on every entry.
 * Entries sign in the given order, so list nested code before the bundle that contains it. Returns absolute-path entries.
 */
export function readDevArtifactSignManifest(file, artifact) {
  let raw;
  try { raw = JSON.parse(readFileSync(file, "utf8")); } catch { fail("SIGN_DEV_ARTIFACT_MANIFEST_INVALID", `sign manifest is not readable JSON: ${file}`); }
  if (raw?.schema !== SIGN_MANIFEST_SCHEMA || !Array.isArray(raw.entries) || !raw.entries.length) {
    fail("SIGN_DEV_ARTIFACT_MANIFEST_INVALID", `sign manifest must be { schema: ${SIGN_MANIFEST_SCHEMA}, entries: [...] } with at least one entry`);
  }
  const absolute = raw.entries.map((entry, index) => {
    if (!entry || typeof entry !== "object") fail("SIGN_DEV_ARTIFACT_MANIFEST_INVALID", `sign manifest entry ${index} must be an object`);
    if (entry.identifier === undefined) fail("SIGN_DEV_ARTIFACT_IDENTIFIER_REQUIRED", `sign manifest entry ${index} (${entry.file}) needs a fixed identifier`);
    return {
      ...entry,
      file: contained(artifact, entry.file, `sign manifest entry ${index} file`),
      ...(entry.entitlements !== undefined ? { entitlements: contained(artifact, entry.entitlements, `sign manifest entry ${index} entitlements`) } : {}),
    };
  });
  const entries = parseSignManifest(JSON.stringify({ schema: SIGN_MANIFEST_SCHEMA, entries: absolute }));
  const seen = new Set();
  entries.forEach((entry, index) => {
    if (seen.has(entry.file)) fail("SIGN_DEV_ARTIFACT_MANIFEST_INVALID", `sign manifest lists ${entry.file} twice`);
    seen.add(entry.file);
    for (const earlier of entries.slice(0, index)) {
      if (entry.file.startsWith(earlier.file + path.sep)) fail("SIGN_DEV_ARTIFACT_ORDER_INVALID", `${path.relative(artifact, earlier.file)} contains ${path.relative(artifact, entry.file)} but is listed first; sign nested code before its bundle`);
    }
  });
  return entries;
}

/** Read the artifact's payload metadata `{ schema: 1, files: [{ path, sha256, ... }] }` and verify every recorded hash against disk. */
export function verifyDevArtifactPayload(artifact, metadataFile) {
  let metadata;
  try { metadata = JSON.parse(readFileSync(metadataFile, "utf8")); } catch { fail("SIGN_DEV_ARTIFACT_METADATA_INVALID", `payload metadata is not readable JSON: ${metadataFile}`); }
  if (metadata?.schema !== DEV_ARTIFACT_PAYLOAD_SCHEMA || !Array.isArray(metadata.files) || !metadata.files.length) {
    fail("SIGN_DEV_ARTIFACT_METADATA_INVALID", `payload metadata must be { schema: ${DEV_ARTIFACT_PAYLOAD_SCHEMA}, files: [...] } with at least one file: ${metadataFile}`);
  }
  const recorded = new Map();
  for (const item of metadata.files) {
    if (!/^[0-9a-f]{64}$/.test(item?.sha256 ?? "")) fail("SIGN_DEV_ARTIFACT_METADATA_INVALID", `payload metadata entry ${JSON.stringify(item?.path)} has no sha256`);
    const absolute = contained(artifact, item.path, "payload metadata path");
    if (!existsSync(absolute)) fail("SIGN_DEV_ARTIFACT_MISSING", `payload file is missing: ${item.path}`);
    const actual = fileEvidence(absolute).sha256;
    if (actual !== item.sha256) fail("SIGN_DEV_ARTIFACT_HASH_MISMATCH", `recorded hash does not match ${item.path}: recorded ${item.sha256}, found ${actual}. Refusing to sign bytes that were not qualified.`);
    recorded.set(absolute, item);
  }
  return { metadata, recorded };
}

const defaultInspect = (run, env) => (command, args) => {
  const result = run(command, args, { env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true, shell: false });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  if (result.error || result.status !== 0) fail("SIGN_DEV_ARTIFACT_INSPECT_FAILED", `${command} ${args.join(" ")} failed: ${output.trim() || result.error?.message}`);
  return output;
};

/**
 * Sign an existing dev artifact. Injectable for tests: `resolveSigning`, `signOne`, `inspect`, `evidenceOf`.
 * Returns the evidence object (also written to `evidencePath`). With `dryRun` only the hash verification and manifest parsing run.
 */
export function signDevArtifact({
  artifact, manifestPath, metadataPath, evidencePath, dryRun = false,
  env = process.env, run = spawnSync, platform = process.platform,
  resolveSigning = (options) => resolveMacosDevelopmentSigning(options),
  signOne = (codesignArgs) => runMacCodesignCommands([codesignArgs], { env, run })[0],
  inspect = defaultInspect(run, env), evidenceOf = fileEvidence,
} = {}) {
  if (!artifact || !manifestPath) fail("SIGN_DEV_ARTIFACT_USAGE", "usage: right-release sign-dev-artifact --artifact <dir> --manifest <sign-manifest.json> [--metadata <payload.json>] [--evidence <path>] [--dry-run]");
  const root = path.resolve(artifact);
  if (!existsSync(root)) fail("SIGN_DEV_ARTIFACT_MISSING", `artifact directory does not exist: ${root}`);
  const metadataFile = path.resolve(metadataPath ?? path.join(root, DEFAULT_PAYLOAD_METADATA));
  const entries = readDevArtifactSignManifest(path.resolve(manifestPath), root);
  const before = verifyDevArtifactPayload(root, metadataFile);
  for (const entry of entries) {
    if (!existsSync(entry.file)) fail("SIGN_DEV_ARTIFACT_MISSING", `sign manifest file is missing: ${path.relative(root, entry.file)}`);
    if (!before.recorded.has(entry.file)) fail("SIGN_DEV_ARTIFACT_NOT_RECORDED", `${path.relative(root, entry.file)} is not in the payload metadata, so its hash was never recorded`);
    if (entry.entitlements && !existsSync(entry.entitlements)) fail("SIGN_DEV_ARTIFACT_MISSING", `entitlements file is missing: ${path.relative(root, entry.entitlements)}`);
  }
  if (dryRun) return { schema: DEV_ARTIFACT_EVIDENCE_SCHEMA, dryRun: true, files: entries.map((entry) => ({ path: toPosix(path.relative(root, entry.file)), identifier: entry.identifier })) };
  if (platform !== "darwin") fail("SIGN_DEV_ARTIFACT_PLATFORM", "sign-dev-artifact must run on macOS");
  if (!evidencePath) fail("SIGN_DEV_ARTIFACT_USAGE", "--evidence requires a path");

  // Same Developer ID resolution as dev signing: no ad hoc and no Apple Development fallback.
  const signing = resolveSigning({ root, bundle: root, descriptor: {}, config: {}, env, run });
  if (signing.adhoc || !/^Developer ID Application:/.test(signing.identity ?? "")) {
    throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED", `sign-dev-artifact needs the provisioned Developer ID Application identity; got ${JSON.stringify(signing.identity)}. There is no ad hoc or Apple Development fallback.`, { identity: signing.identity });
  }
  const { identity, teamIdentifier } = signing;

  const sha256Before = new Map(entries.map((entry) => [entry.file, evidenceOf(entry.file).sha256]));
  const signed = signMacosEntries(entries, { identity, signOne, inspect, evidence: evidenceOf, bundle: root });
  const files = signed.map((record) => {
    const rel = toPosix(path.relative(root, record.file));
    const details = inspect("codesign", ["-dvv", record.file]);
    const team = /^TeamIdentifier=(.+)$/m.exec(details)?.[1]?.trim();
    const identifier = /^Identifier=(.+)$/m.exec(details)?.[1]?.trim();
    if (!team || team !== teamIdentifier) fail("SIGN_DEV_ARTIFACT_TEAM_MISMATCH", `${rel}: expected TeamIdentifier=${teamIdentifier}, found ${team ?? "missing"}`);
    if (!identifier || identifier !== record.identifier) fail("SIGN_DEV_ARTIFACT_IDENTIFIER_MISMATCH", `${rel}: expected Identifier=${record.identifier}, found ${identifier ?? "missing"}`);
    const requirement = inspect("codesign", ["-d", "-r-", record.file]);
    const designatedRequirement = /^designated => (.+)$/m.exec(requirement)?.[1]?.trim();
    if (!designatedRequirement || /\bcdhash\b/.test(designatedRequirement)) fail("SIGN_DEV_ARTIFACT_UNSTABLE_REQUIREMENT", `${rel}: designated requirement ${JSON.stringify(designatedRequirement)} is not identity-based, so privacy grants would not survive a rebuild`);
    return { path: rel, sha256Before: sha256Before.get(record.file), sha256After: record.after.sha256, identifier, teamIdentifier: team, designatedRequirement };
  });

  // Refresh the payload metadata: every recorded hash is recomputed (a signed bundle changes its tree digest too).
  const refreshed = before.metadata.files.map((item) => {
    const evidence = evidenceOf(contained(root, item.path, "payload metadata path"));
    return { ...item, sha256: evidence.sha256, ...(item.sizeBytes !== undefined ? { sizeBytes: evidence.sizeBytes } : {}) };
  });
  const metadata = { ...before.metadata, files: refreshed, signing: { contract: MACOS_PORTABLE_SIGNING_CONTRACT, identity, teamIdentifier, signedPaths: files.map((file) => file.path) } };
  const metadataBefore = evidenceOf(metadataFile).sha256;
  const staging = `${metadataFile}.signing`;
  writeFileSync(staging, `${JSON.stringify(metadata, null, 2)}\n`);
  renameSync(staging, metadataFile);
  verifyDevArtifactPayload(root, metadataFile);

  const result = {
    schema: DEV_ARTIFACT_EVIDENCE_SCHEMA, signingContract: MACOS_PORTABLE_SIGNING_CONTRACT, identity, teamIdentifier, notarized: false, built: false, published: false,
    artifact: root, metadata: { path: toPosix(path.relative(root, metadataFile)), sha256Before: metadataBefore, sha256After: evidenceOf(metadataFile).sha256 }, files,
  };
  const absoluteEvidence = path.resolve(evidencePath);
  mkdirSync(path.dirname(absoluteEvidence), { recursive: true });
  writeFileSync(absoluteEvidence, `${JSON.stringify(result, null, 2)}\n`);
  return result;
}

export function main(args = process.argv.slice(2), options = {}) {
  const take = (flag) => {
    const index = args.indexOf(flag);
    if (index < 0) return undefined;
    const value = args[index + 1];
    if (!value || value.startsWith("--")) fail("SIGN_DEV_ARTIFACT_USAGE", `${flag} requires a value`);
    return value;
  };
  try {
    const result = signDevArtifact({
      artifact: take("--artifact"), manifestPath: take("--manifest"), metadataPath: take("--metadata"), evidencePath: take("--evidence"),
      dryRun: args.includes("--dry-run"), ...options,
    });
    console.log(result.dryRun ? `dry-run: recorded hashes verified; would sign ${result.files.map((file) => file.path).join(", ")}` : `signed ${result.files.length} file(s) as ${result.teamIdentifier}: ${result.files.map((file) => `${file.path} (${file.identifier})`).join(", ")}`);
  } catch (error) {
    console.error(`right-release sign-dev-artifact: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main();
