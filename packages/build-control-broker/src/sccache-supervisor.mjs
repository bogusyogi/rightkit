import { spawn as nodeSpawn } from 'node:child_process';
import { chmod as nodeChmod, lstat as nodeLstat, unlink as nodeUnlink } from 'node:fs/promises';
import net from 'node:net';
import { basename, join } from 'node:path';
import {
  enumerateLiveDescendants,
  managedProcessOwnership,
  readManagedProcessIdentity,
  spawnManagedProcess,
  startManagedProcess,
  terminateProcessTree
} from './process-tree.mjs';

/**
 * Coordinates one broker-owned sccache server.
 *
 * The server is started lazily, at the first managed non-CONTROL job rather
 * than at broker boot: an unhealthy cache must degrade to direct rustc, never
 * take the broker down with it.
 */
export class SccacheSupervisor {
  #tool; #spawn; #timeoutMs; #mode; #ownershipProof; #startServer; #startTimeoutMs;
  #socketPath; #notifyPath; #cacheDir; #maxSizeBytes; #errorLogPath;
  #restartBudget; #restarts = 0; #terminal = null; #degradedReason; #onOwnership; #graceMs; #terminate; #unlink;
  #serverPromise = null; #serverChild = null; #serverPid = null; #shutdownPromise = null;
  constructor({
    tool, spawn = nodeSpawn, timeoutMs = 10_000, mode = 'disabled', ownershipProof = null,
    startServer = startBrokerOwnedSccacheServer, externalRoot = null,
    socketPath = externalRoot ? brokerSccacheSocketPath(externalRoot) : null,
    notifyPath = externalRoot ? brokerSccacheNotifyPath(externalRoot) : null,
    cacheDir = null, maxSizeBytes = null, errorLogPath = null,
    restartBudget = 3, onOwnership = null, startTimeoutMs = 30_000, graceMs = 5_000,
    degradedReason = null, terminate = terminateProcessTree, unlink = nodeUnlink
  } = {}) {
    if (!tool?.path || !Number.isSafeInteger(timeoutMs) || timeoutMs < 10) throw new TypeError('sccache tool & bounded timeout are required');
    if (!['disabled', 'managed'].includes(mode)) throw new TypeError('sccache mode must be disabled or managed');
    if (mode === 'managed' && typeof ownershipProof !== 'function') throw new TypeError('managed sccache requires broker ownership proof');
    if (!Number.isSafeInteger(restartBudget) || restartBudget < 1) throw new TypeError('positive sccache restart budget is required');
    if (!Number.isSafeInteger(startTimeoutMs) || startTimeoutMs < 10) throw new TypeError('bounded sccache start timeout is required');
    this.#tool = tool; this.#spawn = spawn; this.#timeoutMs = timeoutMs; this.#mode = mode; this.#ownershipProof = ownershipProof;
    this.#startServer = startServer; this.#startTimeoutMs = startTimeoutMs; this.#socketPath = socketPath; this.#notifyPath = notifyPath;
    this.#cacheDir = cacheDir; this.#maxSizeBytes = maxSizeBytes; this.#errorLogPath = errorLogPath;
    this.#restartBudget = restartBudget; this.#onOwnership = onOwnership; this.#graceMs = graceMs;
    this.#degradedReason = degradedReason; this.#terminate = terminate; this.#unlink = unlink;
  }

  /** The live server pid, injected into the ownership proof instead of a ps scan. */
  get serverPid() { return this.#serverPid; }

  ensureStarted() {
    if (this.#mode === 'disabled') return Promise.reject(new SccacheOwnershipError('RK_SCCACHE_DISABLED', 'sccache supervision is disabled'));
    return this.#ensureServer();
  }

  /**
   * Single-flight start. `--start-server` is deliberately gone: it daemonizes,
   * which makes broker descendant ownership permanently unprovable.
   */
  #ensureServer() {
    if (this.#terminal) return Promise.reject(new SccacheOwnershipError(this.#terminal.code, this.#terminal.reason));
    if (this.#serverPromise) return this.#serverPromise;
    // The budget is spent per start ATTEMPT, not per observed 'close': a start
    // that fails before the child ever exists emits no close, and counting
    // those would leave a repeatedly-failing server retried without bound.
    if (this.#restarts >= this.#restartBudget) {
      this.#terminal = { code: 'RK_SCCACHE_RESTART_BUDGET', reason: 'sccache-server-restart-budget-exhausted' };
      return Promise.reject(new SccacheOwnershipError(this.#terminal.code, this.#terminal.reason));
    }
    this.#restarts += 1;
    const started = Promise.resolve()
      .then(() => this.#startServer({
        tool: this.#tool, socketPath: this.#socketPath, notifyPath: this.#notifyPath,
        cacheDir: this.#cacheDir, maxSizeBytes: this.#maxSizeBytes, errorLogPath: this.#errorLogPath,
        spawn: this.#spawn, timeoutMs: this.#startTimeoutMs
      }))
      .then(async (outcome) => {
        this.#serverChild = outcome.child; this.#serverPid = outcome.pid;
        outcome.child?.once?.('close', () => {
          if (this.#serverPromise !== started) return;
          this.#serverPromise = null; this.#serverChild = null; this.#serverPid = null;
        });
        if (typeof this.#onOwnership === 'function' && outcome.ownership) await this.#onOwnership(outcome.ownership);
        return Object.freeze({ state: 'RUNNING', pid: outcome.pid });
      })
      .catch((error) => {
        if (this.#serverPromise === started) { this.#serverPromise = null; this.#serverChild = null; this.#serverPid = null; }
        throw error instanceof SccacheOwnershipError ? error : new SccacheOwnershipError('RK_SCCACHE_START_FAILED', error?.message ?? 'managed sccache server failed to start');
      });
    this.#serverPromise = started;
    return started;
  }

  /**
   * Start, then prove, then probe. Proving before starting is a deadlock: the
   * proof requires a live broker-descendant server that only the start creates.
   */
  async sample(environment) {
    if (this.#degradedReason) return degraded(this.#degradedReason);
    try {
      if (this.#mode === 'disabled') throw new SccacheOwnershipError('RK_SCCACHE_DISABLED', 'sccache supervision is disabled');
      await this.#ensureServer();
      const proof = await this.#ownershipProof?.({ environment: Object.freeze({ ...environment }), tool: this.#tool, serverPid: this.#serverPid });
      if (this.#mode === 'managed' && proof !== true && proof?.owned !== true) throw new SccacheOwnershipError('RK_SCCACHE_OWNERSHIP_UNPROVEN', proof?.reason || 'broker cannot prove sccache server child ownership');
      const result = await run(this.#tool.path, ['--show-stats', '--stats-format=json'], environment, this.#spawn, this.#timeoutMs);
      if (result.code !== 0) throw new Error(result.stderr.trim() || `stats exited ${result.code}`);
      return Object.freeze({ status: 'OK', stats: Object.freeze(JSON.parse(result.stdout)) });
    } catch (error) { return degraded(error.code || (error.message === 'sccache supervision is disabled' ? 'RK_SCCACHE_DISABLED' : error.message)); }
  }

  /**
   * Terminate the owned tree & remove both sockets. SIGTERM leaves the server
   * socket file behind, and a stale one is actively harmful: it satisfies
   * isSocket + uid + mode and only fails later, at the stats probe.
   */
  shutdown() {
    if (this.#shutdownPromise) return this.#shutdownPromise;
    this.#shutdownPromise = (async () => {
      const child = this.#serverChild;
      this.#serverPromise = null; this.#serverChild = null; this.#serverPid = null;
      this.#terminal ??= { code: 'RK_SCCACHE_SHUTDOWN', reason: 'sccache-supervisor-shutdown' };
      const reap = child ? await this.#terminate(child, { graceMs: this.#graceMs }).catch((error) => Object.freeze({ status: 'REAP_PENDING', reaped: false, reason: error?.message ?? 'sccache-termination-failed' })) : null;
      for (const path of [this.#socketPath, this.#notifyPath]) {
        if (typeof path !== 'string' || !path) continue;
        try { await this.#unlink(path); } catch (error) { if (error?.code !== 'ENOENT') throw error; }
      }
      return Object.freeze({ status: 'SHUTDOWN', reap });
    })();
    return this.#shutdownPromise;
  }
  delta(before, after) {
    if (before?.status !== 'OK' || after?.status !== 'OK') return Object.freeze({ status: 'DEGRADED', before: before?.reason ?? before?.status ?? 'missing', after: after?.reason ?? after?.status ?? 'missing' });
    const left = flattenNumbers(before.stats); const right = flattenNumbers(after.stats); const changes = {};
    for (const [key, value] of Object.entries(right)) if (left[key] !== undefined) changes[key] = value - left[key];
    return Object.freeze({ status: 'OK', delta: Object.freeze(changes) });
  }
}

export class SccacheOwnershipError extends Error {
  constructor(code, message) { super(message); this.name = 'SccacheOwnershipError'; this.code = code; }
}

/**
 * Return an explicit direct-rustc fallback without choosing it for a caller.
 * Callers must persist/report this mode before executing with this environment.
 */
export function directRustcFallback(environment, diagnostic) {
  if (!environment || typeof environment !== 'object' || Array.isArray(environment)) throw new TypeError('environment is required');
  const reason = diagnostic?.reason ?? diagnostic?.status ?? 'cache-unavailable';
  const { RUSTC_WRAPPER: _wrapper, SCCACHE_SERVER_UDS: _socket, ...fallbackEnvironment } = environment;
  return Object.freeze({ status: 'FALLBACK_DIRECT_RUSTC', reason: String(reason), environment: Object.freeze(fallbackEnvironment) });
}

export const SCCACHE_SOCKET_NAME = '.rk-sccache.sock';
export const SCCACHE_NOTIFY_NAME = '.rk-sccache-notify.sock';
/** `sockaddr_un.sun_path` is 104 bytes on macOS, including its NUL terminator. */
export const SCCACHE_SOCKET_PATH_LIMIT = 103;

/** The broker owns this path; nothing below it is client-supplied. */
export function brokerSccacheSocketPath(externalRoot) {
  if (typeof externalRoot !== 'string' || !externalRoot) throw new TypeError('broker external root is required');
  return join(externalRoot, SCCACHE_SOCKET_NAME);
}

/** Readiness-notification socket, broker-owned on the same terms as the server socket. */
export function brokerSccacheNotifyPath(externalRoot) {
  if (typeof externalRoot !== 'string' || !externalRoot) throw new TypeError('broker external root is required');
  return join(externalRoot, SCCACHE_NOTIFY_NAME);
}

/** A deep external root can exceed sun_path; say so here rather than fail opaquely at bind. */
export function sccacheSocketPathsWithinLimit(...paths) {
  return paths.every((path) => typeof path === 'string' && Buffer.byteLength(path, 'utf8') <= SCCACHE_SOCKET_PATH_LIMIT);
}

/**
 * Broker-internal SERVER-START environment. This is disjoint from the job
 * environment by construction & neither is derived from the other:
 * `SCCACHE_START_SERVER` in a CLIENT environment is fatal ("can't be used with
 * other commands"), which would break the ownership proof's own stats probe.
 */
export function brokerSccacheServerEnvironment({ socketPath, notifyPath, cacheDir, maxSizeBytes, errorLogPath = null, env = process.env } = {}) {
  if (![socketPath, notifyPath, cacheDir].every((value) => typeof value === 'string' && value)) throw new TypeError('sccache server socket, notify & cache paths are required');
  if (!Number.isSafeInteger(maxSizeBytes) || maxSizeBytes < 1) throw new TypeError('positive sccache cache size is required');
  return Object.freeze({
    PATH: env.PATH ?? '',
    HOME: env.HOME ?? '',
    SCCACHE_DIR: cacheDir,
    SCCACHE_CACHE_SIZE: String(maxSizeBytes),
    SCCACHE_SERVER_UDS: socketPath,
    SCCACHE_START_SERVER: '1',
    SCCACHE_NO_DAEMON: '1',
    // 0 disables idle shutdown. The 600s default would silently kill the
    // server between builds & every later job would degrade.
    SCCACHE_IDLE_TIMEOUT: '0',
    SCCACHE_STARTUP_NOTIFY: notifyPath,
    SCCACHE_MAX_FRAME_LENGTH: String(64 * 1024 * 1024),
    ...(typeof errorLogPath === 'string' && errorLogPath ? { SCCACHE_ERROR_LOG: errorLogPath } : {})
  });
}

/**
 * Start one broker-owned sccache server as a live descendant: launcher →
 * sccache, in the foreground (`SCCACHE_NO_DAEMON`), with umask 0o077 so every
 * inode it creates is private to this uid.
 *
 * The umask is deliberately NOT 0o177. A process-wide umask cannot satisfy both
 * "0600 server socket" and "usable per-compile temp directories": 0o177 strips
 * the owner execute bit, so the server's own `mkdir` of its temp directory
 * yields mode 0600, a directory nothing can traverse, and the very first
 * cacheable compile fails with EACCES writing `deps.d` inside it. 0o077 gives
 * 0700 directories (private AND usable) at the cost of a 0755 socket, so the
 * socket is tightened explicitly with chmod once the server has bound it.
 *
 * Accepted race: between the server's bind and this chmod the socket is
 * briefly 0755. It cannot be pre-created — sccache binds the path itself — and
 * it lives inside the broker-owned external root, so the window is bounded to a
 * directory no other uid can enter. A chmod failure is a start failure: never
 * proceed to the ownership proof with a world-accessible socket.
 */
export async function startBrokerOwnedSccacheServer({
  tool, socketPath, notifyPath, cacheDir, maxSizeBytes, errorLogPath = null,
  spawn = nodeSpawn, timeoutMs = 30_000, env = process.env,
  spawnManaged = spawnManagedProcess, startManaged = startManagedProcess,
  readIdentity = readManagedProcessIdentity, ownershipOf = managedProcessOwnership,
  createServer = net.createServer, unlink = nodeUnlink, chmod = nodeChmod
} = {}) {
  if (typeof tool?.path !== 'string' || !tool.path) throw new TypeError('verified sccache tool is required');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 10) throw new TypeError('bounded sccache start timeout is required');
  const serverEnvironment = brokerSccacheServerEnvironment({ socketPath, notifyPath, cacheDir, maxSizeBytes, errorLogPath, env });
  try { await unlink(notifyPath); } catch (error) { if (error?.code !== 'ENOENT') throw error; }
  const listener = createServer();
  const close = () => new Promise((resolve) => { try { listener.close(() => resolve()); } catch { resolve(); } });
  const notified = new Promise((resolve, reject) => {
    listener.once('error', (error) => reject(new SccacheOwnershipError('RK_SCCACHE_START_FAILED', error?.message ?? 'sccache notify listener failed')));
    listener.once('connection', (socket) => { socket.on('error', () => {}); socket.on('data', () => {}); resolve(true); });
  });
  try {
    await new Promise((resolve, reject) => { listener.once('error', reject); listener.listen(notifyPath, resolve); });
  } catch (error) { await close(); throw new SccacheOwnershipError('RK_SCCACHE_START_FAILED', error?.message ?? 'sccache notify socket unavailable'); }
  let child;
  try {
    child = spawnManaged(tool.path, [], { env: serverEnvironment, umask: 0o077, stdio: ['ignore', 'inherit', 'inherit'] }, spawn);
    const { pid } = await startManaged(child, { timeoutMs });
    await boundedNotify(notified, timeoutMs);
    // The handshake means the socket is bound; tighten it before anything —
    // the ownership proof included — is allowed to look at or use it.
    try { await chmod(socketPath, 0o600); }
    catch (error) { throw new SccacheOwnershipError('RK_SCCACHE_SOCKET_CHMOD_FAILED', `sccache server socket could not be restricted to 0600: ${error?.message ?? error?.code ?? 'unknown'}`); }
    const identity = await readIdentity(child.pid);
    if (identity?.status !== 'OK') throw new SccacheOwnershipError('RK_SCCACHE_START_FAILED', `sccache launcher identity unavailable: ${identity?.reason ?? 'unknown'}`);
    const ownership = ownershipOf(child, { identity });
    await close();
    return Object.freeze({ child, pid, ownership });
  } catch (error) {
    await close();
    if (child) await terminateProcessTree(child, { graceMs: 1_000 }).catch(() => null);
    throw error instanceof SccacheOwnershipError ? error : new SccacheOwnershipError('RK_SCCACHE_START_FAILED', error?.message ?? 'managed sccache server failed to start');
  }
}

function boundedNotify(promise, timeoutMs) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new SccacheOwnershipError('RK_SCCACHE_START_TIMEOUT', 'sccache startup notification timed out')), timeoutMs); timer.unref?.(); })]).finally(() => clearTimeout(timer));
}

/**
 * Any socket already present at broker start predates this broker process — a
 * legacy-v1 leftover is exactly that — so it is never broker-owned. Remove it
 * before a fresh, provable socket is created.
 */
export async function prepareBrokerSccacheSocket({ externalRoot, socketPath = brokerSccacheSocketPath(externalRoot), notifyPath = typeof externalRoot === 'string' && externalRoot ? brokerSccacheNotifyPath(externalRoot) : null, unlink = nodeUnlink } = {}) {
  if (typeof socketPath !== 'string' || !socketPath) throw new TypeError('broker sccache socket path is required');
  for (const path of [socketPath, notifyPath]) {
    if (typeof path !== 'string' || !path) continue;
    try { await unlink(path); } catch (error) { if (error?.code !== 'ENOENT') throw error; }
  }
  return socketPath;
}

/**
 * Build the ownership callback SccacheSupervisor requires. Every check is
 * fail-closed: an unproven server degrades to direct rustc rather than routing
 * managed compilation through a daemon this broker cannot account for.
 */
export function brokerOwnedSccacheProof({
  externalRoot,
  socketPath = brokerSccacheSocketPath(externalRoot),
  brokerPid = process.pid,
  uid = process.getuid?.(),
  lstat = nodeLstat,
  spawn = nodeSpawn,
  timeoutMs = 10_000,
  serverPid = null,
  liveDescendants = enumerateLiveDescendants
} = {}) {
  if (typeof socketPath !== 'string' || !socketPath) throw new TypeError('broker sccache socket path is required');
  if (!Number.isSafeInteger(brokerPid) || brokerPid < 1) throw new TypeError('broker pid is required');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 10) throw new TypeError('bounded sccache proof timeout is required');
  const expected = Buffer.from(socketPath, 'utf8');
  return async ({ environment, tool } = {}) => {
    const supplied = environment?.SCCACHE_SERVER_UDS;
    if (typeof supplied !== 'string' || !supplied || !Buffer.from(supplied, 'utf8').equals(expected)) return unowned('sccache-socket-path-mismatch');
    if (typeof tool?.path !== 'string' || !tool.path) return unowned('sccache-tool-path-missing');
    let stats;
    try { stats = await lstat(socketPath); }
    catch (error) { return unowned(error?.code === 'ENOENT' ? 'sccache-socket-missing' : 'sccache-socket-unreadable'); }
    if (typeof stats?.isSocket !== 'function' || !stats.isSocket()) return unowned('sccache-socket-not-a-socket');
    if (!Number.isSafeInteger(uid) || uid < 0 || stats.uid !== uid) return unowned('sccache-socket-foreign-uid');
    if ((stats.mode & 0o777) !== 0o600) return unowned('sccache-socket-mode-unsafe');
    let descendants;
    try { descendants = await liveDescendants(brokerPid, { timeoutMs }); }
    catch (error) { return unowned(error?.reason ?? 'sccache-server-descendants-unreadable'); }
    const observed = typeof serverPid === 'function' ? await serverPid() : serverPid;
    const pid = Number.isSafeInteger(observed) && observed > 0 ? observed : findSccacheDescendant(descendants, tool.path);
    if (!Number.isSafeInteger(pid) || pid < 1) return unowned('sccache-server-pid-unknown');
    if (!descendants.has(pid)) return unowned('sccache-server-not-a-broker-descendant');
    let probe;
    try { probe = await run(tool.path, ['--show-stats', '--stats-format=json'], environment, spawn, timeoutMs); }
    catch (error) { return unowned(error?.code === 'RK_SCCACHE_TIMEOUT' ? 'sccache-stats-timeout' : 'sccache-stats-unavailable'); }
    if (probe.code !== 0) return unowned('sccache-stats-unavailable');
    try { JSON.parse(probe.stdout); } catch { return unowned('sccache-stats-malformed'); }
    return Object.freeze({ owned: true, socketPath, serverPid: pid });
  };
}

function findSccacheDescendant(descendants, toolPath) {
  const name = basename(toolPath);
  for (const entry of descendants.values()) if (entry.command === toolPath || basename(entry.command) === name) return entry.pid;
  return null;
}
const unowned = (reason) => Object.freeze({ owned: false, reason });

const degraded = (reason) => Object.freeze({ status: 'DEGRADED', reason: String(reason), fallback: 'DIRECT_RUSTC_AVAILABLE' });

function flattenNumbers(value, prefix = '', output = {}) {
  if (typeof value === 'number' && Number.isFinite(value)) output[prefix] = value;
  else if (value && typeof value === 'object') for (const [key, child] of Object.entries(value)) flattenNumbers(child, prefix ? `${prefix}.${key}` : key, output);
  return output;
}
function run(command, argv, environment, spawn, timeoutMs) {
  return new Promise((resolve, reject) => {
    let stdout = ''; let stderr = ''; let settled = false;
    const child = spawn(command, argv, { env: environment, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    child.stdout?.on('data', (chunk) => { stdout += chunk; }); child.stderr?.on('data', (chunk) => { stderr += chunk; });
    const finish = (value, error) => { if (settled) return; settled = true; clearTimeout(timer); error ? reject(error) : resolve(value); };
    child.once('error', (error) => finish(null, error)); child.once('close', (code) => finish({ code, stdout, stderr }));
    const timer = setTimeout(() => { child.kill?.('SIGKILL'); finish(null, Object.assign(new Error('sccache command timed out'), { code: 'RK_SCCACHE_TIMEOUT' })); }, timeoutMs); timer.unref?.();
  });
}
