import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { existsSync } from 'node:fs';
import { lstat } from 'node:fs/promises';
import { enumerateLiveDescendants } from './process-tree.mjs';
import {
  brokerOwnedSccacheProof,
  brokerSccacheNotifyPath,
  brokerSccacheServerEnvironment,
  brokerSccacheSocketPath,
  directRustcFallback,
  prepareBrokerSccacheSocket,
  startBrokerOwnedSccacheServer,
  SccacheSupervisor
} from './sccache-supervisor.mjs';

function fakeSpawn(responses, calls, environments = []) { return (_command, argv, options) => { calls.push(argv); environments.push(options?.env ?? null); const child = Object.assign(new EventEmitter(), { stdout: new EventEmitter(), stderr: new EventEmitter(), kill() {} }); const response = responses.shift(); queueMicrotask(() => { if (response.stdout) child.stdout.emit('data', response.stdout); child.emit('close', response.code ?? 0); }); return child; }; }
const fakeChild = (pid = 41) => Object.assign(new EventEmitter(), { pid, kill() {} });
function countingStart(counts, { pid = 4242, delayMs = 0, fail = null } = {}) {
  return async () => {
    counts.push(pid);
    if (delayMs) await new Promise((resolve) => setTimeout(resolve, delayMs));
    if (fail) throw fail;
    const child = fakeChild(pid - 1);
    return { child, pid, ownership: { pid: pid - 1, platform: 'posix' } };
  };
}
const managedSupervisor = (overrides = {}) => new SccacheSupervisor({ tool: { path: '/managed/sccache' }, mode: 'managed', ownershipProof: async () => true, timeoutMs: 100, socketPath: '/managed/.rk-sccache.sock', notifyPath: '/managed/.rk-sccache-notify.sock', ...overrides });

test('one supervised server provides bounded before/after stats deltas', async () => {
  const calls = []; const counts = [];
  const supervisor = managedSupervisor({ startServer: countingStart(counts), spawn: fakeSpawn([{ stdout: '{"compile_requests":2,"cache_hits":{"Rust":1}}' }, { stdout: '{"compile_requests":5,"cache_hits":{"Rust":3}}' }], calls) });
  const before = await supervisor.sample({}); const after = await supervisor.sample({}); const delta = supervisor.delta(before, after);
  assert.deepEqual(calls, [['--show-stats', '--stats-format=json'], ['--show-stats', '--stats-format=json']]);
  assert.equal(counts.length, 1); assert.equal(delta.delta.compile_requests, 3); assert.equal(delta.delta['cache_hits.Rust'], 2);
});

test('sample starts the server before it proves ownership & injects the started pid', async () => {
  const order = []; const calls = [];
  const supervisor = managedSupervisor({
    startServer: async () => { order.push('start'); return { child: fakeChild(), pid: 4242, ownership: { pid: 41, platform: 'posix' } }; },
    ownershipProof: async ({ serverPid }) => { order.push(`proof:${serverPid}`); return true; },
    spawn: fakeSpawn([{ stdout: '{"compile_requests":1}' }], calls)
  });
  const sample = await supervisor.sample({});
  assert.equal(sample.status, 'OK');
  assert.deepEqual(order, ['start', 'proof:4242']);
  assert.deepEqual(calls, [['--show-stats', '--stats-format=json']]);
});

test('the job environment sample() forwards carries no server-start variable', async () => {
  const environments = [];
  const jobEnvironment = Object.freeze({ RUSTC_WRAPPER: '/managed/sccache', SCCACHE_SERVER_UDS: '/managed/.rk-sccache.sock' });
  const supervisor = managedSupervisor({ startServer: countingStart([]), spawn: fakeSpawn([{ stdout: '{}' }], [], environments) });
  assert.equal((await supervisor.sample(jobEnvironment)).status, 'OK');
  for (const key of ['SCCACHE_START_SERVER', 'SCCACHE_NO_DAEMON', 'SCCACHE_STARTUP_NOTIFY', 'SCCACHE_IDLE_TIMEOUT']) assert.equal(environments[0][key], undefined, key);
  assert.deepEqual(Object.keys(environments[0]).sort(), ['RUSTC_WRAPPER', 'SCCACHE_SERVER_UDS']);
});

test('the server-start environment names the foreground, never-idle, notifying server', () => {
  const environment = brokerSccacheServerEnvironment({ socketPath: '/managed/.rk-sccache.sock', notifyPath: '/managed/.rk-sccache-notify.sock', cacheDir: '/managed/compiler-cache', maxSizeBytes: 21_474_836_480, errorLogPath: '/managed/state/sccache-server.log', env: { PATH: '/bin', HOME: '/home' } });
  assert.equal(environment.SCCACHE_START_SERVER, '1');
  assert.equal(environment.SCCACHE_NO_DAEMON, '1');
  assert.equal(environment.SCCACHE_IDLE_TIMEOUT, '0');
  assert.equal(environment.SCCACHE_STARTUP_NOTIFY, '/managed/.rk-sccache-notify.sock');
  assert.equal(environment.SCCACHE_SERVER_UDS, '/managed/.rk-sccache.sock');
  assert.equal(environment.SCCACHE_CACHE_SIZE, '21474836480');
  assert.equal(environment.SCCACHE_MAX_FRAME_LENGTH, String(64 * 1024 * 1024));
  assert.equal(environment.SCCACHE_ERROR_LOG, '/managed/state/sccache-server.log');
  assert.equal(environment.RUSTC_WRAPPER, undefined);
});

test('the server is spawned with umask 0o177 so its own socket is created 0600', async () => {
  const options = [];
  const listener = Object.assign(new EventEmitter(), { listen(_path, done) { done(); queueMicrotask(() => listener.emit('connection', Object.assign(new EventEmitter(), { on() { return this; } }))); }, close(done) { done?.(); } });
  const started = await startBrokerOwnedSccacheServer({
    tool: { path: '/managed/sccache' }, socketPath: '/managed/.rk-sccache.sock', notifyPath: '/managed/.rk-sccache-notify.sock',
    cacheDir: '/managed/compiler-cache', maxSizeBytes: 1_024, timeoutMs: 1_000,
    spawnManaged: (command, argv, spawnOptions) => { options.push({ command, argv, spawnOptions }); return fakeChild(41); },
    startManaged: async () => ({ pid: 4242 }),
    readIdentity: async () => ({ status: 'OK', identity: 'darwin:41:now' }),
    ownershipOf: (child, { identity }) => ({ pid: child.pid, platform: 'posix', identity: identity.identity }),
    createServer: () => listener,
    unlink: async () => {}
  });
  assert.equal(started.pid, 4242);
  assert.equal(options[0].spawnOptions.umask, 0o177);
  assert.equal(options[0].spawnOptions.env.SCCACHE_NO_DAEMON, '1');
  assert.deepEqual(options[0].argv, []);
  assert.equal(started.ownership.identity, 'darwin:41:now');
});

test('concurrent samples share one single-flight server start', async () => {
  const counts = [];
  const supervisor = managedSupervisor({ startServer: countingStart(counts, { delayMs: 20 }), spawn: fakeSpawn([{ stdout: '{}' }, { stdout: '{}' }, { stdout: '{}' }, { stdout: '{}' }], []) });
  const samples = await Promise.all([supervisor.sample({}), supervisor.sample({}), supervisor.sample({}), supervisor.sample({})]);
  assert.deepEqual(samples.map((sample) => sample.status), ['OK', 'OK', 'OK', 'OK']);
  assert.equal(counts.length, 1);
});

test('an exhausted restart budget latches DEGRADED instead of respawning forever', async () => {
  const counts = [];
  const supervisor = managedSupervisor({ startServer: countingStart(counts, { fail: new Error('server died') }), spawn: fakeSpawn([], []) });
  for (let attempt = 0; attempt < 3; attempt += 1) assert.equal((await supervisor.sample({})).reason, 'RK_SCCACHE_START_FAILED');
  const exhausted = await supervisor.sample({});
  assert.equal(exhausted.status, 'DEGRADED'); assert.equal(exhausted.reason, 'RK_SCCACHE_RESTART_BUDGET');
  assert.equal(counts.length, 3);
  assert.equal((await supervisor.sample({})).reason, 'RK_SCCACHE_RESTART_BUDGET');
});

test('a startup notification that never arrives degrades within its bound', async () => {
  const listener = Object.assign(new EventEmitter(), { listen(_path, done) { done(); }, close(done) { done?.(); } });
  const supervisor = managedSupervisor({
    spawn: fakeSpawn([], []),
    startServer: (options) => startBrokerOwnedSccacheServer({
      ...options, timeoutMs: 60,
      spawnManaged: () => fakeChild(41), startManaged: async () => ({ pid: 4242 }),
      readIdentity: async () => ({ status: 'OK', identity: 'darwin:41:now' }),
      createServer: () => listener, unlink: async () => {}
    }),
    cacheDir: '/managed/compiler-cache', maxSizeBytes: 1_024
  });
  const sample = await supervisor.sample({});
  assert.equal(sample.status, 'DEGRADED'); assert.equal(sample.reason, 'RK_SCCACHE_START_TIMEOUT');
});

test('shutdown terminates the owned tree, removes both sockets & is idempotent', async () => {
  const unlinked = []; const terminated = [];
  const supervisor = managedSupervisor({
    startServer: countingStart([]), spawn: fakeSpawn([{ stdout: '{}' }], []),
    terminate: async (child) => { terminated.push(child.pid); return { status: 'REAPED', reaped: true }; },
    unlink: async (path) => { unlinked.push(path); }
  });
  assert.equal((await supervisor.sample({})).status, 'OK');
  const first = await supervisor.shutdown();
  assert.equal(first.reap.reaped, true);
  assert.deepEqual(terminated, [4241]);
  assert.deepEqual(unlinked, ['/managed/.rk-sccache.sock', '/managed/.rk-sccache-notify.sock']);
  assert.equal(await supervisor.shutdown(), first);
  assert.deepEqual(terminated, [4241]); assert.equal(unlinked.length, 2);
});

test('stats failure degrades diagnostics without blocking builds', async () => {
  const supervisor = managedSupervisor({ startServer: countingStart([]), spawn: fakeSpawn([{ code: 1 }], []) });
  const sample = await supervisor.sample({}); assert.equal(sample.status, 'DEGRADED'); assert.equal(sample.fallback, 'DIRECT_RUSTC_AVAILABLE'); assert.equal(supervisor.delta(sample, sample).status, 'DEGRADED');
});

test('sccache is disabled by default and never spawns an unowned server', async () => {
  const calls = []; const counts = [];
  const supervisor = new SccacheSupervisor({ tool: { path: '/managed/sccache' }, spawn: fakeSpawn([], calls), startServer: countingStart(counts) });
  const sample = await supervisor.sample({ RUSTC_WRAPPER: '/managed/sccache' });
  assert.equal(sample.status, 'DEGRADED'); assert.equal(sample.reason, 'RK_SCCACHE_DISABLED'); assert.deepEqual(calls, []); assert.deepEqual(counts, []);
});

test('managed mode returns typed degraded diagnostic when child ownership is unproven', async () => {
  const calls = [];
  const supervisor = managedSupervisor({ ownershipProof: async () => ({ owned: false, reason: 'server escaped launcher process group' }), startServer: countingStart([]), spawn: fakeSpawn([], calls) });
  const sample = await supervisor.sample({});
  assert.equal(sample.status, 'DEGRADED'); assert.equal(sample.reason, 'RK_SCCACHE_OWNERSHIP_UNPROVEN'); assert.deepEqual(calls, []);
});

test('direct-rustc fallback is typed & removes only cache execution routing', () => {
  const fallback = directRustcFallback({ RUSTC_WRAPPER: '/managed/sccache', SCCACHE_SERVER_UDS: '/managed/socket', RUSTC: '/managed/rustc', KEEP: '1' }, { status: 'DEGRADED', reason: 'stats-timeout' });
  assert.equal(fallback.status, 'FALLBACK_DIRECT_RUSTC'); assert.equal(fallback.reason, 'stats-timeout'); assert.deepEqual(fallback.environment, { RUSTC: '/managed/rustc', KEEP: '1' });
});

const SERVER_PID = 4242;
const descendants = (pid = SERVER_PID, command = '/managed/sccache') => async () => new Map([[pid, Object.freeze({ pid, command })]]);
const statsSpawn = (payload = '{"compile_requests":1}') => fakeSpawn([{ code: 0, stdout: payload }], []);
async function socketRoot() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'rk-sccache-proof-'));
  const socketPath = brokerSccacheSocketPath(root);
  const server = net.createServer();
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, resolve); });
  await chmod(socketPath, 0o600);
  return { root, socketPath, cleanup: async () => { await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true, force: true }); } };
}
const proofFor = (item, overrides = {}) => brokerOwnedSccacheProof({ externalRoot: item.root, spawn: statsSpawn(), liveDescendants: descendants(), serverPid: SERVER_PID, timeoutMs: 100, ...overrides });
const ownedEnvironment = (item) => ({ SCCACHE_SERVER_UDS: item.socketPath });
const tool = { path: '/managed/sccache' };

test('broker ownership proof accepts only its own socket, uid, mode, descendant & live stats', async () => {
  const item = await socketRoot();
  try {
    const proof = await proofFor(item)({ environment: ownedEnvironment(item), tool });
    assert.equal(proof.owned, true); assert.equal(proof.socketPath, item.socketPath); assert.equal(proof.serverPid, SERVER_PID);
  } finally { await item.cleanup(); }
});

test('broker ownership proof refuses a mismatched SCCACHE_SERVER_UDS', async () => {
  const item = await socketRoot();
  try {
    for (const environment of [{}, { SCCACHE_SERVER_UDS: `${item.socketPath}x` }, { SCCACHE_SERVER_UDS: path.join(os.tmpdir(), 'other.sock') }]) {
      const proof = await proofFor(item)({ environment, tool });
      assert.deepEqual(proof, { owned: false, reason: 'sccache-socket-path-mismatch' });
    }
  } finally { await item.cleanup(); }
});

test('broker ownership proof refuses a non-socket path & a foreign-uid socket', async () => {
  const item = await socketRoot();
  try {
    const foreign = await proofFor(item, { uid: 0, lstat: async () => ({ isSocket: () => true, uid: 12_345, mode: 0o140600 }) })({ environment: ownedEnvironment(item), tool });
    assert.deepEqual(foreign, { owned: false, reason: 'sccache-socket-foreign-uid' });
    const permissive = await proofFor(item, { lstat: async () => ({ isSocket: () => true, uid: process.getuid(), mode: 0o140666 }) })({ environment: ownedEnvironment(item), tool });
    assert.deepEqual(permissive, { owned: false, reason: 'sccache-socket-mode-unsafe' });
  } finally { await item.cleanup(); }
  const plain = await mkdtemp(path.join(os.tmpdir(), 'rk-sccache-plain-'));
  try {
    await writeFile(brokerSccacheSocketPath(plain), 'not-a-socket');
    const proof = await brokerOwnedSccacheProof({ externalRoot: plain, spawn: statsSpawn(), liveDescendants: descendants(), serverPid: SERVER_PID, timeoutMs: 100 })({ environment: { SCCACHE_SERVER_UDS: brokerSccacheSocketPath(plain) }, tool });
    assert.deepEqual(proof, { owned: false, reason: 'sccache-socket-not-a-socket' });
  } finally { await rm(plain, { recursive: true, force: true }); }
});

test('broker ownership proof refuses a server pid that is not a live broker descendant', async () => {
  const item = await socketRoot();
  try {
    const foreignPid = await proofFor(item, { serverPid: SERVER_PID + 1 })({ environment: ownedEnvironment(item), tool });
    assert.deepEqual(foreignPid, { owned: false, reason: 'sccache-server-not-a-broker-descendant' });
    const noDescendant = await proofFor(item, { serverPid: null, liveDescendants: async () => new Map() })({ environment: ownedEnvironment(item), tool });
    assert.deepEqual(noDescendant, { owned: false, reason: 'sccache-server-pid-unknown' });
  } finally { await item.cleanup(); }
});

test('broker ownership proof refuses when bounded stats do not answer over its socket', async () => {
  const item = await socketRoot();
  try {
    assert.deepEqual(await proofFor(item, { spawn: fakeSpawn([{ code: 1 }], []) })({ environment: ownedEnvironment(item), tool }), { owned: false, reason: 'sccache-stats-unavailable' });
    assert.deepEqual(await proofFor(item, { spawn: fakeSpawn([{ code: 0, stdout: 'not json' }], []) })({ environment: ownedEnvironment(item), tool }), { owned: false, reason: 'sccache-stats-malformed' });
  } finally { await item.cleanup(); }
});

test('a stale socket left by an earlier process is removed before a provable one is created', async () => {
  const item = await socketRoot();
  try {
    assert.equal(await prepareBrokerSccacheSocket({ externalRoot: item.root }), item.socketPath);
    const proof = await proofFor(item)({ environment: ownedEnvironment(item), tool });
    assert.deepEqual(proof, { owned: false, reason: 'sccache-socket-missing' });
    assert.equal(await prepareBrokerSccacheSocket({ externalRoot: item.root }), item.socketPath);
  } finally { await item.cleanup(); }
});

const SCCACHE_BINARY = process.env.RK_SCCACHE_PATH ?? '/opt/homebrew/bin/sccache';
// The only test that exercises all three real blockers at once: start ordering,
// foreground (provable) daemonization, and the 0755 socket the server would
// otherwise create for itself.
test('a real broker-owned sccache server is provable, 0600 & fully reclaimed', { skip: existsSync(SCCACHE_BINARY) ? false : `sccache is absent at ${SCCACHE_BINARY}` }, async () => {
  // A deep mkdtemp root would exceed sun_path; keep this one short on purpose.
  const root = await mkdtemp('/tmp/rk-sccache-live-');
  const tool = { path: SCCACHE_BINARY };
  const socketPath = brokerSccacheSocketPath(root); const notifyPath = brokerSccacheNotifyPath(root);
  let supervisor = null;
  try {
    supervisor = new SccacheSupervisor({
      tool, mode: 'managed', externalRoot: root,
      cacheDir: path.join(root, 'compiler-cache'), maxSizeBytes: 1_073_741_824,
      errorLogPath: path.join(root, 'sccache-server.log'),
      timeoutMs: 20_000, startTimeoutMs: 30_000,
      ownershipProof: brokerOwnedSccacheProof({ externalRoot: root, socketPath, serverPid: () => supervisor.serverPid, timeoutMs: 20_000 })
    });
    const jobEnvironment = { PATH: process.env.PATH, HOME: process.env.HOME, SCCACHE_SERVER_UDS: socketPath };
    const sample = await supervisor.sample(jobEnvironment);
    assert.equal(sample.status, 'OK', JSON.stringify(sample));
    assert.equal(typeof sample.stats.stats.compile_requests, 'number');
    assert.equal((await lstat(socketPath)).mode & 0o777, 0o600);
    const descendants = await enumerateLiveDescendants(process.pid, { timeoutMs: 5_000 });
    assert.ok(descendants.has(supervisor.serverPid), `server ${supervisor.serverPid} is not a broker descendant`);
    const serverPid = supervisor.serverPid;
    const outcome = await supervisor.shutdown();
    assert.equal(outcome.reap.reaped, true, JSON.stringify(outcome));
    assert.throws(() => process.kill(serverPid, 0), (error) => error.code === 'ESRCH');
    assert.equal(existsSync(socketPath), false);
    assert.equal(existsSync(notifyPath), false);
  } finally { await supervisor?.shutdown().catch(() => null); await rm(root, { recursive: true, force: true }); }
});
