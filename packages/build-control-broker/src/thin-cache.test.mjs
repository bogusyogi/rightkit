import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import { defaultThinCacheTimeoutMs, ThinCache, ThinCacheUnavailableError, thinCacheStatsDelta } from './thin-cache.mjs';

const paths = { cargoHome: '/managed/cargo-home', compilerCache: '/managed/compiler-cache', temporary: '/managed/tmp' };
const layout = { target: '/managed/workspace/g/001/target', temp: '/managed/workspace/g/001/tmp' };

function fakeChild(pid = 41) {
  const child = new EventEmitter();
  child.pid = pid; child.gatewayPid = pid + 1; child.exitCode = null; child.signalCode = null;
  child.stdout = new EventEmitter(); child.stderr = new EventEmitter();
  return child;
}

function harness(options = {}) {
  const children = []; const events = []; let connects = 0;
  const stats = JSON.stringify({ version: '0.17.0', cache_location: 'Local disk: "/managed/compiler-cache"', cache_size: 0, max_cache_size: 20 * 1024 ** 3, stats: {} });
  const cache = new ThinCache({
    paths, sccachePath: '/managed/tools/sccache', expectedVersion: '0.17.0', clientSideSupported: true,
    verifyTool: async () => ({ path: '/managed/tools/sccache' }),
    spawnGateway: async () => { const child = fakeChild(41 + children.length * 2); children.push(child); return child; },
    terminate: async (child) => { child.exitCode = 0; child.emit('close', 0, null); return { reaped: true }; },
    connect: async () => { connects += 1; }, run: async () => ({ stdout: stats }), emit: async (event) => { events.push(event); },
    ...options
  });
  return { cache, children, events, connects: () => connects };
}

test('cache telemetry probe timeout is no longer platform-shortened', () => {
  assert.equal(defaultThinCacheTimeoutMs('win32'), 10_000);
  assert.equal(defaultThinCacheTimeoutMs('darwin'), 10_000);
});

test('owns one gateway & injects strict cache environment with health proof', async () => {
  const calls = []; const { cache, children, events } = harness({ platform: 'linux', run: async (...args) => { calls.push(args); return { stdout: '{"version":"0.17.0","cache_location":"Local disk: \\"/managed/compiler-cache\\"","cache_size":0,"max_cache_size":21474836480,"stats":{"compile_requests":7}}' }; } });
  await cache.start();
  const prepared = await cache.prepare({ layout, cargoBuildJobs: 4, jobId: 'job-1' });
  assert.equal(children.length, 1);
  assert.equal(prepared.environment.CARGO_HOME, paths.cargoHome);
  assert.equal(prepared.environment.CARGO_TARGET_DIR, undefined);
  assert.equal(prepared.environment.CARGO_BUILD_TARGET_DIR, undefined);
  assert.equal(prepared.environment.CARGO_INSTALL_ROOT, undefined);
  assert.equal(prepared.environment.RUSTC_WRAPPER, '/managed/tools/sccache');
  assert.equal(prepared.environment.SCCACHE_CLIENT_SIDE, '1');
  assert.equal(prepared.environment.SCCACHE_START_SERVER, '0');
  assert.equal(prepared.environment.SCCACHE_NO_DAEMON, '1');
  assert.equal(prepared.environment.SCCACHE_IGNORE_SERVER_IO_ERROR, '0');
  assert.equal(prepared.environment.SCCACHE_LOCAL_RW_MODE, 'READ_WRITE');
  assert.equal(prepared.environment.SCCACHE_SERVER_UDS, '/managed/tmp/sccache-gateway.sock');
  assert.equal(prepared.environment.CARGO_BUILD_JOBS, '4');
  assert.equal(prepared.environment.CARGO_INCREMENTAL, '0');
  assert.equal(prepared.health.gatewayPid, 42);
  assert.equal(prepared.health.ownership, 'broker');
  assert.equal(prepared.strictNoFallback, true);
  assert.equal(calls.length, 1);
  assert.equal(events.some((event) => event.type === 'CACHE_GATEWAY_READY'), true);
  assert.equal(events.some((event) => event.type === 'CACHE_ADMISSION' && event.id === 'job-1'), true);
  const finished = await cache.finish(prepared);
  assert.equal(finished.status, 'NO_COMPILATION');
  assert.equal(finished.healthStatus, 'SERVICE_OK');
  assert.equal(finished.reuse.status, 'NO_COMPILATION');
  assert.equal(finished.attribution.scope, 'broker-exclusive-client-window');
  assert.equal(finished.attribution.exact, true);
  assert.deepEqual(finished.delta, { compile_requests: 0 });
  await cache.close();
});

test('Windows cache clients use broker-owned TCP gateway', async () => {
  const { cache } = harness({ platform: 'win32', gatewayPort: 43_217 });
  await cache.start();
  const prepared = await cache.prepare({ layout, jobId: 'windows-job' });
  assert.equal(prepared.environment.SCCACHE_SERVER_PORT, '43217');
  assert.equal(prepared.environment.SCCACHE_SERVER_UDS, undefined);
  await cache.close();
});

test('attributes exclusive client-side windows & refuses overlapping roots', async () => {
  let stats = { compile_requests: 0, cache_hits: { Rust: 0 }, cache_misses: { Rust: 0 } };
  const { cache } = harness({ run: async () => ({ stdout: JSON.stringify({ version: '0.17.0', cache_location: 'Local disk: "/managed/compiler-cache"', cache_size: 0, max_cache_size: 20 * 1024 ** 3, stats }) }) });
  await cache.start();
  const first = await cache.prepare({ layout, jobId: 'first' });
  const second = await cache.prepare({ layout, jobId: 'second' });
  stats = { compile_requests: 2, cache_hits: { Rust: 2 }, cache_misses: { Rust: 0 } };
  const firstResult = await cache.finish(first);
  const secondResult = await cache.finish(second);
  assert.equal(firstResult.status, 'NOT_MEASURED');
  assert.equal(firstResult.reason, 'concurrent-cache-clients');
  assert.equal(secondResult.status, 'NOT_MEASURED');
  assert.equal(secondResult.reason, 'concurrent-cache-clients');

  const exclusive = await cache.prepare({ layout, jobId: 'exclusive' });
  stats = { compile_requests: 3, cache_hits: { Rust: 3 }, cache_misses: { Rust: 0 } };
  const result = await cache.finish(exclusive);
  assert.equal(result.status, 'PROVEN');
  assert.deepEqual(result.reuse, { status: 'PROVEN', hits: 1, misses: 0, bypassed: 0, hitRate: 1 });
  assert.equal(result.attribution.scope, 'broker-exclusive-client-window');
  assert.equal(result.attribution.exact, true);
  await cache.close();
});

test('fresh lease avoids per-job stats processes & stale admissions share one probe', async () => {
  let now = 1_000; let release;
  const gate = new Promise((resolve) => { release = resolve; });
  let connects = 0; let block = false;
  const { cache } = harness({ now: () => now, healthLeaseMs: 10, connect: async () => { connects += 1; if (block) await gate; } });
  await cache.start();
  await cache.prepare({ layout });
  assert.equal(connects, 3);
  now += 11; block = true;
  const first = cache.prepare({ layout }); const second = cache.prepare({ layout });
  await Promise.resolve(); release();
  await Promise.all([first, second]);
  assert.equal(connects, 4);
  await cache.close();
});

test('stale health lease does not turn periodic stats into destructive gateway recovery', async () => {
  let now = 1_000; let statsCalls = 0;
  const { cache, children } = harness({ now: () => now, healthLeaseMs: 10, statsIntervalMs: 20, run: async () => { statsCalls += 1; return { stdout: '{"version":"0.17.0","cache_location":"Local disk: \\"/managed/compiler-cache\\"","cache_size":0,"max_cache_size":21474836480,"stats":{}}' }; } });
  await cache.start();
  assert.equal(statsCalls, 1);
  now += 21;
  await cache.prepare({ layout });
  assert.equal(statsCalls, 1, 'admission health must not synchronously collect telemetry');
  assert.equal(children.length, 1, 'slow telemetry must not restart gateway');
  await cache.close();
});

test('gateway startup failure hard-blocks build admission', async () => {
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', verifyTool: async () => ({ path: '/managed/tools/sccache' }), expectedVersion: '0.17.0', spawnGateway: async () => { throw Object.assign(new Error('offline'), { code: 'ENOENT' }); }, connect: async () => {} });
  await assert.rejects(cache.prepare({ layout }), (error) => {
    assert.equal(error instanceof ThinCacheUnavailableError, true);
    assert.equal(error.code, 'RK_SCCACHE_UNAVAILABLE');
    assert.equal(error.cache.status, 'BLOCKED');
    assert.equal(error.cache.strictNoFallback, true);
    assert.match(error.cache.reason, /ENOENT/);
    return true;
  });
});

test('re-verifies digest-pinned sccache before every gateway spawn', async () => {
  let verifications = 0;
  const { cache, children } = harness({ verifyTool: async () => { verifications += 1; return { path: '/managed/tools/sccache' }; } });
  await cache.start();
  children[0].exitCode = 1; children[0].emit('close', 1, null);
  await cache.prepare({ layout });
  assert.equal(verifications, 2);
  assert.equal(children.length, 2);
  await cache.close();
});

test('gateway exit invalidates health & reports unavailable root-cancellation signal', async () => {
  const unavailable = []; const { cache, children } = harness({ onUnavailable: async (event) => { unavailable.push(event); } });
  await cache.start();
  children[0].exitCode = 1; children[0].emit('close', 1, null);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(unavailable.length, 1);
  assert.equal(unavailable[0].reason, 'sccache-gateway-exited');
  assert.equal(unavailable[0].gatewayPid, 42);
  await cache.close();
});

test('stats collection is independent telemetry from owned gateway', async () => {
  const { cache } = harness({ epoch: 'epoch-1', run: async () => ({ stdout: '{"version":"0.17.0","cache_location":"Local disk: \\"/managed/compiler-cache\\"","cache_size":100,"max_cache_size":21474836480,"stats":{"compile_requests":7,"cache_hits":{"Rust":5},"text":"ignored"}}' }) });
  await cache.start();
  const sample = await cache.snapshot();
  assert.equal(sample.status, 'SERVICE_OK');
  assert.deepEqual(sample.stats, { compile_requests: 7, cache_hits: { Rust: 5 } });
  assert.equal(sample.attribution.serverIdentity, 42);
  await cache.close();
});

test('basedirs gate uses broker-trusted roots with platform deduplication', async () => {
  const { cache } = harness({ basedirsEnabled: true, basedirs: ['/operator/root'], platform: 'linux' });
  const prepared = await cache.prepare({ layout: { ...layout, root: '/managed/workspace/g/001' }, repositoryRoot: '/repo', workspaceRoot: '/repo/app' });
  assert.equal(prepared.environment.SCCACHE_BASEDIRS, '/repo:/repo/app:/managed/workspace/g/001:/operator/root');
  assert.equal(cache.basedirsActive, true);
  await cache.close();
});

test('disabled cache blocks build admission explicitly', async () => {
  const cache = new ThinCache({ paths, enabled: false });
  await assert.rejects(cache.prepare({ layout }), (error) => {
    assert.equal(error.cache.status, 'BLOCKED');
    assert.equal(error.cache.healthStatus, 'DISABLED');
    assert.equal(error.cache.reason, 'sccache-disabled');
    assert.equal(error.cache.strictNoFallback, true);
    return true;
  });
});

test('numeric stats delta stays bounded to numeric fields', () => {
  assert.deepEqual(thinCacheStatsDelta({ a: 4, nested: { b: 8 }, counts: {} }, { a: 9, nested: { b: 10 }, counts: { Rust: 3 }, text: 'ignored' }), { a: 5, nested: { b: 2 }, counts: { Rust: 3 } });
});
