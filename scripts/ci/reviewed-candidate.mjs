#!/usr/bin/env node
// Trusted, dependency-free promotion of original archives. Never execute source-run code.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { APPROVED_ARTIFACTS_FILE, REPOSITORY, sha, linuxRequiredFor, loadSealed, checkReceipts, checkLocalReceipts, checkApprovedArtifacts, checkUploadPayloads, executeUpload, childEnv, authenticatedArchiveScanResolver, RUNTIME_PROOF_WORKLOADS, localRuntimeProofSha256 } from "./upload.mjs";
import { validateSupplementPin, verifyArchiveSupplement } from "./supplemental-proof.mjs";

const HEX = /^[0-9a-f]{64}$/;
const POS = /^[1-9][0-9]*$/;
const contexts = new WeakMap();
export const SUPPLEMENTAL_PROOFS_FILE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "ci", "supplemental-proofs.json");
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
// Workload ids of each crate that has trusted runtime coverage. The table lives in upload.mjs (the local receipt checks need it
// there); it is re-exported so this module keeps its public name, and a test asserts it equals scripts/ci/runtime-qualify.mjs.
export { RUNTIME_PROOF_WORKLOADS };
const requireThat = (condition, message) => { if (!condition) throw new Error(`reviewed candidate refused: ${message}`); };
function exact(value, keys, label) {
  requireThat(value && typeof value === "object" && !Array.isArray(value) && same(Object.keys(value).sort(), [...keys].sort()), `${label} has unexpected or missing keys`);
}

// A local candidate (source "local") was qualified on Adrian's own hosts through the RustRight broker. Its assets live on a
// GitHub prerelease named by its ID; the committed SHA-256 of each asset is the only thing that binds the bytes, so the
// names are fixed by convention and nothing about them is read from the release.
export const localTag = (id) => `candidate-${id}`;
export const localAssetName = (id, kind) => (kind === "packed" ? `packed-${id}.zip` : `local-receipt-${kind}-${id}.json`);
const LOCAL_RELEASE_HOSTS = ["https://release-assets.githubusercontent.com", "https://objects.githubusercontent.com", "https://github-releases.githubusercontent.com"];

// A local candidate may carry the GitHub qualification run that was dispatched together with the local one (`--with-ci`): the
// clean-room proof that attaches when it lands. `ci` pins which run, so the committed approval names exactly one run.
export const LOCAL_CI_KEYS = ["workflowId", "runId", "sourceSha", "units", "dispatchedAt"];
const ISO_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/;
function validateLocalCi(candidate) {
  const ci = candidate.ci;
  exact(ci, LOCAL_CI_KEYS, "ci run pin");
  requireThat(POS.test(String(ci.workflowId)) && POS.test(String(ci.runId)), "invalid ci workflowId or runId");
  requireThat(ci.sourceSha === candidate.sourceSha, "the ci run must be for the candidate's source commit");
  requireThat(Array.isArray(ci.units) && ci.units.length > 0 && new Set(ci.units).size === ci.units.length && same([...ci.units].sort(), [...candidate.units].sort()), "the ci run must qualify exactly the candidate units");
  requireThat(typeof ci.dispatchedAt === "string" && ISO_TIME.test(ci.dispatchedAt) && !Number.isNaN(Date.parse(ci.dispatchedAt)), "invalid ci dispatchedAt");
}
// A failed CI run blocks promotion unless a reviewer named every unit of the run here, each with a reason.
function validateCiFailure(candidate) {
  const rows = candidate.allow.ciFailure;
  requireThat(candidate.ci !== undefined, "allow.ciFailure needs a ci run pin");
  requireThat(Array.isArray(rows) && rows.length > 0, "allow.ciFailure must be a nonempty list of { unit, reason }");
  for (const row of rows) {
    exact(row, ["unit", "reason"], "allow.ciFailure row");
    requireThat(candidate.units.includes(row.unit) && typeof row.reason === "string" && row.reason.trim() === row.reason && row.reason.length >= 8 && row.reason.length <= 500, "allow.ciFailure rows name a candidate unit and a reason of 8 to 500 characters");
  }
  requireThat(new Set(rows.map((row) => row.unit)).size === rows.length, "allow.ciFailure names a unit twice");
}

function validateLocalCandidate(candidate, linuxFile) {
  const withCi = Object.hasOwn(candidate, "ci");
  exact(candidate, ["id", "source", "sourceSha", "registry", "units", "allow", "release", "assets", ...(withCi ? ["ci"] : [])], "local candidate");
  requireThat(candidate.source === "local", "unknown candidate source");
  requireThat(/^[a-z0-9][a-z0-9-]{0,79}$/.test(candidate.id), "invalid candidate ID");
  requireThat(/^[0-9a-f]{40}$/.test(candidate.sourceSha), "invalid source SHA");
  requireThat(candidate.registry === "crates", "local qualification supports crates only");
  requireThat(Array.isArray(candidate.units) && candidate.units.length > 0 && new Set(candidate.units).size === candidate.units.length, "units must be nonempty, unique & ordered");
  requireThat(candidate.units.every((unit) => typeof unit === "string" && /^rightkit-[a-z0-9][a-z0-9-]*$/.test(unit)), "invalid unit name");
  requireThat(candidate.allow && typeof candidate.allow === "object" && !Array.isArray(candidate.allow), "allowsets has unexpected or missing keys");
  const withCiFailure = Object.hasOwn(candidate.allow, "ciFailure");
  exact(candidate.allow, ["hostExclusion", "noTest", "ignored", "missingAssets", ...(withCiFailure ? ["ciFailure"] : [])], "allowsets");
  for (const [key, value] of Object.entries(candidate.allow)) if (key !== "ciFailure") requireThat(Array.isArray(value) && new Set(value).size === value.length && value.every((unit) => candidate.units.includes(unit)), "allowsets must be unique subsets of candidate units");
  if (withCi) validateLocalCi(candidate);
  if (withCiFailure) validateCiFailure(candidate);
  exact(candidate.release, ["tag"], "release");
  requireThat(candidate.release.tag === localTag(candidate.id), "release tag must be candidate-<id>");
  // A Linux-required unit needs a pinned Linux receipt, unless the candidate pins the CI run that qualifies it on Linux (no local
  // host can); a candidate with neither is refused, never qualified on two hosts only.
  const linux = linuxRequiredFor({ registry: candidate.registry, units: candidate.units, ...(linuxFile ? { file: linuxFile } : {}) });
  requireThat(linux.problems.length === 0, linux.problems.join("; "));
  exact(candidate.assets, ["packed", "macos", "windows", ...(linux.units.length > 0 && !withCi ? ["linux"] : [])], "asset pins");
  for (const [kind, pin] of Object.entries(candidate.assets)) {
    exact(pin, ["name", "sha256"], `${kind} asset pin`);
    requireThat(pin.name === localAssetName(candidate.id, kind), `${kind} asset name must be ${localAssetName(candidate.id, kind)}`);
    requireThat(HEX.test(pin.sha256), `invalid ${kind} asset SHA-256`);
  }
  return candidate;
}

// Schema 2 extends the existing R8 list; candidates never replace per-unit approvals.
export function validateCandidate(candidate, options = {}) {
  // An Actions candidate has no `source` key; its exact key list below is unchanged.
  if (candidate && typeof candidate === "object" && !Array.isArray(candidate) && Object.hasOwn(candidate, "source")) return validateLocalCandidate(candidate, options.linuxFile);
  exact(candidate, ["id", "sourceSha", "runId", "runAttempt", "workflowId", "manifestSha256", "registry", "units", "allow", "artifacts"], "candidate");
  requireThat(/^[a-z0-9][a-z0-9-]{0,79}$/.test(candidate.id), "invalid candidate ID");
  requireThat(/^[0-9a-f]{40}$/.test(candidate.sourceSha), "invalid source SHA");
  for (const key of ["runId", "runAttempt", "workflowId"]) requireThat(POS.test(String(candidate[key])), `invalid ${key}`);
  requireThat(HEX.test(candidate.manifestSha256), "invalid raw manifest hash");
  requireThat(["npm", "crates"].includes(candidate.registry), "invalid registry");
  requireThat(Array.isArray(candidate.units) && candidate.units.length > 0 && new Set(candidate.units).size === candidate.units.length, "units must be nonempty, unique & ordered");
  const name = candidate.registry === "npm" ? /^@rightkit\/[a-z0-9][a-z0-9-]*$/ : /^rightkit-[a-z0-9][a-z0-9-]*$/;
  requireThat(candidate.units.every((unit) => typeof unit === "string" && name.test(unit)), "invalid unit name");
  exact(candidate.allow, ["hostExclusion", "noTest", "ignored", "missingAssets"], "allowsets");
  for (const value of Object.values(candidate.allow)) requireThat(Array.isArray(value) && new Set(value).size === value.length && value.every((unit) => candidate.units.includes(unit)), "allowsets must be unique subsets of candidate units");
  // A candidate whose units require Linux (ci/linux-required.json) must pin the Linux receipt too; any other candidate must not.
  const linux = linuxRequiredFor({ registry: candidate.registry, units: candidate.units });
  requireThat(linux.problems.length === 0, linux.problems.join("; "));
  exact(candidate.artifacts, ["packed", "macos", "windows", ...(linux.units.length > 0 ? ["linux"] : [])], "artifact pins");
  const ids = new Set();
  for (const [host, pin] of Object.entries(candidate.artifacts)) {
    exact(pin, host === "packed" ? ["id", "digest"] : ["id", "digest", "receiptSha256"], `${host} pin`);
    requireThat(POS.test(String(pin.id)) && !ids.has(String(pin.id)), "invalid or duplicate artifact ID"); ids.add(String(pin.id));
    requireThat(/^sha256:[0-9a-f]{64}$/.test(pin.digest), "artifact requires GitHub SHA-256 digest");
    if (host !== "packed") requireThat(HEX.test(pin.receiptSha256), "invalid raw receipt hash");
  }
  return candidate;
}

export function selectCandidate(file, id) {
  requireThat(/^[a-z0-9][a-z0-9-]{0,79}$/.test(id ?? ""), "candidate ID is required; URLs & paths are forbidden");
  const doc = JSON.parse(fs.readFileSync(file, "utf8"));
  requireThat(doc.schema === 2 && Array.isArray(doc.candidates), "committed schema 2 candidates are required");
  const seen = new Set();
  for (const candidate of doc.candidates) { validateCandidate(candidate); requireThat(!seen.has(candidate.id), "duplicate candidate ID"); seen.add(candidate.id); }
  const found = doc.candidates.filter((candidate) => candidate.id === id);
  requireThat(found.length === 1, "candidate is not committed exactly once");
  return found[0];
}

/** Separate committed pins never alter original candidate or receipt schemas. */
export function selectSupplements(file, candidate, kind, approvedFile = APPROVED_ARTIFACTS_FILE) {
  const doc = JSON.parse(fs.readFileSync(file, "utf8"));
  exact(doc, ["schema", "supplements"], "supplement collection");
  requireThat(doc.schema === 1 && Array.isArray(doc.supplements), "unknown supplement collection schema");
  const seen = new Set();
  for (const pin of doc.supplements) {
    const id = pin?.original?.candidateId;
    // One pin per candidate and kind, except runtime proofs: a candidate may carry one per crate, told apart by proof hash.
    const identity = `${id}:${pin?.kind}${pin?.kind === "runtime-workload-pin" ? `:${pin?.artifact?.proofSha256}` : ""}`;
    requireThat(typeof id === "string" && !seen.has(identity), "invalid or duplicate supplemental candidate/kind");
    seen.add(identity);
    validateSupplementPin(pin, selectCandidate(approvedFile, id));
  }
  const pins = doc.supplements.filter((row) => row.original.candidateId === candidate.id && row.kind === kind);
  for (const pin of pins) validateSupplementPin(pin, candidate);
  return pins;
}
export function selectSupplement(file, candidate, kind, approvedFile = APPROVED_ARTIFACTS_FILE) {
  const pins = selectSupplements(file, candidate, kind, approvedFile);
  requireThat(pins.length <= 1, "more than one supplement pin of this kind; select them all");
  return pins[0] ?? null;
}
/** A qualified runtime row must name a trusted-coverage crate, its trusted workloads, and one pin of this candidate. */
export function validateRuntimeTransfer(row, pins, candidateId) {
  const proof = row.runtimeFeatureProof;
  exact(proof.proof, ["candidateId", "proofSha256", "workloads"], "runtime supplement transfer");
  const workloads = Object.hasOwn(RUNTIME_PROOF_WORKLOADS, row.name) ? RUNTIME_PROOF_WORKLOADS[row.name] : null;
  const pin = pins.find((item) => item.artifact.proofSha256 === proof.proof.proofSha256);
  requireThat(workloads && proof.classification === "always-on" && proof.listedOwner === true && proof.required === true && pin && proof.proof.candidateId === candidateId && same(proof.proof.workloads, workloads), "runtime supplement transfer mismatch");
}
export const selectArchiveSupplement = (file, candidate, approvedFile = APPROVED_ARTIFACTS_FILE) => selectSupplement(file, candidate, "archive-scan-supplement-pin", approvedFile);

export function checkProvenance({ candidate, repository, workflow, run, jobs, promotionSha, comparison }) {
  requireThat(repository.full_name === REPOSITORY && repository.default_branch === "main", "wrong repository/default branch");
  requireThat(String(workflow.id) === String(candidate.workflowId) && workflow.path === ".github/workflows/publish.yml" && workflow.state === "active", "wrong source workflow");
  requireThat(String(run.id) === String(candidate.runId) && Number(run.run_attempt) === Number(candidate.runAttempt), "wrong run/attempt");
  requireThat(run.repository?.id === repository.id && run.repository?.full_name === REPOSITORY && run.head_repository?.id === repository.id && run.head_repository?.full_name === REPOSITORY, "foreign source/head repository");
  requireThat(String(run.workflow_id) === String(candidate.workflowId) && run.path === ".github/workflows/publish.yml", "wrong run workflow");
  requireThat(run.event === "workflow_dispatch" && run.head_branch === "main" && run.head_sha === candidate.sourceSha && run.status === "completed", "source event/ref/SHA/status mismatch");
  requireThat(promotionSha !== candidate.sourceSha && comparison.status === "ahead" && comparison.merge_base_commit?.sha === candidate.sourceSha, "promotion must descend from original source commit");
  const rows = jobs.jobs;
  requireThat(Array.isArray(rows) && rows.length === jobs.total_count, "incomplete job provenance");
  for (const name of ["guard", "plan", "pack"]) {
    const found = rows.filter((job) => job.name === name);
    requireThat(found.length === 1 && found[0].conclusion === "success", `${name} did not succeed`);
  }
  const verify = rows.filter((job) => /^verify \(/.test(job.name));
  requireThat(verify.length === 2 && verify.every((job) => job.conclusion === "success") && verify.some((job) => /^verify \(xcode-27, /.test(job.name)) && verify.some((job) => /^verify \(windows-2025, /.test(job.name)), "both native verify jobs must succeed");
  // verify-linux must have succeeded exactly once when the candidate requires Linux, and must not have run otherwise.
  const linuxRequired = linuxRequiredFor({ registry: candidate.registry, units: candidate.units });
  requireThat(linuxRequired.problems.length === 0, linuxRequired.problems.join("; "));
  const linuxJobs = rows.filter((job) => job.name === "verify-linux");
  requireThat(linuxRequired.units.length > 0 ? linuxJobs.length === 1 && linuxJobs[0].conclusion === "success" : linuxJobs.every((job) => job.conclusion === "skipped"), "the Linux verify job must succeed exactly when the candidate requires Linux");
  requireThat(rows.every((job) => String(job.run_id) === String(candidate.runId) && String(job.run_attempt) === String(candidate.runAttempt) && job.head_sha === candidate.sourceSha), "job provenance mismatch");
}

/**
 * A local candidate's provenance: the prerelease and its tag are the committed candidate's, the tag points at the
 * original source commit, the release holds exactly the pinned asset names, and the promotion descends from the source.
 * Nothing here trusts an asset's content; each asset is hash-checked against its committed pin after download.
 */
export function checkLocalProvenance({ candidate, repository, release, ref, promotionSha, comparison }) {
  requireThat(repository.full_name === REPOSITORY && repository.default_branch === "main", "wrong repository/default branch");
  const tag = candidate.release.tag;
  requireThat(release?.tag_name === tag && release.prerelease === true && release.draft === false, "candidate release is not the published prerelease of its tag");
  requireThat(ref?.ref === `refs/tags/${tag}` && ref.object?.type === "commit" && ref.object.sha === candidate.sourceSha, "candidate tag does not point at the original source commit");
  const expected = Object.entries(candidate.assets).map(([kind]) => localAssetName(candidate.id, kind)).sort();
  const assets = Array.isArray(release.assets) ? release.assets : [];
  requireThat(JSON.stringify(assets.map((asset) => asset?.name).sort()) === JSON.stringify(expected), "candidate release assets differ from the pinned asset set");
  requireThat(assets.every((asset) => POS.test(String(asset.id)) && asset.state === "uploaded" && Number.isSafeInteger(asset.size) && asset.size > 0 && asset.size <= 512 * 1024 * 1024) && new Set(assets.map((asset) => String(asset.id))).size === assets.length, "candidate release has an incomplete or duplicate asset");
  requireThat(promotionSha !== candidate.sourceSha && comparison.status === "ahead" && comparison.merge_base_commit?.sha === candidate.sourceSha, "promotion must descend from original source commit");
}

// ---- the CI run that rides with a local candidate (`ci`) --------------------------------------------------------
export const CI_CONCLUSIONS = Object.freeze(["success", "failure", "pending"]);
export const CI_PROOF_KEYS = Object.freeze(["runId", "runAttempt", "conclusion", "checkedAt", "requiredUnits", "failedJobs"]);

/**
 * The units whose proof only the CI run can give: the Linux-required ones (ci/linux-required.json; no local host is Linux). For
 * these the run must have SUCCEEDED; for every other unit it is attached proof and a failure needs a reviewer's allow.ciFailure.
 * A runtime proof is NOT one of them: publish.yml carries no runtime workload, so a held runtime proof is refused as before.
 */
export function ciRequiredUnits({ candidate, linuxFile }) {
  const linux = linuxRequiredFor({ registry: candidate.registry, units: candidate.units, ...(linuxFile ? { file: linuxFile } : {}) });
  requireThat(linux.problems.length === 0, linux.problems.join("; "));
  return [...linux.units].sort();
}

/** Problems with a COMPLETED, successful run's jobs: the same jobs checkProvenance demands of an Actions candidate's run. */
function ciJobProblems(rows, linuxUnits) {
  const problems = [];
  for (const name of ["guard", "plan", "pack"]) {
    const found = rows.filter((job) => job.name === name);
    if (!(found.length === 1 && found[0].conclusion === "success")) problems.push(`${name} did not succeed`);
  }
  const verify = rows.filter((job) => /^verify \(/.test(job.name));
  if (!(verify.length === 2 && verify.every((job) => job.conclusion === "success") && verify.some((job) => /^verify \(xcode-27, /.test(job.name)) && verify.some((job) => /^verify \(windows-2025, /.test(job.name)))) problems.push("both native verify jobs must succeed");
  const linuxJobs = rows.filter((job) => job.name === "verify-linux");
  if (!(linuxUnits.length > 0 ? linuxJobs.length === 1 && linuxJobs[0].conclusion === "success" : linuxJobs.every((job) => job.conclusion === "skipped"))) problems.push("the Linux verify job must succeed exactly when the run covers Linux-required units");
  return problems;
}

/**
 * What the candidate's pinned run is, read from the Actions API: { runId, runAttempt, conclusion, failedJobs }. The run must be the
 * pinned one of the publish workflow on main at the candidate's commit, dispatched by hand in this repository (the checks an Actions
 * candidate's run passes). conclusion: "pending" while it runs, "success" only when the run AND every required job succeeded,
 * otherwise "failure" with the names of the jobs that did not. Throws on any provenance mismatch.
 */
export function checkCiRun({ candidate, repository, workflow, run, jobs, linuxFile }) {
  const ci = candidate.ci;
  requireThat(ci, "candidate pins no ci run");
  requireThat(repository.full_name === REPOSITORY && repository.default_branch === "main", "wrong repository/default branch");
  requireThat(String(workflow.id) === String(ci.workflowId) && workflow.path === ".github/workflows/publish.yml" && workflow.state === "active", "ci run: wrong source workflow");
  requireThat(String(run.id) === String(ci.runId) && POS.test(String(run.run_attempt)), "ci run: wrong run");
  requireThat(run.repository?.id === repository.id && run.repository?.full_name === REPOSITORY && run.head_repository?.id === repository.id && run.head_repository?.full_name === REPOSITORY, "ci run: foreign source/head repository");
  requireThat(String(run.workflow_id) === String(ci.workflowId) && run.path === ".github/workflows/publish.yml", "ci run: wrong run workflow");
  requireThat(run.event === "workflow_dispatch" && run.head_branch === "main" && run.head_sha === ci.sourceSha && ci.sourceSha === candidate.sourceSha, "ci run: event/ref/SHA mismatch");
  const attempt = Number(run.run_attempt);
  if (run.status !== "completed") return { runId: String(run.id), runAttempt: attempt, conclusion: "pending", failedJobs: [] };
  const rows = jobs?.jobs;
  requireThat(Array.isArray(rows) && rows.length === jobs.total_count, "ci run: incomplete job provenance");
  requireThat(rows.every((job) => String(job.run_id) === String(run.id) && String(job.run_attempt) === String(run.run_attempt) && job.head_sha === ci.sourceSha), "ci run: job provenance mismatch");
  const failedJobs = rows.filter((job) => job.conclusion !== "success" && job.conclusion !== "skipped").map((job) => String(job.name));
  if (run.conclusion !== "success") return { runId: String(run.id), runAttempt: attempt, conclusion: "failure", failedJobs: failedJobs.length ? failedJobs : [`run ${String(run.conclusion)}`] };
  const linux = linuxRequiredFor({ registry: candidate.registry, units: ci.units, ...(linuxFile ? { file: linuxFile } : {}) });
  requireThat(linux.problems.length === 0, linux.problems.join("; "));
  const problems = ciJobProblems(rows, linux.units);
  requireThat(problems.length === 0, `ci run reports success but ${problems.join("; ")}`);
  return { runId: String(run.id), runAttempt: attempt, conclusion: "success", failedJobs: [] };
}

/**
 * The promotion decision on a ci proof: a unit only CI can prove needs a SUCCESSFUL run (pending is refused); otherwise pending
 * proceeds (the local bytes are the approved ones, the run is attached when it lands) and a failure is refused, naming the failed
 * jobs, unless allow.ciFailure names every unit of the run with a reason, which is a reviewer's decision committed with the approval.
 */
export function ciPolicy({ candidate, proof, requiredUnits }) {
  const failed = proof.failedJobs.length ? ` (failed jobs: ${proof.failedJobs.join(", ")})` : "";
  if (requiredUnits.length > 0) requireThat(proof.conclusion === "success", `ci run ${proof.runId} is ${proof.conclusion}${failed}; its success is required for ${requiredUnits.join(", ")}, which local qualification cannot prove, and no allowance covers that`);
  if (proof.conclusion === "failure") {
    const named = new Set((candidate.allow.ciFailure ?? []).map((row) => row.unit));
    requireThat(candidate.units.every((unit) => named.has(unit)), `ci run ${proof.runId} failed${failed}; promotion needs allow.ciFailure to name every unit of the run with a reason`);
  }
}

/** Query the pinned run (read-only, the promotion's own GitHub token) and return the proof the admission records. */
export async function ciProofFor({ candidate, repository, env, fetchImpl, now = () => new Date().toISOString(), linuxFile }) {
  const ci = candidate.ci;
  const workflow = await api("actions/workflows/publish.yml", env, fetchImpl);
  const run = await api(`actions/runs/${ci.runId}`, env, fetchImpl);
  requireThat(POS.test(String(run?.run_attempt)), "ci run: no run attempt");
  const jobs = run?.status === "completed" ? await api(`actions/runs/${ci.runId}/attempts/${run.run_attempt}/jobs?per_page=100`, env, fetchImpl) : null;
  const checked = checkCiRun({ candidate, repository, workflow, run, jobs, linuxFile });
  const proof = { ...checked, checkedAt: now(), requiredUnits: ciRequiredUnits({ candidate, linuxFile }) };
  const ordered = Object.fromEntries(CI_PROOF_KEYS.map((key) => [key, proof[key]]));
  ciPolicy({ candidate, proof: ordered, requiredUnits: ordered.requiredUnits });
  return ordered;
}

/** One release asset's bytes through the API (octet-stream), following only the one redirect to GitHub's release storage. */
export async function releaseAssetBytes(id, env, fetchImpl) {
  const response = await fetchImpl(`https://api.github.com/repos/${REPOSITORY}/releases/assets/${id}`, { headers: { authorization: `Bearer ${env.GITHUB_TOKEN}`, accept: "application/octet-stream", "x-github-api-version": "2022-11-28" }, redirect: "manual", signal: AbortSignal.timeout(60_000) });
  if (response.status === 302) {
    const url = new URL(response.headers.get("location"));
    requireThat(url.protocol === "https:" && !url.username && !url.password && !url.port && LOCAL_RELEASE_HOSTS.includes(url.origin), "unsafe release asset redirect");
    const download = await fetchImpl(url, { redirect: "error", signal: AbortSignal.timeout(120_000) });
    requireThat(download.ok, "release asset download failed");
    return Buffer.from(await download.arrayBuffer());
  }
  requireThat(response.ok, `GitHub release asset ${id} returned ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

export async function api(route, env, fetchImpl, binary = false) {
  const response = await fetchImpl(`https://api.github.com/repos/${REPOSITORY}${route ? `/${route}` : ""}`, { headers: { authorization: `Bearer ${env.GITHUB_TOKEN}`, accept: "application/vnd.github+json", "x-github-api-version": "2022-11-28" }, redirect: "manual", signal: AbortSignal.timeout(60_000) });
  if (binary && response.status === 302) {
    const url = new URL(response.headers.get("location"));
    requireThat(url.protocol === "https:" && (url.hostname.endsWith(".blob.core.windows.net") || url.hostname.endsWith(".actions.githubusercontent.com") || url.hostname === "productionresultssa0.blob.core.windows.net"), "unsafe artifact redirect");
    const download = await fetchImpl(url, { redirect: "error", signal: AbortSignal.timeout(120_000) });
    requireThat(download.ok, "artifact download failed");
    return Buffer.from(await download.arrayBuffer());
  }
  requireThat(response.ok, `GitHub API ${route} returned ${response.status}`);
  return binary ? Buffer.from(await response.arrayBuffer()) : response.json();
}

export function unpackArtifact({ bytes, zipFile, destination, spawn = spawnSync }) {
  fs.mkdirSync(destination, { recursive: true }); fs.writeFileSync(zipFile, bytes);
  const invoke = (args, encoding) => {
    const result = spawn("unzip", args, { encoding, maxBuffer: 512 * 1024 * 1024, env: childEnv(process.env) });
    requireThat(!result.error && result.status === 0, "cannot read pinned ZIP"); return result.stdout;
  };
  const names = String(invoke(["-Z1", zipFile], "utf8")).split(/\r?\n/).filter(Boolean);
  requireThat(names.length > 0 && names.length < 1000 && new Set(names).size === names.length && names.every((name) => /^[A-Za-z0-9][A-Za-z0-9._-]{0,200}$/.test(name)), "unsafe/duplicate/nested ZIP entries");
  // Stream individual members to fresh regular files; no ZIP path or symlink is extracted.
  for (const name of names) fs.writeFileSync(path.join(destination, name), invoke(["-p", zipFile, name], "buffer"), { flag: "wx" });
  return names;
}

export async function authenticateSource({ env, candidateId, approvedFile = APPROVED_ARTIFACTS_FILE, fetchImpl = fetch }) {
  requireThat(env.GITHUB_ACTIONS === "true" && env.GITHUB_REPOSITORY === REPOSITORY && env.GITHUB_REF === "refs/heads/main" && /^[0-9a-f]{40}$/.test(env.GITHUB_SHA ?? ""), "promotion requires trusted main Actions context");
  requireThat(env.GITHUB_TOKEN && env.RUNNER_TEMP, "GitHub read token & runner temp are required");
  requireThat(env.GITHUB_WORKFLOW_REF === `${REPOSITORY}/.github/workflows/publish-reviewed.yml@refs/heads/main`, "wrong promotion workflow");
  const candidate = selectCandidate(approvedFile, candidateId);
  // All API routes derive from committed identifiers, never dispatch URLs or paths.
  const repository = await api("", env, fetchImpl);
  if (candidate.source === "local") {
    const release = await api(`releases/tags/${candidate.release.tag}`, env, fetchImpl);
    const ref = await api(`git/ref/tags/${candidate.release.tag}`, env, fetchImpl);
    const comparison = await api(`compare/${candidate.sourceSha}...${env.GITHUB_SHA}`, env, fetchImpl);
    checkLocalProvenance({ candidate, repository, release, ref, promotionSha: env.GITHUB_SHA, comparison });
    return { candidate, repository, release };
  }
  const workflow = await api("actions/workflows/publish.yml", env, fetchImpl);
  const run = await api(`actions/runs/${candidate.runId}/attempts/${candidate.runAttempt}`, env, fetchImpl);
  const jobs = await api(`actions/runs/${candidate.runId}/attempts/${candidate.runAttempt}/jobs?per_page=100`, env, fetchImpl);
  const comparison = await api(`compare/${candidate.sourceSha}...${env.GITHUB_SHA}`, env, fetchImpl);
  checkProvenance({ candidate, repository, workflow, run, jobs, promotionSha: env.GITHUB_SHA, comparison });
  return { candidate, repository };
}

export async function identifyCandidate(options) {
  const { candidate } = await authenticateSource(options);
  if (options.env.GITHUB_OUTPUT) fs.appendFileSync(options.env.GITHUB_OUTPUT, `source_sha=${candidate.sourceSha}\nregistry=${candidate.registry}\n`);
  return { sourceSha: candidate.sourceSha, registry: candidate.registry };
}

const freezeDeep = (value) => { if (value && typeof value === "object") { Object.values(value).forEach(freezeDeep); Object.freeze(value); } };

/** The four allowsets every checker knows; `ciFailure` is a promotion decision about the CI run, not an allowance for a receipt. */
export const localAllow = (candidate) => { const { ciFailure, ...allow } = candidate.allow; return allow; };
/** A candidate that pins its CI run is qualified on Linux by that run, not by a local Linux receipt (there is none to give). */
export const localLinuxPolicy = (candidate) => (candidate.ci ? { linuxRequired: [] } : {});

/** Local candidate: download each pinned release asset, require its committed SHA-256, rebuild the packed/receipts layout. */
async function prepareLocal({ env, candidate, repository, release, approvedFile, supplementalFile, fetchImpl, spawn, linuxFile }) {
  const root = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "reviewed-candidate-"));
  const packDir = path.join(root, "packed"); const receiptsDir = path.join(root, "receipts");
  for (const [kind, pin] of Object.entries(candidate.assets)) {
    const rows = release.assets.filter((asset) => asset.name === pin.name);
    requireThat(rows.length === 1, `${kind} asset must exist exactly once`);
    const bytes = await releaseAssetBytes(rows[0].id, env, fetchImpl);
    requireThat(bytes.length === rows[0].size && sha("sha256", bytes) === pin.sha256, `${kind} asset size or SHA-256 differs from the committed pin`);
    if (kind === "packed") unpackArtifact({ bytes, zipFile: path.join(root, "packed.zip"), destination: packDir, spawn });
    else { fs.mkdirSync(path.join(receiptsDir, kind), { recursive: true }); fs.writeFileSync(path.join(receiptsDir, kind, pin.name), bytes, { flag: "wx" }); }
  }
  const sealed = loadSealed({ packDir, registry: candidate.registry, unitsText: candidate.units.join(" ") });
  requireThat(sealed.manifest && sealed.manifest.commit === candidate.sourceSha && sealed.manifest.source === "local" && sealed.manifest.candidateId === candidate.id, "source/candidate binding mismatch in the packed manifest");
  const problems = [...sealed.problems, ...checkApprovedArtifacts({ manifest: sealed.manifest, registry: candidate.registry, approvedFile }), ...checkLocalReceipts({ manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, receiptsDir, allow: localAllow(candidate), candidate, ...localLinuxPolicy(candidate) })];
  requireThat(problems.length === 0, problems.join("; "));
  const context = JSON.parse(JSON.stringify({ candidate })); freezeDeep(context);
  contexts.set(context, { env: { ...env }, packDir, receiptsDir, approvedFile, supplementalFile, archiveScanResolver: undefined, manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, repository, linuxFile, admitted: false });
  return { context, packDir, receiptsDir };
}

export async function prepareCandidate({ env, candidateId, approvedFile = APPROVED_ARTIFACTS_FILE, supplementalFile = SUPPLEMENTAL_PROOFS_FILE, fetchImpl = fetch, spawn = spawnSync, linuxFile }) {
  const { candidate, repository, release } = await authenticateSource({ env, candidateId, approvedFile, fetchImpl });
  if (candidate.source === "local") return prepareLocal({ env, candidate, repository, release, approvedFile, supplementalFile, fetchImpl, spawn, linuxFile });
  const supplementPin = selectArchiveSupplement(supplementalFile, candidate, approvedFile);
  const root = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "reviewed-candidate-"));
  const packDir = path.join(root, "packed"); const receiptsDir = path.join(root, "receipts");
  const expectedNames = { packed: "publish-packed", macos: "publish-receipt-verify-xcode-27", windows: "publish-receipt-verify-windows-2025", linux: "publish-receipt-verify-ubuntu-24.04" };
  for (const [host, pin] of Object.entries(candidate.artifacts)) {
    const artifact = await api(`actions/artifacts/${pin.id}`, env, fetchImpl);
    requireThat(String(artifact.id) === String(pin.id) && artifact.name === expectedNames[host] && artifact.expired === false && artifact.digest === pin.digest && String(artifact.workflow_run?.id) === String(candidate.runId) && artifact.workflow_run?.head_sha === candidate.sourceSha && artifact.workflow_run?.head_branch === "main" && artifact.workflow_run?.repository_id === repository.id && artifact.workflow_run?.head_repository_id === repository.id, `${host} artifact provenance/digest mismatch`);
    const bytes = await api(`actions/artifacts/${pin.id}/zip`, env, fetchImpl, true);
    requireThat(`sha256:${sha("sha256", bytes)}` === pin.digest, `${host} downloaded ZIP digest mismatch`);
    const destination = host === "packed" ? packDir : path.join(receiptsDir, host);
    const names = unpackArtifact({ bytes, zipFile: path.join(root, `${host}.zip`), destination, spawn });
    if (host !== "packed") {
      requireThat(names.length === 1 && names[0].endsWith(".json"), `${host} receipt artifact is not exactly one JSON file`);
      requireThat(sha("sha256", fs.readFileSync(path.join(destination, names[0]))) === pin.receiptSha256, `${host} raw receipt hash mismatch`);
    }
  }
  const sealed = loadSealed({ packDir, registry: candidate.registry, unitsText: candidate.units.join(" "), run: { runId: candidate.runId, runAttempt: candidate.runAttempt } });
  requireThat(sealed.manifest && sealed.manifest.commit === candidate.sourceSha && String(sealed.manifest.runAttempt) === String(candidate.runAttempt) && sealed.manifestSha256 === candidate.manifestSha256, "source/attempt/raw manifest binding mismatch");
  let archiveScanResolver;
  if (supplementPin) {
    const evidence = await verifyArchiveSupplement({ candidate, pin: supplementPin, manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, env, fetchImpl, spawn });
    archiveScanResolver = await authenticatedArchiveScanResolver(evidence, candidate, sealed.manifest);
  }
  const problems = [...sealed.problems, ...checkApprovedArtifacts({ manifest: sealed.manifest, registry: candidate.registry, approvedFile }), ...checkReceipts({ manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, receiptsDir, allow: candidate.allow, run: { runId: candidate.runId, runAttempt: candidate.runAttempt }, archiveScanResolver })];
  requireThat(problems.length === 0, problems.join("; "));
  // Freeze the authenticated record; caller cannot widen its allowsets after admission.
  const context = JSON.parse(JSON.stringify({ candidate }));
  const freeze = (value) => { if (value && typeof value === "object") { Object.values(value).forEach(freeze); Object.freeze(value); } }; freeze(context);
  contexts.set(context, { env: { ...env }, packDir, receiptsDir, approvedFile, supplementalFile, archiveScanResolver, manifest: sealed.manifest, repository, admitted: false });
  return { context, packDir, receiptsDir };
}

/** The ci proof the authenticated admission recorded for this context's candidate (null when it pins no CI run). */
export function ciProofOf(context) {
  const bound = contexts.get(context);
  requireThat(bound && bound.admitted === true, "semantic admission is required");
  return bound.ciProof ?? null;
}

export function assertAuthenticatedContext(context, actual) {
  const bound = contexts.get(context);
  requireThat(bound && ["packDir", "receiptsDir", "approvedFile"].every((key) => bound[key] === actual[key]), "unauthenticated candidate context");
  for (const [key, value] of Object.entries(bound.env)) if (key.startsWith("GITHUB_")) requireThat(actual.env[key] === value, "promotion GitHub identity changed");
  requireThat(bound.admitted === true, "semantic admission is required");
  const candidate = context.candidate;
  const local = candidate.source === "local";
  const pins = local ? candidate.assets : candidate.artifacts;
  for (const host of Object.keys(pins).filter((key) => key !== "packed")) {
    const directory = path.join(bound.receiptsDir, host);
    const files = fs.readdirSync(directory);
    requireThat(files.length === 1 && sha("sha256", fs.readFileSync(path.join(directory, files[0]))) === (local ? pins[host].sha256 : pins[host].receiptSha256), "original native receipt changed after admission");
  }
  requireThat(actual.env.PUBLISH_REGISTRY === candidate.registry && actual.env.PUBLISH_UNITS === candidate.units.join(" "), "candidate plan changed");
  for (const [key, envKey] of Object.entries({ hostExclusion: "HOST_EXCLUSION", noTest: "NO_TEST", ignored: "IGNORED", missingAssets: "MISSING_ASSETS" })) requireThat((actual.env[`PUBLISH_ALLOW_${envKey}`] ?? "") === candidate.allow[key].join(" "), "candidate allowsets changed");
  return bound.archiveScanResolver;
}

// ZIP metadata is inspected before streaming selected members. No untrusted entry is
// materialized. Reject ZIP64, links, duplicate/unsafe names & oversized JSON proofs.
function proofZip(bytes, zipFile, wanted, spawn) {
  requireThat(Buffer.isBuffer(bytes) && bytes.length <= 512 * 1024 * 1024 && bytes.length >= 22, "invalid proof ZIP size");
  let end = -1;
  for (let i = bytes.length - 22; i >= Math.max(0, bytes.length - 65557); i--) {
    if (bytes.readUInt32LE(i) === 0x06054b50 && i + 22 + bytes.readUInt16LE(i + 20) === bytes.length) { end = i; break; }
  }
  requireThat(end >= 0 && bytes.readUInt16LE(end + 4) === 0 && bytes.readUInt16LE(end + 6) === 0, "invalid/multidisk proof ZIP");
  const count = bytes.readUInt16LE(end + 10), size = bytes.readUInt32LE(end + 12), offset = bytes.readUInt32LE(end + 16);
  requireThat(count > 0 && count <= 10000 && bytes.readUInt16LE(end + 8) === count && offset + size === end, "invalid/incomplete proof ZIP directory");
  const entries = new Map(); let cursor = offset;
  for (let i = 0; i < count; i++) {
    requireThat(cursor + 46 <= end && bytes.readUInt32LE(cursor) === 0x02014b50, "invalid proof ZIP entry");
    const flags = bytes.readUInt16LE(cursor + 8), mode = bytes.readUInt32LE(cursor + 38) >>> 16;
    const length = bytes.readUInt16LE(cursor + 28), extra = bytes.readUInt16LE(cursor + 30), comment = bytes.readUInt16LE(cursor + 32);
    const next = cursor + 46 + length + extra + comment;
    requireThat(next <= end && bytes.readUInt16LE(cursor + 34) === 0 && !(flags & 1), "encrypted/foreign proof ZIP entry");
    const nameBytes = bytes.subarray(cursor + 46, cursor + 46 + length), name = nameBytes.toString("utf8");
    const parts = name.replace(/\/$/, "").split("/");
    requireThat(Buffer.from(name).equals(nameBytes) && name.length <= 1024 && !/[\\\x00-\x1f\x7f:]/.test(name) && parts.every((part) => /^[A-Za-z0-9][A-Za-z0-9._ -]*$/.test(part) && !/[. ]$/.test(part)) && !entries.has(name), "unsafe/duplicate proof ZIP path");
    const type = mode & 0xf000, directory = name.endsWith("/");
    requireThat((type === 0 || type === (directory ? 0x4000 : 0x8000)) && !(bytes.readUInt32LE(cursor + 38) & 0x400), "proof ZIP contains nonregular entry");
    entries.set(name, { size: bytes.readUInt32LE(cursor + 24), directory }); cursor = next;
  }
  requireThat(cursor === end, "extra proof ZIP directory data");
  fs.writeFileSync(zipFile, bytes, { flag: "wx" });
  const proofs = {};
  for (const name of wanted) {
    const entry = entries.get(name);
    requireThat(entry && !entry.directory && entry.size > 0 && entry.size <= 16 * 1024 * 1024, `missing/oversized regular ${name}`);
    const result = spawn("unzip", ["-p", zipFile, name], { encoding: "buffer", maxBuffer: 16 * 1024 * 1024, env: childEnv(process.env), timeout: 30_000, windowsHide: true });
    requireThat(!result.error && result.status === 0 && Buffer.byteLength(result.stdout ?? "") === entry.size, `cannot read exact ${name}`);
    const raw = Buffer.from(result.stdout); proofs[name] = { value: JSON.parse(raw.toString("utf8")), sha256: sha("sha256", raw) };
  }
  return { proofs, names: [...entries.keys()] };
}

export const artifactMatches = (artifact, { id, digest, name, runId, sourceSha, repository }) =>
  POS.test(String(artifact?.id)) && (!id || String(artifact.id) === String(id)) && /^sha256:[0-9a-f]{64}$/.test(artifact.digest ?? "") && (!digest || artifact.digest === digest) && artifact.name === name && artifact.expired === false && String(artifact.workflow_run?.id) === String(runId) && artifact.workflow_run?.head_sha === sourceSha && artifact.workflow_run?.head_branch === "main" && artifact.workflow_run?.repository_id === repository.id && artifact.workflow_run?.head_repository_id === repository.id;

async function originalQa({ prepared, env, fetchImpl, spawn }) {
  const bound = contexts.get(prepared.context), candidate = prepared.context.candidate;
  const rows = []; let expected;
  for (let page = 1; page <= 10; page++) {
    const listing = await api(`actions/runs/${candidate.runId}/artifacts?per_page=100&page=${page}`, env, fetchImpl);
    requireThat(Number.isSafeInteger(listing.total_count) && listing.total_count >= 0 && listing.total_count <= 1000 && Array.isArray(listing.artifacts), "invalid QA artifact listing");
    expected ??= listing.total_count;
    requireThat(listing.total_count === expected && listing.artifacts.length <= 100, "QA artifact listing changed"); rows.push(...listing.artifacts);
    if (rows.length === expected) break;
    requireThat(listing.artifacts.length === 100 && rows.length < expected && page < 10, "incomplete QA artifact listing");
  }
  requireThat(rows.length === expected && new Set(rows.map((row) => String(row.id))).size === rows.length, "incomplete/duplicate QA artifacts");
  const qa = {}, bindings = {};
  for (const [host, runner] of Object.entries({ macos: "xcode-27", windows: "windows-2025" })) {
    const name = `publish-verify-${runner}-${candidate.runAttempt}`;
    const found = rows.filter((row) => row.name === name);
    requireThat(found.length === 1, `expected exactly one original ${host} QA artifact`);
    requireThat(POS.test(String(found[0].id)) && /^sha256:[a-f0-9]{64}$/.test(found[0].digest ?? ""), `${host} invalid QA artifact ID/digest`);
    const artifact = await api(`actions/artifacts/${found[0].id}`, env, fetchImpl);
    requireThat(artifactMatches(artifact, { id: found[0].id, digest: found[0].digest, name, runId: candidate.runId, sourceSha: candidate.sourceSha, repository: bound.repository }), `${host} QA artifact provenance mismatch`);
    const bytes = await api(`actions/artifacts/${artifact.id}/zip`, env, fetchImpl, true);
    requireThat(`sha256:${sha("sha256", bytes)}` === artifact.digest, `${host} QA ZIP digest mismatch`);
    const { proofs } = proofZip(bytes, path.join(path.dirname(prepared.packDir), `qa-${host}.zip`), ["summary.json", "unit-evidence.json"], spawn);
    qa[host] = { summary: proofs["summary.json"].value, unitEvidence: proofs["unit-evidence.json"].value, summarySha256: proofs["summary.json"].sha256, unitEvidenceSha256: proofs["unit-evidence.json"].sha256 };
    bindings[host] = { id: String(artifact.id), digest: artifact.digest, summarySha256: qa[host].summarySha256, unitEvidenceSha256: qa[host].unitEvidenceSha256 };
  }
  return { qa, bindings };
}

function originalBinding(candidate, manifest, manifestSha256 = candidate.manifestSha256) {
  return { candidateId: candidate.id, candidateSha256: sha("sha256", JSON.stringify(candidate)), sourceSha: candidate.sourceSha, manifestSha256, archives: manifest.units.map(({ name, version, file, sha256, metaFile, metaSha256 }) => ({ name, version, file, sha256, ...(metaFile ? { metaFile, metaSha256 } : {}) })) };
}
const safeHashes = (hashes) => hashes && typeof hashes === "object" && !Array.isArray(hashes) && Object.keys(hashes).length > 0 && Object.entries(hashes).every(([name, hash]) => name.length <= 1024 && !/[\\\x00-\x1f:]/.test(name) && name.split("/").every((part) => part && part !== "." && part !== "..") && HEX.test(hash));

/**
 * A runtime owner of a local candidate is admitted only as "qualified" by the local runtime receipts: the transfer must
 * name the trusted workload ids and carry the hash of the runtime evidence in the PINNED receipts, recomputed here from the
 * receipt bytes the credentialed job itself downloaded and hash-checked. The deep checks ran in the secretless admission.
 */
function validateLocalRuntimeTransfer(row, candidate, receiptsDir) {
  const proof = row.runtimeFeatureProof;
  requireThat(proof.status === "qualified" && proof.classification === "always-on" && proof.listedOwner === true && proof.required === true, "local runtime owner is not qualified by its runtime receipts");
  exact(proof.proof, ["candidateId", "proofSha256", "workloads"], "local runtime transfer");
  requireThat(proof.proof.candidateId === candidate.id && same(proof.proof.workloads, RUNTIME_PROOF_WORKLOADS[row.name]), "local runtime transfer names another candidate or workload set");
  const docs = {};
  for (const host of ["macos", "windows"]) {
    const directory = path.join(receiptsDir, host);
    docs[host] = JSON.parse(fs.readFileSync(path.join(directory, fs.readdirSync(directory)[0]), "utf8"));
  }
  requireThat(HEX.test(proof.proof.proofSha256) && proof.proof.proofSha256 === localRuntimeProofSha256(docs, row.name), "local runtime proof hash differs from the pinned receipts");
}

// The local admission carries no Actions QA artifacts: it names the pinned receipts it read and the per-unit verdicts of
// local-admission.mjs. A runtime owner reads "qualified" only through its local runtime receipts (validateLocalRuntimeTransfer);
// a proof that is still required (HOLD) never reaches here, and any other unit must read not-applicable.
function validateLocalAdmission(admission, prepared, env) {
  const bound = contexts.get(prepared.context), candidate = prepared.context.candidate;
  exact(admission, ["schema", "kind", "original", "sourceTree", "sourceHashes", "verifierSha", "runId", "runAttempt", "receipts", "units", ...(candidate.ci ? ["ciProof"] : [])], "local admission");
  requireThat(admission.schema === 1 && admission.kind === "local-admission" && same(admission.original, originalBinding(candidate, bound.manifest, bound.manifestSha256)), "local original candidate/manifest/archive mismatch");
  requireThat(/^[0-9a-f]{40}$/.test(admission.sourceTree ?? "") && admission.verifierSha === env.GITHUB_SHA && String(admission.runId) === env.GITHUB_RUN_ID && String(admission.runAttempt) === env.GITHUB_RUN_ATTEMPT, "local admission verifier/tree/run/attempt mismatch");
  requireThat(safeHashes(admission.sourceHashes), "invalid local admission source hashes");
  const hosts = Object.keys(candidate.assets).filter((key) => key !== "packed");
  exact(admission.receipts, hosts, "local admission receipts");
  requireThat(hosts.every((host) => admission.receipts[host] === candidate.assets[host].sha256), "local admission receipts differ from the pinned assets");
  requireThat(Array.isArray(admission.units) && admission.units.length === bound.manifest.units.length, "incomplete local unit verdicts");
  const names = new Set();
  for (const row of admission.units) {
    exact(row, ["name", "version", "archiveSha256", "ok", "failures", "policies", "runtimeFeatureProof", "evidenceSha256"], "local unit verdict");
    exact(row.policies, ["macos", "windows"], "local unit host policies");
    const proof = row.runtimeFeatureProof;
    exact(proof, ["classification", "listedOwner", "required", "status", "failures", ...(proof?.status === "qualified" ? ["proof"] : [])], "local runtime feature proof");
    requireThat(Object.values(row.policies).every((policy) => policy === "required-positive-tests") && Array.isArray(proof.failures) && proof.failures.length === 0, "local runtime feature proof failed");
    if (Object.hasOwn(RUNTIME_PROOF_WORKLOADS, row.name)) validateLocalRuntimeTransfer(row, candidate, bound.receiptsDir);
    else requireThat(proof.status === "not-applicable" && proof.required === false, "local runtime feature proof remains required, held or incoherent");
    const unit = bound.manifest.units.find((item) => item.name === row.name);
    requireThat(unit && !names.has(row.name) && row.version === unit.version && row.archiveSha256 === unit.sha256 && row.ok === true && Array.isArray(row.failures) && row.failures.length === 0 && HEX.test(row.evidenceSha256 ?? ""), "failed/duplicate/mismatched local unit verdict"); names.add(row.name);
  }
  if (candidate.ci) validateLocalCiProof(admission.ciProof, candidate, bound.linuxFile);
  return admission;
}

/** The recorded ci proof: it names the pinned run, derives the same CI-required units from the Linux list, and passes the policy. */
function validateLocalCiProof(proof, candidate, linuxFile) {
  exact(proof, CI_PROOF_KEYS, "ci proof");
  requireThat(String(proof.runId) === String(candidate.ci.runId) && Number.isInteger(proof.runAttempt) && proof.runAttempt > 0, "ci proof is for another run");
  requireThat(CI_CONCLUSIONS.includes(proof.conclusion) && typeof proof.checkedAt === "string" && ISO_TIME.test(proof.checkedAt) && !Number.isNaN(Date.parse(proof.checkedAt)), "ci proof conclusion or time is invalid");
  requireThat(Array.isArray(proof.failedJobs) && proof.failedJobs.every((name) => typeof name === "string" && name.length > 0 && name.length <= 200) && (proof.conclusion === "failure" ? proof.failedJobs.length > 0 : proof.failedJobs.length === 0), "ci proof failed jobs are incoherent with its conclusion");
  const required = ciRequiredUnits({ candidate, linuxFile });
  requireThat(same(proof.requiredUnits, required), "ci proof names other required units than the Linux list requires");
  ciPolicy({ candidate, proof, requiredUnits: required });
}

function validateAdmission(admission, prepared, env) {
  if (prepared.context.candidate.source === "local") return validateLocalAdmission(admission, prepared, env);
  const bound = contexts.get(prepared.context), candidate = prepared.context.candidate;
  exact(admission, ["schema", "kind", "original", "sourceTree", "sourceHashes", "verifierSha", "runId", "runAttempt", "qa", "units"], "semantic admission");
  requireThat(admission.schema === 1 && admission.kind === "semantic-admission" && same(admission.original, originalBinding(candidate, bound.manifest)), "semantic original candidate/manifest/archive mismatch");
  requireThat(/^[0-9a-f]{40}$/.test(admission.sourceTree ?? "") && admission.verifierSha === env.GITHUB_SHA && String(admission.runId) === env.GITHUB_RUN_ID && String(admission.runAttempt) === env.GITHUB_RUN_ATTEMPT, "semantic verifier/tree/run/attempt mismatch");
  requireThat(admission.sourceHashes && typeof admission.sourceHashes === "object" && !Array.isArray(admission.sourceHashes) && Object.keys(admission.sourceHashes).length > 0 && Object.entries(admission.sourceHashes).every(([name, hash]) => name.length <= 1024 && !/[\\\x00-\x1f:]/.test(name) && name.split("/").every((part) => part && part !== "." && part !== "..") && HEX.test(hash)), "invalid semantic source hashes");
  exact(admission.qa, ["macos", "windows"], "semantic QA hosts");
  const ids = new Set(Object.values(candidate.artifacts).map((pin) => String(pin.id)));
  for (const host of ["macos", "windows"]) {
    const row = admission.qa[host]; exact(row, ["id", "digest", "summarySha256", "unitEvidenceSha256"], `${host} semantic QA`);
    requireThat(POS.test(String(row.id)) && !ids.has(String(row.id)) && /^sha256:[a-f0-9]{64}$/.test(row.digest ?? "") && HEX.test(row.summarySha256) && HEX.test(row.unitEvidenceSha256), "invalid semantic QA proof binding"); ids.add(String(row.id));
  }
  requireThat(Array.isArray(admission.units) && admission.units.length === bound.manifest.units.length, "incomplete semantic unit verdicts");
  const names = new Set();
  const runtimePins = candidate.registry === "crates" ? selectSupplements(bound.supplementalFile, candidate, "runtime-workload-pin", bound.approvedFile) : [];
  for (const row of admission.units) {
    exact(row, ["name", "version", "archiveSha256", "ok", "policies", "failures", "adoptionPending", "runtime", "runtimeFeatureProof", "runtimeInputHashes", "evidenceSha256"], "semantic unit verdict");
    exact(row.policies, ["macos", "windows"], "semantic unit host policies");
    if (candidate.registry === "crates") {
      const proof = row.runtimeFeatureProof;
      exact(proof, ["classification", "listedOwner", "required", "status", "failures", ...(proof?.status === "qualified" ? ["proof"] : [])], "semantic runtime feature proof");
      requireThat(Array.isArray(proof.failures) && proof.failures.length === 0, "runtime feature proof failed");
      if (proof.status === "qualified") validateRuntimeTransfer(row, runtimePins, candidate.id);
      else requireThat(["false", "held-resource"].includes(proof.classification) && proof.listedOwner === false && proof.required === false && proof.status === "not-applicable", "runtime feature proof remains required, held or incoherent");
    } else requireThat(row.runtimeFeatureProof === null, "npm runtime feature proof must be null");
    requireThat(Object.values(row.policies).every((policy) => policy === "required-positive-tests") && Array.isArray(row.adoptionPending) && row.adoptionPending.every((reason) => typeof reason === "string" && reason.length > 0) && row.runtimeInputHashes && typeof row.runtimeInputHashes === "object" && !Array.isArray(row.runtimeInputHashes) && Object.entries(row.runtimeInputHashes).every(([name, hash]) => name && !/[\\\x00-\x1f:]/.test(name) && name.split("/").every((part) => part && part !== "." && part !== "..") && HEX.test(hash)) && (candidate.registry === "crates" ? row.runtime?.status === "ok" : row.runtime === null), "incomplete semantic native/runtime verdict");
    const unit = bound.manifest.units.find((item) => item.name === row?.name);
    requireThat(unit && !names.has(row.name) && row.version === unit.version && row.archiveSha256 === unit.sha256 && row.ok === true && Array.isArray(row.failures) && row.failures.length === 0 && HEX.test(row.evidenceSha256 ?? ""), "failed/duplicate/mismatched semantic unit verdict"); names.add(row.name);
  }
  // Every committed runtime pin of this candidate is transferred by exactly one qualified unit, and the reverse.
  const transferred = admission.units.filter((row) => row.runtimeFeatureProof?.status === "qualified").map((row) => row.runtimeFeatureProof.proof.proofSha256).sort();
  requireThat(same(transferred, runtimePins.map((pin) => pin.artifact.proofSha256).sort()), "runtime supplement pins differ from qualified units");
  return admission;
}

// Public authentication API has no acceptance handle: only an authenticated artifact
// from this run's successful secretless job can advance the opaque upload context.
export async function authenticateAdmission({ prepared, env, fetchImpl = fetch, spawn = spawnSync }) {
  const bound = contexts.get(prepared.context);
  requireThat(bound && bound.packDir === prepared.packDir && bound.receiptsDir === prepared.receiptsDir && bound.env.GITHUB_SHA === env.GITHUB_SHA && bound.env.GITHUB_RUN_ID === env.GITHUB_RUN_ID && bound.env.GITHUB_RUN_ATTEMPT === env.GITHUB_RUN_ATTEMPT, "admission context identity changed");
  requireThat(POS.test(env.PUBLISH_ADMISSION_ID ?? "") && /^(?:sha256:)?[a-f0-9]{64}$/.test(env.PUBLISH_ADMISSION_DIGEST ?? "") && POS.test(env.GITHUB_RUN_ID ?? "") && POS.test(env.GITHUB_RUN_ATTEMPT ?? ""), "invalid admission artifact ID/digest or promotion run");
  const admissionDigest = `sha256:${env.PUBLISH_ADMISSION_DIGEST.replace(/^sha256:/, "")}`;
  const workflow = await api("actions/workflows/publish-reviewed.yml", env, fetchImpl);
  const run = await api(`actions/runs/${env.GITHUB_RUN_ID}/attempts/${env.GITHUB_RUN_ATTEMPT}`, env, fetchImpl);
  const provenanceChecks = {
    workflowId: POS.test(String(workflow.id)), workflowPath: workflow.path === ".github/workflows/publish-reviewed.yml", workflowState: workflow.state === "active",
    runId: String(run.id) === env.GITHUB_RUN_ID, runAttempt: String(run.run_attempt) === env.GITHUB_RUN_ATTEMPT,
    repositoryId: run.repository?.id === bound.repository.id, repositoryName: run.repository?.full_name === REPOSITORY,
    headRepositoryId: run.head_repository?.id === bound.repository.id, headRepositoryName: run.head_repository?.full_name === REPOSITORY,
    runWorkflowId: String(run.workflow_id) === String(workflow.id), runPath: run.path === workflow.path,
    runEvent: run.event === "workflow_dispatch", runBranch: run.head_branch === "main", runSha: run.head_sha === env.GITHUB_SHA,
    runStatus: ["in_progress", "completed"].includes(run.status),
  };
  requireThat(Object.values(provenanceChecks).every(Boolean), `promotion run/workflow provenance mismatch: ${Object.entries(provenanceChecks).filter(([, ok]) => !ok).map(([name]) => name).join(",")}`);
  const jobs = await api(`actions/runs/${env.GITHUB_RUN_ID}/attempts/${env.GITHUB_RUN_ATTEMPT}/jobs?per_page=100`, env, fetchImpl);
  requireThat(Array.isArray(jobs.jobs) && jobs.jobs.length === jobs.total_count && jobs.jobs.every((job) => String(job.run_id) === env.GITHUB_RUN_ID && String(job.run_attempt) === env.GITHUB_RUN_ATTEMPT && job.head_sha === env.GITHUB_SHA), "promotion job provenance mismatch");
  const seal = jobs.jobs.filter((job) => job.name === "seal-check");
  requireThat(seal.length === 1 && seal[0].status === "completed" && seal[0].conclusion === "success", "seal-check did not succeed in current attempt");
  const artifact = await api(`actions/artifacts/${env.PUBLISH_ADMISSION_ID}`, env, fetchImpl);
  requireThat(artifactMatches(artifact, { id: env.PUBLISH_ADMISSION_ID, digest: admissionDigest, name: "reviewed-admission-receipt", runId: env.GITHUB_RUN_ID, sourceSha: env.GITHUB_SHA, repository: bound.repository }), "admission artifact provenance mismatch");
  const bytes = await api(`actions/artifacts/${artifact.id}/zip`, env, fetchImpl, true);
  requireThat(`sha256:${sha("sha256", bytes)}` === admissionDigest, "admission downloaded ZIP digest mismatch");
  const { proofs, names } = proofZip(bytes, path.join(path.dirname(prepared.packDir), "admission.zip"), ["admission.json"], spawn);
  requireThat(names.length === 1 && names[0] === "admission.json", "admission ZIP must contain exactly admission.json");
  const admission = validateAdmission(proofs["admission.json"].value, prepared, env);
  bound.admitted = true;
  bound.ciProof = admission.ciProof === undefined ? null : structuredClone(admission.ciProof);
  return admission;
}

// Secretless local admission. Source, archive and receipts are read as DATA; nothing the candidate ships is executed.
async function localDryRun({ prepared, env, spawn, fetchImpl }) {
  const bound = contexts.get(prepared.context), candidate = prepared.context.candidate;
  const payloadProblems = checkUploadPayloads({ manifest: bound.manifest, packDir: prepared.packDir, registry: candidate.registry, spawn });
  requireThat(payloadProblems.length === 0, payloadProblems.join("; "));
  const { loadCandidateSource } = await import("./candidate-source.mjs");
  const source = loadCandidateSource({ directory: env.PUBLISH_SOURCE_DIR, sourceSha: candidate.sourceSha, promotionSha: env.GITHUB_SHA, registry: candidate.registry, units: candidate.units, allow: localAllow(candidate), spawn });
  const hosts = Object.keys(candidate.assets).filter((key) => key !== "packed");
  const receipts = Object.fromEntries(hosts.map((host) => { const directory = path.join(prepared.receiptsDir, host); return [host, JSON.parse(fs.readFileSync(path.join(directory, fs.readdirSync(directory)[0]), "utf8"))]; }));
  const { localAdmission } = await import("./local-admission.mjs");
  const result = await localAdmission({ candidate, manifest: bound.manifest, manifestSha256: bound.manifestSha256, source, receipts, packDir: prepared.packDir });
  requireThat(result.ok === true, `local admission failed: ${JSON.stringify({ failures: result.failures, units: result.units.filter((unit) => unit.ok !== true) })}`);
  // The CI run that rides with the candidate is read here, once, and its verdict recorded in the hashed admission; the policy
  // (required units need success, failure needs a reviewer's allowance) refuses before anything reaches the credentialed job.
  const ciProof = candidate.ci ? await ciProofFor({ candidate, repository: bound.repository, env, fetchImpl, linuxFile: bound.linuxFile }) : undefined;
  const admission = { schema: 1, kind: "local-admission", original: originalBinding(candidate, bound.manifest, bound.manifestSha256), sourceTree: source.source.tree, sourceHashes: source.sourceHashes, verifierSha: env.GITHUB_SHA, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, receipts: Object.fromEntries(hosts.map((host) => [host, candidate.assets[host].sha256])), units: result.units, ...(ciProof ? { ciProof } : {}) };
  validateAdmission(admission, prepared, env);
  const destination = path.join(env.RUNNER_TEMP, "publication-admission");
  fs.mkdirSync(destination, { recursive: true });
  requireThat(fs.readdirSync(destination).length === 0, "admission destination must be empty");
  fs.writeFileSync(path.join(destination, "admission.json"), `${JSON.stringify(admission, null, 2)}\n`, { flag: "wx" });
  return admission;
}

export async function runReviewed({ env, dryRun = false, verifyAdmission = false, fetchImpl = fetch, spawn = spawnSync, approvedFile = APPROVED_ARTIFACTS_FILE, supplementalFile = SUPPLEMENTAL_PROOFS_FILE }) {
  requireThat(!(dryRun && verifyAdmission), "conflicting run modes");
  if (dryRun || verifyAdmission) for (const key of ["NODE_AUTH_TOKEN", "NPM_TOKEN", "CARGO_REGISTRY_TOKEN"]) requireThat(!env[key] && !process.env[key], `${key} is present in secretless admission`);
  if (dryRun) for (const key of ["ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN"]) requireThat(!env[key] && !process.env[key], `${key} is present in secretless admission`);
  const prepared = await prepareCandidate({ env, candidateId: env.PUBLISH_CANDIDATE_ID, approvedFile, supplementalFile, fetchImpl, spawn });
  const candidate = prepared.context.candidate, bound = contexts.get(prepared.context);
  if (dryRun && candidate.source === "local") return localDryRun({ prepared, env, spawn, fetchImpl });
  if (dryRun) {
    const payloadProblems = checkUploadPayloads({ manifest: bound.manifest, packDir: prepared.packDir, registry: candidate.registry, spawn });
    requireThat(payloadProblems.length === 0, payloadProblems.join("; "));
    const { loadCandidateSource } = await import("./candidate-source.mjs");
    const { qa, bindings } = await originalQa({ prepared, env, fetchImpl, spawn });
    const source = loadCandidateSource({ directory: env.PUBLISH_SOURCE_DIR, sourceSha: candidate.sourceSha, promotionSha: env.GITHUB_SHA, registry: candidate.registry, units: candidate.units, allow: candidate.allow, spawn });
    const { semanticAdmission } = await import("./semantic-admission.mjs");
    const receipts = Object.fromEntries(["macos", "windows"].map((host) => { const directory = path.join(prepared.receiptsDir, host); return [host, JSON.parse(fs.readFileSync(path.join(directory, fs.readdirSync(directory)[0]), "utf8"))]; }));
    const runtimeProofs = [];
    for (const runtimePin of selectSupplements(supplementalFile, candidate, "runtime-workload-pin", approvedFile)) {
      const { verifyRuntimeSupplement } = await import("./runtime-supplement.mjs");
      runtimeProofs.push(await verifyRuntimeSupplement({ candidate, pin: runtimePin, manifest: bound.manifest, manifestSha256: candidate.manifestSha256, source, packDir: prepared.packDir, env, fetchImpl, spawn }));
    }
    const result = await semanticAdmission({ candidate, manifest: bound.manifest, source, receipts, qa, packDir: prepared.packDir, env, spawn, runtimeProofs });
    requireThat(result.ok === true, `semantic admission failed: ${JSON.stringify({ failures: result.failures ?? [], hosts: result.hostVerdicts ?? {}, units: (result.units ?? []).filter((unit) => unit.ok !== true) })}`);
    const admission = { schema: 1, kind: "semantic-admission", original: originalBinding(candidate, bound.manifest), sourceTree: source.source.tree, sourceHashes: source.sourceHashes, verifierSha: env.GITHUB_SHA, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, qa: bindings, units: result.units };
    validateAdmission(admission, prepared, env);
    const destination = path.join(env.RUNNER_TEMP, "publication-admission");
    fs.mkdirSync(destination, { recursive: true });
    requireThat(fs.readdirSync(destination).length === 0, "admission destination must be empty");
    fs.writeFileSync(path.join(destination, "admission.json"), `${JSON.stringify(admission, null, 2)}\n`, { flag: "wx" });
    return admission;
  }
  const admission = await authenticateAdmission({ prepared, env, fetchImpl, spawn });
  const uploadEnv = { ...env, PUBLISH_REGISTRY: candidate.registry, PUBLISH_UNITS: candidate.units.join(" "), PUBLISH_ALLOW_HOST_EXCLUSION: candidate.allow.hostExclusion.join(" "), PUBLISH_ALLOW_NO_TEST: candidate.allow.noTest.join(" "), PUBLISH_ALLOW_IGNORED: candidate.allow.ignored.join(" "), PUBLISH_ALLOW_MISSING_ASSETS: candidate.allow.missingAssets.join(" ") };
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, `registry=${candidate.registry}\n`);
  if (verifyAdmission) {
    assertAuthenticatedContext(prepared.context, { env: uploadEnv, ...prepared, approvedFile });
    const directory = path.join(env.RUNNER_TEMP, "publication-admission-transfer"); fs.mkdirSync(directory, { recursive: true });
    const receipt = { schema: 1, kind: "semantic-admission-transfer", upload: false, candidateId: candidate.id, candidateSha256: admission.original.candidateSha256, sourceSha: candidate.sourceSha, verifierSha: env.GITHUB_SHA, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, admissionArtifact: { id: env.PUBLISH_ADMISSION_ID, digest: `sha256:${env.PUBLISH_ADMISSION_DIGEST.replace(/^sha256:/, "")}` } };
    fs.writeFileSync(path.join(directory, "admission-transfer.json"), `${JSON.stringify(receipt, null, 2)}\n`, { flag: "wx" });
    return admission;
  }
  return executeUpload({ env: uploadEnv, ...prepared, candidateContext: prepared.context, approvedFile, outDir: path.join(env.RUNNER_TEMP, "publication-receipt"), dryRun: false, fetchImpl, spawn });
}

// ---- direct publication of a green qualification run --------------------------------------------
// Adrian, 2026-10-22: "As soon as the qualification passes on GitHub, GitHub can directly push it to npm and crates."
// publish-direct.yml runs on every completed publish.yml run of main. Its only input is that run (id, attempt, head
// commit from the workflow_run event); registry, units and allowsets are read from the run's own sealed manifest and
// verify receipts, never from a dispatch input, a committed candidate or ci/approved-artifacts.json. Provenance: the run
// is a workflow_dispatch of publish.yml on main of this repository, completed and successful, with guard, plan, pack,
// qualified, both native verify jobs and verify-linux (exactly when the plan requires Linux) successful in the named
// attempt, and this workflow's checkout is that commit or a descendant of it on main. The packed artifact and every
// required receipt are taken from the run's own artifact listing by name, bound to the run and commit, and re-hashed
// after download. The approval entries the uploader requires are written from the sealed manifest itself: the
// qualification run is the approver, and the publication receipt records its run URL as the review reference.
export const QUALIFIED_WORKFLOW = "publish-direct.yml";
export const QUALIFIED_ARTIFACTS = { packed: "publish-packed", macos: "publish-receipt-verify-xcode-27", windows: "publish-receipt-verify-windows-2025", linux: "publish-receipt-verify-ubuntu-24.04" };
const UNIT_NAME = /^(@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*$/;

export async function prepareQualified({ env, fetchImpl = fetch, spawn = spawnSync, linuxFile }) {
  requireThat(env.GITHUB_ACTIONS === "true" && env.GITHUB_REPOSITORY === REPOSITORY && env.GITHUB_REF === "refs/heads/main" && /^[0-9a-f]{40}$/.test(env.GITHUB_SHA ?? ""), "direct publication requires trusted main of the repository");
  requireThat(env.GITHUB_TOKEN && env.RUNNER_TEMP, "GitHub read token & runner temp are required");
  requireThat(env.GITHUB_WORKFLOW_REF === `${REPOSITORY}/.github/workflows/${QUALIFIED_WORKFLOW}@refs/heads/main`, "wrong direct publication workflow");
  const runId = env.PUBLISH_QUALIFIED_RUN_ID ?? "", runAttempt = env.PUBLISH_QUALIFIED_RUN_ATTEMPT ?? "", sourceSha = env.PUBLISH_QUALIFIED_SHA ?? "";
  requireThat(POS.test(runId) && POS.test(runAttempt) && /^[0-9a-f]{40}$/.test(sourceSha), "invalid qualified run id, attempt or commit");
  const repository = await api("", env, fetchImpl);
  const workflow = await api("actions/workflows/publish.yml", env, fetchImpl);
  const run = await api(`actions/runs/${runId}/attempts/${runAttempt}`, env, fetchImpl);
  const jobs = await api(`actions/runs/${runId}/attempts/${runAttempt}/jobs?per_page=100`, env, fetchImpl);
  const comparison = await api(`compare/${sourceSha}...${env.GITHUB_SHA}`, env, fetchImpl);
  requireThat(repository.full_name === REPOSITORY && repository.default_branch === "main", "wrong repository/default branch");
  requireThat(POS.test(String(workflow.id)) && workflow.path === ".github/workflows/publish.yml" && workflow.state === "active", "wrong qualification workflow");
  requireThat(String(run.id) === runId && String(run.run_attempt) === runAttempt && String(run.workflow_id) === String(workflow.id) && run.path === workflow.path, "wrong run/attempt/workflow");
  requireThat(run.repository?.id === repository.id && run.repository?.full_name === REPOSITORY && run.head_repository?.id === repository.id && run.head_repository?.full_name === REPOSITORY, "foreign source/head repository");
  requireThat(run.event === "workflow_dispatch" && run.head_branch === "main" && run.head_sha === sourceSha && run.status === "completed" && run.conclusion === "success", "qualification event/ref/SHA/conclusion mismatch");
  requireThat((env.GITHUB_SHA === sourceSha && comparison.status === "identical") || (env.GITHUB_SHA !== sourceSha && comparison.status === "ahead" && comparison.merge_base_commit?.sha === sourceSha), "direct publication must run at the qualified commit or a descendant of it on main");
  const rows = jobs.jobs;
  requireThat(Array.isArray(rows) && rows.length === jobs.total_count && rows.every((job) => String(job.run_id) === runId && String(job.run_attempt) === runAttempt && job.head_sha === sourceSha), "incomplete or foreign job provenance");
  for (const name of ["guard", "plan", "pack", "qualified"]) {
    const found = rows.filter((job) => job.name === name);
    requireThat(found.length === 1 && found[0].status === "completed" && found[0].conclusion === "success", `${name} did not succeed in attempt ${runAttempt}`);
  }
  const verify = rows.filter((job) => /^verify \(/.test(job.name));
  requireThat(verify.length === 2 && verify.every((job) => job.conclusion === "success") && verify.some((job) => /^verify \(xcode-27, /.test(job.name)) && verify.some((job) => /^verify \(windows-2025, /.test(job.name)), "both native verify jobs must succeed");
  const listing = []; let expected;
  for (let page = 1; page <= 10; page++) {
    const pageDoc = await api(`actions/runs/${runId}/artifacts?per_page=100&page=${page}`, env, fetchImpl);
    requireThat(Number.isSafeInteger(pageDoc.total_count) && pageDoc.total_count >= 0 && pageDoc.total_count <= 1000 && Array.isArray(pageDoc.artifacts) && pageDoc.artifacts.length <= 100, "invalid artifact listing");
    expected ??= pageDoc.total_count; requireThat(pageDoc.total_count === expected, "artifact listing changed");
    listing.push(...pageDoc.artifacts); if (listing.length >= expected) break;
  }
  requireThat(listing.length === expected, "artifact listing is incomplete");
  const root = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "qualified-run-"));
  const packDir = path.join(root, "packed"); const receiptsDir = path.join(root, "receipts");
  const artifacts = {};
  const take = async (host) => {
    const name = QUALIFIED_ARTIFACTS[host];
    const found = listing.filter((artifact) => artifact?.name === name);
    requireThat(found.length === 1 && artifactMatches(found[0], { name, runId, sourceSha, repository }), `${host} artifact must exist exactly once in run ${runId} at ${sourceSha}`);
    const bytes = await api(`actions/artifacts/${found[0].id}/zip`, env, fetchImpl, true);
    const digest = `sha256:${sha("sha256", bytes)}`;
    requireThat(digest === found[0].digest, `${host} downloaded ZIP digest mismatch`);
    const destination = host === "packed" ? packDir : path.join(receiptsDir, host);
    const names = unpackArtifact({ bytes, zipFile: path.join(root, `${host}.zip`), destination, spawn });
    artifacts[host] = { id: String(found[0].id), digest };
    if (host !== "packed") {
      requireThat(names.length === 1 && names[0].endsWith(".json"), `${host} receipt artifact is not exactly one JSON file`);
      artifacts[host].receiptSha256 = sha("sha256", fs.readFileSync(path.join(destination, names[0])));
    }
  };
  await take("packed");
  // Registry and units are the sealed manifest's; loadSealed then re-checks every sealed byte against that manifest.
  let plan = null;
  try { plan = JSON.parse(fs.readFileSync(path.join(packDir, "manifest.json"), "utf8")); } catch { plan = null; }
  requireThat(plan && ["npm", "crates"].includes(plan.registry) && Array.isArray(plan.units) && plan.units.length > 0 && plan.units.every((unit) => typeof unit?.name === "string" && UNIT_NAME.test(unit.name)), "sealed manifest names no registry or units");
  const registry = plan.registry, units = plan.units.map((unit) => unit.name);
  // publish.yml carries no runtime workload, so its success is no runtime proof: a runtime-proof owner stays on the
  // reviewed route (local qualification with its runtime workload, then publish-reviewed), exactly as semantic admission demands.
  const runtimeOwned = units.filter((name) => Object.hasOwn(RUNTIME_PROOF_WORKLOADS, name));
  requireThat(runtimeOwned.length === 0, `${runtimeOwned.join(", ")}: a runtime-proof owner is not published by a qualification run; qualify it locally with its runtime workload and promote it through publish-reviewed`);
  const linuxRequired = linuxRequiredFor({ registry, units, ...(linuxFile ? { file: linuxFile } : {}) });
  requireThat(linuxRequired.problems.length === 0, linuxRequired.problems.join("; "));
  const linuxJobs = rows.filter((job) => job.name === "verify-linux");
  requireThat(linuxRequired.units.length > 0 ? linuxJobs.length === 1 && linuxJobs[0].conclusion === "success" : linuxJobs.every((job) => job.conclusion === "skipped"), "the Linux verify job must succeed exactly when the plan requires Linux");
  for (const host of ["macos", "windows", ...(linuxRequired.units.length > 0 ? ["linux"] : [])]) await take(host);
  const sealed = loadSealed({ packDir, registry, unitsText: units.join(" "), run: { runId, runAttempt } });
  requireThat(sealed.manifest && sealed.manifest.commit === sourceSha, "sealed manifest is not bound to the qualified commit");
  const macos = path.join(receiptsDir, "macos");
  const receipt = JSON.parse(fs.readFileSync(path.join(macos, fs.readdirSync(macos)[0]), "utf8"));
  const names = (list) => (Array.isArray(list) && list.every((item) => typeof item === "string" && item !== "" && !/\s/.test(item)) ? [...list] : null);
  const allow = { hostExclusion: names(receipt.allowHostExclusion), noTest: names(receipt.allowNoTest), ignored: names(receipt.allowIgnored), missingAssets: names(receipt.allowMissingAssets) };
  requireThat(Object.values(allow).every(Boolean), "the macOS receipt carries malformed allowsets");
  const problems = [...sealed.problems, ...checkReceipts({ manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, receiptsDir, allow, run: { runId, runAttempt }, linuxRequired: linuxRequired.units }), ...checkUploadPayloads({ manifest: sealed.manifest, packDir, registry, spawn })];
  requireThat(problems.length === 0, problems.join("; "));
  const approvedFile = path.join(root, "qualified-approval.json");
  const reviewRef = `https://github.com/${REPOSITORY}/actions/runs/${runId}/attempts/${runAttempt}`;
  fs.writeFileSync(approvedFile, `${JSON.stringify({ schema: 1, artifacts: sealed.manifest.units.map((unit) => ({ registry, name: unit.name, version: unit.version, sha256: unit.sha256, reviewer: "publish.yml qualified", reviewRef })) }, null, 2)}\n`, { flag: "wx" });
  const candidate = { id: `qualified-${runId}-${runAttempt}`, source: "qualified", sourceSha, runId, runAttempt, workflowId: String(workflow.id), manifestSha256: sealed.manifestSha256, registry, units, allow, artifacts };
  const context = JSON.parse(JSON.stringify({ candidate })); freezeDeep(context);
  contexts.set(context, { env: { ...env }, packDir, receiptsDir, approvedFile, manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, repository, admitted: true, ciProof: null });
  return { context, packDir, receiptsDir, approvedFile };
}

export async function runQualified({ env, identify = false, fetchImpl = fetch, spawn = spawnSync, linuxFile, pause, log }) {
  if (identify) for (const key of ["NODE_AUTH_TOKEN", "NPM_TOKEN", "CARGO_REGISTRY_TOKEN"]) requireThat(!env[key] && !process.env[key], `${key} is present in secretless identification`);
  const prepared = await prepareQualified({ env, fetchImpl, spawn, linuxFile });
  const { candidate } = prepared.context;
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, `registry=${candidate.registry}\nunits=${candidate.units.join(" ")}\nsource_sha=${candidate.sourceSha}\n`);
  if (identify) return { registry: candidate.registry, units: candidate.units, sourceSha: candidate.sourceSha };
  requireThat(env.PUBLISH_REGISTRY === candidate.registry, `this credentialed step publishes to ${env.PUBLISH_REGISTRY}; the qualified run sealed ${candidate.registry}`);
  const uploadEnv = { ...env, PUBLISH_REGISTRY: candidate.registry, PUBLISH_UNITS: candidate.units.join(" "), PUBLISH_ALLOW_HOST_EXCLUSION: candidate.allow.hostExclusion.join(" "), PUBLISH_ALLOW_NO_TEST: candidate.allow.noTest.join(" "), PUBLISH_ALLOW_IGNORED: candidate.allow.ignored.join(" "), PUBLISH_ALLOW_MISSING_ASSETS: candidate.allow.missingAssets.join(" ") };
  return executeUpload({ env: uploadEnv, packDir: prepared.packDir, receiptsDir: prepared.receiptsDir, candidateContext: prepared.context, approvedFile: prepared.approvedFile, outDir: path.join(env.RUNNER_TEMP, "publication-receipt"), dryRun: false, fetchImpl, spawn, ...(pause === undefined ? {} : { pause }), ...(log === undefined ? {} : { log }) });
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  // Finish module evaluation before lazy admission imports reach this module
  // again through runtime-supplement -> supplemental-proof. Top-level await here
  // makes that backedge wait on its own pending entry evaluation.
  setImmediate(async () => {
    const unfinished = () => {
      process.stderr.write("reviewed candidate refused: CLI execution did not settle\n");
      process.exitCode = 1;
    };
    // An orphaned pending promise must never turn into a successful CLI exit.
    process.once("beforeExit", unfinished);
    const [command, ...flags] = process.argv.slice(2);
    try {
      requireThat((command === "identify" && flags.length === 0) || (command === "run" && flags.length <= 1 && flags.every((flag) => ["--dry-run", "--verify-admission"].includes(flag))) || (command === "run-qualified" && flags.length <= 1 && flags.every((flag) => flag === "--identify")), "usage: reviewed-candidate.mjs identify | run [--dry-run|--verify-admission] | run-qualified [--identify]");
      if (command === "identify") await identifyCandidate({ env: process.env, candidateId: process.env.PUBLISH_CANDIDATE_ID });
      else if (command === "run-qualified") await runQualified({ env: process.env, identify: flags.includes("--identify") });
      else await runReviewed({ env: process.env, dryRun: flags.includes("--dry-run"), verifyAdmission: flags.includes("--verify-admission") });
    } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
    finally { process.removeListener("beforeExit", unfinished); }
  });
}
