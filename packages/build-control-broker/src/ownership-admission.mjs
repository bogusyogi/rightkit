import { spawn as nodeSpawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join, relative, resolve, sep, isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Build admission against RightKit ownership: a root build in an app that re-implements a capability
 * RightKit owns is reported (mode 'report') or refused (mode 'enforce'). Scans run out of process with
 * a hard timeout and bounded output, are cached per scan root, and are deduplicated while in flight. */
export const OWNERSHIP_MODES = Object.freeze(['off', 'report', 'enforce']);
const MAX_OUTPUT_BYTES = 32 * 1024 * 1024;
const here = dirname(fileURLToPath(import.meta.url));

/** Staged runtime layout (<runtime>/ownership-scanner) first, then the source checkout (packages/release). */
export function defaultScannerRoot() {
  for (const candidate of [join(here, '..', '..', 'ownership-scanner'), join(here, '..', '..', 'release')]) {
    if (existsSync(join(candidate, 'ownership-scan-worker.mjs')) && existsSync(join(candidate, 'rightkit-ownership.json'))) return candidate;
  }
  return null;
}

const containsPath = (root, candidate) => { const scoped = relative(resolve(root), resolve(candidate)); return scoped === '' || (!isAbsolute(scoped) && scoped !== '..' && !scoped.startsWith(`..${sep}`)); };

/** Which tree a job is judged by: the app repository, except in the RightKit host monorepo, where
 * RightKit's own crates are the owner (exempt) and other workspaces are scanned on their own. */
export function ownershipScanRoot({ repositoryRoot, workspaceRoot }) {
  const repository = repositoryRoot ?? workspaceRoot;
  if (!repository) return { exempt: 'no-repository' };
  const rightkit = join(repository, 'tools', 'rightkit');
  if (existsSync(join(rightkit, 'packages', 'release', 'rightkit-ownership.json'))) {
    if (!workspaceRoot || containsPath(rightkit, workspaceRoot)) return { exempt: 'rightkit-source' };
    return { root: resolve(workspaceRoot) };
  }
  return { root: resolve(repository) };
}

export function runScanWorker({ scannerRoot, scanRoot, timeoutMs, spawn = nodeSpawn, execPath = process.execPath }) {
  return new Promise((resolveScan) => {
    let child;
    try {
      child = spawn(execPath, [join(scannerRoot, 'ownership-scan-worker.mjs'), scanRoot, join(scannerRoot, 'rightkit-ownership.json')], { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    } catch (error) { resolveScan({ error: `scanner failed to start: ${error.message}` }); return; }
    const out = []; const err = []; let bytes = 0; let settled = false;
    const settleScan = (value) => { if (settled) return; settled = true; clearTimeout(timer); resolveScan(value); };
    const collect = (sink) => (chunk) => {
      bytes += chunk.length;
      if (bytes > MAX_OUTPUT_BYTES) { child.kill(); settleScan({ error: `scanner output exceeded ${MAX_OUTPUT_BYTES} bytes` }); return; }
      sink.push(chunk);
    };
    child.stdout.on('data', collect(out)); child.stderr.on('data', collect(err));
    child.once('error', (error) => settleScan({ error: `scanner failed to start: ${error.message}` }));
    child.once('close', (code) => {
      if (code !== 0) return settleScan({ error: Buffer.concat(err).toString('utf8').trim().slice(0, 2000) || `scanner exited ${code}` });
      try { settleScan({ report: JSON.parse(Buffer.concat(out).toString('utf8')) }); }
      catch (error) { settleScan({ error: `scanner returned invalid JSON: ${error.message}` }); }
    });
    const timer = setTimeout(() => { child.kill(); settleScan({ error: `scan exceeded ${timeoutMs} ms` }); }, timeoutMs);
    timer.unref?.();
  });
}

/** Typed admission outcome from a scanner report. */
export function classifyOwnershipReport(report) {
  const violations = (report.violations ?? []).map((v) => ({ capability: v.capability ?? null, owner: v.package ?? null, fingerprint: v.fingerprint ?? null, file: v.file ?? null, line: v.line ?? null }));
  const problems = report.problems ?? [];
  if (violations.length && report.enrollmentPending) return { outcome: 'pending-enrollment', violations, problems };
  if (violations.length) return { outcome: 'violations', violations, problems };
  if (problems.length) return { outcome: 'problems', violations, problems };
  return { outcome: 'clean', violations, problems };
}

export class OwnershipAdmission {
  #mode; #timeoutMs; #ttlMs; #scannerRoot; #now; #run; #cache = new Map(); #inflight = new Map();

  constructor({ mode = 'report', timeoutMs = 30_000, ttlMs = 60_000, scannerRoot = defaultScannerRoot(), now = Date.now, runScan = runScanWorker } = {}) {
    if (!OWNERSHIP_MODES.includes(mode)) throw new TypeError(`ownership mode must be one of ${OWNERSHIP_MODES.join(', ')}`);
    this.#mode = mode; this.#timeoutMs = timeoutMs; this.#ttlMs = ttlMs; this.#scannerRoot = scannerRoot; this.#now = now; this.#run = runScan;
  }

  get mode() { return this.#mode; }

  /** Evaluate a root build. Never throws; an unavailable scan is outcome 'unscanned'. */
  async evaluate({ repositoryRoot, workspaceRoot }) {
    if (this.#mode === 'off') return Object.freeze({ mode: 'off', outcome: 'off' });
    const target = ownershipScanRoot({ repositoryRoot, workspaceRoot });
    if (target.exempt) return Object.freeze({ mode: this.#mode, outcome: 'exempt', reason: target.exempt });
    const cached = this.#cache.get(target.root);
    if (cached && this.#now() - cached.at < this.#ttlMs) return Object.freeze({ ...cached.result, cached: true });
    let pending = this.#inflight.get(target.root);
    if (!pending) {
      pending = this.#scan(target.root).finally(() => this.#inflight.delete(target.root));
      this.#inflight.set(target.root, pending);
    }
    return pending;
  }

  /** Throws a typed refusal when enforcement applies; report mode never blocks. */
  assertAdmitted(result) {
    if (this.#mode !== 'enforce') return;
    if (result.outcome === 'violations') {
      const list = result.violations.slice(0, 5).map((v) => `${v.capability} (${v.fingerprint} in ${v.file}) is owned by ${v.owner}`).join('; ');
      throw Object.assign(new Error(`RightKit ownership blocks this build: ${list}${result.violations.length > 5 ? `; +${result.violations.length - 5} more` : ''}. Adopt the RightKit owner or request a signed exception: right-release ownership`), { code: 'RK_OWNERSHIP_VIOLATION' });
    }
    if (result.outcome === 'unscanned' || result.outcome === 'problems') {
      throw Object.assign(new Error(`RightKit ownership could not verify this build (${result.error ?? result.problems?.slice(0, 3).join('; ')}); enforcement fails closed`), { code: 'RK_OWNERSHIP_UNVERIFIED' });
    }
  }

  async #scan(root) {
    const started = this.#now();
    if (!this.#scannerRoot) return Object.freeze({ mode: this.#mode, outcome: 'unscanned', scanRoot: root, error: 'ownership scanner is not staged in this runtime', scanMs: 0, cached: false });
    const scanned = await this.#run({ scannerRoot: this.#scannerRoot, scanRoot: root, timeoutMs: this.#timeoutMs });
    const scanMs = this.#now() - started;
    const result = scanned.error
      ? Object.freeze({ mode: this.#mode, outcome: 'unscanned', scanRoot: root, error: scanned.error, scanMs, cached: false })
      : Object.freeze({ mode: this.#mode, ...classifyOwnershipReport(scanned.report), scanRoot: root, scanMs, cached: false });
    // Unscanned results are not cached: the next build retries instead of inheriting a transient failure.
    if (result.outcome !== 'unscanned') this.#cache.set(root, { at: this.#now(), result });
    return result;
  }
}
