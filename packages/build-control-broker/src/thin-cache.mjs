import { execFile as nodeExecFile } from 'node:child_process';
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
  #paths; #tool; #enabled; #clientSide; #maxSize; #timeout; #run; #basedirsEnabled; #basedirs;

  constructor({ paths, sccachePath = null, enabled = true, clientSideSupported = false, maxSizeBytes = 20 * 1024 ** 3, timeoutMs = 2_000, basedirs = null, basedirsEnabled = false, run = defaultRun } = {}) {
    if (!paths?.cargoHome || !paths?.compilerCache) throw new TypeError('thin state cache paths are required');
    if (enabled && (typeof sccachePath !== 'string' || !sccachePath)) throw new TypeError('enabled cache requires sccachePath');
    this.#paths = paths; this.#tool = sccachePath; this.#enabled = enabled; this.#clientSide = clientSideSupported; this.#maxSize = positive(maxSizeBytes, 'maxSizeBytes', 20 * 1024 ** 3); this.#timeout = positive(timeoutMs, 'timeoutMs', 2_000); this.#run = run;
    // SCCACHE_BASEDIRS is gated (v2.7 §12.2): production activation requires
    // the controlled cold/warm/rotated C-vs-D A/B, so even when basedirs are
    // provided they only take effect when basedirsEnabled === true.
    this.#basedirs = Array.isArray(basedirs) ? Object.freeze([...basedirs].filter((value) => typeof value === 'string' && value)) : null;
    this.#basedirsEnabled = basedirsEnabled === true;
  }

  async prepare({ layout, cargoBuildJobs = 4 } = {}) {
    if (!layout?.target || !layout?.temp) throw new TypeError('workspace generation layout is required');
    const base = Object.freeze({
      CARGO_HOME: this.#paths.cargoHome,
      CARGO_TARGET_DIR: layout.target,
      CARGO_BUILD_TARGET_DIR: layout.target,
      CARGO_INSTALL_ROOT: this.#paths.tools,
      CARGO_BUILD_JOBS: String(positive(cargoBuildJobs, 'cargoBuildJobs', 4)),
      TMPDIR: layout.temp,
      TMP: layout.temp,
      TEMP: layout.temp
    });
    if (!this.#enabled) return Object.freeze({ mode: 'direct', status: 'DISABLED', environment: base, before: null });
    const sample = await this.snapshot();
    if (sample.status !== 'OK') return Object.freeze({ mode: 'direct', status: 'DEGRADED', reason: sample.reason, environment: base, before: null });
    const basedirs = this.#basedirsEnabled && this.#basedirs && this.#basedirs.length > 0
      ? Object.freeze({ SCCACHE_BASEDIRS: [...this.#basedirs].join(process.platform === 'win32' ? ';' : ':') })
      : Object.freeze({});
    const environment = Object.freeze({
      ...base,
      RUSTC_WRAPPER: this.#tool,
      SCCACHE_DIR: this.#paths.compilerCache,
      SCCACHE_CACHE_SIZE: String(this.#maxSize),
      ...(this.#basedirsEnabled && this.#basedirs && this.#basedirs.length > 0 ? basedirs : {}),
      ...(this.#clientSide ? { SCCACHE_CLIENT_SIDE: '1' } : {})
    });
    return Object.freeze({ mode: 'sccache', status: 'OK', environment, before: sample.stats });
  }

  async finish(prepared) {
    if (prepared?.mode !== 'sccache') return Object.freeze({ mode: prepared?.mode ?? 'direct', status: prepared?.status ?? 'DISABLED', ...(prepared?.reason ? { reason: prepared.reason } : {}), delta: null });
    const sample = await this.snapshot();
    if (sample.status !== 'OK') return Object.freeze({ mode: 'sccache', status: 'DEGRADED', reason: sample.reason, delta: null });
    return Object.freeze({ mode: 'sccache', status: 'OK', delta: Object.freeze(delta(prepared.before, sample.stats) ?? {}) });
  }

  get basedirsActive() { return Boolean(this.#basedirsEnabled && this.#basedirs && this.#basedirs.length > 0); }

  async snapshot() {
    if (!this.#enabled) return Object.freeze({ status: 'DISABLED', stats: null });
    try {
      const result = await this.#run(this.#tool, ['--show-stats', '--stats-format=json'], {
        env: { ...process.env, SCCACHE_DIR: this.#paths.compilerCache, SCCACHE_CACHE_SIZE: String(this.#maxSize), ...(this.#clientSide ? { SCCACHE_CLIENT_SIDE: '1' } : {}) },
        encoding: 'utf8', timeout: this.#timeout, maxBuffer: MAX_STATS_BYTES
      });
      const parsed = JSON.parse(String(result?.stdout ?? result));
      return Object.freeze({ status: 'OK', stats: Object.freeze(numbers(parsed) ?? {}) });
    } catch (error) {
      return Object.freeze({ status: 'DEGRADED', stats: null, reason: error.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER' ? 'sccache-stats-too-large' : `sccache-stats-unavailable:${error.code ?? error.message}` });
    }
  }
}

export const thinCacheStatsDelta = delta;
