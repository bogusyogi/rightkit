#!/usr/bin/env node
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { runUploadStateMachine, verifySealedRelease } from "./release-state.mjs";
import { assertCleanOverlay, loadBoundedAdminTrust, readVerificationDocument, verifyPromotionEvidence } from "./verification-ticket.mjs";

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const PUBLISH_UPDATE = path.join(TOOL_ROOT, "publish-update.mjs");
const HARDENING = path.join(TOOL_ROOT, "hardeningscan.mjs");
const SIGN_WINDOWS = path.join(TOOL_ROOT, "sign-windows.mjs");
const UPLOAD = path.join(TOOL_ROOT, "upload-large.mjs");
const PUBLIC_BASE = (process.env.RIGHTAPPS_PUBLIC_DOWNLOAD_BASE || "https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev").replace(/\/$/, "");
const API_BASE = (process.env.RIGHTAPPS_API_URL || "https://api.spoares.com").replace(/\/$/, "");
const args = process.argv.slice(2);
let platform = process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform;
let releaseId = "";
let tier = "";
let dryRun = false;
let verificationTicket = "";
let promotionReceipt = "";
const hostReceipts = [];

for (let i = 0; i < args.length; i += 1) {
  const arg = args[i];
  if (arg === "--platform") platform = args[++i];
  else if (arg === "--release") releaseId = args[++i];
  else if (arg === "--tier") tier = args[++i];
  else if (arg.startsWith("--tier=")) tier = arg.slice("--tier=".length);
  else if (arg === "--dry-run") dryRun = true;
  else if (arg === "--verification-ticket") verificationTicket = args[++i] ?? "";
  else if (arg === "--host-receipt") hostReceipts.push(args[++i] ?? "");
  else if (arg === "--promotion-receipt") promotionReceipt = args[++i] ?? "";
  else if (arg === "-h" || arg === "--help") usage(0);
  else fail(`unknown argument: ${arg}`);
}
if (tier !== "patch" && tier !== "update") fail("tier is required (patch|update)");
if (!releaseId) fail("--release is required");
if (platform !== "win" && platform !== "mac") fail("--platform must be win or mac");

const repoRoot = git(process.cwd(), ["rev-parse", "--show-toplevel"]);
assertPrimaryReleaseCheckout(repoRoot);
const candidateCommit = git(repoRoot, ["rev-parse", "HEAD"]);
const candidateTree = git(repoRoot, ["rev-parse", "HEAD^{tree}"]);
if (!verificationTicket || !promotionReceipt || hostReceipts.length === 0) {
  fail("--verification-ticket, --host-receipt, and --promotion-receipt are required");
}
const ticket = readVerificationDocument(path.resolve(verificationTicket));
const adminTrust = loadBoundedAdminTrust();
verifyPromotionEvidence({
  ticket,
  hostReceipts: hostReceipts.map((file) => readVerificationDocument(path.resolve(file))),
  promotionReceipt: readVerificationDocument(path.resolve(promotionReceipt)),
  trustedKeys: adminTrust.ticketKeys,
  hostTrust: { hosts: adminTrust.hosts, keys: adminTrust.hostKeys },
  promotionTrustedKeys: adminTrust.ticketKeys,
  candidateCommit,
  candidateTree,
  commandId: "right-release.upload",
});
assertCleanOverlay({ status: git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]), ticket });
const platformDir = platform === "win" ? "windows" : "mac";
const sealedDir = path.join(repoRoot, ".right-release", "sealed", releaseId, platformDir);
const sealed = verifySealedRelease(sealedDir);
if (sealed.manifest.commit !== candidateCommit) fail("sealed release candidate does not exactly match verification ticket source");
const stateRoot = path.join(repoRoot, ".right-release", "state", releaseId, platformDir);
const backupRoot = path.join(stateRoot, "rollback", tier);
const verifiedMarker = path.join(stateRoot, `verified-${tier}.json`);
if (existsSync(verifiedMarker)) {
  console.log(`right-release upload: already verified ${releaseId} tier=${tier}`);
  process.exit(0);
}
if (!dryRun && !process.env.CLOUDFLARE_API_TOKEN) fail("CLOUDFLARE_API_TOKEN is required before R2 mutation");

await runUploadStateMachine({
  root: repoRoot,
  releaseId,
  platform,
  tier,
  ops: {
    verifyAuthenticode: async (release) => verifyPlatformTrust(release),
    harden: async (release) => {
      const files = release.manifest.files.filter((file) => file.role !== "updater-signature").map((file) => path.join(release.sealedDir, file.name));
      runChecked(process.execPath, [HARDENING, ...files], repoRoot);
    },
    backup: async (release, routes) => backupStableObjects(release, routes),
    upload: async (release) => publishSealed(release),
    register: async () => {},
    verifyRemote: async (release, routes) => verifyUploaded(release, routes),
    restore: async (_release, routes) => restoreStableObjects(routes),
    discardBackup: async () => rmSync(backupRoot, { recursive: true, force: true }),
  },
});
if (!dryRun) writeJson(verifiedMarker, { schema: 1, releaseId, platform, tier, verifiedAt: new Date().toISOString() });
console.log(`right-release upload: ${dryRun ? "dry-run " : ""}verified ${releaseId} tier=${tier}`);

function verifyPlatformTrust(release) {
  if (dryRun) return;
  const artifacts = release.manifest.files.filter((file) => file.role !== "updater-signature");
  if (platform === "win") {
    for (const artifact of artifacts) {
      const file = path.join(release.sealedDir, artifact.name);
      runChecked(process.execPath, [SIGN_WINDOWS, "--verify-only", file], repoRoot);
    }
  } else {
    for (const artifact of artifacts.filter((file) => /\.dmg$/i.test(file.name))) {
      const file = path.join(release.sealedDir, artifact.name);
      runChecked("codesign", ["--verify", "--strict", "--verbose=2", file], repoRoot);
      runChecked("xcrun", ["stapler", "validate", file], repoRoot);
    }
  }
}

function backupStableObjects(release, routes) {
  if (dryRun) return;
  if (existsSync(path.join(backupRoot, "backup-manifest.json"))) restoreStableObjects(routes);
  mkdirSync(backupRoot, { recursive: true });
  const entries = [];
  for (const route of uniqueRoutes(routes)) {
    const bucket = bucketName(route.bucket);
    const file = path.join(backupRoot, `${createHash("sha256").update(`${bucket}/${route.key}`).digest("hex").slice(0, 12)}.bin`);
    const result = wrangler(["r2", "object", "get", `${bucket}/${route.key}`, "--file", file, "--remote"]);
    if (result.status === 0) entries.push({ bucket: route.bucket, key: route.key, existed: true, file: path.basename(file), sha256: hashFile(file) });
    else if (/not found|does not exist|404/i.test(`${result.stdout}\n${result.stderr}`)) entries.push({ bucket: route.bucket, key: route.key, existed: false, file: null, sha256: null });
    else throw new Error(`failed to back up R2 object ${bucket}/${route.key}: ${result.stderr || result.stdout}`);
  }
  writeJson(path.join(backupRoot, "backup-manifest.json"), { schema: 1, releaseId: release.manifest.releaseId, entries });
}

function publishSealed(release) {
  if (dryRun) return;
  const generated = path.join(stateRoot, `sealed-upload-${tier}.config.mjs`);
  const config = sealedUploadConfig(release);
  writeFileSync(generated, `export default ${JSON.stringify(config, null, 2)};\n`);
  runChecked(process.execPath, [PUBLISH_UPDATE, "--config", generated, "--platform", platform], repoRoot, { ...process.env, RIGHT_RELEASE_TIER: tier });
}

function sealedUploadConfig(release) {
  const fileByName = new Map(release.manifest.files.map((file) => [file.name, path.join(release.sealedDir, file.name)]));
  const patchRoutes = release.manifest.routes.patch ?? [];
  const updateRoutes = release.manifest.routes.update ?? [];
  const installers = patchRoutes.filter((route) => route.role === "installer").map((route) => ({ file: fileByName.get(route.name), key: route.key }));
  const updaterRoutes = updateRoutes.map((route) => {
    const patch = patchRoutes.find((candidate) => candidate.name === route.name && candidate.platform === route.platform);
    return { file: fileByName.get(route.name), signature: fileByName.get(route.signature), platform: route.platform, key: route.key, patchKey: patch?.key };
  });
  return {
    schema: 1,
    app: release.manifest.app,
    version: release.manifest.version,
    channel: "stable",
    targets: { [platform]: { installer: { artifacts: installers }, updater: { artifacts: updaterRoutes } } },
  };
}

async function verifyUploaded(release, routes) {
  if (dryRun) return;
  const files = new Map(release.manifest.files.map((file) => [file.name, file]));
  for (const route of uniqueRoutes(routes)) {
    const expected = files.get(route.name);
    if (!expected) throw new Error(`route references unsealed file: ${route.name}`);
    const verifyFile = path.join(stateRoot, `remote-${createHash("sha256").update(route.key).digest("hex").slice(0, 12)}.bin`);
    if (route.bucket === "public") {
      const response = await fetch(`${PUBLIC_BASE}/${route.key}`, { cache: "no-store" });
      if (!response.ok) throw new Error(`public artifact verification failed: ${response.status} ${route.key}`);
      writeFileSync(verifyFile, Buffer.from(await response.arrayBuffer()));
    } else {
      const result = wrangler(["r2", "object", "get", `${bucketName(route.bucket)}/${route.key}`, "--file", verifyFile, "--remote"]);
      if (result.status !== 0) throw new Error(`private artifact verification failed: ${route.key}`);
    }
    if (statSync(verifyFile).size !== expected.sizeBytes || hashFile(verifyFile) !== expected.sha256) throw new Error(`remote hash mismatch: ${route.key}`);
    rmSync(verifyFile, { force: true });
  }
  if (!routes.some((route) => route.role === "updater")) return;
  const apiPlatform = platform === "win" ? "windows-x86_64" : "darwin-aarch64";
  const response = await fetch(`${API_BASE}/v1/apps/${release.manifest.app}/releases/latest.json?platform=${apiPlatform}`, { cache: "no-store" });
  if (!response.ok) throw new Error(`updater API verification failed: ${response.status}`);
  const body = await response.json();
  if (body.version !== release.manifest.version) throw new Error(`updater API version mismatch: expected ${release.manifest.version}, got ${body.version}`);
  const expectedRoutes = new Set(routes.map((route) => route.key));
  const urls = Object.values(body.platforms ?? {}).map((entry) => entry.url).filter(Boolean);
  if (!urls.some((url) => [...expectedRoutes].some((key) => url.endsWith(key)))) throw new Error("updater API returned the wrong artifact URL");
}

function restoreStableObjects(routes) {
  const manifestPath = path.join(backupRoot, "backup-manifest.json");
  if (!existsSync(manifestPath)) return;
  const backup = JSON.parse(readFileSync(manifestPath, "utf8"));
  for (const entry of backup.entries) {
    if (entry.existed) {
      const file = path.join(backupRoot, entry.file);
      if (hashFile(file) !== entry.sha256) throw new Error(`rollback copy hash mismatch: ${entry.key}`);
      runChecked(process.execPath, [UPLOAD, file, entry.key, entry.bucket], repoRoot);
    } else {
      const result = wrangler(["r2", "object", "delete", `${bucketName(entry.bucket)}/${entry.key}`, "--remote"]);
      if (result.status !== 0 && !/not found|404/i.test(`${result.stdout}\n${result.stderr}`)) throw new Error(`rollback delete failed: ${entry.key}`);
    }
  }
  rmSync(backupRoot, { recursive: true, force: true });
}

function uniqueRoutes(routes) {
  return [...new Map(routes.map((route) => [`${route.bucket}/${route.key}`, route])).values()];
}

function bucketName(bucket) {
  return bucket === "public" ? "rightapps-downloads" : "rightapps-updates";
}

function wrangler(runArgs) {
  return spawnSync("pnpm", ["dlx", "wrangler@4", ...runArgs], { cwd: repoRoot, env: process.env, encoding: "utf8", windowsHide: true, shell: process.platform === "win32" });
}

function hashFile(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function git(cwd, runArgs) {
  return commandOutput("git", runArgs, cwd);
}

function commandOutput(cmd, runArgs, cwd = process.cwd()) {
  const result = spawnSync(cmd, runArgs, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) fail(`${cmd} ${runArgs.join(" ")} failed: ${result.stderr}`);
  return result.stdout.trim();
}

function runChecked(cmd, runArgs, cwd, env = process.env) {
  const result = spawnSync(cmd, runArgs, { cwd, env, stdio: "inherit", windowsHide: true, shell: false });
  if (result.status !== 0) throw new Error(`${cmd} exited ${result.status}`);
}

function writeJson(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function usage(code) {
  console.log("usage: right-release upload --release <sealed-id> --platform win|mac --tier patch|update --verification-ticket <file> --host-receipt <file> --promotion-receipt <file>");
  process.exit(code);
}

function fail(message) {
  console.error(`right-release upload: ${message}`);
  process.exit(1);
}
