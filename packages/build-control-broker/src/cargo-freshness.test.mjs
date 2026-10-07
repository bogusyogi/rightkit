import assert from 'node:assert/strict';
import test from 'node:test';
import { CargoFreshnessTracker, classifyColdStart, probeTargetAtStart, wantsCargoJson } from './cargo-freshness.mjs';

test('tracker counts fresh/not-fresh across split chunks and caps package list', () => {
  const tracker = new CargoFreshnessTracker();
  for (let index = 0; index < 60; index += 1) tracker.feed(`{"reason":"compiler-artifact","package_id":"p${index}","fresh":false}\n`);
  tracker.feed('{"reason":"compiler-artifact","package_id":"f","fresh":true}\nnot json\n{"reason":"build-script-executed","package_id":"x"}');
  const summary = tracker.summary();
  assert.equal(summary.notFresh, 60); assert.equal(summary.fresh, 1); assert.equal(summary.buildScriptExecutedEvents, 1);
  assert.equal(summary.notFreshPackages.length, 50); assert.equal(summary.notFreshPackagesTruncated, 10);
});

test('cold start classification and json detection', async () => {
  assert.equal(wantsCargoJson(['build', '--message-format', 'json-render-diagnostics']), true);
  assert.equal(wantsCargoJson(['build', '--', '--message-format=json']), false);
  const missing = await probeTargetAtStart({ target: '/nonexistent-rk-target', generation: 3 });
  assert.equal(missing.existedAtStart, false);
  assert.equal(classifyColdStart(missing, null).coldStart, true);
  const warm = { existedAtStart: true, entriesAtStart: 5 };
  assert.equal(classifyColdStart(warm, { available: false }).coldStart, 'unknown');
  assert.equal(classifyColdStart(warm, { available: true, compilerArtifacts: 4, fresh: 0, notFresh: 4 }).coldStart, true);
  assert.equal(classifyColdStart(warm, { available: true, compilerArtifacts: 4, fresh: 3, notFresh: 1 }).coldStart, false);
});

test('fingerprint census counts rebuilt vs untouched units without a JSON stream', async () => {
  const { mkdtemp, mkdir, writeFile, utimes, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os'); const { join } = await import('node:path');
  const { cargoProfileDirectory, fingerprintCensus, fingerprintCensusStart } = await import('./cargo-freshness.mjs');
  assert.equal(cargoProfileDirectory(['build', '--profile', 'release-iterate']), 'release-iterate');
  assert.equal(cargoProfileDirectory(['build', '--release']), 'release');
  assert.equal(cargoProfileDirectory(['test', '--profile=dev']), 'debug');
  const root = await mkdtemp(join(tmpdir(), 'rk-census-'));
  try {
    const dir = join(root, 'x86_64-pc-windows-msvc', 'release-iterate', '.fingerprint');
    for (const unit of ['fresh-a', 'fresh-b', 'stale-c']) { await mkdir(join(dir, unit), { recursive: true }); await writeFile(join(dir, unit, 'lib'), 'h'); }
    const old = new Date(Date.now() - 60_000);
    for (const unit of ['fresh-a', 'fresh-b', 'stale-c']) await utimes(join(dir, unit, 'lib'), old, old);
    // Filesystem mtimes can trail Date.now() by a few ms (Windows); the census start sits between old and new.
    const start = await fingerprintCensusStart(root, ['build', '--profile', 'release-iterate'], 'x86_64-pc-windows-msvc', { now: () => Date.now() - 30_000 });
    assert.equal(start.unitsAtStart, 3);
    await writeFile(join(dir, 'stale-c', 'lib'), 'h2');
    await mkdir(join(dir, 'new-d')); await writeFile(join(dir, 'new-d', 'lib'), 'h');
    const census = await fingerprintCensus(start);
    assert.equal(census.available, true); assert.equal(census.source, 'fingerprint-census');
    assert.equal(census.notFresh, 2); assert.equal(census.fresh, 2); assert.equal(census.compilerArtifacts, 4);
    assert.deepEqual(classifyColdStart({ existedAtStart: true, entriesAtStart: 2 }, census), { coldStart: false, reason: 'target-nonempty-with-fresh-artifacts' });
  } finally { await rm(root, { recursive: true, force: true }); }
});
