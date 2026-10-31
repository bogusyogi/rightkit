import assert from 'node:assert/strict';
import test from 'node:test';
import { userInfo } from 'node:os';
import path from 'node:path';
import { discoverBrokerSocket, envNamed, managedControlCandidates, resolveManagedRoots } from './managed-roots.mjs';
import { managedHost, managedRuntimeClient, runCargoSync } from './managed-cargo.mjs';

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
  assert.equal(windows.runtimeCurrent, 'D:\\.rightkit-managed\\runtime'); // B10: the contract's dir is `runtime`
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
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => { seen.push(p); return true; }), '/internal/runtime/rustright');
  assert.equal(seen.length, 1);
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.startsWith('/data/')), '/data/runtime/rustright');
  assert.equal(managedRuntimeClient(env, 'win32', (p) => p.endsWith('rustright.exe')).endsWith('rustright.exe'), true);
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.endsWith('rightkit.mjs')), undefined);
});

test('Mac native client survives broker-managed HOME & retains broker endpoint', () => {
  const client = path.posix.join(userInfo().homedir, 'Library', 'Application Support', 'RustRight', 'runtime', 'rustright');
  const env = { HOME: '/managed/build-home', RIGHTKIT_CONTROL_ROOT: '/internal', RIGHTKIT_BUILD_BROKER_SOCKET: '/broker.sock' };
  const exists = (file) => file === client || file === env.RIGHTKIT_BUILD_BROKER_SOCKET;
  assert.equal(managedRuntimeClient(env, 'darwin', exists), client);
  let call;
  runCargoSync(['metadata', '--no-deps'], { env }, { platform: 'darwin', exists, spawn: (...args) => { call = args; return { status: 0 }; } });
  assert.equal(call[0], client);
  assert.deepEqual(call[1], ['cargo', 'metadata', '--no-deps']);
  assert.equal(call[2].env.RIGHTKIT_BUILD_BROKER_SOCKET, '/broker.sock');
  assert.equal(call[2].env.HOME, '/managed/build-home');
});

test('native install lookup retains explicit Mac HOME & Windows drive layout', () => {
  const macClient = '/Users/custom/Library/Application Support/RustRight/runtime/rustright';
  assert.equal(managedRuntimeClient({ HOME: '/Users/custom' }, 'darwin', (file) => file === macClient), macClient);
  const windowsClient = 'D:\\.rustright\\runtime\\rustright.exe';
  assert.equal(managedRuntimeClient({ RIGHTKIT_CONTROL_ROOT: 'D:\\control' }, 'win32', (file) => file === windowsClient), windowsClient);
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
  assert.equal(call[0], '/internal/runtime/rustright');
  assert.equal(call[2].env.RIGHTKIT_BUILD_BROKER_SOCKET, '/explicit');
  assert.equal(runCargoSync([], { env: {} }, { platform: 'win32', exists: () => false, spawn: (command) => ({ command }) }).command, 'cargo');
});

// Shared root-resolution contract with RustRight (bogusyogi/rustright docs/contracts/managed-roots.md,
// fixtures vendored from rustright@b84ae605 (B10: runtimeCurrent is `runtime`)). The `localappdata` cases are the post-cutover Windows
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

// ---- GATE-4 (docs/GATE-BROKER-AUDIT-2026-10-30.md) -------------------------------------------------------------------------

test('M19: roots and the socket read RUSTRIGHT_ first, then the legacy RIGHTKIT_ spelling, for all five names (the broker\'s env_named)', () => {
  const roots = resolveManagedRoots({ platform: 'darwin', home: '/Users/test', env: { RUSTRIGHT_CONTROL_ROOT: '/x', RUSTRIGHT_MANAGED_ROOT: '/d' } });
  assert.equal(roots.controlRoot, '/x'); assert.equal(roots.dataRoot, '/d'); assert.equal(roots.socket, '/x/broker.sock');
  // RUSTRIGHT_ wins over RIGHTKIT_ for each root; an empty RUSTRIGHT_ value is unset and falls to the legacy spelling.
  const both = resolveManagedRoots({ platform: 'darwin', home: '/h', env: { RUSTRIGHT_CONTROL_ROOT: '/new-c', RIGHTKIT_CONTROL_ROOT: '/old-c', RUSTRIGHT_DATA_ROOT: '/new-d', RIGHTKIT_DATA_ROOT: '/old-d' } });
  assert.deepEqual([both.controlRoot, both.dataRoot], ['/new-c', '/new-d']);
  assert.equal(resolveManagedRoots({ platform: 'darwin', home: '/h', env: { RUSTRIGHT_DATA_ROOT: '', RIGHTKIT_DATA_ROOT: '/old-d' } }).dataRoot, '/old-d');
  // The data-root chain keeps its order: DATA_ROOT, MANAGED_ROOT, EXTERNAL_ROOT, each RUSTRIGHT_ first.
  assert.equal(resolveManagedRoots({ platform: 'darwin', home: '/h', env: { RIGHTKIT_DATA_ROOT: 'a', RUSTRIGHT_MANAGED_ROOT: 'b' } }).dataRoot, 'a');
  assert.equal(resolveManagedRoots({ platform: 'darwin', home: '/h', env: { RUSTRIGHT_EXTERNAL_ROOT: 'e', RIGHTKIT_MANAGED_ROOT: 'm' } }).dataRoot, 'm');
  assert.equal(resolveManagedRoots({ platform: 'darwin', home: '/h', env: { RUSTRIGHT_EXTERNAL_ROOT: 'e' } }).dataRoot, 'e');
  assert.equal(envNamed({ RIGHTKIT_BUILD_BROKER_SOCKET: '/legacy' }, 'BUILD_BROKER_SOCKET'), '/legacy');
  assert.equal(envNamed({ RUSTRIGHT_BUILD_BROKER_SOCKET: '/s', RIGHTKIT_BUILD_BROKER_SOCKET: '/legacy' }, 'BUILD_BROKER_SOCKET'), '/s');
  assert.equal(discoverBrokerSocket({ RUSTRIGHT_BUILD_BROKER_SOCKET: '/s' }, { platform: 'darwin', exists: () => { throw new Error('explicit endpoint is authoritative'); } }), '/s');
  assert.equal(discoverBrokerSocket({ RUSTRIGHT_BUILD_BROKER_SOCKET: '/s', RIGHTKIT_BUILD_BROKER_SOCKET: '/legacy' }, { platform: 'darwin' }), '/s');
  // A RUSTRIGHT_-only control root is a candidate: a socket there is found with nothing else set.
  assert.equal(discoverBrokerSocket({ RUSTRIGHT_CONTROL_ROOT: '/ctl' }, { platform: 'darwin', home: '/h', exists: (p) => p === '/ctl/broker.sock' }), '/ctl/broker.sock');
  // Non-broker platforms read the same five through the same helper.
  const linux = managedControlCandidates({ platform: 'linux', env: { RUSTRIGHT_CONTROL_ROOT: '/c', RUSTRIGHT_DATA_ROOT: '/d', RIGHTKIT_MANAGED_ROOT: '/m', RUSTRIGHT_EXTERNAL_ROOT: '/e' } });
  assert.deepEqual(linux, ['/c', '/d', '/m', '/e']);
  // managedHost sees the RUSTRIGHT_ spelling, and only on a broker platform.
  assert.equal(managedHost({ RUSTRIGHT_BUILD_BROKER_SOCKET: '/s' }, 'darwin'), true);
  assert.equal(managedHost({ RUSTRIGHT_BUILD_BROKER_SOCKET: '/s' }, 'linux'), false);
});

test('M20: the renamed RustRight roots are socket candidates after the legacy root, in the broker\'s order, deduplicated', () => {
  const darwin = { platform: 'darwin', home: '/Users/test', env: {} };
  const renamedControl = '/Users/test/Library/Application Support/RustRight/broker/broker.sock';
  const seen = [];
  assert.equal(discoverBrokerSocket({}, { ...darwin, exists: (p) => { seen.push(p); return p === renamedControl; } }), renamedControl);
  assert.deepEqual(seen, [
    '/Users/test/Library/Application Support/RightKit/build-control/broker.sock', '/Volumes/D/.rightkit-managed/broker.sock', renamedControl,
  ]);
  assert.equal(discoverBrokerSocket({}, { ...darwin, exists: (p) => p === '/Volumes/D/.rustright/broker.sock' }), '/Volumes/D/.rustright/broker.sock');
  assert.deepEqual(managedControlCandidates(darwin).slice(-3), ['/Volumes/D/.rightkit-managed', renamedControl.replace('/broker.sock', ''), '/Volumes/D/.rustright']);
  assert.deepEqual(managedControlCandidates({ platform: 'win32', env: {} }), ['D:\\.rightkit-managed', 'D:\\.rustright']);
  // An explicit client root keeps its place after the data root; a root equal to a renamed one is listed once.
  const withClient = managedControlCandidates({ ...darwin, clientRoot: '/c' });
  assert.deepEqual(withClient.slice(1, 3), ['/Volumes/D/.rightkit-managed', '/c']);
  assert.equal(new Set(managedControlCandidates({ ...darwin, env: { RUSTRIGHT_DATA_ROOT: '/Volumes/D/.rustright' } })).size, managedControlCandidates({ ...darwin, env: { RUSTRIGHT_DATA_ROOT: '/Volumes/D/.rustright' } }).length);
});

test('B10: runtimeCurrent is `<control>/runtime` on every platform, as the shared fixtures say, and all 13 fixture cases are present', async () => {
  const { readFile } = await import('node:fs/promises');
  const fixtures = JSON.parse(await readFile(new URL('./fixtures/managed-roots.fixtures.json', import.meta.url), 'utf8'));
  assert.equal(fixtures.cases.length, 13);
  for (const c of fixtures.cases) assert.ok(/[\\/]runtime$/.test(c.expected.runtimeCurrent) && !/runtime-current/.test(c.expected.runtimeCurrent), `${c.name}: the vendored fixture is the post-B10 contract`);
  for (const c of fixtures.cases.filter((x) => x.input.windowsControlDefault === 'legacy')) {
    const roots = resolveManagedRoots({ platform: c.input.platform, home: c.input.home, env: c.input.env });
    assert.equal(roots.runtimeCurrent, c.expected.runtimeCurrent, c.name);
  }
  assert.equal(resolveManagedRoots(options).runtimeCurrent, `${resolveManagedRoots(options).controlRoot}/runtime`);
});

test('M58/B10: the client probe tries `runtime` before the retired `runtime-current` under every root, then the installed RustRight layout', () => {
  const env = { RIGHTKIT_CONTROL_ROOT: '/internal' };
  const seen = [];
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => { seen.push(p); return p === '/internal/runtime-current/rustright'; }), '/internal/runtime-current/rustright', 'a not-yet-migrated host still resolves');
  assert.deepEqual(seen.slice(0, 2), ['/internal/runtime/rustright', '/internal/runtime-current/rustright']);
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p === '/internal/runtime/rustright' || p === '/internal/runtime-current/rustright'), '/internal/runtime/rustright', 'both present: `runtime` wins');
  // RUSTRIGHT_ spelling reaches the probe too.
  assert.equal(managedRuntimeClient({ RUSTRIGHT_CONTROL_ROOT: '/r' }, 'darwin', (p) => p === '/r/runtime/rustright'), '/r/runtime/rustright');
  // Windows: the probe is `<root>\runtime\rustright.exe` first, and the renamed root `D:\.rustright` is a candidate in its own right.
  assert.equal(managedRuntimeClient({ RIGHTKIT_CONTROL_ROOT: 'D:\\control' }, 'win32', (p) => p === 'D:\\control\\runtime\\rustright.exe'), 'D:\\control\\runtime\\rustright.exe');
  assert.equal(managedRuntimeClient({}, 'win32', (p) => p === 'D:\\.rustright\\runtime\\rustright.exe'), 'D:\\.rustright\\runtime\\rustright.exe');
  assert.equal(managedRuntimeClient(env, 'linux', () => true), undefined, 'Linux has no managed client');
});
