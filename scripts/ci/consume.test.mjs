import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { consumeNpm, consumeNpmPortability, declaredEntries, importClosureGroups, installedImportClosure } from './consume.mjs';

test('logs retains generic portability failures even when named recipe reports success', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-consume-test-'));
  const unit = { name: '@rightkit/logs', version: '0.1.6', file: 'logs.tgz', sha256: 'a'.repeat(64), size: 100 };
  let invoked = false;
  try {
    const rows = consumeNpm({ units: [unit], packDir: scratchRoot, scratchRoot, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 1, stderr: 'fixture portability failure' }), npmConsumer: () => { invoked = true; return { name: unit.name, version: unit.version, archiveSha256: unit.sha256, archiveSize: unit.size, qualification: 'R2a', verified: true }; } });
    const row = rows.get(unit.name);
    assert.equal(invoked, true); assert.equal(row.report.verified, false); assert.ok(row.report.portability); assert.ok(row.problems.some(problem => problem.includes('fixture portability failure')));
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});
test('generic npm portability never becomes PK R2a & never invokes named logs runner', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-consume-test-'));
  try {
    const rows = consumeNpm({ units: [{ name: '@rightkit/other', version: '1.0.0', file: 'other.tgz' }], packDir: scratchRoot, scratchRoot, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 1, stderr: 'fixture' }), npmConsumer: () => { throw new Error('must not invoke'); } });
    assert.equal(rows.get('@rightkit/other').report.qualification, undefined);
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});

const settingsName = '@rightkit/settings-ui';
const recipeInventory = () => JSON.parse(fs.readFileSync(new URL('../../ci/consumer-recipes.json', import.meta.url), 'utf8'));
const settingsManifest = () => JSON.parse(fs.readFileSync(new URL('../../packages/settings-ui/package.json', import.meta.url), 'utf8'));

test('reviewed import maps cover every runtime export once & reject missing or foreign pins', () => {
  const entry = recipeInventory().npm[settingsName];
  const declared = declaredEntries(settingsManifest());
  const groups = importClosureGroups(entry, settingsName, declared);
  assert.deepEqual(groups.flatMap(group => group.subpaths).sort(), declared.subpaths.map(item => item.subpath).sort());
  assert.equal(groups.length, 4);
  const minimal = groups.find(group => group.subpaths.includes('./core'));
  assert.deepEqual(minimal.install, { react: '19.3.1' });
  assert.ok(minimal.subpaths.includes('./headless'));
  const missing = structuredClone(entry); delete missing.importClosures['./core'];
  assert.throws(() => importClosureGroups(missing, settingsName, declared), /cover reviewed surface/);
  const foreign = structuredClone(entry); foreign.importClosures['./core'] = { react: '^19' };
  assert.throws(() => importClosureGroups(foreign, settingsName, declared), /outside reviewed closure/);
  const added = structuredClone(declared); added.subpaths.push({ subpath: './unreviewed', runtime: ['extra.js'] });
  assert.throws(() => importClosureGroups(entry, settingsName, added), /missing runtime export/);
});

test('distinct maps get isolated installs & missing peer import remains fatal', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-import-closure-test-'));
  const pkg = settingsManifest();
  const installs = [];
  try {
    const spawn = (command, args, options) => {
      if (command === 'tar') return { status: 0, stdout: Buffer.from(JSON.stringify(pkg)) };
      if (command === 'fixture-npm') {
        installs.push({ cwd: options.cwd, args });
        const dir = path.join(options.cwd, 'node_modules', '@rightkit', 'settings-ui');
        fs.mkdirSync(dir, { recursive: true });
        fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify(pkg));
        for (const file of declaredEntries(pkg).files) {
          fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
          fs.writeFileSync(path.join(dir, file), '');
        }
        for (const arg of args.filter(arg => /@\d/.test(arg))) {
          const split = arg.lastIndexOf('@'); const name = arg.slice(0, split); const version = arg.slice(split + 1);
          const dep = path.join(options.cwd, 'node_modules', ...name.split('/'));
          fs.mkdirSync(dep, { recursive: true }); fs.writeFileSync(path.join(dep, 'package.json'), JSON.stringify({ name, version }));
        }
        return { status: 0 };
      }
      const plan = JSON.parse(fs.readFileSync(args.at(-1), 'utf8'));
      const rows = plan.items.map(item => item.subpath === './core'
        ? { spec: item.spec, ok: false, imported: false, error: "ERR_MODULE_NOT_FOUND: Cannot find package '@rightkit/license'" }
        : { spec: item.spec, ok: true, imported: item.import && item.subpath !== './settings.css' });
      return { status: 0, stdout: 'RK_CONSUME ' + JSON.stringify(rows) + '\n' };
    };
    const result = consumeNpmPortability({ units: [{ name: settingsName, version: pkg.version, file: 'settings.tgz' }], packDir: scratchRoot, scratchRoot, spawn, npm: { command: 'fixture-npm', prefix: [], shell: false } }).get(settingsName);
    assert.equal(installs.length, 4);
    assert.equal(new Set(installs.map(row => row.cwd)).size, 4);
    const minimal = installs.find(row => row.args.includes('react@19.3.1') && !row.args.some(arg => arg.startsWith('@rightkit/')));
    assert.ok(minimal);
    assert.ok(installs.every(row => row.args.includes('--legacy-peer-deps')));
    assert.ok(result.problems.some(problem => problem.includes('ERR_MODULE_NOT_FOUND') && problem.includes('@rightkit/license')));
    assert.equal(result.report.entries, 7);
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});

test('installed closure rejects ambient omitted peers & exact-pin drift', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-import-absence-test-'));
  const dir = path.join(scratch, 'node_modules', '@rightkit', 'settings-ui');
  try {
    fs.mkdirSync(dir, { recursive: true });
    const react = path.join(scratch, 'node_modules', 'react'); fs.mkdirSync(react);
    fs.writeFileSync(path.join(react, 'package.json'), JSON.stringify({ name: 'react', version: '0.0.0' }));
    fs.mkdirSync(path.join(scratch, 'node_modules', '@rightkit', 'license'));
    const result = installedImportClosure({ unit: { name: settingsName, version: '0.7.0', file: 'settings.tgz', sha256: 'a'.repeat(64) }, pkg: settingsManifest(), dir, scratch, installPins: { react: '19.3.1' } });
    assert.ok(result.problems.some(problem => problem.includes('react@19.3.1') && problem.includes('installed react@0.0.0')));
    assert.ok(result.problems.some(problem => problem.includes('unselected peer @rightkit/license')));
    assert.ok(result.evidence.absentPeers.includes('@rightkit/theme'));
    assert.equal(result.evidence.archive.sha256, 'a'.repeat(64));
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
});

test('reviewed closures import legacy-skipped JS exports & retain import and metadata failures', () => {
  const name = '@rightkit/tauri';
  const packed = JSON.parse(fs.readFileSync(new URL('../../packages/tauri/package.json', import.meta.url), 'utf8'));
  const entry = recipeInventory().npm[name];
  assert.deepEqual(entry.importClosures['.'], { '@rightkit/app-update': '0.4.0', '@rightkit/logs': '0.1.6' });
  assert.deepEqual(entry.importClosures['./dialog'], { ...entry.importClosures['.'], '@tauri-apps/plugin-dialog': '2.8.3' });
  const run = ({ reviewed = true, failDialog = false, malformedSkip = false } = {}) => {
    const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-all-imports-test-'));
    const pkg = structuredClone(packed);
    if (malformedSkip) pkg.rightkit.publishCheck.skipImport = './dialog';
    const reviewedEntry = structuredClone(entry);
    if (!reviewed) delete reviewedEntry.importClosures;
    const plans = [];
    try {
      fs.mkdirSync(path.join(scratchRoot, 'ci'));
      fs.writeFileSync(path.join(scratchRoot, 'ci', 'consumer-recipes.json'), JSON.stringify({ npm: { [name]: reviewedEntry } }));
      const spawn = (command, args, options) => {
        if (command === 'tar') return { status: 0, stdout: Buffer.from(JSON.stringify(pkg)) };
        if (command === 'fixture-npm') {
          const dir = path.join(options.cwd, 'node_modules', '@rightkit', 'tauri');
          fs.mkdirSync(dir, { recursive: true });
          fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify(pkg));
          for (const file of declaredEntries(pkg).files) {
            fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
            fs.writeFileSync(path.join(dir, file), '');
          }
          for (const arg of args.filter(arg => /@\d/.test(arg))) {
            const split = arg.lastIndexOf('@'); const depName = arg.slice(0, split); const version = arg.slice(split + 1);
            const dep = path.join(options.cwd, 'node_modules', ...depName.split('/'));
            fs.mkdirSync(dep, { recursive: true });
            fs.writeFileSync(path.join(dep, 'package.json'), JSON.stringify({ name: depName, version }));
          }
          return { status: 0 };
        }
        const plan = JSON.parse(fs.readFileSync(args.at(-1), 'utf8'));
        plans.push(...plan.items);
        return { status: 0, stdout: 'RK_CONSUME ' + JSON.stringify(plan.items.map(item => failDialog && item.subpath === './dialog' && item.import
          ? { spec: item.spec, ok: false, imported: false, error: 'dialog fixture import failed' }
          : { spec: item.spec, ok: true, imported: item.import })) + '\n' };
      };
      const result = consumeNpmPortability({ units: [{ name, version: pkg.version, file: 'tauri.tgz' }], packDir: scratchRoot, scratchRoot, root: scratchRoot, spawn, npm: { command: 'fixture-npm', prefix: [], shell: false } }).get(name);
      return { result, plans };
    } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
  };
  const reviewed = run();
  assert.deepEqual(reviewed.result.problems, []);
  assert.ok(reviewed.plans.every(item => item.import));
  assert.deepEqual(reviewed.result.report.imported, [name, `${name}/dialog`]);
  assert.deepEqual(reviewed.result.report.resolvedOnly, []);
  assert.deepEqual(reviewed.result.report.skippedImport, []);
  assert.ok(run({ failDialog: true }).result.problems.some(problem => problem.includes('dialog fixture import failed')));
  assert.ok(run({ malformedSkip: true }).result.problems.some(problem => problem.includes('skipImport must be an array')));
  const legacy = run({ reviewed: false });
  assert.equal(legacy.plans.find(item => item.subpath === './dialog').import, false);
  assert.deepEqual(legacy.result.report.resolvedOnly, [`${name}/dialog`]);
  assert.deepEqual(legacy.result.report.skippedImport, ['./dialog']);
  assert.deepEqual(packed.rightkit.publishCheck.skipImport, ['./dialog']);
});
