import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { brokerOwnedSccacheProof, brokerSccacheSocketPath, directRustcFallback, prepareBrokerSccacheSocket, SccacheSupervisor } from './sccache-supervisor.mjs';

function fakeSpawn(responses, calls) { return (_command, argv) => { calls.push(argv); const child = Object.assign(new EventEmitter(), { stdout: new EventEmitter(), stderr: new EventEmitter(), kill() {} }); const response = responses.shift(); queueMicrotask(() => { if (response.stdout) child.stdout.emit('data', response.stdout); child.emit('close', response.code ?? 0); }); return child; }; }

test('one supervised server provides bounded before/after stats deltas', async () => {
  const calls = []; const supervisor = new SccacheSupervisor({ tool: { path: '/managed/sccache' }, mode: 'managed', ownershipProof: async () => true, spawn: fakeSpawn([{ code: 0 }, { stdout: '{"compile_requests":2,"cache_hits":{"Rust":1}}' }, { stdout: '{"compile_requests":5,"cache_hits":{"Rust":3}}' }], calls), timeoutMs: 100 });
  const before = await supervisor.sample({}); const after = await supervisor.sample({}); const delta = supervisor.delta(before, after);
  assert.deepEqual(calls, [['--start-server'], ['--show-stats', '--stats-format=json'], ['--show-stats', '--stats-format=json']]); assert.equal(delta.delta.compile_requests, 3); assert.equal(delta.delta['cache_hits.Rust'], 2);
});

test('stats failure degrades diagnostics without blocking builds', async () => {
  const supervisor = new SccacheSupervisor({ tool: { path: '/managed/sccache' }, mode: 'managed', ownershipProof: async () => true, spawn: fakeSpawn([{ code: 0 }, { code: 1 }], []), timeoutMs: 100 });
  const sample = await supervisor.sample({}); assert.equal(sample.status, 'DEGRADED'); assert.equal(sample.fallback, 'DIRECT_RUSTC_AVAILABLE'); assert.equal(supervisor.delta(sample, sample).status, 'DEGRADED');
});

test('sccache is disabled by default and never spawns an unowned server', async () => {
  const calls = [];
  const supervisor = new SccacheSupervisor({ tool: { path: '/managed/sccache' }, spawn: fakeSpawn([], calls) });
  const sample = await supervisor.sample({ RUSTC_WRAPPER: '/managed/sccache' });
  assert.equal(sample.status, 'DEGRADED'); assert.equal(sample.reason, 'RK_SCCACHE_DISABLED'); assert.deepEqual(calls, []);
});

test('managed mode returns typed degraded diagnostic when child ownership is unproven', async () => {
  const calls = [];
  const supervisor = new SccacheSupervisor({ tool: { path: '/managed/sccache' }, mode: 'managed', ownershipProof: async () => ({ owned: false, reason: 'server escaped launcher process group' }), spawn: fakeSpawn([], calls) });
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
