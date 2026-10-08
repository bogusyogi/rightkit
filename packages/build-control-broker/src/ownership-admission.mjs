import { spawn as nodeSpawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/** Protected ownership evidence: bounded worker scans & digest-bound verdict reuse. */
export const OWNERSHIP_MODES = Object.freeze(['off', 'report', 'enforce']);
const MAX_OUTPUT_BYTES = 32 * 1024 * 1024;
const here = dirname(fileURLToPath(import.meta.url));

/** Staged runtime layout first, then source checkout. */
export function defaultScannerRoot() {
  for (const candidate of [join(here, '..', '..', 'ownership-scanner'), join(here, '..', '..', 'release')]) {
    if (existsSync(join(candidate, 'ownership-scan-worker.mjs')) && existsSync(join(candidate, 'rightkit-ownership.json'))) return candidate;
  }
  return null;
}
const shared = defaultScannerRoot();
const { ownershipVerdict, ownershipCacheKey } = shared
  ? await import(pathToFileURL(join(shared, 'ownership-verdict.mjs')).href)
  : { ownershipVerdict: () => ({ admit: false, code: 'RK_OWNERSHIP_UNVERIFIED', problems: ['shared ownership verdict is unavailable'] }), ownershipCacheKey: () => { throw new Error('shared ownership verdict is unavailable'); } };

/** Always scan repository coverage; owner authority comes from protected registry. */
export function ownershipScanRoot({ repositoryRoot, workspaceRoot }) {
  const repository = repositoryRoot ?? workspaceRoot;
  return typeof repository === 'string' && repository.length ? { root: resolve(repository) } : { error: 'repository root is missing or malformed' };
}

export function runScanWorker({ scannerRoot, scanRoot, timeoutMs, inputsOnly = false, spawn = nodeSpawn, execPath = process.execPath }) {
  return new Promise((resolveScan) => {
    let child, timer;
    try {
      child = spawn(execPath, [join(scannerRoot, 'ownership-scan-worker.mjs'), scanRoot, join(scannerRoot, 'rightkit-ownership.json'), ...(inputsOnly ? ['--inputs'] : [])], { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    } catch (error) { resolveScan({ error: 'scanner failed to start: ' + error.message }); return; }
    const out = []; const err = []; let bytes = 0; let settled = false;
    const settleScan = (value) => { if (settled) return; settled = true; clearTimeout(timer); resolveScan(value); };
    const collect = (sink) => (chunk) => {
      bytes += chunk.length;
      if (bytes > MAX_OUTPUT_BYTES) { child.kill(); settleScan({ error: 'scanner output exceeded ' + MAX_OUTPUT_BYTES + ' bytes' }); return; }
      sink.push(chunk);
    };
    child.stdout.on('data', collect(out)); child.stderr.on('data', collect(err));
    child.once('error', (error) => settleScan({ error: 'scanner failed to start: ' + error.message }));
    child.once('close', (code) => {
      if (code !== 0) return settleScan({ error: Buffer.concat(err).toString('utf8').trim().slice(0, 2000) || 'scanner exited ' + code });
      try { settleScan(JSON.parse(Buffer.concat(out).toString('utf8'))); }
      catch (error) { settleScan({ error: 'scanner returned invalid JSON: ' + error.message }); }
    });
    timer = setTimeout(() => { child.kill(); settleScan({ error: 'scan exceeded ' + timeoutMs + ' ms' }); }, timeoutMs);
    timer.unref?.();
  });
}

const outcomes = Object.freeze({
  RK_OWNERSHIP_CLEAN: 'clean', RK_OWNERSHIP_VIOLATION: 'violations', RK_OWNERSHIP_PENDING_ENROLLMENT: 'pending-enrollment',
  RK_OWNERSHIP_MALFORMED_REPORT: 'malformed-report', RK_OWNERSHIP_INCOMPLETE_COVERAGE: 'incomplete-coverage',
  RK_OWNERSHIP_INVALID_REGISTRATION: 'invalid-registration', RK_OWNERSHIP_POLICY_DOWNGRADE: 'policy-downgrade',
  RK_OWNERSHIP_REPORT_FAILED: 'report-failed', RK_OWNERSHIP_UNVERIFIED: 'problems',
});
/** Classification delegates every acceptance decision to shared verdict contract. */
export function classifyOwnershipReport(report, trust) {
  const verdict = ownershipVerdict(report, trust);
  const violations = Array.isArray(report?.violations) ? report.violations.filter((v) => v && typeof v === 'object').map((v) => ({ capability: v.capability ?? null, owner: v.package ?? null, fingerprint: v.fingerprint ?? null, file: v.file ?? null, line: v.line ?? null })) : [];
  return { ...verdict, outcome: outcomes[verdict.code] ?? 'unscanned', violations };
}

export class OwnershipAdmission {
  #mode; #timeoutMs; #scannerRoot; #now; #run; #inputs; #cache = new Map(); #inflight = new Map();

  constructor({ mode = 'report', timeoutMs = 30_000, scannerRoot = defaultScannerRoot(), now = Date.now, runScan = runScanWorker, runInputs = runScanWorker } = {}) {
    if (!OWNERSHIP_MODES.includes(mode)) throw new TypeError('ownership mode must be one of ' + OWNERSHIP_MODES.join(', '));
    this.#mode = mode; this.#timeoutMs = timeoutMs; this.#scannerRoot = scannerRoot; this.#now = now; this.#run = runScan; this.#inputs = runInputs;
  }

  get mode() { return this.#mode; }

  #failure(root, error, code = 'RK_OWNERSHIP_UNVERIFIED') {
    return Object.freeze({ mode: this.#mode, admit: false, code, outcome: code === 'RK_OWNERSHIP_UNVERIFIED' ? 'unscanned' : outcomes[code] ?? 'unscanned', scanRoot: root, error, problems: [error], violations: [], scanMs: 0, cached: false });
  }

  /** Re-authenticate & hash inputs on every admission, including cache hits. */
  async evaluate({ repositoryRoot, workspaceRoot } = {}) {
    if (this.#mode === 'off') return Object.freeze({ mode: 'off', outcome: 'off' });
    const target = ownershipScanRoot({ repositoryRoot, workspaceRoot });
    if (target.error) return this.#failure(null, target.error, 'RK_OWNERSHIP_INVALID_REGISTRATION');
    if (!this.#scannerRoot) return this.#failure(target.root, 'ownership scanner is not staged in this runtime');
    try {
      const observed = await this.#inputs({ scannerRoot: this.#scannerRoot, scanRoot: target.root, timeoutMs: this.#timeoutMs, inputsOnly: true });
      if (observed?.error) return this.#failure(target.root, observed.error, observed.code);
      if (typeof observed?.complete !== 'boolean') return this.#failure(target.root, 'source inventory completeness is missing or malformed', 'RK_OWNERSHIP_INVALID_INPUTS');
      const key = ownershipCacheKey(observed?.inputs);
      if (observed.complete === false) return this.#failure(target.root, 'source content inventory has incomplete coverage', 'RK_OWNERSHIP_INCOMPLETE_COVERAGE');
      const cached = this.#cache.get(key);
      if (cached?.scanRoot === target.root) return Object.freeze({ ...cached, cached: true });
      // Root keeps distinct repository provenance separate; digests determine freshness.
      const flight = target.root + ':' + key;
      let pending = this.#inflight.get(flight);
      if (!pending) {
        pending = this.#scan(target.root, key, observed).finally(() => this.#inflight.delete(flight));
        this.#inflight.set(flight, pending);
      }
      return await pending;
    } catch (error) { return this.#failure(target.root, error.message, error.code?.startsWith('RK_OWNERSHIP_') ? error.code : 'RK_OWNERSHIP_UNVERIFIED'); }
  }

  /** Enforce refuses every non-admitted result; report records identical verdicts. */
  assertAdmitted(result) {
    if (this.#mode !== 'enforce') return;
    if (result?.admit === true && result.code === 'RK_OWNERSHIP_CLEAN') return;
    throw Object.assign(new Error('RightKit ownership refuses this build: ' + (result?.problems?.slice(0, 5).join('; ') || result?.error || 'ownership verdict is missing or malformed')), { code: result?.code ?? 'RK_OWNERSHIP_UNVERIFIED' });
  }

  async #scan(root, key, observed) {
    const started = this.#now();
    const scanned = await this.#run({ scannerRoot: this.#scannerRoot, scanRoot: root, timeoutMs: this.#timeoutMs });
    if (scanned?.error) return this.#failure(root, scanned.error, scanned.code);
    if (ownershipCacheKey(scanned?.inputs) !== key) return this.#failure(root, 'ownership inputs changed between admission & protected scan', 'RK_OWNERSHIP_INPUTS_CHANGED');
    const result = Object.freeze({ mode: this.#mode, ...classifyOwnershipReport(scanned?.report, observed.trust), scanRoot: root, scanMs: this.#now() - started, cached: false });
    if (!['RK_OWNERSHIP_MALFORMED_REPORT', 'RK_OWNERSHIP_UNVERIFIED'].includes(result.code)) {
      this.#cache.set(key, result);
      if (this.#cache.size > 128) this.#cache.delete(this.#cache.keys().next().value);
    }
    return result;
  }
}
