#!/usr/bin/env node
// The one sanctioned route that tags a RightKitSwift release (workflow .github/workflows/publish-swift.yml).
// Dependency-free: node built-ins plus two sibling scripts, installs and builds nothing.
//
//   publish-swift.mjs verify          every guard, no write; prints the exact tag commands (the dry run)
//   publish-swift.mjs tag             every guard again in the write job, then `git tag` + `git push` of that one tag
//   publish-swift.mjs digest --commit <sha>
//                                     prints the tree digest an approved-artifacts entry must hold (git only, local)
//
// Inputs arrive only through the environment (PUBLISH_SWIFT_VERSION, PUBLISH_SWIFT_COMMIT, PUBLISH_DRY_RUN). A tag is
// permanent for every app that resolves it (no yank, no overwrite, no reuse), so each of these must hold first:
//   1. trusted Actions context: workflow_dispatch of publish-swift.yml from main of bogusyogi/rightkit, and the
//      dispatch SHA equals the requested commit;
//   2. the checkout is that commit, clean, an ancestor of origin/main, and origin is this repository;
//   3. swift/RightKitSwift/VERSION equals the version, CHANGELOG.md has a dated `## <version> - YYYY-MM-DD` heading,
//      and the root Package.swift still matches the nested manifest (scripts/swift-root-manifest.test.mjs);
//   4. the tag exists neither locally nor on origin;
//   5. a green broker-dispatched rightkit-shared-ci run (workflow_dispatch; pushes skip native jobs) exists for exactly that commit, including `macos swift-rightkitswift`
//      (swift build, swift test and the generic iOS and watchOS xcodebuild rows are required commands of that job,
//      and the summary job proves every frozen command), `macos root` and mark-green-macos;
//   6. ci/approved-artifacts.json (read from that commit) holds one reviewer-approved registry=swift entry whose
//      sha256 is the tree digest of Package.swift + swift/RightKitSwift at that commit.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { REPOSITORY, loadApprovedArtifacts } from "./upload.mjs";
import { WORKFLOW_PATH, createGitHubActionsClient } from "./last-green.mjs";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const APPROVED_FILE = path.join(ROOT, "ci", "approved-artifacts.json");
export const SWIFT_NAME = "RightKitSwift";
export const PACKAGE_DIR = "swift/RightKitSwift";
export const DIGEST_PATHS = Object.freeze(["Package.swift", PACKAGE_DIR]);
export const WORKFLOW_REF = `${REPOSITORY}/.github/workflows/publish-swift.yml@refs/heads/main`;
export const DRIFT_TEST = path.join("scripts", "swift-root-manifest.test.mjs");
export const REQUIRED_CI_JOBS = Object.freeze(["summary", "mark-green-macos", "macos swift-rightkitswift", "macos root"]);
export const TREE_DIGEST_CONTRACT = "rightkit-swift-tree/v1";
const PLAIN_SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const FULL_SHA = /^[0-9a-f]{40}$/;

const refuse = (message) => { throw new Error(`swift publish refused: ${message}`); };
const need = (condition, message) => { if (!condition) refuse(message); };
const secretName = (name) => /TOKEN|PASSWORD|SECRET|CREDENTIAL/i.test(name) || /^ACTIONS_ID_TOKEN/i.test(name);

/** Dispatch inputs, validated strictly; nothing else is ever read from the dispatch. */
export function parseInputs(env) {
  const version = env.PUBLISH_SWIFT_VERSION ?? "";
  const commit = env.PUBLISH_SWIFT_COMMIT ?? "";
  need(PLAIN_SEMVER.test(version), `version must be plain semver X.Y.Z, got ${JSON.stringify(version)}`);
  need(FULL_SHA.test(commit), "commit must be a full 40-character lowercase hex SHA");
  need(env.PUBLISH_DRY_RUN === "true" || env.PUBLISH_DRY_RUN === "false", "PUBLISH_DRY_RUN must be exactly true or false");
  return { version, commit, dryRun: env.PUBLISH_DRY_RUN === "true" };
}

/** The Actions context must be this repository's publish-swift workflow, dispatched from main at the requested commit. */
export function checkIdentity(env, { commit }) {
  need(env.GITHUB_ACTIONS === "true", "runs only in GitHub Actions");
  need(env.GITHUB_EVENT_NAME === "workflow_dispatch", "runs only for a workflow_dispatch");
  need(env.GITHUB_REPOSITORY === REPOSITORY, `wrong repository ${JSON.stringify(env.GITHUB_REPOSITORY)}`);
  need(env.GITHUB_REF === "refs/heads/main", `must be dispatched from refs/heads/main, not ${JSON.stringify(env.GITHUB_REF)}`);
  need(env.GITHUB_WORKFLOW_REF === WORKFLOW_REF, "wrong workflow");
  need(env.GITHUB_SHA === commit, `commit ${commit} is not the dispatch ref SHA ${env.GITHUB_SHA}`);
  need(typeof env.GITHUB_TOKEN === "string" && env.GITHUB_TOKEN !== "", "GITHUB_TOKEN is required");
  need(typeof env.RUNNER_TEMP === "string" && path.isAbsolute(env.RUNNER_TEMP), "RUNNER_TEMP is required");
  return true;
}

// ---- git ---------------------------------------------------------------------------------------

const cleanEnvironment = (extra = {}) => ({
  ...Object.fromEntries(Object.entries(process.env).filter(([name]) => !secretName(name) && !/^GIT_/i.test(name))),
  GIT_TERMINAL_PROMPT: "0",
  ...extra,
});

/** A git runner bound to a checkout. `extra` is the only way a credential reaches a child. */
export function gitRunner(root, extra = {}) {
  return (args, { binary = false } = {}) => spawnSync("git", args, {
    cwd: root, encoding: binary ? null : "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, env: cleanEnvironment(extra),
  });
}

function gitText(git, args) {
  const result = git(args);
  if (result.error) throw result.error;
  if (result.status !== 0) refuse(`git ${args[0]} failed: ${String(result.stderr ?? "").trim()}`);
  return String(result.stdout ?? "").trim();
}

/** owner/name of a GitHub remote URL (credentials, .git and a trailing slash ignored), or null. */
export function repositoryOfRemote(url) {
  return /github\.com[:/]([\w.-]+\/[\w.-]+?)(?:\.git)?\/?$/i.exec(String(url ?? "").trim())?.[1] ?? null;
}

/**
 * Digest of the exact tree apps resolve at a commit: sha256 over the NUL-separated `git ls-tree -r` records
 * (mode, blob id, path) of Package.swift and swift/RightKitSwift. Blob ids are content hashes, so the digest does
 * not depend on the git or tar version. The reviewer approval is bound to this value.
 */
export function treeDigest(git, commit) {
  need(FULL_SHA.test(commit), "tree digest needs a full commit SHA");
  const result = git(["ls-tree", "-r", "-z", "--full-tree", commit, "--", ...DIGEST_PATHS], { binary: true });
  if (result.error) throw result.error;
  if (result.status !== 0) refuse(`cannot list the Swift tree at ${commit}`);
  const records = Buffer.from(result.stdout ?? []);
  const text = records.toString("utf8");
  need(text.startsWith("100644 blob ") || text.startsWith("100755 blob "), "no Swift tree at that commit");
  need(text.includes(`\t${PACKAGE_DIR}/VERSION\0`) && text.includes("\tPackage.swift\0"), "the Swift tree lacks Package.swift or swift/RightKitSwift/VERSION");
  return crypto.createHash("sha256").update(`${TREE_DIGEST_CONTRACT}\0`).update(records).digest("hex");
}

/** Git-level guards on the checkout. Returns the tree digest. */
export function checkCheckout({ git, root = ROOT, commit, version, repository = REPOSITORY, drift = defaultDrift }) {
  need(gitText(git, ["rev-parse", "HEAD"]) === commit, "the checkout is not the requested commit");
  need(repositoryOfRemote(gitText(git, ["config", "--get", "remote.origin.url"]))?.toLowerCase() === repository.toLowerCase(), `origin is not ${repository}`);
  need(gitText(git, ["status", "--porcelain", "--untracked-files=normal"]) === "", "the working tree is not clean");
  const fetched = git(["fetch", "--quiet", "origin", "main"]);
  if (fetched.error) throw fetched.error;
  need(fetched.status === 0, `cannot fetch origin main: ${String(fetched.stderr ?? "").trim()}`);
  const ancestor = git(["merge-base", "--is-ancestor", commit, "origin/main"]);
  if (ancestor.error) throw ancestor.error;
  need(ancestor.status === 0, `commit ${commit} is not on origin/main`);
  need(gitText(git, ["tag", "-l", version]) === "", `tag ${version} already exists locally`);
  need(gitText(git, ["ls-remote", "--tags", "origin", `refs/tags/${version}`]) === "", `tag ${version} already exists on origin`);
  const drifted = drift(root);
  need(drifted.status === 0, `the root Package.swift drifted from ${PACKAGE_DIR}/Package.swift:\n${drifted.stdout ?? ""}${drifted.stderr ?? ""}`);
  return treeDigest(git, commit);
}

function defaultDrift(root) {
  return spawnSync(process.execPath, ["--test", DRIFT_TEST], { cwd: root, encoding: "utf8", windowsHide: true, env: cleanEnvironment() });
}

// ---- source files ------------------------------------------------------------------------------

const escapeRegExp = (value) => String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** A real calendar date in YYYY-MM-DD. */
export function isCalendarDate(text) {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text ?? "");
  if (!match) return false;
  const date = new Date(Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3])));
  return date.getUTCFullYear() === Number(match[1]) && date.getUTCMonth() === Number(match[2]) - 1 && date.getUTCDate() === Number(match[3]);
}

/** VERSION is exactly the requested version; the changelog has one dated heading for it ("unpublished" never passes). */
export function checkSourceFiles({ root = ROOT, version }) {
  const read = (file) => { try { return fs.readFileSync(path.join(root, PACKAGE_DIR, file), "utf8"); } catch { return refuse(`${PACKAGE_DIR}/${file} is unreadable`); } };
  need(read("VERSION").trim() === version, `${PACKAGE_DIR}/VERSION is ${JSON.stringify(read("VERSION").trim())}, not ${version}`);
  const heading = new RegExp(`^## ${escapeRegExp(version)} [-–—] (\\S+)[ \\t]*$`, "gm");
  const dates = [...read("CHANGELOG.md").matchAll(heading)].map((match) => match[1]);
  need(dates.length === 1 && isCalendarDate(dates[0]), `${PACKAGE_DIR}/CHANGELOG.md needs exactly one heading "## ${version} - YYYY-MM-DD" with a real date`);
  return dates[0];
}

// ---- reviewer approval -------------------------------------------------------------------------

/** One registry=swift entry for this name and version, and its sha256 is the tree digest. Fails closed. */
export function checkApproval({ approvedFile = APPROVED_FILE, version, digest }) {
  const { entries, problems } = loadApprovedArtifacts(approvedFile);
  need(problems.length === 0, `ci/approved-artifacts.json is not valid: ${problems.join("; ")}`);
  const matches = entries.filter((entry) => entry.registry === "swift" && entry.name === SWIFT_NAME && entry.version === version);
  need(matches.length === 1, `${SWIFT_NAME}@${version} is not reviewer-approved: ci/approved-artifacts.json has no swift entry for it`);
  need(matches[0].sha256 === digest, `tree digest ${digest.slice(0, 12)} is not the reviewer-approved ${matches[0].sha256.slice(0, 12)} (${matches[0].reviewRef})`);
  return matches[0];
}

// ---- shared CI evidence ------------------------------------------------------------------------

const runOrder = (a, b) => Number(b?.run_number ?? 0) - Number(a?.run_number ?? 0) || Number(b?.run_attempt ?? 0) - Number(a?.run_attempt ?? 0);

/** The latest broker-dispatched rightkit-shared-ci run on main for exactly this commit; it must have completed green. Only a
 * dispatched run carries the native macOS jobs (a push runs static validation only and skips them), and ci.yml binds it to
 * the commit (expected_sha must equal GITHUB_SHA). */
export function selectCiRun({ runs, repository = REPOSITORY, commit }) {
  const rows = (Array.isArray(runs) ? runs : [])
    .filter((run) => run?.head_sha === commit && run.path === WORKFLOW_PATH && run.event === "workflow_dispatch" && run.head_branch === "main"
      && run.repository?.full_name === repository && run.head_repository?.full_name === repository)
    .sort(runOrder);
  need(rows.length > 0, `no broker-dispatched rightkit-shared-ci run for ${commit} in the latest main runs`);
  const run = rows[0];
  need(run.status === "completed" && run.conclusion === "success", `rightkit-shared-ci run ${run.id} for ${commit} is ${run.status}/${run.conclusion ?? "none"}, not green`);
  return run;
}

/** Each required job exists exactly once for this run attempt, on this commit, and succeeded. Returns the problems. */
export function ciJobProblems({ jobs, run, commit }) {
  if (!Array.isArray(jobs)) return ["the run attempt jobs are unavailable"];
  const problems = [];
  for (const name of REQUIRED_CI_JOBS) {
    const found = jobs.filter((job) => job?.name === name);
    if (found.length !== 1) { problems.push(`expected one "${name}" job, found ${found.length}${name === "macos swift-rightkitswift" && found.length === 0 ? " (CI selected no Swift work for this commit)" : ""}`); continue; }
    const [job] = found;
    if (job.status !== "completed" || job.conclusion !== "success") problems.push(`"${name}" did not succeed (${job.status}/${job.conclusion ?? "none"})`);
    if (String(job.run_id) !== String(run.id) || Number(job.run_attempt) !== Number(run.run_attempt) || job.head_sha !== commit) problems.push(`"${name}" belongs to another run attempt or commit`);
  }
  return problems;
}

export async function checkCi({ client, repository = REPOSITORY, commit }) {
  const run = selectCiRun({ runs: await client.listRuns(), repository, commit });
  const problems = ciJobProblems({ jobs: await client.listAttemptJobs(run.id, run.run_attempt), run, commit });
  need(problems.length === 0, `rightkit-shared-ci run ${run.id}: ${problems.join("; ")}`);
  return { id: String(run.id), attempt: Number(run.run_attempt), number: Number(run.run_number) };
}

// ---- the whole verification, and the tag -------------------------------------------------------

export async function verifyAll({ env = process.env, root = ROOT, git = gitRunner(root), client, approvedFile = APPROVED_FILE, drift } = {}) {
  const inputs = parseInputs(env);
  checkIdentity(env, inputs);
  const digest = checkCheckout({ git, root, commit: inputs.commit, version: inputs.version, drift });
  const changelogDate = checkSourceFiles({ root, version: inputs.version });
  const approval = checkApproval({ approvedFile, version: inputs.version, digest });
  const ci = await checkCi({ client: client ?? createGitHubActionsClient({ token: env.GITHUB_TOKEN, repository: env.GITHUB_REPOSITORY }), commit: inputs.commit });
  return { ...inputs, digest, changelogDate, approvalRef: approval.reviewRef, ci };
}

export const tagCommands = (version, commit) => [
  ["git", ["tag", version, commit]],
  ["git", ["push", "origin", `refs/tags/${version}:refs/tags/${version}`]],
];

/** git env that authenticates one push through the environment, so the token never appears on a command line. */
export const pushCredential = (token) => ({
  GIT_CONFIG_COUNT: "1",
  GIT_CONFIG_KEY_0: "http.https://github.com/.extraheader",
  GIT_CONFIG_VALUE_0: `AUTHORIZATION: basic ${Buffer.from(`x-access-token:${token}`).toString("base64")}`,
});

/** Creates the lightweight tag and pushes exactly it (no force), then reads it back from origin. */
export function createTag({ git, pushGit, version, commit }) {
  const [tag, push] = tagCommands(version, commit);
  gitText(git, tag[1]);
  const pushed = pushGit(push[1]);
  if (pushed.error || pushed.status !== 0) {
    git(["tag", "-d", version]);
    refuse(`pushing tag ${version} failed: ${pushed.error?.message ?? String(pushed.stderr ?? "").trim()}`);
  }
  const remote = gitText(git, ["ls-remote", "--tags", "origin", `refs/tags/${version}`]);
  need(remote.startsWith(commit), `origin does not show tag ${version} at ${commit}`);
}

function writeReceipt({ env, verified, published }) {
  const directory = path.join(env.RUNNER_TEMP, "swift-publication");
  fs.mkdirSync(directory, { recursive: true });
  const receipt = {
    schema: 1, repository: REPOSITORY, name: SWIFT_NAME, version: verified.version, commit: verified.commit, treeDigest: verified.digest,
    changelogDate: verified.changelogDate, approvalRef: verified.approvalRef, ciRun: verified.ci,
    publishRun: { id: env.GITHUB_RUN_ID ?? null, attempt: env.GITHUB_RUN_ATTEMPT ?? null, workflow: env.GITHUB_WORKFLOW_REF ?? null },
    tagged: published, dryRun: verified.dryRun,
  };
  fs.writeFileSync(path.join(directory, "receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
  return receipt;
}

async function main(argv) {
  const [mode, ...rest] = argv;
  if (mode === "digest") {
    need(rest.length === 2 && rest[0] === "--commit", "usage: publish-swift.mjs digest --commit <40-hex sha>");
    process.stdout.write(`${treeDigest(gitRunner(ROOT), rest[1])}\n`);
    return;
  }
  need((mode === "verify" || mode === "tag") && rest.length === 0, "usage: publish-swift.mjs verify | tag | digest --commit <sha> (inputs arrive through the environment)");
  const verified = await verifyAll({ env: process.env });
  const commands = tagCommands(verified.version, verified.commit);
  if (mode === "verify") {
    for (const [command, args] of commands) process.stdout.write(`[publish-swift] ${verified.dryRun ? "dry-run would run" : "will run"}: ${command} ${args.join(" ")}\n`);
    process.stdout.write(`[publish-swift] verified ${SWIFT_NAME} ${verified.version} at ${verified.commit} (CI run ${verified.ci.id}, tree ${verified.digest})\n`);
    return;
  }
  need(!verified.dryRun, "tag mode never runs for a dry run");
  const root = ROOT;
  createTag({ git: gitRunner(root), pushGit: gitRunner(root, pushCredential(process.env.GITHUB_TOKEN)), version: verified.version, commit: verified.commit });
  writeReceipt({ env: process.env, verified, published: true });
  process.stdout.write(`[publish-swift] tagged ${verified.version} at ${verified.commit} on https://github.com/${REPOSITORY}\n`);
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main(process.argv.slice(2)).catch((error) => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
}
