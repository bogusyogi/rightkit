#!/usr/bin/env node
// Supplemental archive proof never replaces original native receipts or archive bytes.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { APPROVED_ARTIFACTS_FILE, REPOSITORY, sha, loadSealed, archiveScanProblems } from "./upload.mjs";
import { validateCandidate, selectCandidate, checkProvenance, unpackArtifact } from "./reviewed-candidate.mjs";

export const SUPPLEMENT_WORKFLOW = ".github/workflows/supplement-archive-scan.yml";
export const SUPPLEMENT_POLICY = "archive-secret-scan/v1";
const SHA = /^[0-9a-f]{40}$/;
const HASH = /^[0-9a-f]{64}$/;
const id = (value) => typeof value === "string" && /^[1-9][0-9]*$/.test(value) && Number.isSafeInteger(Number(value));
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const need = (ok, why) => { if (!ok) throw new Error(`archive supplement refused: ${why}`); };
function exact(value, keys, label) {
  need(value && typeof value === "object" && !Array.isArray(value) && same(Object.keys(value).sort(), [...keys].sort()), `${label} keys mismatch`);
}
export const candidateHash = (candidate) => sha("sha256", JSON.stringify(validateCandidate(candidate)));
const artifactName = (candidateId, attempt) => `archive-scan-supplement-${candidateId}-${attempt}`;
const proofName = "archive-scan-supplement.json";

function policy(value, sourceSha) {
  exact(value, ["id", "sourceSha", "validatorSourceSha"], "policy");
  need(value.id === SUPPLEMENT_POLICY && value.sourceSha === sourceSha && value.validatorSourceSha === sourceSha && SHA.test(sourceSha), "policy/validator revision mismatch");
}
function original(candidate) {
  return { candidateId: candidate.id, candidateSha256: candidateHash(candidate), sourceSha: candidate.sourceSha, manifestSha256: candidate.manifestSha256, packedArtifact: candidate.artifacts.packed };
}
export function checkOriginal(value, candidate) {
  exact(value, ["candidateId", "candidateSha256", "sourceSha", "manifestSha256", "packedArtifact"], "original binding");
  exact(value.packedArtifact, ["id", "digest"], "original packed artifact");
  const expected = original(candidate);
  for (const key of ["candidateId", "candidateSha256", "sourceSha", "manifestSha256"]) need(value[key] === expected[key], `original ${key} mismatch`);
  need(String(value.packedArtifact.id) === String(expected.packedArtifact.id) && value.packedArtifact.digest === expected.packedArtifact.digest, "original archive artifact mismatch");
}

/** Separate committed pin; do not add fields to original schema-2 candidate records. */
export function validateSupplementPin(pin, candidate) {
  validateCandidate(candidate);
  exact(pin, ["schema", "kind", "original", "producer", "artifact", "policy"], "supplement pin");
  need(pin.schema === 1 && ["archive-scan-supplement-pin", "runtime-workload-pin"].includes(pin.kind), "unknown supplement pin version/kind");
  checkOriginal(pin.original, candidate);
  const runtime = pin.kind === "runtime-workload-pin";
  exact(pin.producer, ["workflowId", "runId", "runAttempt", "jobId", "sourceSha", ...(runtime ? ["jobs"] : [])], "producer pin");
  if (runtime) {
    exact(pin.producer.jobs, ["candidate", "win32", "darwin", "qualify"], "runtime jobs");
    need(Object.values(pin.producer.jobs).every(id) && new Set(Object.values(pin.producer.jobs)).size === 4 && pin.producer.jobs.qualify === pin.producer.jobId, "invalid runtime job pins");
  }
  for (const key of ["workflowId", "runId", "runAttempt", "jobId"]) need(id(pin.producer[key]), `invalid producer ${key}`);
  need(SHA.test(pin.producer.sourceSha), "invalid producer source revision");
  if (runtime) { exact(pin.policy, ["id", "sourceSha", "validatorSourceSha"], "runtime policy"); need(pin.policy.id === "runtime-workloads/v1" && pin.policy.sourceSha === pin.producer.sourceSha && pin.policy.validatorSourceSha === pin.producer.sourceSha, "runtime policy mismatch"); }
  else policy(pin.policy, pin.producer.sourceSha);
  exact(pin.artifact, ["id", "digest", "proofSha256"], "supplement artifact pin");
  need(id(pin.artifact.id) && /^sha256:[0-9a-f]{64}$/.test(pin.artifact.digest) && HASH.test(pin.artifact.proofSha256), "invalid supplement artifact identity/hash");
  need(!Object.values(candidate.artifacts).some((a) => String(a.id) === pin.artifact.id), "supplement cannot alias original artifact");
  return pin;
}

function context(env, producer = false) {
  need(env.GITHUB_ACTIONS === "true" && env.GITHUB_REPOSITORY === REPOSITORY && env.GITHUB_REF === "refs/heads/main" && SHA.test(env.GITHUB_SHA ?? ""), "trusted main Actions context required");
  const workflows = producer ? [SUPPLEMENT_WORKFLOW] : [".github/workflows/publish-reviewed.yml"];
  need(workflows.some((file) => env.GITHUB_WORKFLOW_REF === `${REPOSITORY}/${file}@refs/heads/main`), "wrong trusted harness workflow");
  need(env.GITHUB_TOKEN && env.RUNNER_TEMP, "GitHub read token & runner temp required");
  if (producer) {
    need(!env.NODE_AUTH_TOKEN && !env.CARGO_REGISTRY_TOKEN && !env.ACTIONS_ID_TOKEN_REQUEST_TOKEN, "producer must hold no registry/signing credentials");
    need(id(env.GITHUB_RUN_ID) && id(env.GITHUB_RUN_ATTEMPT), "producer run/attempt required");
  }
}

// Same fixed-repository API & uncredentialed signed-storage redirect boundary as candidate-pins.
function evidenceApi(env, fetchImpl) {
  return async (route, binary = false) => {
    let response;
    try {
      response = await fetchImpl(`https://api.github.com/repos/${REPOSITORY}${route ? `/${route}` : ""}`, { method: "GET", headers: { authorization: `Bearer ${env.GITHUB_TOKEN}`, accept: "application/vnd.github+json", "x-github-api-version": "2022-11-28" }, redirect: "manual", signal: AbortSignal.timeout(60_000) });
      if (binary && response.status === 302) {
        const url = new URL(response.headers.get("location"));
        need(url.protocol === "https:" && !url.username && !url.password && !url.port && (url.hostname.endsWith(".blob.core.windows.net") || url.hostname.endsWith(".actions.githubusercontent.com")), "unsafe artifact redirect");
        response = await fetchImpl(url, { method: "GET", redirect: "error", signal: AbortSignal.timeout(120_000) });
      }
      need(response?.ok, "GitHub evidence unavailable");
      return binary ? Buffer.from(await response.arrayBuffer()) : await response.json();
    } catch { throw new Error("archive supplement refused: GitHub evidence unavailable or unsafe"); }
  };
}
function artifactBinding(artifact, pin, { name, runId, sourceSha, repository }) {
  need(String(artifact?.id) === String(pin.id) && artifact.name === name && artifact.expired === false && artifact.digest === pin.digest, "artifact identity/digest/retention mismatch");
  const run = artifact.workflow_run;
  need(String(run?.id) === String(runId) && run.head_sha === sourceSha && run.head_branch === "main" && run.repository_id === repository.id && run.head_repository_id === repository.id, "foreign artifact provenance");
}
async function download(api, pin, destination, name, binding, spawn, nested = false) {
  const artifact = await api(`actions/artifacts/${pin.id}`);
  artifactBinding(artifact, pin, { ...binding, name });
  const bytes = await api(`actions/artifacts/${pin.id}/zip`, true);
  need(`sha256:${sha("sha256", bytes)}` === pin.digest, "downloaded artifact digest mismatch");
  let entries;
  if (!nested) entries = unpackArtifact({ bytes, zipFile: `${destination}.zip`, destination, spawn });
  else {
    need(bytes.length <= 64 * 1024 * 1024, "runtime artifact exceeds bound");
    const zip = `${destination}.zip`;
    fs.writeFileSync(zip, bytes);
    const invoke = (args) => {
      const result = spawn("unzip", args, { encoding: "buffer", windowsHide: true, shell: false, timeout: 30000, maxBuffer: 16 * 1024 * 1024 });
      need(!result.error && !result.signal && result.status === 0, "runtime artifact extraction failed");
      return Buffer.from(result.stdout ?? "");
    };
    entries = invoke(["-Z1", zip]).toString("utf8").trim().split(/\r?\n/);
    need(entries.length > 0 && entries.length < 1000 && new Set(entries).size === entries.length && entries.every((entry) => /^[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*\/?$/.test(entry) && entry.replace(/\/$/, "").split("/").every((part) => part !== "." && part !== "..")), "unsafe runtime artifact paths");
    let total = 0;
    for (const entry of entries) {
      if (entry.endsWith("/")) continue;
      // Materialize bytes with exclusive writes, never archived modes or links.
      const content = invoke(["-p", zip, entry]);
      total += content.length;
      need(total <= 64 * 1024 * 1024, "expanded runtime artifact exceeds bound");
      const file = path.join(destination, ...entry.split("/"));
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, content, { flag: "wx", mode: 0o600 });
    }
  }
  return { entries, artifact };
}
async function originalArchive({ candidate, env, api, root, spawn }) {
  const repository = await api("");
  const workflow = await api("actions/workflows/publish.yml");
  const run = await api(`actions/runs/${candidate.runId}/attempts/${candidate.runAttempt}`);
  const jobs = await api(`actions/runs/${candidate.runId}/attempts/${candidate.runAttempt}/jobs?per_page=100`);
  const comparison = await api(`compare/${candidate.sourceSha}...${env.GITHUB_SHA}`);
  checkProvenance({ candidate, repository, workflow, run, jobs, promotionSha: env.GITHUB_SHA, comparison });
  need(Array.isArray(jobs.jobs) && jobs.jobs.every((job) => Number(job.run_attempt) === Number(candidate.runAttempt) && job.status === "completed"), "original job attempt/status mismatch");
  const packDir = path.join(root, "packed");
  await download(api, candidate.artifacts.packed, packDir, "publish-packed", { runId: candidate.runId, sourceSha: candidate.sourceSha, repository }, spawn);
  const sealed = loadSealed({ packDir, registry: candidate.registry, unitsText: candidate.units.join(" "), run: { runId: candidate.runId, runAttempt: candidate.runAttempt } });
  need(sealed.problems.length === 0 && sealed.manifest?.commit === candidate.sourceSha && sealed.manifestSha256 === candidate.manifestSha256 && String(sealed.manifest.runAttempt) === String(candidate.runAttempt), "original sealed bytes/manifest/source mismatch");
  return { repository, packDir, ...sealed };
}

/** Producer downloads retained original packed artifact only; it never rebuilds/repackages or runs it. */
export async function produceArchiveSupplement({ env = process.env, candidateId, approvedFile = APPROVED_ARTIFACTS_FILE, fetchImpl = fetch, spawn = spawnSync, scanArchive = async (options) => (await import("./archive-secret-scan.mjs")).scanArchiveForSecrets(options) }) {
  context(env, true);
  const candidate = selectCandidate(approvedFile, candidateId);
  const api = evidenceApi(env, fetchImpl);
  const root = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "supplement-producer-"));
  try {
    const { manifest, packDir } = await originalArchive({ candidate, env, api, root, spawn });
    const units = [];
    for (const unit of manifest.units) {
      const archiveScan = await scanArchive({ file: path.join(packDir, unit.file), registry: candidate.registry === "npm" ? "npm" : "cargo", name: unit.name, version: unit.version, scratchRoot: root });
      need(archiveScanProblems(archiveScan, unit.sha256).length === 0, "original archive did not pass scan");
      units.push({ name: unit.name, version: unit.version, file: unit.file, sha256: unit.sha256, archiveScan });
    }
    const proof = { schema: 1, kind: "archive-scan-supplement", original: original(candidate), producer: { workflow: SUPPLEMENT_WORKFLOW, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, sourceSha: env.GITHUB_SHA }, policy: { id: SUPPLEMENT_POLICY, sourceSha: env.GITHUB_SHA, validatorSourceSha: env.GITHUB_SHA }, units };
    const out = path.join(env.RUNNER_TEMP, "archive-scan-supplement");
    fs.mkdirSync(out, { recursive: true });
    const file = path.join(out, proofName);
    fs.writeFileSync(file, `${JSON.stringify(proof, null, 2)}\n`, { flag: "wx", mode: 0o600 });
    return { file, proof, proofSha256: sha("sha256", fs.readFileSync(file)) };
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

/** Pure proof check; authentication is owned by verifyArchiveSupplement below, never by this helper. */
export function validateSupplementProof(proof, { candidate, pin, manifest }) {
  validateSupplementPin(pin, candidate);
  need(pin.kind === "archive-scan-supplement-pin", "wrong archive supplement kind");
  exact(proof, ["schema", "kind", "original", "producer", "policy", "units"], "supplement proof");
  need(proof.schema === 1 && proof.kind === "archive-scan-supplement", "unknown proof version/kind");
  checkOriginal(proof.original, candidate);
  exact(proof.producer, ["workflow", "runId", "runAttempt", "sourceSha"], "proof producer");
  need(proof.producer.workflow === SUPPLEMENT_WORKFLOW && ["runId", "runAttempt", "sourceSha"].every((key) => proof.producer[key] === pin.producer[key]), "proof producer mismatch");
  policy(proof.policy, pin.producer.sourceSha);
  need(same(proof.policy, pin.policy), "proof policy mismatch");
  need(manifest?.commit === candidate.sourceSha && manifest.registry === candidate.registry && Array.isArray(manifest.units) && same(manifest.units.map((u) => u.name), candidate.units), "original manifest identity/order mismatch");
  need(Array.isArray(proof.units) && proof.units.length === manifest.units.length, "proof unit coverage mismatch");
  for (let i = 0; i < manifest.units.length; i++) {
    const row = proof.units[i]; const unit = manifest.units[i];
    exact(row, ["name", "version", "file", "sha256", "archiveScan"], "proof unit");
    need(["name", "version", "file", "sha256"].every((key) => row[key] === unit[key]), "proof archive identity mismatch");
    need(archiveScanProblems(row.archiveScan, unit.sha256).length === 0, "supplement scan failed or malformed");
  }
  return proof;
}

/** Shared fixed-repository producer authentication; callers cannot substitute artifact/run checks. */
export async function authenticateSupplement({ candidate, pin, env, fetchImpl = fetch, spawn = spawnSync, root, repository, workflowPath, requiredJobs, name }) {
  const api = evidenceApi(env, fetchImpl);
    need(repository?.full_name === REPOSITORY && Number.isSafeInteger(repository.id), "fixed repository mismatch");
    const workflow = await api(`actions/workflows/${path.posix.basename(workflowPath)}`);
    need(String(workflow.id) === pin.producer.workflowId && workflow.path === workflowPath && workflow.state === "active", "producer workflow mismatch");
    const route = `actions/runs/${pin.producer.runId}/attempts/${pin.producer.runAttempt}`;
    const run = await api(route);
    need(String(run.id) === pin.producer.runId && Number(run.run_attempt) === Number(pin.producer.runAttempt) && String(run.workflow_id) === pin.producer.workflowId && run.path === workflowPath && run.event === "workflow_dispatch" && run.head_branch === "main" && run.head_sha === pin.producer.sourceSha && run.status === "completed" && run.conclusion === "success", "producer run provenance/success mismatch");
    for (const repo of [run.repository, run.head_repository]) need(repo?.id === repository.id && repo.full_name === REPOSITORY, "foreign producer repository");
    const comparison = await api(`compare/${pin.producer.sourceSha}...${env.GITHUB_SHA}`);
    need(["ahead", "identical"].includes(comparison.status) && comparison.merge_base_commit?.sha === pin.producer.sourceSha, "validator must descend from pinned policy/validator revision");
    const jobs = await api(`${route}/jobs?per_page=100`);
    need(Array.isArray(jobs.jobs) && jobs.jobs.length === jobs.total_count && new Set(jobs.jobs.map((job) => job.id)).size === jobs.jobs.length, "incomplete producer jobs");
    need(jobs.jobs.every((job) => String(job.run_id) === pin.producer.runId && Number(job.run_attempt) === Number(pin.producer.runAttempt) && job.head_sha === pin.producer.sourceSha), "producer job provenance mismatch");
    for (const [name, jobId] of Object.entries(requiredJobs)) {
      const selected = jobs.jobs.filter((job) => String(job.id) === jobId && job.name === name);
      need(selected.length === 1 && jobs.jobs.filter((job) => job.name === name).length === 1 && selected[0].status === "completed" && selected[0].conclusion === "success", "pinned producer job did not succeed");
    }
    const destination = path.join(root, "supplement");
    const { entries } = await download(api, pin.artifact, destination, name, { runId: pin.producer.runId, sourceSha: pin.producer.sourceSha, repository: repository }, spawn, pin.kind === "runtime-workload-pin");
    return { entries, destination, api };
}

export async function retainedSupplementOriginal({ candidate, env, fetchImpl = fetch, root, spawn = spawnSync }) {
  context(env);
  return originalArchive({ candidate, env, api: evidenceApi(env, fetchImpl), root, spawn });
}

const authenticated = new WeakMap();
/** Trusted main validator authenticates original archive retention & successful pinned producer run. */
export async function verifyArchiveSupplement({ candidate, pin, manifest, manifestSha256, env, fetchImpl = fetch, spawn = spawnSync }) {
  context(env);
  validateSupplementPin(pin, candidate);
  need(manifestSha256 === candidate.manifestSha256, "caller manifest hash mismatch");
  const api = evidenceApi(env, fetchImpl);
  const root = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "supplement-validator-"));
  try {
    const original = await originalArchive({ candidate, env, api, root, spawn });
    need(same(original.manifest, manifest), "caller manifest differs from retained original");
    const { destination } = await authenticateSupplement({ candidate, pin, env, fetchImpl, spawn, root, repository: original.repository, workflowPath: SUPPLEMENT_WORKFLOW, requiredJobs: { "archive-scan": pin.producer.jobId }, name: artifactName(candidate.id, pin.producer.runAttempt) });
    need(same(fs.readdirSync(destination), [proofName]), "supplement artifact must contain exactly one proof");
    const raw = fs.readFileSync(path.join(destination, proofName));
    need(sha("sha256", raw) === pin.artifact.proofSha256, "raw supplemental proof hash mismatch");
    const proof = validateSupplementProof(JSON.parse(raw.toString("utf8")), { candidate, pin, manifest: original.manifest });
    const result = Object.freeze({ candidateId: candidate.id, manifestSha256, proofSha256: pin.artifact.proofSha256 });
    // No caller-supplied JSON or boolean can impersonate authenticated supplemental evidence.
    authenticated.set(result, { candidateSha256: candidateHash(candidate), reports: new Map(proof.units.map((row) => [row.name, structuredClone(row.archiveScan)])) });
    return result;
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

/** Return fresh report copies for missing historical archiveScan fields only; never override a failure. */
export function supplementalArchiveScan(result, candidate, unit) {
  const admitted = authenticated.get(result);
  need(admitted && admitted.candidateSha256 === candidateHash(candidate), "unauthenticated supplemental evidence");
  const report = admitted.reports.get(unit.name);
  need(archiveScanProblems(report, unit.sha256).length === 0, "supplement does not cover original archive");
  return structuredClone(report);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    need(process.argv[2] === "produce" && process.argv.length === 3, "usage: supplemental-proof.mjs produce");
    const result = await produceArchiveSupplement({ candidateId: process.env.PUBLISH_CANDIDATE_ID });
    console.log(`archive supplement proof SHA-256 ${result.proofSha256}`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
