#!/usr/bin/env node
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { verifySealedRelease } from "./release-state.mjs";

const REPO_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})\/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const RELEASE_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/;

export function prepareGitHubRelease({ repoRoot, releaseId, platform, repo, stateRoot }) {
  if (!RELEASE_RE.test(releaseId)) throw new Error(`invalid release id: ${releaseId}`);
  if (platform !== "mac" && platform !== "win") throw new Error("platform must be mac or win");
  if (!REPO_RE.test(repo)) throw new Error(`invalid GitHub repository: ${repo}`);
  const platformDir = platform === "win" ? "windows" : "mac";
  const sealed = verifySealedRelease(path.join(repoRoot, ".right-release", "sealed", releaseId, platformDir));
  if (sealed.manifest.platform !== platform) throw new Error("sealed release platform mismatch");
  const installers = sealed.manifest.files.filter((file) => file.role === "installer");
  if (installers.length !== 1) throw new Error(`sealed release must contain exactly one installer; found ${installers.length}`);
  const releaseState = stateRoot ?? path.join(repoRoot, ".right-release", "state", releaseId, platformDir, "github");
  mkdirSync(releaseState, { recursive: true });
  const manifestAsset = path.join(releaseState, `${sealed.manifest.app}-${sealed.manifest.version}-${platform}-release-manifest.json`);
  const checksumsAsset = path.join(releaseState, `${sealed.manifest.app}-${sealed.manifest.version}-${platform}-SHA256SUMS.txt`);
  copyFileSync(sealed.manifestPath, manifestAsset);
  writeFileSync(checksumsAsset, `${installers[0].sha256}  ${installers[0].name}\n`);
  const installer = path.join(sealed.sealedDir, installers[0].name);
  return {
    sealed,
    installer,
    assets: [installer, manifestAsset, checksumsAsset],
    tag: `v${sealed.manifest.version}`,
    title: `${sealed.manifest.app === "cutright" ? "CutRight Studio" : sealed.manifest.app} ${sealed.manifest.version}`,
    notes: [
      `Signed ${platform === "mac" ? "& notarized universal macOS" : "Windows"} installer.`,
      "",
      `Build commit: \`${sealed.manifest.commit}\``,
      `SHA-256: \`${installers[0].sha256}\``,
    ].join("\n"),
  };
}

export function publishGitHubRelease(plan, { repo, dryRun = false, run = runCommand } = {}) {
  const visibility = JSON.parse(run("gh", ["repo", "view", repo, "--json", "visibility"]));
  if (visibility.visibility !== "PUBLIC") throw new Error(`GitHub releases require a public repository: ${repo}`);
  verifyPlatformTrust(plan);
  const existing = run("gh", ["release", "view", plan.tag, "--repo", repo, "--json", "tagName"], { allowFailure: true });
  if (dryRun) return { status: existing.ok ? "would-update" : "would-create", tag: plan.tag, assets: plan.assets };
  if (!existing.ok) {
    const notesFile = `${plan.assets[1]}.notes.md`;
    writeFileSync(notesFile, `${plan.notes}\n`);
    run("gh", ["release", "create", plan.tag, "--repo", repo, "--title", plan.title, "--notes-file", notesFile]);
    rmSync(notesFile, { force: true });
  }
  run("gh", ["release", "upload", plan.tag, ...plan.assets, "--repo", repo, "--clobber"]);
  for (const asset of plan.assets) verifyRemoteAsset(plan.tag, repo, asset, run);
  return { status: "verified", tag: plan.tag, assets: plan.assets };
}

function verifyPlatformTrust(plan) {
  if (plan.sealed.manifest.platform !== "mac") return;
  runCommand("spctl", ["--assess", "--type", "open", "--context", "context:primary-signature", "--verbose=2", plan.installer]);
  runCommand("xcrun", ["stapler", "validate", plan.installer]);
}

function verifyRemoteAsset(tag, repo, asset, run) {
  const verifyDir = `${path.dirname(asset)}/verify-${path.basename(asset)}`;
  rmSync(verifyDir, { recursive: true, force: true });
  mkdirSync(verifyDir, { recursive: true });
  run("gh", ["release", "download", tag, "--repo", repo, "--pattern", path.basename(asset), "--dir", verifyDir]);
  const downloaded = path.join(verifyDir, path.basename(asset));
  if (!existsSync(downloaded) || sha256(downloaded) !== sha256(asset)) throw new Error(`GitHub release asset hash mismatch: ${path.basename(asset)}`);
  rmSync(verifyDir, { recursive: true, force: true });
}

function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function runCommand(command, args, { allowFailure = false } = {}) {
  const result = spawnSync(command, args, { encoding: "utf8", windowsHide: true });
  const output = result.stdout?.trim() ?? "";
  if (result.status !== 0) {
    if (allowFailure) return { ok: false, output, error: result.stderr?.trim() ?? "" };
    throw new Error(`${command} ${args.join(" ")} failed: ${result.stderr?.trim() || `exit ${result.status}`}`);
  }
  return allowFailure ? { ok: true, output } : output;
}

function usage(code) {
  console.log("usage: right-release github --release <sealed-id> --platform mac|win --repo owner/repo [--dry-run]");
  process.exit(code);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const options = { platform: process.platform === "win32" ? "win" : "mac", releaseId: "", repo: "", dryRun: false };
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--platform") options.platform = args[++index];
    else if (args[index] === "--release") options.releaseId = args[++index];
    else if (args[index] === "--repo") options.repo = args[++index];
    else if (args[index] === "--dry-run") options.dryRun = true;
    else if (args[index] === "-h" || args[index] === "--help") usage(0);
    else throw new Error(`unknown argument: ${args[index]}`);
  }
  if (!options.releaseId || !options.repo) usage(2);
  const repoRoot = runCommand("git", ["rev-parse", "--show-toplevel"]);
  const plan = prepareGitHubRelease({ repoRoot, ...options });
  const result = publishGitHubRelease(plan, options);
  console.log(`right-release github: ${result.status} ${result.tag}`);
}
