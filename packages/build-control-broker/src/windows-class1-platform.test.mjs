import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, win32 } from 'node:path';
import test from 'node:test';
import { createWindowsClass1Platform } from './windows-class1-platform.mjs';

const BUILD_ROOT = 'C:\\rk\\builds\\class1-abc';
const COMMAND = 'C:\\Tools\\node\\node.exe';

async function fixture() {
  const dir = await mkdtemp(join(tmpdir(), 'rk-win-class1-'));
  const sandboxExec = join(dir, 'rightkit-sandbox-exec.exe');
  await writeFile(sandboxExec, 'stub');
  return { dir, sandboxExec, close: () => rm(dir, { recursive: true, force: true }) };
}

function recordingSpawn() {
  const calls = [];
  const spawn = (command, args, options) => {
    calls.push({ command, args, options });
    return { command, args, options };
  };
  return { spawn, calls };
}

const codeOf = (code) => (error) => error.code === code;

test('invocation shape: read-only build root, writable grants, cwd, --env, then the command', async () => {
  const f = await fixture();
  try {
    const platform = createWindowsClass1Platform({ sandboxExec: f.sandboxExec });
    const writable = 'C:\\rk\\scratch';
    const invocation = await platform.protectedInvocation({
      buildRoot: BUILD_ROOT,
      command: COMMAND,
      args: ['--version', 'a b'],
      cwd: `${BUILD_ROOT}\\src`,
      env: { PATH: 'C:\\Tools', CI: '1' },
      writableRoots: [writable],
    });
    assert.equal(invocation.command, f.sandboxExec);
    assert.deepEqual([...invocation.args], [
      '--read-only', BUILD_ROOT,
      '--writable', writable,
      '--cwd', `${BUILD_ROOT}\\src`,
      '--env', 'PATH=C:\\Tools',
      '--env', 'CI=1',
      '--', COMMAND, '--version', 'a b',
    ]);
    assert.equal(invocation.cwd, `${BUILD_ROOT}\\src`);
    assert.ok(Object.isFrozen(invocation.args));
  } finally {
    await f.close();
  }
});

test('a relative command is refused with RK_CLASS1_COMMAND_INVALID', async () => {
  const f = await fixture();
  try {
    const platform = createWindowsClass1Platform({ sandboxExec: f.sandboxExec });
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: 'node.exe' }), codeOf('RK_CLASS1_COMMAND_INVALID'));
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: 'bin\\node.exe' }), codeOf('RK_CLASS1_COMMAND_INVALID'));
  } finally {
    await f.close();
  }
});

test('a cwd outside the build root is refused with RK_CLASS1_CWD_OUTSIDE', async () => {
  const f = await fixture();
  try {
    const platform = createWindowsClass1Platform({ sandboxExec: f.sandboxExec });
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, cwd: 'C:\\elsewhere' }), codeOf('RK_CLASS1_CWD_OUTSIDE'));
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, cwd: `${BUILD_ROOT}\\..\\other` }), codeOf('RK_CLASS1_CWD_OUTSIDE'));
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, cwd: 'src' }), codeOf('RK_CLASS1_CWD_OUTSIDE'));
  } finally {
    await f.close();
  }
});

test('a missing or relative sandboxExec is refused with RK_CLASS1_SANDBOX_MISSING', async () => {
  const f = await fixture();
  try {
    for (const sandboxExec of [undefined, 'rightkit-sandbox-exec.exe', join(f.dir, 'absent.exe')]) {
      const platform = createWindowsClass1Platform({ sandboxExec });
      await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND }), codeOf('RK_CLASS1_SANDBOX_MISSING'));
      await assert.rejects(platform.spawnProtectedChild({ buildRoot: BUILD_ROOT, command: COMMAND }), codeOf('RK_CLASS1_SANDBOX_MISSING'));
    }
  } finally {
    await f.close();
  }
});

test('env is forwarded as --env flags and never as the launcher environment', async () => {
  const f = await fixture();
  try {
    const { spawn, calls } = recordingSpawn();
    const platform = createWindowsClass1Platform({ sandboxExec: f.sandboxExec, spawn });
    await platform.spawnProtectedChild({ buildRoot: BUILD_ROOT, command: COMMAND, args: ['x'], env: { SECRET_FREE: 'yes' } });
    assert.equal(calls.length, 1);
    const [{ command, args, options }] = calls;
    assert.equal(command, f.sandboxExec);
    const envIndex = args.indexOf('--env');
    assert.notEqual(envIndex, -1, 'env must be forwarded');
    assert.equal(args[envIndex + 1], 'SECRET_FREE=yes');
    assert.ok(args.indexOf('--env') < args.indexOf('--'), '--env flags must precede the command separator');
    assert.deepEqual(args.slice(args.indexOf('--')), ['--', COMMAND, 'x']);
    assert.equal(options.windowsHide, true);
    assert.equal(options.cwd, win32.dirname(f.sandboxExec));
    assert.ok(!('SECRET_FREE' in (options.env ?? {})), 'child env must not leak into the launcher');
  } finally {
    await f.close();
  }
});

test('malformed env entries are refused with RK_CLASS1_ENV_INVALID', async () => {
  const f = await fixture();
  try {
    const platform = createWindowsClass1Platform({ sandboxExec: f.sandboxExec });
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, env: { 'BAD=KEY': '1' } }), codeOf('RK_CLASS1_ENV_INVALID'));
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, env: { OK: 'a\0b' } }), codeOf('RK_CLASS1_ENV_INVALID'));
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, env: ['A=1'] }), codeOf('RK_CLASS1_ENV_INVALID'));
  } finally {
    await f.close();
  }
});

test('writable roots may not contain or lie inside the build root', async () => {
  const f = await fixture();
  try {
    const platform = createWindowsClass1Platform({ sandboxExec: f.sandboxExec });
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, writableRoots: [`${BUILD_ROOT}\\out`] }), codeOf('RK_CLASS1_PROFILE_INVALID'));
    await assert.rejects(platform.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, writableRoots: ['C:\\rk\\builds'] }), codeOf('RK_CLASS1_PROFILE_INVALID'));
  } finally {
    await f.close();
  }
});

test('a build root that changed after admission is refused with RK_CLASS1_CLONE_MUTATED', async () => {
  const f = await fixture();
  try {
    const clone = { root: BUILD_ROOT, files: ['a.txt'], digest: 'expected' };
    const digest = async () => ({ digest: 'changed' });
    const guarded = createWindowsClass1Platform({ sandboxExec: f.sandboxExec, digest });
    await assert.rejects(guarded.protectedInvocation({ buildRoot: BUILD_ROOT, command: COMMAND, clone }), codeOf('RK_CLASS1_CLONE_MUTATED'));
  } finally {
    await f.close();
  }
});
