import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { pathToFileURL } from 'node:url';
import { HOOKS, readActiveDigest, resolveHookTarget, runHookLauncher } from './hook-launcher.mjs';

const digest = 'a'.repeat(64);
const nextDigest = 'b'.repeat(64);
const prefix = '[HOOK:rightkit-policy-launcher:unavailable]';

async function withRoot(run) {
  const root = realpathSync.native(mkdtempSync(path.join(tmpdir(), 'rk-launcher-')));
  try { return await run(root); } finally { rmSync(root, { recursive: true, force: true }); }
}

function writeContent(root, hash, source = '// probe\n') {
  const targets = {};
  for (const [hook, { target }] of Object.entries(HOOKS)) {
    const file = path.join(root, `content-${hash}`, ...target.split('/'));
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, source);
    targets[hook] = file;
  }
  return targets;
}
const writePointer = (root, value) => writeFileSync(path.join(root, 'active.json'), typeof value === 'string' ? value : JSON.stringify(value));

// stdin and stdout are getters that throw: the launcher must leave both to the target script.
function fakeProcess(args = ['--flag', 'value']) {
  const written = [];
  const proc = {
    argv: ['/opt/node/bin/node', '/policy/launch/write-hook.mjs', ...args],
    exitCode: undefined,
    stderr: { write: (text) => { written.push(text); return true; } },
    get stdin() { throw new Error('launcher touched stdin'); },
    get stdout() { throw new Error('launcher touched stdout'); },
  };
  return { proc, written };
}

test('launcher imports the active content script in-process and passes argv, stdin, stdout and the exit code through', async () => {
  await withRoot(async (root) => {
    const targets = writeContent(root, digest);
    writePointer(root, { schemaVersion: 1, digest });
    const { proc, written } = fakeProcess();
    const imports = [];
    // Stands in for the hook script: it sees the rewritten argv and owns the exit code, exactly as when run directly.
    const importModule = async (url) => { imports.push({ url, argv1: proc.argv[1] }); proc.exitCode = 2; };
    await runHookLauncher('write', { root, proc, importModule });
    assert.deepEqual(imports, [{ url: pathToFileURL(targets.write).href, argv1: targets.write }]);
    assert.deepEqual(proc.argv, ['/opt/node/bin/node', targets.write, '--flag', 'value']);
    assert.equal(proc.exitCode, 2);
    assert.deepEqual(written, []);
  });
});

test('launcher leaves a zero exit code and silent output alone', async () => {
  await withRoot(async (root) => {
    writeContent(root, digest);
    writePointer(root, { schemaVersion: 1, digest });
    const { proc, written } = fakeProcess([]);
    await runHookLauncher('session', { root, proc, importModule: async () => {} });
    assert.equal(proc.exitCode, undefined);
    assert.deepEqual(written, []);
  });
});

test('each hook resolves its own fixed script and a moved pointer selects the new content without any other change', async () => {
  await withRoot(async (root) => {
    const first = writeContent(root, digest);
    const second = writeContent(root, nextDigest);
    writePointer(root, { schemaVersion: 1, digest });
    assert.equal((await resolveHookTarget('write', root)).target, first.write);
    assert.equal((await resolveHookTarget('session', root)).target, first.session);
    writePointer(root, { schemaVersion: 1, digest: nextDigest });
    assert.deepEqual(await resolveHookTarget('write', root), { digest: nextDigest, target: second.write });
    assert.equal((await resolveHookTarget('session', root)).target, second.session);
  });
});

test('a real dynamic import runs the target module from the content directory', async () => {
  await withRoot(async (root) => {
    const targets = writeContent(root, digest, 'globalThis.__rkLauncherProbe = import.meta.url;\n');
    writePointer(root, { schemaVersion: 1, digest });
    const { proc } = fakeProcess([]);
    try {
      await runHookLauncher('write', { root, proc });
      assert.equal(globalThis.__rkLauncherProbe, pathToFileURL(targets.write).href);
    } finally { delete globalThis.__rkLauncherProbe; }
  });
});

test('an error thrown by the target propagates untouched', async () => {
  await withRoot(async (root) => {
    writeContent(root, digest);
    writePointer(root, { schemaVersion: 1, digest });
    const { proc, written } = fakeProcess();
    await assert.rejects(runHookLauncher('write', { root, proc, importModule: async () => { throw new Error('target failed'); } }), /target failed/);
    assert.equal(proc.exitCode, undefined);
    assert.deepEqual(written, []);
  });
});

// Missing, malformed or unusable installs take the same path as `node <missing script>` did: nothing on stdout, one
// stderr line, exit 1, which Claude Code and Codex treat as a non-blocking hook error. The target is never imported.
const unusable = [
  ['pointer-missing', 'no active.json', () => {}],
  ['pointer-unreadable', 'active.json is a directory', (root) => mkdirSync(path.join(root, 'active.json'))],
  ['pointer-malformed', 'not JSON', (root) => writePointer(root, '{ digest:')],
  ['pointer-malformed', 'empty file', (root) => writePointer(root, '')],
  ['pointer-malformed', 'JSON array', (root) => writePointer(root, [digest])],
  ['pointer-malformed', 'JSON null', (root) => writePointer(root, 'null')],
  ['pointer-malformed', 'unknown schema version', (root) => writePointer(root, { schemaVersion: 2, digest })],
  ['pointer-malformed', 'missing schema version', (root) => writePointer(root, { digest })],
  ['pointer-malformed', 'digest is not a string', (root) => writePointer(root, { schemaVersion: 1, digest: 7 })],
  ['pointer-malformed', 'oversized pointer', (root) => writePointer(root, JSON.stringify({ schemaVersion: 1, digest, pad: 'x'.repeat(8192) }))],
  ['pointer-digest-invalid', 'uppercase digest', (root) => writePointer(root, { schemaVersion: 1, digest: digest.toUpperCase() })],
  ['pointer-digest-invalid', 'short digest', (root) => writePointer(root, { schemaVersion: 1, digest: digest.slice(1) })],
  ['pointer-digest-invalid', 'long digest', (root) => writePointer(root, { schemaVersion: 1, digest: `${digest}0` })],
  ['pointer-digest-invalid', 'trailing newline', (root) => writePointer(root, { schemaVersion: 1, digest: `${digest}\n` })],
  ['pointer-digest-invalid', 'relative path above the root', (root) => writePointer(root, { schemaVersion: 1, digest: '../../../../etc' })],
  ['pointer-digest-invalid', 'path-separator digest', (root) => writePointer(root, { schemaVersion: 1, digest: `${'a'.repeat(30)}/../${'a'.repeat(30)}` })],
  ['pointer-digest-invalid', 'absolute path', (root) => writePointer(root, { schemaVersion: 1, digest: path.join(root, 'content-x') })],
  ['target-missing', 'pointer names content that is not installed', (root) => writePointer(root, { schemaVersion: 1, digest: nextDigest })],
  ['target-missing', 'target is a directory', (root) => {
    mkdirSync(path.join(root, `content-${digest}`, ...HOOKS.write.target.split('/')), { recursive: true });
    writePointer(root, { schemaVersion: 1, digest });
  }, false],
];
for (const [code, label, arrange, installContent = true] of unusable) {
  test(`launcher refuses with ${code} (${label}) and exits 1 without importing or printing to stdout`, async () => {
    await withRoot(async (root) => {
      if (installContent) writeContent(root, digest);
      arrange(root);
      const { proc, written } = fakeProcess();
      const argv = [...proc.argv];
      let imported = 0;
      await runHookLauncher('write', { root, proc, importModule: async () => { imported += 1; } });
      assert.equal(imported, 0);
      assert.equal(proc.exitCode, 1);
      assert.deepEqual(proc.argv, argv);
      assert.equal(written.length, 1);
      assert.ok(written[0].startsWith(`${prefix} ${code}:`), written[0]);
      assert.ok(written[0].endsWith('\n'));
    });
  });
}

test('launcher refuses a missing policy root, an unknown hook and a copy that is not installed under launch/', async () => {
  await withRoot(async (root) => {
    const cases = [
      [{ root: path.join(root, 'absent') }, 'write', 'root-missing'],
      [{ root }, 'bogus', 'unknown-hook'],
      [{}, 'write', 'not-installed'], // this source copy is not inside a launch/ directory
    ];
    for (const [deps, hook, code] of cases) {
      const { proc, written } = fakeProcess();
      let imported = 0;
      await runHookLauncher(hook, { ...deps, proc, importModule: async () => { imported += 1; } });
      assert.equal(imported, 0, code);
      assert.equal(proc.exitCode, 1, code);
      assert.ok(written[0].startsWith(`${prefix} ${code}:`), written[0]);
    }
  });
});

test('a content directory that resolves outside the root is refused', async (t) => {
  await withRoot(async (base) => {
    const root = path.join(base, 'root');
    const outside = path.join(base, 'outside');
    mkdirSync(root, { recursive: true });
    writeContent(outside, digest);
    try { symlinkSync(path.join(outside, `content-${digest}`), path.join(root, `content-${digest}`), 'junction'); }
    catch (error) { t.skip(`cannot create a directory link here: ${error.code}`); return; }
    writePointer(root, { schemaVersion: 1, digest });
    await assert.rejects(resolveHookTarget('write', root), (error) => error.code === 'target-outside-root');
    const { proc, written } = fakeProcess();
    let imported = 0;
    await runHookLauncher('write', { root, proc, importModule: async () => { imported += 1; } });
    assert.equal(imported, 0);
    assert.equal(proc.exitCode, 1);
    assert.ok(written[0].startsWith(`${prefix} target-outside-root:`), written[0]);
  });
});

test('readActiveDigest returns exactly the pointer digest', async () => {
  await withRoot(async (root) => {
    writePointer(root, { schemaVersion: 1, digest, note: 'extra fields are ignored' });
    assert.equal(await readActiveDigest(root), digest);
  });
});
