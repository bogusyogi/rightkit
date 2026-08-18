import { spawn as nodeSpawn } from 'node:child_process';
import { lstat as nodeLstat, unlink as nodeUnlink } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { enumerateLiveDescendants } from './process-tree.mjs';

/** Coordinates bounded sccache probes. Daemon process ownership is platform-specific. */
export class SccacheSupervisor {
  #tool; #spawn; #timeoutMs; #started = null; #mode; #ownershipProof;
  constructor({ tool, spawn = nodeSpawn, timeoutMs = 10_000, mode = 'disabled', ownershipProof = null }) {
    if (!tool?.path || !Number.isSafeInteger(timeoutMs) || timeoutMs < 10) throw new TypeError('sccache tool & bounded timeout are required');
    if (!['disabled', 'managed'].includes(mode)) throw new TypeError('sccache mode must be disabled or managed');
    if (mode === 'managed' && typeof ownershipProof !== 'function') throw new TypeError('managed sccache requires broker ownership proof');
    this.#tool = tool; this.#spawn = spawn; this.#timeoutMs = timeoutMs; this.#mode = mode; this.#ownershipProof = ownershipProof;
  }
  ensureStarted(environment) {
    if (this.#mode === 'disabled') return Promise.reject(new SccacheOwnershipError('RK_SCCACHE_DISABLED', 'sccache supervision is disabled'));
    if (!this.#started) this.#started = run(this.#tool.path, ['--start-server'], environment, this.#spawn, this.#timeoutMs).then(({ code, stderr }) => {
      if (code !== 0) throw new Error(`managed sccache server failed to start: ${stderr.trim() || code}`);
      return Object.freeze({ state: 'RUNNING' });
    }).catch((error) => { this.#started = null; throw error; });
    return this.#started;
  }
  async sample(environment) {
    try {
      const proof = await this.#ownershipProof?.({ environment: Object.freeze({ ...environment }), tool: this.#tool });
      if (this.#mode === 'managed' && proof !== true && proof?.owned !== true) throw new SccacheOwnershipError('RK_SCCACHE_OWNERSHIP_UNPROVEN', proof?.reason || 'broker cannot prove sccache server child ownership');
      await this.ensureStarted(environment);
      const result = await run(this.#tool.path, ['--show-stats', '--stats-format=json'], environment, this.#spawn, this.#timeoutMs);
      if (result.code !== 0) throw new Error(result.stderr.trim() || `stats exited ${result.code}`);
      return Object.freeze({ status: 'OK', stats: Object.freeze(JSON.parse(result.stdout)) });
    } catch (error) { this.#started = null; return degraded(error.code || (error.message === 'sccache supervision is disabled' ? 'RK_SCCACHE_DISABLED' : error.message)); }
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

/** The broker owns this path; nothing below it is client-supplied. */
export function brokerSccacheSocketPath(externalRoot) {
  if (typeof externalRoot !== 'string' || !externalRoot) throw new TypeError('broker external root is required');
  return join(externalRoot, SCCACHE_SOCKET_NAME);
}

/**
 * Any socket already present at broker start predates this broker process — a
 * legacy-v1 leftover is exactly that — so it is never broker-owned. Remove it
 * before a fresh, provable socket is created.
 */
export async function prepareBrokerSccacheSocket({ externalRoot, socketPath = brokerSccacheSocketPath(externalRoot), unlink = nodeUnlink } = {}) {
  if (typeof socketPath !== 'string' || !socketPath) throw new TypeError('broker sccache socket path is required');
  try { await unlink(socketPath); } catch (error) { if (error?.code !== 'ENOENT') throw error; }
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
