import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { systemGit, type Git } from './git.js';
import { checkRoot, type AdapterArtifact, type AdapterDecl, type JourneyDecl } from './manifest.js';
import { adapterEnv, newSelection, parseResults, writeRunFiles } from './protocol.js';
import { RECEIPT_SCHEMA, validateReceipt, type Counts, type Receipt, type ReceiptArtifact, type ReceiptJourney } from './receipt.js';
import { problem, sha256File, sha256Text, type Problem } from './util.js';

export class JourneysError extends Error {
  constructor(
    message: string,
    readonly problems: Problem[] = [],
  ) {
    super(message);
    this.name = 'JourneysError';
  }
}

export interface RunOptions {
  /** App root holding journeys.toml. */
  root: string;
  ids?: string[];
  platform?: string;
  /** Default "fast". */
  tier?: string;
  runRoot?: string;
  /** Extra environment for the adapter (tests use it to choose a fixture mode). */
  env?: Record<string, string>;
  git?: Git;
  timeoutMs?: number;
}

export interface RunResult {
  receipt: Receipt;
  receiptPath: string;
  runRoot: string;
  ok: boolean;
}

const hostPlatform = (): string => (process.platform === 'win32' ? 'windows' : process.platform === 'darwin' ? 'macos' : 'linux');
const ZERO_TREE = '0'.repeat(40);

interface SourceIdentity {
  head: string;
  tree: string;
  dirty: boolean;
  diff_sha256: string | null;
  fingerprint: string;
}

function probeSource(git: Git, root: string): SourceIdentity {
  const head = git.run(['rev-parse', 'HEAD'])?.trim();
  const tree = git.run(['rev-parse', 'HEAD^{tree}'])?.trim();
  const status = git.run(['status', '--porcelain']);
  if (!head || !tree || status === null || !/^[0-9a-f]{40}$/.test(head) || !/^[0-9a-f]{40}$/.test(tree)) {
    throw new JourneysError('source identity unavailable: the app root is not inside a git repository with a commit');
  }
  const dirty = status.trim() !== '';
  let diff: string | null = null;
  if (dirty) {
    const parts = [git.run(['diff', 'HEAD', '--binary']) ?? ''];
    for (const f of (git.run(['ls-files', '--others', '--exclude-standard']) ?? '').split('\n').filter(Boolean).sort()) {
      parts.push(`${f}\0${sha256File(path.join(root, f)) ?? 'unreadable'}`);
    }
    diff = sha256Text(parts.join('\n'));
  }
  return { head, tree, dirty, diff_sha256: diff, fingerprint: `${tree}:${diff ?? ''}` };
}

interface Resolved {
  role: string;
  path: string;
}

function parseCargoArtifacts(stdout: string, decls: AdapterArtifact[]): Map<string, string> {
  const found = new Map<string, string>();
  for (const line of stdout.split('\n')) {
    if (!line.startsWith('{')) continue;
    let m: Record<string, unknown>;
    try {
      m = JSON.parse(line) as Record<string, unknown>;
    } catch {
      continue;
    }
    if (m.reason !== 'compiler-artifact' || typeof m.executable !== 'string') continue;
    const target = (m.target as { name?: string } | undefined)?.name;
    const pkg = typeof m.package_id === 'string' ? m.package_id : '';
    for (const d of decls) {
      if (d.cargo_target && target === d.cargo_target) found.set(d.role, m.executable);
      else if (d.cargo_package && !d.cargo_target && (pkg.includes(`/${d.cargo_package}#`) || pkg.startsWith(`${d.cargo_package} `) || pkg.includes(`#${d.cargo_package}@`) || pkg.includes(`${d.cargo_package}@`))) {
        found.set(d.role, m.executable);
      }
    }
  }
  return found;
}

function measure(files: Resolved[]): ReceiptArtifact[] {
  return files.map((f) => {
    let size = 0;
    try {
      size = fs.statSync(f.path).size;
    } catch {
      /* missing file: sha stays unmeasured */
    }
    return { role: f.role, path: f.path, size, sha256: sha256File(f.path) ?? '0'.repeat(64) };
  });
}

/**
 * `journeys run`: select, record source identity, build, launch the adapter, collect, write the receipt. Any failure
 * ends the run with a receipt whose verdict is computed by `validateReceipt`, never by this function.
 * Phase 1 supports `runner = "adapter"` only; the adapter is started directly, not through the interactive-session
 * launcher (D11), so the receipt records a noninteractive session.
 */
export function runJourneys(opts: RunOptions): RunResult {
  const root = path.resolve(opts.root);
  const git = opts.git ?? systemGit(root);
  const loaded = checkRoot(root);
  if (!loaded.manifest || !loaded.manifestText || loaded.problems.length > 0) {
    throw new JourneysError('the manifest is invalid; run `journeys check`', loaded.problems);
  }
  const manifest = loaded.manifest;
  const platform = opts.platform ?? hostPlatform();
  const tier = opts.tier ?? 'fast';

  if (opts.ids) {
    for (const id of opts.ids) {
      const j = manifest.journeys.find((x) => x.id === id);
      if (!j) throw new JourneysError(`journey ${id} is not in the manifest (a misspelled filter runs nothing)`);
      if (!j.platforms.includes(platform as never)) throw new JourneysError(`journey ${id} does not declare platform ${platform}`);
    }
  }
  const selected: JourneyDecl[] = manifest.journeys.filter(
    (j) => (!opts.ids || opts.ids.includes(j.id)) && j.platforms.includes(platform as never) && j.tier === tier,
  );

  const manifestSha = sha256Text(loaded.manifestText);
  const selection = newSelection({
    app: manifest.app,
    platform,
    tier,
    manifest_sha256: manifestSha,
    journeys: selected.map((j) => ({ id: j.id, entry: j.entry })),
  });
  const runRoot = path.resolve(opts.runRoot ?? path.join(os.tmpdir(), 'rightkit-qa', `journeys-${selection.run_id.slice(0, 8)}`));
  const files = writeRunFiles(runRoot, selection);
  const dataDir = path.join(runRoot, 'data');
  fs.mkdirSync(dataDir, { recursive: true });

  const startedAt = new Date().toISOString();
  const source = probeSource(git, root);
  const notes: Problem[] = [];
  let abort = null as string | null;
  const stop = (reason: string): void => {
    abort ??= reason;
    notes.push(problem('run_aborted', null, reason));
  };

  const adapterNames = new Set(selected.map((j) => j.adapter ?? `(${j.runner})`));
  let adapter: AdapterDecl | null = null;
  if (selected.length === 0) stop('zero journeys selected');
  else if (selected.some((j) => j.runner !== 'adapter')) stop('runner "scenario" is not implemented in this phase; use an adapter');
  else if (adapterNames.size > 1) stop(`one adapter per run is supported; selected journeys use ${[...adapterNames].join(', ')}`);
  else adapter = manifest.adapters.find((a) => a.name === selected[0]!.adapter) ?? null;
  if (selected.length > 0 && !abort && !adapter) stop('adapter not found');

  const decls: AdapterArtifact[] = adapter ? [adapter.executable, ...adapter.artifacts] : [];
  const buildStarted = new Date().toISOString();
  const resolved: Resolved[] = [];
  let tree_after = source.tree;
  if (adapter && !abort) {
    if (adapter.build.length > 0) {
      const [cmd, ...args] = adapter.build;
      const b = spawnSync(cmd!, args, {
        cwd: root,
        encoding: 'utf8',
        maxBuffer: 512 * 1024 * 1024,
        windowsHide: true,
        shell: process.platform === 'win32',
        timeout: opts.timeoutMs ?? 600_000,
      });
      if (b.status !== 0) stop(`build exited ${b.status ?? 'abnormally'}`);
      else {
        const cargo = parseCargoArtifacts(b.stdout, decls);
        for (const d of decls) {
          const p = d.path ? path.resolve(root, d.path) : cargo.get(d.role);
          if (!p) stop(`build produced no artifact for role ${d.role}`);
          else resolved.push({ role: d.role, path: p });
        }
      }
    } else {
      for (const d of decls) {
        if (!d.path) stop(`artifact ${d.role} needs a path when the adapter has no build`);
        else resolved.push({ role: d.role, path: path.resolve(root, d.path) });
      }
    }
    for (const r of resolved) if (!fs.existsSync(r.path)) stop(`artifact ${r.role} does not exist at ${r.path}`);
  }
  const buildArtifacts = measure(resolved).map((a) => ({ ...a, produced_by: 'build_output' as const }));
  const buildEnded = new Date().toISOString();
  const after = probeSource(git, root);
  if (after.fingerprint !== source.fingerprint) {
    tree_after = sha256Text(`${after.fingerprint}|changed`).slice(0, 40);
    stop('the source tree changed during the build');
  }

  // Launch: re-hash immediately before starting; a changed digest is recorded and the adapter is not started.
  const launched = measure(resolved);
  for (const [i, l] of launched.entries()) {
    if (l.sha256 !== buildArtifacts[i]!.sha256) stop(`artifact ${l.role} changed between build and launch`);
  }
  let exitCode = -1;
  let argv: string[] = [];
  if (adapter && !abort) {
    const exe = launched.find((l) => l.role === adapter!.executable.role)!;
    const isScript = /\.(mjs|cjs|js)$/.test(exe.path);
    const cmd = isScript ? process.execPath : exe.path;
    argv = [...(isScript ? [exe.path] : []), ...adapter.args];
    const r = spawnSync(cmd, argv, {
      cwd: root,
      encoding: 'utf8',
      windowsHide: true,
      timeout: opts.timeoutMs ?? 600_000,
      env: {
        ...process.env,
        ...opts.env,
        ...adapterEnv(files),
        RIGHTKIT_QA_DATA_DIR: dataDir,
        RIGHTKIT_QA_MODE: 'runtime',
      },
    });
    exitCode = r.status ?? -1;
    argv = [cmd, ...argv];
    fs.writeFileSync(path.join(runRoot, 'adapter.log'), `${r.stdout ?? ''}\n--- stderr ---\n${r.stderr ?? ''}\n`);
    if (r.error) stop(`adapter failed to run: ${r.error.message}`);
  }

  // Collect.
  const resultsText = fs.readFileSync(files.resultsPath, 'utf8');
  const parsed = parseResults(resultsText, { run_id: selection.run_id, nonce: selection.nonce, ids: selected.map((j) => j.id) });
  const byId = new Map(parsed.journeys.map((j) => [j.id, j]));
  const nowIso = new Date().toISOString();
  const journeys: ReceiptJourney[] = selected.map((j) => {
    const rec = parsed.discarded ? undefined : byId.get(j.id);
    if (!rec) {
      return {
        id: j.id,
        status: 'missing',
        release_critical: j.release_critical,
        started_at: nowIso,
        duration_ms: 0,
        units: { total: 0, passed: 0, failed: 0, skipped: 0 },
        evidence: [],
        reason: abort ? `not executed: ${abort}` : 'no result record from the adapter',
      };
    }
    return {
      id: j.id,
      status: rec.status,
      release_critical: j.release_critical,
      started_at: rec.started_at,
      duration_ms: rec.duration_ms,
      units: rec.units,
      evidence: rec.evidence,
      reason: rec.reason,
    };
  });
  const counts: Counts = {
    selected: selected.length,
    executed: journeys.filter((j) => j.status !== 'missing').length,
    passed: journeys.filter((j) => j.status === 'passed').length,
    failed: journeys.filter((j) => j.status === 'failed').length,
    skipped: journeys.filter((j) => j.status === 'skipped').length,
    errored: journeys.filter((j) => j.status === 'errored').length,
    missing: journeys.filter((j) => j.status === 'missing').length,
    duplicate: parsed.duplicates,
  };
  const mapText = loaded.mapText;
  const receipt: Receipt = {
    schema: RECEIPT_SCHEMA,
    run_id: selection.run_id,
    nonce: selection.nonce,
    app: manifest.app,
    kind: 'functional',
    verdict: 'incomplete',
    started_at: startedAt,
    ended_at: nowIso,
    host: {
      os: hostPlatform(),
      arch: process.arch,
      session: process.env.RIGHTKIT_QA_INTERACTIVE === '1' ? 'interactive' : 'noninteractive',
    },
    source: { head: source.head, tree: source.tree, dirty: source.dirty, diff_sha256: source.diff_sha256 },
    manifest: { path: 'journeys.toml', sha256: manifestSha },
    mapping: mapText === null ? null : { path: 'journeys-map.toml', sha256: sha256Text(mapText) },
    build: {
      command: adapter?.build ?? [],
      source_tree: source.tree || ZERO_TREE,
      tree_after_build: tree_after,
      started_at: buildStarted,
      ended_at: buildEnded,
      broker_receipt: null,
      artifacts: buildArtifacts,
    },
    launched,
    isolation: { data_dir: dataDir, guard: 'passed', qa_mode: 'runtime', real_data_touched: false },
    adapter: {
      name: parsed.start?.adapter ?? adapter?.name ?? 'none',
      version: parsed.start?.adapter_version ?? 'unknown',
      protocol: 1,
      argv,
      exit_code: exitCode,
      results_sha256: sha256Text(resultsText),
    },
    selection: { ids: selected.map((j) => j.id), platform, tier },
    journeys,
    counts,
    problems: [],
  };
  const check = (): ReturnType<typeof validateReceipt> =>
    validateReceipt(receipt, { resultsText, manifestText: loaded.manifestText, manifest, evidenceRoot: runRoot });
  const first = check();
  receipt.verdict = first.computed;
  const final = check();
  receipt.problems = [...notes, ...final.problems];
  const receiptPath = path.join(runRoot, 'journeys-receipt.json');
  fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
  return { receipt, receiptPath, runRoot, ok: final.ok };
}
