#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { catalogDigest, contentInventory, MANIFEST_FILE, SCANNER_VERSION } from "./ownership.mjs";
import { createOwnershipEvidence, createOwnershipEvidenceForTest } from "./ownership-evidence.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, sha256 } from "./ownership-approval.mjs";
import { readCommittedKeyAuthority } from "./ownership-key-authority.mjs";
import { ownershipCacheKey, ownershipVerdict } from "./ownership-verdict.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const catalogPath = path.join(here, MANIFEST_FILE);
const MODULE_SPECIFIERS = /^\s*(?:import|export)\s[^'";]*?from\s*['"]([^'"]+)['"]|^\s*import\s*['"]([^'"]+)['"]/gm;
function scannerPolicyDigest() {
  const files = new Map(), queue = [fileURLToPath(import.meta.url)];
  while (queue.length) {
    const file = queue.shift(); if (files.has(file)) continue;
    const bytes = readFileSync(file); files.set(file, sha256(bytes));
    for (const match of bytes.toString("utf8").matchAll(MODULE_SPECIFIERS)) {
      const specifier = match[1] ?? match[2];
      if (specifier.startsWith("node:")) continue;
      queue.push(specifier.startsWith(".") ? createRequire(file).resolve(specifier) : fileURLToPath(import.meta.resolve(specifier)));
    }
  }
  return sha256({ version: SCANNER_VERSION, files: [...files].map(([file, digest]) => ({ path: path.relative(here, file).replaceAll("\\", "/"), digest })).sort((a, b) => a.path.localeCompare(b.path)) });
}

function scanInputs(appRoot, { manifestPath = catalogPath, trustLoader = loadTrustedExternal, inventory = contentInventory, gitRunner = spawnSync, policyDigest = scannerPolicyDigest } = {}) {
  const root = path.resolve(appRoot), manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const state = trustLoader();
  if (state.protection?.ok !== true && !(state.pending === true && state.enrolled === false && state.trust === null && state.protection?.reason === "external trust file is absent"))
    throw Object.assign(new Error("protected ownership trust location was not verified"), { code: "RK_OWNERSHIP_INVALID_INPUTS" });
  let trust = state.trust, keyLogDigest = null;
  if (trust?.keyAuthority) {
    const resolved = readCommittedKeyAuthority(trust);
    if (!resolved.ok) throw Object.assign(new Error(resolved.problems.join("; ")), { code: "RK_OWNERSHIP_INVALID_INPUTS" });
    trust = resolved.resolvedTrust; keyLogDigest = resolved.digest;
  }
  const origin = gitRunner("git", ["-C", root, "remote", "get-url", "origin"], { encoding: "utf8", windowsHide: true, timeout: 2500 });
  let repositoryIdentity = null;
  try { if (origin?.status === 0) repositoryIdentity = canonicalRepositoryIdentity(String(origin.stdout).trim()); } catch {}
  const source = inventory(root);
  if (typeof source.complete !== "boolean" || !Array.isArray(source.coverage) || source.complete !== (source.coverage.length === 0))
    throw Object.assign(new Error("ownership content inventory completeness is missing or malformed"), { code: "RK_OWNERSHIP_INVALID_INPUTS" });
  const roots = trust?.repositories?.find((entry) => entry.repositoryIdentity === repositoryIdentity)?.roots ?? ["."];
  const approvals = roots.map((registered) => {
    const file = path.join(root, ...(registered === "." ? [] : registered.split("/")), ".rightkit", "ownership-exceptions.json");
    try { return { root: registered, digest: sha256(readFileSync(file)) }; }
    catch (error) { if (error.code !== "ENOENT") throw error; return { root: registered, digest: sha256({ absent: true }) }; }
  });
  const inputs = {
    contentDigest: source.digest, catalogDigest: catalogDigest(manifest),
    trustDigest: sha256({ trust, repositoryIdentity, enrolled: state.enrolled, pending: state.pending, keyLogDigest }),
    approvalsDigest: sha256(approvals), scannerPolicyDigest: policyDigest(),
    protectedFloors: { minimumScannerVersion: trust?.minimumScannerVersion ?? null, minimumCatalogVersion: trust?.minimumCatalogVersion ?? null, minimumCatalogDigest: trust?.minimumCatalogDigest ?? null },
  };
  ownershipCacheKey(inputs);
  return { inputs, trust, complete: source.complete, enrollmentPending: trust?.enrollmentComplete === false || state.enrolled !== true || state.pending === true,
    metadata: { repositoryIdentity, scannerVersion: SCANNER_VERSION, catalogVersion: manifest.version ?? String(manifest.schema ?? "1"), catalogDigest: inputs.catalogDigest, coverage: source.coverage, roots: roots.map((registered) => ({ root: registered })) } };
}

export function ownershipScanInputs(appRoot) { return scanInputs(appRoot); }
export function ownershipScanInputsForTest(appRoot, options) { return scanInputs(appRoot, options); }

function scanProtected(appRoot, options, createEvidence) {
  const before = scanInputs(appRoot, options);
  const preliminary = { ok: true, enrollmentPending: before.enrollmentPending, violations: [], problems: [], coverage: before.metadata.coverage, metadata: before.metadata };
  const preflight = ownershipVerdict(preliminary, before.trust);
  if (!preflight.admit && preflight.code !== "RK_OWNERSHIP_PENDING_ENROLLMENT") return { ...before, report: { ...preliminary, ok: false }, verdict: preflight };
  let checked;
  try { checked = createEvidence(); }
  catch (error) {
    if (preflight.code !== "RK_OWNERSHIP_PENDING_ENROLLMENT") throw error;
    const report = { ...preliminary, ok: false, problems: [error.message] };
    return { ...before, report, verdict: ownershipVerdict(report, before.trust) };
  }
  const report = JSON.parse(JSON.stringify({ ...checked.result, ok: checked.ok, enrollmentPending: checked.enrollmentPending === true,
    metadata: { ...checked.result.metadata, scannerVersion: checked.evidence.scannerVersion, catalogVersion: checked.evidence.catalogVersion } }));
  const verdict = ownershipVerdict(report, before.trust);
  const after = scanInputs(appRoot, options);
  if (ownershipCacheKey(before.inputs) !== ownershipCacheKey(after.inputs) || before.complete !== after.complete || sha256(before.metadata.coverage) !== sha256(after.metadata.coverage))
    return { error: "ownership inputs changed during protected evidence scan", code: "RK_OWNERSHIP_INPUTS_CHANGED" };
  return { inputs: after.inputs, trust: after.trust, report, verdict, evidence: checked.evidence };
}

export function scanOwnership(appRoot) { return scanProtected(appRoot, {}, () => createOwnershipEvidence(appRoot)); }
export function scanOwnershipForTest(appRoot, options) { return scanProtected(appRoot, options, () => createOwnershipEvidenceForTest(appRoot, options)); }

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [appRoot, manifestPath, mode] = process.argv.slice(2);
    if (!appRoot || !manifestPath || path.resolve(manifestPath) !== catalogPath || mode && mode !== "--inputs") throw new TypeError("app root & installed protected catalog path are required");
    process.stdout.write(JSON.stringify(mode === "--inputs" ? ownershipScanInputs(path.resolve(appRoot)) : scanOwnership(path.resolve(appRoot))) + "\n");
  } catch (error) {
    process.stdout.write(JSON.stringify({ error: error.message, code: error.code?.startsWith("RK_OWNERSHIP_") ? error.code : "RK_OWNERSHIP_UNVERIFIED" }) + "\n");
  }
}
