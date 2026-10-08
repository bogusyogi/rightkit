import fs from 'node:fs';
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
  if (process.platform === 'win32') return path.resolve('D:\\.rightkit-managed');
  return '/Volumes/D/.rightkit-managed';
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
