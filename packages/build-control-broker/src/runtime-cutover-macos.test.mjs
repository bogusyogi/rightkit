import assert from 'node:assert/strict';
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createMacOSRuntimeCallbacks } from './runtime-cutover-macos.mjs';

const manifest = { schema: 'rightkit.build-control.runtime.v2', version: 'v2-test', clientDigest: 'c'.repeat(64), brokerDigest: 'b'.repeat(64) };
const macOSOnly = { skip: process.platform === 'win32' ? 'macOS-only path semantics' : false };

function harness({ open = 0, unloaded = false } = {}) {
  let pid = 10;
  let loaded = !unloaded;
  const calls = [];
  const execFile = async (file, args) => {
    calls.push([file, args]);
    if (args[0] === 'print') {
      if (!loaded) throw new Error('Could not find service');
      return { stdout: `pid = ${pid}\n` };
    }
    if (args[0] === 'enable') return { stdout: '' };
    if (args[0] === 'bootstrap') { loaded = true; return { stdout: '' }; }
    if (args[0] === 'kickstart') { pid += 1; return { stdout: '' }; }
    throw new Error('unexpected command');
  };
  const netModule = { createConnection(endpoint) {
    calls.push(['socket.connect', endpoint]);
    const listeners = {};
    const handle = { once(type, fn) { listeners[type] = fn; return handle; }, on(type, fn) { listeners[type] = fn; return handle; }, write(value) { calls.push(['socket.write', value]); }, destroy() {} };
    queueMicrotask(() => { listeners.connect?.(); listeners.data?.(Buffer.from(`${JSON.stringify({ type: 'HANDSHAKE', runtime: manifest })}\n${JSON.stringify({ type: 'STATUS', id: 'runtime-health-probe', state: 'ACTIVATION_PENDING', runtime: manifest })}\n`)); });
    return handle;
  } };
  return { calls, callbacks: createMacOSRuntimeCallbacks({ launchdLabel: 'com.rightkit.broker', endpoint: '/tmp/rk.sock', execFile, netModule, drainProbe: async () => open, plistPath: '/Library/LaunchAgents/com.rightkit.broker.plist', timeoutMs: 200 }) };
}

test('macOS cutover uses exact launchctl target, waits PID change & validates handshake', async () => {
  const { callbacks, calls } = harness();
  const result = await callbacks.restartService({ manifest, timeoutMs: 200 });
  assert.equal(result.pid, 11);
  assert.deepEqual(calls.filter(([file]) => file === 'launchctl').map(([file, args]) => [file, args]), [
    ['launchctl', ['enable', 'system/com.rightkit.broker']],
    ['launchctl', ['print', 'system/com.rightkit.broker']],
    ['launchctl', ['kickstart', '-k', 'system/com.rightkit.broker']],
    ['launchctl', ['print', 'system/com.rightkit.broker']],
  ]);
  const statusWrites = calls.filter(([kind]) => kind === 'socket.write');
  assert.equal(statusWrites.length, 1);
  assert.deepEqual(JSON.parse(statusWrites[0][1]), { type: 'STATUS', protocol: 'rightkit.build-control.client.v2', runtime: manifest, id: 'runtime-health-probe' });
  await callbacks.probeService({ manifest, restart: result, timeoutMs: 200 });
});

test('macOS cutover refuses while thin requests remain active', async () => {
  const { callbacks, calls } = harness({ open: 1 });
  await assert.rejects(callbacks.restartService({ manifest, timeoutMs: 200 }), /open v1 request/);
  assert.equal(calls.length, 0);
});

test('macOS cutover rejects handshake digest mismatch', async () => {
  const { callbacks } = harness();
  const bad = { ...manifest, brokerDigest: 'd'.repeat(64) };
  await assert.rejects(callbacks.restartService({ manifest: bad, timeoutMs: 200 }), /HANDSHAKE identity mismatch/);
});

test('macOS cutover bootstraps exact supplied plist when service is unloaded', macOSOnly, async () => {
  const { callbacks, calls } = harness({ unloaded: true });
  const result = await callbacks.restartService({ manifest, timeoutMs: 200 });
  assert.equal(result.pid, 10);
  assert.deepEqual(calls.filter(([file]) => file === 'launchctl').slice(0, 4), [
    ['launchctl', ['enable', 'system/com.rightkit.broker']],
    ['launchctl', ['print', 'system/com.rightkit.broker']],
    ['launchctl', ['bootstrap', 'system', '/Library/LaunchAgents/com.rightkit.broker.plist']],
    ['launchctl', ['print', 'system/com.rightkit.broker']],
  ]);
  assert.equal(calls.some(([file, args]) => file === 'launchctl' && args[0] === 'kickstart'), false);
});

test('macOS process probe binds launchd PID to exact resolved runtime script', async () => {
  const temporary = await realpath(await mkdtemp(join(tmpdir(), 'rk-cutover-process-')));
  const root = join(temporary, 'Library', 'Application Support', 'RightKit', 'build-control');
  await mkdir(root, { recursive: true });
  try {
    const runtime = join(root, 'runtime-hash'); const script = join(runtime, 'build-control-broker', 'src', 'broker.mjs');
    await mkdir(join(runtime, 'build-control-broker', 'src'), { recursive: true }); await writeFile(script, ''); await symlink('runtime-hash', join(root, 'runtime-current'));
    let suffix = '';
    const callbacks = createMacOSRuntimeCallbacks({
      launchdLabel: 'dev.rightkit.broker', endpoint: join(root, 'broker.sock'), externalRoot: root, plistPath: join(root, 'broker.plist'), configPath: join(root, 'config.json'), nodePath: process.execPath, drainProbe: async () => 0,
      processKill: () => {},
      execFile: async (_file, args) => {
        if (args[0] === 'print') return { stdout: 'pid = 42\n' };
        if (args[0] === '-ww') return { stdout: `${process.execPath} ${join(root, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs')} ${join(root, 'config.json')}${suffix}\n` };
        throw new Error('unexpected command');
      },
    });
    assert.deepEqual(await callbacks.processProbe({ pid: 42, runtime }), { alive: true, runtimePath: runtime, scriptPath: script });
    suffix = ' --foreign-argument';
    assert.deepEqual(await callbacks.processProbe({ pid: 42, runtime }), { alive: false });
    assert.deepEqual(await callbacks.processProbe({ pid: 41, runtime }), { alive: false });
  } finally { await rm(temporary, { recursive: true, force: true }); }
});

test('activation handshake uses supplied socket even with legacy socket environment', async (t) => {
  const previous = process.env.RIGHTKIT_BUILD_BROKER_SOCKET;
  process.env.RIGHTKIT_BUILD_BROKER_SOCKET = '/legacy/broker.sock';
  t.after(() => { if (previous === undefined) delete process.env.RIGHTKIT_BUILD_BROKER_SOCKET; else process.env.RIGHTKIT_BUILD_BROKER_SOCKET = previous; });
  const { callbacks, calls } = harness({ unloaded: true });
  const result = await callbacks.restartService({ manifest });
  await callbacks.probeService({ manifest, restart: result });
  assert.deepEqual(calls.filter(([kind]) => kind === 'socket.connect'), [['socket.connect', '/tmp/rk.sock'], ['socket.connect', '/tmp/rk.sock']]);
});

test('rollback unloads candidate & bootstraps prior once using restored config socket', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'rk-rollback-launchd-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const prior = 'runtime-prior'; const configPath = join(root, 'config.json');
  await mkdir(join(root, prior));
  await writeFile(join(root, prior, 'runtime-manifest.json'), JSON.stringify({ ...manifest, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40) }));
  await writeFile(configPath, JSON.stringify({ unixSocket: '/restored/legacy.sock' }));
  let loaded = true; const calls = [];
  const callbacks = createMacOSRuntimeCallbacks({
    launchdLabel: 'dev.rightkit.broker', uid: 501, externalRoot: root, configPath,
    endpoint: '/candidate/internal.sock', plistPath: join(root, 'broker.plist'), drainProbe: async () => 0,
    processProbe: async () => ({ alive: true }), timeoutMs: 200,
    execFile: async (_file, args) => {
      calls.push(args);
      if (args[0] === 'print') { if (!loaded) throw new Error('unloaded'); return { stdout: 'pid = 42\n' }; }
      if (args[0] === 'bootout') loaded = false;
      if (args[0] === 'bootstrap') loaded = true;
      return { stdout: '' };
    },
    netModule: { createConnection(endpoint) {
      assert.equal(endpoint, '/restored/legacy.sock');
      const listeners = {}; const handle = { once(type, fn) { listeners[type] = fn; return handle; }, on(type, fn) { listeners[type] = fn; return handle; }, write() {}, destroy() {} };
      queueMicrotask(() => { listeners.connect?.(); listeners.data?.(`${JSON.stringify({ type: 'HANDSHAKE', runtime: manifest })}\n`); });
      return handle;
    } },
  });
  await callbacks.stopService();
  assert.equal(loaded, false);
  assert.deepEqual(calls.slice(0, 2), [['disable', 'gui/501/dev.rightkit.broker'], ['bootout', 'gui/501/dev.rightkit.broker']]);
  const result = await callbacks.rollbackService({ prior, manifest });
  assert.equal(result.pid, 42);
  assert.equal(calls.filter(([command]) => command === 'bootstrap').length, 1);
  assert.equal(calls.some(([command]) => command === 'kickstart'), false);
});

test('first cutover rollback keeps legacy admission stopped', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-cutover-legacy-'));
  try {
    await mkdir(join(root, 'runtime-prior'));
    const { callbacks } = harness();
    let loaded = true;
    const legacy = createMacOSRuntimeCallbacks({
      launchdLabel: 'com.rightkit.broker', endpoint: '/tmp/rk.sock', externalRoot: root, plistPath: '/Library/LaunchAgents/com.rightkit.broker.plist',
      drainProbe: async () => 0, timeoutMs: 200, processProbe: async () => ({ alive: true, runtimePath: join(root, 'runtime-prior') }),
      execFile: async (_file, args) => {
        if (args[0] === 'disable') return { stdout: '' };
        if (args[0] === 'bootout') { loaded = false; return { stdout: '' }; }
        if (args[0] === 'print') { if (!loaded) throw new Error('not loaded'); return { stdout: 'pid = 10\n' }; }
        throw new Error('unexpected command');
      },
      netModule: { createConnection() { const listeners = {}; const handle = { once(type, fn) { listeners[type] = fn; return handle; }, on(type, fn) { listeners[type] = fn; return handle; }, destroy() {} }; queueMicrotask(() => listeners.error?.(Object.assign(new Error('closed'), { code: 'ENOENT' }))); return handle; } },
    });
    const result = await legacy.rollbackService({ prior: 'runtime-prior', manifest, timeoutMs: 200 });
    assert.equal(result.legacy, true);
    assert.equal(result.stopped, true);
    assert.equal(result.runtimeIdentity, join(root, 'runtime-prior'));
    assert.ok(callbacks);
  } finally { await rm(root, { recursive: true, force: true }); }
});
