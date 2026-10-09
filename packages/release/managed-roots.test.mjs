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
  assert.equal(windows.dataRoot, windows.controlRoot);
  assert.equal(windows.socket, null);
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

test('release client lookup prefers control runtime, then managed root, and resolves only rustright', () => {
  const env = { RIGHTKIT_CONTROL_ROOT: '/internal', RIGHTKIT_MANAGED_ROOT: '/data' }; const seen = [];
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => { seen.push(p); return true; }), '/internal/runtime-current/rustright');
  assert.equal(seen.length, 1);
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.startsWith('/data/')), '/data/runtime-current/rustright');
  assert.equal(managedRuntimeClient(env, 'win32', (p) => p.endsWith('rustright.exe')).endsWith('rustright.exe'), true);
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.endsWith('rightkit.mjs')), undefined);
});

test('Mac managed Cargo never falls through to cargo; missing client or socket is typed', () => {
  const spawn = () => { throw new Error('direct cargo must not run'); };
  assert.equal(runCargoSync(['metadata'], { env: {} }, { platform: 'darwin', exists: () => false, spawn }).error.code, 'RK_BROKER_UNAVAILABLE');
  assert.equal(runCargoSync(['metadata'], { env: {} }, { platform: 'darwin', exists: (p) => p.endsWith('rustright'), spawn }).error.code, 'RK_BROKER_UNAVAILABLE');
  assert.equal(runCargoSync(['metadata'], { env: { GITHUB_ACTIONS: 'true' } }, { platform: 'darwin', exists: () => false, spawn: (command) => ({ command }) }).command, 'cargo', 'hosted GitHub runner uses direct cargo');
  assert.equal(runCargoSync(['metadata'], { env: { GITHUB_ACTIONS: 'true', RIGHTKIT_BUILD_BROKER_SOCKET: '/x' } }, { platform: 'darwin', exists: () => false, spawn }).error.code, 'RK_BROKER_UNAVAILABLE', 'managed host never falls back');
  let call;
  const env = { RIGHTKIT_CONTROL_ROOT: '/internal', RIGHTKIT_BUILD_BROKER_SOCKET: '/explicit' };
  runCargoSync(['metadata'], { env }, { platform: 'darwin', exists: () => true, spawn: (...args) => { call = args; return { status: 0 }; } });
  assert.equal(call[0], '/internal/runtime-current/rustright');
  assert.equal(call[2].env.RIGHTKIT_BUILD_BROKER_SOCKET, '/explicit');
  assert.equal(runCargoSync([], { env: {} }, { platform: 'win32', exists: () => false, spawn: (command) => ({ command }) }).command, 'cargo');
});

// Shared root-resolution contract with RustRight (bogusyogi/rustright docs/contracts/managed-roots.md,
// fixtures vendored from rustright@b34006c). The `localappdata` cases are the post-cutover Windows
// default and stay skipped here until the coordinated flip.
test("managed roots match the RustRight shared fixtures (legacy Windows default)", async () => {
  const { readFile } = await import("node:fs/promises");
  const fixtures = JSON.parse(await readFile(new URL("./fixtures/managed-roots.fixtures.json", import.meta.url), "utf8"));
  const cases = fixtures.cases.filter((c) => c.input.windowsControlDefault === "legacy");
  assert.ok(cases.length >= 10);
  for (const c of cases) {
    const roots = resolveManagedRoots({ platform: c.input.platform, home: c.input.home, env: c.input.env });
    const actual = Object.fromEntries(Object.keys(c.expected).map((key) => [key, roots[key] ?? null]));
    assert.deepEqual(actual, c.expected, c.name);
  }
});
