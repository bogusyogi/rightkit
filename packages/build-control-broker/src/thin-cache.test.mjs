import assert from 'node:assert/strict';
import test from 'node:test';
import { ThinCache, thinCacheStatsDelta } from './thin-cache.mjs';

const paths = { cargoHome: '/managed/cargo-home', compilerCache: '/managed/compiler-cache', tools: '/managed/tools' };
const layout = { target: '/managed/workspace/generations/000001/target', temp: '/managed/workspace/generations/000001/tmp' };

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
});

test('cache probe failure visibly degrades to direct rustc for job', async () => {
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', run: async () => { throw Object.assign(new Error('offline'), { code: 'ENOENT' }); } });
  const prepared = await cache.prepare({ layout });
  assert.equal(prepared.mode, 'direct');
  assert.equal(prepared.status, 'DEGRADED');
  assert.match(prepared.reason, /ENOENT/);
  assert.equal(prepared.environment.RUSTC_WRAPPER, undefined);
});

test('records bounded numeric cache deltas', async () => {
  const samples = ['{"compile_requests":7,"cache_hits":{"Rust":5},"text":"ignored"}', '{"compile_requests":10,"cache_hits":{"Rust":7},"text":"ignored"}'];
  const cache = new ThinCache({ paths, sccachePath: '/managed/tools/sccache', run: async () => ({ stdout: samples.shift() }) });
  const prepared = await cache.prepare({ layout });
  const result = await cache.finish(prepared);
  assert.deepEqual(result.delta, { compile_requests: 3, cache_hits: { Rust: 2 } });
  assert.deepEqual(thinCacheStatsDelta({ a: 4 }, { a: 9 }), { a: 5 });
});

test('disabled cache remains explicit', async () => {
  const cache = new ThinCache({ paths, enabled: false });
  const prepared = await cache.prepare({ layout });
  assert.deepEqual(await cache.finish(prepared), { mode: 'direct', status: 'DISABLED', delta: null });
});
