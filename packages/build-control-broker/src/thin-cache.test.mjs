import assert from 'node:assert/strict';
import test from 'node:test';
import { defaultThinCacheTimeoutMs, ThinCache, thinCacheStatsDelta } from './thin-cache.mjs';

const paths = { cargoHome: '/managed/cargo-home', compilerCache: '/managed/compiler-cache', tools: '/managed/tools' };
const layout = { target: '/managed/workspace/generations/000001/target', temp: '/managed/workspace/generations/000001/tmp' };

test('cache sampling allows measured Windows process startup overhead', () => {
  assert.equal(defaultThinCacheTimeoutMs('win32'), 5_000);
  assert.equal(defaultThinCacheTimeoutMs('darwin'), 2_000);
});

test('injects shared Cargo/sccache & persistent generation environment', async () => {
  const calls = [];
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', clientSideSupported: true, run: async (...args) => { calls.push(args); return { stdout: '{"compile_requests":7,"cache_hits":{"Rust":5}}' }; } });
  const prepared = await cache.prepare({ layout, cargoBuildJobs: 4 });
  assert.equal(prepared.mode, 'sccache');
  assert.equal(prepared.environment.CARGO_HOME, paths.cargoHome);
  assert.equal(prepared.environment.CARGO_TARGET_DIR, layout.target);
  assert.equal(prepared.environment.CARGO_INSTALL_ROOT, paths.tools);
  assert.equal(prepared.environment.CARGO_BUILD_BUILD_DIR, undefined);
  assert.equal(prepared.environment.RUSTC_WRAPPER, '/managed/tools/sccache');
  assert.equal(prepared.environment.SCCACHE_CLIENT_SIDE, '1');
  assert.equal(prepared.environment.CARGO_BUILD_JOBS, '4');
  assert.deepEqual(calls[0][1], ['--show-stats', '--stats-format=json']);
  const finished = await cache.finish(prepared);
  assert.equal(finished.status, 'SERVICE_OK');
  assert.equal(finished.delta, null);
  assert.equal(finished.reason, 'client-side-counters-unattributable');
  assert.equal(finished.attribution.scope, 'client-side-counters-unattributable');
  assert.equal(calls.length, 1);
});

test('cache probe failure visibly degrades to direct rustc for job', async () => {
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', run: async () => { throw Object.assign(new Error('offline'), { code: 'ENOENT' }); } });
  const prepared = await cache.prepare({ layout });
  assert.equal(prepared.mode, 'direct');
  assert.equal(prepared.status, 'DEGRADED');
  assert.match(prepared.reason, /ENOENT/);
  assert.equal(prepared.environment.RUSTC_WRAPPER, undefined);
});

test('cache probe failure preserves bounded stderr diagnosis', async () => {
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', run: async () => { throw Object.assign(new Error('exit'), { code: 2, stderr: 'socket ownership failed\n' }); } });
  const prepared = await cache.prepare({ layout });
  assert.equal(prepared.reason, 'sccache-stats-unavailable:2:socket ownership failed');
});

test('records bounded numeric cache deltas', async () => {
  const samples = ['{"version":"0.17","cache_location":"Disk","stats":{"compile_requests":7,"cache_hits":{"Rust":5},"text":"ignored"}}', '{"version":"0.17","cache_location":"Disk","stats":{"compile_requests":10,"cache_hits":{"Rust":7},"text":"ignored"}}'];
  let now = 1_000;
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', epoch: 'epoch-1', now: () => now += 1_000, run: async () => ({ stdout: samples.shift() }) });
  const prepared = await cache.prepare({ layout });
  const result = await cache.finish(prepared);
  assert.deepEqual(result.delta, { compile_requests: 3, cache_hits: { Rust: 2 } });
  assert.equal(result.status, 'SERVICE_OK');
  assert.equal(result.attribution.epoch, 'epoch-1');
  assert.equal(result.attribution.scope, 'shared-cumulative-root-window');
  assert.equal(result.attribution.windowStartedAt, '1970-01-01T00:00:02.000Z');
  assert.equal(result.attribution.windowEndedAt, '1970-01-01T00:00:03.000Z');
  assert.equal(result.attribution.serverIdentity, null);
  assert.deepEqual(thinCacheStatsDelta({ a: 4 }, { a: 9 }), { a: 5 });
});

test('counter reset degrades attribution instead of emitting negative deltas', async () => {
  const samples = ['{"stats":{"compile_requests":10}}', '{"stats":{"compile_requests":1}}'];
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', run: async () => ({ stdout: samples.shift() }) });
  const result = await cache.finish(await cache.prepare({ layout }));
  assert.equal(result.status, 'DEGRADED');
  assert.equal(result.reason, 'sccache-counter-reset');
  assert.equal(result.delta, null);
});

test('disabled cache remains explicit', async () => {
  const cache = new ThinCache({ paths, enabled: false });
  const prepared = await cache.prepare({ layout });
  assert.deepEqual(await cache.finish(prepared), { mode: 'direct', status: 'DISABLED', delta: null });
});
