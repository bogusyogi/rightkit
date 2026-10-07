#!/usr/bin/env node
// Run only from an externally managed, OS-protected runner. Repository
// workflows have no path to this App key or protected configuration.
import { createPrivateKey, sign as cryptoSign } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { auditPublicProtectedInstall, auditProtectedInstallForTest } from "./private-local.mjs";
import { sourceTreeDigest } from "./receipt.mjs";
import { nativeGitExecutable, protectedChildEnvironment } from "./runtime-environment.mjs";

const INSTALL_ROOTS = Object.freeze({
  win32: "C:\\Program Files\\Orthic Labs\\RightKit",
  darwin: "/Library/Application Support/Orthic Labs/RightKit",
});
const PRODUCER_PATHS = Object.freeze(Object.fromEntries(Object.entries(INSTALL_ROOTS).map(([platform, root]) => [platform, path.join(root, "ownership", "adapters", "public-check-producer.mjs")])));
const APP_CONFIG_PATHS = Object.freeze({
  win32: path.join(INSTALL_ROOTS.win32, "ownership", "public-check.json"),
  darwin: path.join(INSTALL_ROOTS.darwin, "ownership", "public-check.json"),
});
const APP_KEY_PATHS = Object.freeze({
  win32: path.join(INSTALL_ROOTS.win32, "ownership", "public-check-app.pem"),
  darwin: path.join(INSTALL_ROOTS.darwin, "ownership", "public-check-app.pem"),
});
const CHECK_NAME = "RightKit protected ownership";
const API = "https://api.github.com";

function arg(args, key) { const i = args.indexOf(key); return i < 0 ? undefined : args[i + 1]; }
function git(root, args) {
  const result = spawnSync(nativeGitExecutable(), ["-c", "core.fsmonitor=false", "-C", root, ...args], { encoding: "buffer", windowsHide: true, timeout: 15_000, env: protectedChildEnvironment() });
  if (result?.status !== 0) throw new Error(`git ${args.join(" ")} failed`);
  return Buffer.isBuffer(result.stdout) ? result.stdout : Buffer.from(result.stdout ?? "");
}
function canonicalIdentity(value) {
  const scp = value.match(/^([^@/:]+)@([^:]+):(.+)$/);
  const url = new URL(scp ? `ssh://${scp[2]}/${scp[3]}` : value);
  if (!["https:", "ssh:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error("unsupported repository identity URL");
  let repo = url.pathname.replace(/^\/+/, "").replace(/\/+$/, "").replace(/\.git$/i, "");
  if (url.hostname.toLowerCase() === "github.com") repo = repo.toLowerCase();
  if (!repo || repo.split("/").some((part) => !part || part === "." || part === "..")) throw new Error("invalid repository identity path");
  return `https://${url.hostname.toLowerCase()}${url.port ? `:${url.port}` : ""}/${repo}`;
}
function decodeRepo(identity) {
  const url = new URL(identity);
  if (url.hostname !== "github.com") throw new Error("public check producer supports github.com repositories only");
  const parts = url.pathname.slice(1).split("/");
  if (parts.length !== 2 || parts.some((part) => !part)) throw new Error("repository identity must name one GitHub owner/repository");
  return { owner: parts[0], repo: parts[1] };
}
function base64url(value) { return Buffer.from(value).toString("base64url"); }
function appJwt(appId, privatePem) {
  const now = Math.floor(Date.now() / 1000);
  const header = base64url(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const payload = base64url(JSON.stringify({ iat: now - 30, exp: now + 8 * 60, iss: appId }));
  const input = `${header}.${payload}`;
  const signature = cryptoSign("RSA-SHA256", Buffer.from(input), createPrivateKey(privatePem)).toString("base64url");
  return `${input}.${signature}`;
}
function validateAppConfig(value) {
  const service = value?.service;
  const validSid = (sid) => typeof sid === "string" && /^S-1-\d+(?:-\d+)+$/.test(sid);
  if (!value || value.schema !== 1 || !Number.isSafeInteger(value.appId) || value.appId <= 0 || !Number.isSafeInteger(value.installationId) || value.installationId <= 0 || value.checkName !== CHECK_NAME
    || !service || !validSid(service.windowsSid) || !Array.isArray(service.agentWindowsSids) || !service.agentWindowsSids.length || service.agentWindowsSids.some((sid) => !validSid(sid)) || service.agentWindowsSids.includes(service.windowsSid)
    || typeof service.windowsCheckoutRoot !== "string" || !path.win32.isAbsolute(service.windowsCheckoutRoot)
    || typeof service.macosCheckoutRoot !== "string" || !path.posix.isAbsolute(service.macosCheckoutRoot)
    || !Number.isSafeInteger(service.macosUid) || service.macosUid <= 0 || !Array.isArray(service.agentMacosUids) || !service.agentMacosUids.length || service.agentMacosUids.some((uid) => !Number.isSafeInteger(uid) || uid <= 0) || service.agentMacosUids.includes(service.macosUid)) {
    throw new Error("protected GitHub App check configuration is malformed or lacks distinct service/agent identities");
  }
  return value;
}
async function api(url, token, options = {}) {
  const response = await fetch(url, {
    signal: AbortSignal.timeout(20_000),
    ...options,
    headers: { Accept: "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28", ...(token ? { Authorization: `Bearer ${token}` } : {}), ...(options.headers ?? {}) },
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(`GitHub API ${response.status}: ${body.message ?? "request failed"}`);
  return body;
}

async function publishPublicOwnershipCheckImpl({ repoRoot, expectedCommit, platform = process.platform, audit, runner = spawnSync, scanRunner = spawnSync, apiCall = api, configPath = APP_CONFIG_PATHS[platform], keyPath = APP_KEY_PATHS[platform], scanner, trustModule } = {}) {
  const installed = audit();
  if (installed?.ok !== true) throw Object.assign(new Error((installed?.problems ?? ["protected install audit failed"]).join("; ")), { type: "protected-install-invalid" });
  const root = path.resolve(repoRoot ?? "");
  const commit = git(root, ["rev-parse", "HEAD"]).toString("utf8").trim();
  const tree = git(root, ["rev-parse", `${commit}^{tree}`]).toString("utf8").trim();
  if (commit !== expectedCommit) throw Object.assign(new Error("checkout does not match externally requested source commit"), { type: "source-commit-mismatch" });
  if (git(root, ["status", "--porcelain=v1", "--untracked-files=all"]).toString("utf8").trim()) throw Object.assign(new Error("protected verifier checkout must be clean"), { type: "source-dirty" });
  const identity = canonicalIdentity(git(root, ["remote", "get-url", "origin"]).toString("utf8").trim());
  const repo = decodeRepo(identity);
  const sourceDigest = sourceTreeDigest(git(root, ["ls-tree", "-rz", "--full-tree", commit]));
  const config = validateAppConfig(JSON.parse(readFileSync(configPath, "utf8")));
  const privatePem = readFileSync(keyPath, "utf8");
  const trusted = trustModule ?? await import(new URL("../../packages/release/ownership-approval.mjs", import.meta.url));
  const trustState = trusted.loadTrustedExternal();
  const registration = trusted.verifyRepositoryRegistration(trustState.trust, identity);
  if (!registration.ok || registration.registration.visibility !== "public") throw Object.assign(new Error(registration.problems.join("; ") || "repository is not registered public"), { type: "protected-registration-rejected" });

  let scan;
  let scanError;
  let conclusion = "success";
  let summary;
  try {
    if (scanner) scan = await scanner(root);
    else scan = runProtectedScan(root, expectedCommit, scanRunner);
    if (!scan.ok) throw Object.assign(new Error(scan.error?.message ?? "protected ownership scan, coverage, or floor failed"), { type: scan.error?.type ?? "protected-scan-failed" });
    if (scan.evidence.commit !== commit || scan.evidence.tree !== tree || scan.evidence.repositoryIdentity !== identity || scan.evidence.visibility !== "public") throw new Error("scanner evidence does not match requested public source identity");
    summary = `Protected ownership scan passed for ${identity} at ${commit}. Catalog ${scan.evidence.catalogDigest}; source tree ${sourceDigest}; roots ${scan.evidence.registeredRoots.join(", ")}.`;
  } catch (error) {
    conclusion = "failure";
    scanError = { type: error.type ?? "protected-scan-failed", message: error.message };
    summary = `Protected ownership scan failed for ${identity} at ${commit}: ${error.message}`;
  }

  const jwt = appJwt(config.appId, privatePem);
  const tokenRecord = await apiCall(`${API}/app/installations/${config.installationId}/access_tokens`, jwt, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
  if (typeof tokenRecord.token !== "string" || !tokenRecord.token) throw new Error("GitHub App token response was malformed");
  const token = tokenRecord.token;
  const created = await apiCall(`${API}/repos/${encodeURIComponent(repo.owner)}/${encodeURIComponent(repo.repo)}/check-runs`, token, {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: CHECK_NAME, head_sha: commit, status: "completed", conclusion, started_at: new Date().toISOString(), completed_at: new Date().toISOString(), output: { title: conclusion === "success" ? "Protected ownership verified" : "Protected ownership failed", summary } }),
  });
  if (created.app?.id !== config.appId || created.head_sha !== commit || created.name !== CHECK_NAME || created.status !== "completed" || created.conclusion !== conclusion) throw new Error("GitHub returned a check run without expected App authority or source binding");
  const readback = await apiCall(`${API}/repos/${encodeURIComponent(repo.owner)}/${encodeURIComponent(repo.repo)}/commits/${commit}/check-runs?check_name=${encodeURIComponent(CHECK_NAME)}`, token);
  const observed = (readback.check_runs ?? []).find((run) => run.id === created.id);
  if (!observed || observed.app?.id !== config.appId || observed.head_sha !== commit || observed.name !== CHECK_NAME || observed.status !== "completed" || observed.conclusion !== conclusion) throw new Error("GitHub check readback did not preserve protected App and commit binding");
  return { ok: conclusion === "success", repositoryIdentity: identity, commit, tree, sourceDigest, appId: config.appId, checkName: CHECK_NAME, checkRunId: created.id, conclusion, ...(scanError ? { error: scanError } : {}) };
}

function runProtectedScan(repoRoot, expectedCommit, scanRunner = spawnSync) {
  const entry = fileURLToPath(import.meta.url);
  const result = scanRunner(process.execPath, [entry, "--scan-only", "--repo-root", repoRoot, "--expected-commit", expectedCommit], { encoding: "utf8", windowsHide: true, timeout: 120_000, maxBuffer: 4 * 1024 * 1024, env: protectedChildEnvironment() });
  if (result?.error?.code === "ETIMEDOUT" || result?.signal === "SIGTERM") throw Object.assign(new Error("protected scanner exceeded 120 second timeout"), { type: "protected-scan-timeout" });
  let response;
  try { response = JSON.parse(result.stdout.trim()); }
  catch { throw Object.assign(new Error("protected scanner returned malformed output"), { type: "protected-scan-output-invalid" }); }
  if (result?.status !== 0) throw Object.assign(new Error(response?.error?.message ?? "fixed protected scanner process failed"), { type: response?.error?.type ?? "protected-scan-process-failed" });
  if (response?.ok !== true && response?.ok !== false) throw Object.assign(new Error("protected scanner response is malformed"), { type: "protected-scan-output-invalid" });
  return response;
}

async function scanPublicOwnershipSource(repoRoot, expectedCommit) {
  const commit = git(repoRoot, ["rev-parse", "HEAD"]).toString("utf8").trim();
  if (commit !== expectedCommit) throw Object.assign(new Error("checkout does not match externally requested source commit"), { type: "source-commit-mismatch" });
  const trusted = await import(new URL("../../packages/release/ownership-approval.mjs", import.meta.url));
  const trustState = trusted.loadTrustedExternal();
  const identity = canonicalIdentity(git(repoRoot, ["remote", "get-url", "origin"]).toString("utf8").trim());
  const registration = trusted.verifyRepositoryRegistration(trustState.trust, identity);
  if (!registration.ok || registration.registration.visibility !== "public") throw Object.assign(new Error(registration.problems.join("; ") || "repository is not registered public"), { type: "protected-registration-rejected" });
  const evidenceModule = await import(new URL("../../packages/release/ownership-evidence.mjs", import.meta.url));
  const scan = evidenceModule.createOwnershipEvidence(repoRoot, repoRoot);
  if (!scan.ok) throw Object.assign(new Error("protected ownership scan, coverage, or floor failed"), { type: "protected-scan-failed" });
  const tree = git(repoRoot, ["rev-parse", `${commit}^{tree}`]).toString("utf8").trim();
  if (scan.evidence.commit !== commit || scan.evidence.tree !== tree || scan.evidence.repositoryIdentity !== identity || scan.evidence.visibility !== "public") throw Object.assign(new Error("scanner evidence does not match requested public source identity"), { type: "source-identity-mismatch" });
  return { ok: true, evidence: scan.evidence };
}

export function publishPublicOwnershipCheck({ repoRoot, expectedCommit } = {}) {
  return publishPublicOwnershipCheckImpl({ repoRoot, expectedCommit, audit: () => auditPublicProtectedInstall({ repoRoot }) });
}

export function publishPublicOwnershipCheckForTest(options) {
  const { platform, audit, ...rest } = options;
  const injectedAudit = audit ?? (() => auditProtectedInstallForTest({ platform, root: options.auditRoot ?? INSTALL_ROOTS[platform], entry: PRODUCER_PATHS[platform], runner: options.auditRunner, ...(options.auditOptions ?? {}) }));
  return publishPublicOwnershipCheckImpl({ ...rest, audit: injectedAudit });
}

async function main(args) {
  try {
    if (args.includes("--scan-only")) {
      const result = await scanPublicOwnershipSource(arg(args, "--repo-root"), arg(args, "--expected-commit"));
      process.stdout.write(`${JSON.stringify(result)}\n`);
      process.exitCode = 0;
      return;
    }
    const result = await publishPublicOwnershipCheck({ repoRoot: arg(args, "--repo-root"), expectedCommit: arg(args, "--expected-commit") });
    process.stdout.write(`${JSON.stringify(result)}\n`);
    process.exitCode = result.ok ? 0 : 1;
  } catch (error) {
    process.stdout.write(`${JSON.stringify({ ok: false, error: { type: error.type ?? "public-check-failed", message: error.message } })}\n`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2));

export function runProtectedScanForTest(repoRoot, expectedCommit, scanRunner) { return runProtectedScan(repoRoot, expectedCommit, scanRunner); }

export const __testOnly = Object.freeze({ validateAppConfig, appJwt, CHECK_NAME, APP_CONFIG_PATHS, APP_KEY_PATHS });
