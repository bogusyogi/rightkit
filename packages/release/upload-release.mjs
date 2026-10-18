#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { prepareGitHubRelease, publishGitHubRelease } from "./github-release.mjs";
import { authorizeGitHubUpload } from "./github-upload-gate.mjs";
import { recordPublishedVersion } from "./artifact-storage.mjs";
import { admitInstalledQa } from "./release-qa-audit.mjs";

const args = process.argv.slice(2);
let platform = process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform;
let releaseId = "";
let tier = "";
let repo = "";
let configName = "right-release.config.mjs";
let dryRun = false;
let apply = false;

for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--platform") platform = args[++index];
  else if (arg === "--release") releaseId = args[++index];
  else if (arg === "--tier") tier = args[++index];
  else if (arg.startsWith("--tier=")) tier = arg.slice("--tier=".length);
  else if (arg === "--repo") repo = args[++index];
  else if (arg === "--config") configName = args[++index];
  else if (arg === "--dry-run") dryRun = true;
  else if (arg === "--apply") apply = true;
  else if (arg === "-h" || arg === "--help") usage(0);
  else fail(`unknown argument: ${arg}`);
}

// Android: the Google Play internal track, with its own gate and a dry run by default (android-play-upload.mjs). No tier: the
// internal track has no patch/update feed.
if (platform === "android") {
  const { main } = await import("./android-play-upload.mjs");
  process.exit(await main({ releaseId, configName, apply, dryRun }));
}
// iOS: TestFlight through the App Store Connect build-upload API (in process, no key file), with its own gate and a dry run by default (ios-upload.mjs).
if (platform === "ios") {
  const { main } = await import("./ios-upload.mjs");
  process.exit(await main({ releaseId, configName, apply, dryRun }));
}
if (apply) fail("--apply is Android and iOS only; win and mac preview with --dry-run");

// Tier, primary checkout, clean tree, sealed commit, ownership evidence, provider and repository are checked by
// the gate that `right-release github --release` also uses; nothing below runs until it passes.
let authority;
try {
  authority = await authorizeGitHubUpload({ command: "right-release upload", tier, releaseId, platform, configName, repo });
} catch (error) {
  fail(error.message);
}
const { repoRoot, platformDir, sealed, configPath } = authority;
repo = authority.repo;

// Hardening step 4: installed QA is recorded, not enforced; a missing receipt warns and the outcome is written to state.
if (!dryRun) {
  admitInstalledQa({ repoRoot, manifest: sealed.manifest, platform, configName: path.relative(process.cwd(), configPath) || configName, releaseConfigSha256: createHash("sha256").update(readFileSync(configPath)).digest("hex") }, { provider: "github-releases" });
}
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
// This runs only after GitHub has returned successful remote byte verification.
// Candidate builds never enter published.json.
if (!dryRun) recordPublishedVersion({ app: sealed.manifest.app, version: sealed.manifest.version, platform });
console.log(`right-release upload: ${dryRun ? "dry-run " : ""}${result.status} ${releaseId} tier=${tier} repo=${repo}`);

function writeJson(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function usage(code) {
  console.log("usage: right-release upload --release <sealed-id> --platform win|mac --tier patch|update [--config right-release.config.mjs] [--repo owner/repo] [--dry-run]\n       right-release upload --platform android --release <sealed-id> [--config right-release.config.mjs] [--apply]   (Play internal track; dry run unless --apply)\n       right-release upload --platform ios --release <sealed-id> [--config right-release.config.mjs] [--apply]   (TestFlight; dry run unless --apply)");
  process.exit(code);
}

function fail(message) {
  console.error(`right-release upload: ${message}`);
  process.exit(1);
}
