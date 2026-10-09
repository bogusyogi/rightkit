import {
  closeSync,
  existsSync,
  lstatSync,
  mkdirSync,
  openSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  statfsSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";

export const CACHE_SCHEMA = 1;
export const DEFAULT_TARGET_MAX_BYTES = 12 * 1024 ** 3;
export const DEFAULT_SCCACHE_MAX_BYTES = 32 * 1024 ** 3;
export const DEFAULT_DESIRED_FREE_BYTES = 60 * 1024 ** 3;
export const DEFAULT_HARD_FREE_BYTES = 25 * 1024 ** 3;
export const DEFAULT_BACKUP_RETENTION_DAYS = 7;
// Any build holds release -> suite slot -> GC -> entry lease; prune holds only GC.
export const CACHE_LOCK_ORDER = ["release", "suite-build-slot", "gc", "entry-lease"];

const MARKER = ".rightkit-cache-entry.json";

export function resolveSharedCacheRoot({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  if (platform === "darwin" || platform === "mac") {
    const override = env.RIGHT_RELEASE_CACHE_ROOT;
    if (override) {
      if (!isAbsoluteForPlatform(override, platform)) throw new Error("RIGHT_RELEASE_CACHE_ROOT must be an absolute path");
      return resolveForPlatform(override, platform);
    }
    throw new Error("RIGHT_RELEASE_CACHE_ROOT is required on macOS; set it to external storage");
  }
  return resolveStableRoot({
    platform,
    env,
    home,
    explicitOverride: env.RIGHT_RELEASE_CACHE_ROOT,
    overrideName: "RIGHT_RELEASE_CACHE_ROOT",
    win32EnvVar: "LOCALAPPDATA",
    win32Segments: ["RightSuite", "Cache", "release"],
    otherSegments: ["rightsuite", "release"],
  });
}

// resolveStableRoot centralizes platform-native root derivation for both cache roots
// (waste-tolerant, may split) and lock roots (must never split, or mutual exclusion
// silently breaks). Route every such root through this one primitive.
export function resolveStableRoot({
  platform = process.platform,
  env = process.env,
  home = os.homedir(),
  explicitOverride,
  overrideName,
  darwinSegments,
  win32EnvVar = "LOCALAPPDATA",
  win32Segments,
  otherSegments,
  otherBaseSegments = [".cache"],
} = {}) {
  if (explicitOverride) {
    if (!isAbsoluteForPlatform(explicitOverride, platform)) throw new Error(`${overrideName} must be an absolute path`);
    return resolveForPlatform(explicitOverride, platform);
  }
  if (platform === "win32" || platform === "win") {
    const local = env[win32EnvVar];
    if (!local || !isAbsoluteForPlatform(local, platform)) throw new Error(`${win32EnvVar} must be an absolute path for RightSuite`);
    return path.win32.resolve(local, ...win32Segments);
  }
  if (platform === "darwin" || platform === "mac") {
    return path.posix.join(home, ...darwinSegments);
  }
  // Never read a shell-profile-scoped variable (XDG_CACHE_HOME, XDG_CONFIG_HOME,
  // npm_config_cache, etc.) as an implicit default here: those differ between login
  // and non-login shells, so the same process can resolve to two different roots
  // depending on how it was launched. Always derive from the OS home directory instead.
  return path.posix.resolve(path.posix.join(home, ...otherBaseSegments), ...otherSegments);
}

export function resolveCacheLayout({ cacheRoot, platform, architecture, app, fingerprint, kind = "release" } = {}) {
  if (!cacheRoot || !path.isAbsolute(cacheRoot)) throw new Error("cacheRoot must be an absolute path");
  if (kind !== "release" && kind !== "test") throw new Error(`invalid cache target kind: ${kind}`);
  for (const [name, value] of Object.entries({ platform, architecture, app, fingerprint })) {
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(String(value || ""))) throw new Error(`invalid cache ${name}`);
  }
  const root = path.resolve(cacheRoot);
  const targetRoot = path.join(root, kind === "release" ? "targets" : "test-targets");
  const targetDir = path.join(targetRoot, platform, app, fingerprint);
  assertLexicalInside(targetRoot, targetDir);
  const id = `${kind === "release" ? "target" : "test-target"}:${platform}:${app}:${fingerprint}`;
  return {
    cacheRoot: root,
    targetRoot,
    targetDir,
    markerPath: path.join(targetDir, MARKER),
    cargoHome: path.join(root, "cargo-home"),
    sccacheDir: path.join(root, "sccache"),
    leasesDir: path.join(root, "leases"),
    leasePath: path.join(root, "leases", `${encodeURIComponent(id)}.json`),
    suiteSlotPath: path.join(root, "leases", "suite-build-slot.json"),
    gcLockPath: path.join(root, "gc", "gc.lock.json"),
    migrationDir: path.join(root, "migration"),
    platform,
    architecture,
    app,
    fingerprint,
    kind,
    id,
  };
}

export function readCachePolicy(env = process.env) {
  return {
    targetMaxBytes: readPositive(env.RIGHT_RELEASE_TARGET_MAX_BYTES, DEFAULT_TARGET_MAX_BYTES),
    sccacheMaxBytes: readPositive(env.RIGHT_RELEASE_SCCACHE_MAX_BYTES, DEFAULT_SCCACHE_MAX_BYTES),
    desiredFreeBytes: readPositive(env.RIGHT_RELEASE_DESIRED_FREE_BYTES, DEFAULT_DESIRED_FREE_BYTES),
    hardFreeBytes: readPositive(env.RIGHT_RELEASE_HARD_FREE_BYTES, DEFAULT_HARD_FREE_BYTES),
    backupRetentionDays: readPositive(env.RIGHT_RELEASE_BACKUP_RETENTION_DAYS, DEFAULT_BACKUP_RETENTION_DAYS),
  };
}

export function resolveSharedCacheIdentity({ cargoLockPath, cargoTomlPath, rustcVerbose, targetTriple, profile = "release", features } = {}) {
  const rustc = rustcVerbose ?? readRustcVerbose();
  const host = rustc.match(/^host:\s*(.+)$/m)?.[1];
  if (!host) throw new Error("rustc -vV did not report a host target triple");
  const target = targetTriple || host;
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(target)) throw new Error("invalid Cargo target triple");
  const architecture = target.split("-")[0];
  const cargoLock = cargoLockPath && existsSync(cargoLockPath) ? readFileSync(cargoLockPath, "utf8") : null;
  const cargoLockSha256 = cargoLock === null ? "none" : sha256(cargoLock);
  const cargoBuildGraphSha256 = cargoLock === null ? "none" : sha256(normalizeCargoLockForCache(cargoLock));
  const nativeFeatures = features ?? cargoNativeFeatures(cargoTomlPath);
  const payload = JSON.stringify({ cargoBuildGraphSha256, rustc, target, architecture, profile, features: [...nativeFeatures].sort() });
  return { cargoLockSha256, cargoBuildGraphSha256, rustc, target, architecture, profile, features: [...nativeFeatures].sort(), fingerprint: sha256(payload).slice(0, 16) };
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

/**
 * Cargo invalidates local crates from their manifests and sources inside a
 * reused target directory. Their package version is release metadata, not a
 * dependency artifact identity. Registry and Git packages retain every byte,
 * including source, version, and checksum.
 */
function normalizeCargoLockForCache(source) {
  const lines = String(source).replace(/\r\n?/g, "\n").split("\n");
  const normalized = [];
  for (let start = 0; start < lines.length;) {
    if (lines[start].trim() !== "[[package]]") {
      normalized.push(lines[start]);
      start += 1;
      continue;
    }
    let end = start + 1;
    while (end < lines.length && lines[end].trim() !== "[[package]]") end += 1;
    const block = lines.slice(start, end);
    const external = block.some((line) => /^\s*(?:source|checksum)\s*=/.test(line));
    normalized.push(...(external ? block : block.filter((line) => !/^\s*version\s*=/.test(line))));
    start = end;
  }
  return normalized.join("\n");
}

export function ensureCacheEntry({ layout, metadata = {}, now = new Date() } = {}) {
  requireLayout(layout);
  const root = resolvedRoot(layout.cacheRoot);
  const targetDir = safeEntryDirectory(root, layout);
  mkdirSafeDescendant(root, targetDir);
  assertPhysicalInside(path.join(root, layout.kind === "release" ? "targets" : "test-targets"), targetDir);
  const markerPath = path.join(targetDir, MARKER);
  let entry = readJson(markerPath);
  if (entry && !validEntry(entry, layout)) throw new Error(`invalid cache entry marker: ${markerPath}`);
  if (!entry) {
    const at = iso(now);
    entry = {
      schema: CACHE_SCHEMA, id: layout.id, kind: layout.kind, platform: layout.platform,
      architecture: layout.architecture, app: layout.app, fingerprint: layout.fingerprint,
      createdAt: at, lastUsedAt: at, lastSuccessfulBuildAt: null,
      toolchain: metadata.toolchain ?? { cargo: "unknown", rustc: "unknown", host: "unknown" },
    };
    atomicJson(markerPath, entry);
  }
  return entry;
}

export function acquireCacheLease({ layout, pid = process.pid, argv = process.argv, now = new Date() } = {}) {
  requireLayout(layout);
  const payload = { schema: CACHE_SCHEMA, id: layout.id, pid: Number(pid), argv: [...argv], createdAt: iso(now) };
  const root = resolvedRoot(layout.cacheRoot);
  const gcPayload = { schema: CACHE_SCHEMA, pid: Number(pid), argv: [...argv], createdAt: iso(now), order: CACHE_LOCK_ORDER };
  acquireAtomicLock(path.join(root, "gc", "gc.lock.json"), gcPayload, "global GC lock", now);
  try {
    ensureCacheEntry({ layout, now });
    acquireAtomicLock(layout.leasePath, payload, "cache entry lease", now);
    touchEntry(layout, now);
    return releaseFor(layout.leasePath, payload);
  } finally {
    releaseFor(path.join(root, "gc", "gc.lock.json"), gcPayload).release();
  }
}

export function acquireSuiteBuildSlot({ layout, pid = process.pid, argv = process.argv, waitMs = 900_000, now = new Date(), sleep = defaultSleep } = {}) {
  requireLayout(layout);
  const started = Date.now();
  const payload = { schema: CACHE_SCHEMA, pid: Number(pid), argv: [...argv], createdAt: iso(now) };
  const waits = { layer: "suite-build-slot", waitMs: 0, blockers: [] };
  for (;;) {
    try {
      acquireAtomicLock(layout.suiteSlotPath, payload, "suite build slot", new Date());
      waits.waitMs = Date.now() - started;
      return Object.assign(releaseFor(layout.suiteSlotPath, payload), { waits });
    } catch (error) {
      if (!/active/i.test(error.message)) throw error;
      if (!waits.blockers.includes(error.message)) waits.blockers.push(error.message);
      if (Date.now() - started >= waitMs) throw new Error(`${error.message}; suite build slot timed out after ${waitMs}ms without alternate-target fallback`);
      sleep(Math.min(250, Math.max(1, waitMs - (Date.now() - started))));
    }
  }
}

export function markCacheEntrySuccessful({ layout, now = new Date() } = {}) {
  requireLayout(layout);
  const entry = readJson(layout.markerPath);
  if (!validEntry(entry, layout)) throw new Error(`invalid cache entry marker: ${layout.markerPath}`);
  entry.lastUsedAt = iso(now);
  entry.lastSuccessfulBuildAt = iso(now);
  atomicJson(layout.markerPath, entry);
  return entry;
}

export function inspectCache({ cacheRoot, statfs = statfsSync, now = new Date() } = {}) {
  const root = readonlyRoot(cacheRoot);
  if (!root) return { cacheRoot: path.resolve(cacheRoot), entries: [], backups: [], targetBytes: 0, backupBytes: 0, cacheFreeBytes: 0, device: undefined, inspectedAt: iso(now) };
  const entries = [];
  for (const kind of ["targets", "test-targets"]) {
    const kindRoot = path.join(root, kind);
    if (!existsSync(kindRoot) || lstatSync(kindRoot).isSymbolicLink()) continue;
    for (const platformEntry of safeDirEntries(kindRoot)) for (const appEntry of safeDirEntries(platformEntry.path)) for (const fingerprintEntry of safeDirEntries(appEntry.path)) {
      const dir = fingerprintEntry.path;
      const markerPath = path.join(dir, MARKER);
      const marker = readJson(markerPath);
      const expectedKind = kind === "targets" ? "release" : "test";
      const layout = marker ? resolveCacheLayout({ cacheRoot: root, platform: marker.platform, architecture: marker.architecture, app: marker.app, fingerprint: marker.fingerprint, kind: expectedKind }) : null;
      if (!layout || layout.targetDir !== dir || !validEntry(marker, layout)) continue;
      const leasePath = path.join(root, "leases", `${encodeURIComponent(marker.id)}.json`);
      const lease = readJson(leasePath);
      const live = isLiveLease(lease, now);
      entries.push({ id: marker.id, dir, marker, bytes: directoryBytes(dir), leased: live, lease: live ? lease : null });
    }
  }
  const backups = [];
  for (const namespace of BACKUP_NAMESPACES) {
    const namespaceRoot = path.join(root, namespace);
    for (const entry of safeDirEntries(namespaceRoot)) {
      const stat = lstatSync(entry.path);
      backups.push({ id: `backup:${namespace}:${entry.name}`, namespace, name: entry.name, dir: entry.path, bytes: directoryBytes(entry.path), mtimeMs: stat.mtimeMs });
    }
  }
  const fs = statfs(root, { bigint: false });
  return { cacheRoot: root, entries, backups, targetBytes: entries.filter((x) => x.marker.kind === "release").reduce((sum, x) => sum + x.bytes, 0), backupBytes: backups.reduce((sum, x) => sum + x.bytes, 0), cacheFreeBytes: freeBytes(fs), device: fs.dev ?? safeDevice(root), inspectedAt: iso(now) };
}

export function inspectWriteVolumes({ repoRoot, vaultRoot, cacheRoot, statfs = statfsSync } = {}) {
  const paths = [
    { role: "repo", path: path.resolve(repoRoot || vaultRoot) },
    { role: "cache", path: path.resolve(cacheRoot) },
  ];
  const volumes = [];
  for (const item of paths) {
    const fs = statfs(item.path, { bigint: false });
    const device = fs.dev ?? safeDevice(item.path) ?? `${fs.type}:${item.path}`;
    const existing = volumes.find((volume) => String(volume.device) === String(device));
    if (existing) { existing.roles.push(item.role); existing.paths.push(item.path); continue; }
    volumes.push({ device, roles: [item.role], paths: [item.path], freeBytes: freeBytes(fs) });
  }
  return volumes;
}

export function assertWriteVolumeFloors({ volumes, policy = readCachePolicy(), configuredRepoMinimumBytes = DEFAULT_HARD_FREE_BYTES } = {}) {
  for (const volume of volumes ?? []) {
    const repoRequired = volume.roles.includes("repo") ? Math.max(configuredRepoMinimumBytes, DEFAULT_HARD_FREE_BYTES) : 0;
    const cacheRequired = volume.roles.includes("cache") ? policy.hardFreeBytes : 0;
    const required = Math.max(repoRequired, cacheRequired);
    if (volume.freeBytes < required) throw new Error(`insufficient free space on ${volume.roles.join("+")} volume ${volume.device}: ${(volume.freeBytes / 1024 ** 3).toFixed(1)} GiB free, ${(required / 1024 ** 3).toFixed(1)} GiB required`);
  }
  return true;
}

export function planCachePrune({ snapshot, policy = readCachePolicy(), protectedEntryIds = new Set() } = {}) {
  const protectedIds = new Set(protectedEntryIds);
  const candidates = (snapshot.entries ?? [])
    .filter((entry) => entry.marker?.kind === "release" && !entry.leased && !protectedIds.has(entry.id))
    .sort((a, b) => String(a.marker.lastSuccessfulBuildAt ?? a.marker.lastUsedAt ?? a.marker.createdAt).localeCompare(String(b.marker.lastSuccessfulBuildAt ?? b.marker.lastUsedAt ?? b.marker.createdAt)) || a.id.localeCompare(b.id));
  let bytes = snapshot.targetBytes ?? 0;
  let free = snapshot.cacheFreeBytes ?? Infinity;
  const selected = [];
  const backupCutoff = new Date(snapshot.inspectedAt ?? Date.now()).getTime() - policy.backupRetentionDays * 86_400_000;
  for (const entry of snapshot.backups ?? []) {
    if (entry.mtimeMs < backupCutoff) selected.push({ id: entry.id, dir: entry.dir, bytes: entry.bytes, reason: "expired-backup" });
  }
  for (const entry of candidates) {
    if (bytes <= policy.targetMaxBytes && free >= policy.desiredFreeBytes) break;
    selected.push({ id: entry.id, dir: entry.dir, bytes: entry.bytes, reason: bytes > policy.targetMaxBytes ? "target-cap" : "free-space" });
    bytes -= entry.bytes; free += entry.bytes;
  }
  return { schema: CACHE_SCHEMA, cacheRoot: snapshot.cacheRoot, candidateIds: selected.map((entry) => entry.id), candidates: selected, estimatedReclaimedBytes: selected.reduce((sum, entry) => sum + entry.bytes, 0), targetBytesAfter: bytes, cacheFreeBytesAfter: free };
}

export function applyCachePrune({ plan, cacheRoot, dryRun = true, beforeDelete } = {}) {
  const result = { candidateIds: (plan?.candidates ?? []).map((entry) => entry.id), removedIds: [], reclaimedBytes: 0, dryRun };
  if (dryRun) return result;
  const root = resolvedRoot(cacheRoot || plan?.cacheRoot);
  const lockPath = path.join(root, "gc", "gc.lock.json");
  const lockPayload = { schema: CACHE_SCHEMA, pid: process.pid, argv: process.argv, createdAt: iso(new Date()), order: CACHE_LOCK_ORDER };
  acquireAtomicLock(lockPath, lockPayload, "global GC lock", new Date());
  try {
    for (const candidate of plan?.candidates ?? []) {
      if (!safePruneCandidate(root, candidate)) continue;
      beforeDelete?.(candidate);
      if (!safePruneCandidate(root, candidate)) continue;
      if (!dryRun) {
        rmSync(realpathSync(candidate.dir), { recursive: true, force: false, maxRetries: 2 });
        result.removedIds.push(candidate.id); result.reclaimedBytes += candidate.bytes ?? 0;
      }
    }
  } finally {
    releaseFor(lockPath, lockPayload).release();
  }
  return result;
}

export function migrateLegacyCache({ legacyRoot, legacyTargetRoot, legacyCargoHome, legacyReleaseLockPath, layout, app, dryRun = true, waitMs = 0, onPostMove, writeMigrationReceipt = atomicJson } = {}) {
  requireLayout(layout);
  if (app !== layout.app) throw new Error("legacy cache app must match cache layout");
  const root = dryRun ? path.resolve(layout.cacheRoot) : resolvedRoot(layout.cacheRoot);
  const receipt = path.join(root, "migration", `${app}.json`);
  const existing = readJson(receipt);
  if (existing) return { ...existing, resumed: true, dryRun };
  const linkPath = path.resolve(legacyRoot);
  const target = layout.targetDir;
  const initial = resolveLegacyMigrationSource({ linkPath, legacyTargetRoot, layout });
  const outcome = { schema: CACHE_SCHEMA, app, legacyRoot: initial.source, target, dryRun, moved: false, coldCache: false, cargoHomeMoved: false, deferredLegacyCargoHome: null, createdAt: iso(new Date()) };
  if (initial.coldCache) return persistMigrationOutcome({ outcome: { ...outcome, coldCache: true }, receipt, root, dryRun });
  if (pathEntryExists(target)) throw new Error(`cache migration target already exists: ${target}`);
  if (dryRun) return outcome;

  let suiteSlot;
  let gcExclusion;
  try {
    suiteSlot = acquireSuiteBuildSlot({ layout, pid: process.pid, argv: process.argv, waitMs });
    gcExclusion = acquireMigrationGcExclusion({ layout });
    assertMigrationIdle({ legacyReleaseLockPath, layout });
    const { source } = resolveLegacyMigrationSource({ linkPath, legacyTargetRoot, layout });
    const actualTarget = safeEntryDirectory(root, layout);
    const targetParent = path.dirname(actualTarget);
    mkdirSafeDescendant(root, targetParent);
    const sourceDevice = safeDevice(source);
    if (sourceDevice !== undefined && sourceDevice !== safeDevice(targetParent)) return persistMigrationOutcome({ outcome: { ...outcome, coldCache: true }, receipt, root, dryRun });

    let movedTarget = false;
    let createdMarker = false;
    let movedCargoHome = false;
    let replacedEmptyCargoHome = false;
    const sharedCargoHome = path.join(root, "cargo-home");
    try {
      renameSync(source, actualTarget); movedTarget = true;
      onPostMove?.("after-target-move");
      createdMarker = !existsSync(path.join(actualTarget, MARKER));
      ensureCacheEntry({ layout });
      onPostMove?.("after-entry-marker");
      if (legacyCargoHome && existsSync(legacyCargoHome)) {
        if (lstatSync(legacyCargoHome).isSymbolicLink()) throw new Error(`legacy Cargo home migration refuses symlink source: ${legacyCargoHome}`);
        if (!existsSync(sharedCargoHome)) {
          renameSync(legacyCargoHome, sharedCargoHome); outcome.cargoHomeMoved = movedCargoHome = true;
        } else if (isEmptyDirectory(sharedCargoHome)) {
          rmSync(sharedCargoHome, { recursive: true, force: false }); replacedEmptyCargoHome = true;
          renameSync(legacyCargoHome, sharedCargoHome); outcome.cargoHomeMoved = movedCargoHome = true;
        } else outcome.deferredLegacyCargoHome = path.resolve(legacyCargoHome);
      }
      onPostMove?.("after-cargo-home-move");
      mkdirSafeDescendant(root, path.join(root, "migration"));
      onPostMove?.("before-receipt");
      writeMigrationReceipt(receipt, { ...outcome, moved: true, dryRun: false, completedAt: iso(new Date()) });
      outcome.moved = true;
      if (linkPath !== source) { try { if (lstatSync(linkPath).isSymbolicLink()) unlinkSync(linkPath); } catch { /* already absent */ } }
    } catch (error) {
      if (movedCargoHome && existsSync(sharedCargoHome) && !existsSync(legacyCargoHome)) renameSync(sharedCargoHome, legacyCargoHome);
      if (replacedEmptyCargoHome && !existsSync(sharedCargoHome)) mkdirSafeDescendant(root, sharedCargoHome);
      if (movedTarget && existsSync(actualTarget) && !existsSync(source)) {
        if (createdMarker) safeUnlink(path.join(actualTarget, MARKER));
        renameSync(actualTarget, source);
      }
      throw error;
    }
    return outcome;
  } finally {
    gcExclusion?.release();
    suiteSlot?.release();
  }
}

function persistMigrationOutcome({ outcome, receipt, root, dryRun }) { if (!dryRun) { mkdirSafeDescendant(root, path.join(root, "migration")); atomicJson(receipt, { ...outcome, dryRun: false, completedAt: iso(new Date()) }); } return outcome; }

function resolveLegacyMigrationSource({ linkPath, legacyTargetRoot, layout }) {
  if (!pathEntryExists(linkPath)) return { source: linkPath, coldCache: true };
  if (!lstatSync(linkPath).isSymbolicLink()) return { source: linkPath, coldCache: false };
  if (!legacyTargetRoot) throw new Error(`legacy cache migration refuses unexpected target link: ${linkPath}`);
  const declared = path.resolve(legacyTargetRoot);
  const legacyCargoTargetRoot = path.resolve(declared, "..", "..");
  const exactFingerprintDir = path.join(legacyCargoTargetRoot, layout.platform, layout.fingerprint);
  if (declared !== exactFingerprintDir) throw new Error(`legacy cache migration refuses unexpected target link: ${linkPath}`);
  let physical;
  let expected;
  let legacyRoot;
  try {
    physical = realpathSync(linkPath);
    expected = realpathSync(exactFingerprintDir);
    legacyRoot = realpathSync(legacyCargoTargetRoot);
  } catch { throw new Error(`legacy cache migration refuses unexpected target link: ${linkPath}`); }
  try { assertLexicalInside(legacyRoot, expected); } catch { throw new Error(`legacy cache migration refuses unexpected target link: ${linkPath}`); }
  if (physical !== expected) throw new Error(`legacy cache migration refuses unexpected target link: ${linkPath}`);
  return { source: physical, coldCache: false };
}

function acquireMigrationGcExclusion({ layout }) {
  const payload = { schema: CACHE_SCHEMA, pid: process.pid, argv: process.argv, createdAt: iso(new Date()), order: CACHE_LOCK_ORDER };
  acquireAtomicLock(layout.gcLockPath, payload, "global GC lock", new Date());
  return releaseFor(layout.gcLockPath, payload);
}

function assertMigrationIdle({ legacyReleaseLockPath, layout }) {
  if (legacyReleaseLockPath && pathEntryExists(legacyReleaseLockPath)) throw new Error(`cache migrate refused: legacy release lock is live: ${legacyReleaseLockPath}`);
  for (const entry of safeLeaseEntries(layout.leasesDir)) {
    if (entry.name === path.basename(layout.suiteSlotPath)) continue;
    if (entry.symbolicLink) throw new Error(`cache migrate refused: unsafe shared lease path: ${entry.path}`);
    if (!entry.file) continue;
    const lease = readJson(entry.path);
    if (isLiveLease(lease)) throw new Error(`cache migrate refused: shared entry lease is live: pid ${lease.pid}`);
  }
}

function requireLayout(layout) { if (!layout?.targetDir || !layout?.cacheRoot) throw new Error("cache layout is required"); }
function readPositive(value, fallback) { if (value === undefined || value === "") return fallback; const number = Number(value); if (!Number.isFinite(number) || number <= 0) throw new Error("cache policy values must be positive byte counts"); return number; }
function iso(value) { return new Date(value).toISOString(); }
function pathEntryExists(file) { try { lstatSync(file); return true; } catch (error) { if (error?.code === "ENOENT") return false; throw error; } }
function resolvedRoot(cacheRoot) { if (!cacheRoot || !path.isAbsolute(cacheRoot)) throw new Error("cacheRoot must be an absolute path"); if (existsSync(cacheRoot) && lstatSync(cacheRoot).isSymbolicLink()) throw new Error(`unsafe symlink cache root: ${cacheRoot}`); mkdirSync(cacheRoot, { recursive: true }); if (lstatSync(cacheRoot).isSymbolicLink()) throw new Error(`unsafe symlink cache root: ${cacheRoot}`); return realpathSync(cacheRoot); }
function readonlyRoot(cacheRoot) { if (!cacheRoot || !path.isAbsolute(cacheRoot)) throw new Error("cacheRoot must be an absolute path"); if (!existsSync(cacheRoot)) return null; if (lstatSync(cacheRoot).isSymbolicLink()) throw new Error(`unsafe symlink cache root: ${cacheRoot}`); return realpathSync(cacheRoot); }
function atomicJson(file, value) { mkdirSync(path.dirname(file), { recursive: true }); const temp = `${file}.tmp-${process.pid}-${Math.random().toString(16).slice(2)}`; writeFileSync(temp, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 }); renameSync(temp, file); }
function readJson(file) { try { return JSON.parse(readFileSync(file, "utf8")); } catch { return null; } }
function validEntry(entry, layout) { return Boolean(entry && entry.schema === CACHE_SCHEMA && entry.id === layout.id && entry.kind === layout.kind && entry.platform === layout.platform && entry.architecture === layout.architecture && entry.app === layout.app && entry.fingerprint === layout.fingerprint && typeof entry.createdAt === "string" && typeof entry.lastUsedAt === "string" && Object.prototype.hasOwnProperty.call(entry, "lastSuccessfulBuildAt") && entry.toolchain && typeof entry.toolchain === "object"); }
function assertLexicalInside(parent, child) { const relative = path.relative(path.resolve(parent), path.resolve(child)); if (!relative || relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) throw new Error("cache path escapes its root"); }
function assertPhysicalInside(parent, child) { const parentReal = realpathSync(parent); const childReal = realpathSync(child); assertLexicalInside(parentReal, childReal); }
function safeDirEntries(dir) { try { return readdirSync(dir, { withFileTypes: true }).filter((entry) => entry.isDirectory() && !entry.isSymbolicLink()).map((entry) => ({ name: entry.name, path: path.join(dir, entry.name) })); } catch { return []; } }
function safeLeaseEntries(dir) { try { return readdirSync(dir, { withFileTypes: true }).map((entry) => ({ name: entry.name, path: path.join(dir, entry.name), file: entry.isFile(), symbolicLink: entry.isSymbolicLink() })); } catch (error) { if (error?.code === "ENOENT") return []; throw error; } }
function directoryBytes(dir) { let bytes = 0; const walk = (item) => { const stat = lstatSync(item); if (stat.isSymbolicLink()) return; if (stat.isDirectory()) for (const child of readdirSync(item)) walk(path.join(item, child)); else bytes += stat.size; }; try { walk(dir); } catch { return 0; } return bytes; }
function freeBytes(fs) { return Number(fs.bavail) * Number(fs.bsize); }
function safeDevice(dir) { try { return statSync(dir).dev; } catch { return undefined; } }
function isEmptyDirectory(dir) { try { return readdirSync(dir).length === 0; } catch { return false; } }
function isLiveLease(lease) { return Boolean(lease && Number.isInteger(Number(lease.pid)) && Number(lease.pid) > 0 && processIsAlive(lease.pid)); }
function processIsAlive(pid) { try { process.kill(Number(pid), 0); return true; } catch (error) { return error?.code === "EPERM"; } }
function safeUnlink(file) { try { unlinkSync(file); } catch { /* already gone */ } }
function acquireAtomicLock(file, payload, label, now) {
  mkdirSync(path.dirname(file), { recursive: true });
  try { const fd = openSync(file, "wx", 0o600); writeFileSync(fd, `${JSON.stringify(payload)}\n`); closeSync(fd); return; } catch (error) {
    if (error.code !== "EEXIST") throw error;
    const holder = readJson(file);
    if (!isLiveLease(holder, now)) { safeUnlink(file); return acquireAtomicLock(file, payload, label, now); }
    throw new Error(`${label} active: pid ${holder.pid}`);
  }
}
function releaseFor(file, payload) { let released = false; return { release() { if (released) return; released = true; const current = readJson(file); if (current?.pid === payload.pid && current?.createdAt === payload.createdAt) safeUnlink(file); } }; }
function touchEntry(layout, now) { const entry = readJson(layout.markerPath); if (!validEntry(entry, layout)) throw new Error(`invalid cache entry marker: ${layout.markerPath}`); entry.lastUsedAt = iso(now); atomicJson(layout.markerPath, entry); }
function defaultSleep(ms) { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms); }
const BACKUP_NAMESPACES = ["recovery", "manual-backups", "dev-target-backups", "legacy-backups"];
function safePruneCandidate(root, candidate) {
  if (!candidate?.dir || path.resolve(candidate.dir) === root) return false;
  if (!existsSync(candidate.dir) || lstatSync(candidate.dir).isSymbolicLink()) return false;
  const dir = realpathSync(candidate.dir);
  if (candidate.id?.startsWith("backup:")) {
    const [, namespace, name] = candidate.id.split(":");
    if (!BACKUP_NAMESPACES.includes(namespace) || !name || path.basename(dir) !== name) return false;
    const backupRoot = path.join(root, namespace);
    try { return path.dirname(dir) === backupRoot && realpathSync(backupRoot) === backupRoot; } catch { return false; }
  }
  const targets = [path.join(root, "targets"), path.join(root, "test-targets")];
  const targetRoot = targets.find((item) => { try { assertLexicalInside(item, dir); return true; } catch { return false; } });
  if (!targetRoot) return false;
  try {
    assertPhysicalInside(targetRoot, dir);
    const marker = readJson(path.join(dir, MARKER));
    const kind = targetRoot.endsWith(`${path.sep}targets`) ? "release" : "test";
    const layout = marker && resolveCacheLayout({ cacheRoot: root, platform: marker.platform, architecture: marker.architecture, app: marker.app, fingerprint: marker.fingerprint, kind });
    return Boolean(layout && layout.targetDir === dir && marker.id === candidate.id && validEntry(marker, layout) && !isLiveLease(readJson(layout.leasePath), new Date()));
  } catch { return false; }
}
function safeEntryDirectory(root, layout) { return path.join(root, layout.kind === "release" ? "targets" : "test-targets", layout.platform, layout.app, layout.fingerprint); }
function mkdirSafeDescendant(root, dir) {
  assertLexicalInside(root, dir);
  let current = root;
  for (const part of path.relative(root, dir).split(path.sep)) {
    current = path.join(current, part);
    if (existsSync(current)) { if (lstatSync(current).isSymbolicLink()) throw new Error(`unsafe symlink cache ancestor: ${current}`); }
    else mkdirSync(current);
  }
}
function isWindowsAbsolute(value) { return /^[A-Za-z]:[\\/]/.test(value) || /^\\\\/.test(value); }
function isAbsoluteForPlatform(value) { return isWindowsAbsolute(value) || path.posix.isAbsolute(value); }
function resolveForPlatform(value) { return isWindowsAbsolute(value) ? path.win32.resolve(value) : path.posix.resolve(value); }
function readRustcVerbose() { const result = spawnSync("rustc", ["-vV"], { encoding: "utf8", windowsHide: true }); if (result.status !== 0) throw new Error(`rustc -vV failed: ${(result.stderr || result.error?.message || `exit ${result.status}`).trim()}`); return result.stdout.trim(); }
function cargoNativeFeatures(cargoTomlPath) { if (!cargoTomlPath || !existsSync(cargoTomlPath)) return []; return [...readFileSync(cargoTomlPath, "utf8").matchAll(/features\s*=\s*\[([^\]]+)\]/g)].flatMap((match) => match[1].match(/"([^"]+)"/g) ?? []).map((value) => value.slice(1, -1)).filter((value) => /sqlcipher|openssl/i.test(value)); }

/**
 * SwiftPM counterpart of resolveSharedCacheIdentity: identity is the Swift toolchain, Package.resolved
 * (dependency pins; absent for dependency-free packages), target architecture and profile. No Rust inputs.
 */
export function resolveSwiftCacheIdentity({ swiftVersion, packageResolvedPath, architecture, profile = "release" } = {}) {
  if (!swiftVersion || !String(swiftVersion).trim()) throw new Error("swift --version did not report a toolchain");
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(architecture ?? "")) throw new Error("invalid Swift target architecture");
  const resolved = packageResolvedPath && existsSync(packageResolvedPath) ? readFileSync(packageResolvedPath, "utf8").replace(/\r\n?/g, "\n") : null;
  const packageResolvedSha256 = resolved === null ? "none" : sha256(resolved);
  const swift = String(swiftVersion).trim();
  return { projectType: "swiftpm", packageResolvedSha256, swift, target: architecture, architecture, profile, features: [],
    fingerprint: sha256(JSON.stringify({ projectType: "swiftpm", packageResolvedSha256, swift, architecture, profile })).slice(0, 16) };
}
