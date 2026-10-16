import { createHash } from "node:crypto";
import { readFileSync, statSync, openSync, fstatSync, readSync, closeSync } from "node:fs";
import path from "node:path";
import { verifySealedRelease } from "./release-state.mjs";
import { verifyOwnershipEvidence } from "./ownership-evidence.mjs";
import { validateMediaSidecarReceipt } from "./media-sidecar.mjs";

export class PublicationError extends Error {
  constructor(code, message) { super(`[${code}] ${message}`); this.name = "PublicationError"; this.code = code; }
}
export const digestBytes = (bytes) => createHash("sha256").update(bytes).digest("hex");

// Read once. Upload boundaries consume these bytes, never reopen their source path.
export function readVerifiedArtifact(file, evidence) {
  let bytes, identity;
  try {
    const before = statSync(file);
    bytes = readFileSync(file);
    identity = statSync(file);
    if (!sameIdentity(before, identity)) throw new PublicationError("PUBLICATION_SOURCE_CHANGED", `artifact changed while reading: ${file}`);
  } catch (error) {
    if (error instanceof PublicationError) throw error;
    throw new PublicationError("PUBLICATION_ARTIFACT_MISSING", `artifact unreadable: ${file}`);
  }
  if (!/^[a-f0-9]{64}$/.test(evidence?.sha256 ?? "") || digestBytes(bytes) !== evidence.sha256) {
    throw new PublicationError("PUBLICATION_HASH_MISMATCH", `artifact sha256 mismatch: ${file}`);
  }
  if (evidence.sizeBytes !== undefined && bytes.length !== evidence.sizeBytes) {
    throw new PublicationError("PUBLICATION_SIZE_MISMATCH", `artifact size mismatch: ${file}`);
  }
  return { file, bytes, sha256: evidence.sha256, sizeBytes: bytes.length, identity };
}
function sameIdentity(a, b) {
  return a.isFile() && b.isFile() && ["dev", "ino", "size", "mtimeMs", "ctimeMs"].every((key) => a[key] === b[key]);
}
export function assertArtifactUnchanged(artifact) {
  let current;
  try { current = statSync(artifact.file); } catch { /* refusal below */ }
  if (!current || !sameIdentity(artifact.identity, current)) throw new PublicationError("PUBLICATION_SOURCE_CHANGED", `artifact replaced after verification: ${artifact.file}`);
  if (digestBytes(artifact.bytes) !== artifact.sha256 || artifact.bytes.length !== artifact.sizeBytes) throw new PublicationError("PUBLICATION_HASH_MISMATCH", `verified bytes changed: ${artifact.file}`);
  return artifact.bytes;
}

export function verifyPublicationRelease({ sealedDir, repoRoot, appRoot, commit, app, version, platform, configPath, releaseId }, deps = {}) {
  let sealed;
  try { sealed = verifySealedRelease(sealedDir); }
  catch (error) {
    const code = /hash mismatch/.test(error.message) ? "PUBLICATION_HASH_MISMATCH" : /size mismatch/.test(error.message) ? "PUBLICATION_SIZE_MISMATCH" : "PUBLICATION_SEAL_INVALID";
    throw new PublicationError(code, error.message);
  }
  const manifest = sealed.manifest;
  if (!/^[a-f0-9]{40}$/.test(commit ?? "") || manifest.commit !== commit
    || (app && manifest.app !== app) || (version && manifest.version !== version)
    || manifest.platform !== platform || (releaseId && manifest.releaseId !== releaseId)) throw new PublicationError("PUBLICATION_SOURCE_MISMATCH", "sealed release source or product identity mismatch");
  const ownership = (deps.verifyOwnership ?? verifyOwnershipEvidence)(manifest.ownershipEvidence, repoRoot, appRoot);
  if (!ownership.ok || manifest.ownershipEvidence?.commit !== commit) throw new PublicationError("PUBLICATION_OWNERSHIP_INVALID", `sealed ownership evidence rejected: ${ownership.reason ?? "commit mismatch"}`);
  if (!manifest.checkpoints?.includes("signed") || !manifest.checkpoints?.includes("hardened")) throw new PublicationError("PUBLICATION_SIGNATURE_MISSING", "signed & hardened seal checkpoints are required");
  if (configPath && digestBytes(readFileSync(configPath)) !== manifest.signing?.configSha256) throw new PublicationError("PUBLICATION_POLICY_MISMATCH", "release config differs from sealed signing policy");
  if (manifest.mediaSidecars !== undefined) {
    try { validateMediaSidecarReceipt(manifest.mediaSidecars); }
    catch (error) { throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", error.message); }
  }
  const receipts = manifest.signing?.receipts;
  if (!Array.isArray(receipts) || !receipts.length) throw new PublicationError("PUBLICATION_SIGNATURE_MISSING", "sealed signing receipts are required");
  let notarized = false;
  let signed = false;
  let androidVerified = false;
  for (const receipt of receipts) {
    const artifact = readVerifiedArtifact(path.resolve(appRoot, receipt.file), receipt);
    let value;
    try { value = JSON.parse(artifact.bytes); } catch { throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", "invalid sealed signing receipt"); }
    if (value.schema !== 1) throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", "malformed sealed signing receipt");
    if (value.contract !== undefined) {
      verifyAndroidPublicationReceipt(value, manifest, platform);
      androidVerified = true; signed = true;
      continue;
    }
    if (Array.isArray(value.files)) {
      if (!value.files.length || value.files.some((item) => !/^[a-f0-9]{64}$/.test(item.after?.sha256 ?? "") || item.timestampPresent !== true
        || (platform === "win" ? item.authenticode !== "Valid" || item.subject !== "CN=Damned Ventures LLC" : item.codesign !== "strict-valid" || item.identity !== value.identity))) throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", "sealed signature verification evidence rejected");
      signed = true;
    } else if (Array.isArray(value.embedded)) {
      if (!value.embedded.length || !/^[a-f0-9]{64}$/.test(value.rawExe?.signedSha256 ?? "") || value.rawExe.signedSha256 !== value.rawExe.verifiedSha256
        || value.embedded.some((item) => item.sha256 !== value.rawExe.signedSha256)) throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", "sealed installer embedding evidence rejected");
    } else if (!value.notarization) throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", "unrecognized sealed signing receipt");
    if (value.notarization) {
      if (value.notarization.status !== "Accepted" || !value.stapling?.length || value.stapling.some((item) => item.validated !== true)) throw new PublicationError("PUBLICATION_NOTARIZATION_INVALID", "notarization/stapling evidence rejected");
      if (!value.archive?.file || !value.archive.sha256) throw new PublicationError("PUBLICATION_NOTARIZATION_INVALID", "notarization archive binding missing");
      readVerifiedArtifact(path.resolve(appRoot, value.archive.file), value.archive);
      notarized = true;
    }
  }
  if (!signed) throw new PublicationError("PUBLICATION_SIGNATURE_MISSING", "sealed native signing evidence is required");
  if (platform === "mac" && !notarized) throw new PublicationError("PUBLICATION_NOTARIZATION_MISSING", "sealed notarization evidence is required");
  if (platform === "android" && !androidVerified) throw new PublicationError("PUBLICATION_SIGNATURE_MISSING", "sealed android AAB upload-key receipt is required");
  const artifacts = new Map();
  for (const entry of manifest.files ?? []) {
    if (typeof entry.name !== "string" || path.basename(entry.name) !== entry.name || artifacts.has(entry.name)) throw new PublicationError("PUBLICATION_SEAL_INVALID", "unsafe or duplicate sealed artifact name");
    artifacts.set(entry.name, readVerifiedArtifact(path.join(sealedDir, entry.name), entry));
  }
  if (!artifacts.size) throw new PublicationError("PUBLICATION_SEAL_INVALID", "sealed release has no artifacts");
  return { ...sealed, artifacts };
}

// Android AAB receipt (android-release.mjs: contract "android-aab-upload-key-v1"). The receipt was already read from the
// manifest's signing.receipts and hash-verified by the caller; here it must describe exactly the sealed "aab" file and be
// signed by the pinned upload certificate. Fails closed with a specific message on any mismatch.
const ANDROID_RECEIPT_CONTRACT = "android-aab-upload-key-v1";
function verifyAndroidPublicationReceipt(value, manifest, platform) {
  const invalid = (message) => { throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", message); };
  if (value.contract !== ANDROID_RECEIPT_CONTRACT) invalid(`unrecognized signing receipt contract: ${JSON.stringify(value.contract)}`);
  if (platform !== "android" || manifest.platform !== "android") invalid("android AAB receipt in a non-android release");
  const signer = value.signer?.certSha256;
  if (!/^[a-f0-9]{64}$/.test(signer ?? "")) invalid("android AAB receipt has no signer certificate sha256");
  if (value.signer.debugCertificate === true || /Android Debug/i.test(String(value.signer.subject ?? ""))) invalid("android AAB receipt is signed with the Android debug certificate");
  const pinned = manifest.signing?.uploadCertSha256;
  if (!/^[a-f0-9]{64}$/.test(pinned ?? "")) invalid("sealed signing policy has no pinned android upload certificate");
  if (signer !== pinned) invalid("android AAB signer certificate is not the pinned upload certificate");
  const aabs = (manifest.files ?? []).filter((entry) => entry.role === "aab");
  if (aabs.length !== 1) invalid(aabs.length ? "sealed release has more than one aab entry" : "sealed release has no aab entry");
  const [aab] = aabs;
  if (!/^[a-f0-9]{64}$/.test(value.file?.sha256 ?? "") || value.file.sha256 !== aab.sha256) invalid("android AAB receipt sha256 differs from the sealed aab entry");
  if (!Number.isInteger(value.file.sizeBytes) || value.file.sizeBytes !== aab.sizeBytes) invalid("android AAB receipt size differs from the sealed aab entry");
  if (value.file.name !== aab.name) invalid("android AAB receipt names a different file than the sealed aab entry");
}

// Bounded memory for large artifacts. Transport must read back against this digest.
export function verifyArtifactFile(file, evidence) {
  if (!/^[a-f0-9]{64}$/.test(evidence?.sha256 ?? "")) throw new PublicationError("PUBLICATION_SEAL_INVALID", "publication hash evidence is required");
  const fd = openSync(file, "r");
  try {
    const before = fstatSync(fd), hash = createHash("sha256"), chunk = Buffer.alloc(1024 * 1024);
    let sizeBytes = 0, count;
    while (sizeBytes < before.size && (count = readSync(fd, chunk, 0, Math.min(chunk.length, before.size - sizeBytes), null)) > 0) {
      hash.update(chunk.subarray(0, count)); sizeBytes += count;
    }
    if (!sameIdentity(before, fstatSync(fd))) throw new PublicationError("PUBLICATION_SOURCE_CHANGED", `artifact changed while hashing: ${file}`);
    if (hash.digest("hex") !== evidence.sha256) throw new PublicationError("PUBLICATION_HASH_MISMATCH", `artifact sha256 mismatch: ${file}`);
    if (evidence.sizeBytes !== undefined && sizeBytes !== evidence.sizeBytes) throw new PublicationError("PUBLICATION_SIZE_MISMATCH", `artifact size mismatch: ${file}`);
    return { file, sha256: evidence.sha256, sizeBytes, identity: before };
  } finally { closeSync(fd); }
}
