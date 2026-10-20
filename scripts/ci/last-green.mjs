import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const MAX_RUNS = 40;
const MAX_JOB_PAGES = 5;
const API_ROOT = "https://api.github.com";
export const WORKFLOW_PATH = ".github/workflows/ci.yml";
export const MARK_GREEN_JOBS = Object.freeze({ windows: "mark-green-windows", macos: "mark-green-macos" });
export const RUNNERS = Object.freeze({ windows: "windows-2025", macos: "xcode-27" });
export const EVIDENCE_CONTRACT = "rightkit-shared-ci-last-green/v1";

// Keep this list aligned with shared-CI behavior; unrelated publishing/readback edits do not invalidate markers.
export const COMPATIBILITY_GROUPS = Object.freeze({
  runner: Object.freeze([
    ".github/actions/rk-setup/action.yml", ".github/workflows/ci.yml", "rust-toolchain.toml", "rust-toolchain",
  ]),
  toolchain: Object.freeze([
    "Cargo.toml", "package.json", "pnpm-workspace.yaml",
  ]),
  evidence: Object.freeze([
    "ci/gate.config.json", "ci/shards.json", "ci/known-red.json", "ci/chrome-for-testing.json",
    "scripts/ci/ci-plan.mjs", "scripts/ci/ci-summary.mjs", "scripts/ci/gate.mjs", "scripts/ci/select.mjs",
    "scripts/ci/shards.mjs", "scripts/ci/test-results.mjs", "scripts/ci/secret-scan.mjs",
    "scripts/ci/provision-chrome.mjs", "scripts/ci/provision-ort.mjs",
    "crates/rightkit-ffmpeg/catalog/native-libraries.json",
  ]),
});
const OPTIONAL_INPUTS = Object.freeze(["rust-toolchain.toml", "rust-toolchain"]);
const REQUIRED_INPUTS = Object.freeze(Object.values(COMPATIBILITY_GROUPS).flat());
const REQUIRED_FILES = Object.freeze(REQUIRED_INPUTS.filter((file) => !OPTIONAL_INPUTS.includes(file)));
const FULL_SHA = /^[a-f0-9]{40}$/i;
const safeId = (value) => Number.isSafeInteger(Number(value)) && Number(value) > 0;
const sameId = (left, right) => String(left) === String(right);
const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");

function isInGroup(file, prefixes) {
  return prefixes.some((prefix) => prefix.endsWith("/") ? file.startsWith(prefix) : file === prefix);
}

/** Hash committed path/mode/blob inventories for each declared marker-compatibility boundary. */
export function compatibilityFingerprints(pathEvidence) {
  if (!pathEvidence || typeof pathEvidence !== "object" || Array.isArray(pathEvidence)) return null;
  const entries = Object.entries(pathEvidence).map(([file, evidence]) => [file.replaceAll("\\", "/").replace(/^\.\//, ""), evidence]);
  if (entries.some(([file, evidence]) => !file || file.startsWith("/") || file.includes(":") || file.split("/").some((part) => part === ".." || part === ".") || !evidence || typeof evidence !== "object" || !/^[0-7]{6}$/.test(evidence.mode ?? "") || !/^[a-f0-9]{40,64}$/i.test(evidence.oid ?? ""))) return null;
  if (new Set(entries.map(([file]) => file)).size !== entries.length) return null;
  const byPath = new Map(entries);
  if (REQUIRED_FILES.some((file) => !byPath.has(file))) return null;
  const result = {};
  for (const [group, prefixes] of Object.entries(COMPATIBILITY_GROUPS)) {
    const files = entries.filter(([file]) => isInGroup(file, prefixes)).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
    if (prefixes.some((file) => !OPTIONAL_INPUTS.includes(file) && !byPath.has(file))) return null;
    result[group] = sha256(JSON.stringify({ contract: EVIDENCE_CONTRACT, group, files }));
  }
  return result;
}

function runProblems(run, { repository, runner }) {
  const problems = [];
  if (!run || !safeId(run.id) || !safeId(run.run_number) || !safeId(run.run_attempt)) problems.push("run identity or attempt is malformed");
  if (run?.path !== WORKFLOW_PATH) problems.push("workflow path is not the shared CI workflow");
  if (run?.repository?.full_name !== repository || run?.head_repository?.full_name !== repository) problems.push("run repository or head repository differs");
  if (run?.head_branch !== "main" || run?.event !== "push") problems.push("run is not a push to main");
  if (!FULL_SHA.test(run?.head_sha ?? "")) problems.push("run commit is not a full SHA");
  if (run?.status !== "completed") problems.push("workflow run is not completed");
  if (run?.conclusion === "cancelled") problems.push("workflow run was cancelled");
  if (!MARK_GREEN_JOBS[runner]) problems.push("runner key is unsupported");
  return problems;
}

function attemptJobsFor(attempts, run) {
  if (!Array.isArray(attempts)) return null;
  const matches = attempts.filter((attempt) => sameId(attempt?.runId, run?.id) && Number(attempt?.runAttempt) === Number(run?.run_attempt));
  return matches.length === 1 && Array.isArray(matches[0].jobs) ? matches[0].jobs : null;
}

function markerJobProblems(jobs, run, runner) {
  const name = MARK_GREEN_JOBS[runner];
  if (!Array.isArray(jobs)) return ["matching run attempt jobs are unavailable"];
  const matches = jobs.filter((job) => job?.name === name);
  if (matches.length !== 1) return [`expected one ${name} job for matching run attempt`];
  const job = matches[0];
  const problems = [];
  if (!sameId(job.run_id, run.id) || Number(job.run_attempt) !== Number(run.run_attempt)) problems.push("mark-green job run attempt differs");
  if (job.head_sha !== run.head_sha || job.head_branch !== "main") problems.push("mark-green job commit or branch differs");
  if (job.status !== "completed" || job.conclusion !== "success") problems.push("mark-green job did not succeed");
  return problems;
}

function compareFingerprints(candidate, current) {
  if (!candidate || !current) return ["compatibility fingerprints are unavailable"];
  const problems = [];
  for (const group of Object.keys(COMPATIBILITY_GROUPS)) {
    if (!candidate[group] || !current[group]) problems.push(`${group} fingerprint is unavailable`);
    else if (!/^[a-f0-9]{64}$/i.test(candidate[group]) || !/^[a-f0-9]{64}$/i.test(current[group])) problems.push(`${group} fingerprint is malformed`);
    else if (candidate[group] !== current[group]) problems.push(`${group} inputs changed since marker run`);
  }
  return problems;
}

const runOrder = (a, b) => Number(b?.run_number ?? 0) - Number(a?.run_number ?? 0) || Number(b?.run_attempt ?? 0) - Number(a?.run_attempt ?? 0);

/** Select latest proven per-runner marker; missing evidence always yields full-baseline fallback. */
export function selectLastGreenCandidate({
  runs, attempts, repository, currentHead, runner, ancestryBySha, fingerprintsBySha,
}) {
  if (!FULL_SHA.test(currentHead ?? "")) return { base: "", provenance: null, reason: "full baseline: current checkout is not a full SHA" };
  if (!MARK_GREEN_JOBS[runner]) return { base: "", provenance: null, reason: "full baseline: unsupported runner key" };
  if (!Array.isArray(runs) || !runs.length) return { base: "", provenance: null, reason: "full baseline: no workflow runs were available" };
  const ordered = [...runs].sort(runOrder);
  const rejections = [];
  for (const run of ordered) {
    const problems = runProblems(run, { repository, runner });
    const jobs = attemptJobsFor(attempts, run);
    problems.push(...markerJobProblems(jobs, run, runner));
    if (ancestryBySha?.[run?.head_sha] !== true) problems.push("git ancestry is not verified");
    const candidateFingerprints = fingerprintsBySha?.[run?.head_sha];
    const currentFingerprints = fingerprintsBySha?.[currentHead];
    problems.push(...compareFingerprints(candidateFingerprints, currentFingerprints));
    if (problems.length === 0) {
      const jobName = MARK_GREEN_JOBS[runner];
      return {
        base: run.head_sha,
        provenance: {
          repository,
          workflow: WORKFLOW_PATH,
          runId: String(run.id),
          runNumber: run.run_number,
          runAttempt: run.run_attempt,
          commit: run.head_sha,
          runner: RUNNERS[runner],
          markGreenJob: jobName,
          runnerFingerprint: candidateFingerprints.runner,
          toolchainFingerprint: candidateFingerprints.toolchain,
          evidenceContract: EVIDENCE_CONTRACT,
          evidenceFingerprint: candidateFingerprints.evidence,
        },
        reason: `eligible ${jobName} passed on ${run.head_sha}`,
      };
    }
    if (run?.path === WORKFLOW_PATH && run?.head_branch === "main" && run?.event === "push") {
      rejections.push(`run ${run?.id ?? "?"}: ${[...new Set(problems)].join(", ")}`);
    }
  }
  const detail = rejections[0] ?? "no matching shared CI push run";
  return { base: "", provenance: null, reason: `full baseline: ${detail}` };
}

const fallback = (reason) => ({ base: "", provenance: null, reason: `full baseline: ${reason}` });
const validRepository = (value) => /^[\w.-]+\/[\w.-]+$/.test(value ?? "");
const secretName = (name) => /TOKEN|PASSWORD|SECRET|CREDENTIAL/i.test(name) || /^ACTIONS_ID_TOKEN/i.test(name);
const gitEnvironment = () => Object.fromEntries(Object.entries(process.env).filter(([name]) => !secretName(name) && !/^GIT_/i.test(name)));

function runGit(args, binary = false) {
  return spawnSync("git", args, {
    cwd: ROOT,
    encoding: binary ? null : "utf8",
    windowsHide: true,
    maxBuffer: 64 * 1024 * 1024,
    env: gitEnvironment(),
  });
}

function currentCommit(gitRunner = runGit) {
  const result = gitRunner(["rev-parse", "HEAD"]);
  const sha = String(result.stdout ?? "").trim();
  return result.status === 0 && FULL_SHA.test(sha) ? sha : "";
}

function isAncestor(ancestor, head, gitRunner = runGit) {
  return gitRunner(["merge-base", "--is-ancestor", ancestor, head]).status === 0;
}

function commitFingerprints(sha, gitRunner = runGit) {
  const paths = [...new Set([...REQUIRED_INPUTS, ...OPTIONAL_INPUTS])];
  const result = gitRunner(["ls-tree", "-r", "-z", "--full-tree", sha, "--", ...paths], true);
  if (result.error || result.status !== 0) return null;
  const evidence = {};
  for (const record of Buffer.from(result.stdout ?? []).toString("utf8").split("\0").filter(Boolean)) {
    const match = /^([0-7]{6}) blob ([a-f0-9]{40,64})\t(.+)$/i.exec(record);
    if (!match) return null;
    evidence[match[3]] = { mode: match[1], oid: match[2] };
  }
  return compatibilityFingerprints(evidence);
}

/** Fixed-host, read-only Actions API client. It never follows API-provided links or redirects. */
export function createGitHubActionsClient({ token, repository, fetchImpl = globalThis.fetch }) {
  if (!validRepository(repository)) throw new Error("repository identity is unavailable");
  if (typeof token !== "string" || !token) throw new Error("GITHUB_TOKEN is unavailable");
  if (typeof fetchImpl !== "function") throw new Error("fetch is unavailable");

  async function get(apiPath) {
    let response;
    try {
      response = await fetchImpl(`${API_ROOT}${apiPath}`, {
        method: "GET",
        redirect: "error",
        signal: AbortSignal.timeout(15_000),
        headers: {
          Authorization: `Bearer ${token}`,
          Accept: "application/vnd.github+json",
          "X-GitHub-Api-Version": "2022-11-28",
        },
      });
    } catch {
      throw new Error("GitHub API request failed; full baseline required");
    }
    if (!response?.ok) throw new Error(`GitHub API unavailable (${response?.status ?? "unknown status"}); full baseline required`);
    try { return await response.json(); } catch { throw new Error("GitHub API returned invalid JSON; full baseline required"); }
  }

  return Object.freeze({
    async listRuns() {
      const data = await get(`/repos/${repository}/actions/workflows/ci.yml/runs?branch=main&event=push&per_page=${MAX_RUNS}&page=1`);
      if (!Number.isSafeInteger(data?.total_count) || data.total_count < 0 || !Array.isArray(data?.workflow_runs) || data.workflow_runs.length > MAX_RUNS || (data.total_count > 0 && data.workflow_runs.length === 0)) throw new Error("GitHub API runs response was invalid; full baseline required");
      return data.workflow_runs.slice(0, MAX_RUNS);
    },
    async listAttemptJobs(runId, runAttempt) {
      if (!safeId(runId) || !safeId(runAttempt)) throw new Error("run attempt identity is invalid; full baseline required");
      const all = [];
      let total = null;
      for (let page = 1; page <= MAX_JOB_PAGES; page += 1) {
        const data = await get(`/repos/${repository}/actions/runs/${Number(runId)}/attempts/${Number(runAttempt)}/jobs?per_page=100&page=${page}`);
        if (!Array.isArray(data?.jobs) || !Number.isSafeInteger(data.total_count) || data.total_count < 0) throw new Error("GitHub API jobs response was invalid; full baseline required");
        if (total === null) total = data.total_count;
        else if (data.total_count !== total) throw new Error("GitHub API jobs pages changed during read; full baseline required");
        all.push(...data.jobs);
        if (all.length >= total) return all;
        if (!data.jobs.length) throw new Error("GitHub API jobs listing was incomplete; full baseline required");
      }
      throw new Error("GitHub API jobs exceeded pagination bound; full baseline required");
    },
  });
}

async function resolveRunner(runs, runner, { client, repository, currentHead, currentFingerprints, gitRunner }) {
  const attempts = [];
  const ancestryBySha = {};
  const fingerprintsBySha = { [currentHead]: currentFingerprints };
  const ordered = [...runs].sort(runOrder);
  for (const run of ordered) {
    if (runProblems(run, { repository, runner }).length) continue;
    const sha = run.head_sha;
    if (ancestryBySha[sha] === undefined) ancestryBySha[sha] = isAncestor(sha, currentHead, gitRunner);
    if (!ancestryBySha[sha]) continue;
    if (!fingerprintsBySha[sha]) fingerprintsBySha[sha] = commitFingerprints(sha, gitRunner);
    if (compareFingerprints(fingerprintsBySha[sha], currentFingerprints).length) continue;

    const jobs = await client.listAttemptJobs(run.id, run.run_attempt);
    attempts.push({ runId: run.id, runAttempt: run.run_attempt, jobs });
    const selected = selectLastGreenCandidate({
      runs: ordered.filter((candidate) => Number(candidate.run_number) >= Number(run.run_number)),
      attempts, repository, currentHead, runner, ancestryBySha, fingerprintsBySha,
    });
    if (selected.base) return selected;
  }
  return selectLastGreenCandidate({ runs: ordered, attempts, repository, currentHead, runner, ancestryBySha, fingerprintsBySha });
}

/** Resolve both OS markers. Current HEAD and all trust evidence come from local Git/API state. */
export async function resolveLastGreen({ env = process.env, fetchImpl = globalThis.fetch, gitRunner = runGit } = {}) {
  const currentHead = currentCommit(gitRunner);
  const results = { windows: fallback("marker evidence could not be verified"), macos: fallback("marker evidence could not be verified") };
  if (!currentHead) {
    results.windows = fallback("current checkout HEAD is unavailable from Git");
    results.macos = fallback("current checkout HEAD is unavailable from Git");
    return { currentHead: "", results };
  }
  const currentFingerprints = commitFingerprints(currentHead, gitRunner);
  if (!currentFingerprints) {
    results.windows = fallback("current compatibility inputs could not be fingerprinted");
    results.macos = fallback("current compatibility inputs could not be fingerprinted");
    return { currentHead, results };
  }
  if (!env.GITHUB_TOKEN) {
    results.windows = fallback("GITHUB_TOKEN is unavailable");
    results.macos = fallback("GITHUB_TOKEN is unavailable");
    return { currentHead, results };
  }
  let client;
  let runs;
  try {
    client = createGitHubActionsClient({ token: env.GITHUB_TOKEN, repository: env.GITHUB_REPOSITORY, fetchImpl });
    runs = await client.listRuns();
  } catch (error) {
    const reason = error instanceof Error ? error.message : "GitHub API verification failed";
    results.windows = fallback(reason.replace(/^full baseline:\s*/i, ""));
    results.macos = fallback(reason.replace(/^full baseline:\s*/i, ""));
    return { currentHead, results };
  }
  try {
    results.windows = await resolveRunner(runs, "windows", { client, repository: env.GITHUB_REPOSITORY, currentHead, currentFingerprints, gitRunner });
    results.macos = await resolveRunner(runs, "macos", { client, repository: env.GITHUB_REPOSITORY, currentHead, currentFingerprints, gitRunner });
  } catch (error) {
    const reason = error instanceof Error ? error.message : "GitHub API verification failed";
    results.windows = fallback(reason.replace(/^full baseline:\s*/i, ""));
    results.macos = fallback(reason.replace(/^full baseline:\s*/i, ""));
  }
  return { currentHead, results };
}

function outputPath(args, env) {
  if (!env.RUNNER_TEMP || !path.isAbsolute(env.RUNNER_TEMP)) throw new Error("RUNNER_TEMP is unavailable");
  const temp = path.resolve(env.RUNNER_TEMP);
  const index = args.indexOf("--out");
  const requested = index >= 0 ? args[index + 1] : "last-green.json";
  if (!requested || requested.startsWith("-")) throw new Error("--out must be a filename under RUNNER_TEMP");
  const target = path.isAbsolute(requested) ? path.resolve(requested) : path.resolve(temp, requested);
  const relative = path.relative(temp, target);
  if (!relative || relative === "." || relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative) || path.dirname(target) !== temp) throw new Error("--out must stay directly under RUNNER_TEMP");
  return target;
}

async function cli() {
  const args = process.argv.slice(2);
  if (args[0] !== "resolve") throw new Error("usage: last-green.mjs resolve [--out <RUNNER_TEMP filename>]");
  if (args.length !== 1 && !(args.length === 3 && args[1] === "--out")) throw new Error("usage: last-green.mjs resolve [--out <RUNNER_TEMP filename>]");
  const target = outputPath(args, process.env);
  const result = await resolveLastGreen();
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, `${JSON.stringify(result, null, 2)}\n`);
  console.log(`last-green: windows=${result.results.windows.base || "full baseline"}; macos=${result.results.macos.base || "full baseline"}; output=${target}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  cli().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
