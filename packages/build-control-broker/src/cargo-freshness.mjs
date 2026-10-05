import { lstat, readdir } from 'node:fs/promises';
import { join } from 'node:path';

const NOT_FRESH_CAP = 50;

/**
 * Incremental parser for the full `cargo --message-format=json` stdout stream.
 * It sees every byte (before head/tail truncation) and keeps only counters.
 * NOTE: `build-script-executed` is emitted by Cargo even when the build script
 * output is replayed from a fresh fingerprint, so it is NOT proof of re-execution.
 */
export class CargoFreshnessTracker {
  #buffer = ''; #artifacts = 0; #fresh = 0; #notFresh = 0; #buildScriptEvents = 0; #notFreshIds = []; #notFreshOverflow = 0; #sawJson = false; #parseErrors = 0;

  feed(chunk) {
    this.#buffer += Buffer.isBuffer(chunk) ? chunk.toString('utf8') : String(chunk);
    let newline;
    while ((newline = this.#buffer.indexOf('\n')) >= 0) {
      const line = this.#buffer.slice(0, newline).trim(); this.#buffer = this.#buffer.slice(newline + 1);
      this.#line(line);
    }
    // Bound memory against a pathological unterminated line.
    if (this.#buffer.length > 1024 * 1024) this.#buffer = '';
  }

  #line(line) {
    if (!line.startsWith('{')) return;
    let message;
    try { message = JSON.parse(line); } catch { this.#parseErrors += 1; return; }
    if (typeof message?.reason !== 'string') return;
    this.#sawJson = true;
    if (message.reason === 'compiler-artifact') {
      this.#artifacts += 1;
      if (message.fresh === true) this.#fresh += 1;
      else {
        this.#notFresh += 1;
        if (this.#notFreshIds.length < NOT_FRESH_CAP) this.#notFreshIds.push(String(message.package_id ?? message.target?.name ?? 'unknown'));
        else this.#notFreshOverflow += 1;
      }
    } else if (message.reason === 'build-script-executed') this.#buildScriptEvents += 1;
  }

  summary() {
    this.#line(this.#buffer.trim()); this.#buffer = '';
    if (!this.#sawJson) return Object.freeze({ available: false, reason: 'no-cargo-json-messages' });
    return Object.freeze({
      available: true,
      compilerArtifacts: this.#artifacts, fresh: this.#fresh, notFresh: this.#notFresh,
      // Emitted for fresh (replayed) and re-executed build scripts alike.
      buildScriptExecutedEvents: this.#buildScriptEvents, buildScriptExecutedNote: 'emitted-even-when-output-replayed-from-fresh-fingerprint',
      notFreshPackages: Object.freeze([...this.#notFreshIds]), notFreshPackagesTruncated: this.#notFreshOverflow,
      parseErrors: this.#parseErrors
    });
  }
}

/** True when argv asks Cargo for JSON messages (`--message-format json|json-*` in any spelling). */
export function wantsCargoJson(argv = []) {
  for (let index = 0; index < argv.length; index += 1) {
    const item = argv[index];
    if (item === '--') break;
    const value = item === '--message-format' ? argv[index + 1] : typeof item === 'string' && item.startsWith('--message-format=') ? item.slice('--message-format='.length) : null;
    if (typeof value === 'string' && /(^|,)json/.test(value)) return true;
  }
  return false;
}

/** Shallow (non-recursive) target probe taken before spawn; never blocks on a tree walk. */
export async function probeTargetAtStart(layout, { readdirFn = readdir, state = null } = {}) {
  const path = layout?.target ?? null;
  const base = { path, generation: layout?.generation ?? null, layoutVersion: `generation-${layout?.generation ?? 'unknown'}`, rotationReason: state?.rotationReason ?? null, rotatedFrom: state?.rotatedFrom ?? null, leaf: layout?.leaf ?? null, leafReason: layout?.leafReason ?? null, orphanLeaf: layout?.orphanLeaf ?? null };
  if (!path) return Object.freeze({ ...base, existedAtStart: false, entriesAtStart: null, probeError: 'no-target-path' });
  try {
    const entries = await readdirFn(path);
    return Object.freeze({ ...base, existedAtStart: true, entriesAtStart: entries.length });
  } catch (error) {
    if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') return Object.freeze({ ...base, existedAtStart: false, entriesAtStart: 0 });
    return Object.freeze({ ...base, existedAtStart: null, entriesAtStart: null, probeError: error?.code ?? 'probe-failed' });
  }
}

/** Cargo's output directory name for the profile selected by argv. */
export function cargoProfileDirectory(argv = []) {
  let profile = null; let release = false;
  for (let index = 0; index < argv.length && argv[index] !== '--'; index += 1) {
    const item = argv[index];
    if (item === '--release' || item === '-r') release = true;
    else if (item === '--profile') profile = argv[index + 1] ?? null;
    else if (typeof item === 'string' && item.startsWith('--profile=')) profile = item.slice('--profile='.length);
  }
  if (profile === 'dev' || profile === 'test') return 'debug';
  if (profile === 'bench') return 'release';
  return profile ?? (release ? 'release' : 'debug');
}

/**
 * Format-agnostic freshness for Cargo runs without a JSON message stream (for
 * example the Tauri CLI's own `cargo build`): Cargo rewrites a unit's
 * `.fingerprint/<unit>` files when it rebuilds that unit and leaves fresh units
 * untouched. Start records the clock and the unit count; the census at exit
 * counts units written since start. Untouched units can include units from
 * other feature sets in the same profile directory, so `fresh` is an upper bound.
 */
export async function fingerprintCensusStart(targetPath, argv = [], targetTriple = null, { now = Date.now, readdirFn = readdir } = {}) {
  if (!targetPath) return null;
  const profile = cargoProfileDirectory(argv);
  const directories = [...new Set([targetTriple ? join(targetPath, targetTriple, profile, '.fingerprint') : null, join(targetPath, profile, '.fingerprint')].filter(Boolean))];
  let unitsAtStart = 0;
  for (const directory of directories) unitsAtStart += (await readdirFn(directory).catch(() => [])).length;
  return Object.freeze({ sinceMs: now(), profile, directories: Object.freeze(directories), unitsAtStart });
}

export async function fingerprintCensus(start, { readdirFn = readdir, lstatFn = lstat } = {}) {
  if (!start) return Object.freeze({ available: false, reason: 'fingerprint-census-not-started' });
  let rebuilt = 0; let untouched = 0;
  for (const directory of start.directories) {
    for (const unit of await readdirFn(directory).catch(() => [])) {
      let newest = 0;
      for (const file of await readdirFn(join(directory, unit)).catch(() => [])) {
        const stats = await lstatFn(join(directory, unit, file)).catch(() => null);
        if (stats && stats.mtimeMs > newest) newest = stats.mtimeMs;
      }
      if (newest >= start.sinceMs) rebuilt += 1; else untouched += 1;
    }
  }
  if (rebuilt + untouched === 0) return Object.freeze({ available: false, reason: 'no-fingerprint-units-found', source: 'fingerprint-census', profile: start.profile });
  return Object.freeze({
    available: true, source: 'fingerprint-census', profile: start.profile,
    compilerArtifacts: rebuilt + untouched, fresh: untouched, notFresh: rebuilt, unitsAtStart: start.unitsAtStart,
    freshNote: 'untouched-units-may-include-units-outside-this-build'
  });
}

/** Combine the start probe and the freshness stream into a coldStart verdict. */
export function classifyColdStart(target, freshness) {
  if (!target || target.existedAtStart === null) return Object.freeze({ coldStart: 'unknown', reason: 'target-probe-failed' });
  if (!target.existedAtStart || !(target.entriesAtStart > 0)) return Object.freeze({ coldStart: true, reason: target.existedAtStart ? 'target-empty-at-start' : 'target-missing-at-start' });
  if (!freshness?.available) return Object.freeze({ coldStart: 'unknown', reason: 'target-nonempty-freshness-unavailable' });
  if (freshness.compilerArtifacts === 0) return Object.freeze({ coldStart: 'unknown', reason: 'no-compiler-artifacts-reported' });
  if (freshness.fresh === 0) return Object.freeze({ coldStart: true, reason: 'target-nonempty-but-no-fresh-artifacts' });
  return Object.freeze({ coldStart: false, reason: 'target-nonempty-with-fresh-artifacts' });
}

export function targetConsoleLine({ phase, target, freshness, cold }) {
  if (phase === 'start') {
    const state = target?.existedAtStart === null || target?.existedAtStart === undefined ? 'unknown' : target.existedAtStart && target.entriesAtStart > 0 ? 'warm-candidate' : 'cold';
    return `[rightkit] target ${state} at start (${target?.entriesAtStart ?? '?'} entries, generation ${target?.generation ?? '?'})
`;
  }
  const word = cold?.coldStart === true ? 'cold' : cold?.coldStart === false ? 'warm' : 'unknown';
  const counts = freshness?.available ? `${freshness.notFresh}/${freshness.compilerArtifacts}` : 'unknown (no cargo json stream)';
  return `[rightkit] target ${word} (${cold?.reason ?? 'n/a'}); not-fresh ${counts}
`;
}
