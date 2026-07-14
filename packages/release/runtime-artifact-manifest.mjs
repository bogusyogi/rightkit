import { sign as edSign, verify as edVerify } from "node:crypto";

export const RUNTIME_ARTIFACT_SCHEMA = 1;
export const RUNTIME_ARTIFACT_KINDS = Object.freeze([
  "asr-model",
  "ocr-model",
  "ort-runtime",
  "media-runtime",
  "tokenizer",
  "preprocessing",
]);
export const RIGHT_SUITE_APP_KEYS = Object.freeze([
  "viewright",
  "heardright",
  "mailright",
  "scraperight",
  "coderight",
]);

const TARGET_OSES = new Set(["windows", "darwin", "linux", "ios"]);
const TARGET_ARCHES = new Set(["x86_64", "aarch64", "universal"]);
const VERSION_FIELDS = Object.freeze([
  "runtime",
  "model",
  "tokenizer",
  "preprocessing",
  "license",
  "provenance",
]);
const SHA256 = /^[0-9a-f]{64}$/;

export function buildRuntimeArtifactManifest(input) {
  const entitlement = cloneObject(input.entitlement);
  const target = cloneObject(input.target);
  const manifest = {
    schema: RUNTIME_ARTIFACT_SCHEMA,
    artifactKind: input.artifactKind,
    entitlement,
    distribution: cloneObject(input.distribution),
    target,
    object: {
      r2Key: "",
      filename: input.filename,
      sha256: input.sha256,
      sizeBytes: input.sizeBytes,
    },
    pointerKey: "",
    versions: cloneObject(input.versions),
    provenance: cloneObject(input.provenance),
    promotion: cloneObject(input.promotion),
  };
  if (manifest.distribution?.delivery === "bundled") {
    manifest.object.r2Key = null;
    manifest.pointerKey = null;
  } else {
    manifest.object.r2Key = runtimeArtifactObjectKey(manifest);
    manifest.pointerKey = runtimeArtifactManifestPointerKey(manifest);
  }
  return validateRuntimeArtifactManifest(manifest);
}

export function runtimeArtifactObjectKey(value) {
  const appKey = value?.entitlement?.appKey;
  const digest = value?.object?.sha256 ?? value?.sha256;
  const filename = value?.object?.filename ?? value?.filename;
  assertAppKey(appKey, "entitlement.appKey");
  assertSha256(digest, "object.sha256");
  assertFilename(filename);
  return `${appKey}/runtime-artifacts/objects/sha256/${digest}/${filename}`;
}

export function runtimeArtifactManifestPointerKey(value) {
  const appKey = value?.entitlement?.appKey;
  const kind = value?.artifactKind;
  const os = value?.target?.os;
  const arch = value?.target?.arch;
  assertAppKey(appKey, "entitlement.appKey");
  if (!RUNTIME_ARTIFACT_KINDS.includes(kind)) fail("artifactKind is unsupported");
  if (!TARGET_OSES.has(os)) fail("target.os is unsupported");
  if (!TARGET_ARCHES.has(arch)) fail("target.arch is unsupported");
  return `${appKey}/runtime-artifacts/${kind}/${os}/${arch}/current/manifest.json`;
}

export function validateRuntimeArtifactManifest(value) {
  assertPlainObject(value, "manifest");
  assertExactKeys(value, [
    "schema", "artifactKind", "entitlement", "distribution", "target", "object", "pointerKey",
    "versions", "provenance", "promotion",
  ], "manifest");
  if (value.schema !== RUNTIME_ARTIFACT_SCHEMA) fail("schema must be 1");
  if (!RUNTIME_ARTIFACT_KINDS.includes(value.artifactKind)) fail("artifactKind is unsupported");

  assertPlainObject(value.entitlement, "entitlement");
  assertExactKeys(value.entitlement, ["appKey", "tier"], "entitlement");
  assertAppKey(value.entitlement.appKey, "entitlement.appKey");
  if (!["pro", "public", "bundled"].includes(value.entitlement.tier)) fail("entitlement.tier is unsupported");

  assertPlainObject(value.distribution, "distribution");
  assertExactKeys(value.distribution, ["delivery", "bucket"], "distribution");
  const laneContracts = {
    "private-r2": { tier: "pro", bucket: "rightapps-updates" },
    "public-r2": { tier: "public", bucket: "rightapps-downloads" },
    bundled: { tier: "bundled", bucket: null },
  };
  const lane = laneContracts[value.distribution.delivery];
  if (!lane) fail("distribution.delivery is unsupported");
  if (value.entitlement.tier !== lane.tier || value.distribution.bucket !== lane.bucket) {
    fail("distribution lane contradicts entitlement tier or bucket");
  }
  if (["asr-model", "ocr-model", "tokenizer"].includes(value.artifactKind)
      && (value.distribution.delivery !== "private-r2" || value.entitlement.tier !== "pro")) {
    fail("model artifacts must use private-r2 with Pro entitlement");
  }

  assertPlainObject(value.target, "target");
  assertExactKeys(value.target, ["os", "arch"], "target");
  if (!TARGET_OSES.has(value.target.os)) fail("target.os is unsupported");
  if (!TARGET_ARCHES.has(value.target.arch)) fail("target.arch is unsupported");

  assertPlainObject(value.object, "object");
  assertExactKeys(value.object, ["r2Key", "filename", "sha256", "sizeBytes"], "object");
  assertFilename(value.object.filename);
  assertSha256(value.object.sha256, "object.sha256");
  if (!Number.isSafeInteger(value.object.sizeBytes) || value.object.sizeBytes <= 0) fail("object.sizeBytes must be a positive safe integer");
  if (value.distribution.delivery === "bundled") {
    if (value.object.r2Key !== null || value.pointerKey !== null) fail("bundled artifacts must not claim R2 keys");
  } else {
    if (value.object.r2Key !== runtimeArtifactObjectKey(value)) fail("object.r2Key contradicts its immutable SHA-256 object key");
    if (value.pointerKey !== runtimeArtifactManifestPointerKey(value)) fail("pointerKey must be the replace-only stable current manifest key");
  }

  assertPlainObject(value.versions, "versions");
  assertExactKeys(value.versions, VERSION_FIELDS, "versions");
  for (const field of VERSION_FIELDS) assertNonempty(value.versions[field], `versions.${field}`);

  assertPlainObject(value.provenance, "provenance");
  assertExactKeys(value.provenance, ["source", "sourceRevision", "licenseId", "noticeSha256"], "provenance");
  assertHttpsUrl(value.provenance.source, "provenance.source");
  assertNonempty(value.provenance.sourceRevision, "provenance.sourceRevision");
  assertNonempty(value.provenance.licenseId, "provenance.licenseId");
  assertSha256(value.provenance.noticeSha256, "provenance.noticeSha256");
  if (normalizeLicense(value.versions.license) !== normalizeLicense(value.provenance.licenseId)) {
    fail("license version contradicts provenance licenseId");
  }

  assertPlainObject(value.promotion, "promotion");
  assertExactKeys(value.promotion, ["authorityAppKey", "promotionId", "promotedAt", "evidenceSha256"], "promotion");
  assertAppKey(value.promotion.authorityAppKey, "promotion.authorityAppKey");
  assertNonempty(value.promotion.promotionId, "promotion.promotionId");
  const promotedAt = new Date(value.promotion.promotedAt);
  if (Number.isNaN(promotedAt.getTime()) || promotedAt.toISOString() !== value.promotion.promotedAt) fail("promotion.promotedAt must be an exact ISO-8601 UTC timestamp");
  assertSha256(value.promotion.evidenceSha256, "promotion.evidenceSha256");
  return value;
}

export function canonicalRuntimeArtifactManifest(manifest) {
  validateRuntimeArtifactManifest(manifest);
  return JSON.stringify(sortDeep(manifest));
}

export function signRuntimeArtifactManifest(manifest, privateKey, keyId) {
  assertNonempty(keyId, "signature.keyId");
  const canonical = Buffer.from(canonicalRuntimeArtifactManifest(manifest), "utf8");
  return {
    manifest,
    signature: {
      algorithm: "Ed25519",
      keyId,
      value: edSign(null, canonical, privateKey).toString("base64url"),
    },
  };
}

export function verifyRuntimeArtifactEnvelope(value, publicKey) {
  assertPlainObject(value, "envelope");
  assertExactKeys(value, ["manifest", "signature"], "envelope");
  validateRuntimeArtifactManifest(value.manifest);
  assertPlainObject(value.signature, "signature");
  assertExactKeys(value.signature, ["algorithm", "keyId", "value"], "signature");
  if (value.signature.algorithm !== "Ed25519") fail("signature algorithm must be Ed25519");
  assertNonempty(value.signature.keyId, "signature.keyId");
  assertNonempty(value.signature.value, "signature.value");
  let signature;
  try {
    signature = Buffer.from(value.signature.value, "base64url");
  } catch {
    fail("signature is not base64url");
  }
  if (signature.length !== 64 || !edVerify(null, Buffer.from(canonicalRuntimeArtifactManifest(value.manifest), "utf8"), publicKey, signature)) {
    fail("signature verification failed");
  }
  return value.manifest;
}

function sortDeep(value) {
  if (Array.isArray(value)) return value.map(sortDeep);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, sortDeep(value[key])]));
  }
  return value;
}

function assertExactKeys(value, expected, label) {
  const expectedSet = new Set(expected);
  for (const key of Object.keys(value)) if (!expectedSet.has(key)) fail(`${label} has unknown field ${key}`);
  for (const key of expected) if (!Object.hasOwn(value, key)) fail(`${label}.${key} is required`);
}

function assertPlainObject(value, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value) || Object.getPrototypeOf(value) !== Object.prototype) fail(`${label} must be a plain object`);
}

function assertAppKey(value, label) {
  if (!RIGHT_SUITE_APP_KEYS.includes(value)) fail(`${label} is not a Right Suite app`);
}

function assertFilename(value) {
  assertNonempty(value, "object.filename");
  if (value === "." || value === ".." || value.includes("/") || value.includes("\\") || value.includes("..")) fail("object.filename must be one safe basename");
}

function assertSha256(value, label) {
  if (typeof value !== "string" || !SHA256.test(value)) fail(`${label} must be lowercase SHA-256`);
}

function assertNonempty(value, label) {
  if (typeof value !== "string" || value.trim() !== value || value.length === 0) fail(`${label} must be a nonempty exact string`);
}

function assertHttpsUrl(value, label) {
  assertNonempty(value, label);
  let url;
  try { url = new URL(value); } catch { fail(`${label} must be an HTTPS URL`); }
  if (url.protocol !== "https:") fail(`${label} must be an HTTPS URL`);
}

function normalizeLicense(value) {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, "");
}

function cloneObject(value) {
  return value && typeof value === "object" ? structuredClone(value) : value;
}

function fail(message) {
  throw new Error(`invalid runtime artifact manifest: ${message}`);
}
