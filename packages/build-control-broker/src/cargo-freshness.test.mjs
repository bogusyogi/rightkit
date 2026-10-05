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
