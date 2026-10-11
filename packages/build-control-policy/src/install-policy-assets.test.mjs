import assert from 'node:assert/strict';
import { appendFileSync, cpSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { HOOKS } from './hook-launcher.mjs';
import { installPolicyAssets } from './install-policy-assets.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));

async function withTemp(run) {
  const base = realpathSync.native(mkdtempSync(path.join(tmpdir(), 'rk-policy-pointer-')));
  try { return await run(base); } finally { rmSync(base, { recursive: true, force: true }); }
}

/** A copy of this package's src/ that installs as a different policy digest when `activation` differs. */
function sourceCopy(base, name, activation = '') {
  const sourceRoot = path.join(base, name);
  cpSync(here, path.join(sourceRoot, 'src'), { recursive: true });
  if (activation) appendFileSync(path.join(sourceRoot, 'src', 'rightkit-drift.mjs'), `\n// ${activation}\n`);
  return sourceRoot;
}

const bytesOf = (file) => readFileSync(file);
const pointerOf = (root) => JSON.parse(readFileSync(path.join(root, 'active.json'), 'utf8'));

test('activating a different policy digest moves only the pointer; launcher files are identical and untouched', async () => {
  await withTemp(async (base) => {
    const root = path.join(base, 'agent-policy');
    const first = await installPolicyAssets({ root, sourceRoot: sourceCopy(base, 'one', 'activation one') });
    const launcherFiles = readdirSync(first.launchDir).sort();
    assert.deepEqual(launcherFiles, ['hook-launcher.mjs', 'session-hook.mjs', 'write-hook.mjs']);
    assert.deepEqual([...first.updatedLaunchers].sort(), launcherFiles);
    assert.equal(first.pointerUpdated, true);
    assert.deepEqual(pointerOf(root), { schemaVersion: 1, digest: first.digest });
    const before = Object.fromEntries(launcherFiles.map((name) => [name, { bytes: bytesOf(path.join(first.launchDir, name)), ino: statSync(path.join(first.launchDir, name)).ino }]));

    const second = await installPolicyAssets({ root, sourceRoot: sourceCopy(base, 'two', 'activation two') });
    assert.notEqual(second.digest, first.digest);
    assert.notEqual(second.installRoot, first.installRoot);
    assert.equal(second.writeLauncher, first.writeLauncher);
    assert.equal(second.sessionLauncher, first.sessionLauncher);
    assert.deepEqual(second.updatedLaunchers, []);
    assert.equal(second.pointerUpdated, true);
    assert.deepEqual(pointerOf(root), { schemaVersion: 1, digest: second.digest });
    for (const name of launcherFiles) {
      const file = path.join(second.launchDir, name);
      assert.ok(bytesOf(file).equals(before[name].bytes), `${name} bytes changed`);
      assert.equal(statSync(file).ino, before[name].ino, `${name} was rewritten`);
      assert.ok(!bytesOf(file).includes(first.digest) && !bytesOf(file).includes(second.digest), `${name} embeds a policy digest`);
    }
    // The superseded content-addressed directory stays in place.
    assert.ok(statSync(path.join(first.installRoot, 'manifest.json')).isFile());
    assert.ok(statSync(path.join(second.installRoot, 'manifest.json')).isFile());
    assert.deepEqual(readdirSync(root).sort(), ['active.json', `content-${first.digest}`, `content-${second.digest}`, 'launch'].sort());
  });
});

test('reinstalling the same policy rewrites neither the launchers nor the pointer', async () => {
  await withTemp(async (base) => {
    const root = path.join(base, 'agent-policy');
    const sourceRoot = sourceCopy(base, 'one', 'same');
    const first = await installPolicyAssets({ root, sourceRoot });
    const files = [first.pointerPath, first.writeLauncher, first.sessionLauncher, path.join(first.launchDir, 'hook-launcher.mjs')];
    const inodes = files.map((file) => statSync(file).ino);
    const again = await installPolicyAssets({ root, sourceRoot });
    assert.equal(again.digest, first.digest);
    assert.deepEqual(again.updatedLaunchers, []);
    assert.equal(again.pointerUpdated, false);
    assert.deepEqual(files.map((file) => statSync(file).ino), inodes);
  });
});

test('launcher bytes: core is a byte copy of the source, stubs name their hook and import the core', async () => {
  await withTemp(async (base) => {
    const installed = await installPolicyAssets({ root: path.join(base, 'agent-policy'), sourceRoot: sourceCopy(base, 'one') });
    assert.ok(bytesOf(path.join(installed.launchDir, 'hook-launcher.mjs')).equals(bytesOf(path.join(here, 'hook-launcher.mjs'))));
    for (const [hook, { launcher }] of Object.entries(HOOKS)) {
      const stub = readFileSync(path.join(installed.launchDir, launcher), 'utf8');
      assert.ok(stub.includes("from './hook-launcher.mjs'"), launcher);
      assert.ok(stub.includes(`runHookLauncher(${JSON.stringify(hook)})`), launcher);
      assert.ok(!stub.includes('content-'), launcher);
    }
  });
});

test('a stale or missing launcher is replaced; nothing else in launch/ changes', async () => {
  await withTemp(async (base) => {
    const root = path.join(base, 'agent-policy');
    const sourceRoot = sourceCopy(base, 'one');
    const first = await installPolicyAssets({ root, sourceRoot });
    appendFileSync(first.writeLauncher, '// tampered\n');
    rmSync(first.sessionLauncher);
    const again = await installPolicyAssets({ root, sourceRoot });
    assert.deepEqual([...again.updatedLaunchers].sort(), ['session-hook.mjs', 'write-hook.mjs']);
    assert.equal(again.pointerUpdated, false);
    assert.ok(!readFileSync(first.writeLauncher, 'utf8').includes('tampered'));
  });
});

test('the pointer does not move when the new policy cannot be installed', async () => {
  await withTemp(async (base) => {
    const root = path.join(base, 'agent-policy');
    const first = await installPolicyAssets({ root, sourceRoot: sourceCopy(base, 'one', 'activation one') });
    const broken = sourceCopy(base, 'two', 'activation two');
    rmSync(path.join(broken, 'src', 'ownership-source.mjs'));
    await assert.rejects(installPolicyAssets({ root, sourceRoot: broken }), { code: 'ENOENT' });
    assert.deepEqual(pointerOf(root), { schemaVersion: 1, digest: first.digest });
  });
});

test('installed launchers locate the root from their own location and run the active hook script', async () => {
  await withTemp(async (base) => {
    const root = path.join(base, 'agent-policy');
    const first = await installPolicyAssets({ root, sourceRoot: sourceCopy(base, 'one', 'activation one') });
    const core = await import(pathToFileURL(path.join(first.launchDir, 'hook-launcher.mjs')).href);
    const run = async () => {
      const imported = [];
      const proc = { argv: ['node', first.writeLauncher, 'arg'], exitCode: undefined, stderr: { write: () => true } };
      await core.runHookLauncher('write', { proc, importModule: async (url) => { imported.push(url); } });
      return { imported, proc };
    };
    const one = await run();
    assert.deepEqual(one.imported, [pathToFileURL(realpathSync(first.writeHook)).href]);
    assert.deepEqual(one.proc.argv.slice(2), ['arg']);

    const second = await installPolicyAssets({ root, sourceRoot: sourceCopy(base, 'two', 'activation two') });
    const two = await run();
    assert.deepEqual(two.imported, [pathToFileURL(realpathSync(second.writeHook)).href]);
    assert.notDeepEqual(two.imported, one.imported);
  });
});
