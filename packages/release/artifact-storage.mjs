import {
  existsSync,
  copyFileSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";

export const ARTIFACT_MARKER_SCHEMA = 1;
export const DEFAULT_MANAGED_ROOT = process.platform === "win32"
  ? "D:\\.rightkit-managed"
  : "/Volumes/D/.rightkit-managed";

const SAFE_COMPONENT = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/;

export function resolveManagedRoot({ env = process.env } = {}) {
  const value = env.RIGHTKIT_MANAGED_ROOT || DEFAULT_MANAGED_ROOT;
  return path.resolve(value);
}

export function resolveArtifactRoot({ app, managedRoot } = {}) {
  assertComponent(app, "app");
  return path.join(path.resolve(managedRoot ?? resolveManagedRoot()), "artifacts", app);
}

export function resolveArtifactDir({ app, version, managedRoot } = {}) {
  assertComponent(app, "app");
  assertComponent(version, "version");
  return path.join(resolveArtifactRoot({ app, managedRoot }), version);
}

/** Resolve one declared installer/updater path, allowing managed output. */
export function resolveArtifactInput({ root, app, version, declared, managedRoot } = {}) {
  const local = path.resolve(root, declared);
  if (existsSync(local)) return local;
  const managed = path.join(resolveArtifactDir({ app, version, managedRoot }), path.basename(declared));
  return existsSync(managed) ? managed : local;
}

export function markerPath({ app, version, managedRoot } = {}) {
  return path.join(resolveArtifactDir({ app, version, managedRoot }), ".rightkit-artifact.json");
}

export function publishedPointerPath({ app, managedRoot } = {}) {
  return path.join(resolveArtifactRoot({ app, managedRoot }), "published.json");
}

/**
 * Create or refresh the lease marker before a build writes managed artifacts.
 * Marker fields intentionally remain the small cross-process contract.
 */
export function beginArtifactLease({ app, version, managedRoot, now = Date.now(), ownerPid = process.pid, leaseMs = 2 * 60 * 60 * 1000 } = {}) {
  if (!Number.isFinite(now) || !Number.isSafeInteger(ownerPid) || ownerPid < 1 || !Number.isFinite(leaseMs) || leaseMs <= 0) {
    throw new TypeError("artifact lease requires finite now/leaseMs and positive ownerPid");
  }
  const root = resolveArtifactRoot({ app, managedRoot });
  ensureDirectory(root);
  const lock = acquireRetentionLock(root, now);
  if (!lock) throw new Error(`artifact retention lock is held; cannot lease ${app}@${version}`);
  try {
    const directory = resolveArtifactDir({ app, version, managedRoot });
    ensureDirectory(directory);
    const file = markerPath({ app, version, managedRoot });
    assertSafeMutation(file);
    if (existsSync(file)) {
      const previous = readArtifactMarker(file);
      if (isActiveLease(previous, now) && previous.ownerPid !== ownerPid) {
        throw new Error(`artifact lease is held by PID ${previous.ownerPid}: ${app}@${version}`);
      }
    }
    const marker = { schemaVersion: ARTIFACT_MARKER_SCHEMA, app, version, createdAt: now, ownerPid, leaseUntil: now + leaseMs };
    writeJson(file, marker);
    return marker;
  } finally {
    lock.release();
  }
}

export function completeArtifactLease({ app, version, managedRoot, completedAt = Date.now() } = {}) {
  if (!Number.isFinite(completedAt)) throw new TypeError("completedAt must be finite");
  const root = resolveArtifactRoot({ app, managedRoot });
  const lock = acquireRetentionLock(root, completedAt);
  if (!lock) throw new Error(`artifact retention lock is held; cannot complete ${app}@${version}`);
  try {
    const file = markerPath({ app, version, managedRoot });
    if (!existsSync(file)) throw new Error(`artifact marker missing: ${file}`);
    const marker = readArtifactMarker(file);
    if (isActiveLease(marker, completedAt) && marker.ownerPid !== process.pid) throw new Error(`artifact lease is owned by PID ${marker.ownerPid}: ${app}@${version}`);
    const completed = { schemaVersion: ARTIFACT_MARKER_SCHEMA, app, version, createdAt: marker.createdAt, ownerPid: 0, leaseUntil: 0, completedAt };
    writeJson(file, completed);
    return completed;
  } finally {
    lock.release();
  }
}

export function readArtifactMarker(file) {
  const marker = JSON.parse(readFileSync(file, "utf8"));
  if (marker?.schemaVersion !== ARTIFACT_MARKER_SCHEMA) throw new Error(`unsupported artifact marker schema: ${file}`);
  for (const key of ["app", "version"]) assertComponent(marker[key], key);
  for (const key of ["createdAt", "ownerPid", "leaseUntil"]) {
    if (!Number.isFinite(marker[key])) throw new Error(`artifact marker ${key} must be numeric: ${file}`);
  }
  if (marker.completedAt != null && !Number.isFinite(marker.completedAt)) throw new Error(`artifact marker completedAt must be numeric: ${file}`);
  return marker;
}

/**
 * Read publication state. Missing or malformed state is unknown and must keep
 * every artifact. An explicit empty versions array means known never-published.
 */
export function readPublishedVersions({ app, managedRoot } = {}) {
  const file = publishedPointerPath({ app, managedRoot });
  if (!existsSync(file)) return { state: "unknown", versions: [], path: file };
  try {
    const pointer = JSON.parse(readFileSync(file, "utf8"));
    if (pointer.platforms != null && (!pointer.platforms || typeof pointer.platforms !== "object" || Array.isArray(pointer.platforms))) {
      return { state: "unknown", versions: [], platforms: {}, path: file, reason: "published pointer has invalid platforms" };
    }
    if (!Array.isArray(pointer.versions) || pointer.versions.some((version) => !SAFE_COMPONENT.test(String(version)))) {
      return { state: "unknown", versions: [], path: file, reason: "published pointer has invalid versions" };
    }
    const platforms = Object.fromEntries(Object.entries(pointer.platforms ?? {}).map(([platform, version]) => {
      if (!SAFE_COMPONENT.test(platform)) throw new Error("published pointer has invalid platform");
      return [platform, String(version)];
    }));
    if (Object.values(platforms).some((version) => !SAFE_COMPONENT.test(version))) return { state: "unknown", versions: [], platforms: {}, path: file, reason: "published pointer has invalid platform version" };
    return { state: "known", versions: [...new Set(pointer.versions.map(String))], platforms, path: file };
  } catch (error) {
    return { state: "unknown", versions: [], path: file, reason: error.message };
  }
}

/** Record only a publication that completed remote verification. */
export function recordPublishedVersion({ app, version, platform = "default", managedRoot, now = Date.now() } = {}) {
  assertComponent(version, "version");
  const root = resolveArtifactRoot({ app, managedRoot });
  ensureDirectory(root);
  const lock = acquireRetentionLock(root, now);
  if (!lock) throw new Error("retention lock is held by a live owner; publication state cannot be updated");
  try {
    const current = readPublishedVersions({ app, managedRoot });
    if (current.state === "unknown" && existsSync(current.path)) throw new Error(`published state is unknown; refusing to overwrite ${current.path}`);
    if (!SAFE_COMPONENT.test(String(platform))) throw new Error("platform must be a safe release component");
    const platforms = { ...current.platforms, [platform]: version };
    const versions = [...new Set(Object.values(platforms))];
    // Pointer contract is deliberately tiny; publication time is already
    // evidenced by upload receipt and marker files.
    writeJson(current.path, { versions, platforms });
    return { state: "known", versions, platforms, path: current.path };
  } finally {
    lock.release();
  }
}

/**
 * Keep three newest completed artifact versions plus every published version.
 * Unknown publication state never deletes data. Invalid/incomplete markers are
 * also retained because they may belong to an interrupted build.
 */
export function retainArtifacts({ app, managedRoot, now = Date.now(), keep = 3, remove = true } = {}) {
  if (!Number.isInteger(keep) || keep < 0) throw new TypeError("keep must be a non-negative integer");
  const root = resolveArtifactRoot({ app, managedRoot });
  assertSafeMutation(root);
  if (!existsSync(root)) return { state: "known", removed: [], retained: [] };
  const lock = acquireRetentionLock(root, now);
  if (!lock) return { state: "unknown", removed: [], retained: [], reason: "retention lock is held by a live owner" };

  try {
    const publication = readPublishedVersions({ app, managedRoot });
    if (publication.state !== "known") return { state: "unknown", removed: [], retained: [], reason: publication.reason ?? "publication state is unknown" };
    const candidates = [];
    for (const entry of readdirSync(root, { withFileTypes: true })) {
      if (!entry.isDirectory() || !SAFE_COMPONENT.test(entry.name)) continue;
      const directory = path.join(root, entry.name);
      const markerFile = path.join(directory, ".rightkit-artifact.json");
      if (!existsSync(markerFile)) continue;
      try {
        const marker = readArtifactMarker(markerFile);
        if (marker.app !== app || marker.version !== entry.name || marker.completedAt == null) continue;
        if (isActiveLease(marker, now)) continue;
        candidates.push({ version: entry.name, directory, markerFile, completedAt: marker.completedAt });
      } catch {
        // Preserve malformed state. A future doctor can repair it explicitly.
      }
    }
    candidates.sort((left, right) => right.completedAt - left.completedAt || right.version.localeCompare(left.version));
    const retainedVersions = new Set([
      ...candidates.slice(0, keep).map((candidate) => candidate.version),
      ...publication.versions,
    ]);
    const removed = [];
    for (const candidate of candidates) {
      if (retainedVersions.has(candidate.version)) continue;
      // Re-read under the retention lock immediately before deletion. A build
      // may have renewed this lease after the first directory scan.
      if (!existsSync(candidate.markerFile)) continue;
      const current = readArtifactMarker(candidate.markerFile);
      if (isActiveLease(current, now) || current.completedAt == null) continue;
      assertSafeMutation(candidate.directory);
      if (remove) rmSync(candidate.directory, { recursive: true, force: false });
      removed.push(candidate.version);
    }
    return { state: "known", removed, retained: [...retainedVersions].sort(), published: publication.versions };
  } finally {
    lock.release();
  }
}

/** Atomically copy sealed/source files into their managed version directory. */
export function materializeArtifacts({ app, version, files, managedRoot, now = Date.now(), leaseMs } = {}) {
  if (!Array.isArray(files) || files.length === 0) throw new TypeError("artifact files are required");
  const root = resolveArtifactRoot({ app, managedRoot });
  ensureDirectory(root);
  const lock = acquireRetentionLock(root, now);
  if (!lock) throw new Error(`artifact retention lock is held; cannot materialize ${app}@${version}`);
  const directory = resolveArtifactDir({ app, version, managedRoot });
  const markerFile = markerPath({ app, version, managedRoot });
  try {
    if (existsSync(markerFile)) {
      const marker = readArtifactMarker(markerFile);
      if (isActiveLease(marker, now) && marker.ownerPid !== process.pid) {
        throw new Error(`artifact lease is owned by PID ${marker.ownerPid}: ${app}@${version}`);
      }
    }
  } catch (error) {
    lock.release();
    throw error;
  }
  const temp = `${directory}.tmp-${process.pid}`;
  try {
    assertSafeMutation(directory);
    assertSafeMutation(temp);
    rmSync(temp, { recursive: true, force: true });
    ensureDirectory(temp);
    for (const entry of files) {
      const source = path.resolve(entry.source ?? entry.file ?? "");
      const name = path.basename(entry.name ?? source);
      if (!source || !existsSync(source) || !statSync(source).isFile()) throw new Error(`artifact source missing: ${source}`);
      assertSafeMutation(source);
      if (!SAFE_COMPONENT.test(name)) throw new Error(`unsafe artifact filename: ${name}`);
      copyFileSync(source, path.join(temp, name));
    }
    if (existsSync(directory)) {
      for (const entry of readdirSync(directory, { withFileTypes: true })) {
        if (entry.name === ".rightkit-artifact.json" || !entry.isFile()) continue;
        const existing = path.join(directory, entry.name);
        assertSafeMutation(existing);
        if (!existsSync(path.join(temp, entry.name))) copyFileSync(existing, path.join(temp, entry.name));
      }
    }
    mkdirSync(directory, { recursive: true });
    for (const entry of readdirSync(temp, { withFileTypes: true })) {
      if (entry.isFile()) {
        const destination = path.join(directory, entry.name);
        assertSafeMutation(destination);
        rmSync(destination, { force: true });
        renameSync(path.join(temp, entry.name), destination);
      }
    }
  } catch (error) {
    safeRemove(temp);
    throw error;
  } finally {
    lock.release();
  }
  return directory;
}

function writeJson(file, value) {
  ensureDirectory(path.dirname(file));
  assertSafeMutation(file);
  const temp = `${file}.tmp-${process.pid}`;
  assertSafeMutation(temp);
  writeFileSync(temp, `${JSON.stringify(value, null, 2)}\n`);
  renameSync(temp, file);
}

function assertComponent(value, label) {
  if (!SAFE_COMPONENT.test(String(value ?? ""))) throw new Error(`${label} must be a safe release component`);
}

function ensureDirectory(directory) {
  assertSafeMutation(directory);
  mkdirSync(directory, { recursive: true });
}

function assertSafeMutation(target) {
  const absolute = path.resolve(target);
  const root = path.parse(absolute).root;
  const relative = path.relative(root, absolute);
  let current = root;
  for (const segment of relative ? relative.split(path.sep) : []) {
    current = path.join(current, segment);
    try {
      if (lstatSync(current).isSymbolicLink()) throw new Error(`refusing filesystem mutation through symlink: ${current}`);
    } catch (error) {
      if (error.code === "ENOENT") continue;
      throw error;
    }
  }
}

function safeRemove(target) {
  assertSafeMutation(target);
  rmSync(target, { recursive: true, force: true });
}

function isActiveLease(marker, now) {
  if (Number(marker.ownerPid) > 0 && processAlive(marker.ownerPid)) return true;
  return Number(marker.leaseUntil) > Number(now);
}

function acquireRetentionLock(root, now) {
  ensureDirectory(root);
  const directory = path.join(root, ".retention-lock");
  assertSafeMutation(directory);
  try {
    mkdirSync(directory);
  } catch (error) {
    if (error.code !== "EEXIST") throw error;
    const ownerFile = path.join(directory, "owner.json");
    let owner;
    try { owner = JSON.parse(readFileSync(ownerFile, "utf8")); } catch { return null; }
    if (!Number.isSafeInteger(owner?.pid) || owner.pid < 1) return null;
    if (processAlive(owner.pid)) return null;
    safeRemove(directory);
    mkdirSync(directory);
  }
  const ownerFile = path.join(directory, "owner.json");
  writeJson(ownerFile, { pid: process.pid, createdAt: now });
  return { release: () => safeRemove(directory) };
}

function processAlive(pid) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return false;
  try { process.kill(Number(pid), 0); return true; } catch (error) { return error.code !== "ESRCH"; }
}
