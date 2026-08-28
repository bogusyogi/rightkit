import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";

export const NATIVE_FINALIZATION_SCHEMA = 1;
export const NATIVE_PROVENANCE_SCHEME = "rightkit-release";

const PLACEHOLDER = /(?:placeholder|fabricat|pending|unknown|example|dummy|todo|changeme)/i;
const STATUS = new Set(["complete", "completed", "pass", "passed", "verified"]);

export function sha256Bytes(value) {
  return createHash("sha256").update(value).digest("hex");
}

export function sha256File(file) {
  return sha256Bytes(readFileSync(file));
}

export function normalizeDigest(value, label = "digest") {
  const digest = String(value ?? "").toLowerCase().replace(/^sha256:/, "");
  if (!/^[0-9a-f]{64}$/.test(digest)) throw new Error(`${label} must be a SHA-256 hex digest`);
  return digest;
}

export function canonicalSignedArtifacts(artifacts = []) {
  if (!Array.isArray(artifacts)) throw new TypeError("signedArtifacts must be an array");
  return artifacts.map((artifact) => {
    if (typeof artifact === "string") return { path: artifact };
    if (!artifact || typeof artifact !== "object") throw new TypeError("signed artifact must be a path or object");
    const value = { ...artifact };
    if (!value.path && value.file) value.path = value.file;
    if (!value.path) throw new Error("signed artifact path is required");
    if (value.sha256 != null) value.sha256 = normalizeDigest(value.sha256, `signed artifact ${value.path}`);
    if (value.sizeBytes != null && (!Number.isSafeInteger(value.sizeBytes) || value.sizeBytes < 0)) {
      throw new Error(`signed artifact size must be a non-negative integer: ${value.path}`);
    }
    return {
      path: String(value.path).replaceAll("\\", "/"),
      ...(value.sha256 ? { sha256: value.sha256 } : {}),
      ...(value.sizeBytes != null ? { sizeBytes: value.sizeBytes } : {}),
    };
  }).sort((left, right) => left.path.localeCompare(right.path));
}

export function canonicalPackageIdentity(identity = {}) {
  if (typeof identity === "string") identity = { name: identity };
  if (!identity || typeof identity !== "object" || Array.isArray(identity)) throw new TypeError("package identity must be an object");
  const result = {};
  for (const key of ["name", "path", "kind", "version", "target", "platform", "architecture"]) {
    if (identity[key] != null) result[key] = String(identity[key]).replaceAll("\\", "/");
  }
  if (Array.isArray(identity.artifacts)) {
    result.artifacts = identity.artifacts.map((value) => String(value).replaceAll("\\", "/")).sort();
  }
  if (!Object.keys(result).length) throw new Error("package identity must include a stable name, path, kind, or artifact list");
  return result;
}

/**
 * Mint identity from signed files and package identity.  Archive bytes are
 * included when available; before packaging, identity remains bound to the
 * signed artifact set plus stable package identity and is completed later by
 * createNativeFinalizationReceipt.
 */
export function mintNativeProvenance({
  app,
  version,
  platform,
  architecture,
  targetTriple,
  signedArtifacts = [],
  packageIdentity,
  archiveSha256,
} = {}) {
  const identity = canonicalPackageIdentity(packageIdentity);
  const signed = canonicalSignedArtifacts(signedArtifacts);
  if (!app || !version || !platform || !architecture) throw new Error("native provenance identity is incomplete");
  const archive = archiveSha256 == null ? undefined : normalizeDigest(archiveSha256, "archiveSha256");
  const payload = {
    app: String(app),
    version: String(version),
    platform: String(platform),
    architecture: String(architecture),
    ...(targetTriple ? { targetTriple: String(targetTriple) } : {}),
    signedArtifacts: signed,
    packageIdentity: identity,
    ...(archive ? { archiveSha256: archive } : {}),
  };
  const digest = sha256Bytes(JSON.stringify(payload));
  const pathParts = [app, version, platform, architecture].map((value) => encodeURIComponent(String(value)));
  return `${NATIVE_PROVENANCE_SCHEME}://${pathParts.join("/")}/${digest}`;
}

export function validateNativeProvenance(value, { expectedScheme = NATIVE_PROVENANCE_SCHEME } = {}) {
  const provenance = String(value ?? "");
  if (!provenance.startsWith(`${expectedScheme}://`)) {
    throw new Error(`native finalizer provenance must use ${expectedScheme}://`);
  }
  if (PLACEHOLDER.test(provenance)) throw new Error("native finalizer returned placeholder provenance");
  const digest = provenance.slice(provenance.lastIndexOf("/") + 1);
  normalizeDigest(digest, "native provenance identity");
  return provenance;
}

export function finalizationArchive(file, { root } = {}) {
  const resolved = root && !path.isAbsolute(String(file)) ? path.resolve(root, file) : path.resolve(String(file));
  if (!existsSync(resolved)) throw new Error(`native finalization archive missing: ${resolved}`);
  const stat = statSync(resolved);
  if (!stat.isFile()) throw new Error(`native finalization archive is not a file: ${resolved}`);
  return {
    path: root ? path.relative(root, resolved).replaceAll("\\", "/") : resolved.replaceAll("\\", "/"),
    sha256: sha256File(resolved),
    sizeBytes: stat.size,
  };
}

export function createNativeFinalizationReceipt({
  app,
  version,
  platform,
  architecture,
  targetTriple,
  signedArtifacts = [],
  packageIdentity,
  archive,
  archivePath,
  archiveSha256,
  finalizerOutput,
  root,
  now = new Date().toISOString(),
} = {}) {
  const identity = canonicalPackageIdentity(packageIdentity);
  const signed = canonicalSignedArtifacts(signedArtifacts);
  let resolvedArchive = archive;
  if (!resolvedArchive && archivePath) resolvedArchive = finalizationArchive(archivePath, { root });
  if (resolvedArchive) {
    const archiveFile = resolvedArchive.path || resolvedArchive.file;
    const digest = resolvedArchive.sha256 ?? archiveSha256;
    if (!digest) throw new Error("native finalization archive digest is required");
    if (archiveFile && root) {
      const observed = finalizationArchive(archiveFile, { root });
      if (normalizeDigest(digest, "archiveSha256") !== observed.sha256) throw new Error("native finalization archive digest does not match archive bytes");
      resolvedArchive = observed;
    }
    resolvedArchive = {
      ...resolvedArchive,
      ...(archiveFile ? { path: String(archiveFile).replaceAll("\\", "/") } : {}),
      sha256: normalizeDigest(digest, "archiveSha256"),
      ...(resolvedArchive.sizeBytes != null ? { sizeBytes: resolvedArchive.sizeBytes } : {}),
    };
  } else if (archiveSha256) {
    resolvedArchive = { sha256: normalizeDigest(archiveSha256, "archiveSha256") };
  } else {
    throw new Error("native finalization receipt requires final archive digest");
  }
  const provenance = mintNativeProvenance({
    app,
    version,
    platform,
    architecture,
    targetTriple,
    signedArtifacts: signed,
    packageIdentity: identity,
    archiveSha256: resolvedArchive.sha256,
  });
  const output = finalizerOutput && typeof finalizerOutput === "object" ? finalizerOutput : {};
  if (output.provenance != null) validateNativeProvenance(output.provenance);
  if (output.signedArtifacts != null || output.signed != null) {
    const observed = canonicalSignedArtifacts(output.signedArtifacts ?? output.signed);
    if (JSON.stringify(observed) !== JSON.stringify(signed)) throw new Error("native finalizer signed artifact identity mismatch");
  }
  if (output.packageIdentity != null || output.package != null) {
    const observed = canonicalPackageIdentity(output.packageIdentity ?? output.package);
    if (JSON.stringify(observed) !== JSON.stringify(identity)) throw new Error("native finalizer package identity mismatch");
  }
  const outputArchiveDigest = output.archiveSha256 ?? output.archive?.sha256 ?? output.archive?.digest;
  if (outputArchiveDigest != null && normalizeDigest(outputArchiveDigest, "finalizer archiveSha256") !== resolvedArchive.sha256) {
    throw new Error("native finalizer archive digest does not match final archive");
  }
  const receipt = {
    schema: NATIVE_FINALIZATION_SCHEMA,
    kind: "rightkit-native-finalization",
    status: "verified",
    app: String(app),
    version: String(version),
    platform: String(platform),
    architecture: String(architecture),
    ...(targetTriple ? { targetTriple: String(targetTriple) } : {}),
    provenance,
    signedArtifacts: signed,
    packageIdentity: identity,
    archive: resolvedArchive,
    archiveSha256: resolvedArchive.sha256,
    finalizedAt: now,
  };
  validateNativeFinalizationOutput(receipt, { requireArchive: true });
  return receipt;
}

export function validateNativeFinalizationOutput(output, {
  expectedProvenance,
  expectedArchiveSha256,
  expectedSignedArtifacts,
  expectedPackageIdentity,
  requireArchive = false,
} = {}) {
  if (!output || typeof output !== "object" || Array.isArray(output)) throw new Error("native finalizer output must be an object");
  if (output.schema != null && output.schema !== NATIVE_FINALIZATION_SCHEMA) throw new Error(`unsupported native finalizer schema: ${output.schema}`);
  const status = String(output.status ?? "").toLowerCase();
  if (!STATUS.has(status)) throw new Error("native finalizer output must have complete, pass, or verified status");
  const provenance = validateNativeProvenance(output.provenance);
  if (expectedProvenance && provenance !== expectedProvenance) throw new Error("native finalizer provenance identity mismatch");
  const signed = canonicalSignedArtifacts(output.signedArtifacts ?? output.signed ?? []);
  if (expectedSignedArtifacts) {
    const expected = canonicalSignedArtifacts(expectedSignedArtifacts);
    if (JSON.stringify(signed) !== JSON.stringify(expected)) throw new Error("native finalizer signed artifact identity mismatch");
  }
  if (expectedPackageIdentity) {
    const actual = canonicalPackageIdentity(output.packageIdentity ?? output.package ?? {});
    const expected = canonicalPackageIdentity(expectedPackageIdentity);
    if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error("native finalizer package identity mismatch");
  }
  const archiveValue = output.archiveSha256 ?? output.archive?.sha256 ?? output.archive?.digest;
  if (requireArchive && !archiveValue) throw new Error("native finalizer output must bind final archive digest");
  const archiveSha256 = archiveValue ? normalizeDigest(archiveValue, "archiveSha256") : undefined;
  if (expectedArchiveSha256 && archiveSha256 !== normalizeDigest(expectedArchiveSha256, "expected archiveSha256")) {
    throw new Error("native finalizer archive digest mismatch");
  }
  if (output.archive?.path != null && PLACEHOLDER.test(String(output.archive.path))) throw new Error("native finalizer archive path is a placeholder");
  return { ...output, provenance, signedArtifacts: signed, ...(archiveSha256 ? { archiveSha256 } : {}) };
}

export const validateNativeFinalization = validateNativeFinalizationOutput;
export const mintRightKitProvenance = mintNativeProvenance;

export function readNativeFinalizationOutput(file) {
  if (!file) throw new TypeError("native finalizer output path is required");
  if (!existsSync(file)) throw new Error(`native finalizer output missing: ${file}`);
  let output;
  try { output = JSON.parse(readFileSync(file, "utf8")); } catch (error) { throw new Error(`native finalizer output is not valid JSON: ${file}: ${error.message}`); }
  return validateNativeFinalizationOutput(output);
}

export const finalizeNativeRelease = createNativeFinalizationReceipt;

export function resolveNativeFinalizerConfig(nativeAssembly) {
  const finalizer = nativeAssembly?.finalizer;
  if (!finalizer) return undefined;
  if (typeof finalizer === "string") return { cmd: finalizer, args: [] };
  if (!finalizer || typeof finalizer !== "object" || Array.isArray(finalizer)) throw new Error("nativeAssembly.finalizer must be a command object");
  const command = finalizer.command ?? finalizer;
  if (!command?.cmd || typeof command.cmd !== "string" || !Array.isArray(command.args ?? [])) throw new Error("nativeAssembly.finalizer must declare cmd and args");
  return {
    ...finalizer,
    cmd: command.cmd,
    args: [...(command.args ?? [])].map(String),
    output: finalizer.output ?? finalizer.receipt ?? finalizer.receiptPath ?? finalizer.outputFile,
  };
}
