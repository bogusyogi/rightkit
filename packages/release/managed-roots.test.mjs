import assert from 'node:assert/strict';
import test from 'node:test';
import { discoverBrokerSocket, resolveManagedRoots } from './managed-roots.mjs';
import { managedRuntimeClient, runCargoSync } from './managed-cargo.mjs';

const options = { platform: 'darwin', home: '/Users/test', env: {} };
test('Mac control is internal & bulk data remains external; Windows keeps one root', () => {
  const roots = resolveManagedRoots(options);
  assert.equal(roots.controlRoot, '/Users/test/Library/Application Support/RightKit/build-control');
  assert.equal(roots.dataRoot, '/Volumes/D/.rightkit-managed');
  assert.equal(roots.agentPolicy, `${roots.controlRoot}/agent-policy`);
  const windows = resolveManagedRoots({ platform: 'win32', env: {} });
  assert.equal(windows.controlRoot, 'D:\\.rightkit-managed');
  assert.equal(windows.runtimeCurrent, 'D:\\.rightkit-managed\\runtime-current');
  assert.equal(resolveManagedRoots({ ...options, env: { RIGHTKIT_MANAGED_ROOT: '/data' } }).controlRoot, roots.controlRoot);
});

test('socket discovery prefers internal, then legacy, with explicit endpoint authoritative', () => {
  const roots = resolveManagedRoots(options); const seen = [];
  assert.equal(discoverBrokerSocket({}, { ...options, exists: (p) => { seen.push(p); return true; } }), roots.socket);
  assert.deepEqual(seen, [roots.socket]);
  seen.length = 0;
  assert.equal(discoverBrokerSocket({}, { ...options, exists: (p) => { seen.push(p); return p.startsWith(roots.legacyRoot); } }), `${roots.legacyRoot}/broker.sock`);
  assert.deepEqual(seen, [roots.socket, `${roots.legacyRoot}/broker.sock`]);
  assert.equal(discoverBrokerSocket({ RIGHTKIT_BUILD_BROKER_SOCKET: '/explicit' }, { ...options, exists: () => { throw new Error('must not inspect filesystem'); } }), '/explicit');
});

test('release client lookup prefers control runtime with legacy fallback', () => {
  const env = { RIGHTKIT_CONTROL_ROOT: '/internal', RIGHTKIT_MANAGED_ROOT: '/data' }; const seen = [];
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => { seen.push(p); return true; }), '/internal/runtime-current/build-control-clients/src/rightkit.mjs');
  assert.equal(seen.length, 1);
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.startsWith('/data/')), '/data/runtime-current/build-control-clients/src/rightkit.mjs');
});

test('Mac managed Cargo never falls through to cargo; missing client or socket is typed', () => {
  const spawn = () => { throw new Error('direct cargo must not run'); };
  assert.equal(runCargoSync(['metadata'], { env: {} }, { platform: 'darwin', exists: () => false, spawn }).error.code, 'RK_BROKER_UNAVAILABLE');
  assert.equal(runCargoSync(['metadata'], { env: {} }, { platform: 'darwin', exists: (p) => p.endsWith('rightkit.mjs'), spawn }).error.code, 'RK_BROKER_UNAVAILABLE');
  let call;
  const env = { RIGHTKIT_CONTROL_ROOT: '/internal', RIGHTKIT_BUILD_BROKER_SOCKET: '/explicit' };
  runCargoSync(['metadata'], { env }, { platform: 'darwin', exists: () => true, spawn: (...args) => { call = args; return { status: 0 }; } });
  assert.equal(call[1][0], '/internal/runtime-current/build-control-clients/src/rightkit.mjs');
  assert.equal(call[2].env.RIGHTKIT_BUILD_BROKER_SOCKET, '/explicit');
  assert.equal(runCargoSync([], { env: {} }, { platform: 'win32', exists: () => false, spawn: (command) => ({ command }) }).command, 'cargo');
});
