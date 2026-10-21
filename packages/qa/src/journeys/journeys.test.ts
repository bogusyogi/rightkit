import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, describe, expect, it } from 'vitest';
import { census, censusDiff, countTests } from './census.js';
import { checkTombstones, classifyPath, coverageChanges, runGate } from './gate.js';
import { systemGit, type Git } from './git.js';
import { checkRoot, parseManifest, parseMap, validateManifest } from './manifest.js';
import { newSelection, parseResults, writeRunFiles, adapterEnv, Reporter } from './protocol.js';
import { validateReceipt, validateReceiptFile, type Receipt, type ValidateReceiptOptions } from './receipt.js';
import { JourneysError, runJourneys } from './run.js';
import { parseToml } from './toml.js';
import { canonicalJson, sha256Text } from './util.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.resolve(here, '../../journeys-fixture-adapter.mjs').replace(/\\/g, '/');
const scratch: string[] = [];
const tmp = (label: string): string => {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), `rk-journeys-${label}-`));
  scratch.push(d);
  return d;
};
afterAll(() => {
  for (const d of scratch) fs.rmSync(d, { recursive: true, force: true });
});

const TREE = 'b'.repeat(40);
const HEAD = 'a'.repeat(40);
function fakeGit(extra: Record<string, string | null> = {}): Git {
  const table: Record<string, string | null> = {
    'rev-parse HEAD': `${HEAD}\n`,
    'rev-parse HEAD^{tree}': `${TREE}\n`,
    'status --porcelain': '',
    ...extra,
  };
  return { run: (args) => (args.join(' ') in table ? table[args.join(' ')]! : null) };
}

const MANIFEST = `version = 1
app = "fixture"
coverage_owners = ["bogusyogi"]

[[adapter]]
name = "fx"
executable = { role = "adapter", path = "${FIXTURE}" }
artifacts = [{ role = "app", path = "fixture-app.txt" }]
args = ["fixture-app.txt"]
sources = ["adapter/**"]

${['J-001', 'J-002', 'J-003']
  .map(
    (id) => `[[journey]]
id = "${id}"
title = "fixture ${id}"
platforms = ["macos", "windows", "linux"]
surface = "cli"
runner = "adapter"
adapter = "fx"
entry = "entries/${id}.scenario.toml"
tier = "fast"
data = "generated"
release_critical = true
`,
  )
  .join('\n')}`;

function appRoot(manifest = MANIFEST): string {
  const root = tmp('app');
  fs.mkdirSync(path.join(root, 'entries'));
  for (const id of ['J-001', 'J-002', 'J-003']) fs.writeFileSync(path.join(root, 'entries', `${id}.scenario.toml`), '# entry\n');
  fs.writeFileSync(path.join(root, 'fixture-app.txt'), 'built binary stand-in\n');
  fs.writeFileSync(path.join(root, 'journeys.toml'), manifest);
  return root;
}

// ---------------------------------------------------------------------------------------------------------------

describe('toml reader', () => {
  it('reads tables, arrays of tables, inline tables and arrays', () => {
    const t = parseToml(`a = 1 # c\n[x]\nb = "q\\"s"\nc = ['l']\n[[y]]\nz = { k = true, n = [1, 2] }\n[[y]]\nz = {}\n`);
    expect(t.a).toBe(1);
    expect(t.x).toEqual({ b: 'q"s', c: ['l'] });
    expect((t.y as unknown[]).length).toBe(2);
    expect((t.y as { z: unknown }[])[0]!.z).toEqual({ k: true, n: [1, 2] });
  });
  it('refuses duplicate keys and unsupported values', () => {
    expect(() => parseToml('a = 1\na = 2\n')).toThrow(/duplicate/);
    expect(() => parseToml('a = 1979-05-27\n')).toThrow();
  });
});

describe('manifest validation (journeys check)', () => {
  it('accepts the fixture manifest', () => {
    const r = checkRoot(appRoot());
    expect(r.problems).toEqual([]);
    expect(r.manifest?.journeys).toHaveLength(3);
  });
  const codes = (text: string, root = appRoot()): string[] => {
    const parsed = parseManifest(text);
    return [...parsed.problems, ...(parsed.value ? validateManifest(parsed.value, { root }) : [])].map((p) => p.code);
  };
  it('refuses a duplicate id', () => {
    expect(codes(`${MANIFEST}\n${MANIFEST.split('[[journey]]')[1]!.replace(/^/, '[[journey]]')}`)).toContain('duplicate_id');
  });
  it('refuses a missing entry', () => {
    const root = appRoot();
    fs.rmSync(path.join(root, 'entries', 'J-002.scenario.toml'));
    expect(codes(MANIFEST, root)).toContain('entry_missing');
  });
  it('refuses data = "real"', () => {
    expect(codes(MANIFEST.replace('data = "generated"', 'data = "real"'))).toContain('real_data');
  });
  it('refuses an undeclared adapter', () => {
    expect(codes(MANIFEST.replace('adapter = "fx"', 'adapter = "nope"'))).toContain('adapter_unresolved');
  });
  it('refuses an adapter on a scenario runner and a missing adapter reference', () => {
    expect(codes(MANIFEST.replace('runner = "adapter"\nadapter = "fx"', 'runner = "adapter"'))).toContain('adapter_missing');
  });
  it('refuses a bad id, platform, surface and tier', () => {
    expect(codes(MANIFEST.replace('id = "J-001"', 'id = "one"'))).toContain('schema');
    expect(codes(MANIFEST.replace('platforms = ["macos", "windows", "linux"]', 'platforms = ["beos"]'))).toContain('schema');
    expect(codes(MANIFEST.replace('surface = "cli"', 'surface = "gui"'))).toContain('schema');
  });
  it('native-ui must launch the built app binary', () => {
    const native = MANIFEST.replaceAll('surface = "cli"', 'surface = "native-ui"');
    expect(codes(native)).toEqual([]); // the adapter declares an app artifact
    expect(codes(native.replace('artifacts = [{ role = "app", path = "fixture-app.txt" }]\n', ''))).toContain('native_ui_no_binary');
    expect(codes(native.replace('runner = "adapter"\nadapter = "fx"', 'runner = "scenario"'))).toContain('native_ui_no_binary');
    expect(codes(native.replace('args = ["fixture-app.txt"]', 'args = ["fixture-app.txt", "--vite"]'))).toContain('native_ui_mocked');
    expect(codes(native.replace('sources = ["adapter/**"]', 'sources = ["adapter/**"]\nmocked_ipc = true'))).toContain('native_ui_mocked');
  });
  it('requires a replacement or a risk acceptance on a tombstone', () => {
    const t = `${MANIFEST}\n[[change]]\nid = "J-009"\nkind = "removed"\nbefore_after_sha256 = "x"\napproval = "https://github.com/o/r/issues/1#issuecomment-2"\n`;
    expect(codes(t)).toContain('change_unresolved');
  });
  it('checks the map against the manifest', () => {
    const root = appRoot();
    fs.writeFileSync(
      path.join(root, 'journeys-map.toml'),
      'version = 1\n[[behavior]]\nid = "B-a"\ntitle = "a"\npaths = ["src/**"]\njourneys = ["J-001", "J-404"]\n',
    );
    expect(checkRoot(root).problems.map((p) => p.code)).toContain('behavior_journey_unknown');
    expect(parseMap('version = 1\n[[behavior]]\nid = "B"\n').problems.map((p) => p.code)).toContain('schema');
  });
});

// ---------------------------------------------------------------------------------------------------------------

describe('adapter protocol helpers', () => {
  const sel = newSelection({ app: 'a', platform: 'windows', tier: 'fast', manifest_sha256: '0'.repeat(64), journeys: [{ id: 'J-001', entry: 'e' }] });
  const ids = ['J-001'];
  const line = (o: object): string => `${JSON.stringify({ run_id: sel.run_id, nonce: sel.nonce, ...o })}\n`;
  const start = line({
    type: 'start', protocol: 1, adapter: 'x', adapter_version: '1', started_at: '2026-10-21T10:00:00Z',
    artifacts: [{ role: 'app', path: 'p', sha256: 'c'.repeat(64) }], isolation: { data_dir: 'd', guard: 'passed', qa_mode: 'runtime' },
  });
  const journey = line({
    type: 'journey', id: 'J-001', status: 'passed', started_at: '2026-10-21T10:00:00Z', duration_ms: 3,
    units: { total: 1, passed: 1, failed: 0, skipped: 0 }, evidence: [], reason: null,
  });
  const end = line({ type: 'end', selected: 1, reported: 1, ended_at: '2026-10-21T10:00:01Z' });

  it('writes selection.json, an empty results file and the protocol environment', () => {
    const dir = tmp('run');
    const files = writeRunFiles(dir, sel);
    expect(JSON.parse(fs.readFileSync(files.selectionPath, 'utf8')).nonce).toBe(sel.nonce);
    expect(fs.readFileSync(files.resultsPath, 'utf8')).toBe('');
    const env = adapterEnv(files);
    expect(env.RIGHTKIT_JOURNEYS_PROTOCOL).toBe('1');
    expect(path.isAbsolute(env.RIGHTKIT_JOURNEYS_SELECTION!)).toBe(true);
    expect(sel.nonce).toMatch(/^[0-9a-f]{32}$/);
  });
  it('accepts a complete, matching results file', () => {
    const r = parseResults(start + journey + end, { ...sel, ids });
    expect(r.problems).toEqual([]);
    expect(r.journeys).toHaveLength(1);
  });
  it('rejects every malformed or foreign record', () => {
    const codes = (text: string): string[] => parseResults(text, { ...sel, ids }).problems.map((p) => p.code);
    expect(codes('')).toContain('protocol_violation');
    expect(codes(start + journey)).toContain('protocol_violation'); // no end
    expect(codes(journey + end)).toContain('protocol_violation'); // no start
    expect(codes(start + journey + journey + end)).toContain('duplicate_result');
    expect(codes(start + journey.replace('J-001', 'J-777') + end)).toContain('unselected_result');
    expect(codes(start + journey.replace(sel.nonce, 'e'.repeat(32)) + end)).toEqual(['stale_results']);
    expect(codes(start + journey.replace(sel.run_id, '00000000-0000-4000-8000-000000000000') + end)).toEqual(['stale_results']);
    expect(codes(start + journey.replace('"passed"', '"maybe"') + end)).toContain('protocol_violation');
    expect(codes(start + journey.replace('"reason":null', '"reason":null').replace('"passed"', '"failed"') + end)).toContain('protocol_violation'); // non-passed needs a reason
    expect(codes(start + 'not json\n' + journey + end)).toContain('protocol_violation');
    expect(codes(start + journey + end.replace('"reported":1', '"reported":2'))).toContain('protocol_violation');
    expect(codes(start.replace('"passed"', '"failed"') + journey + end)).toContain('isolation');
    expect(codes(start + journey + end + journey)).toContain('protocol_violation'); // record after end
  });
  it('the Node helper writes lines the validator accepts', () => {
    const dir = tmp('rep');
    const files = writeRunFiles(dir, sel);
    const rep = Reporter.fromEnv({ ...adapterEnv(files) });
    rep.start({ adapter: 'x', adapter_version: '1', artifacts: [], isolation: { data_dir: 'd', guard: 'passed', qa_mode: 'runtime' } });
    rep.journey({ id: 'J-001', status: 'passed', duration_ms: 1, units: { total: 1, passed: 1, failed: 0, skipped: 0 } });
    rep.end();
    expect(parseResults(fs.readFileSync(files.resultsPath, 'utf8'), { ...sel, ids }).problems).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------------------------

interface Fixture {
  root: string;
  runRoot: string;
  receipt: Receipt;
  receiptPath: string;
  resultsText: string;
  opts: ValidateReceiptOptions;
}

function runFixture(mode: string, extra: { git?: Git; ids?: string[] } = {}): Fixture {
  const root = appRoot();
  const runRoot = path.join(tmp('runroot'), 'run');
  const result = runJourneys({ root, platform: 'windows', runRoot, env: { RK_FIXTURE_MODE: mode }, git: extra.git ?? fakeGit(), ...(extra.ids ? { ids: extra.ids } : {}) });
  const resultsText = fs.readFileSync(path.join(runRoot, 'results.jsonl'), 'utf8');
  const loaded = checkRoot(root);
  return {
    root,
    runRoot,
    receipt: result.receipt,
    receiptPath: result.receiptPath,
    resultsText,
    opts: { resultsText, manifestText: loaded.manifestText, manifest: loaded.manifest, evidenceRoot: runRoot, tree: TREE },
  };
}

describe('journeys run against the fixture adapter', () => {
  it('a clean run is passed and re-validates offline, from the file as well', () => {
    const f = runFixture('pass');
    expect(f.receipt.verdict).toBe('passed');
    expect(f.receipt.counts).toMatchObject({ selected: 3, executed: 3, passed: 3, missing: 0 });
    expect(f.receipt.launched.map((l) => l.role).sort()).toEqual(['adapter', 'app']);
    expect(validateReceipt(f.receipt, f.opts)).toMatchObject({ ok: true, verdict: 'passed', problems: [] });
    const viaFile = validateReceiptFile(f.receiptPath, { manifestPath: path.join(f.root, 'journeys.toml'), manifest: f.opts.manifest ?? null, tree: TREE });
    expect(viaFile.ok).toBe(true);
  });

  // One negative test per pass condition: each fixture behaviour must be refused, with its own problem code.
  const refused: [string, string, 'failed' | 'incomplete'][] = [
    ['skip', 'skipped', 'failed'],
    ['fail', 'journey_failed', 'failed'],
    ['error', 'journey_errored', 'failed'],
    ['dup', 'duplicate_result', 'failed'],
    ['missing', 'missing_result', 'incomplete'],
    ['unselected', 'unselected_result', 'failed'],
    ['stale-nonce', 'stale_results', 'incomplete'],
    ['stale-run', 'stale_results', 'incomplete'],
    ['wrong-digest', 'artifact_mismatch', 'failed'],
    ['exit1', 'adapter_exit', 'failed'],
    ['zero-units', 'journey_inconsistent', 'failed'],
    ['noop', 'zero_executed', 'incomplete'],
    ['no-end', 'protocol_violation', 'incomplete'],
    ['no-start', 'protocol_violation', 'incomplete'],
    ['bad-end-count', 'protocol_violation', 'incomplete'],
    ['bad-isolation', 'isolation', 'failed'],
  ];
  for (const [mode, code, verdict] of refused) {
    it(`refuses fixture mode ${mode} (${code}) and the saved receipt re-validates the same way`, () => {
      const f = runFixture(mode);
      expect(f.receipt.verdict).not.toBe('passed');
      expect(f.receipt.verdict).toBe(verdict);
      const v = validateReceipt(f.receipt, f.opts);
      expect(v.ok).toBe(false);
      expect(v.problems.map((p) => p.code)).toContain(code);
      expect(v.verdict).toBe(verdict);
    });
  }

  it('a misspelled filter is an error, and a filter that selects nothing yields an incomplete receipt', () => {
    expect(() => runFixture('pass', { ids: ['J-01'] })).toThrow(JourneysError);
    const root = appRoot();
    const r = runJourneys({ root, platform: 'windows', tier: 'full', runRoot: path.join(tmp('z'), 'r'), git: fakeGit() });
    expect(r.receipt.verdict).toBe('incomplete');
    expect(r.ok).toBe(false);
    expect(r.receipt.counts.selected).toBe(0);
    expect(validateReceipt(r.receipt, { resultsText: '', manifestText: fs.readFileSync(path.join(root, 'journeys.toml'), 'utf8') }).problems.map((p) => p.code)).toContain('zero_selected');
  });

  it('a dirty tree is recorded with a diff digest and refused for release signoff', () => {
    const f = runFixture('pass', { git: fakeGit({ 'status --porcelain': ' M a.rs\n', 'diff HEAD --binary': 'diff', 'ls-files --others --exclude-standard': '' }) });
    expect(f.receipt.source.dirty).toBe(true);
    expect(f.receipt.source.diff_sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(validateReceipt(f.receipt, f.opts).ok).toBe(true);
    const v = validateReceipt(f.receipt, { ...f.opts, requireClean: true });
    expect(v.ok).toBe(false);
    expect(v.problems.map((p) => p.code)).toContain('dirty_source');
  });

  it('a source tree that changes during the build fails the run before launch', () => {
    let calls = 0;
    const git: Git = {
      run: (args) => {
        const key = args.join(' ');
        if (key === 'status --porcelain') return ++calls > 1 ? ' M x\n' : '';
        if (key === 'diff HEAD --binary') return 'late change';
        if (key === 'ls-files --others --exclude-standard') return '';
        return fakeGit().run(args);
      },
    };
    const f = runFixture('pass', { git });
    expect(f.receipt.verdict).not.toBe('passed');
    expect(f.receipt.adapter.exit_code).toBe(-1);
    expect(validateReceipt(f.receipt, f.opts).problems.map((p) => p.code)).toContain('source_mismatch');
  });

  it('refuses a runner "scenario" journey in this phase with an incomplete receipt', () => {
    const root = appRoot(MANIFEST.replace('runner = "adapter"\nadapter = "fx"', 'runner = "scenario"'));
    const r = runJourneys({ root, platform: 'windows', runRoot: path.join(tmp('s'), 'r'), git: fakeGit() });
    expect(r.ok).toBe(false);
    expect(r.receipt.counts.missing).toBeGreaterThan(0);
  });

  it('refuses an invalid manifest and a non-git root without writing a receipt', () => {
    expect(() => runJourneys({ root: appRoot(MANIFEST.replace('data = "generated"', 'data = "real"')), git: fakeGit() })).toThrow(JourneysError);
    expect(() => runJourneys({ root: appRoot(), git: { run: () => null } })).toThrow(/source identity/);
  });
});

describe('validate-receipt: each D5 pass condition, mutated on a passing receipt', () => {
  const good = (): Fixture => runFixture('pass');
  const mutate = (f: Fixture, edit: (r: Receipt) => void, opts: ValidateReceiptOptions = f.opts): string[] => {
    const r = structuredClone(f.receipt);
    edit(r);
    const v = validateReceipt(r, opts);
    expect(v.ok).toBe(false);
    expect(v.verdict).not.toBe('passed');
    return v.problems.map((p) => p.code);
  };
  const sha = (c: string): string => c.repeat(64);

  it('zero selected', () => {
    const codes = mutate(good(), (r) => {
      r.selection.ids = [];
      r.journeys = [];
      r.counts = { selected: 0, executed: 0, passed: 0, failed: 0, skipped: 0, errored: 0, missing: 0, duplicate: 0 };
    });
    expect(codes).toContain('zero_selected');
  });
  it('zero executed (all synthesized missing)', () => {
    const codes = mutate(good(), (r) => {
      r.journeys.forEach((j) => (j.status = 'missing'));
      r.counts = { ...r.counts, executed: 0, passed: 0, missing: 3 };
    });
    expect(codes).toEqual(expect.arrayContaining(['zero_executed', 'missing_result']));
  });
  it('a missing journey record', () => {
    expect(mutate(good(), (r) => (r.journeys.pop(), (r.counts.executed = 2), (r.counts.passed = 2), (r.counts.missing = 1)))).toContain('missing_result');
  });
  it('a duplicate journey record', () => {
    expect(mutate(good(), (r) => r.journeys.push(structuredClone(r.journeys[0]!)))).toContain('duplicate_result');
  });
  it('an unselected journey record', () => {
    expect(mutate(good(), (r) => r.journeys.push({ ...structuredClone(r.journeys[0]!), id: 'J-404' }))).toContain('unselected_result');
  });
  it('skipped, errored and failed journeys', () => {
    for (const [status, code] of [['skipped', 'skipped'], ['errored', 'journey_errored'], ['failed', 'journey_failed']] as const) {
      expect(mutate(good(), (r) => ((r.journeys[0]!.status = status), (r.journeys[0]!.reason = 'x')))).toContain(code);
    }
  });
  it('a passed journey with zero or failing units', () => {
    expect(mutate(good(), (r) => (r.journeys[0]!.units = { total: 0, passed: 0, failed: 0, skipped: 0 }))).toContain('journey_inconsistent');
    expect(mutate(good(), (r) => (r.journeys[0]!.units = { total: 2, passed: 1, failed: 1, skipped: 0 }))).toContain('journey_inconsistent');
    expect(mutate(good(), (r) => (r.journeys[0]!.units = { total: 2, passed: 1, failed: 0, skipped: 1 }))).toContain('journey_inconsistent');
  });
  it('a receipt that disagrees with the results file', () => {
    const f = good();
    const codes = mutate(f, (r) => (r.journeys[0]!.status = 'skipped'));
    expect(codes).toContain('receipt_results_mismatch');
  });
  it('a run_id or nonce mismatch', () => {
    const f = good();
    expect(mutate(f, (r) => (r.nonce = 'e'.repeat(32)))).toContain('stale_results');
    expect(mutate(f, (r) => (r.run_id = '00000000-0000-4000-8000-000000000000'))).toContain('stale_results');
  });
  it('missing results or manifest cannot be proven', () => {
    const f = good();
    expect(mutate(f, () => undefined, { ...f.opts, resultsText: null })).toContain('results_unavailable');
    expect(mutate(f, () => undefined, { ...f.opts, manifestText: null })).toContain('manifest_unverified');
    expect(validateReceipt(f.receipt, { ...f.opts, resultsText: null }).verdict).toBe('incomplete');
  });
  it('an artifact digest mismatch, and a binary that was not built', () => {
    const f = good();
    expect(mutate(f, (r) => (r.launched[0]!.sha256 = sha('d')))).toContain('artifact_mismatch');
    expect(mutate(f, (r) => (r.launched[0]!.role = 'stranger'))).toContain('artifact_not_from_build');
    expect(mutate(f, (r) => (r.launched = []))).toContain('no_launch');
    expect(mutate(f, (r) => (r.build.artifacts[0]!.produced_by = 'broker_receipt'))).toContain('artifact_not_from_build');
  });
  it('an artifact that differs on this host when files are checked', () => {
    const f = good();
    fs.writeFileSync(path.join(f.root, 'fixture-app.txt'), 'swapped binary\n');
    const codes = mutate(f, () => undefined, { ...f.opts, checkFiles: true });
    expect(codes).toContain('artifact_mismatch');
  });
  it('source, build and manifest bindings', () => {
    const f = good();
    expect(mutate(f, (r) => (r.build.tree_after_build = 'c'.repeat(40)))).toContain('source_mismatch');
    expect(mutate(f, (r) => (r.build.source_tree = 'c'.repeat(40)))).toContain('source_mismatch');
    expect(mutate(f, () => undefined, { ...f.opts, tree: 'c'.repeat(40) })).toContain('source_mismatch');
    expect(mutate(f, () => undefined, { ...f.opts, manifestText: `${f.opts.manifestText}\n# edited\n` })).toContain('source_mismatch');
    expect(mutate(f, (r) => (r.manifest.sha256 = sha('1')))).toContain('source_mismatch');
  });
  it('dirty source without a diff digest is malformed', () => {
    expect(mutate(good(), (r) => ((r.source.dirty = true), (r.source.diff_sha256 = null)))).toContain('schema');
  });
  it('an isolation failure', () => {
    const f = good();
    expect(mutate(f, (r) => ((r.isolation as { real_data_touched: boolean }).real_data_touched = true))).toContain('isolation');
    expect(mutate(f, (r) => ((r.isolation as { guard: string }).guard = 'failed'))).toContain('isolation');
    expect(mutate(f, (r) => ((r.isolation as { qa_mode: string }).qa_mode = 'release'))).toContain('isolation');
    expect(mutate(f, (r) => (r.isolation.data_dir = '/real/user/data'))).toContain('isolation');
  });
  it('a non-zero adapter exit, and a results file that is not the one read', () => {
    const f = good();
    expect(mutate(f, (r) => (r.adapter.exit_code = 1))).toContain('adapter_exit');
    expect(mutate(f, (r) => (r.adapter.results_sha256 = sha('2')))).toContain('adapter_exit');
  });
  it('an evidence digest mismatch or missing evidence file', () => {
    const f = good();
    fs.writeFileSync(path.join(f.runRoot, 'e.log'), 'real log');
    expect(mutate(f, (r) => (r.journeys[0]!.evidence = [{ path: 'e.log', sha256: sha('3'), kind: 'log' }]))).toContain('evidence_mismatch');
    expect(mutate(f, (r) => (r.journeys[0]!.evidence = [{ path: 'gone.log', sha256: sha('3'), kind: 'log' }]))).toContain('evidence_mismatch');
    expect(mutate(f, (r) => (r.journeys[0]!.evidence = [{ path: '../escape', sha256: sha('3'), kind: 'log' }]))).toContain('evidence_mismatch');
    expect(mutate(f, (r) => (r.journeys[0]!.evidence = [{ path: 'e.log', sha256: sha256Text('real log'), kind: 'log' }]), { ...f.opts, evidenceRoot: undefined })).toContain('evidence_unverified');
    const ok = structuredClone(f.receipt);
    ok.journeys[0]!.evidence = [{ path: 'e.log', sha256: sha256Text('real log'), kind: 'log' }];
    // The results file records no evidence, but the receipt's own evidence digest is what is checked here.
    expect(validateReceipt(ok, f.opts).problems.map((p) => p.code)).not.toContain('evidence_mismatch');
  });
  it('a lying verdict or counts field is recomputed, never believed', () => {
    const f = good();
    const lie = structuredClone(f.receipt);
    lie.journeys[0]!.status = 'skipped';
    lie.verdict = 'passed';
    const v = validateReceipt(lie, f.opts);
    expect(v.ok).toBe(false);
    expect(v.problems.map((p) => p.code)).toEqual(expect.arrayContaining(['skipped', 'verdict_mismatch', 'counts_mismatch']));
    expect(mutate(f, (r) => (r.counts.passed = 99))).toContain('counts_mismatch');
    const honest = structuredClone(f.receipt);
    honest.verdict = 'failed';
    expect(validateReceipt(honest, f.opts).problems.map((p) => p.code)).toContain('verdict_mismatch');
  });
  it('a malformed receipt is incomplete, not passed', () => {
    expect(validateReceipt({}, {}).verdict).toBe('incomplete');
    expect(validateReceipt(null, {}).ok).toBe(false);
    expect(validateReceipt({ ...good().receipt, schema: 'other/1' }, {}).ok).toBe(false);
  });
  it('a platform the journey does not declare, and an unknown journey id', () => {
    const f = good();
    expect(mutate(f, (r) => (r.selection.ids[0] = 'J-404'))).toContain('selection_invalid');
    expect(mutate(f, (r) => (r.selection.platform = 'beos'))).toContain('selection_invalid');
  });
});

// ---------------------------------------------------------------------------------------------------------------

describe('census', () => {
  it('counts Rust and JS tests and their tags', () => {
    const rust = `
// rk-test: pure
#[test]
fn a() {}

#[test]
fn b() {}

// rk-test: pin #12
#[tokio::test]
#[ignore]
fn c() {}

#[test] // rk-test: contract
fn d() {}

// rk-test: pin
#[test]
fn e() {}
`;
    const r = countTests(rust, 'rust');
    expect(r).toMatchObject({ tests: 5, tagged: 3, untagged: 2, invalidTag: 1 });
    expect(r.byClass).toEqual({ pure: 1, contract: 1, pin: 1 });
    const js = `
// rk-test: pure
it('a', () => {});
it('b', () => {});
  test.each([1])('c', () => {});
// rk-test: contract
test('d', () => {});
describe('not a test', () => {});
// rk-test: nonsense
it.skip('e', () => {});
`;
    const j = countTests(js, 'js');
    expect(j).toMatchObject({ tests: 5, tagged: 2, untagged: 3, invalidTag: 1 });
  });

  it('counts a tree, journeys and the ratio, skipping vendored and build directories', () => {
    const root = appRoot();
    fs.mkdirSync(path.join(root, 'src'));
    fs.mkdirSync(path.join(root, 'node_modules'));
    fs.mkdirSync(path.join(root, 'rk-snapshot'));
    fs.writeFileSync(path.join(root, 'src', 'a.rs'), '// rk-test: pure\n#[test]\nfn a(){}\n#[test]\nfn b(){}\n');
    fs.writeFileSync(path.join(root, 'src', 'a.test.ts'), "it('x', () => {});\n");
    fs.writeFileSync(path.join(root, 'node_modules', 'x.test.js'), "it('v', () => {});\n");
    fs.writeFileSync(path.join(root, 'rk-snapshot', 'y.rs'), '#[test]\nfn z(){}\n');
    const c = census(root);
    expect(c).toMatchObject({ tests: 3, tagged: 1, untagged: 2, journeys: 3, ratio: 1 });
    const none = tmp('empty');
    expect(census(none)).toMatchObject({ tests: 0, journeys: 0, ratio: null });
  });

  const gitOk = spawnSync('git', ['--version']).status === 0;
  it.skipIf(!gitOk)('diffs against a base ref through git', () => {
    const root = tmp('repo');
    const g = (...args: string[]): void => void spawnSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@t', '-c', 'commit.gpgsign=false', ...args], { cwd: root });
    g('init', '-q');
    fs.writeFileSync(path.join(root, 'a.rs'), '#[test]\nfn a(){}\n');
    g('add', '.');
    g('commit', '-q', '-m', 'base');
    fs.writeFileSync(path.join(root, 'a.rs'), '#[test]\nfn a(){}\n#[test]\nfn b(){}\n// rk-test: pure\n#[test]\nfn c(){}\n');
    fs.writeFileSync(path.join(root, 'new.test.ts'), "it('n', () => {});\n");
    const d = censusDiff(root, systemGit(root), 'HEAD');
    expect(d).not.toBeNull();
    expect(d!.added).toMatchObject({ tests: 3, untagged: 2, tagged: 1 });
    expect(d!.removed.tests).toBe(0);
    expect(d!.journeysBefore).toBeNull();
    expect(censusDiff(root, systemGit(root), 'no-such-ref')).toBeNull();
  });
});

// ---------------------------------------------------------------------------------------------------------------

describe('gate (advisory)', () => {
  const mapText = `version = 1
[[behavior]]
id = "B-sources"
title = "Sources"
paths = ["crates/engine/src/sources/**", "ui/src/sources/**"]
journeys = ["J-001", "J-002"]
[[behavior]]
id = "B-other"
title = "Other"
paths = ["crates/other/**"]
journeys = ["J-003"]
[[exempt]]
paths = ["docs/**", "**/*.md", "**/tests/**", "scripts/**"]
reason = "docs and test-only"
[global]
paths = ["Cargo.lock", "rightkit-qa.toml"]
`;
  const map = parseMap(mapText).value!;

  it('classifies mapped, exempt, unmapped, global and coverage files', () => {
    const cls = (p: string): string => classifyPath(map, p).class;
    expect(cls('crates/engine/src/sources/add.rs')).toBe('mapped');
    expect(classifyPath(map, 'ui/src/sources/List.tsx').behaviors).toEqual(['B-sources']);
    expect(cls('docs/guide.md')).toBe('exempt');
    expect(cls('crates/engine/tests/x.rs')).toBe('exempt');
    expect(cls('crates/engine/src/billing/pay.rs')).toBe('unmapped');
    expect(cls('Cargo.lock')).toBe('global');
    expect(cls('journeys.toml')).toBe('global');
    expect(cls('journeys-map.toml')).toBe('coverage-file');
    // an exempt glob never hides mapped product source
    expect(cls('crates/other/tests/x.rs')).toBe('mapped');
    expect(cls('.\\Cargo.lock')).toBe('global');
  });

  const manifest = parseManifest(MANIFEST).value!;
  const diffOf = (paths: string[], extra: Record<string, string | null> = {}): Git =>
    fakeGit({ 'diff --name-only HEAD': paths.join('\n'), 'ls-files --others --exclude-standard': '', 'show HEAD:journeys.toml': null, 'show HEAD:journeys-map.toml': null, ...extra });

  it('reports unmapped paths, required journeys and unknown coverage, and never throws', () => {
    const report = runGate({ root: '.', git: diffOf(['crates/engine/src/sources/a.rs', 'crates/engine/src/billing/x.rs', 'docs/a.md']), base: 'HEAD', manifest, map });
    expect(report.unmapped).toEqual(['crates/engine/src/billing/x.rs']);
    expect(report.required.map((r) => r.journey)).toEqual(['J-001', 'J-002']);
    expect(report.coverage.every((c) => c.status === 'unknown')).toBe(true);
    expect(report.findings.join('\n')).toMatch(/unmapped product path crates\/engine\/src\/billing\/x\.rs/);
    const broken = runGate({ root: '.', git: { run: () => null }, base: 'nope', manifest, map: null });
    expect(broken.findings.length).toBeGreaterThan(0);
  });

  it('a global path requires every mapped journey', () => {
    const report = runGate({ root: '.', git: diffOf(['Cargo.lock']), base: 'HEAD', manifest, map });
    expect(report.required.map((r) => r.journey)).toEqual(['J-001', 'J-002', 'J-003']);
  });

  const receiptFor = (tree: string, journey: string): Receipt =>
    ({ selection: { ids: [journey], platform: 'windows', tier: 'fast' }, source: { tree }, journeys: [{ id: journey, status: 'passed' }] }) as unknown as Receipt;

  it('a receipt for the claimed tree is valid; an older one is carried forward only over untouched paths', () => {
    const T0 = '1'.repeat(40);
    const claimed = 'b'.repeat(40);
    const base = { root: '.', base: 'HEAD', manifest, map, claimedTree: claimed } as const;
    const git = (changed: string[]): Git => diffOf(['ui/src/sources/a.tsx'], { [`diff --name-only ${T0} ${claimed}`]: changed.join('\n') });
    const row = (g: Git, r: Receipt) => runGate({ ...base, git: g, receipts: [r] }).coverage.find((c) => c.journey === 'J-001' && c.platform === 'windows')!;
    expect(row(git([]), receiptFor(claimed, 'J-001')).status).toBe('valid');
    expect(row(git(['docs/readme.md']), receiptFor(T0, 'J-001')).status).toBe('carried-forward');
    expect(row(git(['crates/engine/src/sources/x.rs']), receiptFor(T0, 'J-001')).status).toBe('stale');
    expect(row(git(['Cargo.lock']), receiptFor(T0, 'J-001')).status).toBe('stale');
    expect(row(git(['entries/J-001.scenario.toml']), receiptFor(T0, 'J-001')).status).toBe('stale');
    expect(row(git(['adapter/main.rs']), receiptFor(T0, 'J-001')).status).toBe('stale');
    expect(row(git(['journeys.toml']), receiptFor(T0, 'J-001')).status).toBe('stale');
    // a manifest whose adapter declares no sources can never carry a receipt forward
    const noSources = parseManifest(MANIFEST.replace('sources = ["adapter/**"]\n', '')).value!;
    const r = runGate({ ...base, manifest: noSources, git: git([]), receipts: [receiptFor(T0, 'J-001')] });
    expect(r.coverage.find((c) => c.journey === 'J-001' && c.platform === 'windows')!.status).toBe('stale');
  });

  it('demands a tombstone for each coverage reduction, bound to its exact change', () => {
    const baseM = manifest;
    const weaker = parseManifest(
      MANIFEST.replace('[[journey]]\nid = "J-003"', '[[journey]]\nid = "J-003"').replace(/\[\[journey\]\]\nid = "J-002"[\s\S]*?release_critical = true\n/, '').replace('release_critical = true', 'release_critical = false'),
    ).value!;
    const changes = coverageChanges(baseM, map, weaker, map);
    expect(changes.map((c) => `${c.id}:${c.kind}`)).toEqual(expect.arrayContaining(['J-002:removed', 'J-001:downgraded']));
    const removed = changes.find((c) => c.id === 'J-002')!;
    expect(removed.sha256).toBe(sha256Text(canonicalJson({ id: 'J-002', before: baseM.journeys[1], after: null })));
    const good = { id: 'J-002', kind: 'removed' as const, before_after_sha256: removed.sha256, approval: 'https://github.com/bogusyogi/x/issues/1#issuecomment-5', replacement: 'J-001', risk_accepted: false };
    expect(checkTombstones([removed], []).map((t) => t.status)).toEqual(['missing']);
    expect(checkTombstones([removed], [{ ...good, before_after_sha256: 'x' }]).map((t) => t.status)).toEqual(['hash-mismatch']);
    expect(checkTombstones([removed], [{ ...good, approval: 'ok' }]).map((t) => t.status)).toEqual(['approval-malformed']);
    expect(checkTombstones([removed], [{ ...good, replacement: null }]).map((t) => t.status)).toEqual(['unresolved']);
    expect(checkTombstones([removed], [good]).map((t) => t.status)).toEqual(['ok']);
  });

  it('flags platform loss, weakened fields, owner change and map shrinkage', () => {
    const platformLoss = parseManifest(MANIFEST.replace('platforms = ["macos", "windows", "linux"]', 'platforms = ["macos"]')).value!;
    expect(coverageChanges(manifest, map, platformLoss, map).map((c) => c.kind)).toContain('platform-reduced');
    const weakened = parseManifest(MANIFEST.replace('surface = "cli"', 'surface = "web"')).value!;
    expect(coverageChanges(manifest, map, weakened, map).map((c) => c.kind)).toContain('weakened');
    const owners = parseManifest(MANIFEST.replace('["bogusyogi"]', '["someone"]')).value!;
    expect(coverageChanges(manifest, map, owners, map).map((c) => c.id)).toContain('coverage_owners');
    const smaller = parseMap(mapText.replace('journeys = ["J-001", "J-002"]', 'journeys = ["J-001"]').replace('"scripts/**"]', '"scripts/**", "crates/**"]')).value!;
    const kinds = coverageChanges(manifest, map, manifest, smaller);
    expect(kinds.map((c) => c.kind)).toEqual(['map-shrunk', 'map-shrunk']);
    expect(coverageChanges(manifest, map, manifest, null).map((c) => c.id)).toContain('journeys-map');
    expect(coverageChanges(manifest, map, manifest, map)).toEqual([]);
  });
});
