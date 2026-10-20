#!/usr/bin/env node
// Trusted, dependency-free promotion of original archives. Never execute source-run code.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { APPROVED_ARTIFACTS_FILE, REPOSITORY, sha, loadSealed, checkReceipts, checkApprovedArtifacts, executeUpload, childEnv } from "./upload.mjs";

const HEX = /^[0-9a-f]{64}$/;
const POS = /^[1-9][0-9]*$/;
const contexts = new WeakMap();
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
  requireThat(rows.every((job) => String(job.run_id) === String(candidate.runId) && job.head_sha === candidate.sourceSha), "job provenance mismatch");
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

export async function prepareCandidate({ env, candidateId, approvedFile = APPROVED_ARTIFACTS_FILE, fetchImpl = fetch, spawn = spawnSync }) {
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
  const problems = [...sealed.problems, ...checkApprovedArtifacts({ manifest: sealed.manifest, registry: candidate.registry, approvedFile }), ...checkReceipts({ manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, receiptsDir, allow: candidate.allow, run: { runId: candidate.runId, runAttempt: candidate.runAttempt } })];
  requireThat(problems.length === 0, problems.join("; "));
  // Freeze the authenticated record; caller cannot widen its allowsets after admission.
  const context = JSON.parse(JSON.stringify({ candidate }));
  const freeze = (value) => { if (value && typeof value === "object") { Object.values(value).forEach(freeze); Object.freeze(value); } }; freeze(context);
  contexts.set(context, { env: { ...env }, packDir, receiptsDir, approvedFile });
  return { context, packDir, receiptsDir };
}

export function assertAuthenticatedContext(context, actual) {
  const bound = contexts.get(context);
  requireThat(bound && ["packDir", "receiptsDir", "approvedFile"].every((key) => bound[key] === actual[key]), "unauthenticated candidate context");
  for (const [key, value] of Object.entries(bound.env)) if (key.startsWith("GITHUB_")) requireThat(actual.env[key] === value, "promotion GitHub identity changed");
  const candidate = context.candidate;
  requireThat(actual.env.PUBLISH_REGISTRY === candidate.registry && actual.env.PUBLISH_UNITS === candidate.units.join(" "), "candidate plan changed");
  for (const [key, envKey] of Object.entries({ hostExclusion: "HOST_EXCLUSION", noTest: "NO_TEST", ignored: "IGNORED", missingAssets: "MISSING_ASSETS" })) requireThat((actual.env[`PUBLISH_ALLOW_${envKey}`] ?? "") === candidate.allow[key].join(" "), "candidate allowsets changed");
}

export async function runReviewed({ env, dryRun, fetchImpl = fetch, spawn = spawnSync }) {
  const prepared = await prepareCandidate({ env, candidateId: env.PUBLISH_CANDIDATE_ID, fetchImpl, spawn });
  const candidate = prepared.context.candidate;
  const uploadEnv = { ...env, PUBLISH_REGISTRY: candidate.registry, PUBLISH_UNITS: candidate.units.join(" "), PUBLISH_ALLOW_HOST_EXCLUSION: candidate.allow.hostExclusion.join(" "), PUBLISH_ALLOW_NO_TEST: candidate.allow.noTest.join(" "), PUBLISH_ALLOW_IGNORED: candidate.allow.ignored.join(" "), PUBLISH_ALLOW_MISSING_ASSETS: candidate.allow.missingAssets.join(" ") };
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, `registry=${candidate.registry}\n`);
  return executeUpload({ env: uploadEnv, ...prepared, candidateContext: prepared.context, approvedFile: APPROVED_ARTIFACTS_FILE, outDir: path.join(env.RUNNER_TEMP, "publication-receipt"), dryRun, fetchImpl, spawn });
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const [command, ...flags] = process.argv.slice(2);
  try {
    requireThat(command === "run" && flags.length <= 1 && flags.every((flag) => flag === "--dry-run"), "usage: reviewed-candidate.mjs run [--dry-run]");
    await runReviewed({ env: process.env, dryRun: flags.includes("--dry-run") });
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
