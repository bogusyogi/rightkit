import { spawn, spawnSync, type ChildProcess, type SpawnOptions } from 'node:child_process';
import { closeSync, fstatSync, mkdirSync, openSync, readFileSync, readSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { killProcessTree } from './process.js';

/** Format tag of a detached dev server's record file. */
const RECORD_FORMAT = 'rightkit-dev-server/1';
/** Bytes of a log kept for error messages and output(). */
const LOG_TAIL_LIMIT = 16 * 1024;

/**
 * A live process as the OS reports it. `startTime` is the platform's own text (ps lstart, or the CIM CreationDate):
 * compare it only with another reading from the same host.
 */
export interface ProcessIdentity { pid: number; startTime: string; command: string }

/** The JSON record a detached dev server leaves, so a later process can find and stop it. */
export interface DetachedRecord {
  format: 'rightkit-dev-server/1';
  pid: number;
  startTime: string;
  command: string;
  url: string;
  port: number;
  logPath: string;
  recordedAt: string;
}

/**
 * The process operations a detached dev server uses. The defaults use child_process, the OS process table and
 * killProcessTree. Tests may replace them; production code leaves them unset.
 */
export interface DevServerRuntime {
  /** Starts the server process. */
  spawn(command: string, args: string[], options: SpawnOptions): ChildProcess;
  /** The live identity of pid, or undefined when there is no such process (a zombie counts as gone). */
  lookup(pid: number): ProcessIdentity | undefined;
  /** Stops the process tree rooted at pid: the whole process group on POSIX, taskkill /T /F on Windows. */
  stopTree(pid: number): Promise<void>;
}

/** Thrown instead of stopping a pid whose live identity is not the server that the record (or this process) names. */
export class DevServerIdentityError extends Error {
  readonly pid: number;
  constructor(pid: number, reason: string) {
    super(`refusing to stop pid ${pid}: ${reason}`);
    this.name = 'DevServerIdentityError';
    this.pid = pid;
  }
}

export const defaultDevServerRuntime: DevServerRuntime = {
  spawn: (command, args, options) => spawn(command, args, options),
  lookup: (pid) => lookupProcess(pid),
  stopTree: (pid) => killProcessTree(pid),
};

/** The live identity of pid from the OS process table, or undefined when there is no such process (or only a zombie). */
export function lookupProcess(pid: number): ProcessIdentity | undefined {
  if (!Number.isInteger(pid) || pid <= 0) return undefined;
  return process.platform === 'win32' ? lookupWindows(pid) : lookupPosix(pid);
}

/** ps prints lstart (five fields), then stat, then the full command. -ww keeps the command from being cut to the terminal width. */
const PS_LINE = /^(\S+\s+\S+\s+\d+\s+\d\d:\d\d:\d\d\s+\d{4})\s+(\S+)\s+(.+)$/;
function lookupPosix(pid: number): ProcessIdentity | undefined {
  const result = spawnSync('ps', ['-ww', '-o', 'lstart=', '-o', 'stat=', '-o', 'command=', '-p', String(pid)], { encoding: 'utf8', timeout: 5_000, windowsHide: true });
  if (result.error || result.status !== 0) return undefined;
  const match = PS_LINE.exec(result.stdout.trim());
  if (!match || match[2].startsWith('Z')) return undefined;
  return { pid, startTime: match[1].replace(/\s+/g, ' '), command: match[3].trim() };
}
function lookupWindows(pid: number): ProcessIdentity | undefined {
  const script = `$p = Get-CimInstance Win32_Process -Filter 'ProcessId=${pid}' -ErrorAction SilentlyContinue; if ($p) { @{ start = $p.CreationDate.ToString('o'); command = [string]$p.CommandLine } | ConvertTo-Json -Compress }`;
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], { encoding: 'utf8', timeout: 15_000, windowsHide: true });
  if (result.error || result.status !== 0) return undefined;
  const text = result.stdout.trim();
  if (!text) return undefined;
  let parsed: { start?: unknown; command?: unknown };
  try { parsed = JSON.parse(text) as { start?: unknown; command?: unknown }; } catch { return undefined; }
  if (typeof parsed.start !== 'string' || !parsed.start) return undefined;
  return { pid, startTime: parsed.start, command: typeof parsed.command === 'string' ? parsed.command : '' };
}

function isDetachedRecord(value: unknown): value is DetachedRecord {
  if (typeof value !== 'object' || value === null) return false;
  const record = value as Record<string, unknown>;
  return record.format === RECORD_FORMAT
    && Number.isInteger(record.pid) && (record.pid as number) > 0
    && typeof record.startTime === 'string' && record.startTime.length > 0
    && typeof record.command === 'string'
    && typeof record.url === 'string'
    && Number.isInteger(record.port)
    && typeof record.logPath === 'string'
    && typeof record.recordedAt === 'string';
}

/** The record at `recordPath`, or undefined when there is none. A file that is not a record throws and is never acted on. */
export function readDetachedRecord(recordPath: string): DetachedRecord | undefined {
  let text: string;
  try {
    text = readFileSync(recordPath, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
  let value: unknown;
  try { value = JSON.parse(text); } catch { throw new Error(`dev server record ${recordPath} is not JSON`); }
  if (!isDetachedRecord(value)) throw new Error(`dev server record ${recordPath} is malformed`);
  return value;
}

/** Writes a new record. It refuses to replace an existing file, so a second server cannot take over the first one's record. */
export function writeDetachedRecord(recordPath: string, record: DetachedRecord): void {
  mkdirSync(path.dirname(recordPath), { recursive: true });
  writeFileSync(recordPath, `${JSON.stringify(record, null, 2)}\n`, { flag: 'wx' });
}

export function removeDetachedRecord(recordPath: string): void {
  rmSync(recordPath, { force: true });
}

/** The last 16 KiB of a log file, or '' when it cannot be read. Never throws. */
export function readLogTail(logPath: string): string {
  let fd: number | undefined;
  try {
    fd = openSync(logPath, 'r');
    const size = fstatSync(fd).size;
    const start = Math.max(0, size - LOG_TAIL_LIMIT);
    const buffer = Buffer.alloc(size - start);
    readSync(fd, buffer, 0, buffer.length, start);
    return buffer.toString('utf8');
  } catch {
    return '';
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

const normalizeCommand = (command: string) => command.replace(/\s+/g, ' ').trim();

/** Why `live` is not the process that `expected` describes, or undefined when it is. The start time is checked first, since it changes when a pid is reused. */
function identityMismatch(live: ProcessIdentity, expected: Pick<ProcessIdentity, 'startTime' | 'command'>): string | undefined {
  if (live.startTime !== expected.startTime) return 'its start time differs from the record, so the pid was reused';
  if (normalizeCommand(live.command) !== normalizeCommand(expected.command)) return 'its command differs from the record';
  return undefined;
}

/** Detached servers this process started and has not stopped: identity and record path, for stopDevServer({ pid }). */
const startedHere = new Map<number, { identity: ProcessIdentity; recordPath: string }>();

/** Called by startDevServer once a detached server is ready. */
export function rememberDetachedServer(identity: ProcessIdentity, recordPath: string): void {
  startedHere.set(identity.pid, { identity: { ...identity }, recordPath });
}

/**
 * Stops the process tree rooted at `expected.pid`, but only while the live process there is the one `expected`
 * describes. Throws DevServerIdentityError, and signals nothing, when it is another process. Removes the record
 * (when given) once nothing is left, and reports whether the process was running.
 */
export async function stopRecordedProcess(expected: ProcessIdentity, runtime: DevServerRuntime, recordPath?: string): Promise<{ wasRunning: boolean; leftoverPids: number[] }> {
  const live = runtime.lookup(expected.pid);
  if (live) {
    const mismatch = identityMismatch(live, expected);
    if (mismatch) throw new DevServerIdentityError(expected.pid, mismatch);
    await runtime.stopTree(expected.pid);
  }
  const after = runtime.lookup(expected.pid);
  const leftoverPids = after && identityMismatch(after, expected) === undefined ? [expected.pid] : [];
  if (leftoverPids.length === 0) {
    startedHere.delete(expected.pid);
    if (recordPath !== undefined) removeDetachedRecord(recordPath);
  }
  return { wasRunning: live !== undefined, leftoverPids };
}

/**
 * Stops a detached dev server. With `{ recordPath }` it reads the record and checks the live process against the
 * record's start time and command; with `{ pid }` it checks against the identity this process recorded when it
 * started the server. A pid whose identity does not match is refused with DevServerIdentityError, and nothing is
 * signalled. Returns false when nothing was running (a missing record is a no-op), true when a running server was
 * stopped. A record is removed once its server is gone; a server that survives the stop throws, and keeps its record.
 */
export async function stopDevServer(target: { pid: number } | { recordPath: string }, runtime: Partial<DevServerRuntime> = {}): Promise<boolean> {
  const rt: DevServerRuntime = { ...defaultDevServerRuntime, ...runtime };
  let expected: ProcessIdentity;
  let recordPath: string | undefined;
  if ('recordPath' in target) {
    const record = readDetachedRecord(target.recordPath);
    if (!record) return false;
    expected = { pid: record.pid, startTime: record.startTime, command: record.command };
    recordPath = target.recordPath;
  } else {
    const entry = startedHere.get(target.pid);
    if (!entry) throw new DevServerIdentityError(target.pid, 'no identity is recorded for this pid in this process; stop it with recordPath');
    expected = entry.identity;
    recordPath = entry.recordPath;
  }
  const { wasRunning, leftoverPids } = await stopRecordedProcess(expected, rt, recordPath);
  if (leftoverPids.length > 0) throw new Error(`dev server pid ${expected.pid} is still running after stop; its record was kept`);
  return wasRunning;
}
