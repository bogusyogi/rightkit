// Protected policy bundle: one digest over everything that decides ownership enforcement, so editing the
// scanner/verifier code (or the allowlist/registry) while keeping the catalog cannot weaken enforcement.
// Trust-sync pins the bundle into protected trust; release/admission gates refuse when the running bundle differs.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { canonicalJson } from "./ownership-approval.mjs";
import { ALLOWLIST_FILE, diffAllowlists, validateAllowlist } from "./ownership-dependency-allowlist.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const MODULE_SPECIFIERS = /^\s*(?:import|export)\s[^'";]*?from\s*['"]([^'"]+)['"]|^\s*import\s*['"]([^'"]+)['"]/gm;
const HEX = /^[a-f0-9]{64}$/;
export const POLICY_BUNDLE_SCHEMA = 1;
const CATALOG_FILE = "rightkit-ownership.json";
const REGISTRY_FILE = "ownership-repositories.json";
// Step 4: the release QA requirement floor is protected like the catalog. Dropping a required check changes the digest.
export const QA_FLOOR_FILE = "release-qa-floor.json";
// Modules that make or carry enforcement decisions. Their import closure is pinned.
export const POLICY_BUNDLE_ENTRYPOINTS = Object.freeze([
  "ownership-scan-worker.mjs", "ownership-evidence.mjs", "ownership-verdict.mjs", "ownership-admission.mjs",
  "ownership-staged.mjs", "ownership-hooks.mjs", "ownership-key-trust.mjs", "release-qa-receipt.mjs",
]);
// Data files are pinned through their own digests, not as source files.
const DATA_FILES = new Set([CATALOG_FILE, REGISTRY_FILE, ALLOWLIST_FILE, QA_FLOOR_FILE]);

const sha256Hex = (bytes) => createHash("sha256").update(bytes).digest("hex");
const canonicalDigest = (value) => sha256Hex(canonicalJson(value));
// Normalize CRLF so a checkout's line-ending policy does not change the digest.
const textDigest = (bytes) => sha256Hex(Buffer.from(bytes.toString("utf8").replace(/\r\n/g, "\n")));

function sourceName(file, dir) {
  const relative = path.relative(dir, file).split(path.sep).join("/");
  if (!relative.startsWith("../")) return relative;
  const dependency = /^.*node_modules\/((?:@[^/]+\/)?[^/]+)\/(.+)$/.exec(file.split(path.sep).join("/"));
  return dependency ? `node_modules/${dependency[1]}/${dependency[2]}` : relative;
}

const closureCache = new Map();
/** Import closure of the enforcement entrypoints as [{ path, sha256 }] sorted by path. */
export function scannerSourceFiles({ dir = here, entrypoints = POLICY_BUNDLE_ENTRYPOINTS } = {}) {
  const key = `${dir}\0${entrypoints.join("\0")}`;
  if (closureCache.has(key)) return closureCache.get(key);
  const files = new Map(), queue = entrypoints.map((name) => path.join(dir, name));
  while (queue.length) {
    const file = queue.shift();
    if (files.has(file)) continue;
    let bytes;
    try { bytes = readFileSync(file); } catch (error) { if (error.code === "ENOENT" && !file.startsWith(dir)) continue; throw error; }
    files.set(file, file.endsWith(".json") ? sha256Hex(Buffer.from(canonicalJson(JSON.parse(bytes.toString("utf8"))))) : textDigest(bytes));
    if (!/\.(?:mjs|js|cjs)$/.test(file)) continue;
    for (const match of bytes.toString("utf8").matchAll(MODULE_SPECIFIERS)) {
      const specifier = match[1] ?? match[2];
      if (specifier.startsWith("node:")) continue;
      try { queue.push(specifier.startsWith(".") ? path.resolve(path.dirname(file), specifier) : createRequire(file).resolve(specifier)); }
      catch { queue.push(path.resolve(path.dirname(file), specifier)); }
    }
  }
  const result = [...files].map(([file, digest]) => ({ path: sourceName(file, dir), sha256: digest }))
    .filter((entry) => !DATA_FILES.has(entry.path)).sort((a, b) => a.path.localeCompare(b.path));
  closureCache.set(key, result);
  return result;
}

function readJsonOrNull(file) {
  try { return JSON.parse(readFileSync(file, "utf8")); } catch { return null; }
}

/**
 * @param {{ dir?: string, manifest?: object, registry?: object|null, allowlist?: object|null, qaFloor?: object|null, scannerVersion: string, files?: Array<{path:string,sha256:string}> }} input
 * Missing registry/allowlist digest as null; the digest still binds that absence.
 */
export function computePolicyBundle({ dir = here, manifest, registry, allowlist, qaFloor, scannerVersion, files } = {}) {
  if (!/^\d+(?:\.\d+)*$/.test(String(scannerVersion ?? ""))) throw new Error("policy bundle requires a scanner version");
  const catalog = manifest ?? readJsonOrNull(path.join(dir, CATALOG_FILE));
  if (!catalog) throw new Error("policy bundle requires the ownership catalog");
  const registryValue = registry === undefined ? readJsonOrNull(path.join(dir, REGISTRY_FILE)) : registry;
  const allowlistValue = allowlist === undefined ? readJsonOrNull(path.join(dir, ALLOWLIST_FILE)) : allowlist;
  const qaFloorValue = qaFloor === undefined ? readJsonOrNull(path.join(dir, QA_FLOOR_FILE)) : qaFloor;
  const parts = {
    schema: POLICY_BUNDLE_SCHEMA,
    scannerVersion: String(scannerVersion),
    catalogDigest: canonicalDigest(catalog),
    registryDigest: registryValue === null ? null : canonicalDigest(registryValue),
    allowlistDigest: allowlistValue === null ? null : canonicalDigest(allowlistValue),
    qaFloorDigest: qaFloorValue === null ? null : canonicalDigest(qaFloorValue),
    files: files ?? scannerSourceFiles({ dir }),
  };
  return { ...parts, digest: canonicalDigest(parts) };
}

/** Running bundle for the code that is executing now. */
export function runningPolicyBundle({ scannerVersion, manifest, dir = here, ...rest } = {}) {
  return computePolicyBundle({ dir, scannerVersion, manifest, ...rest });
}

export function validatePolicyBundle(bundle) {
  const problems = [];
  if (!bundle || typeof bundle !== "object" || Array.isArray(bundle)) return { ok: false, problems: ["policy bundle must be an object"] };
  if (bundle.schema !== POLICY_BUNDLE_SCHEMA) problems.push("policy bundle schema is unsupported");
  if (!HEX.test(bundle.digest ?? "")) problems.push("policy bundle digest is malformed");
  if (!/^\d+(?:\.\d+)*$/.test(bundle.scannerVersion ?? "")) problems.push("policy bundle scanner version is malformed");
  for (const key of ["catalogDigest"]) if (!HEX.test(bundle[key] ?? "")) problems.push(`policy bundle ${key} is malformed`);
  for (const key of ["registryDigest", "allowlistDigest"]) if (bundle[key] !== null && !HEX.test(bundle[key] ?? "")) problems.push(`policy bundle ${key} is malformed`);
  // Bundles pinned before step 4 carry no qaFloorDigest; they stay well-formed but can never match a running bundle.
  if (bundle.qaFloorDigest !== undefined && bundle.qaFloorDigest !== null && !HEX.test(bundle.qaFloorDigest)) problems.push("policy bundle qaFloorDigest is malformed");
  if (!Array.isArray(bundle.files) || bundle.files.some((file) => !file || typeof file.path !== "string" || !HEX.test(file.sha256 ?? ""))) problems.push("policy bundle file list is malformed");
  else {
    const { digest, ...parts } = bundle;
    if (HEX.test(digest ?? "") && canonicalDigest(parts) !== digest) problems.push("policy bundle digest does not match its contents");
  }
  return { ok: problems.length === 0, problems };
}

/** Semantic diff of two bundles (the previous one may be null/absent). */
export function diffPolicyBundles(before, after) {
  const prior = new Map((before?.files ?? []).map((file) => [file.path, file.sha256]));
  const current = new Map((after?.files ?? []).map((file) => [file.path, file.sha256]));
  return {
    filesAdded: [...current.keys()].filter((p) => !prior.has(p)).sort(),
    filesRemoved: [...prior.keys()].filter((p) => !current.has(p)).sort(),
    filesChanged: [...current.keys()].filter((p) => prior.has(p) && prior.get(p) !== current.get(p)).sort(),
    scannerVersion: before?.scannerVersion !== after?.scannerVersion ? { from: before?.scannerVersion ?? null, to: after?.scannerVersion ?? null } : null,
    catalogChanged: before?.catalogDigest !== after?.catalogDigest,
    registryChanged: before?.registryDigest !== after?.registryDigest,
    allowlistChanged: before?.allowlistDigest !== after?.allowlistDigest,
    qaFloorChanged: (before?.qaFloorDigest ?? null) !== (after?.qaFloorDigest ?? null),
  };
}

/**
 * Host-independent fingerprint of protected trust for cross-host comparison (`ownership trust-compare`).
 * Excludes only the per-host key-authority checkout path; every other field, including the pinned policy
 * bundle and dependency allowlist, must be identical on two hosts that synced the same release.
 */
export function trustComparison(trust) {
  if (!trust || typeof trust !== "object") throw new Error("no protected trust to compare");
  const { keyAuthority, ...rest } = trust;
  const comparable = { ...rest, ...(keyAuthority ? { keyAuthority: { repositoryIdentity: keyAuthority.repositoryIdentity } } : {}) };
  return {
    digest: canonicalDigest(comparable),
    components: {
      policyBundle: trust.policyBundle?.digest ?? null,
      dependencyAllowlist: trust.dependencyAllowlist ? canonicalDigest(trust.dependencyAllowlist) : null,
      repositories: canonicalDigest(trust.repositories ?? []),
      minimumCatalogDigest: trust.minimumCatalogDigest ?? null,
      minimumScannerVersion: trust.minimumScannerVersion ?? null,
      keyCheckpoint: trust.keyCheckpoint ?? null,
      rootFingerprint: trust.rootFingerprint ?? null,
    },
  };
}

export { diffAllowlists, validateAllowlist };
