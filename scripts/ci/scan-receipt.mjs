#!/usr/bin/env node
// The deterministic-scan receipt (rightkit#888): publish.yml's `scans` job writes it, publish-reviewed.yml's
// `seal-check` job verifies it for the original run, fail closed. Trusted code only; it never runs candidate code.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { digestScanSet } from "../../packages/release/scan-digest.mjs";
import { LOCAL_SCAN_CLASSES, LOCAL_SCAN_SCHEMA, localScanShapeProblems } from "./upload.mjs";

export const SCAN_RECEIPT_SCHEMA = "rightkit.scan-receipt/1";
export const SCAN_RECEIPT_FILE = "scan-receipt.json";
export const scanReceiptArtifactName = (runAttempt) => `scan-receipt-${runAttempt}`;
// The versions publish.yml installs. The workflow contract proves the workflow pins equal these.
export const SCAN_TOOL_PINS = Object.freeze({ "cargo-deny": "0.20.2", "cargo-audit": "0.22.2", actionlint: "1.7.12" });
export const ACTIONLINT_SHA256 = "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8";
// A full-tree plan always contains these (workspace Cargo.lock, deny config and workflows); a receipt without them is not evidence.
export const REQUIRED_SCAN_IDS = Object.freeze(["cargo-metadata", "cargo-deny", "cargo-audit", "actionlint", "yaml-parse"]);
// The scans job runs on Ubuntu and proves class A only. Class B (compile, lint) runs in the native verify jobs on their own
// hosts, because native crates do not build on Linux. A receipt that records any other class set is not this job's evidence.
export const SCAN_RECEIPT_CLASSES = Object.freeze(["A"]);

const HEX40 = /^[0-9a-f]{40}$/;
const HEX64 = /^[0-9a-f]{64}$/;
const refuse = (message) => { throw new Error(`scan receipt refused: ${message}`); };
const requireThat = (condition, message) => { if (!condition) refuse(message); };
export const sha256Hex = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const versionToken = (text, version) => new RegExp(`(^|[^0-9.])${version.replaceAll(".", "\\.")}($|[^0-9.])`).test(String(text));

/** The pinned tool versions as the installed binaries report them; null when a tool is missing. */
export function observeToolVersions(spawn = spawnSync) {
  const probe = (command, args) => {
    const r = spawn(command, args, { encoding: "utf8", shell: false });
    return !r || r.error || r.status !== 0 ? null : String(r.stdout || r.stderr || "").split(/\r?\n/).find((line) => line.trim())?.trim() ?? null;
  };
  return { "cargo-deny": probe("cargo", ["deny", "--version"]), "cargo-audit": probe("cargo", ["audit", "--version"]), actionlint: probe("actionlint", ["-version"]) };
}

/** Why the observed tool versions are not the pinned ones (empty when they are). */
export function toolVersionProblems(observed) {
  return Object.entries(SCAN_TOOL_PINS).filter(([tool, version]) => !observed?.[tool] || !versionToken(observed[tool], version)).map(([tool, version]) => `${tool} must report ${version}, got ${observed?.[tool] ?? "nothing"}`);
}

/**
 * CI gate success: at least one scan was planned, every planned scan produced a result row, none is deferred, not-run,
 * failed or missing, and the runner reports ok and complete.
 */
export function scanGatePassed(plannedScans, result) {
  if (!Array.isArray(plannedScans) || plannedScans.length === 0 || !result || !Array.isArray(result.scans)) return false;
  if (result.ok !== true || result.complete !== true || (result.deferred ?? []).length) return false;
  const byId = new Map(result.scans.map((row) => [row.id, row]));
  return result.scans.length === plannedScans.length && plannedScans.every((scan) => byId.get(scan.id)?.status === "pass");
}

/**
 * Every reason this runScans result cannot be publication evidence (empty when it can). `plannedClassA` is the plan's class A
 * scan list: every planned class A scan must have produced exactly one passing row, and nothing else may have run.
 * Without a non-empty planned list nothing can be proven, so it is a problem (fail closed).
 */
export function resultProblems(result, plannedClassA) {
  const problems = [];
  if (!result || !Array.isArray(result.scans) || result.scans.length === 0) return ["no scans ran"];
  if (!Array.isArray(plannedClassA) || plannedClassA.length === 0) problems.push("no planned class A scans to prove");
  if (result.ok !== true || result.complete !== true) problems.push("the scan result is not ok and complete");
  if (JSON.stringify(result.classes) !== JSON.stringify(SCAN_RECEIPT_CLASSES)) problems.push(`the scan result must cover exactly class ${SCAN_RECEIPT_CLASSES.join(", ")}`);
  if ((result.deferred ?? []).length) problems.push(`deferred scans are not allowed: ${result.deferred.join(", ")}`);
  for (const row of result.scans) if (row.status !== "pass") problems.push(`${row.id}: ${row.status}`);
  for (const row of result.scans) if (row.class !== "A") problems.push(`${row.id}: class ${row.class} is not proven by the scans job`);
  const ids = new Set(result.scans.map((row) => row.id));
  for (const id of REQUIRED_SCAN_IDS) if (!ids.has(id)) problems.push(`required scan ${id} is missing from the plan`);
  if (Array.isArray(plannedClassA) && plannedClassA.length > 0 && !scanGatePassed(plannedClassA, result)) problems.push("not every planned class A scan ran and passed");
  return problems;
}

export function buildScanReceipt({ result, plannedClassA, sourceCommit, headCommit, cargoLockSha256, observedTools, run }) {
  const problems = [...resultProblems(result, plannedClassA), ...toolVersionProblems(observedTools)];
  const rows = (result?.scans ?? []).map(({ id, class: cls, command, status }) => ({ id, class: cls, command, status }));
  return {
    schema: SCAN_RECEIPT_SCHEMA,
    passed: problems.length === 0,
    problems,
    sourceCommit, headCommit, cargoLockSha256,
    repository: run.repository, runId: String(run.runId), runAttempt: String(run.runAttempt),
    tools: { pinned: { ...SCAN_TOOL_PINS }, observed: { ...observedTools }, scanned: { ...(result?.toolVersions ?? {}) } },
    classes: [...SCAN_RECEIPT_CLASSES],
    scanSetDigest: digestScanSet(rows),
    classDigests: { A: digestScanSet(rows.filter((r) => r.class === "A")) },
    inputDigest: result?.inputDigest ?? null,
    deferred: [...(result?.deferred ?? [])],
    scans: rows,
  };
}

/** Throws unless the receipt shows a complete, passed scan of exactly this source commit and lockfile. */
export function verifyScanReceipt(receipt, { sourceSha, cargoLockSha256, runId, runAttempt, repository }) {
  requireThat(receipt && typeof receipt === "object" && !Array.isArray(receipt), "receipt is not an object");
  requireThat(receipt.schema === SCAN_RECEIPT_SCHEMA, "unknown receipt schema");
  requireThat(receipt.passed === true && Array.isArray(receipt.problems) && receipt.problems.length === 0, "receipt does not show a passed scan");
  requireThat(HEX40.test(sourceSha ?? "") && receipt.sourceCommit === sourceSha && receipt.headCommit === sourceSha, "receipt is not bound to the candidate source commit");
  requireThat(HEX64.test(cargoLockSha256 ?? "") && receipt.cargoLockSha256 === cargoLockSha256, "receipt is not bound to the candidate Cargo.lock hash");
  requireThat(String(receipt.runId) === String(runId) && String(receipt.runAttempt) === String(runAttempt) && receipt.repository === repository, "receipt is not bound to the original run");
  requireThat(Array.isArray(receipt.deferred) && receipt.deferred.length === 0, "receipt records a deferred scan");
  requireThat(Array.isArray(receipt.scans) && receipt.scans.length > 0 && receipt.scans.every((row) => row?.status === "pass" && typeof row.id === "string" && row.class === "A" && typeof row.command === "string"), "receipt scans are not all passed class A scans");
  requireThat(JSON.stringify(receipt.classes) === JSON.stringify(SCAN_RECEIPT_CLASSES), `receipt must record exactly class ${SCAN_RECEIPT_CLASSES.join(", ")}`);
  requireThat(HEX64.test(receipt.scanSetDigest ?? "") && digestScanSet(receipt.scans) === receipt.scanSetDigest, "receipt scan set digest does not match its scans");
  requireThat(receipt.classDigests && !("B" in receipt.classDigests) && digestScanSet(receipt.scans.filter((r) => r.class === "A")) === receipt.classDigests.A, "receipt class digests do not match its scans");
  const ids = new Set(receipt.scans.map((row) => row.id));
  for (const id of REQUIRED_SCAN_IDS) requireThat(ids.has(id), `receipt lacks required scan ${id}`);
  requireThat(receipt.tools?.pinned && Object.entries(SCAN_TOOL_PINS).every(([tool, version]) => receipt.tools.pinned[tool] === version), "receipt tool pins differ from the trusted pins");
  const toolProblems = toolVersionProblems(receipt.tools?.observed);
  requireThat(toolProblems.length === 0, `receipt tool versions: ${toolProblems.join("; ")}`);
  return true;
}

// ---- local candidates -------------------------------------------------------------------------
// A local candidate has no Actions `scans` job. Each host runs the same scan module (class A, then class B through the
// RustRight broker) over the scope of the candidate's units and writes this record into its receipt. Class B is required
// here because no native verify job follows. The record is bound to the source commit and the candidate's Cargo.lock.

/** Every reason a local scan result cannot be publication evidence. `planned` is the full plan; every planned scan must have passed. */
export function localResultProblems(result, planned) {
  const problems = [];
  if (!result || !Array.isArray(result.scans) || result.scans.length === 0) return ["no scans ran"];
  if (!Array.isArray(planned) || planned.length === 0) problems.push("no planned scans to prove");
  if (result.ok !== true || result.complete !== true) problems.push("the scan result is not ok and complete");
  if (JSON.stringify(result.classes) !== JSON.stringify(LOCAL_SCAN_CLASSES)) problems.push(`the scan result must cover exactly classes ${LOCAL_SCAN_CLASSES.join(", ")}`);
  if ((result.deferred ?? []).length) problems.push(`deferred scans are not allowed: ${result.deferred.join(", ")}`);
  for (const row of result.scans) if (row.status !== "pass") problems.push(`${row.id}: ${row.status}`);
  const ids = new Set(result.scans.map((row) => row.id));
  for (const id of REQUIRED_SCAN_IDS) if (!ids.has(id)) problems.push(`required scan ${id} is missing from the plan`);
  if (!result.scans.some((row) => row.class === "B")) problems.push("no class B (compile, lint) scan ran");
  if (Array.isArray(planned) && planned.length > 0 && !scanGatePassed(planned, result)) problems.push("not every planned scan ran and passed");
  return problems;
}

export function buildLocalScanRecord({ result, planned, sourceCommit, headCommit, cargoLockSha256, observedTools, units, files }) {
  const problems = [...localResultProblems(result, planned), ...toolVersionProblems(observedTools)];
  const rows = (result?.scans ?? []).map(({ id, class: cls, command, status }) => ({ id, class: cls, command, status }));
  return {
    schema: LOCAL_SCAN_SCHEMA, passed: problems.length === 0, problems, sourceCommit, headCommit, cargoLockSha256,
    scope: { units: [...units], files },
    tools: { pinned: { ...SCAN_TOOL_PINS }, observed: { ...observedTools } },
    classes: [...LOCAL_SCAN_CLASSES], scanSetDigest: digestScanSet(rows),
    classDigests: { A: digestScanSet(rows.filter((r) => r.class === "A")), B: digestScanSet(rows.filter((r) => r.class === "B")) },
    inputDigest: result?.inputDigest ?? null, deferred: [...(result?.deferred ?? [])], scans: rows,
  };
}

/** Throws unless the record shows a complete, passed local scan of exactly this source commit, lockfile and unit set. */
export function verifyLocalScanRecord(record, { sourceSha, cargoLockSha256, units }) {
  const shape = localScanShapeProblems(record, { commit: sourceSha, units });
  requireThat(shape.length === 0, shape.join("; "));
  requireThat(HEX64.test(cargoLockSha256 ?? "") && record.cargoLockSha256 === cargoLockSha256, "local scan record is not bound to the candidate Cargo.lock hash");
  requireThat(HEX64.test(record.scanSetDigest ?? "") && digestScanSet(record.scans) === record.scanSetDigest, "local scan set digest does not match its scans");
  requireThat(record.classDigests && digestScanSet(record.scans.filter((r) => r.class === "A")) === record.classDigests.A && digestScanSet(record.scans.filter((r) => r.class === "B")) === record.classDigests.B, "local scan class digests do not match its scans");
  const ids = new Set(record.scans.map((row) => row.id));
  for (const id of REQUIRED_SCAN_IDS) requireThat(ids.has(id), `local scan record lacks required scan ${id}`);
  requireThat(record.scans.some((row) => row.class === "B"), "local scan record has no class B scan");
  requireThat(record.tools?.pinned && Object.entries(SCAN_TOOL_PINS).every(([tool, version]) => record.tools.pinned[tool] === version), "local scan tool pins differ from the trusted pins");
  const toolProblems = toolVersionProblems(record.tools?.observed);
  requireThat(toolProblems.length === 0, `local scan tool versions: ${toolProblems.join("; ")}`);
  return true;
}

/** Verify the scan record inside every pinned local receipt of a prepared candidate against the candidate checkout. */
export function verifyLocalScanReceipts({ candidate, receiptsDir, candidateDir }) {
  const lock = path.join(candidateDir, "Cargo.lock");
  requireThat(fs.existsSync(lock), "candidate has no Cargo.lock to bind");
  const cargoLockSha256 = sha256Hex(fs.readFileSync(lock));
  const hosts = Object.keys(candidate.assets).filter((key) => key !== "packed");
  for (const host of hosts) {
    const directory = path.join(receiptsDir, host);
    const files = fs.readdirSync(directory);
    requireThat(files.length === 1, `${host} receipt directory must hold exactly one receipt`);
    let doc;
    try { doc = JSON.parse(fs.readFileSync(path.join(directory, files[0]), "utf8")); } catch { refuse(`${host} receipt is not valid JSON`); }
    verifyLocalScanRecord(doc?.scans, { sourceSha: candidate.sourceSha, cargoLockSha256, units: candidate.units });
  }
  return hosts;
}

/**
 * Fetch the original run's scan receipt and verify it against the candidate. `api`, `artifactMatches` and
 * `unpack` come from reviewed-candidate.mjs (injected so tests need no network).
 */
export async function verifyOriginalScanReceipt({ candidate, repository, env, candidateDir, fetchImpl, workDir, api, artifactMatches, unpack }) {
  const jobs = await api(`actions/runs/${candidate.runId}/attempts/${candidate.runAttempt}/jobs?per_page=100`, env, fetchImpl);
  requireThat(Array.isArray(jobs.jobs) && jobs.jobs.length === jobs.total_count, "incomplete original job listing");
  const scansJob = jobs.jobs.filter((job) => job.name === "scans");
  requireThat(scansJob.length === 1 && scansJob[0].conclusion === "success" && String(scansJob[0].run_id) === String(candidate.runId) && String(scansJob[0].run_attempt) === String(candidate.runAttempt) && scansJob[0].head_sha === candidate.sourceSha, "the original run's scans job did not succeed for the candidate source");
  const name = scanReceiptArtifactName(candidate.runAttempt);
  const listing = await api(`actions/runs/${candidate.runId}/artifacts?name=${encodeURIComponent(name)}&per_page=100`, env, fetchImpl);
  requireThat(Number.isSafeInteger(listing.total_count) && Array.isArray(listing.artifacts), "invalid scan receipt artifact listing");
  const found = listing.artifacts.filter((row) => row.name === name);
  requireThat(listing.total_count === listing.artifacts.length && found.length === 1 && found.length === listing.artifacts.length, "expected exactly one original scan receipt artifact");
  const artifact = await api(`actions/artifacts/${found[0].id}`, env, fetchImpl);
  requireThat(artifactMatches(artifact, { id: found[0].id, digest: found[0].digest, name, runId: candidate.runId, sourceSha: candidate.sourceSha, repository }), "scan receipt artifact provenance mismatch");
  const bytes = await api(`actions/artifacts/${artifact.id}/zip`, env, fetchImpl, true);
  requireThat(`sha256:${sha256Hex(bytes)}` === artifact.digest, "scan receipt ZIP digest mismatch");
  fs.mkdirSync(workDir, { recursive: true });
  const destination = path.join(workDir, "scan-receipt");
  const names = unpack({ bytes, zipFile: path.join(workDir, "scan-receipt.zip"), destination });
  requireThat(names.length === 1 && names[0] === SCAN_RECEIPT_FILE, "scan receipt artifact is not exactly one scan-receipt.json");
  let receipt;
  try { receipt = JSON.parse(fs.readFileSync(path.join(destination, SCAN_RECEIPT_FILE), "utf8")); } catch { refuse("scan receipt is not valid JSON"); }
  const lock = path.join(candidateDir, "Cargo.lock");
  requireThat(fs.existsSync(lock), "candidate has no Cargo.lock to bind");
  verifyScanReceipt(receipt, { sourceSha: candidate.sourceSha, cargoLockSha256: sha256Hex(fs.readFileSync(lock)), runId: candidate.runId, runAttempt: candidate.runAttempt, repository: repository.full_name });
  return receipt;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const [command, ...rest] = process.argv.slice(2);
  try {
    requireThat(command === "verify" && rest.length === 0, "usage: scan-receipt.mjs verify");
    const { authenticateSource, prepareCandidate, api, artifactMatches, unpackArtifact } = await import("./reviewed-candidate.mjs");
    const { env } = process;
    requireThat(env.PUBLISH_SOURCE_DIR && env.RUNNER_TEMP, "PUBLISH_SOURCE_DIR and RUNNER_TEMP are required");
    const { candidate, repository } = await authenticateSource({ env, candidateId: env.PUBLISH_CANDIDATE_ID });
    if (candidate.source === "local") {
      // A local candidate's scan records live in its pinned receipts; prepareCandidate re-downloads and hash-checks them.
      const prepared = await prepareCandidate({ env, candidateId: env.PUBLISH_CANDIDATE_ID });
      verifyLocalScanReceipts({ candidate, receiptsDir: prepared.receiptsDir, candidateDir: env.PUBLISH_SOURCE_DIR });
      process.stdout.write(`local scan records verified for ${candidate.sourceSha}\n`);
    } else {
      await verifyOriginalScanReceipt({ candidate, repository, env, candidateDir: env.PUBLISH_SOURCE_DIR, fetchImpl: fetch, workDir: path.join(env.RUNNER_TEMP, "scan-receipt-verify"), api, artifactMatches, unpack: unpackArtifact });
      process.stdout.write(`scan receipt verified for ${candidate.sourceSha}\n`);
    }
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
