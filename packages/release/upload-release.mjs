#!/usr/bin/env node
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { prepareGitHubRelease, publishGitHubRelease } from "./github-release.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { verifySealedRelease } from "./release-state.mjs";
import { assertCleanSource } from "./source-gate.mjs";

const args = process.argv.slice(2);
let platform = process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform;
let releaseId = "";
let tier = "";
let repo = "";
let configName = "right-release.config.mjs";
let dryRun = false;

for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--platform") platform = args[++index];
  else if (arg === "--release") releaseId = args[++index];
  else if (arg === "--tier") tier = args[++index];
  else if (arg.startsWith("--tier=")) tier = arg.slice("--tier=".length);
  else if (arg === "--repo") repo = args[++index];
  else if (arg === "--config") configName = args[++index];
  else if (arg === "--dry-run") dryRun = true;
  else if (arg === "-h" || arg === "--help") usage(0);
  else fail(`unknown argument: ${arg}`);
}
if (tier !== "patch" && tier !== "update") fail("tier is required (patch|update)");
if (!releaseId) fail("--release is required");
if (platform !== "win" && platform !== "mac") fail("--platform must be win or mac");

const repoRoot = git(process.cwd(), ["rev-parse", "--show-toplevel"]);
assertPrimaryReleaseCheckout(repoRoot);
const candidateCommit = git(repoRoot, ["rev-parse", "HEAD"]);
assertCleanSource({
  status: git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
  commandId: "right-release upload",
});
const platformDir = platform === "win" ? "windows" : "mac";
const sealed = verifySealedRelease(path.join(repoRoot, ".right-release", "sealed", releaseId, platformDir));
if (sealed.manifest.commit !== candidateCommit) fail("sealed release was built from a different commit than the current checkout");

const configPath = path.resolve(configName);
if (!existsSync(configPath)) fail(`release config is required: ${configName}`);
const releaseConfig = (await import(`${pathToFileURL(configPath).href}?upload=${Date.now()}`)).default;
if (releaseConfig?.distribution?.provider !== "github-releases") {
  fail("release config distribution.provider must be github-releases");
}
const configuredRepo = releaseConfig?.distribution?.repository ?? "";
if (!configuredRepo) fail("release config distribution.repository is required for github-releases");
if (repo && repo !== configuredRepo) fail(`--repo must match configured distribution.repository (${configuredRepo})`);
repo = configuredRepo;

const stateRoot = path.join(repoRoot, ".right-release", "state", releaseId, platformDir);
const verifiedMarker = path.join(stateRoot, `verified-${tier}-github-releases.json`);
if (existsSync(verifiedMarker)) {
  console.log(`right-release upload: already verified ${releaseId} tier=${tier} provider=github-releases`);
  process.exit(0);
}
const plan = prepareGitHubRelease({ repoRoot, releaseId, platform, repo });
const result = publishGitHubRelease(plan, { repo, dryRun });
if (!dryRun) writeJson(verifiedMarker, {
  schema: 1,
  releaseId,
  platform,
  tier,
  provider: "github-releases",
  repository: repo,
  tag: result.tag,
  verifiedAt: new Date().toISOString(),
});
console.log(`right-release upload: ${dryRun ? "dry-run " : ""}${result.status} ${releaseId} tier=${tier} repo=${repo}`);

function git(cwd, runArgs) {
  const result = spawnSync("git", runArgs, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) fail(`git ${runArgs.join(" ")} failed: ${result.stderr}`);
  return result.stdout.trim();
}

function writeJson(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function usage(code) {
  console.log("usage: right-release upload --release <sealed-id> --platform win|mac --tier patch|update [--config right-release.config.mjs] [--repo owner/repo] [--dry-run]");
  process.exit(code);
}

function fail(message) {
  console.error(`right-release upload: ${message}`);
  process.exit(1);
}
