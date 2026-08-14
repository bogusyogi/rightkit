import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { directRustcFallback, SccacheSupervisor } from './sccache-supervisor.mjs';

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
