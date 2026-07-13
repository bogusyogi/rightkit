#!/usr/bin/env node
import { createHash } from "node:crypto";
import { access, readFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn } from "node:child_process";
import { pruneReleaseObjects } from "./prune-r2.mjs";

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const UPLOAD = path.join(TOOL_ROOT, "upload-large.mjs");
const API_BASE = process.env.RIGHTAPPS_API_URL || "https://api.spoares.com";
const DOWNLOAD_BASE = process.env.RIGHTAPPS_UPDATE_DOWNLOAD_BASE || "https://rightapps-license-gate.adrdsouza.workers.dev";
const PUBLIC_BASE = process.env.RIGHTAPPS_PUBLIC_DOWNLOAD_BASE || "https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev";
const PIPELINE_VERSION = JSON.parse(await readFile(path.join(TOOL_ROOT, "package.json"), "utf8")).version;
const tier = process.env.RIGHT_RELEASE_TIER || "";
const args = process.argv.slice(2);
let configName = "right-release.config.mjs";
let platform = process.platform === "win32" ? "win" : "mac";
let dryRun = false;

for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === "--config") configName = args[++i];
  else if (arg === "--platform") platform = args[++i];
  else if (arg === "--dry-run") dryRun = true;
  else fail(`unknown argument: ${arg}`);
}

if (tier !== "patch" && tier !== "update") {
  fail("RIGHT_RELEASE_TIER is required (patch|update)");
}

const configPath = path.resolve(configName);
const root = path.dirname(configPath);
const config = (await import(pathToFileURL(configPath))).default;
const target = config?.targets?.[platform];
const artifacts = target?.updater?.artifacts ?? [];
const installers = target?.installer?.artifacts ?? [];

if (tier === "patch") {
  if (!installers.length) fail(`${config?.app ?? "app"} ${platform} patch has no installer.artifacts`);
  for (const installer of installers) {
    assertCurrentKey(installer.key, "installer");
    const file = path.resolve(root, installer.file);
    if (!dryRun) await access(file).catch(() => fail(`missing signed installer: ${installer.file}`));
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
    const file = path.resolve(root, artifact.file);
    const signatureFile = path.resolve(root, artifact.signature);
    if (!dryRun) {
      await access(file).catch(() => fail(`missing patch updater payload: ${artifact.file}`));
      await access(signatureFile).catch(() => fail(`missing patch updater signature: ${artifact.signature}`));
    }
    if (!uploadedKeys.has(installerKey)) {
      console.log(`${dryRun ? "[dry-run] " : ""}UPLOAD public/${installerKey}`);
      if (!dryRun) await run(process.execPath, [UPLOAD, file, installerKey, "public"], root);
      uploadedKeys.add(installerKey);
    }
    const signature = dryRun ? `<signature:${artifact.signature}>` : (await readFile(signatureFile, "utf8")).trim();
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
    const file = path.resolve(root, installer.file);
    if (!uploadedKeys.has(installer.key)) {
      console.log(`${dryRun ? "[dry-run] " : ""}UPLOAD public/${installer.key}`);
      if (!dryRun) await run(process.execPath, [UPLOAD, file, installer.key, "public"], root);
      uploadedKeys.add(installer.key);
    }
    const metadata = dryRun ? { sha256: null, sizeBytes: null } : await fileMetadata(file);
    installerManifestArtifacts.set(installer.key, {
      kind: "installer",
      r2Key: installer.key,
      ...metadata,
      updaterSignature: null,
    });
  }
  if (!artifacts.length) {
    console.log(`${dryRun ? "[dry-run] " : ""}NO patch updater artifacts; skipping patch manifest registration`);
    await pruneReleaseObjects({
      app: config.app,
      platform: platform === "mac" ? "mac" : "windows",
      dryRun,
      keep: {
        public: installers.map((artifact) => artifact.key),
        private: target.updater.artifacts.map((artifact) => artifact.key),
      },
    });
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
      public: [...installers.map((artifact) => artifact.key), ...registrations.keys()],
      private: [],
    },
  });
  process.exit(0);
}

if (!artifacts.length) fail(`${config?.app ?? "app"} ${platform} update has no updater.artifacts`);

const platforms = {};
const registrations = new Map();
const updaterManifestArtifacts = new Map();
const uploadedKeys = new Set();
for (const artifact of artifacts) {
  assertCurrentKey(artifact.key, "updater");
  const file = path.resolve(root, artifact.file);
  const signatureFile = path.resolve(root, artifact.signature);
  if (!dryRun) {
    await access(file).catch(() => fail(`missing updater artifact: ${artifact.file}`));
    await access(signatureFile).catch(() => fail(`missing updater signature: ${artifact.signature}`));
  }
  if (!uploadedKeys.has(artifact.key)) {
    console.log(`${dryRun ? "[dry-run] " : ""}UPLOAD private/${artifact.key}`);
    if (!dryRun) await run(process.execPath, [UPLOAD, file, artifact.key, "private"], root);
    uploadedKeys.add(artifact.key);
  }
  const signature = dryRun ? `<signature:${artifact.signature}>` : (await readFile(signatureFile, "utf8")).trim();
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

async function fileMetadata(file) {
  const bytes = await readFile(file);
  return {
    sha256: createHash("sha256").update(bytes).digest("hex"),
    sizeBytes: (await stat(file)).size,
  };
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

function run(cmd, runArgs, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, runArgs, { cwd, env: process.env, stdio: "inherit", windowsHide: true });
    child.on("error", reject);
    child.on("exit", (code) => code === 0 ? resolve() : reject(new Error(`${cmd} exited ${code}`)));
  });
}

function assertCurrentKey(key, kind) {
  const pattern = kind === "installer"
    ? /\/installers\/(?:mac|windows)\/current\//
    : /\/updates\/(?:mac|windows)\/current\//;
  if (!pattern.test(key)) {
    fail(`${kind} key must target the replaceable current R2 object, got: ${key}`);
  }
}

function patchInstallerKey(artifact) {
  if (artifact.patchKey) return artifact.patchKey;
  if (!artifact.key.includes("/updates/")) fail(`patch updater key must be convertible to installers/current, got: ${artifact.key}`);
  return artifact.key.replace("/updates/", "/installers/");
}

async function register(route, body) {
  const token = process.env.RIGHTAPPS_ADMIN_TOKEN;
  if (!token) fail("RIGHTAPPS_ADMIN_TOKEN is required to register an update");
  const response = await fetch(`${API_BASE}${route}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
      "x-store-slug": process.env.RIGHTAPPS_STORE_SLUG || "rightapps",
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) fail(`registration failed: ${response.status} ${await response.text()}`);
  console.log(await response.text());
}

function fail(message) {
  console.error(`publish-update: ${message}`);
  process.exit(1);
}
