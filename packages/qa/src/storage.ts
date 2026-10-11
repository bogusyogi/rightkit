import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const RUN_MARKER = '.rightkit-run.json';
export const DEFAULT_RETENTION_BYTES = 20_000_000_000;
export const DEFAULT_YOUNG_MS = 24 * 60 * 60_000;
export const DEFAULT_KEEP_COUNT = 5;
const LOCK_WAIT_MS = 5_000;

export interface RunMarker {
  schemaVersion: 1;
  app: string;
  runId: string;
  createdAt: number;
  ownerPid: number;
  leaseUntil: number;
  completedAt?: number;
  /** Owned internal-temp companion; validated against its binding before cleanup. */
  appRoot?: string;
}

export interface ManagedRun {
  root: string;
  app: string;
  runId: string;
  marker: RunMarker;
}

export interface RetentionPolicy {
  maxBytes?: number;
  keepCount?: number;
  youngMs?: number;
  now?: number;
  keepRunIds?: string[];
}

export interface RetentionReport {
  removed: string[];
  retained: Array<{ path: string; reason: string }>;
  totalBytes: number;
  capBytes: number;
  overCapBytes: number;
  blockedBytes: number;
  unknownBytes: number;
}

export function resolveManagedRoot(explicit?: string): string {
  if (explicit?.trim()) return path.resolve(explicit);
  if (process.env.RIGHTKIT_MANAGED_ROOT?.trim()) return path.resolve(process.env.RIGHTKIT_MANAGED_ROOT);
  const [workstation, volume] = process.platform === 'win32' ? ['D:\\.rightkit-managed', 'D:\\'] : ['/Volumes/D/.rightkit-managed', '/Volumes/D'];
  if (fs.existsSync(volume)) return path.resolve(workstation);
  // Hosted CI and other machines: runner temp, then the system temp dir.
  const runnerTemp = process.env.RUNNER_TEMP?.trim();
  return path.join(runnerTemp && fs.existsSync(runnerTemp) ? runnerTemp : os.tmpdir(), 'rightkit-managed');
}

export function safeKey(value: string, name: string): string {
  const v = String(value ?? '').trim();
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(v)) throw new Error(`unsafe QA ${name}: ${value}`);
  return v;
}

export function runRoot(managedRoot: string, app: string, runId: string): string {
  const root = path.resolve(managedRoot);
  const appKey = safeKey(app, 'app');
  const id = safeKey(runId, 'run id');
  const result = path.join(root, 'runs', appKey, id);
  if (!isInside(path.join(root, 'runs', appKey), result)) throw new Error('QA run path escapes managed root');
  return result;
}

export function createManagedRun(options: {
  managedRoot?: string;
  app: string;
  runId?: string;
  leaseMs?: number;
  retention?: RetentionPolicy;
}): ManagedRun {
  const managedRoot = resolveManagedRoot(options.managedRoot);
  const app = safeKey(options.app, 'app');
  const runId = safeKey(options.runId ?? cryptoRandomId(), 'run id');
  const appRoot = path.join(managedRoot, 'runs', app);
  withRetentionLock(appRoot, () => {
    ensureSafeDirectory(managedRoot);
    ensureSafeDirectory(path.join(managedRoot, 'runs'));
    ensureSafeDirectory(appRoot);
    const retention = { now: Date.now(), ...(options.retention ?? {}) };
    const report: RetentionReport = { removed: [], retained: [], totalBytes: 0, capBytes: DEFAULT_RETENTION_BYTES, overCapBytes: 0, blockedBytes: 0, unknownBytes: 0 };
    pruneUnlocked(appRoot, app, retention, report);
    if (report.overCapBytes > 0) throw new Error(`QA retention cap is blocked by ${report.overCapBytes} bytes of protected or unknown data`);
    const root = runRoot(managedRoot, app, runId);
    if (fs.existsSync(root)) throw new Error(`QA run already exists: ${root}`);
    fs.mkdirSync(root, { recursive: true });
    const marker: RunMarker = {
      schemaVersion: 1,
      app,
      runId,
      createdAt: Date.now(),
      ownerPid: process.pid,
      leaseUntil: Date.now() + Math.max(1, options.leaseMs ?? DEFAULT_YOUNG_MS),
    };
    writeMarker(root, marker);
  });
  const root = runRoot(managedRoot, app, runId);
  const marker = readMarker(path.join(root, RUN_MARKER));
  if (!marker) throw new Error(`QA run marker was not created: ${root}`);
  return { root, app, runId, marker };
}

export function finishManagedRun(run: ManagedRun | string): RunMarker | undefined {
  const root = typeof run === 'string' ? run : run.root;
  const markerPath = path.join(path.resolve(root), RUN_MARKER);
  const marker = readMarker(markerPath);
  if (!marker) return undefined;
  const home = ownedAppRoot(root, marker);
  if (home) {
    copyAppTree(path.join(home, 'evidence'), path.join(root, 'evidence', 'app'));
    copyAppTree(path.join(home, 'captures'), path.join(root, 'captures'));
    for (const name of fs.readdirSync(home)) if (name.endsWith('.log')) copyAppTree(path.join(home, name), path.join(root, 'evidence', name));
  }
  const finished: RunMarker = { ...marker, ownerPid: 0, leaseUntil: 0, completedAt: Date.now() };
  writeMarker(root, finished);
  return finished;
}

export function pruneManagedRuns(
  managedRoot: string,
  app: string,
  policy: RetentionPolicy = {},
): RetentionReport {
  const root = resolveManagedRoot(managedRoot);
  const appKey = safeKey(app, 'app');
  const appRoot = path.join(root, 'runs', appKey);
  const cap = Number.isFinite(policy.maxBytes) && (policy.maxBytes as number) >= 0 ? policy.maxBytes as number : DEFAULT_RETENTION_BYTES;
  const report: RetentionReport = { removed: [], retained: [], totalBytes: 0, capBytes: cap, overCapBytes: 0, blockedBytes: 0, unknownBytes: 0 };
  if (!safeDirectory(appRoot)) return report;
  return withRetentionLock(appRoot, () => pruneUnlocked(appRoot, appKey, policy, report));
}

function pruneUnlocked(appRoot: string, app: string, policy: RetentionPolicy, report: RetentionReport): RetentionReport {
  const now = policy.now ?? Date.now();
  const cap = Number.isFinite(policy.maxBytes) && (policy.maxBytes as number) >= 0 ? policy.maxBytes as number : DEFAULT_RETENTION_BYTES;
  report.capBytes = cap;
  const keepCount = Math.max(0, Math.floor(policy.keepCount ?? DEFAULT_KEEP_COUNT));
  const youngMs = Math.max(0, policy.youngMs ?? DEFAULT_YOUNG_MS);
  const candidates: Array<{ root: string; marker: RunMarker; bytes: number; active: boolean }> = [];
  let total = 0;
  let entries: fs.Dirent[];
  try { entries = fs.readdirSync(appRoot, { withFileTypes: true }); } catch { return report; }
  for (const entry of entries) {
    if (entry.name.startsWith('.')) continue;
    if (entry.isSymbolicLink()) { report.retained.push({ path: path.join(appRoot, entry.name), reason: 'symlink skipped' }); continue; }
    if (!entry.isDirectory()) continue;
    const runPath = path.join(appRoot, entry.name);
    if (!isInside(appRoot, runPath)) continue;
    const marker = readMarker(path.join(runPath, RUN_MARKER));
    if (!marker || marker.app !== app || marker.runId !== entry.name) {
      const bytes = directoryBytes(runPath);
      if (bytes !== undefined) report.unknownBytes += bytes;
      report.retained.push({ path: runPath, reason: 'unknown or malformed run metadata' });
      continue;
    }
    const bytes = directoryBytes(runPath);
    if (bytes === undefined) { report.retained.push({ path: runPath, reason: 'symlink or unreadable content skipped' }); continue; }
    const active = marker.ownerPid > 0 && isProcessLive(marker.ownerPid) || marker.leaseUntil > now;
    total += bytes;
    candidates.push({ root: runPath, marker, bytes, active });
  }
  candidates.sort((a, b) => b.marker.createdAt - a.marker.createdAt || a.marker.runId.localeCompare(b.marker.runId));
  const countProtected = new Set(candidates.slice(0, keepCount).map((x) => x.marker.runId));
  const protectedIds = new Set<string>();
  const callerProtected = new Set(policy.keepRunIds ?? []);
  for (const item of candidates) {
    if (now - item.marker.createdAt < youngMs) protectedIds.add(item.marker.runId);
    if (item.active) protectedIds.add(item.marker.runId);
    if (callerProtected.has(item.marker.runId)) protectedIds.add(item.marker.runId);
  }
  const old = candidates
    .filter((x) => !protectedIds.has(x.marker.runId) && (total > cap || !countProtected.has(x.marker.runId)))
    .sort((a, b) => a.marker.createdAt - b.marker.createdAt || a.marker.runId.localeCompare(b.marker.runId));
  const removed = new Set<string>();
  for (const item of old) {
    if (total <= cap && countProtected.has(item.marker.runId)) continue;
    if (!safeDirectory(item.root)) continue;
    try {
      removeManagedAppRoot(item.root, item.marker);
      fs.rmSync(item.root, { recursive: true, force: false });
      report.removed.push(item.root);
      removed.add(item.marker.runId);
      total -= item.bytes;
    } catch (error) {
      report.retained.push({ path: item.root, reason: describeError(error) });
    }
  }
  for (const item of candidates) {
    if (!removed.has(item.marker.runId) && (protectedIds.has(item.marker.runId) || (total <= cap && countProtected.has(item.marker.runId)))) report.retained.push({ path: item.root, reason: callerProtected.has(item.marker.runId) ? 'caller protected run' : item.active ? 'active run; may belong to a live run' : (now - item.marker.createdAt < youngMs ? 'younger than retention window' : 'newest protected run') });
  }
  report.blockedBytes = candidates.filter((item) => !removed.has(item.marker.runId) && (protectedIds.has(item.marker.runId) || (total <= cap && countProtected.has(item.marker.runId)))).reduce((sum, item) => sum + item.bytes, 0);
  report.totalBytes = total + report.unknownBytes;
  report.overCapBytes = Math.max(0, report.totalBytes - report.capBytes);
  return report;
}

function withRetentionLock<T>(appRoot: string, body: () => T): T {
  ensureSafeDirectory(path.dirname(appRoot));
  ensureSafeDirectory(appRoot);
  const lock = path.join(appRoot, '.retention-lock');
  const owner = path.join(lock, 'owner.json');
  const deadline = Date.now() + LOCK_WAIT_MS;
  while (true) {
    try {
      fs.mkdirSync(lock);
      fs.writeFileSync(owner, JSON.stringify({ pid: process.pid, createdAt: Date.now() }), 'utf8');
      break;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
      let pid: number | undefined;
      try { pid = JSON.parse(fs.readFileSync(owner, 'utf8')).pid; } catch { /* uncertain owner: wait */ }
      if (typeof pid === 'number' && pid > 0 && !isProcessLive(pid)) {
        try { fs.rmSync(lock, { recursive: true, force: true }); } catch { /* retry */ }
      } else if (Date.now() >= deadline) {
        throw new Error(`QA retention lock is held: ${lock}`);
      }
      if (Date.now() < deadline) sleepSync(20);
    }
  }
  try { return body(); } finally { try { fs.rmSync(lock, { recursive: true, force: true }); } catch { /* best effort */ } }
}

function writeMarker(root: string, marker: RunMarker): void {
  const target = path.join(root, RUN_MARKER);
  const temp = `${target}.${process.pid}.${Date.now()}.tmp`;
  fs.writeFileSync(temp, `${JSON.stringify(marker)}\n`, { encoding: 'utf8', flag: 'wx' });
  fs.renameSync(temp, target);
}

function readMarker(file: string): RunMarker | undefined {
  try {
    const raw: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
    if (!raw || typeof raw !== 'object') return undefined;
    const x = raw as Record<string, unknown>;
    if (x.schemaVersion !== 1 || typeof x.app !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(x.app) || typeof x.runId !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(x.runId) || typeof x.createdAt !== 'number' || !Number.isFinite(x.createdAt) || x.createdAt <= 0 || typeof x.ownerPid !== 'number' || !Number.isInteger(x.ownerPid) || x.ownerPid < 0 || typeof x.leaseUntil !== 'number' || !Number.isFinite(x.leaseUntil) || x.leaseUntil < 0) return undefined;
    if (x.completedAt !== undefined && (typeof x.completedAt !== 'number' || !Number.isFinite(x.completedAt) || x.completedAt < 0)) return undefined;
    if (x.appRoot !== undefined && typeof x.appRoot !== 'string') return undefined;
    return x as unknown as RunMarker;
  } catch { return undefined; }
}

function directoryBytes(root: string): number | undefined {
  let total = 0;
  const walk = (dir: string): boolean => {
    let entries: fs.Dirent[];
    try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return false; }
    for (const entry of entries) {
      const p = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) return false;
      if (entry.isDirectory()) { if (!walk(p)) return false; }
      else if (entry.isFile()) { try { total += fs.statSync(p).size; } catch { return false; } }
      else return false;
    }
    return true;
  };
  return walk(root) ? total : undefined;
}

function safeDirectory(dir: string): boolean {
  try { return fs.lstatSync(dir).isDirectory() && !fs.lstatSync(dir).isSymbolicLink(); } catch { return false; }
}

function ensureSafeDirectory(dir: string): void {
  const absolute = path.resolve(dir);
  const parts = absolute.split(path.sep);
  let current = parts[0] || path.sep;
  for (const part of parts.slice(1)) {
    if (!part) continue;
    current = path.join(current, part);
    try { if (fs.lstatSync(current).isSymbolicLink()) throw new Error(`refusing symlink in managed path: ${current}`); } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
  }
  fs.mkdirSync(absolute, { recursive: true });
}

function isInside(root: string, candidate: string): boolean {
  const relative = path.relative(path.resolve(root), path.resolve(candidate));
  return relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative);
}

function isProcessLive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch (error) { return (error as NodeJS.ErrnoException).code === 'EPERM'; }
}

function describeError(error: unknown): string {
  const x = error as NodeJS.ErrnoException;
  return x?.code ? `${x.code}: ${x.message}` : String((error as Error)?.message ?? error);
}

function sleepSync(ms: number): void { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms); }
function cryptoRandomId(): string { return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`; }

/** Short internal paths keep launchd files accessible & service sockets within sockaddr_un. */
export function appTempDir(): string {
  return process.platform === 'darwin' ? '/private/tmp' : fs.realpathSync(os.tmpdir());
}

export function createManagedAppRoot(run: ManagedRun): string {
  const parent = path.join(appTempDir(), 'rightkit-qa');
  ensureSafeDirectory(parent);
  const home = fs.mkdtempSync(path.join(parent, `${run.app.slice(0, 16)}-`));
  fs.writeFileSync(path.join(home, '.rightkit-app.json'), JSON.stringify({ runRoot: fs.realpathSync(run.root), app: run.app, runId: run.runId }), { flag: 'wx' });
  fs.writeFileSync(path.join(home, HOME_MARKER), JSON.stringify({ schemaVersion: 1, pid: process.pid, startedAtMs: Date.now(), app: run.app, runId: run.runId }), { flag: 'wx' });
  run.marker.appRoot = home;
  writeMarker(run.root, run.marker);
  return home;
}

function ownedAppRoot(root: string, marker: RunMarker): string | undefined {
  const home = marker.appRoot;
  if (!home || !fs.existsSync(home)) return undefined;
  const parent = path.dirname(home);
  const temp = path.dirname(parent);
  const allowedTemp = temp === appTempDir() || process.platform === 'darwin' && (temp === '/private/tmp' || isInside('/private/var/folders', temp) && path.basename(temp) === 'T');
  if (path.basename(parent) !== 'rightkit-qa' || !allowedTemp || fs.realpathSync(home) !== home || !safeDirectory(home)) throw new Error('invalid QA app temp root');
  const binding = JSON.parse(fs.readFileSync(path.join(home, '.rightkit-app.json'), 'utf8'));
  if (binding.runRoot !== fs.realpathSync(root) || binding.app !== marker.app || binding.runId !== marker.runId) throw new Error('QA app temp ownership mismatch');
  return home;
}

export function removeManagedAppRoot(root: string, marker: RunMarker): void {
  const home = ownedAppRoot(root, marker);
  if (home) fs.rmSync(home, { recursive: true, force: false });
}

function copyAppTree(source: string, target: string): void {
  const meta = fs.lstatSync(source);
  if (meta.isSymbolicLink()) throw new Error('symlink in QA app evidence');
  if (fs.existsSync(target) && fs.lstatSync(target).isSymbolicLink()) throw new Error('symlink in managed QA evidence');
  if (meta.isDirectory()) {
    ensureSafeDirectory(target);
    for (const name of fs.readdirSync(source)) copyAppTree(path.join(source, name), path.join(target, name));
  } else if (meta.isFile()) {
    ensureSafeDirectory(path.dirname(target));
    fs.copyFileSync(source, target);
  } else throw new Error('unsupported QA app evidence file');
}

// ---------------------------------------------------------------------------------------------
// QA root hygiene: every home is removed when its run ends; dead homes are swept at run start;
// a root over its size cap refuses to start another run. Mirrors the Rust crate's `storage`.

/** Marker inside each home: the owning process and when the run started. */
export const HOME_MARKER = '.rightkit-home.json';
/** Bytes the whole `rightkit-qa` root may hold after the start-of-run sweep. `0` disables the cap. */
export const ROOT_CAP_ENV = 'RIGHTKIT_QA_ROOT_CAP_BYTES';
/** Seconds a home whose owner is dead must be old before the start-of-run sweep removes it. */
export const HOME_MAX_AGE_ENV = 'RIGHTKIT_QA_HOME_MAX_AGE_SECS';
/** `1` keeps the home of a failed run for inspection (its path and size are printed). */
export const KEEP_FAILED_HOME_ENV = 'RIGHTKIT_QA_KEEP_FAILED_HOME';
/** `0` skips the start-of-run sweep. */
export const HOME_SWEEP_ENV = 'RIGHTKIT_QA_HOME_SWEEP';
export const DEFAULT_ROOT_CAP_BYTES = 4_000_000_000;
export const DEFAULT_HOME_MAX_AGE_MS = 2 * 60 * 60_000;

export function qaRoot(): string { return path.join(appTempDir(), 'rightkit-qa'); }

export function rootCapBytes(env: NodeJS.ProcessEnv = process.env): number {
  const n = Number(env[ROOT_CAP_ENV]?.trim());
  return env[ROOT_CAP_ENV]?.trim() && Number.isFinite(n) && n >= 0 ? Math.floor(n) : DEFAULT_ROOT_CAP_BYTES;
}

export function homeMaxAgeMs(env: NodeJS.ProcessEnv = process.env): number {
  const n = Number(env[HOME_MAX_AGE_ENV]?.trim());
  return env[HOME_MAX_AGE_ENV]?.trim() && Number.isFinite(n) && n >= 0 ? Math.floor(n * 1000) : DEFAULT_HOME_MAX_AGE_MS;
}

export function keepFailedHomeRequested(env: NodeJS.ProcessEnv = process.env): boolean {
  return ['1', 'true', 'yes'].includes(env[KEEP_FAILED_HOME_ENV]?.trim() ?? '');
}

/** A directory this runner created as a QA home: a real directory directly under `rightkit-qa` with the binding file. */
export function isOwnedHome(home: string): boolean {
  try {
    const resolved = path.resolve(home);
    return fs.lstatSync(resolved).isDirectory() && !fs.lstatSync(resolved).isSymbolicLink()
      && path.basename(path.dirname(resolved)) === 'rightkit-qa'
      && fs.statSync(path.join(resolved, '.rightkit-app.json')).isFile();
  } catch { return false; }
}

/** Bytes under `dir`, skipping symlinks and unreadable entries (an estimate, not a proof). */
function treeBytes(dir: string): number {
  let total = 0;
  const stack = [dir];
  while (stack.length) {
    const current = stack.pop() as string;
    let entries: fs.Dirent[];
    try { entries = fs.readdirSync(current, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      const p = path.join(current, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) stack.push(p);
      else if (entry.isFile()) { try { total += fs.statSync(p).size; } catch { /* vanished */ } }
    }
  }
  return total;
}

function readHomeMarker(home: string): { pid: number; startedAtMs: number } | undefined {
  try {
    const x = JSON.parse(fs.readFileSync(path.join(home, HOME_MARKER), 'utf8')) as Record<string, unknown>;
    if (typeof x.pid === 'number' && Number.isInteger(x.pid) && x.pid >= 0 && typeof x.startedAtMs === 'number' && Number.isFinite(x.startedAtMs)) return { pid: x.pid, startedAtMs: x.startedAtMs };
  } catch { /* none */ }
  return undefined;
}

export interface HomeCleanReport {
  removed: Array<{ path: string; bytes: number; reason: string }>;
  retained: Array<{ path: string; reason: string }>;
  totalBytes: number;
  removedBytes: number;
}

export interface CleanQaHomesOptions {
  /** The `.../rightkit-qa` directory. Default: this runner's own root. Must be named `rightkit-qa`. */
  parent?: string;
  maxAgeMs?: number;
  dryRun?: boolean;
  now?: number;
  /** Liveness probe, injectable for tests. Default: signal 0. */
  isAlive?: (pid: number) => boolean;
}

/**
 * Remove the homes of dead runs. A home goes when its recorded process is gone and it is older than
 * `maxAgeMs`; a home whose pid is alive is never touched, nor is a directory without the binding file,
 * a symlink, or anything outside `parent`. Homes from before the pid marker existed fall back to their
 * run record's liveness. Deletion never follows a symlink out of the root.
 */
export function cleanQaHomes(options: CleanQaHomesOptions = {}): HomeCleanReport {
  const report: HomeCleanReport = { removed: [], retained: [], totalBytes: 0, removedBytes: 0 };
  const parent = path.resolve(options.parent ?? qaRoot());
  if (path.basename(parent) !== 'rightkit-qa' || !safeDirectory(parent)) return report;
  let canonicalParent: string;
  let entries: fs.Dirent[];
  try { canonicalParent = fs.realpathSync(parent); entries = fs.readdirSync(canonicalParent, { withFileTypes: true }); } catch { return report; }
  const now = options.now ?? Date.now();
  const maxAge = options.maxAgeMs ?? homeMaxAgeMs();
  const alive = options.isAlive ?? isProcessLive;
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink() || entry.name.startsWith('.')) continue;
    const dir = path.join(canonicalParent, entry.name);
    let binding: Record<string, unknown>;
    try { binding = JSON.parse(fs.readFileSync(path.join(dir, '.rightkit-app.json'), 'utf8')); } catch {
      report.retained.push({ path: dir, reason: 'no readable binding file: not proven ours' });
      continue;
    }
    let live: boolean;
    let started: number;
    const marker = readHomeMarker(dir);
    if (marker) { live = alive(marker.pid); started = marker.startedAtMs; }
    else {
      const run = typeof binding.runRoot === 'string' ? readMarker(path.join(binding.runRoot, RUN_MARKER)) : undefined;
      if (run) { live = run.ownerPid > 0 && alive(run.ownerPid) || run.leaseUntil > now; started = run.createdAt; }
      else { live = false; try { started = fs.statSync(dir).mtimeMs; } catch { started = now; } }
    }
    const bytes = treeBytes(dir);
    report.totalBytes += bytes;
    if (live) { report.retained.push({ path: dir, reason: 'owner process alive' }); continue; }
    if (now - started < maxAge) { report.retained.push({ path: dir, reason: 'owner gone but younger than the window' }); continue; }
    if (!options.dryRun) {
      try { removeHomeDirectory(dir, canonicalParent); } catch (error) { report.retained.push({ path: dir, reason: describeError(error) }); continue; }
    }
    report.removedBytes += bytes;
    report.removed.push({ path: dir, bytes, reason: 'owner process gone and older than the window' });
  }
  return report;
}

/** Remove `dir` only when it is a plain directory directly inside `canonicalParent`. */
function removeHomeDirectory(dir: string, canonicalParent: string): void {
  if (!safeDirectory(dir) || fs.realpathSync(dir) !== dir || path.dirname(dir) !== canonicalParent) throw new Error('path is not a plain home directory');
  fs.rmSync(dir, { recursive: true, force: false });
}

export interface QaRootOverCap { root: string; totalBytes: number; capBytes: number; largest: Array<{ path: string; bytes: number }> }

/** Thrown before a run starts when the QA root holds more than its cap after the sweep. */
export class QaRootOverCapError extends Error {
  readonly detail: QaRootOverCap;
  constructor(detail: QaRootOverCap) {
    super(`QA root ${detail.root} holds ${detail.totalBytes} bytes, over its ${detail.capBytes} byte cap (${ROOT_CAP_ENV}); largest homes: ${detail.largest.map((h) => `${h.path} (${h.bytes} bytes)`).join('; ')}; run \`cleanQaHomes()\`, or remove those homes, then retry`);
    this.name = 'QaRootOverCapError';
    this.detail = detail;
  }
}

/** Throw {@link QaRootOverCapError} when the directories under `parent` hold more than `capBytes` (`0` disables). */
export function checkQaRootCap(parent: string, capBytes: number): void {
  if (!(capBytes > 0) || !safeDirectory(parent)) return;
  let entries: fs.Dirent[];
  try { entries = fs.readdirSync(parent, { withFileTypes: true }); } catch { return; }
  const homes = entries.filter((e) => e.isDirectory() && !e.isSymbolicLink()).map((e) => ({ path: path.join(parent, e.name), bytes: treeBytes(path.join(parent, e.name)) }));
  const total = homes.reduce((sum, h) => sum + h.bytes, 0);
  if (total <= capBytes) return;
  homes.sort((a, b) => b.bytes - a.bytes);
  throw new QaRootOverCapError({ root: parent, totalBytes: total, capBytes, largest: homes.slice(0, 5) });
}

export interface PreflightOptions {
  /** Force the sweep and cap on or off. Default: on, except under vitest (it runs against the developer's real root). */
  enabled?: boolean;
  capBytes?: number;
  maxAgeMs?: number;
}

/** Run before a run is created: sweep dead homes, then refuse to start over the cap. */
export function preflightQaRoot(options: PreflightOptions = {}): void {
  const enabled = options.enabled ?? !process.env.VITEST;
  if (!enabled) return;
  const parent = qaRoot();
  if (!safeDirectory(parent)) return;
  if ((process.env[HOME_SWEEP_ENV] ?? '').trim() !== '0') cleanQaHomes({ parent, maxAgeMs: options.maxAgeMs });
  checkQaRootCap(parent, options.capBytes ?? rootCapBytes());
}

/** `"<path> (<bytes> bytes)"` for a kept home. */
export function describeKeptHome(home: string): string { return `${home} (${treeBytes(home)} bytes)`; }
