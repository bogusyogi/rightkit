import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { appendFile, chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { admitClass1, CLASS1_CLONE_UNSUPPORTED, CLASS1_HELPER_UNTRUSTED, CLASS1_MESSAGE, CLASS1_UNSUPPORTED, cloneFileWithHelper, createClass1Platform, createCloneRoot, createMacosClass1Platform, createMacosClass1PlatformFromConfig, revalidateClone, seatbeltProfile, validateClosure, verifyCloneHelper } from './class1.mjs';
import { digestInputClosure, normalizeClosurePath } from './input-closure.mjs';

const isDarwin = process.platform === 'darwin';
const helperBin = process.env.RK_CLONEFILE_BIN;
async function dirs() {
  const base = await realpath(await mkdtemp(join(tmpdir(), 'rk-class1-')));
  const source = join(base, 'source'); const parent = join(base, 'builds');
  await mkdir(join(source, 'src'), { recursive: true }); await mkdir(parent, { recursive: true });
  await writeFile(join(source, 'Cargo.toml'), '[package]\nname = "x"\n');
  await writeFile(join(source, 'src', 'lib.rs'), 'pub fn a() {}\n');
  return { base, source, parent, close: () => rm(base, { recursive: true, force: true }) };
}
const FILES = ['Cargo.toml', 'src/lib.rs'];
const codeOf = (code) => (error) => error.code === code;

test('non-darwin hosts get the typed class1-unsupported default and fail closed', async () => {
  for (const platform of ['win32', 'linux']) {
    const p = createClass1Platform({ platform });
    assert.equal(p.supported, false);
    await assert.rejects(p.createProtectedBuildRoot({ sourceRoot: '/x', files: [], buildRootParent: '/y', id: 'a' }), (error) => codeOf(CLASS1_UNSUPPORTED)(error) && error.message.includes(CLASS1_MESSAGE) && error.platform === platform);
    await assert.rejects(p.spawnProtectedChild({ buildRoot: '/x', command: '/bin/true' }), codeOf(CLASS1_UNSUPPORTED));
  }
});

test('a Windows implementation registers through the same interface without changing the Mac path', async () => {
  const calls = [];
  const windows = { createProtectedBuildRoot: async (input) => { calls.push(['root', input.id]); return { root: 'C:\\clone', digest: 'd', files: [], release: async () => {} }; }, spawnProtectedChild: async () => { calls.push(['spawn']); return new EventEmitter(); }, protectedInvocation: async () => ({ command: 'x', args: [], cwd: 'C:\\clone' }) };
  const p = createClass1Platform({ platform: 'win32', registry: { win32: windows } });
  assert.equal(p.supported, true);
  await p.createProtectedBuildRoot({ id: 'w1' }); await p.spawnProtectedChild({});
  assert.deepEqual(calls, [['root', 'w1'], ['spawn']]);
  const incomplete = createClass1Platform({ platform: 'win32', registry: { win32: { createProtectedBuildRoot() {} } } });
  assert.equal(incomplete.supported, false, 'a partial implementation is not accepted');
});

test('admission is fail-closed: unsupported hosts, class-2 work and failing clones are all refused', async () => {
  const unsupported = createClass1Platform({ platform: 'win32' });
  const refused = await admitClass1({ platform: unsupported, evidenceClass: 'class1', input: {} });
  assert.equal(refused.admitted, false); assert.equal(refused.code, CLASS1_UNSUPPORTED); assert.equal(refused.reason, CLASS1_MESSAGE);
  const dev = await admitClass1({ platform: createClass1Platform({ platform: 'win32' }), evidenceClass: 'class2', input: {} });
  assert.equal(dev.admitted, false); assert.equal(dev.reason, 'not an evidence build');
  const failing = { supported: true, createProtectedBuildRoot: async () => { throw Object.assign(new Error('cross volume'), { code: 'RK_CLASS1_CROSS_VOLUME' }); } };
  const failed = await admitClass1({ platform: failing, evidenceClass: 'class1', input: {} });
  assert.equal(failed.admitted, false); assert.equal(failed.code, 'RK_CLASS1_CROSS_VOLUME');
  const admitted = await admitClass1({ platform: { supported: true, createProtectedBuildRoot: async () => ({ root: '/r' }) }, evidenceClass: 'class1', input: {} });
  assert.equal(admitted.admitted, true);
});

test('closure validation rejects symlinks that escape the source root and accepts in-root links', async () => {
  const d = await dirs();
  try {
    await symlink('/etc/hosts', join(d.source, 'escape.txt'));
    await assert.rejects(validateClosure({ sourceRoot: d.source, files: ['escape.txt'] }), codeOf('RK_CLASS1_SYMLINK_ESCAPE'));
    await rm(join(d.source, 'escape.txt'));
    await symlink('src/lib.rs', join(d.source, 'alias.rs'));
    assert.deepEqual(await validateClosure({ sourceRoot: d.source, files: ['alias.rs', 'Cargo.toml'] }), ['Cargo.toml', 'alias.rs']);
  } finally { await d.close(); }
});

test('closure never contains .git and rejects directories, missing inputs and relative roots', async () => {
  const d = await dirs();
  try {
    assert.throws(() => normalizeClosurePath('.git/HEAD'), codeOf('RK_INPUT_PATH_INVALID'));
    await assert.rejects(validateClosure({ sourceRoot: d.source, files: ['src'] }), codeOf('RK_CLASS1_UNSUPPORTED_INPUT'));
    await assert.rejects(validateClosure({ sourceRoot: d.source, files: ['nope.rs'] }), codeOf('RK_CLASS1_INPUT_MISSING'));
    await assert.rejects(validateClosure({ sourceRoot: 'relative', files: [] }), codeOf('RK_CLASS1_INVALID_ROOT'));
  } finally { await d.close(); }
});

test('clone refuses a build-root parent on another volume (clonefile needs one volume)', async () => {
  const d = await dirs();
  try {
    const fake = { stat: async (path) => ({ dev: path === d.parent ? 2 : 1 }) };
    await assert.rejects(createCloneRoot({ sourceRoot: d.source, files: FILES, buildRootParent: d.parent, id: 'b1', fs: fake }), codeOf('RK_CLASS1_CROSS_VOLUME'));
  } finally { await d.close(); }
});

test('clone fails closed when cloning is unsupported and leaves no partial root behind', async () => {
  const d = await dirs();
  try {
    const fs = { cloneFile: async () => { throw Object.assign(new Error('clonefile not supported'), { code: 'ENOTSUP' }); } };
    await assert.rejects(createCloneRoot({ sourceRoot: d.source, files: FILES, buildRootParent: d.parent, id: 'b2', fs }), codeOf('RK_CLASS1_CLONE_UNSUPPORTED'));
    await assert.rejects(stat(join(d.parent, 'b2')), (error) => error.code === 'ENOENT');
  } finally { await d.close(); }
});

test('a source that changes while cloning is refused (RK_CLASS1_INPUTS_MUTATED)', async () => {
  const d = await dirs();
  try {
    const fs = { cloneFile: async (from, to) => { await copyFile(from, to); await appendFile(join(d.source, 'Cargo.toml'), '# drift\n'); } };
    await assert.rejects(createCloneRoot({ sourceRoot: d.source, files: FILES, buildRootParent: d.parent, id: 'b3', fs }), codeOf('RK_CLASS1_INPUTS_MUTATED'));
  } finally { await d.close(); }
});

test('build-root ids must be plain names (no traversal into other directories)', async () => {
  const d = await dirs();
  try {
    await assert.rejects(createCloneRoot({ sourceRoot: d.source, files: FILES, buildRootParent: d.parent, id: '../escape' }), codeOf('RK_CLASS1_INVALID_ID'));
  } finally { await d.close(); }
});

test('seatbelt profile is a write allowlist: deny all writes, re-allow managed roots, deny the build root last', () => {
  const profile = seatbeltProfile({ buildRoot: '/Volumes/D/builds/b1', writableRoots: ['/Volumes/D/cache/target'], temporaryRoots: ['/var/folders/t'] });
  const lines = profile.trim().split('\n');
  assert.equal(lines[0], '(version 1)'); assert.equal(lines[1], '(allow default)'); assert.equal(lines[2], '(deny file-write*)');
  assert.match(lines[3], /\/dev\/null/);
  assert.equal(lines[4], '(allow file-write* (subpath "/Volumes/D/cache/target"))');
  assert.equal(lines[5], '(allow file-write* (subpath "/var/folders/t"))');
  assert.equal(lines.at(-1), '(deny file-write* (subpath "/Volumes/D/builds/b1"))');
  assert.throws(() => seatbeltProfile({ buildRoot: '/b', writableRoots: ['/b/target'] }), codeOf('RK_CLASS1_PROFILE_INVALID'));
  assert.throws(() => seatbeltProfile({ buildRoot: '/b', temporaryRoots: ['/b'] }), codeOf('RK_CLASS1_PROFILE_INVALID'));
  assert.throws(() => seatbeltProfile({ buildRoot: '/b"x' }), codeOf('RK_CLASS1_PROFILE_INVALID'));
  assert.throws(() => seatbeltProfile({ buildRoot: 'relative' }), codeOf('RK_CLASS1_PROFILE_INVALID'));
});

function fakeSpawn() { const calls = []; const spawn = (command, args, options) => { calls.push({ command, args, options }); return new EventEmitter(); }; return { calls, spawn }; }

test('children are spawned only under sandbox-exec with the deny profile, inside the clone', async () => {
  const { calls, spawn } = fakeSpawn();
  const p = createMacosClass1Platform({ spawn, exists: async () => true });
  await p.spawnProtectedChild({ buildRoot: '/var/x/b1', cwd: '/var/x/b1/src', command: '/usr/bin/cargo', args: ['build'], env: { A: '1' }, writableRoots: ['/var/x/target'] });
  assert.equal(calls[0].command, '/usr/bin/sandbox-exec');
  assert.deepEqual(calls[0].args.slice(0, 2), ['-p', seatbeltProfile({ buildRoot: '/var/x/b1', writableRoots: ['/var/x/target'] })]);
  assert.deepEqual(calls[0].args.slice(2), ['--', '/usr/bin/cargo', 'build']);
  assert.equal(calls[0].options.cwd, '/var/x/b1/src');
});

test('children are refused for relative commands, cwd outside the clone, or a missing sandbox', async () => {
  const { spawn } = fakeSpawn();
  const p = createMacosClass1Platform({ spawn, exists: async () => true });
  await assert.rejects(p.spawnProtectedChild({ buildRoot: '/var/x/b1', command: 'cargo' }), codeOf('RK_CLASS1_COMMAND_INVALID'));
  await assert.rejects(p.spawnProtectedChild({ buildRoot: '/var/x/b1', cwd: '/var/x/other', command: '/usr/bin/cargo' }), codeOf('RK_CLASS1_CWD_OUTSIDE'));
  const missing = createMacosClass1Platform({ spawn, exists: async () => false });
  await assert.rejects(missing.spawnProtectedChild({ buildRoot: '/var/x/b1', command: '/usr/bin/cargo' }), codeOf('RK_CLASS1_SANDBOX_MISSING'));
});

test('a clone whose bytes moved after admission is never built from (revalidated before spawn)', async () => {
  const d = await dirs();
  try {
    const { calls, spawn } = fakeSpawn();
    const p = createMacosClass1Platform({ spawn, exists: async () => true });
    const clone = await createCloneRootFake(d);
    // An agent with write access to its own clone file would have to chmod first; the digest still catches it.
    await chmod(join(clone.root, 'src', 'lib.rs'), 0o644);
    await writeFile(join(clone.root, 'src', 'lib.rs'), 'pub fn tampered() {}\n');
    await assert.rejects(p.spawnProtectedChild({ buildRoot: clone.root, clone, command: '/usr/bin/cargo' }), codeOf('RK_CLASS1_CLONE_MUTATED'));
    assert.equal(calls.length, 0, 'spawn must not run after a moved clone');
    assert.equal(await revalidateClone(await createCloneRootFake(d)), true);
  } finally { await d.close(); }
});

async function createCloneRootFake(d) {
  // Plain copy through the injected clone seam; the real clone primitive is covered by the helper test below.
  return createCloneRoot({ sourceRoot: d.source, files: FILES, buildRootParent: d.parent, id: `fake-${Math.random().toString(36).slice(2)}`, fs: { cloneFile: (from, to) => copyFile(from, to) } });
}

// Contract on this host: COPYFILE_FICLONE_FORCE either clones (APFS, same volume) or fails closed with
// RK_CLASS1_CLONE_UNSUPPORTED and leaves no residue. It must never fall back to a silent byte copy.
test('real clone primitive: clones and verifies, or fails closed with no residue (never a silent copy)', { skip: (!isDarwin || !helperBin) && 'darwin only; set RK_CLONEFILE_BIN to the built rightkit-clonefile' }, async (t) => {
  const d = await dirs();
  try {
    await mkdir(join(d.source, 'deep', 'er'), { recursive: true });
    await writeFile(join(d.source, 'deep', 'er', 'mod.rs'), 'mod x;\n');
    await symlink('src/lib.rs', join(d.source, 'alias.rs'));
    const files = ['Cargo.toml', 'src/lib.rs', 'deep/er/mod.rs', 'alias.rs'];
    const sourceDigest = (await digestInputClosure(d.source, files)).digest;
    const attempt = await createMacosClass1Platform({ fs: { cloneFile: cloneFileWithHelper(helperBin) } }).createProtectedBuildRoot({ sourceRoot: d.source, files, buildRootParent: d.parent, id: 'real-1' }).then((value) => ({ value }), (error) => ({ error }));
    if (attempt.error) {
      assert.equal(attempt.error.code, 'RK_CLASS1_CLONE_UNSUPPORTED', attempt.error.message);
      assert.deepEqual(await readdir(d.parent), [], 'a refused clone leaves no partial root');
      t.diagnostic(`clonefile unavailable to this Node on this host: ${attempt.error.message}`);
      return;
    }
    const clone = attempt.value;
    assert.equal(clone.digest, sourceDigest);
    assert.equal((await lstat(join(clone.root, 'alias.rs'))).isSymbolicLink(), true);
    assert.equal(await readFile(join(clone.root, 'deep', 'er', 'mod.rs'), 'utf8'), 'mod x;\n');
    await assert.rejects(writeFile(join(clone.root, 'src', 'lib.rs'), 'owner write'), (error) => error.code === 'EACCES');
    await clone.release();
    await assert.rejects(stat(clone.root), (error) => error.code === 'ENOENT');
  } finally { await d.close(); }
});

test('kernel write denial: a Seatbelt child cannot write into the clone but can write its managed target root', { skip: !isDarwin && 'darwin only' }, async () => {
  // Uses a plain copy through the seam: this test is about the kernel denial, not the clone primitive.
  const probe = spawnSync('/usr/bin/sandbox-exec', ['-p', '(version 1)(allow default)', '/usr/bin/true']);
  if (probe.status !== 0) return; // sandbox-exec unavailable in this session; the profile tests still pin the rules
  const d = await dirs();
  try {
    const clone = await createCloneRootFake(d);
    const target = join(d.base, 'target'); await mkdir(target);
    const profile = seatbeltProfile({ buildRoot: clone.root, writableRoots: [target] });
    const denied = spawnSync('/usr/bin/sandbox-exec', ['-p', profile, '--', '/bin/sh', '-c', `echo tampered > '${join(clone.root, 'Cargo.toml')}'`]);
    assert.notEqual(denied.status, 0, 'write into the build root must be denied');
    const outside = join(d.base, 'not-managed'); await mkdir(outside);
    const unlisted = spawnSync('/usr/bin/sandbox-exec', ['-p', profile, '--', '/bin/sh', '-c', `echo x > '${join(outside, 'f')}'`]);
    assert.notEqual(unlisted.status, 0, 'writes outside the managed roots must be denied');
    const devnull = spawnSync('/usr/bin/sandbox-exec', ['-p', profile, '--', '/bin/sh', '-c', 'echo ok > /dev/null']);
    assert.equal(devnull.status, 0, String(devnull.stderr));
    const allowed = spawnSync('/usr/bin/sandbox-exec', ['-p', profile, '--', '/bin/sh', '-c', `echo out > '${join(target, 'artifact')}'`]);
    assert.equal(allowed.status, 0, String(allowed.stderr));
    assert.equal(await readFile(join(target, 'artifact'), 'utf8'), 'out\n');
    assert.equal(await readFile(join(clone.root, 'Cargo.toml'), 'utf8'), '[package]\nname = "x"\n');
    await clone.release();
  } finally { await d.close(); }
});

test('clone seam: no clone primitive configured means no clone, no residue (fail closed, never a byte copy)', async () => {
  const d = await dirs();
  try {
    await assert.rejects(createCloneRoot({ sourceRoot: d.source, files: FILES, buildRootParent: d.parent, id: 'nohelper' }), codeOf(CLASS1_CLONE_UNSUPPORTED));
    await assert.rejects(stat(join(d.parent, 'nohelper')), (error) => error.code === 'ENOENT');
  } finally { await d.close(); }
});

test('helper exit statuses map to typed class-1 codes; a missing binary fails closed as unsupported', async () => {
  const cases = [
    [3, CLASS1_CLONE_UNSUPPORTED], [4, 'RK_CLASS1_CROSS_VOLUME'], [5, 'RK_CLASS1_CLONE_SYMLINK_REFUSED'], [6, 'RK_CLASS1_UNSUPPORTED_INPUT'], [1, 'RK_CLASS1_CLONE_FAILED'],
  ];
  for (const [status, code] of cases) {
    const run = async () => { throw Object.assign(new Error('exit'), { code: status, stderr: '{"ok":false,"code":"X"}\n' }); };
    await assert.rejects(cloneFileWithHelper('/opt/rk/runtime/bin/rightkit-clonefile', { run })('/a', '/b'), codeOf(code));
  }
  const missing = cloneFileWithHelper('/opt/rk/runtime/bin/rightkit-clonefile', { run: async () => { throw Object.assign(new Error('spawn ENOENT'), { code: 'ENOENT' }); } });
  await assert.rejects(missing('/a', '/b'), codeOf(CLASS1_CLONE_UNSUPPORTED));
  assert.throws(() => cloneFileWithHelper('rightkit-clonefile'), codeOf(CLASS1_CLONE_UNSUPPORTED), 'a relative helper path is refused');
});

test('the helper must sit inside the protected runtime and be root-owned and not writable by others', async () => {
  const d = await dirs();
  try {
    const runtime = join(d.base, 'runtime'); const bin = join(runtime, 'bin'); await mkdir(bin, { recursive: true });
    const helper = join(bin, 'rightkit-clonefile'); await writeFile(helper, '#!/bin/sh\n'); await chmod(helper, 0o755);
    const realRoot = await realpath(runtime);
    // Real stat (this test process is not root): ownership refusal is the proof the check runs.
    await assert.rejects(verifyCloneHelper({ helper, protectedRoot: runtime }), codeOf(CLASS1_HELPER_UNTRUSTED));
    // Injected owner view: root-owned everywhere is accepted.
    const rootOwned = async (path) => { const info = await stat(path); return { isFile: () => info.isFile(), mode: info.mode, uid: 0 }; };
    assert.equal(await verifyCloneHelper({ helper, protectedRoot: runtime, stat: rootOwned }), await realpath(helper));
    // A helper outside the protected runtime is refused even when root-owned.
    const outside = join(d.base, 'rightkit-clonefile'); await writeFile(outside, 'x'); await chmod(outside, 0o755);
    await assert.rejects(verifyCloneHelper({ helper: outside, protectedRoot: runtime, stat: rootOwned }), codeOf(CLASS1_HELPER_UNTRUSTED));
    // A group-writable helper is refused.
    await chmod(helper, 0o775);
    await assert.rejects(verifyCloneHelper({ helper, protectedRoot: runtime, stat: rootOwned }), codeOf(CLASS1_HELPER_UNTRUSTED));
    await chmod(helper, 0o755);
    assert.ok(realRoot);
    // A missing helper is unsupported, never a fallback.
    await assert.rejects(verifyCloneHelper({ helper: join(bin, 'absent'), protectedRoot: runtime, stat: rootOwned }), codeOf(CLASS1_CLONE_UNSUPPORTED));
  } finally { await d.close(); }
});

test('config-built darwin platform records why it is unsupported and admission reports that code', async () => {
  const missing = await createMacosClass1PlatformFromConfig({ helper: null, protectedRoot: null });
  assert.equal(missing.supported, false);
  const refused = await admitClass1({ platform: missing, evidenceClass: 'class1', input: {} });
  assert.equal(refused.admitted, false); assert.equal(refused.code, CLASS1_CLONE_UNSUPPORTED);
  const untrusted = await createMacosClass1PlatformFromConfig({ helper: '/tmp/no/rightkit-clonefile', protectedRoot: '/tmp/no' });
  const refusedUntrusted = await admitClass1({ platform: untrusted, evidenceClass: 'class1', input: {} });
  assert.equal(refusedUntrusted.admitted, false); assert.equal(refusedUntrusted.code, CLASS1_CLONE_UNSUPPORTED);
});

test('protected invocation is the Seatbelt argv for a clone-rooted command and never spawns on its own', async () => {
  const { calls, spawn } = fakeSpawn();
  const p = createMacosClass1Platform({ spawn, exists: async () => true });
  const inv = await p.protectedInvocation({ buildRoot: '/var/x/b1', command: '/usr/bin/cargo', args: ['check'], cwd: '/var/x/b1/crate', writableRoots: ['/var/x/target'] });
  assert.equal(inv.command, '/usr/bin/sandbox-exec'); assert.equal(inv.cwd, '/var/x/b1/crate'); assert.deepEqual(inv.args.slice(2), ['--', '/usr/bin/cargo', 'check']);
  assert.equal(calls.length, 0, 'protectedInvocation must not spawn');
  assert.equal(p.protection.mechanism, 'apfs-clonefile+seatbelt'); assert.equal(p.protection.fallbackCopy, false);
});
