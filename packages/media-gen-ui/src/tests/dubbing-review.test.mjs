import test from 'node:test';
import assert from 'node:assert/strict';

// The resolver hook must be registered before the .ts modules load, so these imports are dynamic.
await import('./ts-resolve.mjs');
const { checkDubbingLanguages, dubMaxTempoOf, dubReviewOf, PIPELINE_TEMPLATES } = await import('../lib/pipeline-workflows.ts');
const { mockEngine } = await import('../lib/browser-mock-api.ts');

const seg = (id, start_ms, end_ms, synth_ms, text = 'Bonjour.') => ({ id, start_ms, end_ms, text, source_text: 'Hello.', synth_ms });
const planInput = (segments, over = {}) => ({ source_lang: 'en', target_lang: 'fr', segments, ...over });
const dubPlan = (input) => mockEngine('command', { name: 'studio.pipelines.dubbing_plan', input });
/** The needs_review asset shape dub_plan settles (dubbing.rs plan_step), built from the plan. */
const assetOf = (plan) => ({ review_status: 'needs_review', report: plan.segments, options: plan.options, summary: plan.summary });

test('the dubbing language contract: explicit, different, and passthrough only for a media test', () => {
  assert.equal(checkDubbingLanguages({ source_lang: 'en', target_lang: 'EN' })?.code, 'dubbing_same_language');
  assert.equal(checkDubbingLanguages({ source_lang: 'en', target_lang: 'en', passthrough: true }), null);
  assert.equal(checkDubbingLanguages({ source_lang: 'en', target_lang: 'en', passthrough: 'yes' }), null);
  assert.equal(checkDubbingLanguages({ source_lang: 'en' })?.code, 'invalid_input');
  assert.equal(checkDubbingLanguages({ source_lang: 'en', target_lang: 'fr' }), null);
});

test('the dubbing template runs the engine order: dub, dub_plan, srt_export, compose', () => {
  const tpl = PIPELINE_TEMPLATES.find((t) => t.id === 'opencreator_dubbing');
  assert.deepEqual(tpl.steps.map((s) => s.id), ['transcribe', 'translate', 'dub', 'dub_plan', 'srt_export', 'compose']);
  const plan = tpl.steps.find((s) => s.id === 'dub_plan');
  assert.deepEqual(plan.depends_on, ['dub']);
  assert.equal(plan.native, 'dub_plan');
  assert.equal(plan.retry.max_attempts, 6);
  assert.deepEqual(tpl.steps.find((s) => s.id === 'srt_export').depends_on, ['dub_plan']);
  assert.deepEqual(tpl.steps.find((s) => s.id === 'compose').depends_on, ['srt_export', 'dub_plan']);
  for (const key of ['source_lang', 'target_lang', 'passthrough', 'original_audio', 'max_tempo', 'allow_shift', 'min_gap_ms']) {
    assert.ok(tpl.inputs.includes(key), `template input ${key}`);
  }
});

test('the speed-up limit a run carries defaults to 1.15 and reads a typed or numeric value', () => {
  assert.equal(dubMaxTempoOf({}), 1.15);
  assert.equal(dubMaxTempoOf({ max_tempo: '' }), 1.15);
  assert.equal(dubMaxTempoOf({ max_tempo: '1.3' }), 1.3);
  assert.equal(dubMaxTempoOf({ max_tempo: 1.25 }), 1.25);
});

test('a needs_review plan maps to the table and the three explicit choices, with the reported drift', async () => {
  const plan = await dubPlan(planInput([seg('1', 0, 1000, 1500, 'Bonjour trop long.'), seg('2', 1000, 2000, 950, 'Suite.')]));
  assert.equal(plan.status, 'needs_review');
  const review = dubReviewOf(assetOf(plan), 1.15);
  assert.equal(review.rows.length, 2);
  assert.equal(review.rows[0].action, 'overflow');
  assert.equal(review.rows[0].excess_ms, 500);
  assert.equal(review.rows[0].target_text, 'Bonjour trop long.');
  assert.equal(review.suggested_max_tempo, 1.5);

  const raise = review.choices.find((c) => c.id === 'raise_max_tempo');
  assert.equal(raise.enabled, true);
  assert.equal(raise.label, 'Raise the speed-up limit to 1.50');
  assert.deepEqual(raise.inputs, { max_tempo: 1.5 });

  const shift = review.choices.find((c) => c.id === 'allow_shift');
  assert.deepEqual(shift.inputs, { allow_shift: true });
  assert.equal(shift.drift_ms, 305, 'the drift is the engine preview text, not an estimate');

  const shorten = review.choices.find((c) => c.id === 'shorten_translation');
  assert.equal(shorten.inputs, null, 'shortening the translation is not a retry');
  assert.deepEqual(shorten.segment_ids, ['1']);
});

test('a ratio above the single-stage ceiling disables the speed-up choice and says why', async () => {
  const plan = await dubPlan(planInput([seg('1', 0, 1000, 2500, 'Très long.'), seg('2', 1000, 2000, 900)]));
  const review = dubReviewOf(assetOf(plan), 1.15);
  const raise = review.choices.find((c) => c.id === 'raise_max_tempo');
  assert.equal(raise.enabled, false);
  assert.match(raise.reason, /above the 2\.0 single-stage ceiling/);
  assert.ok(review.choices.some((c) => c.id === 'allow_shift' && c.enabled), 'shifting stays available');
});

test('a plan held only by the media length keeps the engine note and offers no retry choice', async () => {
  const plan = await dubPlan(planInput([seg('1', 0, 1000, 900), seg('2', 1000, 2000, 900)], { media_duration_ms: 1500 }));
  assert.equal(plan.status, 'needs_review');
  const review = dubReviewOf(assetOf(plan), 1.15);
  assert.deepEqual(review.choices, []);
  assert.equal(review.suggested_max_tempo, null);
  assert.ok(review.notes.some((n) => n.startsWith('lengthen the video')));
});

test('the browser mirror refuses the same language and blank-only input as the engine', async () => {
  await assert.rejects(dubPlan(planInput([seg('1', 0, 1000, 900)], { target_lang: 'en' })), /dubbing_same_language/);
  await assert.rejects(dubPlan(planInput([seg('1', 0, 1000, 0, '')])), /every segment is blank/);
  await assert.rejects(dubPlan(planInput([seg('1', 0, 1000, 900)], { max_tempo: 2.5 })), /max_tempo must be between/);
});
