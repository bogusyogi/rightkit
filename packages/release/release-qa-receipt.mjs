// Release QA receipts (hardening step 4): no signed release on any channel without a passing, artifact-bound
// QA receipt for the exact commit.
//
//   source stage    admits the candidate build/sign: QA ran against the committed source.
//   installed stage admits upload/publication: QA ran on the built, sealed artifacts (exact sha256) and chains
//                   to the source receipt digest recorded in the sealed manifest.
//
// A receipt is written only by `right-release qa`, which runs RightKit's own runner (`rightkit-qa`), digests the
// runner binary and the evidence bundle it produced, evaluates the protected QA floor, and MACs the result with a
// key held in the protected credential store (Keychain / protected file / CI secret) — not app-writable JSON.
// The floor, this verifier, and the receipt schema are pinned in the protected policy bundle.
import { createHash, createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { canonicalJson, canonicalRepositoryIdentity } from "./ownership-approval.mjs";
import { OWNERSHIP_LOCKFILES } from "./ownership-lockfiles.mjs";
import { QA_FLOOR_FILE, runningPolicyBundle } from "./ownership-policy-bundle.mjs";
import { createCredentialStore, ProtectedSecretMissingError } from "./protected-credentials.mjs";
import { isManagedHost } from "./managed-host-secrets.mjs";
import { SCANNER_VERSION } from "./ownership.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const HEX64 = /^[a-f0-9]{64}$/;
export const QA_RECEIPT_SCHEMA = 1;
export const QA_STAGES = Object.freeze(["source", "installed"]);
export const QA_KEY_ENV = "RIGHTKIT_QA_RECEIPT_KEY";
const MAC_DOMAIN = "rightkit-qa-receipt-v1\0";

export class QaAdmissionError extends Error {
  constructor(message, { command } = {}) {
    super(command ? `${message}\n  run: ${command}` : message);
    this.name = "QaAdmissionError";
    this.code = "QA_ADMISSION_REFUSED";
    this.command = command ?? null;
  }
}

const sha256Hex = (bytes) => createHash("sha256").update(bytes).digest("hex");
const digestOf = (value) => sha256Hex(canonicalJson(value));
const platformDirOf = (platform) => (platform === "win" ? "windows" : platform);

/** Exact command an agent must run to obtain the missing/invalid receipt. */
export function qaCommandFor({ stage, platform, releaseId, configName = "right-release.config.mjs" }) {
  return `right-release qa --stage ${stage} --platform ${platform}${stage === "installed" ? ` --release ${releaseId}` : ""} --config ${configName}`;
}

export function qaReceiptPath({ repoRoot, releaseId, platform, stage }) {
  return path.join(repoRoot, ".right-release", "qa", releaseId, platformDirOf(platform), `${stage}.receipt.json`);
}

export function loadQaFloor(dir = here) {
  const file = path.join(dir, QA_FLOOR_FILE);
  let floor;
  try { floor = JSON.parse(readFileSync(file, "utf8")); } catch (error) { throw new Error(`QA floor is unreadable (${QA_FLOOR_FILE}): ${error.message}`); }
  const problems = validateQaFloor(floor);
  if (problems.length) throw new Error(`QA floor is invalid: ${problems.join("; ")}`);
  return { floor, digest: digestOf(floor) };
}

export function validateQaFloor(floor) {
  const problems = [];
  if (!floor || typeof floor !== "object" || Array.isArray(floor)) return ["floor must be an object"];
  if (floor.schema !== 1) problems.push("floor schema must be 1");
  if (!Number.isInteger(floor.version) || floor.version < 1) problems.push("floor version must be a positive integer");
  if (typeof floor.runner !== "string" || !floor.runner) problems.push("floor runner is required");
  for (const stage of QA_STAGES) {
    const required = floor.stages?.[stage]?.required;
    if (!Array.isArray(required) || required.length === 0 || required.some((id) => typeof id !== "string" || !id)) problems.push(`floor stage ${stage} must list required check ids`);
  }
  return problems;
}

/** The app's release config declares how floor ids map to evidence check names (exact, or `prefix*`). */
export function validateAppQaConfig(qa, floor, stage) {
  const problems = [];
  if (!qa || typeof qa !== "object") return [`release config must declare qa { version, config, checks } (floor requires ${floor.stages[stage].required.join(", ")})`];
  if (typeof qa.version !== "string" && typeof qa.version !== "number") problems.push("qa.version is required");
  if (typeof qa.config !== "string" || !qa.config) problems.push("qa.config (rightkit-qa.toml path) is required");
  for (const id of floor.stages[stage].required) {
    const patterns = qa.checks?.[id];
    if (!Array.isArray(patterns) || patterns.length === 0 || patterns.some((p) => typeof p !== "string" || !p)) problems.push(`qa.checks must map required floor check "${id}" to evidence check names`);
  }
  return problems;
}

const matches = (pattern, name) => (pattern.endsWith("*") ? name.startsWith(pattern.slice(0, -1)) : name === pattern);

/** Per required id: passed only when at least one mapped evidence check exists and every match passed. */
export function evaluateRequiredChecks({ floor, stage, qa, evidence }) {
  const evidenceChecks = Array.isArray(evidence?.checks) ? evidence.checks : [];
  return floor.stages[stage].required.map((id) => {
    const patterns = qa.checks?.[id] ?? [];
    const found = evidenceChecks.filter((check) => patterns.some((pattern) => matches(pattern, String(check.name))));
    const bad = found.filter((check) => check.status !== "passed");
    const status = found.length === 0 ? "missing" : bad.length ? "failed" : "passed";
    return { id, status, evidenceChecks: found.map((check) => ({ name: String(check.name), status: String(check.status) })) };
  });
}

function git(repoRoot, args, { runner = spawnSync, binary = false } = {}) {
  const result = runner("git", ["-C", repoRoot, ...args], { encoding: binary ? "buffer" : "utf8", windowsHide: true, maxBuffer: 256 * 1024 * 1024 });
  if (result?.status !== 0) throw new Error(`QA receipt: git ${args.join(" ")} failed`);
  return binary ? result.stdout : String(result.stdout).trim();
}

/** Dependency closure: sha256 of every committed lockfile at the exact commit. */
export function dependencyClosure(repoRoot, commit, runner) {
  const names = git(repoRoot, ["ls-tree", "-r", "--name-only", commit], { runner }).split("\n").filter(Boolean);
  const lockfiles = names.filter((name) => OWNERSHIP_LOCKFILES.includes(path.posix.basename(name))).sort()
    .map((name) => ({ path: name, sha256: sha256Hex(git(repoRoot, ["show", `${commit}:${name}`], { runner, binary: true })) }));
  return { lockfiles, digest: digestOf(lockfiles) };
}

export function sourceIdentity(repoRoot, runner) {
  const commit = git(repoRoot, ["rev-parse", "HEAD"], { runner });
  return {
    commit,
    tree: git(repoRoot, ["rev-parse", `${commit}^{tree}`], { runner }),
    repositoryIdentity: canonicalRepositoryIdentity(git(repoRoot, ["remote", "get-url", "origin"], { runner })),
  };
}

export function currentPolicyBundleDigest(dir = here) {
  return runningPolicyBundle({ scannerVersion: SCANNER_VERSION, dir }).digest;
}

/** MAC key from the protected store (Keychain / protected file). Hosted CI may supply it as a per-run secret. */
export function resolveQaReceiptKey({ env = process.env, credentialStore } = {}) {
  const store = credentialStore ?? createCredentialStore({ env });
  try { return store.read("qa-receipt-key"); }
  catch (error) {
    if (!(error instanceof ProtectedSecretMissingError)) throw error;
    const fallback = !isManagedHost(env) && env.GITHUB_ACTIONS === "true" ? String(env[QA_KEY_ENV] ?? "").trim() : "";
    if (fallback) return fallback;
    throw error;
  }
}

/** First `right-release qa` on a host creates the key. macOS Keychain / Windows protected file. */
export function ensureQaReceiptKey({ env = process.env, credentialStore, platform = process.platform, provision } = {}) {
  try { return resolveQaReceiptKey({ env, credentialStore }); }
  catch (error) {
    if (!(error instanceof ProtectedSecretMissingError) || !provision) throw error;
    provision(randomBytes(32).toString("hex"), platform);
    return resolveQaReceiptKey({ env, credentialStore });
  }
}

const mac = (key, digest) => createHmac("sha256", key).update(`${MAC_DOMAIN}${digest}`).digest("hex");

/** Seal a receipt body: digest over the canonical body, MAC over the digest. */
export function sealQaReceipt(body, key) {
  const digest = digestOf(body);
  return { ...body, digest, mac: mac(key, digest) };
}

export function receiptDigest(receipt) {
  const { digest, mac: _mac, ...body } = receipt;
  return digestOf(body);
}

export function buildQaReceipt({
  stage, app, platform, releaseId, source, closure, releaseConfigSha256, qa, floor, floorDigest, policyBundleDigest,
  runner, evidence, evidenceSha256, required, artifacts = [], sourceReceiptDigest = null, now = new Date(),
}) {
  const evidenceOk = evidence?.status === "passed" && Boolean(evidence?.finishedAt);
  const status = evidenceOk && required.every((item) => item.status === "passed") ? "passed" : "failed";
  return {
    schema: QA_RECEIPT_SCHEMA, stage, app, platform, releaseId,
    repositoryIdentity: source.repositoryIdentity, commit: source.commit, tree: source.tree,
    dependencyClosure: closure,
    buildConfig: { releaseConfigSha256, platform },
    qaPlan: { version: String(qa.version), configPath: qa.config, planDigest: digestOf({ version: String(qa.version), config: qa.config, checks: qa.checks ?? {}, args: qa.args ?? {} }) },
    floor: { version: floor.version, digest: floorDigest },
    policyBundleDigest,
    runner,
    evidence: { sha256: evidenceSha256, runId: evidence?.runId ?? null, status: evidence?.status ?? null, schemaVersion: evidence?.schemaVersion ?? null },
    required, checks: (evidence?.checks ?? []).map((check) => ({ name: String(check.name), status: String(check.status) })),
    artifacts, sourceReceiptDigest, status, createdAt: now.toISOString(),
  };
}

/**
 * Verify a receipt against the exact current state. Returns { ok, reason, receipt }.
 * `expected.artifacts` ([{name, sha256}]) must equal the receipt's artifacts exactly (installed stage).
 */
export function verifyQaReceipt(receipt, { key, stage, repoRoot, releaseConfigSha256, floorDir = here, artifacts, sourceReceiptDigest, gitRunner, policyBundleDigest, source, closure }) {
  const refuse = (reason) => ({ ok: false, reason });
  if (!receipt || receipt.schema !== QA_RECEIPT_SCHEMA) return refuse("receipt is missing or has an unsupported schema");
  if (receipt.stage !== stage) return refuse(`receipt is for stage ${receipt.stage}, not ${stage}`);
  if (!HEX64.test(receipt.digest ?? "") || !HEX64.test(receipt.mac ?? "")) return refuse("receipt is unsealed");
  if (receiptDigest(receipt) !== receipt.digest) return refuse("receipt contents do not match its digest");
  const expectedMac = Buffer.from(mac(key, receipt.digest), "hex");
  if (!timingSafeEqual(expectedMac, Buffer.from(receipt.mac, "hex"))) return refuse("receipt MAC is invalid (not written by this host's `right-release qa`)");
  if (receipt.status !== "passed") return refuse("receipt records a failed QA run");
  let floor;
  try { floor = loadQaFloor(floorDir); } catch (error) { return refuse(error.message); }
  if (receipt.floor?.digest !== floor.digest) return refuse("QA floor changed since the receipt was written");
  let bundle;
  try { bundle = policyBundleDigest ?? currentPolicyBundleDigest(floorDir); } catch (error) { return refuse(`policy bundle could not be computed: ${error.message}`); }
  if (receipt.policyBundleDigest !== bundle) return refuse("protected policy bundle changed since the receipt was written");
  if (receipt.evidence?.status !== "passed") return refuse("evidence bundle did not pass");
  // Every floor-required check must be recorded and passed — skipped, failed, or missing refuses.
  for (const id of floor.floor.stages[stage].required) {
    const item = (receipt.required ?? []).find((entry) => entry.id === id);
    if (!item) return refuse(`required check ${id} is not recorded in the receipt`);
    if (item.status !== "passed") return refuse(`required check ${id} is ${item.status}`);
    if (!item.evidenceChecks?.length || item.evidenceChecks.some((check) => check.status !== "passed")) return refuse(`required check ${id} has incomplete or non-passing evidence`);
  }
  let current, currentClosure;
  try { current = source ?? sourceIdentity(repoRoot, gitRunner); currentClosure = closure ?? dependencyClosure(repoRoot, current.commit, gitRunner); }
  catch (error) { return refuse(error.message); }
  if (receipt.commit !== current.commit) return refuse(`receipt is for commit ${String(receipt.commit).slice(0, 12)}, current commit is ${current.commit.slice(0, 12)}`);
  if (receipt.tree !== current.tree) return refuse("receipt source tree differs from the current commit tree");
  if (receipt.repositoryIdentity !== current.repositoryIdentity) return refuse("receipt repository identity differs");
  if (receipt.dependencyClosure?.digest !== currentClosure.digest) return refuse("dependency closure (lockfile digests) changed since the receipt was written");
  if (releaseConfigSha256 !== undefined && receipt.buildConfig?.releaseConfigSha256 !== releaseConfigSha256) return refuse("release config changed since the receipt was written");
  if (receipt.runner?.basename !== floor.floor.runner || !HEX64.test(receipt.runner?.sha256 ?? "")) return refuse(`receipt was not produced by the ${floor.floor.runner} runner`);
  if (stage === "installed") {
    const wanted = [...(artifacts ?? [])].map((item) => `${item.name}:${item.sha256}`).sort();
    const have = (receipt.artifacts ?? []).map((item) => `${item.name}:${item.sha256}`).sort();
    if (!wanted.length || JSON.stringify(wanted) !== JSON.stringify(have)) return refuse("receipt artifact hashes do not match the sealed artifacts");
    if (!sourceReceiptDigest || receipt.sourceReceiptDigest !== sourceReceiptDigest) return refuse("installed QA is not chained to the source QA receipt sealed into this release");
  }
  return { ok: true, receipt };
}

export function readQaReceipt(file) {
  try { return JSON.parse(readFileSync(file, "utf8")); } catch { return null; }
}

export function writeQaReceipt(file, receipt) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(receipt, null, 2)}\n`);
}

function loadKeyOrRefuse(opts, command) {
  try { return (opts.resolveKey ?? resolveQaReceiptKey)({ env: opts.env, credentialStore: opts.credentialStore }); }
  catch (error) { throw new QaAdmissionError(`QA receipt key is unavailable (${error.message})`, { command }); }
}

/**
 * Build gate: source QA must have passed for this exact commit before a candidate is built/signed.
 * Returns the receipt digest to record in the sealed manifest.
 */
export function assertSourceQaAdmission({ repoRoot, releaseId, platform, configName, releaseConfigSha256, ...opts }) {
  const command = qaCommandFor({ stage: "source", platform, releaseId, configName });
  const receipt = readQaReceipt(qaReceiptPath({ repoRoot, releaseId, platform, stage: "source" }));
  if (!receipt) throw new QaAdmissionError(`no source QA receipt for ${releaseId} (${platform}); a candidate cannot be built or signed without passing source QA`, { command });
  const verdict = verifyQaReceipt(receipt, { key: loadKeyOrRefuse(opts, command), stage: "source", repoRoot, releaseConfigSha256, floorDir: opts.floorDir, gitRunner: opts.gitRunner, policyBundleDigest: opts.policyBundleDigest, source: opts.source, closure: opts.closure });
  if (!verdict.ok) throw new QaAdmissionError(`source QA receipt rejected: ${verdict.reason}`, { command });
  return { digest: receipt.digest, floorDigest: receipt.floor.digest };
}

/**
 * Upload/publication gate for every provider (R2 + RightApps, GitHub Releases, updater channels): an installed-QA
 * receipt for the exact sealed artifact hashes and commit must exist and pass. Dev builds never reach this gate —
 * install-app --dev and the devArtifact lane never upload — and there is deliberately no manifest flag that skips it.
 */
export function assertInstalledQaAdmission({ repoRoot, manifest, platform, configName, releaseConfigSha256, ...opts }) {
  const releaseId = manifest.releaseId;
  const command = qaCommandFor({ stage: "installed", platform, releaseId, configName });
  const receipt = readQaReceipt(qaReceiptPath({ repoRoot, releaseId, platform, stage: "installed" }));
  if (!receipt) throw new QaAdmissionError(`no installed QA receipt for ${releaseId} (${platform}); nothing is published or uploaded without passing installed QA on these exact artifacts`, { command });
  const verdict = verifyQaReceipt(receipt, {
    key: loadKeyOrRefuse(opts, command), stage: "installed", repoRoot, releaseConfigSha256, floorDir: opts.floorDir,
    artifacts: (manifest.files ?? []).map((file) => ({ name: file.name, sha256: file.sha256 })),
    sourceReceiptDigest: manifest.qa?.sourceReceiptDigest,
    gitRunner: opts.gitRunner, policyBundleDigest: opts.policyBundleDigest, source: opts.source, closure: opts.closure,
  });
  if (!verdict.ok) throw new QaAdmissionError(`installed QA receipt rejected: ${verdict.reason}`, { command });
  if (receipt.commit !== manifest.commit) throw new QaAdmissionError("installed QA receipt commit differs from the sealed release commit", { command });
  return { digest: receipt.digest };
}
