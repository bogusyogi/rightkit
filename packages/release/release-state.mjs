import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { existsSync, readFileSync, readdirSync, watch } from "node:fs";
import path from "node:path";
import { readCachePolicy, resolveCacheLayout } from "./cache-policy.mjs";
import { resolveNativeCargoLayout } from "./native-cargo-layout.mjs";
import { bufferQaFindings } from "./qa-control-scan.mjs";
import { sealedManifestAllowsContentDevtools } from "./devtools-policy.mjs";
import { assertManifestRegistryEvidence } from "./rightkit-registry-resolver.mjs";

export function cacheFingerprint({ cargoLockSha256, rustc, target, architecture, features = [] }) {
  const payload = JSON.stringify({
    cargoLockSha256,
    rustc,
    target,
    architecture,
    features: [...features].sort(),
  });
  return createHash("sha256").update(payload).digest("hex").slice(0, 16);
}

export function resolveReleaseLayout({ repoRoot, configPath, config, nativeLayout }) {
  const resolvedRepoRoot = path.resolve(repoRoot);
  const appRoot = path.dirname(path.resolve(configPath));
  const resolvedNativeLayout = nativeLayout ?? resolveNativeCargoLayout({ appRoot, config });
  return {
    repoRoot: resolvedRepoRoot,
    appRoot,
    vaultRoot: path.join(resolvedRepoRoot, ".right-release"),
    targetLink: resolvedNativeLayout.targetLink,
    nativeLayout: resolvedNativeLayout,
  };
}

export function commandOutputPortable(cmd, args, { cwd, env = process.env } = {}) {
  const windows = process.platform === "win32";
  if (windows && ![cmd, ...args].every((value) => /^[A-Za-z0-9._:/@+=-]+$/.test(value))) {
    throw new Error("unsafe Windows command token");
  }
  const executable = windows ? (env.ComSpec || env.COMSPEC || "cmd.exe") : cmd;
  const executableArgs = windows ? ["/d", "/s", "/c", [cmd, ...args].join(" ")] : args;
  const result = spawnSync(executable, executableArgs, {
    cwd,
    env,
    encoding: "utf8",
    windowsHide: true,
  });
  if (result.status !== 0) {
    const detail = result.error?.message || result.stderr?.trim() || `exit ${result.status}`;
    throw new Error(`${cmd} ${args.join(" ")} failed: ${detail}`);
  }
  return result.stdout.trim();
}

export function watchProgress(paths, onProgress, { maxFallbackWatchers = 128, watchFactory = watch, readDirectory = readdirSync } = {}) {
  if (!Number.isInteger(maxFallbackWatchers) || maxFallbackWatchers < 1) throw new TypeError("maxFallbackWatchers must be a positive integer");
  const watchers = [];
  const fallback = { remaining: maxFallbackWatchers };
  for (const candidate of paths) {
    if (!candidate || !existsSync(candidate)) continue;
    try {
      watchers.push(watchFactory(candidate, { recursive: true }, onProgress));
    } catch {
      watchDirectoryTree(candidate, onProgress, watchers, fallback, { watchFactory, readDirectory });
    }
  }
  return () => {
    for (const watcher of watchers) watcher.close();
  };
}

function watchDirectoryTree(root, onProgress, watchers, fallback, { watchFactory = watch, readDirectory = readdirSync } = {}) {
  const pending = [root];
  const visited = new Set();
  while (pending.length) {
    const directory = pending.pop();
    if (visited.has(directory)) continue;
    visited.add(directory);
    if (fallback.remaining <= 0) return;
    try { watchers.push(watchFactory(directory, onProgress)); fallback.remaining -= 1; } catch { /* output still counts as progress */ }
    let entries;
    try { entries = readDirectory(directory, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      if (entry.isDirectory()) pending.push(path.join(directory, entry.name));
    }
  }
}

export function releaseEnvironment({ root, cacheRoot, platform, architecture, app, cacheKey, kind = "release", appRoot, mode = "legacy", env = process.env }) {
  if (kind !== "release" && kind !== "test") throw new Error(`invalid target kind: ${kind}`);
  if (mode !== "legacy" && mode !== "shared" && mode !== "broker") throw new Error(`invalid cache mode: ${mode}`);
  if (mode === "shared") {
    if (!cacheRoot || !platform || !architecture || !app || !cacheKey || !appRoot) throw new Error("shared release environment requires cacheRoot, platform, architecture, app, cacheKey, and appRoot");
    const layout = resolveCacheLayout({ cacheRoot, platform, architecture, app, fingerprint: cacheKey, kind });
    const policy = readCachePolicy(env);
    return {
      CARGO_TARGET_DIR: layout.targetDir,
      CARGO_HOME: layout.cargoHome,
      SCCACHE_DIR: layout.sccacheDir,
      SCCACHE_CACHE_SIZE: `${policy.sccacheMaxBytes / 1024 ** 3}G`,
      SCCACHE_BASEDIRS: [path.resolve(root), path.resolve(appRoot)].join(path.delimiter),
      RUSTC_WRAPPER: "sccache",
      // Doc-mandated (architecture section 5): a dropped sccache connection must
      // fall through to plain rustc, not fail the build. Without this, a single
      // transient server disconnect (seen on Windows as error 10054) kills a
      // build that would otherwise have compiled fine uncached.
      SCCACHE_IGNORE_SERVER_IO_ERROR: "1",
      RIGHT_RELEASE_CACHE_OWNER: "rightkit-v2",
    };
  }
  const targetKind = kind === "release" ? "cargo-target" : "test-target";
  return {
    CARGO_TARGET_DIR: path.resolve(root, "cache", targetKind, platform, cacheKey),
    CARGO_HOME: path.resolve(root, "cache", "cargo-home"),
    SCCACHE_DIR: path.resolve(root, "cache", "sccache"),
    RUSTC_WRAPPER: "sccache",
    RIGHT_RELEASE_CACHE_OWNER: undefined,
  };
}

export function verifySealedRelease(sealedDir, options = {}) {
  const manifestPath = path.join(sealedDir, "release-manifest.json");
  if (!existsSync(manifestPath)) throw new Error(`sealed manifest missing: ${manifestPath}`);
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (manifest.schema !== 1) throw new Error(`unsupported sealed manifest schema: ${manifest.schema}`);
  if (manifest.kind != null && typeof manifest.kind !== "string") throw new Error("sealed manifest kind must be a string when present");
  if (!manifest.checkpoints?.includes("sealed")) throw new Error("release is not sealed");
  assertManifestRegistryEvidence(manifest, options.legacyReleaseIds ? { legacyReleaseIds: options.legacyReleaseIds } : undefined);
  for (const item of manifest.files ?? []) {
    if (typeof item.name !== "string" || path.basename(item.name) !== item.name || item.name === "." || item.name === "..") throw new Error("unsafe sealed artifact name");
    const file = path.join(sealedDir, item.name);
    if (!existsSync(file)) throw new Error(`sealed file missing: ${item.name}`);
    const bytes = readFileSync(file);
    const actualHash = createHash("sha256").update(bytes).digest("hex");
    if (actualHash !== item.sha256) throw new Error(`sealed file hash mismatch: ${item.name}`);
    if (bytes.length !== item.sizeBytes) throw new Error(`sealed file size mismatch: ${item.name}`);
    // Every seal and upload path verifies through here. A compressed installer hides the bytes, so
    // release.mjs also scans the raw binaries before packaging; this catches a payload that is not compressed.
    // Covers the QA control server marker, the wdio WebDriver plugin, remote-debugging and devtools strings, and
    // reads inside ZIP containers (MSIX/APPX). Compressed NSIS/DMG installers stay opaque here by nature.
    // An allow-listed app (devtools-policy.json) that recorded its content-only DevTools allowance may carry Tauri's devtools code.
    const surfaces = bufferQaFindings(bytes, { allowContentOnlyDevtools: sealedManifestAllowsContentDevtools(manifest) });
    if (surfaces.length) throw new Error(`sealed file contains a test-automation or debugging surface that must never ship (the QA control server and the like): ${item.name}: ${surfaces.map((f) => f.reason).join("; ")}`);
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
  inputHashes,
  cacheKey,
  ops,
}) {
  const releaseId = `${app}-${version}-${commit.slice(0, 8)}`;
  const platformDir = platform === "win" ? "windows" : platform === "mac" ? "mac" : platform;
  const sealedDir = path.join(root, ".right-release", "sealed", releaseId, platformDir);
  const sealedRoot = path.join(root, ".right-release", "sealed");
  if (existsSync(sealedRoot)) {
    for (const releaseEntry of readdirSync(sealedRoot, { withFileTypes: true })) {
      if (!releaseEntry.isDirectory()) continue;
      const releaseRoot = path.join(sealedRoot, releaseEntry.name);
      for (const platformEntry of readdirSync(releaseRoot, { withFileTypes: true })) {
        if (!platformEntry.isDirectory()) continue;
        const manifestPath = path.join(releaseRoot, platformEntry.name, "release-manifest.json");
        if (!existsSync(manifestPath)) continue;
        const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
        if (manifest.app === app && manifest.version === version && manifest.commit !== commit) {
          throw new Error(
            `${app} version ${version} is already sealed from commit ${manifest.commit} (${manifest.releaseId}); bump the version before rebuilding`,
          );
        }
      }
    }
  }
  if (existsSync(path.join(sealedDir, "release-manifest.json"))) {
    const sealed = verifySealedRelease(sealedDir);
    if (sealed.manifest.app !== app || sealed.manifest.version !== version || sealed.manifest.commit !== commit) {
      throw new Error(`sealed release identity mismatch: ${releaseId}`);
    }
    if (sealed.manifest.platform !== platform) throw new Error(`sealed release platform identity mismatch: ${releaseId}`);
    if (inputHashes && JSON.stringify(sealed.manifest.inputs) !== JSON.stringify(inputHashes)) {
      throw new Error(`sealed release input identity mismatch: ${releaseId}`);
    }
    if (cacheKey && sealed.manifest.cacheKey !== cacheKey) throw new Error(`sealed release cache identity mismatch: ${releaseId}`);
    return { status: "sealed", resumed: true, releaseId, sealedDir };
  }
  await ops.preflight?.({ root, app, version, commit, platform, requiredInputs, releaseId });
  await ops.prepare?.({ root, app, version, commit, platform, releaseId });
  await ops.build?.({ root, app, version, commit, platform, releaseId });
  await ops.sign?.({ root, app, version, commit, platform, releaseId });
  await ops.harden?.({ root, app, version, commit, platform, releaseId });
  await ops.preSeal?.({ root, app, version, commit, platform, releaseId, sealedDir });
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
