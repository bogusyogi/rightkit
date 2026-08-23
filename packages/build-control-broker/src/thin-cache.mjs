import { execFile as nodeExecFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { promisify } from 'node:util';

const defaultRun = promisify(nodeExecFile);
const MAX_STATS_BYTES = 256 * 1024;
// Generation-scoped target paths (workspaces/<key>/generations/000012/target)
// and repo roots participate in sccache cache identity. Without stable base
// dirs those paths appear as absolute input paths, so rotated generations
// cannot reuse cache entries. SCCACHE_BASEDIRS remains behind a configuration
// gate (v2.7 §12): permanent activation requires the controlled C-vs-D A/B.
const basedirKeys = new Set(['compilerCache']);
const platformBasedirKey = process.platform === 'win32' ? 'compilerCacheWin' : 'compilerCachePosix';
export const defaultThinCacheTimeoutMs = (platform = process.platform) => platform === 'win32' ? 5_000 : 2_000;

export class ThinCacheUnavailableError extends Error {
  constructor(reason, healthStatus = 'DEGRADED') {
    super(`required sccache unavailable: ${reason}`);
    this.name = 'ThinCacheUnavailableError';
    this.code = 'RK_SCCACHE_UNAVAILABLE';
    this.cache = Object.freeze({ mode: 'sccache', status: 'BLOCKED', healthStatus, reason, delta: null });
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

export class ThinCache {
  #paths; #tool; #enabled; #clientSide; #maxSize; #timeout; #run; #basedirsEnabled; #basedirs; #platform; #epoch; #now;

  constructor({ paths, sccachePath = null, enabled = true, clientSideSupported = false, maxSizeBytes = 20 * 1024 ** 3, timeoutMs = defaultThinCacheTimeoutMs(), basedirs = null, basedirsEnabled = false, platform = process.platform, run = defaultRun, epoch = randomUUID(), now = Date.now } = {}) {
    if (!paths?.cargoHome || !paths?.compilerCache) throw new TypeError('thin state cache paths are required');
    if (enabled && (typeof sccachePath !== 'string' || !sccachePath)) throw new TypeError('enabled cache requires sccachePath');
    this.#paths = paths; this.#tool = sccachePath; this.#enabled = enabled; this.#clientSide = clientSideSupported; this.#maxSize = positive(maxSizeBytes, 'maxSizeBytes', 20 * 1024 ** 3); this.#timeout = positive(timeoutMs, 'timeoutMs', 2_000); this.#run = run;
    // SCCACHE_BASEDIRS is gated (v2.7 §12.2): production activation requires
    // the controlled cold/warm/rotated C-vs-D A/B, so even when basedirs are
    // provided they only take effect when basedirsEnabled === true.
    this.#basedirs = Array.isArray(basedirs) ? Object.freeze([...basedirs].filter((value) => typeof value === 'string' && value)) : null;
    this.#basedirsEnabled = basedirsEnabled === true;
    this.#platform = platform;
    this.#epoch = String(epoch);
    this.#now = now;
  }

  async prepare({ layout, cargoBuildJobs = 4, repositoryRoot = null, workspaceRoot = null } = {}) {
    if (!layout?.target || !layout?.temp) throw new TypeError('workspace generation layout is required');
    const base = Object.freeze({
      CARGO_HOME: this.#paths.cargoHome,
      CARGO_TARGET_DIR: layout.target,
      CARGO_BUILD_TARGET_DIR: layout.target,
      // No CARGO_INSTALL_ROOT: generic `cargo install` is denied (v2.8 §11) and
      // must never write into the digest-registered managed tools namespace.
      CARGO_BUILD_JOBS: String(positive(cargoBuildJobs, 'cargoBuildJobs', 4)),
      TMPDIR: layout.temp,
      TMP: layout.temp,
      TEMP: layout.temp
    });
    if (!this.#enabled) throw new ThinCacheUnavailableError('sccache-disabled', 'DISABLED');
    const sample = await this.snapshot();
    if (sample.status !== 'SERVICE_OK') throw new ThinCacheUnavailableError(sample.reason);
    const trustedRoots = [repositoryRoot, workspaceRoot, layout.root, ...(this.#basedirs ?? [])].filter((value) => typeof value === 'string' && value);
    const seen = new Set();
    const basedirs = this.#basedirsEnabled
      ? Object.freeze({ SCCACHE_BASEDIRS: trustedRoots.filter((value) => { const key = this.#platform === 'win32' ? value.toLowerCase() : value; if (seen.has(key)) return false; seen.add(key); return true; }).join(this.#platform === 'win32' ? ';' : ':') })
      : Object.freeze({});
    const environment = Object.freeze({
      ...base,
      RUSTC_WRAPPER: this.#tool,
      SCCACHE_DIR: this.#paths.compilerCache,
      SCCACHE_CACHE_SIZE: String(this.#maxSize),
      ...(this.#basedirsEnabled ? basedirs : {}),
      ...(this.#clientSide ? { SCCACHE_CLIENT_SIDE: '1' } : {})
    });
    return Object.freeze({ mode: 'sccache', status: 'SERVICE_OK', environment, before: sample.stats, attribution: sample.attribution });
  }

  async finish(prepared) {
    if (prepared?.mode !== 'sccache') return Object.freeze({ mode: prepared?.mode ?? 'direct', status: prepared?.status ?? 'DISABLED', ...(prepared?.reason ? { reason: prepared.reason } : {}), delta: null });
    if (this.#clientSide) return Object.freeze({ mode: 'sccache', status: 'SERVICE_OK', reason: 'client-side-counters-unattributable', delta: null, attribution: Object.freeze({ ...prepared.attribution, windowStartedAt: prepared.attribution?.sampledAt ?? null, windowEndedAt: new Date(this.#now()).toISOString() }) });
    const sample = await this.snapshot();
    if (sample.status !== 'SERVICE_OK') return Object.freeze({ mode: 'sccache', status: 'DEGRADED', reason: sample.reason, delta: null, attribution: sample.attribution ?? prepared.attribution ?? null });
    const measured = delta(prepared.before, sample.stats) ?? {};
    if (containsNegative(measured)) return Object.freeze({ mode: 'sccache', status: 'DEGRADED', reason: 'sccache-counter-reset', delta: null, attribution: Object.freeze({ ...sample.attribution, windowStartedAt: prepared.attribution?.sampledAt ?? null, windowEndedAt: sample.attribution.sampledAt }) });
    return Object.freeze({ mode: 'sccache', status: 'SERVICE_OK', delta: Object.freeze(measured), attribution: Object.freeze({ ...sample.attribution, windowStartedAt: prepared.attribution?.sampledAt ?? null, windowEndedAt: sample.attribution.sampledAt }) });
  }

  get basedirsActive() { return this.#basedirsEnabled; }

  async snapshot() {
    if (!this.#enabled) return Object.freeze({ status: 'DISABLED', stats: null });
    try {
      const result = await this.#run(this.#tool, ['--show-stats', '--stats-format=json'], {
        env: { ...process.env, SCCACHE_DIR: this.#paths.compilerCache, SCCACHE_CACHE_SIZE: String(this.#maxSize), ...(this.#clientSide ? { SCCACHE_CLIENT_SIDE: '1' } : {}) },
        encoding: 'utf8', timeout: this.#timeout, maxBuffer: MAX_STATS_BYTES
      });
      const parsed = JSON.parse(String(result?.stdout ?? result));
      return Object.freeze({ status: 'SERVICE_OK', stats: Object.freeze(numbers(parsed.stats ?? parsed) ?? {}), attribution: Object.freeze({ scope: this.#clientSide ? 'client-side-counters-unattributable' : 'shared-cumulative-root-window', epoch: this.#epoch, serverIdentity: null, mode: this.#clientSide ? 'client-side' : 'server', sampledAt: new Date(this.#now()).toISOString() }) });
    } catch (error) {
      return Object.freeze({ status: 'DEGRADED', stats: null, reason: cacheFailureReason(error) });
    }
  }
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
  return `sccache-stats-unavailable:${cause}${diagnostic ? `:${diagnostic}` : ''}`;
}
