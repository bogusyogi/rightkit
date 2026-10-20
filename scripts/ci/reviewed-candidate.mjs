#!/usr/bin/env node
// Trusted, dependency-free promotion of original archives. Never execute source-run code.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { APPROVED_ARTIFACTS_FILE, REPOSITORY, sha, loadSealed, checkReceipts, checkApprovedArtifacts, checkUploadPayloads, executeUpload, childEnv, authenticatedArchiveScanResolver } from "./upload.mjs";
import { validateSupplementPin, verifyArchiveSupplement } from "./supplemental-proof.mjs";

const HEX = /^[0-9a-f]{64}$/;
const POS = /^[1-9][0-9]*$/;
const contexts = new WeakMap();
export const SUPPLEMENTAL_PROOFS_FILE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "ci", "supplemental-proofs.json");
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const requireThat = (condition, message) => { if (!condition) throw new Error(`reviewed candidate refused: ${message}`); };
function exact(value, keys, label) {
  requireThat(value && typeof value === "object" && !Array.isArray(value) && same(Object.keys(value).sort(), [...keys].sort()), `${label} has unexpected or missing keys`);
}

// Schema 2 extends the existing R8 list; candidates never replace per-unit approvals.
export function validateCandidate(candidate) {
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
  exact(candidate.artifacts, ["packed", "macos", "windows"], "artifact pins");
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
export function selectSupplement(file, candidate, kind, approvedFile = APPROVED_ARTIFACTS_FILE) {
  const doc = JSON.parse(fs.readFileSync(file, "utf8"));
  exact(doc, ["schema", "supplements"], "supplement collection");
  requireThat(doc.schema === 1 && Array.isArray(doc.supplements), "unknown supplement collection schema");
  const seen = new Set();
  for (const pin of doc.supplements) {
    const id = pin?.original?.candidateId;
    const identity = `${id}:${pin?.kind}`;
    requireThat(typeof id === "string" && !seen.has(identity), "invalid or duplicate supplemental candidate/kind");
    seen.add(identity);
    validateSupplementPin(pin, selectCandidate(approvedFile, id));
  }
  const pin = doc.supplements.find((row) => row.original.candidateId === candidate.id && row.kind === kind) ?? null;
  if (pin) validateSupplementPin(pin, candidate);
  return pin;
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
  requireThat(rows.every((job) => String(job.run_id) === String(candidate.runId) && String(job.run_attempt) === String(candidate.runAttempt) && job.head_sha === candidate.sourceSha), "job provenance mismatch");
}

async function api(route, env, fetchImpl, binary = false) {
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

export async function prepareCandidate({ env, candidateId, approvedFile = APPROVED_ARTIFACTS_FILE, supplementalFile = SUPPLEMENTAL_PROOFS_FILE, fetchImpl = fetch, spawn = spawnSync }) {
  const { candidate, repository } = await authenticateSource({ env, candidateId, approvedFile, fetchImpl });
  const supplementPin = selectArchiveSupplement(supplementalFile, candidate, approvedFile);
  const root = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "reviewed-candidate-"));
  const packDir = path.join(root, "packed"); const receiptsDir = path.join(root, "receipts");
  const expectedNames = { packed: "publish-packed", macos: "publish-receipt-verify-xcode-27", windows: "publish-receipt-verify-windows-2025" };
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

export function assertAuthenticatedContext(context, actual) {
  const bound = contexts.get(context);
  requireThat(bound && ["packDir", "receiptsDir", "approvedFile"].every((key) => bound[key] === actual[key]), "unauthenticated candidate context");
  for (const [key, value] of Object.entries(bound.env)) if (key.startsWith("GITHUB_")) requireThat(actual.env[key] === value, "promotion GitHub identity changed");
  requireThat(bound.admitted === true, "semantic admission is required");
  const candidate = context.candidate;
  for (const host of ["macos", "windows"]) {
    const directory = path.join(bound.receiptsDir, host);
    const files = fs.readdirSync(directory);
    requireThat(files.length === 1 && sha("sha256", fs.readFileSync(path.join(directory, files[0]))) === candidate.artifacts[host].receiptSha256, "original native receipt changed after admission");
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

const artifactMatches = (artifact, { id, digest, name, runId, sourceSha, repository }) =>
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

function originalBinding(candidate, manifest) {
  return { candidateId: candidate.id, candidateSha256: sha("sha256", JSON.stringify(candidate)), sourceSha: candidate.sourceSha, manifestSha256: candidate.manifestSha256, archives: manifest.units.map(({ name, version, file, sha256, metaFile, metaSha256 }) => ({ name, version, file, sha256, ...(metaFile ? { metaFile, metaSha256 } : {}) })) };
}
function validateAdmission(admission, prepared, env) {
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
  for (const row of admission.units) {
    exact(row, ["name", "version", "archiveSha256", "ok", "policies", "failures", "adoptionPending", "runtime", "runtimeFeatureProof", "runtimeInputHashes", "evidenceSha256"], "semantic unit verdict");
    exact(row.policies, ["macos", "windows"], "semantic unit host policies");
    if (candidate.registry === "crates") {
      const proof = row.runtimeFeatureProof;
      exact(proof, ["classification", "listedOwner", "required", "status", "failures", ...(proof?.status === "qualified" ? ["proof"] : [])], "semantic runtime feature proof");
      requireThat(Array.isArray(proof.failures) && proof.failures.length === 0, "runtime feature proof failed");
      if (proof.status === "qualified") {
        const pin = selectSupplement(bound.supplementalFile, candidate, "runtime-workload-pin", bound.approvedFile);
        exact(proof.proof, ["candidateId", "proofSha256", "workloads"], "runtime supplement transfer");
        requireThat(row.name === "rightkit-logs" && proof.classification === "always-on" && proof.listedOwner === true && proof.required === true && pin && proof.proof.candidateId === candidate.id && proof.proof.proofSha256 === pin.artifact.proofSha256 && same(proof.proof.workloads, ["tracing", "detached"]), "runtime supplement transfer mismatch");
      } else requireThat(["false", "held-resource"].includes(proof.classification) && proof.listedOwner === false && proof.required === false && proof.status === "not-applicable", "runtime feature proof remains required, held or incoherent");
    } else requireThat(row.runtimeFeatureProof === null, "npm runtime feature proof must be null");
    requireThat(Object.values(row.policies).every((policy) => policy === "required-positive-tests") && Array.isArray(row.adoptionPending) && row.adoptionPending.every((reason) => typeof reason === "string" && reason.length > 0) && row.runtimeInputHashes && typeof row.runtimeInputHashes === "object" && !Array.isArray(row.runtimeInputHashes) && Object.entries(row.runtimeInputHashes).every(([name, hash]) => name && !/[\\\x00-\x1f:]/.test(name) && name.split("/").every((part) => part && part !== "." && part !== "..") && HEX.test(hash)) && (candidate.registry === "crates" ? row.runtime?.status === "ok" : row.runtime === null), "incomplete semantic native/runtime verdict");
    const unit = bound.manifest.units.find((item) => item.name === row?.name);
    requireThat(unit && !names.has(row.name) && row.version === unit.version && row.archiveSha256 === unit.sha256 && row.ok === true && Array.isArray(row.failures) && row.failures.length === 0 && HEX.test(row.evidenceSha256 ?? ""), "failed/duplicate/mismatched semantic unit verdict"); names.add(row.name);
  }
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
  requireThat(POS.test(String(workflow.id)) && workflow.path === ".github/workflows/publish-reviewed.yml" && workflow.state === "active" && String(run.id) === env.GITHUB_RUN_ID && String(run.run_attempt) === env.GITHUB_RUN_ATTEMPT && run.repository?.id === bound.repository.id && run.repository?.full_name === REPOSITORY && run.head_repository?.id === bound.repository.id && run.head_repository?.full_name === REPOSITORY && String(run.workflow_id) === String(workflow.id) && run.path === workflow.path && run.event === "workflow_dispatch" && run.head_branch === "main" && run.head_sha === env.GITHUB_SHA && ["in_progress", "completed"].includes(run.status), "promotion run/workflow provenance mismatch");
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
  return admission;
}

export async function runReviewed({ env, dryRun = false, verifyAdmission = false, fetchImpl = fetch, spawn = spawnSync, approvedFile = APPROVED_ARTIFACTS_FILE, supplementalFile = SUPPLEMENTAL_PROOFS_FILE }) {
  requireThat(!(dryRun && verifyAdmission), "conflicting run modes");
  if (dryRun || verifyAdmission) for (const key of ["NODE_AUTH_TOKEN", "NPM_TOKEN", "CARGO_REGISTRY_TOKEN"]) requireThat(!env[key] && !process.env[key], `${key} is present in secretless admission`);
  if (dryRun) for (const key of ["ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN"]) requireThat(!env[key] && !process.env[key], `${key} is present in secretless admission`);
  const prepared = await prepareCandidate({ env, candidateId: env.PUBLISH_CANDIDATE_ID, approvedFile, supplementalFile, fetchImpl, spawn });
  const candidate = prepared.context.candidate, bound = contexts.get(prepared.context);
  if (dryRun) {
    const payloadProblems = checkUploadPayloads({ manifest: bound.manifest, packDir: prepared.packDir, registry: candidate.registry, spawn });
    requireThat(payloadProblems.length === 0, payloadProblems.join("; "));
    const { loadCandidateSource } = await import("./candidate-source.mjs");
    const { qa, bindings } = await originalQa({ prepared, env, fetchImpl, spawn });
    const source = loadCandidateSource({ directory: env.PUBLISH_SOURCE_DIR, sourceSha: candidate.sourceSha, promotionSha: env.GITHUB_SHA, registry: candidate.registry, units: candidate.units, allow: candidate.allow, spawn });
    const { semanticAdmission } = await import("./semantic-admission.mjs");
    const receipts = Object.fromEntries(["macos", "windows"].map((host) => { const directory = path.join(prepared.receiptsDir, host); return [host, JSON.parse(fs.readFileSync(path.join(directory, fs.readdirSync(directory)[0]), "utf8"))]; }));
    const runtimePin = selectSupplement(supplementalFile, candidate, "runtime-workload-pin", approvedFile);
    let runtimeProof = null;
    if (runtimePin) {
      const { verifyRuntimeSupplement } = await import("./runtime-supplement.mjs");
      runtimeProof = await verifyRuntimeSupplement({ candidate, pin: runtimePin, manifest: bound.manifest, manifestSha256: candidate.manifestSha256, source, packDir: prepared.packDir, env, fetchImpl, spawn });
    }
    const result = await semanticAdmission({ candidate, manifest: bound.manifest, source, receipts, qa, packDir: prepared.packDir, env, spawn, runtimeProof });
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

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const [command, ...flags] = process.argv.slice(2);
  try {
    requireThat((command === "identify" && flags.length === 0) || (command === "run" && flags.length <= 1 && flags.every((flag) => ["--dry-run", "--verify-admission"].includes(flag))), "usage: reviewed-candidate.mjs identify | run [--dry-run|--verify-admission]");
    if (command === "identify") await identifyCandidate({ env: process.env, candidateId: process.env.PUBLISH_CANDIDATE_ID });
    else await runReviewed({ env: process.env, dryRun: flags.includes("--dry-run"), verifyAdmission: flags.includes("--verify-admission") });
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
