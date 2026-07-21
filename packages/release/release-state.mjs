import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

export function cacheFingerprint({ cargoLockSha256, rustc, target, features = [] }) {
  const payload = JSON.stringify({
    cargoLockSha256,
    rustc,
    target,
    features: [...features].sort(),
  });
  return createHash("sha256").update(payload).digest("hex").slice(0, 16);
}

export function resolveReleaseLayout({ repoRoot, configPath }) {
  const resolvedRepoRoot = path.resolve(repoRoot);
  const appRoot = path.dirname(path.resolve(configPath));
  return {
    repoRoot: resolvedRepoRoot,
    appRoot,
    vaultRoot: path.join(resolvedRepoRoot, ".right-release"),
    targetLink: path.join(appRoot, "src-tauri", "target"),
  };
}

export function releaseEnvironment({ root, platform, cacheKey, kind = "release" }) {
  if (kind !== "release" && kind !== "test") throw new Error(`invalid target kind: ${kind}`);
  const targetKind = kind === "release" ? "cargo-target" : "test-target";
  return {
    CARGO_TARGET_DIR: path.resolve(root, "cache", targetKind, platform, cacheKey),
    CARGO_HOME: path.resolve(root, "cache", "cargo-home"),
    SCCACHE_DIR: path.resolve(root, "cache", "sccache"),
    RUSTC_WRAPPER: "sccache",
  };
}

export function verifySealedRelease(sealedDir) {
  const manifestPath = path.join(sealedDir, "release-manifest.json");
  if (!existsSync(manifestPath)) throw new Error(`sealed manifest missing: ${manifestPath}`);
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (manifest.schema !== 1) throw new Error(`unsupported sealed manifest schema: ${manifest.schema}`);
  if (!manifest.checkpoints?.includes("sealed")) throw new Error("release is not sealed");
  for (const item of manifest.files ?? []) {
    const file = path.join(sealedDir, item.name);
    if (!existsSync(file)) throw new Error(`sealed file missing: ${item.name}`);
    const bytes = readFileSync(file);
    const actualHash = createHash("sha256").update(bytes).digest("hex");
    if (actualHash !== item.sha256) throw new Error(`sealed file hash mismatch: ${item.name}`);
    if (bytes.length !== item.sizeBytes) throw new Error(`sealed file size mismatch: ${item.name}`);
  }
  return { manifest, manifestPath, sealedDir };
}

export async function runBuildStateMachine({
  root,
  app,
  version,
  commit,
  platform,
  requiredInputs = [],
  ops,
}) {
  const releaseId = `${app}-${version}-${commit.slice(0, 8)}`;
  const platformDir = platform === "win" ? "windows" : platform === "mac" ? "mac" : platform;
  const sealedDir = path.join(root, ".right-release", "sealed", releaseId, platformDir);
  if (existsSync(path.join(sealedDir, "release-manifest.json"))) {
    const sealed = verifySealedRelease(sealedDir);
    if (sealed.manifest.app !== app || sealed.manifest.version !== version || sealed.manifest.commit !== commit) {
      throw new Error(`sealed release identity mismatch: ${releaseId}`);
    }
    return { status: "sealed", resumed: true, releaseId, sealedDir };
  }
  await ops.preflight?.({ root, app, version, commit, platform, requiredInputs, releaseId });
  await ops.prepare?.({ root, app, version, commit, platform, releaseId });
  await ops.build?.({ root, app, version, commit, platform, releaseId });
  await ops.sign?.({ root, app, version, commit, platform, releaseId });
  await ops.harden?.({ root, app, version, commit, platform, releaseId });
  await ops.seal?.({ root, app, version, commit, platform, releaseId, sealedDir });
  return { status: "sealed", resumed: false, releaseId, sealedDir };
}

export async function runUploadStateMachine({ root, releaseId, platform, tier, ops = {} }) {
  if (tier !== "patch" && tier !== "update") throw new Error("upload tier is required (patch|update)");
  if (!/^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/.test(String(releaseId))) throw new Error(`invalid release id: ${releaseId}`);
  const platformDir = platform === "win" ? "windows" : platform === "mac" ? "mac" : platform;
  const sealedDir = path.join(root, ".right-release", "sealed", releaseId, platformDir);
  const sealed = verifySealedRelease(sealedDir);
  if (sealed.manifest.platform !== platform) throw new Error("sealed release platform mismatch");
  const routes = sealed.manifest.routes?.[tier] ?? [];
  if (!routes.length) throw new Error(`sealed release has no ${tier} upload route`);
  validateUploadRoutes(routes, tier, platformDir);

  await ops.verifyAuthenticode?.(sealed);
  await ops.harden?.(sealed);
  let backedUp = false;
  try {
    await ops.backup?.(sealed, routes);
    backedUp = true;
    await ops.upload?.(sealed, routes);
    await ops.register?.(sealed, routes);
    await ops.verifyRemote?.(sealed, routes);
    await ops.discardBackup?.(sealed, routes);
  } catch (error) {
    if (backedUp) await ops.restore?.(sealed, routes);
    throw error;
  }
  return { status: "verified", releaseId, tier, sealedDir };
}

function validateUploadRoutes(routes, tier, platformDir) {
  const expectedBucket = tier === "patch" ? "public" : "private";
  const expectedLane = tier === "patch" ? "installers" : "updates";
  const marker = `/${expectedLane}/${platformDir}/current/`;
  for (const route of routes) {
    if (!/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(String(route.key)) || String(route.key).includes("//")) {
      throw new Error(`unsafe R2 key: ${route.key}`);
    }
    if (route.bucket !== expectedBucket || !String(route.key).includes(marker)) {
      throw new Error(`${tier} route must use ${expectedBucket} ${expectedLane}/${platformDir}/current: ${route.key}`);
    }
    if (String(route.key).includes("..")) throw new Error(`${tier} route contains traversal: ${route.key}`);
  }
}
