import type { ChildProcess } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync } from 'node:fs';
import { createServer } from 'node:net';
import path from 'node:path';
import { TimeoutError, validateTimeout } from './cdp.js';
import { freePort, ownedProcessReceipt, spawnOwnedProcess, stopOwnedProcess, type OwnedProcess } from './process.js';
import {
  defaultDevServerRuntime, readLogTail, rememberDetachedServer, stopRecordedProcess, writeDetachedRecord,
  type DetachedRecord, type DevServerRuntime,
} from './devserver-detached.js';
export { DevServerIdentityError, readDetachedRecord, stopDevServer, type DetachedRecord, type DevServerRuntime, type ProcessIdentity } from './devserver-detached.js';

/** Bounded tail of the server's stdout and stderr kept for error messages. */
const OUTPUT_LIMIT = 16 * 1024;
/** Output that means the server lost its port race. */
const COLLISION = /EADDRINUSE|address already in use/i;

/**
 * Why a dev server failed to become ready. `port-in-use`: every candidate port was taken.
 * `exited`: the process ended first. `timeout`: the total budget passed. `check-failed`: the
 * caller's check threw. `start-failed`: the command could not be started.
 */
export type DevServerFailure = 'exited' | 'timeout' | 'port-in-use' | 'check-failed' | 'start-failed';

export interface DevServerReadiness {
  /**
   * URLs that must each answer 2xx to a GET. Required unless `check` is given. A function receives
   * the chosen port.
   */
  urls?: string[] | ((port: number) => string[]);
  /** Bound for one request (or one `check` call). */
  attemptTimeoutMs: number;
  /** Bound for the readiness wait of one launch attempt. */
  totalTimeoutMs: number;
  /** Delay between rounds of probes. */
  intervalMs: number;
  /** Caller predicate used instead of the URL probe. True means ready. A throw fails the launch. */
  check?: (port: number) => boolean | Promise<boolean>;
}

export interface DevServerOptions {
  /** Executable to start, hidden by default. */
  command: string;
  /** Arguments; every `{port}` is replaced with the chosen port. */
  args?: string[];
  cwd?: string;
  /** Merged over process.env. The chosen port is set under `portEnv`. */
  env?: NodeJS.ProcessEnv;
  /** Run through the platform shell (for .cmd shims such as npm on Windows). Default false. */
  shell?: boolean;
  /** Default true: no visible console window on Windows. */
  windowsHide?: boolean;
  /** Candidate ports, tried in order. A collision moves to the next one. */
  ports?: number[];
  /** Without `ports`: the first free port at or above this one is chosen (probed by binding). */
  portFloor?: number;
  /** Environment variable that receives the port. Default `PORT`. */
  portEnv?: string;
  /** Launch attempts before giving up on port collisions. Default 5. With `ports`, at most the list length. */
  maxPortAttempts?: number;
  readiness: DevServerReadiness;
  /**
   * Default false: the server is stopped with this process's tree, as before. True: the server outlives this process.
   * It gets its own process group (POSIX) or a detached process (Windows), its output goes to `<recordPath>.log`
   * (never to this process's pipes), and once ready a JSON record is written to `recordPath`. Requires recordPath.
   * Stop it later with stopDevServer({ recordPath }).
   */
  detached?: boolean;
  /** Absolute path of a detached server's record. It must not exist yet. Requires detached. */
  recordPath?: string;
  /** @internal Replaces spawn, process lookup and tree stop for detached servers. Tests use it; leave it unset. */
  runtime?: Partial<DevServerRuntime>;
}

export interface DevServerStopReport {
  /** Pids this launch started that are still running after the stop. Empty when the tree is gone. */
  leftoverPids: number[];
}

/** A dev server that is ready. The process was launched by this package and is stopped by `stop()`. */
export interface DevServer {
  pid: number;
  port: number;
  /** `http://127.0.0.1:<port>`. */
  url: string;
  /** Launch attempts it took, including the successful one. */
  attempts: number;
  /** Detached servers only: the record file, the log file, and the process identity the record holds. */
  recordPath?: string;
  logPath?: string;
  startTime?: string;
  command?: string;
  /** Stops the whole process tree this launch started. Idempotent. */
  stop(): Promise<DevServerStopReport>;
  /** The bounded tail of stdout and stderr so far. */
  output(): string;
}

/** A failed launch. Its process tree has already been stopped; `leftoverPids` lists what remained. */
export class DevServerError extends Error {
  readonly reason: DevServerFailure;
  readonly pid?: number;
  readonly port?: number;
  readonly attempts: number;
  /** Bounded tail of the server's output. */
  readonly output: string;
  readonly leftoverPids: number[];
  constructor(reason: DevServerFailure, detail: string, fields: { pid?: number; port?: number; attempts: number; output: string; leftoverPids: number[] }) {
    const tail = fields.output.trim().slice(-2_000);
    super(`dev server ${reason}: ${detail}${tail ? `\n--- output tail ---\n${tail}` : ''}`);
    this.name = 'DevServerError';
    this.reason = reason;
    this.pid = fields.pid;
    this.port = fields.port;
    this.attempts = fields.attempts;
    this.output = fields.output;
    this.leftoverPids = fields.leftoverPids;
  }
}

function validPort(port: number): boolean { return Number.isInteger(port) && port >= 1 && port <= 65535; }

function validateOptions(options: DevServerOptions) {
  if (typeof options.command !== 'string' || !options.command) throw new Error('command must be a non-empty string');
  const { readiness } = options;
  validateTimeout(readiness.attemptTimeoutMs);
  validateTimeout(readiness.totalTimeoutMs);
  if (!Number.isFinite(readiness.intervalMs) || readiness.intervalMs <= 0) throw new Error('readiness.intervalMs must be a positive number');
  if (readiness.urls === undefined && !readiness.check) throw new Error('readiness needs urls or check');
  if (options.ports !== undefined && (!Array.isArray(options.ports) || options.ports.length === 0 || !options.ports.every(validPort))) {
    throw new Error('ports must be a non-empty list of ports from 1 to 65535');
  }
  if (options.portFloor !== undefined && !validPort(options.portFloor)) throw new Error('portFloor must be a port from 1 to 65535');
  if (options.maxPortAttempts !== undefined && (!Number.isInteger(options.maxPortAttempts) || options.maxPortAttempts < 1)) throw new Error('maxPortAttempts must be a positive integer');
  if (options.portEnv !== undefined && !options.portEnv) throw new Error('portEnv must be a non-empty name');
  if (options.detached !== undefined && typeof options.detached !== 'boolean') throw new Error('detached must be a boolean');
  if (options.detached) {
    if (typeof options.recordPath !== 'string' || !path.isAbsolute(options.recordPath)) throw new Error('detached needs recordPath, an absolute path');
  } else if (options.recordPath !== undefined) {
    throw new Error('recordPath requires detached: true');
  }
}

/** Binds 127.0.0.1:<port> briefly; true when nothing else holds it. */
function portFreeOn(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const server = createServer();
    server.once('error', () => resolve(false));
    server.listen(port, '127.0.0.1', () => server.close(() => resolve(true)));
  });
}
/** The first free port at or above `floor`, skipping `skip`. */
async function probeFrom(floor: number, skip: Set<number>): Promise<number | undefined> {
  for (let port = floor; port <= 65_535; port++) {
    if (!skip.has(port) && (await portFreeOn(port))) return port;
  }
  return undefined;
}
async function probeUrl(url: string, timeoutMs: number): Promise<boolean> {
  try {
    const response = await fetch(url, { redirect: 'manual', signal: AbortSignal.timeout(timeoutMs) });
    await response.body?.cancel().catch(() => {});
    return response.status >= 200 && response.status < 300;
  } catch {
    return false;
  }
}
/** One round of readiness: every URL answers 2xx, or the check resolves true. A hung check counts as not ready. */
async function roundReady(readiness: DevServerReadiness, port: number, attemptTimeoutMs: number): Promise<boolean> {
  if (readiness.check) {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([
        Promise.resolve().then(() => readiness.check!(port)),
        new Promise<boolean>((resolve) => { timer = setTimeout(() => resolve(false), attemptTimeoutMs); }),
      ]);
    } finally { clearTimeout(timer); }
  }
  const urls = typeof readiness.urls === 'function' ? readiness.urls(port) : readiness.urls ?? [];
  for (const url of urls) if (!(await probeUrl(url, attemptTimeoutMs))) return false;
  return true;
}
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/**
 * Starts a dev server hidden, chooses its port, and waits until it is ready. Failure, including a
 * port collision that exhausts the candidates, stops the whole process tree this launch started and
 * reports leftovers. A port collision (EADDRINUSE in the output) moves to the next candidate.
 *
 * A probe cannot take a floor, so `portFloor` probes upward from the floor with the same bind test
 * that `freePort` uses; another process can still take the port before the server binds it.
 */
export async function startDevServer(options: DevServerOptions): Promise<DevServer> {
  validateOptions(options);
  // Validated above: a detached server has an absolute recordPath, and any other server has none.
  const recordPath = options.detached ? options.recordPath : undefined;
  if (recordPath !== undefined && existsSync(recordPath)) throw new Error(`record already exists at ${recordPath}: stop that server with stopDevServer first`);
  if (recordPath !== undefined) mkdirSync(path.dirname(recordPath), { recursive: true });
  const runtime: DevServerRuntime = { ...defaultDevServerRuntime, ...options.runtime };
  const portEnv = options.portEnv ?? 'PORT';
  const maxAttempts = Math.min(options.maxPortAttempts ?? 5, options.ports?.length ?? Number.POSITIVE_INFINITY);
  const candidates = options.ports ? [...options.ports] : undefined;
  const tried = new Set<number>();
  let floor = options.portFloor;
  let attempts = 0;
  for (;;) {
    if (attempts >= maxAttempts) throw new DevServerError('port-in-use', `no free port after ${attempts} attempt(s)`, { attempts, output: '', leftoverPids: [] });
    let port: number | undefined;
    if (candidates) {
      port = candidates.shift();
      // A listed port that is already held is skipped before anything is spawned on it. Probing a
      // foreign listener's port would count its answers as readiness, and would keep sockets to it open.
      attempts++;
      if (port !== undefined && !(await portFreeOn(port))) { tried.add(port); continue; }
    } else if (floor !== undefined) port = await probeFrom(floor, tried);
    else port = await freePort();
    if (port === undefined) throw new DevServerError('port-in-use', candidates ? 'every listed port is in use' : `no free port at or above ${floor}`, { attempts, output: '', leftoverPids: [] });
    tried.add(port);
    floor = port + 1;
    if (!candidates) attempts++;
    const launched = recordPath !== undefined
      ? await launchDetachedOnce(options, runtime, port, portEnv, attempts, recordPath)
      : await launchOnce(options, port, portEnv, attempts);
    if (launched) return launched;
  }
}

/** Starts the detached child with stdio on a fresh log file, then closes this process's copy of the descriptor. */
function spawnDetachedChild(options: DevServerOptions, runtime: DevServerRuntime, port: number, portEnv: string, logPath: string, attempts: number): ChildProcess {
  const logFd = openSync(logPath, 'w'); // a fresh log per attempt, so an earlier collision cannot be read back as this one's
  try {
    return runtime.spawn(options.command, (options.args ?? []).map((arg) => arg.replaceAll('{port}', String(port))), {
      cwd: options.cwd,
      env: { ...process.env, ...options.env, [portEnv]: String(port) },
      stdio: ['ignore', logFd, logFd],
      windowsHide: options.windowsHide ?? true,
      shell: options.shell ?? false,
      detached: true,
    });
  } catch (error) {
    throw new DevServerError('start-failed', error instanceof Error ? error.message : String(error), { port, attempts, output: readLogTail(logPath), leftoverPids: [] });
  } finally {
    closeSync(logFd);
  }
}

/**
 * One detached launch on one port. The server gets its own process group (POSIX) or a detached process (Windows), and
 * its stdio is a log file, never this process's pipes, so it keeps running after this process exits. The child is
 * unreferenced. Every kill is guarded by the start time read right after the spawn, so a recycled pid is never signalled.
 * Once ready, its record is written and its identity is kept for stopDevServer({ pid }). Returns undefined after a port
 * collision, once the server it started is stopped.
 */
async function launchDetachedOnce(options: DevServerOptions, runtime: DevServerRuntime, port: number, portEnv: string, attempts: number, recordPath: string): Promise<DevServer | undefined> {
  const { readiness } = options;
  const logPath = `${recordPath}.log`;
  const child = spawnDetachedChild(options, runtime, port, portEnv, logPath, attempts);
  const state: { exit?: { code: number | null; signal: NodeJS.Signals | null }; spawnError?: Error } = {};
  child.on('error', (error) => { state.spawnError ??= error; });
  child.once('exit', (code, signal) => { state.exit = { code, signal }; });
  child.unref();
  const pid = child.pid;
  if (pid === undefined) {
    throw new DevServerError('start-failed', state.spawnError?.message ?? `failed to start ${options.command}`, { port, attempts, output: readLogTail(logPath), leftoverPids: [] });
  }
  const launchedAs = runtime.lookup(pid);
  if (!launchedAs) {
    throw new DevServerError('exited', 'process exited before it could be identified', { pid, port, attempts, output: readLogTail(logPath), leftoverPids: [] });
  }
  const ours = (): boolean => {
    const live = runtime.lookup(pid);
    return live !== undefined && live.startTime === launchedAs.startTime;
  };
  const output = () => readLogTail(logPath);
  const finish = async (reason: DevServerFailure | 'collision', detail: string): Promise<undefined> => {
    if (ours()) await runtime.stopTree(pid).catch(() => {});
    const leftoverPids = ours() ? [pid] : [];
    if (reason === 'collision' && leftoverPids.length === 0) return undefined;
    throw new DevServerError(reason === 'collision' ? 'port-in-use' : reason, detail, { pid, port, attempts, output: output(), leftoverPids });
  };

  const deadline = Date.now() + readiness.totalTimeoutMs;
  for (;;) {
    if (COLLISION.test(output())) return finish('collision', `port ${port} is in use`);
    if (state.spawnError) return finish('start-failed', state.spawnError.message);
    if (state.exit) return finish('exited', `process exited before ready (code ${state.exit.code}, signal ${state.exit.signal})`);
    let isReady: boolean;
    try {
      isReady = await roundReady(readiness, port, readiness.attemptTimeoutMs);
    } catch (error) {
      return finish('check-failed', error instanceof Error ? error.message : String(error));
    }
    if (isReady) {
      const identity = runtime.lookup(pid);
      if (!identity || identity.startTime !== launchedAs.startTime) return finish('exited', 'process exited before ready');
      const url = `http://127.0.0.1:${port}`;
      const record: DetachedRecord = {
        format: 'rightkit-dev-server/1', pid, startTime: identity.startTime, command: identity.command,
        url, port, logPath, recordedAt: new Date().toISOString(),
      };
      try {
        writeDetachedRecord(recordPath, record);
      } catch (error) {
        return finish('start-failed', `could not write record ${recordPath}: ${error instanceof Error ? error.message : String(error)}`);
      }
      rememberDetachedServer(identity, recordPath);
      let stopping: Promise<DevServerStopReport> | undefined;
      return {
        pid, port, url, attempts, recordPath, logPath, startTime: identity.startTime, command: identity.command, output,
        stop: () => (stopping ??= stopRecordedProcess(identity, runtime, recordPath).then(({ leftoverPids }) => ({ leftoverPids }))),
      };
    }
    if (Date.now() >= deadline) return finish('timeout', `not ready after ${readiness.totalTimeoutMs}ms on port ${port}`);
    await sleep(Math.max(1, Math.min(readiness.intervalMs, deadline - Date.now())));
  }
}

export interface WaitForHttpOptions {
  /** Total budget in ms. Default 120000. Each attempt is capped at min(30 s, remaining time), never below 1 s. */
  timeoutMs?: number;
  /** Delay between attempts in ms. Default 250. */
  intervalMs?: number;
}

/**
 * Polls `url` with GET until it answers with a 2xx status. A refused, reset or timed-out attempt just means
 * not yet ready. When the total budget passes, it throws a TimeoutError with the message
 * `Timed out waiting for <url>`.
 */
export async function waitForHttp(url: string, options: WaitForHttpOptions = {}): Promise<void> {
  let target: URL;
  try { target = new URL(url); } catch { throw new Error('waitForHttp url must be an http or https URL'); }
  if (target.protocol !== 'http:' && target.protocol !== 'https:') throw new Error('waitForHttp url must be an http or https URL');
  const timeoutMs = validateTimeout(options.timeoutMs ?? 120_000);
  const intervalMs = options.intervalMs ?? 250;
  if (!Number.isFinite(intervalMs) || intervalMs <= 0) throw new Error('intervalMs must be a positive number');
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const attemptMs = Math.max(1_000, Math.min(30_000, deadline - Date.now()));
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(attemptMs) });
      await response.body?.cancel().catch(() => {});
      if (response.ok) return;
    } catch {
      // Refused, reset or timed out: the server is not up yet.
    }
    if (Date.now() >= deadline) throw new TimeoutError(`Timed out waiting for ${url}`);
    await sleep(Math.max(1, Math.min(intervalMs, deadline - Date.now())));
  }
}

/** One launch on one port. Returns the ready server, or undefined after a port collision (already stopped). */
async function launchOnce(options: DevServerOptions, port: number, portEnv: string, attempts: number): Promise<DevServer | undefined> {
  const { readiness } = options;
  let handle: OwnedProcess;
  try {
    handle = spawnOwnedProcess(options.command, (options.args ?? []).map((arg) => arg.replaceAll('{port}', String(port))), {
      cwd: options.cwd,
      env: { ...process.env, ...options.env, [portEnv]: String(port) },
      stdio: 'pipe',
      windowsHide: options.windowsHide ?? true,
      shell: options.shell ?? false,
    });
  } catch (error) {
    throw new DevServerError('start-failed', error instanceof Error ? error.message : String(error), { port, attempts, output: '', leftoverPids: [] });
  }
  let output = '';
  const append = (chunk: string) => { output = (output + chunk).slice(-OUTPUT_LIMIT); };
  handle.child.stdout?.setEncoding('utf8');
  handle.child.stderr?.setEncoding('utf8');
  handle.child.stdout?.on('data', append);
  handle.child.stderr?.on('data', append);
  let exit: { code: number | null; signal: NodeJS.Signals | null } | undefined;
  handle.child.once('exit', (code, signal) => { exit = { code, signal }; });

  /** Stops this launch's tree, then fails, or reports a collision as a retry (undefined) when nothing leaked. */
  const finish = async (reason: DevServerFailure | 'collision', detail: string): Promise<undefined> => {
    await stopOwnedProcess(handle).catch(() => {});
    const leftoverPids = ownedProcessReceipt().leftoverPids.filter((pid) => pid === handle.pid);
    if (reason === 'collision' && leftoverPids.length === 0) return undefined;
    throw new DevServerError(reason === 'collision' ? 'port-in-use' : reason, detail, { pid: handle.pid, port, attempts, output, leftoverPids });
  };

  const deadline = Date.now() + readiness.totalTimeoutMs;
  for (;;) {
    if (COLLISION.test(output)) return finish('collision', `port ${port} is in use`);
    if (exit) return finish('exited', `process exited before ready (code ${exit.code}, signal ${exit.signal})`);
    let ready: boolean;
    try {
      ready = await roundReady(readiness, port, readiness.attemptTimeoutMs);
    } catch (error) {
      return finish('check-failed', error instanceof Error ? error.message : String(error));
    }
    if (ready) {
      let stopping: Promise<DevServerStopReport> | undefined;
      return {
        pid: handle.pid,
        port,
        url: `http://127.0.0.1:${port}`,
        attempts,
        stop: () => (stopping ??= (async () => {
          await stopOwnedProcess(handle);
          return { leftoverPids: ownedProcessReceipt().leftoverPids.filter((pid) => pid === handle.pid) };
        })()),
        output: () => output,
      };
    }
    if (Date.now() >= deadline) return finish('timeout', `not ready after ${readiness.totalTimeoutMs}ms on port ${port}`);
    await sleep(Math.max(1, Math.min(readiness.intervalMs, deadline - Date.now())));
  }
}
