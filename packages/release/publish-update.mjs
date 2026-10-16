#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import { readFileSync } from "node:fs";
import { spawnSync } from "./background-process.mjs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pruneReleaseObjects } from "./prune-r2.mjs";
import { loadReleaseToken } from "./release-token.mjs";
import { registerRightAppsRelease } from "./rightapps-register.mjs";
import { recordPublishedVersion } from "./artifact-storage.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { assertCleanSource } from "./source-gate.mjs";
import { verifyPublicationRelease, PublicationError, assertArtifactUnchanged, digestBytes } from "./publication-artifacts.mjs";
import { uploadVerifiedR2Artifact } from "./r2-verified-upload.mjs";
import { verifyPlatformTrust } from "./github-release.mjs";
import { admitInstalledQa } from "./release-qa-audit.mjs";

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const API_BASE = process.env.RIGHTAPPS_API_URL || "https://api.spoares.com";
const DOWNLOAD_BASE = process.env.RIGHTAPPS_UPDATE_DOWNLOAD_BASE || "https://rightapps-license-gate.adrdsouza.workers.dev";
const PUBLIC_BASE = process.env.RIGHTAPPS_PUBLIC_DOWNLOAD_BASE || "https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev";
const PIPELINE_VERSION = JSON.parse(await readFile(path.join(TOOL_ROOT, "package.json"), "utf8")).version;
let tier = process.env.RIGHT_RELEASE_TIER || "";
const args = process.argv.slice(2);
let configName = "right-release.config.mjs";
let platform = process.platform === "win32" ? "win" : "mac";
let dryRun = false;
let releaseId = "";

for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === "--config") configName = args[++i];
  else if (arg === "--platform") platform = args[++i];
  else if (arg === "--tier") tier = args[++i];
  else if (arg.startsWith("--tier=")) tier = arg.slice("--tier=".length);
  else if (arg === "--release") releaseId = args[++i];
  else if (arg === "--dry-run") dryRun = true;
  else fail(`unknown argument: ${arg}`);
}

if (tier !== "patch" && tier !== "update") fail("tier is required (--tier patch|update or RIGHT_RELEASE_TIER)");

if (!["win", "mac"].includes(platform)) throw new PublicationError("PUBLICATION_PLATFORM_INVALID", "--platform must be win or mac");
const configPath = path.resolve(configName);
const root = path.dirname(configPath);
const config = (await import(pathToFileURL(configPath))).default;
if (config?.distribution?.provider !== "r2-rightapps") {
  fail("release config distribution.provider must be r2-rightapps");
}
const target = config?.targets?.[platform];
const artifacts = target?.updater?.artifacts ?? [];
const installers = target?.installer?.artifacts ?? [];
const versionForStorage = config.version || target?.updater?.version;
let sealed;
if (!dryRun || releaseId) {
  if (!/^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/.test(releaseId)) throw new PublicationError("PUBLICATION_SEAL_REQUIRED", "--release <sealed-id> is required");
  const repoRoot = git(["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot);
  const commit = git(["rev-parse", "HEAD"]);
  assertCleanSource({ status: git(["status", "--porcelain=v1", "-z", "--untracked-files=all"]), commandId: "right-release publish-update" });
  sealed = verifyPublicationRelease({ sealedDir: path.join(repoRoot, ".right-release", "sealed", releaseId, platform === "win" ? "windows" : "mac"), repoRoot, appRoot: root, commit, app: config.app, version: versionForStorage, platform, configPath, releaseId });
  const routes = sealed.manifest.routes?.[tier] ?? [];
  const configured = [...(tier === "patch" ? installers.map((item) => ({ ...item, role: "installer" })) : []), ...artifacts.map((item) => ({ ...item, key: tier === "patch" ? patchInstallerKey(item) : item.key, role: "updater" }))];
  for (const item of configured) {
    if (!routes.some((route) => route.key === item.key && route.role === item.role && route.bucket === (tier === "patch" ? "public" : "private") && route.name === path.basename(item.file) && (!item.signature || route.signature === path.basename(item.signature)))) throw new PublicationError("PUBLICATION_ROUTE_INVALID", `config route is not sealed: ${item.key}`);
  }
  if (!configured.length) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "seal has no configured upload route");
  const installer = sealed.manifest.files.find((item) => item.role.split("+").includes("installer"));
  if (!installer) throw new PublicationError("PUBLICATION_SIGNATURE_MISSING", "sealed installer required for platform trust verification");
  verifyPlatformTrust({ sealed, installer: path.join(sealed.sealedDir, installer.name) });
  for (const artifact of sealed.artifacts.values()) assertArtifactUnchanged(artifact);
  // Hardening step 4: installed QA is recorded, not enforced (R2 patch + update tiers); a missing receipt warns and is written to state.
  if (!dryRun) {
    admitInstalledQa({ repoRoot, manifest: sealed.manifest, platform, configName: path.relative(process.cwd(), configPath) || configName, releaseConfigSha256: digestBytes(readFileSync(configPath)) }, { provider: "r2" });
  }
}
const artifactFile = (declared) => {
  if (!sealed) return path.resolve(root, declared);
  const artifact = sealed.artifacts.get(path.basename(declared));
  if (!artifact) throw new PublicationError("PUBLICATION_ARTIFACT_MISSING", `artifact is not sealed: ${declared}`);
  return artifact.file;
};
const verifiedArtifact = (file) => sealed.artifacts.get(path.basename(file));
// MSIX updates (updater.kind appinstaller) trust the package Authenticode signature (publisher-pinned in
// the manifest), so no minisign .sig exists; the route records the sealed package digest instead.
const appInstallerKind = target?.updater?.kind === "appinstaller";
const signatureFor = (artifact) => appInstallerKind
  ? `msix-authenticode-sha256:${assertArtifactUnchanged(verifiedArtifact(artifact.file)) && verifiedArtifact(artifact.file).sha256}`
  : signatureText(artifactFile(artifact.signature));
const signatureText = (file) => {
  const signature = assertArtifactUnchanged(verifiedArtifact(file)).toString("utf8").trim();
  if (!signature) throw new PublicationError("PUBLICATION_SIGNATURE_MISSING", `sealed updater signature is empty: ${file}`);
  return signature;
};
function git(args) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) throw new PublicationError("PUBLICATION_SOURCE_INVALID", "cannot read publication source identity");
  return result.stdout.trim();
}

if (tier === "patch" && !installers.length) fail(`${config?.app ?? "app"} ${platform} patch has no installer.artifacts`);
if (tier === "update" && !artifacts.length) fail(`${config?.app ?? "app"} ${platform} update has no updater.artifacts`);

if (sealed) for (const artifact of artifacts) signatureFor(artifact);
let releaseToken = null;
if (!dryRun && (tier === "update" || artifacts.length > 0)) {
  try {
    releaseToken = await loadReleaseToken();
  } catch (error) {
    fail(error.message);
  }
}

if (tier === "patch") {
  for (const installer of installers) {
    assertCurrentKey(installer.key, "installer");
    assertVersionedKey(installer.versionedKey);
    artifactFile(installer.file);
  }
  const platforms = {};
  const registrations = new Map();
  const updaterManifestArtifacts = new Map();
  const installerManifestArtifacts = new Map();
  const uploadedKeys = new Set();
  for (const artifact of artifacts) {
    assertCurrentKey(artifact.key, "updater");
    const installerKey = patchInstallerKey(artifact);
    assertCurrentKey(installerKey, "installer");
    const file = artifactFile(artifact.file);
    if (!appInstallerKind) artifactFile(artifact.signature);
    if (!uploadedKeys.has(installerKey)) {
      console.log(`${dryRun ? "[dry-run] " : ""}UPLOAD public/${installerKey}`);
      if (!dryRun) uploadVerifiedR2Artifact(verifiedArtifact(file), installerKey, "public", { cwd: root });
      uploadedKeys.add(installerKey);
    }
    const signature = dryRun ? `<signature:${artifact.signature ?? "msix-authenticode"}>` : signatureFor(artifact);
    const artifactKey = artifact.artifactKey || installerKey.replaceAll("/", "__");
    const metadata = dryRun ? { sha256: null, sizeBytes: null } : await fileMetadata(file);
    platforms[artifact.platform] = {
      signature,
      url: `${PUBLIC_BASE}/${installerKey}`,
    };
    registrations.set(installerKey, { artifactKey, path: installerKey, ...metadata });
    updaterManifestArtifacts.set(installerKey, {
      kind: "updater",
      r2Key: installerKey,
      ...metadata,
      updaterSignature: signature,
    });
  }
  for (const installer of installers) {
    const file = artifactFile(installer.file);
    if (!uploadedKeys.has(installer.key)) {
      console.log(`${dryRun ? "[dry-run] " : ""}UPLOAD public/${installer.key}`);
      if (!dryRun) uploadVerifiedR2Artifact(verifiedArtifact(file), installer.key, "public", { cwd: root });
      uploadedKeys.add(installer.key);
    }
    const metadata = dryRun ? { sha256: null, sizeBytes: null } : await fileMetadata(file);
    installerManifestArtifacts.set(installer.key, {
      kind: "installer",
      r2Key: installer.key,
      ...metadata,
      updaterSignature: null,
    });
    // The versioned copy is the App Installer install source: Windows repairs by re-adding from the URI the
    // package came from, so it must outlive the version that replaces it.
    if (installer.versionedKey && !uploadedKeys.has(installer.versionedKey)) {
      console.log(`${dryRun ? "[dry-run] " : ""}UPLOAD public/${installer.versionedKey}`);
      if (!dryRun) uploadVerifiedR2Artifact(verifiedArtifact(file), installer.versionedKey, "public", { cwd: root });
      uploadedKeys.add(installer.versionedKey);
    }
  }
  if (!artifacts.length) {
    console.log(`${dryRun ? "[dry-run] " : ""}NO patch updater artifacts; skipping patch manifest registration`);
    await pruneReleaseObjects({
      app: config.app,
      platform: platform === "mac" ? "mac" : "windows",
      dryRun,
      keep: {
        public: installerKeepKeys(),
        private: target.updater.artifacts.map((artifact) => artifact.key),
      },
      retainPreviousVersions: installers.some((artifact) => artifact.versionedKey) ? 1 : 0,
    });
    if (!dryRun) recordPublishedVersion({ app: config.app, version: versionForStorage, platform, configPath, releaseId });
    process.exit(0);
  }
  const version = config.version || target.updater.version;
  if (!version) fail(`${config.app} config must expose version or targets.${platform}.updater.version`);
  const body = {
    appKey: config.app,
    version,
    channel: config.channel || "stable",
    platform: platform === "mac" ? "darwin" : "windows",
    manifest: {
      version,
      tier,
      notes: process.env.RIGHT_RELEASE_NOTES || `${config.app} ${version}`,
      pub_date: new Date().toISOString(),
      platforms,
    },
    artifacts: [...registrations.values()],
    artifactManifest: buildArtifactManifest({
      appKey: config.app,
      appVersion: version,
      tier,
      platform,
      artifacts: [...installerManifestArtifacts.values(), ...updaterManifestArtifacts.values()],
    }),
  };
  console.log(`${dryRun ? "[dry-run] " : ""}POST ${API_BASE}/v1/admin/apps/patches`);
  console.log(JSON.stringify(body));
  if (!dryRun) await register("/v1/admin/apps/patches", body);
  await pruneReleaseObjects({
    app: config.app,
    platform: platform === "mac" ? "mac" : "windows",
    dryRun,
    keep: {
      public: [...installerKeepKeys(), ...registrations.keys()],
      private: [],
    },
    retainPreviousVersions: installers.some((artifact) => artifact.versionedKey) ? 1 : 0,
  });
  if (!dryRun) recordPublishedVersion({ app: config.app, version: versionForStorage, platform, configPath, releaseId });
  process.exit(0);
}

const platforms = {};
const registrations = new Map();
const updaterManifestArtifacts = new Map();
const uploadedKeys = new Set();
for (const artifact of artifacts) {
  assertCurrentKey(artifact.key, "updater");
  const file = artifactFile(artifact.file);
  if (!appInstallerKind) artifactFile(artifact.signature);
  if (!uploadedKeys.has(artifact.key)) {
    console.log(`${dryRun ? "[dry-run] " : ""}UPLOAD private/${artifact.key}`);
    if (!dryRun) uploadVerifiedR2Artifact(verifiedArtifact(file), artifact.key, "private", { cwd: root });
    uploadedKeys.add(artifact.key);
  }
  const signature = dryRun ? `<signature:${artifact.signature ?? "msix-authenticode"}>` : signatureFor(artifact);
  const artifactKey = artifact.artifactKey || artifact.key.replaceAll("/", "__");
  const metadata = dryRun ? { sha256: null, sizeBytes: null } : await fileMetadata(file);
  platforms[artifact.platform] = {
    signature,
    url: `${DOWNLOAD_BASE}/${artifact.key}`,
  };
  registrations.set(artifact.key, { artifactKey, path: artifact.key, ...metadata });
  updaterManifestArtifacts.set(artifact.key, {
    kind: "updater",
    r2Key: artifact.key,
    ...metadata,
    updaterSignature: signature,
  });
}

const version = config.version || target.updater.version;
if (!version) fail(`${config.app} config must expose version or targets.${platform}.updater.version`);
const body = {
  appKey: config.app,
  version,
  channel: config.channel || "stable",
  platform: platform === "mac" ? "darwin" : "windows",
  manifest: {
    version,
    tier,
    notes: process.env.RIGHT_RELEASE_NOTES || `${config.app} ${version}`,
    pub_date: new Date().toISOString(),
    platforms,
  },
  artifacts: [...registrations.values()],
  artifactManifest: buildArtifactManifest({
    appKey: config.app,
    appVersion: version,
    tier,
    platform,
    artifacts: [...updaterManifestArtifacts.values()],
  }),
};
const route = "/v1/admin/apps/releases";
console.log(`${dryRun ? "[dry-run] " : ""}POST ${API_BASE}${route}`);
console.log(JSON.stringify(body));

if (!dryRun) await register(route, body);
await pruneReleaseObjects({
  app: config.app,
  platform: platform === "mac" ? "mac" : "windows",
  dryRun,
  keep: {
    public: installers.map((artifact) => artifact.key),
    private: [...registrations.keys()],
  },
});
if (!dryRun) recordPublishedVersion({ app: config.app, version: versionForStorage, platform, configPath, releaseId });

function fileMetadata(file) {
  const artifact = verifiedArtifact(file);
  assertArtifactUnchanged(artifact);
  return { sha256: artifact.sha256, sizeBytes: artifact.sizeBytes };
}

function buildArtifactManifest({ appKey, appVersion, tier, platform, artifacts }) {
  return {
    schema: 1,
    pipelineVersion: PIPELINE_VERSION,
    appKey,
    appVersion,
    tier,
    platform: platform === "mac" ? "darwin" : "windows",
    artifacts,
  };
}


function assertCurrentKey(key, kind) {
  const pattern = kind === "installer"
    ? /\/installers\/(?:mac|windows)\/current\//
    : /\/updates\/(?:mac|windows)\/current\//;
  if (!pattern.test(key)) {
    fail(`${kind} key must target the replaceable current R2 object, got: ${key}`);
  }
}

// Current + versioned installer keys: both stay public so the old install source is never deleted.
function installerKeepKeys() {
  return installers.flatMap((artifact) => [artifact.key, ...(artifact.versionedKey ? [artifact.versionedKey] : [])]);
}

function assertVersionedKey(key) {
  if (key === undefined) return;
  if (!/\/installers\/(?:mac|windows)\/versions\/[^/]+\/[^/]+$/.test(key)) {
    fail(`installer versionedKey must be <app>/installers/<os>/versions/<version>/<file>, got: ${key}`);
  }
}

function patchInstallerKey(artifact) {
  if (artifact.patchKey) return artifact.patchKey;
  if (!artifact.key.includes("/updates/")) fail(`patch updater key must be convertible to installers/current, got: ${artifact.key}`);
  return artifact.key.replace("/updates/", "/installers/");
}

async function register(route, body) {
  try {
    const response = await registerRightAppsRelease(route, body, { token: releaseToken });
    if (response !== null) console.log(typeof response === "string" ? response : JSON.stringify(response));
  } catch (error) {
    fail(error.message);
  }
}

function fail(message) {
  console.error(`publish-update: ${message}`);
  process.exit(1);
}
