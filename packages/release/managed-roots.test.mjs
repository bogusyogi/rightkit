import assert from 'node:assert/strict';
import test from 'node:test';
import crypto from 'node:crypto';
import net from 'node:net';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir, userInfo } from 'node:os';
import path from 'node:path';
import { CANONICAL_WINDOWS_PIPE, LEGACY_WINDOWS_PIPE, discoverBrokerSocket, gateRunsRoot, resolveManagedRoots } from './managed-roots.mjs';
import { loadVector } from './test-support/vectors.mjs';
import { managedRuntimeClient, resolveLinuxBrokerRoute, runCargoSync } from './managed-cargo.mjs';

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
  assert.equal(windows.runtimeCurrent, 'D:\\.rightkit-managed\\runtime', 'GB-63: the one fixed live runtime directory (the shared fixtures), no runtime-current link');
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

test('release client lookup: the live runtime first, then the control root runtime, then managed roots, then the legacy runtime-current link; only rustright is ever selected', () => {
  const env = { HOME: '/Users/test', RIGHTKIT_CONTROL_ROOT: '/internal', RIGHTKIT_MANAGED_ROOT: '/data' }; const seen = [];
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => { seen.push(p); return true; }), '/Users/test/Library/Application Support/RustRight/runtime/rustright');
  assert.equal(seen.length, 1);
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.startsWith('/internal/runtime/')), '/internal/runtime/rustright');
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.startsWith('/data/runtime/')), '/data/runtime/rustright');
  assert.equal(managedRuntimeClient(env, 'darwin', (p) => p.startsWith('/data/runtime-current/')), '/data/runtime-current/rustright', 'the legacy link of a host not yet cut over, last');
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
  assert.equal(call[0], path.posix.join(process.env.HOME ?? '', 'Library/Application Support/RustRight/runtime/rustright'), 'GB-63: the live runtime at its final RustRight path is selected first');
  assert.equal(call[2].env.RIGHTKIT_BUILD_BROKER_SOCKET, '/explicit');
  assert.equal(runCargoSync([], { env: {} }, { platform: 'win32', exists: () => false, spawn: (command) => ({ command }) }).command, 'cargo');
});

// Shared root-resolution contract with RustRight (docs/contracts/managed-roots.md): the vector is RustRight's, pinned in test-support/vectors/vectors-source.json.
// The `localappdata` cases are the post-cutover Windows default and stay skipped here until the coordinated flip.
test("managed roots match the RustRight shared fixtures (legacy Windows default); `runtimeCurrent` is <control>/runtime", () => {
  const fixtures = loadVector("managed-roots.fixtures.json");
  const cases = fixtures.cases.filter((c) => c.input.windowsControlDefault === "legacy");
  assert.ok(cases.length >= 10);
  for (const c of cases) {
    const roots = resolveManagedRoots({ platform: c.input.platform, home: c.input.home, env: c.input.env });
    const actual = Object.fromEntries(Object.keys(c.expected).map((key) => [key, roots[key] ?? null]));
    assert.deepEqual(actual, c.expected, c.name);
  }
});

test("[GB-63] one resolver: RUSTRIGHT_* names beat their legacy RIGHTKIT_* spellings, empty is unset, and a stale legacy variable never overrides a current one", () => {
  const roots = (env) => resolveManagedRoots({ platform: "darwin", home: "/Users/test", env });
  assert.equal(roots({ RIGHTKIT_CONTROL_ROOT: "/legacy", RUSTRIGHT_CONTROL_ROOT: "/new" }).controlRoot, "/new");
  assert.equal(roots({ RIGHTKIT_CONTROL_ROOT: "/legacy" }).controlRoot, "/legacy", "a legacy name still works when the current one is unset");
  assert.equal(roots({ RUSTRIGHT_CONTROL_ROOT: "", RIGHTKIT_CONTROL_ROOT: "/legacy" }).controlRoot, "/legacy");
  assert.equal(roots({ RUSTRIGHT_DATA_ROOT: "/d1", RIGHTKIT_DATA_ROOT: "/d2", RIGHTKIT_MANAGED_ROOT: "/d3" }).dataRoot, "/d1");
  assert.equal(roots({ RUSTRIGHT_MANAGED_ROOT: "/m1", RIGHTKIT_DATA_ROOT: "/d2" }).dataRoot, "/d2", "the kinds are tried in order (data, managed, external), each with its current name before its legacy one, as RustRight's resolver does");
  assert.equal(roots({}).runtimeRoot, "/Users/test/Library/Application Support/RustRight");
  assert.equal(roots({ RUSTRIGHT_RUNTIME_ROOT: "/lab/runtime-root" }).liveRuntime, "/lab/runtime-root/runtime");
  assert.equal(resolveManagedRoots({ platform: "win32", home: "C:\\Users\\t", env: {} }).liveRuntime, "D:\\.rustright\\runtime");
});

test("[GB-63] the installed client is selected in one order: the live runtime, then <control>/runtime, then the legacy runtime-current link last; PATH decoys play no part; a Windows client is the native .exe", () => {
  const seen = (platform, env, present) => managedRuntimeClient(env, platform, (p) => present.includes(p));
  const home = "/Users/test";
  const live = `${home}/Library/Application Support/RustRight/runtime/rustright`;
  const control = `${home}/Library/Application Support/RightKit/build-control`;
  const env = { HOME: home, PATH: "/tmp/decoy:/usr/bin" };
  assert.equal(seen("darwin", env, [`${control}/runtime-current/rustright`, `${control}/runtime/rustright`, live]), live);
  assert.equal(seen("darwin", env, [`${control}/runtime-current/rustright`, `${control}/runtime/rustright`]), `${control}/runtime/rustright`);
  assert.equal(seen("darwin", env, [`${control}/runtime-current/rustright`]), `${control}/runtime-current/rustright`, "the legacy link only when nothing newer exists");
  assert.equal(seen("darwin", env, ["/tmp/decoy/rustright"]), undefined, "a client on PATH is never selected by this resolver");
  assert.equal(seen("darwin", { ...env, RUSTRIGHT_RUNTIME_ROOT: "/lab/rt" }, ["/lab/rt/runtime/rustright", live]), "/lab/rt/runtime/rustright", "RUSTRIGHT_RUNTIME_ROOT first");
  assert.equal(seen("darwin", { ...env, RUSTRIGHT_CONTROL_ROOT: "/ctl", RIGHTKIT_CONTROL_ROOT: "/old" }, ["/old/runtime/rustright", "/ctl/runtime/rustright"]), "/ctl/runtime/rustright", "conflicting legacy and current env: the current one");
  const winRoot = "D:\\.rightkit-managed";
  assert.equal(seen("win32", { USERPROFILE: "C:\\Users\\t" }, ["D:\\.rustright\\runtime\\rustright.exe", `${winRoot}\\runtime\\rustright.exe`]), "D:\\.rustright\\runtime\\rustright.exe");
  assert.equal(seen("win32", { USERPROFILE: "C:\\Users\\t" }, [`${winRoot}\\runtime\\rustright.cmd`]), undefined, "a .cmd shim is never the client");
  assert.equal(seen("linux", env, [live]), undefined, "a direct host has no broker client");
});

test("[GB-63] the endpoint: an explicit pipe or socket is authoritative; a Windows shell with no environment finds the configured pipe, else the canonical one; a pre-rename pipe means the canonical one", () => {
  assert.equal(discoverBrokerSocket({ RUSTRIGHT_BUILD_BROKER_SOCKET: "/explicit", RIGHTKIT_BUILD_BROKER_SOCKET: "/old" }, { platform: "darwin", exists: () => true }), "/explicit");
  assert.equal(discoverBrokerSocket({ RIGHTKIT_BUILD_BROKER_SOCKET: "/old" }, { platform: "darwin", exists: () => false }), "/old");
  assert.equal(discoverBrokerSocket({ RUSTRIGHT_BUILD_BROKER_SOCKET: "\\\\.\\pipe\\explicit" }, { platform: "win32" }), "\\\\.\\pipe\\explicit");
  assert.equal(discoverBrokerSocket({ RUSTRIGHT_BUILD_BROKER_SOCKET: LEGACY_WINDOWS_PIPE }, { platform: "win32" }), CANONICAL_WINDOWS_PIPE);
  assert.equal(discoverBrokerSocket({}, { platform: "win32", readFile: () => { throw new Error("ENOENT"); } }), CANONICAL_WINDOWS_PIPE, "no environment, no config: the canonical pipe");
  assert.equal(discoverBrokerSocket({}, { platform: "win32", readFile: () => JSON.stringify({ windowsPipe: "\\\\.\\pipe\\lab-broker" }) }), "\\\\.\\pipe\\lab-broker", "a custom pipe named by the installed config is never ignored");
  assert.equal(discoverBrokerSocket({}, { platform: "win32", readFile: () => JSON.stringify({ windowsPipe: LEGACY_WINDOWS_PIPE }) }), CANONICAL_WINDOWS_PIPE);
  for (const bad of [{ windowsPipe: "C:\\not-a-pipe" }, { windowsPipe: "\\\\.\\pipe\\a\\b" }, { windowsPipe: 42 }]) assert.equal(discoverBrokerSocket({}, { platform: "win32", readFile: () => JSON.stringify(bad) }), CANONICAL_WINDOWS_PIPE, JSON.stringify(bad));
  assert.equal(discoverBrokerSocket({ RUSTRIGHT_CONTROL_ROOT: "/ctl" }, { platform: "darwin", exists: (p) => p === "/ctl/broker.sock" }), "/ctl/broker.sock");
});


// E2E L0-06: a direct (Linux) host wrote its scan logs and direct diagnostics under /Volumes/D/.rightkit-managed/runs, the Mac data
// volume, creating /Volumes/D as root. Only a managed Mac or Windows host has a data root to put runs in.
test('the gate runs root is the managed data root on Mac and Windows and a per-account state directory elsewhere, never the Mac volume', () => {
  assert.equal(gateRunsRoot({ platform: 'darwin', env: {}, home: '/Users/test' }), '/Volumes/D/.rightkit-managed/runs');
  assert.equal(gateRunsRoot({ platform: 'win32', env: {}, home: 'C:\\Users\\test' }), 'D:\\.rightkit-managed\\runs');
  assert.equal(gateRunsRoot({ platform: 'darwin', env: { RIGHTKIT_MANAGED_ROOT: '/data' }, home: '/Users/test' }), '/data/runs');
  for (const platform of ['linux', 'freebsd']) {
    const root = gateRunsRoot({ platform, env: {}, home: '/home/test' });
    assert.equal(root, '/home/test/.local/state/rightkit/runs');
    assert.ok(!root.startsWith('/Volumes'), root);
  }
  assert.equal(gateRunsRoot({ platform: 'linux', env: { RIGHTKIT_MANAGED_ROOT: '/data' }, home: '/home/test' }), '/data/runs', 'an explicit data root still wins on Linux');
  assert.equal(gateRunsRoot({ platform: 'linux', env: { RUSTRIGHT_DATA_ROOT: '/rr-data', RIGHTKIT_DATA_ROOT: '/old' }, home: '/home/test' }), '/rr-data/runs', 'GB-63: the RUSTRIGHT_ name names the data root too, and beats its legacy spelling');
  assert.equal(gateRunsRoot({ platform: 'linux', env: { RIGHTKIT_SCRATCH_ROOT: '/scratch', RIGHTKIT_MANAGED_ROOT: '/data' }, home: '/home/test' }), '/scratch', 'RIGHTKIT_SCRATCH_ROOT wins everywhere');
});

// RA-18 / L3-10: one resolver. A Linux host that names no data root never defaults to the Mac volume; one that names its roots resolves as RustRight's does.
test('[RA-18] a Linux or other host with no named data root defaults to the account state directory, never the Mac volume; a named root resolves exactly as the shared fixtures say', () => {
  for (const platform of ['linux', 'freebsd']) {
    const roots = resolveManagedRoots({ platform, home: '/home/test', env: {} });
    assert.equal(roots.dataRoot, '/home/test/.local/state/rightkit');
    assert.equal(roots.controlRoot, roots.dataRoot, 'control equals data off Mac, as RustRight resolves it');
    assert.equal(roots.socket, '/home/test/.local/state/rightkit/broker.sock');
    assert.equal(roots.runtimeCurrent, '/home/test/.local/state/rightkit/runtime');
    assert.equal(roots.liveRuntime, '/home/test/.local/state/rightkit/runtime', 'no runtime root of its own off Mac and Windows: the control root');
    assert.equal(roots.legacyRoot, '/Volumes/D/.rightkit-managed', 'the shared fixtures pin legacyRoot on every platform; it is a name, not a default');
    for (const value of Object.values(roots)) assert.ok(!String(value).startsWith('/Volumes') || value === roots.legacyRoot, String(value));
    assert.equal(gateRunsRoot({ platform, env: {}, home: '/home/test' }), `${roots.dataRoot}/runs`, 'the runs root is the data root\'s: one answer');
  }
  const named = resolveManagedRoots({ platform: 'linux', home: '/home/test', env: { RUSTRIGHT_DATA_ROOT: '/srv/rk', RUSTRIGHT_CONTROL_ROOT: '/srv/ctl' } });
  assert.deepEqual([named.dataRoot, named.controlRoot, named.socket], ['/srv/rk', '/srv/ctl', '/srv/ctl/broker.sock']);
  // The managed platforms keep their defaults.
  assert.equal(resolveManagedRoots({ platform: 'darwin', home: '/Users/test', env: {} }).dataRoot, '/Volumes/D/.rightkit-managed');
  assert.equal(resolveManagedRoots({ platform: 'win32', home: 'C:\\Users\\t', env: {} }).dataRoot, 'D:\\.rightkit-managed');
});

// The declared Linux route, over real files, a real symlink and a real unix socket: the lookup answers with the client the gate's executor drives, or with none.
async function declaredLinuxHost(run) {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'rk-lbc-')));
  const client = path.join(root, 'bin', 'rustright'); mkdirSync(path.dirname(client));
  writeFileSync(client, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  const link = path.join(root, 'rustright-link'); symlinkSync(client, link);
  const socket = path.join(root, 'b.sock');
  const server = net.createServer(); await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socket, resolve); });
  const digest = crypto.createHash('sha256').update('#!/bin/sh\nexit 0\n').digest('hex');
  const env = { HOME: '/home/test', RIGHTKIT_LINUX_BROKER_CLIENT: link, RIGHTKIT_LINUX_BROKER_SOCKET: socket, RIGHTKIT_LINUX_BROKER_CLIENT_SHA256: digest };
  try { await run({ root, client, link, socket, digest, env }); } finally { server.close(); rmSync(root, { recursive: true, force: true }); }
}
const unixOnly = { skip: process.platform === 'win32' ? 'unix sockets and POSIX modes' : false };

test('[RA-18] a declared Linux broker route resolves to the very client the gate executor validates (real path, pinned digest); an undeclared Linux host stays direct with no client', unixOnly, async () => {
  await declaredLinuxHost(({ client, link, env, digest, socket }) => {
    const route = resolveLinuxBrokerRoute({ env, platform: 'linux' });
    assert.equal(route.ok, true, JSON.stringify(route));
    assert.equal(managedRuntimeClient(env, 'linux'), route.client, 'one client: the executor\'s');
    assert.equal(route.client, client, 'resolved through the symlink to the real file');
    // The legacy endpoint spelling is fine when it is the same endpoint; another endpoint is ambiguous for the executor and for the lookup alike.
    for (const same of [{ RUSTRIGHT_BUILD_BROKER_SOCKET: socket }, { RIGHTKIT_BUILD_BROKER_SOCKET: socket }]) assert.equal(managedRuntimeClient({ ...env, ...same }, 'linux'), client);
    for (const bad of [
      { RUSTRIGHT_BUILD_BROKER_SOCKET: '/elsewhere/broker.sock' },
      { RIGHTKIT_LINUX_BROKER_CLIENT_SHA256: 'f'.repeat(64) },
      { RIGHTKIT_LINUX_BROKER_CLIENT_SHA256: undefined },
      { RIGHTKIT_LINUX_BROKER_CLIENT_SHA256: digest.slice(1) },
      { RIGHTKIT_LINUX_BROKER_SOCKET: undefined },
      { RIGHTKIT_LINUX_BROKER_CLIENT: 'rustright' },
      { RIGHTKIT_LINUX_BROKER_CLIENT: `${path.dirname(client)}/../bin/rustright` },
      { RIGHTKIT_LINUX_BROKER_CLIENT: `${link}-gone` },
    ]) {
      const declared = { ...env, ...bad };
      for (const key of Object.keys(declared)) if (declared[key] === undefined) delete declared[key];
      assert.equal(resolveLinuxBrokerRoute({ env: declared, platform: 'linux' }).ok, false, JSON.stringify(bad));
      assert.equal(managedRuntimeClient(declared, 'linux'), undefined, `the lookup agrees with the executor's refusal: ${JSON.stringify(bad)}`);
    }
    // A client sitting where the managed roots and PATH would put one never makes an undeclared Linux host a broker host.
    const undeclared = { HOME: '/home/test', PATH: `${path.dirname(client)}:/usr/bin`, RUSTRIGHT_CONTROL_ROOT: path.dirname(path.dirname(client)), RUSTRIGHT_BUILD_BROKER_SOCKET: socket };
    assert.equal(managedRuntimeClient(undeclared, 'linux', () => true), undefined);
    assert.equal(managedRuntimeClient({}, 'linux'), undefined);
    assert.equal(managedRuntimeClient(env, 'freebsd'), client, 'every non-Mac, non-Windows platform follows the same declaration rule as managed-cargo directPlatform');
  });
});
