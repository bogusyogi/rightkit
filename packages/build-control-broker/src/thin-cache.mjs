import { execFile as nodeExecFile, spawn as nodeSpawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { rm } from 'node:fs/promises';
import { createConnection } from 'node:net';
import { dirname, join } from 'node:path';
import { promisify } from 'node:util';
import { spawnManagedProcess, startManagedProcess, terminateProcessTree } from './process-tree.mjs';

const defaultRun = promisify(nodeExecFile);
const MAX_STATS_BYTES = 256 * 1024;
const DEFAULT_GATEWAY_PORT = 4227;
const DEFAULT_HEALTH_LEASE_MS = 10_000;
const DEFAULT_RECOVERY_MS = 10_000;
const DEFAULT_STATS_INTERVAL_MS = 60_000;
export const defaultThinCacheTimeoutMs = () => 10_000;

export class ThinCacheUnavailableError extends Error {
  constructor(reason, healthStatus = 'DEGRADED', proof = null) {
    super(`required sccache unavailable: ${reason}`);
    this.name = 'ThinCacheUnavailableError';
    this.code = 'RK_SCCACHE_UNAVAILABLE';
    this.cache = Object.freeze({ mode: 'sccache', status: 'BLOCKED', healthStatus, reason, delta: null, strictNoFallback: true, ...(proof ? { health: proof } : {}) });
  }
}

function positive(value, label, fallback) {
  const selected = value ?? fallback;
  if (!Number.isSafeInteger(selected) || selected < 1) throw new TypeError(`${label} must be a positive integer`);
  return selected;
}

function numbers(value) {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  return Object.fromEntries(Object.entries(value).flatMap(([key, entry]) => {
    const selected = numbers(entry);
    return selected === null ? [] : [[key, selected]];
  }));
}

function delta(before, after) {
  if (typeof before === 'number' && typeof after === 'number') return after - before;
  if (!after || typeof after !== 'object' || Array.isArray(after)) return null;
  const output = {};
  for (const [key, value] of Object.entries(after)) {
    const selected = delta(before?.[key], value);
    if (selected !== null && (!(typeof selected === 'object') || Object.keys(selected).length)) output[key] = selected;
  }
  return output;
}

function childAlive(child) { return Boolean(child && child.exitCode === null && child.signalCode == null); }
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const comparablePath = (value, platform) => {
  const normalized = String(value ?? '').replace(/[\\/]+/g, '/').replace(/\/+$/g, '');
  return platform === 'win32' ? normalized.toLowerCase() : normalized;
};
const localDiskPath = (value) => String(value ?? '').match(/^Local disk:\s*"([^"]+)"$/)?.[1] ?? null;

function defaultConnect(endpoint, timeoutMs) {
  return new Promise((resolve, reject) => {
    const socket = endpoint.kind === 'pipe'
      ? createConnection(endpoint.path)
      : createConnection({ host: '127.0.0.1', port: endpoint.port });
    let settled = false;
    const finish = (error = null) => {
      if (settled) return;
      settled = true; clearTimeout(timer); socket.destroy();
      if (error) reject(error); else resolve();
    };
    const timer = setTimeout(() => finish(Object.assign(new Error('sccache gateway connect timed out'), { code: 'ETIMEDOUT' })), timeoutMs);
    socket.once('connect', () => finish());
    socket.once('error', finish);
  });
}

async function defaultSpawnGateway(tool, environment, { platform, spawn, timeoutMs }) {
  const child = spawnManagedProcess(tool, [], { env: environment, stdio: ['ignore', 'pipe', 'pipe'], platform, killOnDisconnect: true }, spawn);
  try {
    const started = await startManagedProcess(child, { platform, timeoutMs });
    child.gatewayPid = started.pid;
    return child;
  } catch (error) {
    await terminateProcessTree(child, { platform, graceMs: 1_000 }).catch(() => {});
    throw error;
  }
}

export class ThinCache {
  #paths; #tool; #verifyTool; #expectedVersion; #enabled; #clientSide; #maxSize; #timeout; #run; #basedirsEnabled; #basedirs; #platform; #epoch; #now; #port; #leaseMs; #recoveryMs; #statsIntervalMs; #spawn; #spawnGateway; #terminate; #connect; #emit; #onUnavailable;
  #child = null; #health = null; #healthInFlight = null; #recoveryInFlight = null; #timer = null; #lastStats = null; #lastStatsSampledAt = 0; #lastStatsLoggedAt = 0; #stopping = false; #gatewayStderr = '';

  constructor({ paths, sccachePath = null, verifyTool = null, expectedVersion = null, enabled = true, clientSideSupported = false, maxSizeBytes = 20 * 1024 ** 3, timeoutMs = defaultThinCacheTimeoutMs(), basedirs = null, basedirsEnabled = false, platform = process.platform, gatewayPort = DEFAULT_GATEWAY_PORT, healthLeaseMs = DEFAULT_HEALTH_LEASE_MS, recoveryTimeoutMs = DEFAULT_RECOVERY_MS, statsIntervalMs = DEFAULT_STATS_INTERVAL_MS, run = defaultRun, spawn = nodeSpawn, spawnGateway = defaultSpawnGateway, terminate = terminateProcessTree, connect = defaultConnect, emit = async () => {}, onUnavailable = async () => {}, epoch = randomUUID(), now = Date.now } = {}) {
    if (!paths?.cargoHome || !paths?.compilerCache) throw new TypeError('thin state cache paths are required');
    if (enabled && (typeof sccachePath !== 'string' || !sccachePath)) throw new TypeError('enabled cache requires sccachePath');
    if (enabled && typeof verifyTool !== 'function') throw new TypeError('enabled cache requires verifyTool');
    if (enabled && (typeof expectedVersion !== 'string' || !/^\d+\.\d+\.\d+$/.test(expectedVersion))) throw new TypeError('enabled cache requires exact expectedVersion');
    this.#paths = paths; this.#tool = sccachePath; this.#verifyTool = verifyTool; this.#expectedVersion = expectedVersion; this.#enabled = enabled; this.#clientSide = clientSideSupported; this.#maxSize = positive(maxSizeBytes, 'maxSizeBytes', 20 * 1024 ** 3); this.#timeout = positive(timeoutMs, 'timeoutMs', DEFAULT_RECOVERY_MS); this.#run = run;
    this.#basedirs = Array.isArray(basedirs) ? Object.freeze([...basedirs].filter((value) => typeof value === 'string' && value)) : null;
    this.#basedirsEnabled = basedirsEnabled === true;
    this.#platform = platform; this.#epoch = String(epoch); this.#now = now; this.#port = positive(gatewayPort, 'gatewayPort', DEFAULT_GATEWAY_PORT); this.#leaseMs = positive(healthLeaseMs, 'healthLeaseMs', DEFAULT_HEALTH_LEASE_MS); this.#recoveryMs = positive(recoveryTimeoutMs, 'recoveryTimeoutMs', DEFAULT_RECOVERY_MS); this.#statsIntervalMs = positive(statsIntervalMs, 'statsIntervalMs', DEFAULT_STATS_INTERVAL_MS); this.#spawn = spawn; this.#spawnGateway = spawnGateway; this.#terminate = terminate; this.#connect = connect; this.#emit = emit; this.#onUnavailable = onUnavailable;
  }

  get #endpoint() {
    if (this.#platform === 'win32') return Object.freeze({ kind: 'tcp', host: '127.0.0.1', port: this.#port, display: `127.0.0.1:${this.#port}` });
    const temporary = this.#paths.temporary ?? join(dirname(this.#paths.compilerCache), 'tmp');
    const path = join(temporary, 'sccache-gateway.sock');
    return Object.freeze({ kind: 'pipe', path, display: path });
  }

  #endpointEnvironment() { return this.#endpoint.kind === 'pipe' ? { SCCACHE_SERVER_UDS: this.#endpoint.path } : { SCCACHE_SERVER_PORT: String(this.#endpoint.port) }; }
  #baseEnvironment() { return { SCCACHE_DIR: this.#paths.compilerCache, SCCACHE_CACHE_SIZE: String(this.#maxSize), SCCACHE_LOCAL_RW_MODE: 'READ_WRITE', ...this.#endpointEnvironment() }; }
  #clientEnvironment() { return { ...this.#baseEnvironment(), SCCACHE_START_SERVER: '0', SCCACHE_NO_DAEMON: '1', SCCACHE_IDLE_TIMEOUT: '0', SCCACHE_IGNORE_SERVER_IO_ERROR: '0', ...(this.#clientSide ? { SCCACHE_CLIENT_SIDE: '1' } : {}) }; }

  async start() {
    if (!this.#enabled) return;
    if (childAlive(this.#child) && this.#health) return;
    await this.#recover('broker-start');
    if (!this.#timer) {
      this.#timer = setInterval(() => { void this.#heartbeat(); }, Math.max(1_000, Math.floor(this.#leaseMs / 2)));
      this.#timer.unref?.();
    }
  }

  async close() {
    this.#stopping = true;
    if (this.#timer) clearInterval(this.#timer);
    this.#timer = null;
    await Promise.allSettled([this.#healthInFlight, this.#recoveryInFlight].filter(Boolean));
    const child = this.#child; this.#child = null; this.#health = null;
    if (child) await this.#terminate(child, { platform: this.#platform, graceMs: 2_000 }).catch(() => {});
    if (this.#endpoint.kind === 'pipe') await rm(this.#endpoint.path, { force: true }).catch(() => {});
    await this.#event({ type: 'CACHE_GATEWAY_STOPPED', epoch: this.#epoch, endpoint: this.#endpoint.display });
  }

  async prepare({ layout, cargoBuildJobs = 4, repositoryRoot = null, workspaceRoot = null, jobId = null } = {}) {
    if (!layout?.target || !layout?.temp) throw new TypeError('workspace generation layout is required');
    const base = Object.freeze({
      CARGO_HOME: this.#paths.cargoHome,
      CARGO_BUILD_JOBS: String(positive(cargoBuildJobs, 'cargoBuildJobs', 4)),
      TMPDIR: layout.temp, TMP: layout.temp, TEMP: layout.temp
    });
    if (!this.#enabled) throw new ThinCacheUnavailableError('sccache-disabled', 'DISABLED');
    const health = await this.#admit();
    const trustedRoots = [repositoryRoot, workspaceRoot, layout.root, ...(this.#basedirs ?? [])].filter((value) => typeof value === 'string' && value);
    const seen = new Set();
    const basedirs = this.#basedirsEnabled
      ? Object.freeze({ SCCACHE_BASEDIRS: trustedRoots.filter((value) => { const key = this.#platform === 'win32' ? value.toLowerCase() : value; if (seen.has(key)) return false; seen.add(key); return true; }).join(this.#platform === 'win32' ? ';' : ':') })
      : Object.freeze({});
    const environment = Object.freeze({ ...base, RUSTC_WRAPPER: this.#tool, ...this.#clientEnvironment(), ...(this.#basedirsEnabled ? basedirs : {}) });
    const admission = Object.freeze({ ...health, admittedAt: new Date(this.#now()).toISOString(), strictNoFallback: true });
    await this.#event({ type: 'CACHE_ADMISSION', id: jobId, status: 'SERVICE_OK', health: admission });
    return Object.freeze({ mode: 'sccache', status: 'SERVICE_OK', environment, before: this.#lastStats?.stats ?? null, attribution: this.#attribution(), health: admission, strictNoFallback: true });
  }

  async finish(prepared) {
    if (prepared?.mode !== 'sccache') return Object.freeze({ mode: prepared?.mode ?? 'direct', status: prepared?.status ?? 'DISABLED', ...(prepared?.reason ? { reason: prepared.reason } : {}), delta: null });
    if (this.#clientSide) return Object.freeze({ mode: 'sccache', status: 'SERVICE_OK', reason: 'client-side-counters-unattributable', delta: null, health: prepared.health ?? null, strictNoFallback: true, attribution: Object.freeze({ ...prepared.attribution, windowStartedAt: prepared.attribution?.sampledAt ?? null, windowEndedAt: new Date(this.#now()).toISOString() }) });
    const sample = await this.snapshot();
    if (sample.status !== 'SERVICE_OK') return Object.freeze({ mode: 'sccache', status: 'DEGRADED', reason: sample.reason, delta: null, health: prepared.health ?? null, strictNoFallback: true, attribution: sample.attribution ?? prepared.attribution ?? null });
    const measured = delta(prepared.before, sample.stats) ?? {};
    if (containsNegative(measured)) return Object.freeze({ mode: 'sccache', status: 'DEGRADED', reason: 'sccache-counter-reset', delta: null, health: prepared.health ?? null, strictNoFallback: true, attribution: Object.freeze({ ...sample.attribution, windowStartedAt: prepared.attribution?.sampledAt ?? null, windowEndedAt: sample.attribution.sampledAt }) });
    return Object.freeze({ mode: 'sccache', status: 'SERVICE_OK', delta: Object.freeze(measured), health: prepared.health ?? null, strictNoFallback: true, attribution: Object.freeze({ ...sample.attribution, windowStartedAt: prepared.attribution?.sampledAt ?? null, windowEndedAt: sample.attribution.sampledAt }) });
  }

  get basedirsActive() { return this.#basedirsEnabled; }

  async snapshot({ timeoutMs = this.#timeout } = {}) {
    if (!this.#enabled) return Object.freeze({ status: 'DISABLED', stats: null });
    if (!childAlive(this.#child)) return Object.freeze({ status: 'DEGRADED', stats: null, reason: 'sccache-gateway-not-owned' });
    try {
      const result = await this.#run(this.#tool, ['--show-stats', '--stats-format=json'], { env: { ...process.env, ...this.#clientEnvironment() }, encoding: 'utf8', timeout: Math.max(1, Math.min(this.#timeout, timeoutMs)), maxBuffer: MAX_STATS_BYTES });
      if (!childAlive(this.#child)) throw Object.assign(new Error('sccache gateway exited during stats collection'), { code: 'RK_SCCACHE_GATEWAY_EXITED' });
      const parsed = JSON.parse(String(result?.stdout ?? result));
      const expected = comparablePath(this.#paths.compilerCache, this.#platform);
      const location = comparablePath(localDiskPath(parsed.cache_location), this.#platform);
      if (!location || location !== expected) throw Object.assign(new Error(`sccache storage path mismatch: ${parsed.cache_location ?? 'missing'}`), { code: 'RK_SCCACHE_STORAGE_MISMATCH' });
      if (parsed.max_cache_size !== this.#maxSize) throw Object.assign(new Error(`sccache storage size mismatch: ${parsed.max_cache_size ?? 'missing'}`), { code: 'RK_SCCACHE_STORAGE_MISMATCH' });
      if (parsed.version !== this.#expectedVersion) throw Object.assign(new Error(`sccache storage version mismatch: ${parsed.version ?? 'missing'}`), { code: 'RK_SCCACHE_STORAGE_MISMATCH' });
      const storage = Object.freeze({ location: parsed.cache_location, cacheSizeBytes: parsed.cache_size ?? null, maxSizeBytes: parsed.max_cache_size, version: parsed.version, configuredRwMode: 'READ_WRITE' });
      const sample = Object.freeze({ status: 'SERVICE_OK', stats: Object.freeze(numbers(parsed.stats ?? {}) ?? {}), storage, attribution: this.#attribution() });
      this.#lastStats = sample; this.#lastStatsSampledAt = this.#now();
      return sample;
    } catch (error) {
      return Object.freeze({ status: 'DEGRADED', stats: null, reason: cacheFailureReason(error), attribution: this.#attribution() });
    }
  }

  #attribution() {
    return Object.freeze({ scope: this.#clientSide ? 'client-side-counters-unattributable' : 'shared-cumulative-root-window', epoch: this.#epoch, serverIdentity: this.#child?.gatewayPid ?? this.#child?.pid ?? null, mode: this.#clientSide ? 'client-side' : 'server', sampledAt: new Date(this.#now()).toISOString() });
  }

  async #admit() {
    const age = this.#health ? Math.max(0, this.#now() - this.#health.sampledAtMs) : Infinity;
    if (childAlive(this.#child) && age <= this.#leaseMs) return this.#proof(age);
    try {
      await this.#ensureHealthy();
      return this.#proof(Math.max(0, this.#now() - this.#health.sampledAtMs));
    } catch (error) {
      throw new ThinCacheUnavailableError(error.message, 'DEGRADED', this.#health ? this.#proof(Math.max(0, this.#now() - this.#health.sampledAtMs)) : null);
    }
  }

  #proof(ageMs) { return Object.freeze({ epoch: this.#epoch, gatewayPid: this.#child?.gatewayPid ?? this.#child?.pid ?? null, ownerPid: this.#child?.pid ?? null, endpoint: this.#endpoint.display, sampledAt: this.#health?.sampledAt ?? null, ageMs, leaseMs: this.#leaseMs, ownership: 'broker', storageSampledAt: this.#health?.storageSampledAt ?? null, storage: this.#health?.storage ?? null }); }

  async #ensureHealthy() {
    if (this.#healthInFlight) return this.#healthInFlight;
    this.#healthInFlight = (async () => {
      if (!childAlive(this.#child)) return this.#recover('gateway-not-running');
      try {
        await this.#connect(this.#endpoint, Math.min(1_000, this.#recoveryMs));
        const sample = !this.#lastStats || this.#now() - this.#lastStatsSampledAt >= this.#statsIntervalMs
          ? await this.#probeStorage(Math.min(this.#timeout, this.#recoveryMs))
          : this.#lastStats;
        return this.#markHealthy(sample.storage, sample.attribution?.sampledAt ?? null);
      } catch (error) {
        return this.#recover(`health-probe:${error.code ?? error.message}`);
      }
    })().finally(() => { this.#healthInFlight = null; });
    return this.#healthInFlight;
  }

  async #recover(reason) {
    if (this.#recoveryInFlight) return this.#recoveryInFlight;
    this.#recoveryInFlight = (async () => {
      const deadline = this.#now() + this.#recoveryMs;
      await this.#event({ type: 'CACHE_RECOVERY_STARTED', reason, epoch: this.#epoch, endpoint: this.#endpoint.display });
      if (reason !== 'broker-start') await Promise.resolve().then(() => this.#onUnavailable(Object.freeze({ reason: `sccache-gateway-recovery:${reason}`, epoch: this.#epoch, gatewayPid: this.#child?.gatewayPid ?? this.#child?.pid ?? null, ownerPid: this.#child?.pid ?? null }))).catch(() => {});
      if (this.#child) await this.#terminate(this.#child, { platform: this.#platform, graceMs: Math.min(2_000, this.#recoveryMs) }).catch(() => {});
      this.#child = null; this.#health = null; this.#gatewayStderr = '';
      if (this.#endpoint.kind === 'pipe') await rm(this.#endpoint.path, { force: true }).catch(() => {});
      const inherited = { ...process.env };
      delete inherited.SCCACHE_CLIENT_SIDE; delete inherited.SCCACHE_ERROR_LOG;
      const environment = { ...inherited, ...this.#baseEnvironment(), SCCACHE_START_SERVER: '1', SCCACHE_NO_DAEMON: '1', SCCACHE_IDLE_TIMEOUT: '0' };
      try {
        const verifiedTool = await this.#verifyTool();
        if (verifiedTool?.path !== this.#tool) throw Object.assign(new Error('sccache registry verification returned a different executable'), { code: 'TOOL_CHANGED' });
        const child = await this.#spawnGateway(this.#tool, environment, { platform: this.#platform, spawn: this.#spawn, timeoutMs: Math.max(1, deadline - this.#now()) });
        this.#child = child; this.#epoch = randomUUID();
        child.stdout?.resume?.();
        child.stderr?.on?.('data', (chunk) => { this.#gatewayStderr = `${this.#gatewayStderr}${chunk}`.slice(-4_096); });
        child.once?.('close', (code, signal) => {
          if (this.#child === child) {
            this.#health = null;
            void Promise.resolve(this.#onUnavailable(Object.freeze({ reason: 'sccache-gateway-exited', epoch: this.#epoch, gatewayPid: child.gatewayPid ?? child.pid, ownerPid: child.pid, code, signal }))).catch(() => {});
            void this.#event({ type: 'CACHE_GATEWAY_EXITED', epoch: this.#epoch, gatewayPid: child.gatewayPid ?? child.pid, ownerPid: child.pid, code, signal, stderr: this.#gatewayStderr });
          }
        });
        let lastError = null;
        while (this.#now() < deadline) {
          if (!childAlive(child)) throw new Error(`sccache-gateway-exited:${child.exitCode ?? child.signalCode ?? 'unknown'}`);
          try {
            await this.#connect(this.#endpoint, Math.min(500, Math.max(1, deadline - this.#now())));
            const sample = await this.#probeStorage(Math.max(1, deadline - this.#now()));
            this.#markHealthy(sample.storage, sample.attribution?.sampledAt ?? null);
            await this.#event({ type: 'CACHE_GATEWAY_READY', epoch: this.#epoch, gatewayPid: child.gatewayPid ?? child.pid, ownerPid: child.pid, endpoint: this.#endpoint.display, storage: sample.storage, strictNoFallback: true });
            return;
          } catch (error) {
            lastError = error;
            await delay(Math.min(25, Math.max(1, deadline - this.#now())));
          }
        }
        throw Object.assign(new Error(`sccache-gateway-recovery-timeout:${lastError?.code ?? lastError?.message ?? 'unknown'}`), { code: 'ETIMEDOUT' });
      } catch (error) {
        const failed = this.#child; this.#child = null; this.#health = null;
        if (failed) await this.#terminate(failed, { platform: this.#platform, graceMs: 1_000 }).catch(() => {});
        if (error?.event?.type === 'TOOL_CHANGED') await this.#event(error.event);
        await this.#event({ type: 'CACHE_RECOVERY_FAILED', code: error.code ?? null, reason: error.message, endpoint: this.#endpoint.display, stderr: this.#gatewayStderr });
        const diagnostic = this.#gatewayStderr.replace(/\s+/g, ' ').trim().slice(0, 1_024);
        // Preserve typed spawn failures in recovery receipts. Callers need the
        // stable code (for example ENOENT) to distinguish missing tool/runtime
        // from a generic gateway outage; the human message remains diagnostic.
        const cause = error?.code ?? error?.message ?? 'unknown';
        const detail = error?.code && error?.message && error.message !== error.code ? `:${error.message}` : '';
        throw new Error(`sccache-gateway-unavailable:${cause}${detail}${diagnostic ? `:${diagnostic}` : ''}`);
      }
    })().finally(() => { this.#recoveryInFlight = null; });
    return this.#recoveryInFlight;
  }

  #markHealthy(storage, storageSampledAt) {
    const sampledAtMs = this.#now();
    this.#health = Object.freeze({ sampledAtMs, sampledAt: new Date(sampledAtMs).toISOString(), storage: storage ?? null, storageSampledAt });
  }

  async #probeStorage(timeoutMs) {
    if (!childAlive(this.#child)) throw Object.assign(new Error('sccache gateway is not owned'), { code: 'RK_SCCACHE_GATEWAY_EXITED' });
    await this.#connect(this.#endpoint, Math.min(1_000, timeoutMs));
    const sample = await this.snapshot({ timeoutMs });
    if (sample.status !== 'SERVICE_OK') throw Object.assign(new Error(sample.reason), { code: 'RK_SCCACHE_STORAGE_UNAVAILABLE' });
    if (!childAlive(this.#child)) throw Object.assign(new Error('sccache gateway exited after storage RPC'), { code: 'RK_SCCACHE_GATEWAY_EXITED' });
    await this.#connect(this.#endpoint, Math.min(1_000, timeoutMs));
    return sample;
  }

  async #heartbeat() {
    if (this.#stopping || !this.#enabled) return;
    try {
      await this.#ensureHealthy();
      if (this.#lastStats && this.#lastStatsSampledAt > this.#lastStatsLoggedAt) {
        const sample = this.#lastStats; this.#lastStatsLoggedAt = this.#lastStatsSampledAt;
        await this.#event({ type: 'CACHE_STATS_SNAPSHOT', status: sample.status, reason: sample.reason ?? null, storage: sample.storage ?? null, attribution: sample.attribution ?? null, stats: sample.stats ?? null });
      }
    } catch (error) {
      await this.#event({ type: 'CACHE_HEALTH_BLOCKED', reason: error.message, endpoint: this.#endpoint.display });
    }
  }

  #event(event) { return Promise.resolve().then(() => this.#emit(Object.freeze({ ...event, at: new Date(this.#now()).toISOString() }))).catch(() => null); }
}

export const thinCacheStatsDelta = delta;

function containsNegative(value) {
  if (typeof value === 'number') return value < 0;
  return Boolean(value && typeof value === 'object' && Object.values(value).some(containsNegative));
}

function cacheFailureReason(error) {
  if (error?.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER') return 'sccache-stats-too-large';
  const diagnostic = String(error?.stderr ?? '').replace(/\s+/g, ' ').trim().slice(0, 1_024);
  const cause = error?.code ?? error?.signal ?? error?.message ?? 'unknown';
  const detail = error?.message && error.message !== cause ? String(error.message).replace(/\s+/g, ' ').trim().slice(0, 1_024) : '';
  return `sccache-stats-unavailable:${cause}${detail ? `:${detail}` : ''}${diagnostic ? `:${diagnostic}` : ''}`;
}
