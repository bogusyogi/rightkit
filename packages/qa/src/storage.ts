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
  const home = fs.mkdtempSync(path.join(parent, `${qaHomeLabel(run.app)}-`));
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
// a product whose own homes hold too much, or a root that holds far too much, refuses to start
// another run. Mirrors the Rust crate's `storage`.

/** Marker inside each home: the owning process and when the run started. */
export const HOME_MARKER = '.rightkit-home.json';
/** Binding file inside each home: the run record it belongs to and its product (`app`). */
const BINDING_FILE = '.rightkit-app.json';
/**
 * Bytes one product's homes may hold after the start-of-run sweep. `0` disables the cap. The cap is per
 * product: a home belongs to the product named by its binding file (else by its directory name prefix),
 * and other products' homes never count against it.
 */
export const ROOT_CAP_ENV = 'RIGHTKIT_QA_ROOT_CAP_BYTES';
/** Bytes the whole `rightkit-qa` root may hold, all products together. Above it every run is refused (the disk is at risk). `0` disables it. */
export const ROOT_CEILING_ENV = 'RIGHTKIT_QA_ROOT_CEILING_BYTES';
/** Seconds a home whose owner is dead must be untouched before the start-of-run sweep removes it. */
export const HOME_MAX_AGE_ENV = 'RIGHTKIT_QA_HOME_MAX_AGE_SECS';
/** `1` keeps the home of a failed run for inspection (its path and size are printed). */
export const KEEP_FAILED_HOME_ENV = 'RIGHTKIT_QA_KEEP_FAILED_HOME';
/** `0` skips the start-of-run sweep. */
export const HOME_SWEEP_ENV = 'RIGHTKIT_QA_HOME_SWEEP';
export const DEFAULT_ROOT_CAP_BYTES = 4_000_000_000;
export const DEFAULT_ROOT_CEILING_BYTES = 32_000_000_000;
export const DEFAULT_HOME_MAX_AGE_MS = 2 * 60 * 60_000;
/** The app key is cut to this many characters to make the product label in a home's name. */
const HOME_LABEL_LENGTH = 16;

export function qaRoot(): string { return path.join(appTempDir(), 'rightkit-qa'); }

function bytesFromEnv(env: NodeJS.ProcessEnv, key: string, fallback: number): number {
  const raw = env[key]?.trim();
  const n = Number(raw);
  return raw && Number.isFinite(n) && n >= 0 ? Math.floor(n) : fallback;
}

/** The per-product cap from the environment, else {@link DEFAULT_ROOT_CAP_BYTES}. */
export function rootCapBytes(env: NodeJS.ProcessEnv = process.env): number {
  return bytesFromEnv(env, ROOT_CAP_ENV, DEFAULT_ROOT_CAP_BYTES);
}

/** The root-wide ceiling from the environment, else {@link DEFAULT_ROOT_CEILING_BYTES}. */
export function rootCeilingBytes(env: NodeJS.ProcessEnv = process.env): number {
  return bytesFromEnv(env, ROOT_CEILING_ENV, DEFAULT_ROOT_CEILING_BYTES);
}

/**
 * Limits a run start enforces on the QA root, as an app sets them in code. A value set here wins over
 * the environment variable; `undefined` leaves the limit to the environment, then the default; `0`
 * disables it.
 */
export interface QaRootLimits {
  /** Per-product cap ({@link ROOT_CAP_ENV}). */
  capBytes?: number;
  /** Root-wide ceiling ({@link ROOT_CEILING_ENV}). */
  ceilingBytes?: number;
}

/** {@link QaRootLimits} after setter, environment and default are applied. `0` means disabled. */
export interface ResolvedQaRootLimits {
  capBytes: number;
  ceilingBytes: number;
}

function limitFromCode(value: number, name: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) throw new TypeError(`${name} must be a non-negative number of bytes (0 disables), got ${String(value)}`);
  return Math.floor(value);
}

/** Setter, else environment, else default. A setter value that is not a non-negative number throws. */
export function resolveQaRootLimits(limits: QaRootLimits = {}, env: NodeJS.ProcessEnv = process.env): ResolvedQaRootLimits {
  return {
    capBytes: limits.capBytes === undefined ? rootCapBytes(env) : limitFromCode(limits.capBytes, 'capBytes'),
    ceilingBytes: limits.ceilingBytes === undefined ? rootCeilingBytes(env) : limitFromCode(limits.ceilingBytes, 'ceilingBytes'),
  };
}

export function homeMaxAgeMs(env: NodeJS.ProcessEnv = process.env): number {
  const n = Number(env[HOME_MAX_AGE_ENV]?.trim());
  return env[HOME_MAX_AGE_ENV]?.trim() && Number.isFinite(n) && n >= 0 ? Math.floor(n * 1000) : DEFAULT_HOME_MAX_AGE_MS;
}

export function keepFailedHomeRequested(env: NodeJS.ProcessEnv = process.env): boolean {
  return ['1', 'true', 'yes'].includes(env[KEEP_FAILED_HOME_ENV]?.trim() ?? '');
}

/** The product label a home's directory name carries: the app key cut to 16 characters. */
export function qaHomeLabel(app: string): string { return app.slice(0, HOME_LABEL_LENGTH); }

/**
 * The product label of a directory name that follows the naming pattern of a QA home, else `undefined`.
 * The pattern is `<label>-<suffix>`: the label is an app key cut to 16 characters (a safe key: ASCII
 * letters, digits, `.`, `_`, `-`, starting with a letter or digit), and the suffix is the 6 letters and
 * digits `mkdtemp` appends here or the 16 lowercase hex digits the Rust crate appends. Both runners write
 * into the same root, so both suffixes are accepted.
 */
export function parseQaHomeName(name: string): string | undefined {
  const cut = name.lastIndexOf('-');
  if (cut <= 0) return undefined;
  const label = name.slice(0, cut);
  const suffix = name.slice(cut + 1);
  const suffixOk = /^[0-9a-f]{16}$/.test(suffix) || /^[A-Za-z0-9]{6}$/.test(suffix);
  const labelOk = label.length <= HOME_LABEL_LENGTH && /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(label);
  return suffixOk && labelOk ? label : undefined;
}

/** The binding file of a home, when it is a readable JSON object. */
function readBinding(home: string): Record<string, unknown> | undefined {
  try {
    const raw: unknown = JSON.parse(fs.readFileSync(path.join(home, BINDING_FILE), 'utf8'));
    return raw && typeof raw === 'object' && !Array.isArray(raw) ? raw as Record<string, unknown> : undefined;
  } catch { return undefined; }
}

/** The product a home belongs to: the `app` in its binding file, else the label in its directory name, else the whole name. */
export function qaHomeProduct(name: string, binding?: Record<string, unknown>): string {
  const app = binding?.app;
  if (typeof app === 'string' && app !== '') return app;
  return parseQaHomeName(name) ?? name;
}

/** Two product names are the same product when their home-name labels are equal. */
function sameProduct(homeProduct: string, app: string): boolean { return qaHomeLabel(homeProduct) === qaHomeLabel(app); }

/** A directory this runner created as a QA home: a real directory directly under `rightkit-qa` with the binding file. */
export function isOwnedHome(home: string): boolean {
  try {
    const resolved = path.resolve(home);
    return fs.lstatSync(resolved).isDirectory() && !fs.lstatSync(resolved).isSymbolicLink()
      && path.basename(path.dirname(resolved)) === 'rightkit-qa'
      && fs.statSync(path.join(resolved, BINDING_FILE)).isFile();
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

/**
 * True when `dir` itself or anything under it was modified after `cutoffMs` (milliseconds since the epoch).
 * Symlinks are examined, never followed. Stops at the first newer entry. A tree that cannot be inspected
 * counts as recently modified: it is not proven idle.
 */
function modifiedAfter(dir: string, cutoffMs: number): boolean {
  const mtime = (p: string): number | undefined => { try { return fs.lstatSync(p).mtimeMs; } catch { return undefined; } };
  const top = mtime(dir);
  if (top === undefined || top > cutoffMs) return true;
  const stack = [dir];
  while (stack.length) {
    const current = stack.pop() as string;
    let entries: fs.Dirent[];
    try { entries = fs.readdirSync(current, { withFileTypes: true }); } catch { return true; }
    for (const entry of entries) {
      const p = path.join(current, entry.name);
      const t = mtime(p);
      if (t === undefined || t > cutoffMs) return true;
      if (entry.isDirectory()) stack.push(p);
    }
  }
  return false;
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
 * Why a home that has no owner marker (`.rightkit-home.json`, or no binding file beside it) must stay, or
 * `undefined` when it may be swept.
 *
 * "Dead" for such a home means every one of these holds:
 * - its name matches the home naming pattern ({@link parseQaHomeName});
 * - no pid recorded for it is alive: the pid in a home marker that has no binding beside it, and the
 *   `ownerPid` of the run record its binding names (`runRoot`), when either is readable;
 * - nothing in the home, the directory itself included, was modified within `maxAgeMs`.
 *
 * Not checked: which processes have the home as their working directory or an open file in it (no process
 * table walk), the run record's lease (`leaseUntil` is a timer set when the run began, not proof of a live
 * process, and a cancelled run's lease is what kept these homes for 24 hours), and whether a pid in the
 * record now belongs to an unrelated process (that keeps the home). A process that holds the home but writes
 * nothing for the whole window is not noticed.
 */
function unmarkedHomeBlocker(dir: string, name: string, binding: Record<string, unknown> | undefined, markerPid: number | undefined, now: number, maxAgeMs: number, alive: (pid: number) => boolean): string | undefined {
  if (parseQaHomeName(name) === undefined) return 'no owner marker and the name is not a QA home name: not proven ours';
  const recorded = binding?.runRoot;
  const runRoot = typeof recorded === 'string' ? recorded : undefined;
  const record = runRoot ? readMarker(path.join(runRoot, RUN_MARKER)) : undefined;
  for (const pid of [markerPid, record?.ownerPid]) {
    if (pid !== undefined && pid > 0 && alive(pid)) return `no owner marker; recorded owner process ${pid} is alive`;
  }
  if (modifiedAfter(dir, now - maxAgeMs)) return 'no owner marker; modified within the window';
  return undefined;
}

/**
 * Remove the homes of dead runs. Two kinds of home exist:
 * - A home with an owner marker (`.rightkit-home.json`) and a binding file goes when its recorded process is
 *   gone and its run started more than `maxAgeMs` ago. A home whose pid is alive is never touched.
 * - Any other home (written before the marker existed, or missing a file) goes only when it is named like a QA
 *   home, no pid its records name is alive, and nothing in it was modified within `maxAgeMs`
 *   (see `unmarkedHomeBlocker` for what is and is not checked).
 *
 * Symlinks, dot-directories, anything outside `parent` and any path that is not a plain directory directly
 * inside it are never removed. Deletion never follows a symlink out of the root.
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
    if (entry.name.startsWith('.')) continue;
    const dir = path.join(canonicalParent, entry.name);
    if (entry.isSymbolicLink()) { report.retained.push({ path: dir, reason: 'symlink skipped' }); continue; }
    if (!entry.isDirectory()) continue;
    const binding = readBinding(dir);
    const marker = readHomeMarker(dir);
    const bytes = treeBytes(dir);
    report.totalBytes += bytes;
    let keep: string | undefined;
    let reason: string;
    if (binding && marker) {
      reason = 'owner process gone and older than the window';
      if (alive(marker.pid)) keep = 'owner process alive';
      else if (now - marker.startedAtMs < maxAge) keep = 'owner gone but younger than the window';
    } else {
      reason = 'no owner marker, no live recorded owner, untouched for longer than the window';
      keep = unmarkedHomeBlocker(dir, entry.name, binding, marker?.pid, now, maxAge, alive);
    }
    if (keep !== undefined) { report.retained.push({ path: dir, reason: keep }); continue; }
    if (!options.dryRun) {
      try { removeHomeDirectory(dir, canonicalParent); } catch (error) { report.retained.push({ path: dir, reason: describeError(error) }); continue; }
    }
    report.removedBytes += bytes;
    report.removed.push({ path: dir, bytes, reason });
  }
  return report;
}

/** Remove `dir` only when it is a plain directory directly inside `canonicalParent`. */
function removeHomeDirectory(dir: string, canonicalParent: string): void {
  if (!safeDirectory(dir) || fs.realpathSync(dir) !== dir || path.dirname(dir) !== canonicalParent) throw new Error('path is not a plain home directory');
  fs.rmSync(dir, { recursive: true, force: false });
}

/** Which limit a refused run exceeded. */
export type QaRootLimitKind = 'product-cap' | 'root-ceiling';

export interface QaRootOverCap {
  kind: QaRootLimitKind;
  root: string;
  /** The product the refused run is for; `undefined` when the caller named none (only the ceiling applies then). */
  app: string | undefined;
  /** Bytes counted against the limit: the product's homes, or every home. */
  totalBytes: number;
  /** Homes counted against the limit. */
  homeCount: number;
  /** The limit that was exceeded. */
  limitBytes: number;
  /** Largest homes first. */
  largest: Array<{ path: string; bytes: number; product: string }>;
}

/** `"5.26 GB (5260000000 bytes)"`, or `"812 bytes"` below a megabyte. */
function humanBytes(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(2)} GB (${bytes} bytes)`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB (${bytes} bytes)`;
  return `${bytes} bytes`;
}

function describeRootOverCap(detail: QaRootOverCap): string {
  const who = detail.app ? ` for \`${detail.app}\`` : '';
  const largest = detail.largest.map((h) => ` ${h.path} [${h.product}] (${humanBytes(h.bytes)});`).join('');
  const head = detail.kind === 'product-cap'
    ? `QA run${who} refused: the ${detail.app ?? 'product'} homes in ${detail.root} hold ${humanBytes(detail.totalBytes)} in ${detail.homeCount} home(s), over the ${humanBytes(detail.limitBytes)} per-product cap (${ROOT_CAP_ENV}; \`rootCapBytes\` sets it in code, 0 disables). Other products' homes are not counted. Largest ${detail.app ?? 'product'} homes:`
    : `QA run${who} refused: ${detail.root} holds ${humanBytes(detail.totalBytes)} across all products in ${detail.homeCount} home(s), over the ${humanBytes(detail.limitBytes)} root ceiling (${ROOT_CEILING_ENV}; \`rootCeilingBytes\` sets it in code, 0 disables), so the disk may be at risk. Largest homes:`;
  return `${head}${largest} Run \`cleanQaHomes()\` (or \`rightkit-qa clean\`) to remove the homes of dead runs (owner process gone and untouched for the home age bound, 2 hours by default, ${HOME_MAX_AGE_ENV}); a home it keeps belongs to a live or recent run, so remove it by hand once you know that run is over, then retry`;
}

/** Thrown before a run starts when the run's product is over its cap, or the root over its ceiling, after the sweep. */
export class QaRootOverCapError extends Error {
  readonly detail: QaRootOverCap;
  constructor(detail: QaRootOverCap) {
    super(describeRootOverCap(detail));
    this.name = 'QaRootOverCapError';
    this.detail = detail;
  }
}

/**
 * Throw {@link QaRootOverCapError} when the homes of the product `app` hold more than `limits.capBytes`
 * (checked first), or when every directory under `parent` together holds more than `limits.ceilingBytes`.
 * `0` disables a limit; without `app` only the ceiling applies. A home's product is the `app` in its binding
 * file, else the label in its directory name. Symlinks are not followed or counted.
 */
export function checkQaRootLimits(parent: string, app: string | undefined, limits: ResolvedQaRootLimits): void {
  const cap = app !== undefined && limits.capBytes > 0 ? limits.capBytes : 0;
  const ceiling = limits.ceilingBytes > 0 ? limits.ceilingBytes : 0;
  if ((cap === 0 && ceiling === 0) || !safeDirectory(parent)) return;
  let entries: fs.Dirent[];
  try { entries = fs.readdirSync(parent, { withFileTypes: true }); } catch { return; }
  const homes: Array<{ path: string; bytes: number; product: string; own: boolean }> = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
    const dir = path.join(parent, entry.name);
    const product = qaHomeProduct(entry.name, readBinding(dir));
    const own = app !== undefined && sameProduct(product, app);
    // Another product's home is sized only for the ceiling.
    homes.push({ path: dir, product, own, bytes: (own && cap > 0) || ceiling > 0 ? treeBytes(dir) : 0 });
  }
  const refuse = (kind: QaRootLimitKind, limitBytes: number, counted: typeof homes): never => {
    const sorted = [...counted].sort((a, b) => b.bytes - a.bytes);
    throw new QaRootOverCapError({
      kind,
      root: parent,
      app,
      totalBytes: sorted.reduce((sum, h) => sum + h.bytes, 0),
      homeCount: sorted.length,
      limitBytes,
      largest: sorted.slice(0, 5).map((h) => ({ path: h.path, bytes: h.bytes, product: h.product })),
    });
  };
  if (cap > 0) {
    const own = homes.filter((h) => h.own);
    if (own.reduce((sum, h) => sum + h.bytes, 0) > cap) refuse('product-cap', cap, own);
  }
  if (ceiling > 0 && homes.reduce((sum, h) => sum + h.bytes, 0) > ceiling) refuse('root-ceiling', ceiling, homes);
}

/** {@link checkQaRootLimits} with only the per-product cap. */
export function checkQaRootCap(parent: string, app: string, capBytes: number): void {
  checkQaRootLimits(parent, app, { capBytes, ceilingBytes: 0 });
}

export interface PreflightOptions {
  /** Force the sweep and limits on or off. Default: on, except under vitest (it runs against the developer's real root). */
  enabled?: boolean;
  /** The product the run is for. Without it only the root ceiling applies. */
  app?: string;
  /** Per-product cap in bytes, set in code: wins over {@link ROOT_CAP_ENV}. `0` disables. */
  capBytes?: number;
  /** Root-wide ceiling in bytes, set in code: wins over {@link ROOT_CEILING_ENV}. `0` disables. */
  ceilingBytes?: number;
  maxAgeMs?: number;
  /** The `.../rightkit-qa` directory to sweep and check. Default: this runner's own root. A test hook. */
  parent?: string;
  /** Environment the variables are read from. Default: `process.env`. A test hook. */
  env?: NodeJS.ProcessEnv;
}

/** Run before a run is created: sweep dead homes, then refuse to start when the run's product is over its cap or the root over its ceiling. */
export function preflightQaRoot(options: PreflightOptions = {}): void {
  const enabled = options.enabled ?? !process.env.VITEST;
  if (!enabled) return;
  const env = options.env ?? process.env;
  const parent = options.parent ?? qaRoot();
  if (!safeDirectory(parent)) return;
  if ((env[HOME_SWEEP_ENV] ?? '').trim() !== '0') cleanQaHomes({ parent, maxAgeMs: options.maxAgeMs ?? homeMaxAgeMs(env) });
  checkQaRootLimits(parent, options.app, resolveQaRootLimits({ capBytes: options.capBytes, ceilingBytes: options.ceilingBytes }, env));
}

/** `"<path> (<bytes> bytes)"` for a kept home. */
export function describeKeptHome(home: string): string { return `${home} (${treeBytes(home)} bytes)`; }
