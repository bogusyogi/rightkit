#!/usr/bin/env node
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { verifySealedRelease } from "./release-state.mjs";
import { addonManifestSha256, validateAddonConfig, validateAddonManifest } from "./addon-contract.mjs";

const REPO_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})\/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const RELEASE_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/;
const SHA256 = /^[a-f0-9]{64}$/;

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
  writeFileSync(checksumsAsset, `${sealed.manifest.files.map((file) => `${file.sha256}  ${file.name}`).join("\n")}\n`);
  const installer = path.join(sealed.sealedDir, installers[0].name);
  const assets = [...sealed.manifest.files.map((file) => path.join(sealed.sealedDir, file.name)), manifestAsset, checksumsAsset];
  return {
    sealed,
    installer,
    assets,
    tag: `v${sealed.manifest.version}`,
    title: `${sealed.manifest.app === "cutright" ? "CutRight Studio" : sealed.manifest.app} ${sealed.manifest.version}`,
    notes: [
      `Signed ${platform === "mac" ? "& notarized universal macOS" : "Windows"} installer.`,
      "",
      `Build commit: \`${sealed.manifest.commit}\``,
      `Artifacts: ${sealed.manifest.files.map((file) => `\`${file.name}\``).join(", ")}`,
    ].join("\n"),
    notesFile: path.join(releaseState, "release-notes.md"),
  };
}

/**
 * Build one shared GitHub payload for direct/no-installer releases. The signed
 * release manifest is authoritative; checksums, provenance, SBOM, and archives
 * are attached convenience/evidence payloads and are never uploaded by a
 * product-local publisher.
 */
export function prepareGitHubDirectRelease({
  repoRoot,
  repo,
  product,
  version,
  manifestPath,
  signaturePath,
  checksumsPath,
  archivePaths = [],
  provenancePaths = [],
  sbomPaths = [],
  assets = [],
  stateRoot,
}) {
  if (!REPO_RE.test(repo)) throw new Error(`invalid GitHub repository: ${repo}`);
  const root = path.resolve(repoRoot ?? process.cwd());
  const manifestFile = path.resolve(root, manifestPath);
  const signatureFile = path.resolve(root, signaturePath);
  const checksumsFile = path.resolve(root, checksumsPath);
  for (const file of [manifestFile, signatureFile, checksumsFile]) {
    if (!existsSync(file) || !statSync(file).isFile()) throw new Error(`direct release payload missing: ${file}`);
  }
  const manifest = JSON.parse(readFileSync(manifestFile, "utf8"));
  if (manifest.schemaVersion !== 1 || manifest.kind !== "rightkit-direct-release-manifest") throw new Error("invalid direct release manifest");
  if (product && manifest.product !== product) throw new Error("direct release product mismatch");
  if (version && manifest.version !== version) throw new Error("direct release version mismatch");
  if (!RELEASE_RE.test(manifest.version) || manifest.tag !== `v${manifest.version}`) throw new Error("direct release manifest tag is invalid");
  const declared = new Set((manifest.assets ?? []).map((asset) => asset.name));
  if (!declared.size) throw new Error("direct release manifest has no archive assets");
  if ([...declared].some((name) => /(?:installer|setup)\.(?:exe|dmg|pkg|msi)$/i.test(name))) throw new Error("direct release must not contain installer assets");
  const local = [
    ...archivePaths,
    ...provenancePaths,
    ...sbomPaths,
    ...assets,
    manifestFile,
    signatureFile,
    checksumsFile,
  ].map((value) => typeof value === "string" ? value : value?.path ?? value?.file).filter(Boolean).map((file) => path.resolve(root, file));
  const unique = new Map();
  for (const file of local) {
    if (!existsSync(file) || !statSync(file).isFile()) throw new Error(`direct release payload missing: ${file}`);
    if (unique.has(path.basename(file)) && unique.get(path.basename(file)) !== file) throw new Error(`duplicate direct release payload name: ${path.basename(file)}`);
    unique.set(path.basename(file), file);
  }
  const requiredEvidence = ["release-manifest.json", "release-manifest.sig", "checksums.json"];
  for (const name of requiredEvidence) if (![...unique.keys()].some((candidate) => candidate === name || candidate.endsWith(`-${name}`))) throw new Error(`direct release payload missing ${name}`);
  for (const name of declared) if (!unique.has(name)) throw new Error(`direct release archive is not attached: ${name}`);
  const releaseState = stateRoot ?? path.join(root, ".right-release", "state", "direct", manifest.product, manifest.version, "github");
  mkdirSync(releaseState, { recursive: true });
  const notesFile = path.join(releaseState, "release-notes.md");
  const payload = [...unique.values()];
  const plan = {
    kind: "direct-release",
    manifest,
    manifestPath: manifestFile,
    signaturePath: signatureFile,
    checksumsPath: checksumsFile,
    assets: payload,
    tag: manifest.tag,
    title: `${manifest.product} ${manifest.version}`,
    notes: [`Direct portable ${manifest.product} release.`, "", `Build commit: \`${manifest.sourceCommit}\``, "Manifest, detached signature, checksums, provenance, SBOM, and archives are attached."].join("\n"),
    notesFile,
    manifestUrl: `https://github.com/${repo}/releases/download/${manifest.tag}/${path.basename(manifestFile)}`,
  };
  validateGitHubDirectReleasePlan(plan);
  return plan;
}

export const prepareGitHubPortableRelease = prepareGitHubDirectRelease;
export const prepareGitHubManifestRelease = prepareGitHubDirectRelease;

/**
 * Recheck every direct-release payload immediately before publication. The
 * signed manifest remains authority; checksums.json is only a digest-bound
 * convenience index for archives and attached provenance/SBOM evidence.
 */
export function validateGitHubDirectReleasePlan(plan) {
  if (!plan || plan.kind !== "direct-release") throw new Error("invalid direct GitHub release plan");
  if (!plan.manifestPath || !plan.signaturePath || !plan.checksumsPath || !Array.isArray(plan.assets)) throw new Error("direct GitHub release plan is incomplete");
  const manifestFile = path.resolve(plan.manifestPath);
  const signatureFile = path.resolve(plan.signaturePath);
  const checksumsFile = path.resolve(plan.checksumsPath);
  for (const file of [manifestFile, signatureFile, checksumsFile]) {
    if (!existsSync(file) || !statSync(file).isFile()) throw new Error(`direct release payload missing: ${file}`);
  }
  if (statSync(signatureFile).size < 1) throw new Error("detached release manifest signature is missing");
  const manifest = JSON.parse(readFileSync(manifestFile, "utf8"));
  if (manifest.schemaVersion !== 1 || manifest.kind !== "rightkit-direct-release-manifest" || !RELEASE_RE.test(manifest.version ?? "") || manifest.tag !== `v${manifest.version}`) {
    throw new Error("invalid direct release manifest");
  }
  if (plan.tag !== manifest.tag) throw new Error("direct GitHub release tag does not match manifest");
  if (!SHA256.test(manifest.checksumsSha256 ?? "")) throw new Error("direct release checksums digest is missing");
  if (sha256(checksumsFile) !== manifest.checksumsSha256) throw new Error("checksums.json is not manifest-bound");
  const checksums = JSON.parse(readFileSync(checksumsFile, "utf8"));
  if (checksums.schemaVersion !== 1 || checksums.algorithm !== "sha256" || !checksums.assets || typeof checksums.assets !== "object" || Array.isArray(checksums.assets)) {
    throw new Error("invalid direct release checksums");
  }
  for (const [name, digest] of Object.entries(checksums.assets)) if (!SHA256.test(digest)) throw new Error(`invalid checksum entry: ${name}`);
  const payloadByName = new Map();
  for (const value of plan.assets) {
    const file = path.resolve(typeof value === "string" ? value : value?.path ?? value?.file ?? "");
    if (!file || !existsSync(file) || !statSync(file).isFile()) throw new Error(`direct release payload missing: ${file}`);
    const name = path.basename(file);
    if (payloadByName.has(name) && payloadByName.get(name) !== file) throw new Error(`duplicate direct release payload name: ${name}`);
    payloadByName.set(name, file);
  }
  const requiredEvidence = ["release-manifest.json", "release-manifest.sig", "checksums.json"];
  for (const name of requiredEvidence) if (![...payloadByName.keys()].some((candidate) => candidate === name || candidate.endsWith(`-${name}`))) throw new Error(`direct release payload missing ${name}`);
  for (const file of [manifestFile, signatureFile, checksumsFile]) if (![...payloadByName.values()].includes(file)) throw new Error(`direct release authority file is not attached: ${path.basename(file)}`);
  const expected = new Map();
  const addExpected = (entry, role) => {
    if (!entry || typeof entry.name !== "string" || !SHA256.test(entry.sha256 ?? "")) throw new Error(`invalid direct release ${role} evidence`);
    if (expected.has(entry.name)) throw new Error(`duplicate direct release evidence name: ${entry.name}`);
    expected.set(entry.name, { digest: entry.sha256, size: role === "archive" ? entry.size : undefined });
  };
  if (!Array.isArray(manifest.assets) || !manifest.assets.length) throw new Error("direct release manifest has no archive assets");
  for (const asset of manifest.assets) {
    if (/(?:installer|setup)\.(?:exe|dmg|pkg|msi)$/i.test(asset.name ?? "")) throw new Error("direct release must not contain installer assets");
    addExpected(asset, "archive");
    addExpected({ name: asset.provenanceName, sha256: asset.provenanceSha256 }, "provenance");
    addExpected({ name: asset.sbomName, sha256: asset.sbomSha256 }, "SBOM");
  }
  for (const [name, evidence] of expected) {
    const file = payloadByName.get(name);
    if (!file) throw new Error(`direct release evidence is not attached: ${name}`);
    if (evidence.size !== undefined && (!Number.isSafeInteger(evidence.size) || evidence.size < 1 || statSync(file).size !== evidence.size)) throw new Error(`direct release archive size mismatch: ${name}`);
    if (sha256(file) !== evidence.digest) throw new Error(`direct release payload hash mismatch: ${name}`);
    if (checksums.assets[name] !== evidence.digest) throw new Error(`direct release checksum mismatch: ${name}`);
  }
  return { ...plan, manifest, checksums };
}

export function repositoryFromRemote(repoRoot) {
  const remote = runCommand("git", ["remote", "get-url", "origin"], { cwd: repoRoot });
  const match = remote.trim().match(/github\.com[:/]([^/]+\/[^/]+?)(?:\.git)?$/i);
  if (!match || !REPO_RE.test(match[1])) throw new Error(`origin is not a GitHub repository: ${remote.trim()}`);
  return match[1];
}

export function prepareGitHubAddonRelease({ repoRoot, config, configRoot = repoRoot, platform, repo }) {
  if (platform !== "mac" && platform !== "win") throw new Error("platform must be mac or win");
  if (!REPO_RE.test(repo)) throw new Error(`invalid GitHub repository: ${repo}`);
  validateAddonConfig(config, { root: configRoot, platform, allowMissingExecutables: true });
  const commit = runCommand("git", ["rev-parse", "HEAD"], { cwd: repoRoot });
  const sealedDir = path.join(repoRoot, ".right-release", "addons", config.addon, config.version, commit.slice(0, 8), platform);
  const manifestPath = path.join(sealedDir, "addon-manifest.json");
  if (!existsSync(manifestPath)) throw new Error(`sealed add-on manifest not found: ${manifestPath}`);
  const manifest = validateAddonManifest(JSON.parse(readFileSync(manifestPath, "utf8")));
  if (manifest.addon !== config.addon || manifest.version !== config.version || manifest.platform !== platform || manifest.commit !== commit) throw new Error("sealed add-on identity mismatch");
  const assets = manifest.files.map((file) => {
    const asset = path.join(sealedDir, file.name);
    if (!existsSync(asset) || statSync(asset).size !== file.sizeBytes || sha256(asset) !== file.sha256) throw new Error(`sealed add-on file mismatch: ${file.name}`);
    return asset;
  });
  const digest = addonManifestSha256(manifest);
  const tag = `addon-${manifest.addon}-v${manifest.version}-${platform}-sha256-${digest}`;
  const stateDir = path.join(repoRoot, ".right-release", "state", "addons", manifest.addon, manifest.version, commit.slice(0, 8), platform, "github");
  mkdirSync(stateDir, { recursive: true });
  return {
    kind: "addon", manifest, manifestPath, sealedDir, assets: [...assets, manifestPath], tag,
    title: `${manifest.addon} ${manifest.version} ${platform} add-on`,
    notes: [`Signed ${platform === "mac" ? "macOS" : "Windows"} add-on component set.`, "", `Build commit: \`${manifest.commit}\``, `Manifest SHA-256: \`${digest}\``].join("\n"),
    notesFile: path.join(stateDir, "release-notes.md"),
    manifestUrl: `https://github.com/${repo}/releases/download/${tag}/addon-manifest.json`,
  };
}

export function publishGitHubRelease(plan, { repo, dryRun = false, run = runCommand } = {}) {
  if (plan.kind === "direct-release") validateGitHubDirectReleasePlan(plan);
  const visibility = JSON.parse(run("gh", ["repo", "view", repo, "--json", "visibility"]));
  if (visibility.visibility !== "PUBLIC" && !(plan.kind !== "addon" && visibility.visibility === "PRIVATE")) {
    throw new Error(`GitHub releases require a public or private repository: ${repo}`);
  }
  if (!dryRun && plan.kind === "direct-release") {
    // Direct payloads are already sealed by the signed manifest contract. Do
    // not route them through desktop-installer trust checks.
  } else if (!dryRun) {
    if (plan.kind === "addon") verifyAddonTrust(plan);
    else verifyPlatformTrust(plan);
  }
  const existing = run("gh", ["release", "view", plan.tag, "--repo", repo, "--json", "tagName"], { allowFailure: true });
  if (dryRun) return { status: existing.ok ? plan.kind === "addon" ? "would-verify" : "would-update" : "would-create", tag: plan.tag, assets: plan.assets };
  if (existing.ok) {
    const missing = plan.assets.filter((asset) => inspectRemoteAsset(plan.tag, repo, asset, run) === "missing");
    if (missing.length === 0) return { status: "already-verified", tag: plan.tag, assets: plan.assets, manifestUrl: plan.manifestUrl };
    run("gh", ["release", "upload", plan.tag, ...missing, "--repo", repo]);
    for (const asset of plan.assets) verifyRemoteAsset(plan.tag, repo, asset, run);
    return { status: "resumed-verified", tag: plan.tag, assets: plan.assets, manifestUrl: plan.manifestUrl };
  }
  if (!existing.ok) {
    const notesFile = plan.notesFile;
    writeFileSync(notesFile, `${plan.notes}\n`);
    run("gh", ["release", "create", plan.tag, "--repo", repo, "--title", plan.title, "--notes-file", notesFile]);
    rmSync(notesFile, { force: true });
  }
  run("gh", ["release", "upload", plan.tag, ...plan.assets, "--repo", repo]);
  for (const asset of plan.assets) verifyRemoteAsset(plan.tag, repo, asset, run);
  return { status: "verified", tag: plan.tag, assets: plan.assets, manifestUrl: plan.manifestUrl };
}

function verifyAddonTrust(plan) {
  for (const file of plan.manifest.files.filter((entry) => entry.executable)) {
    const executable = path.join(plan.sealedDir, file.name);
    if (plan.manifest.platform === "mac") {
      const output = runCombined("codesign", ["-dv", "--verbose=4", executable]);
      if (!/TeamIdentifier=6KLGD3LLKF/.test(output)) throw new Error(`codesign team mismatch: ${file.name}`);
      runCommand("codesign", ["--verify", "--strict", "--verbose=2", executable]);
    } else runCommand(process.execPath, [path.join(path.dirname(fileURLToPath(import.meta.url)), "sign-windows.mjs"), "--verify-only", executable]);
  }
}

function verifyPlatformTrust(plan) {
  if (plan.sealed.manifest.platform === "win") {
    runCommand(process.execPath, [path.join(path.dirname(fileURLToPath(import.meta.url)), "sign-windows.mjs"), "--verify-only", plan.installer]);
    return;
  }
  runCommand("spctl", ["--assess", "--type", "open", "--context", "context:primary-signature", "--verbose=2", plan.installer]);
  runCommand("xcrun", ["stapler", "validate", plan.installer]);
}

function verifyRemoteAsset(tag, repo, asset, run) {
  if (inspectRemoteAsset(tag, repo, asset, run) !== "match") throw new Error(`GitHub release asset missing: ${path.basename(asset)}`);
}

function inspectRemoteAsset(tag, repo, asset, run) {
  const verifyDir = `${path.dirname(asset)}/verify-${path.basename(asset)}`;
  rmSync(verifyDir, { recursive: true, force: true });
  mkdirSync(verifyDir, { recursive: true });
  const result = run("gh", ["release", "download", tag, "--repo", repo, "--pattern", path.basename(asset), "--dir", verifyDir], { allowFailure: true });
  if (!result.ok) {
    rmSync(verifyDir, { recursive: true, force: true });
    if (/no assets match|not found/i.test(`${result.output ?? ""}\n${result.error ?? ""}`)) return "missing";
    throw new Error(`cannot inspect GitHub release asset: ${path.basename(asset)}: ${result.error || result.output}`);
  }
  const downloaded = path.join(verifyDir, path.basename(asset));
  if (!existsSync(downloaded) || sha256(downloaded) !== sha256(asset)) throw new Error(`GitHub release asset hash mismatch: ${path.basename(asset)}`);
  rmSync(verifyDir, { recursive: true, force: true });
  return "match";
}

function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function runCommand(command, args, { allowFailure = false, cwd } = {}) {
  const result = spawnSync(command, args, { cwd, encoding: "utf8", windowsHide: true });
  const output = result.stdout?.trim() ?? "";
  if (result.status !== 0) {
    if (allowFailure) return { ok: false, output, error: result.stderr?.trim() ?? "" };
    throw new Error(`${command} ${args.join(" ")} failed: ${result.stderr?.trim() || `exit ${result.status}`}`);
  }
  return allowFailure ? { ok: true, output } : output;
}

function runCombined(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed: ${result.stderr?.trim() || `exit ${result.status}`}`);
  return `${result.stdout ?? ""}${result.stderr ?? ""}`;
}

function usage(code) {
  console.log("usage: right-release github (--release <sealed-id> | --addon <config>) --platform mac|win [--repo owner/repo] [--dry-run]");
  process.exit(code);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const options = { platform: process.platform === "win32" ? "win" : "mac", releaseId: "", addon: "", repo: "", dryRun: false };
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--platform") options.platform = args[++index];
    else if (args[index] === "--release") options.releaseId = args[++index];
    else if (args[index] === "--addon") options.addon = args[++index];
    else if (args[index] === "--repo") options.repo = args[++index];
    else if (args[index] === "--dry-run") options.dryRun = true;
    else if (args[index] === "-h" || args[index] === "--help") usage(0);
    else throw new Error(`unknown argument: ${args[index]}`);
  }
  if (Boolean(options.releaseId) === Boolean(options.addon)) usage(2);
  const repoRoot = runCommand("git", ["rev-parse", "--show-toplevel"]);
  let plan;
  if (options.addon) {
    const configPath = path.resolve(options.addon);
    const config = (await import(`${pathToFileURL(configPath).href}?github=${Date.now()}`)).default;
    options.repo ||= config.distribution?.repository ?? "";
    if (config.distribution?.provider !== "github-releases") throw new Error("add-on config must select github-releases distribution");
    plan = prepareGitHubAddonRelease({ repoRoot, config, configRoot: path.dirname(configPath), ...options });
  } else {
    options.repo ||= repositoryFromRemote(repoRoot);
    plan = prepareGitHubRelease({ repoRoot, ...options });
  }
  const result = publishGitHubRelease(plan, options);
  console.log(`right-release github: ${result.status} ${result.tag}`);
  if (result.manifestUrl) console.log(`manifestUrl: ${result.manifestUrl}`);
}
