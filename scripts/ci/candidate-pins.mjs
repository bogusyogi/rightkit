#!/usr/bin/env node
// Read-only evidence collection. Output deliberately lacks reviewer & reviewRef.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { REPOSITORY, sha, linuxRequiredFor, loadSealed, checkReceipts } from "./upload.mjs";
import { unpackArtifact, validateCandidate } from "./reviewed-candidate.mjs";

const need = (ok, why) => { if (!ok) throw new Error(`candidate pins refused: ${why}`); };
const id = (v) => /^[1-9][0-9]*$/.test(String(v ?? "")) && Number.isSafeInteger(Number(v));
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const names = { packed: "publish-packed", macos: "publish-receipt-verify-xcode-27", windows: "publish-receipt-verify-windows-2025" };
const LINUX_ARTIFACT = "publish-receipt-verify-ubuntu-24.04";
const allowFields = { hostExclusion: "allowHostExclusion", noTest: "allowNoTest", ignored: "allowIgnored", missingAssets: "allowMissingAssets" };

export function checkQualification({ repository, workflow, run, jobs, runId, linuxRequired = false }) {
  need(id(repository?.id) && repository.full_name === REPOSITORY && repository.default_branch === "main" && repository.private === false, "wrong public repository/default branch");
  need(id(workflow?.id) && workflow.path === ".github/workflows/publish.yml" && workflow.state === "active", "wrong qualification workflow");
  need(String(run?.id) === String(runId) && id(run.run_attempt) && /^[0-9a-f]{40}$/.test(run.head_sha ?? ""), "invalid run/attempt/source SHA");
  for (const source of [run.repository, run.head_repository]) need(source?.id === repository.id && source.full_name === REPOSITORY, "foreign source/head repository");
  need(run.workflow_id === workflow.id && run.path === workflow.path && run.event === "workflow_dispatch" && run.head_branch === "main" && run.status === "completed" && run.conclusion === "success", "qualification workflow/event/ref/status mismatch");
  need(Array.isArray(jobs?.jobs) && jobs.jobs.length === jobs.total_count && new Set(jobs.jobs.map((j) => j.id)).size === jobs.jobs.length, "incomplete/duplicate job provenance");
  need(jobs.jobs.every((j) => id(j.id) && String(j.run_id) === String(runId) && Number(j.run_attempt) === Number(run.run_attempt) && j.head_sha === run.head_sha), "job run/attempt/SHA mismatch");
  const successful = (name) => {
    const rows = jobs.jobs.filter((j) => j.name === name);
    need(rows.length === 1 && rows[0].status === "completed" && rows[0].conclusion === "success", `${name} must succeed exactly once`);
  };
  for (const name of ["guard", "plan", "pack"]) successful(name);
  const qualified = jobs.jobs.filter((j) => j.name === "qualified");
  // Historical qualification lacked this aggregate. Absence is reportable for
  // unsigned collection only; an existing failed/duplicate aggregate is not.
  if (qualified.length > 0) successful("qualified");
  const verify = jobs.jobs.filter((j) => /^verify \(/.test(j.name));
  need(verify.length === 2 && verify.every((j) => j.status === "completed" && j.conclusion === "success") && verify.some((j) => /^verify \(xcode-27, /.test(j.name)) && verify.some((j) => /^verify \(windows-2025, /.test(j.name)), "both native verification jobs must succeed");
  const linuxJobs = jobs.jobs.filter((j) => j.name === "verify-linux");
  need(linuxRequired ? linuxJobs.length === 1 && linuxJobs[0].status === "completed" && linuxJobs[0].conclusion === "success" : linuxJobs.every((j) => j.conclusion === "skipped"), "the Linux verification job must succeed exactly when the plan requires Linux");
  return qualified.length === 0 ? [{ kind: "qualified-job", status: "missing" }] : [];
}

export function proposalOutput(outputDir, env) {
  need(typeof outputDir === "string" && path.isAbsolute(outputDir), "explicit absolute output directory required");
  const destination = path.resolve(outputDir);
  need(!fs.existsSync(destination), "output directory must be fresh");
  const parent = fs.realpathSync(path.dirname(destination));
  const within = (root) => {
    if (!root || !path.isAbsolute(root) || !fs.existsSync(root)) return false;
    const rel = path.relative(fs.realpathSync(root), parent);
    return rel === "" || (!path.isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${path.sep}`));
  };
  need(within(env.RUNNER_TEMP) || (process.platform === "win32" && within("D:/scratch")), "output must be under D:/scratch or RUNNER_TEMP");
  return path.join(parent, path.basename(destination));
}

export async function collectCandidatePins({ runId, candidateId, outputDir, env = process.env, fetchImpl = fetch, spawn = spawnSync }) {
  need(id(runId), "explicit numeric qualification run ID required");
  need(typeof candidateId === "string" && /^[a-z0-9][a-z0-9-]{0,79}$/.test(candidateId), "invalid candidate ID; paths & URLs forbidden");
  need(typeof env.GITHUB_TOKEN === "string" && env.GITHUB_TOKEN.length > 0, "GITHUB_TOKEN read authentication required");
  const root = proposalOutput(outputDir, env);
  async function api(route, binary = false) {
    let response;
    try {
      response = await fetchImpl(`https://api.github.com/repos/${REPOSITORY}${route ? `/${route}` : ""}`, { method: "GET", headers: { authorization: `Bearer ${env.GITHUB_TOKEN}`, accept: "application/vnd.github+json", "x-github-api-version": "2022-11-28" }, redirect: "manual", signal: AbortSignal.timeout(60_000) });
      if (binary && response.status === 302) {
        const url = new URL(response.headers.get("location"));
        need(url.protocol === "https:" && !url.username && !url.password && !url.port && (url.hostname.endsWith(".blob.core.windows.net") || url.hostname.endsWith(".actions.githubusercontent.com")), "unsafe artifact redirect");
        // Signed storage URL receives no authentication headers & cannot redirect again.
        response = await fetchImpl(url, { method: "GET", redirect: "error", signal: AbortSignal.timeout(120_000) });
      }
    } catch { throw new Error("candidate pins refused: GitHub evidence request failed"); }
    need(response?.ok, `GitHub evidence returned ${response?.status ?? "unknown"}`);
    try { return binary ? Buffer.from(await response.arrayBuffer()) : await response.json(); }
    catch { throw new Error("candidate pins refused: unreadable GitHub evidence"); }
  }
  const repository = await api(""); const workflow = await api("actions/workflows/publish.yml");
  const run = await api(`actions/runs/${runId}`);
  need(id(run?.run_attempt), "invalid run attempt");
  const attemptRoute = `actions/runs/${runId}/attempts/${run.run_attempt}`;
  const exactRun = await api(attemptRoute);
  need(same(run.head_sha, exactRun.head_sha) && Number(run.run_attempt) === Number(exactRun.run_attempt), "run changed during collection");
  const jobs = await api(`${attemptRoute}/jobs?per_page=100`);
  // The sealed manifest is not downloaded yet; the Linux requirement is read from the exact packed artifact below, so the
  // job check runs again once the units are known. First pass: any verify-linux job that exists must have succeeded.
  const missingProof = checkQualification({ repository, workflow, run: exactRun, jobs, runId, linuxRequired: jobs.jobs.some((j) => j.name === "verify-linux" && j.conclusion !== "skipped") });
  const listed = await api(`actions/runs/${runId}/artifacts?per_page=100`);
  need(Array.isArray(listed?.artifacts) && listed.artifacts.length === listed.total_count, "incomplete artifact listing");
  const selected = {};
  const wanted = { ...names, ...(jobs.jobs.some((j) => j.name === "verify-linux" && j.conclusion !== "skipped") ? { linux: LINUX_ARTIFACT } : {}) };
  for (const [host, name] of Object.entries(wanted)) {
    const rows = listed.artifacts.filter((a) => a.name === name);
    need(rows.length === 1 && id(rows[0].id), `${host} artifact must exist exactly once`);
    const a = await api(`actions/artifacts/${rows[0].id}`);
    need(a.id === rows[0].id && a.name === name && a.expired === false && /^sha256:[0-9a-f]{64}$/.test(a.digest ?? "") && a.digest === rows[0].digest, `${host} artifact identity/expiry/digest mismatch`);
    need(String(a.workflow_run?.id) === String(runId) && a.workflow_run.head_sha === exactRun.head_sha && a.workflow_run.head_branch === "main" && a.workflow_run.repository_id === repository.id && a.workflow_run.head_repository_id === repository.id, `${host} foreign artifact provenance`);
    selected[host] = a;
  }
  need(new Set(Object.values(selected).map((a) => a.id)).size === Object.keys(wanted).length, "duplicate artifact IDs");
  fs.mkdirSync(root);
  const packDir = path.join(root, "packed"); const receiptsDir = path.join(root, "receipts");
  const pins = {}; const receipts = {};
  for (const [host, artifact] of Object.entries(selected)) {
    const bytes = await api(`actions/artifacts/${artifact.id}/zip`, true);
    need(`sha256:${sha("sha256", bytes)}` === artifact.digest, `${host} downloaded ZIP digest mismatch`);
    const destination = host === "packed" ? packDir : path.join(receiptsDir, host);
    const entries = unpackArtifact({ bytes, zipFile: path.join(root, `${host}.zip`), destination, spawn });
    pins[host] = { id: String(artifact.id), digest: artifact.digest };
    if (host !== "packed") {
      need(entries.length === 1 && entries[0].endsWith(".json"), `${host} artifact must contain exactly one JSON receipt`);
      const raw = fs.readFileSync(path.join(destination, entries[0]));
      pins[host].receiptSha256 = sha("sha256", raw);
      receipts[host] = JSON.parse(raw.toString("utf8"));
      need(receipts[host].host === host && String(receipts[host].runAttempt) === String(exactRun.run_attempt), `${host} receipt host/attempt mismatch`);
    }
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(packDir, "manifest.json"), "utf8"));
  need(Array.isArray(manifest?.units) && manifest.units.length > 0, "empty sealed units");
  const units = manifest.units.map((u) => u.name); const allow = {};
  for (const [key, field] of Object.entries(allowFields)) {
    need(Array.isArray(receipts.macos[field]) && Array.isArray(receipts.windows[field]), `missing ${key} allowset`);
    // Preserve exact recorded order; differing native proof fails closed.
    need(same(receipts.macos[field], receipts.windows[field]), `native ${key} allowsets differ`);
    allow[key] = receipts.macos[field];
  }
  const sealed = loadSealed({ packDir, registry: manifest.registry, unitsText: units.join(" "), run: { runId: String(runId), runAttempt: String(exactRun.run_attempt) } });
  // The Linux requirement comes from the sealed units: a Linux-required candidate must have run and pinned Linux, and nothing else may.
  const linux = linuxRequiredFor({ registry: manifest.registry, units });
  need(linux.problems.length === 0, linux.problems.join("; "));
  need((linux.units.length > 0) === ("linux" in selected), "the Linux receipt must be present exactly when the sealed units require Linux");
  const candidate = validateCandidate({ id: candidateId, sourceSha: exactRun.head_sha, runId: String(runId), runAttempt: String(exactRun.run_attempt), workflowId: String(workflow.id), manifestSha256: sealed.manifestSha256, registry: manifest.registry, units, allow, artifacts: pins });
  need(manifest.commit === candidate.sourceSha && String(manifest.runAttempt) === candidate.runAttempt, "sealed source/attempt mismatch");
  const missingScanDiagnostics = new Set();
  for (const host of ["macos", "windows"]) {
    for (const unit of manifest.units) {
      const rows = Array.isArray(receipts[host].units) ? receipts[host].units.filter((row) => row?.name === unit.name) : [];
      if (rows.length === 1 && !Object.hasOwn(rows[0], "archiveScan")) {
        missingProof.push({ kind: "archive-scan", status: "missing", host, unit: unit.name, sha256: unit.sha256 });
        missingScanDiagnostics.add(`${host} receipt: ${unit.name}: archiveScan proof is missing or malformed`);
      }
    }
  }
  // Run original validator against untouched raw receipts. Only its exact
  // diagnostic for an absent field may become an explicit proposal deficit.
  // Null/malformed/failed reports & every other receipt failure still refuse.
  const receiptProblems = checkReceipts({ manifest, manifestSha256: sealed.manifestSha256, receiptsDir, allow, run: { runId: candidate.runId, runAttempt: candidate.runAttempt } });
  const problems = [...sealed.problems, ...receiptProblems.filter((problem) => !missingScanDiagnostics.has(problem))];
  need(problems.length === 0, problems.join("; "));
  const artifacts = manifest.units.map((u) => {
    need(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(u.version ?? "") && /^[0-9a-f]{64}$/.test(u.sha256 ?? ""), "invalid per-unit version/hash");
    return { registry: manifest.registry, name: u.name, version: u.version, sha256: u.sha256 };
  });
  const current = await api(`actions/runs/${runId}`);
  need(current.run_attempt === exactRun.run_attempt && current.head_sha === exactRun.head_sha && current.status === "completed" && current.conclusion === "success", "run changed during collection");
  const proposal = { schema: 2, candidates: [candidate], artifacts };
  const report = { candidateId: candidate.id, sourceSha: candidate.sourceSha, runId: candidate.runId, runAttempt: candidate.runAttempt, manifestSha256: candidate.manifestSha256, publicationAuthorized: false, missingProof };
  const proposalFile = path.join(root, "candidate-pin-proposal.json");
  const reportFile = path.join(root, "pin-collection-report.json");
  fs.writeFileSync(reportFile, `${JSON.stringify(report, null, 2)}\n`, { flag: "wx" });
  fs.writeFileSync(proposalFile, `${JSON.stringify(proposal, null, 2)}\n`, { flag: "wx" });
  return { proposal, proposalFile, report, reportFile };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [runId, candidateId, outputDir, ...extra] = process.argv.slice(2);
    need(extra.length === 0, "usage: candidate-pins.mjs <run-id> <candidate-id> <absolute-new-output-dir>");
    const { proposalFile, reportFile, report } = await collectCandidatePins({ runId, candidateId, outputDir });
    process.stdout.write(`Unapproved pin proposal: ${proposalFile}\nMissing proof: ${report.missingProof.length}; report: ${reportFile}\n`);
  } catch { process.stderr.write("Candidate pin collection refused; no approval granted.\n"); process.exitCode = 1; }
}
