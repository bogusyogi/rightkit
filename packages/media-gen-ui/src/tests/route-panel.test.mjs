import test from 'node:test';
import assert from 'node:assert/strict';
import {
  estimateLabel,
  fixHints,
  isChoosable,
  paidChoices,
  preferenceLabel,
  preferenceNote,
  routeFromError,
  routeFromResponse,
  routeId,
  routeInput,
  settingsSectionFor,
  stateLabels
} from '../views/route-panel-logic.ts';
import { settingsSectionOrDefault, SETTINGS_SECTION_IDS } from '../lib/nav.ts';
import { mockRouting } from '../lib/browser-mock-api.ts';

const alt = (provider, model, states, extra = {}) => ({
  provider,
  model,
  cloud: true,
  states: { configured: true, installed: null, qualified: true, usable: true, ...states },
  est_usd: 0.05,
  est_source: 'default_output',
  eligible: true,
  reason: 'usable',
  ...extra
});

const answer = (outcome, alternatives = [], extra = {}) => ({
  kind: 'image',
  outcome,
  route_choice_required: outcome === 'route_choice_required',
  basis: 'none',
  chosen: null,
  alternatives,
  preferences: { privacy: 'any', spend: 'ask', quality: 'balanced', time: 'balanced' },
  selection: null,
  selection_note: null,
  note: 'note text',
  preferences_not_applied: ['quality', 'time'],
  preferences_not_applied_reason: 'the catalogue records no quality or latency facts',
  ...extra
});

test('the four states read as separate labels, and a null state reads n/a', () => {
  const labels = stateLabels({ configured: false, installed: null, qualified: true, usable: false });
  assert.deepEqual(
    labels.map((s) => [s.label, s.value]),
    [
      ['Key', 'no'],
      ['Installed', 'n/a'],
      ['Price fresh', 'yes'],
      ['Usable', 'no']
    ]
  );
  assert.deepEqual(stateLabels({ configured: true, installed: true, qualified: null, usable: true }).map((s) => s.value), ['yes', 'yes', 'n/a', 'yes']);
});

test('chip tone follows the value: yes is ok, no is bad, n/a is neutral, and the text carries the value too', () => {
  const tones = stateLabels({ configured: true, installed: false, qualified: null, usable: true }).map((s) => [s.key, s.tone, s.value]);
  assert.deepEqual(tones, [
    ['configured', 'ok', 'yes'],
    ['installed', 'bad', 'no'],
    ['qualified', 'neutral', 'n/a'],
    ['usable', 'ok', 'yes']
  ]);
  // A missing state (an older engine answer) is no check, not a negative.
  assert.deepEqual(stateLabels({}).map((s) => s.value), ['n/a', 'n/a', 'n/a', 'n/a']);
});

test('a fix hint opens the Settings section whose id it names', () => {
  assert.equal(settingsSectionFor('API keys'), 'api-keys');
  assert.equal(settingsSectionFor('Models'), 'models');
  for (const section of ['API keys', 'Models']) {
    assert.ok(SETTINGS_SECTION_IDS.includes(settingsSectionFor(section)), section);
  }
});

test('a requested Settings section opens only when it exists, else Settings opens its default', () => {
  assert.equal(settingsSectionOrDefault('models'), 'models');
  assert.equal(settingsSectionOrDefault('api-keys'), 'api-keys');
  assert.equal(settingsSectionOrDefault('no-such-section'), undefined);
  assert.equal(settingsSectionOrDefault(''), undefined);
  assert.equal(settingsSectionOrDefault(null), undefined);
  assert.equal(settingsSectionOrDefault(undefined), undefined);
});

test('the Settings section ids match the list the scenario expects, in its order', () => {
  const labelFor = { appearance: 'Appearance', setup: 'Setup', legal: 'Legal', 'api-keys': 'API keys', models: 'Models', spend: 'Spend', engine: 'Engine', about: 'About' };
  assert.deepEqual(SETTINGS_SECTION_IDS.map((id) => labelFor[id]), ['Appearance', 'Setup', 'Legal', 'API keys', 'Models', 'Spend', 'Engine', 'About']);
});

test('each engine outcome maps to its view and keeps the result', () => {
  const cases = [
    ['chosen', 'chosen'],
    ['route_choice_required', 'choice'],
    ['no_usable_route', 'none'],
    ['ambiguous_route', 'ambiguous']
  ];
  for (const [outcome, status] of cases) {
    const loaded = routeFromResponse(answer(outcome));
    assert.equal(loaded.ok, true, outcome);
    assert.equal(loaded.view.status, status);
    assert.equal(loaded.view.result.outcome, outcome);
  }
});

test('an answer the panel cannot read is a message, never a guessed route', () => {
  assert.equal(routeFromResponse(null).ok, false);
  assert.equal(routeFromResponse({ outcome: 'free_route', alternatives: [] }).ok, false);
  assert.equal(routeFromResponse({ outcome: 'chosen' }).ok, false, 'alternatives list required');
});

test('a route_choice_required refusal arriving as an error reads as the choice view', () => {
  const details = answer('route_choice_required', [alt('fal', 'flux', {}, { est_usd: 0.0065 })]);
  const loaded = routeFromError('route_choice_required', details, 'usable: fal/flux');
  assert.equal(loaded.ok, true);
  assert.equal(loaded.view.status, 'choice');
});

test('a no_usable_route refusal arriving as an error reads as the none view with its reasons', () => {
  const details = answer('no_usable_route', [alt('elevenlabs', 'v4', { configured: false, usable: false }, { reason: 'no key for elevenlabs' })]);
  const loaded = routeFromError('no_usable_route', details, 'no usable voice route');
  assert.equal(loaded.ok, true);
  assert.equal(loaded.view.status, 'none');
  assert.equal(loaded.view.result.alternatives[0].reason, 'no key for elevenlabs');
});

test('other failures, and refusals without a readable result, are their text', () => {
  assert.deepEqual(routeFromError('invalid_input', null, 'privacy must be one of'), { ok: false, message: 'privacy must be one of' });
  assert.deepEqual(routeFromError('route_choice_required', null, 'name provider'), { ok: false, message: 'name provider' });
  assert.deepEqual(routeFromError(null, null, '   '), { ok: false, message: 'The route check failed.' });
});

test('estimates read free, per request, or at default size', () => {
  assert.equal(estimateLabel({ est_usd: 0, est_source: 'default_output' }), 'free');
  assert.equal(estimateLabel({ est_usd: 0.0065, est_source: 'request' }), '$0.0065 for this request');
  assert.equal(estimateLabel({ est_usd: 0.04, est_source: 'default_output' }), '$0.0400 at default size');
});

test('only usable, eligible, paid alternatives are choices, and a choice is pickable only when the picker shows it', () => {
  const usablePaid = alt('fal', 'flux', {}, { est_usd: 0.0065 });
  const free = alt('local-x', 'm', { installed: true, configured: true }, { cloud: false, est_usd: 0, reason: 'free' });
  const noKey = alt('elevenlabs', 'v4', { configured: false, usable: false });
  const outOfPicker = alt('wavespeed', 'qwen', {}, { est_usd: 0.02 });
  const choices = paidChoices([usablePaid, free, noKey, outOfPicker], ['fal/flux', 'local-x/m']);
  assert.deepEqual(choices.map((c) => [c.id, c.pickable, c.estimate]), [
    ['fal/flux', true, '$0.0065 at default size'],
    ['wavespeed/qwen', false, '$0.0200 at default size']
  ]);
  const view = routeFromResponse(answer('route_choice_required', [usablePaid])).view;
  assert.equal(isChoosable(view, usablePaid), true);
  assert.equal(isChoosable(view, free), false, 'a free route is never offered as a paid choice');
  assert.equal(isChoosable(view, noKey), false);
  const chosenView = routeFromResponse(answer('chosen', [usablePaid])).view;
  assert.equal(isChoosable(chosenView, usablePaid), false, 'a chosen answer offers no pick');
});

test('fix hints point at the Settings section that clears each blocker, once per section', () => {
  assert.deepEqual(fixHints(alt('fal', 'x', { configured: false, usable: false })), [{ section: 'API keys', label: 'Open Settings > API keys' }]);
  assert.deepEqual(fixHints(alt('local-qts', 'q', { cloud: false, installed: false, usable: false }, { cloud: false })).map((h) => h.section), ['Models']);
  assert.deepEqual(
    fixHints(alt('fal', 'x', { configured: false, installed: false, qualified: false, usable: false })).map((h) => h.section),
    ['API keys', 'Models']
  );
  assert.deepEqual(fixHints(alt('fal', 'x', { usable: false }, { reason: 'would exceed the daily cap' })), [], 'a cap has no Settings fix here');
});

test('a not-applied preference is named in one sentence, and a route answer that applies all has none', () => {
  assert.equal(preferenceNote({ preferences_not_applied: ['quality', 'time'], preferences_not_applied_reason: 'the catalogue records no quality or latency facts' }), 'quality and time saved but not applied: the catalogue records no quality or latency facts.');
  assert.equal(preferenceNote({ preferences_not_applied: [] }), null);
  assert.equal(preferenceNote({}), null);
});

test('the route input names provider and model only for a cloud pick', () => {
  assert.deepEqual(routeInput('image', null), { kind: 'image' });
  assert.deepEqual(routeInput('video', { provider: 'fal', model: 'fal-ai/veo3.1/fast' }), { kind: 'video', provider: 'fal', model: 'fal-ai/veo3.1/fast' });
  assert.equal(routeId({ provider: 'fal', model: 'm' }), 'fal/m');
});

test('preference values read as the labels the panel shows', () => {
  assert.equal(preferenceLabel({ privacy: 'local_only', spend: 'ask' }, 'privacy'), 'This computer only');
  assert.equal(preferenceLabel({ privacy: 'any', spend: 'free_only' }, 'spend'), 'Free only');
});

// ---------------------------------------------------------------- the browser mock mirrors the engine shapes

test('image with the default preferences has no free route: route_choice_required lists the usable paid routes', async () => {
  const raw = await mockRouting('media.route', { kind: 'image' });
  assert.equal(raw.outcome, 'route_choice_required');
  assert.equal(raw.route_choice_required, true);
  assert.equal(raw.chosen, null);
  assert.deepEqual(raw.preferences_not_applied, ['quality', 'time']);
  const loaded = routeFromResponse(raw);
  assert.equal(loaded.view.status, 'choice');
  const choices = paidChoices(loaded.view.result.alternatives, ['fal/fal-ai/flux-2/klein/9b', 'wavespeed/qwen-image']);
  assert.ok(choices.some((c) => c.id === 'fal/fal-ai/flux-2/klein/9b' && c.pickable), 'fal FLUX is a usable paid choice');
  assert.ok(choices.every((c) => c.id.includes('/')));
  const nano = loaded.view.result.alternatives.find((a) => a.provider === 'nanobanana');
  assert.equal(nano.states.configured, false);
  assert.equal(nano.states.usable, false);
  assert.match(nano.reason, /no key for nanobanana/);
  assert.equal(nano.states.installed, null, 'a cloud route has no install check');
});

test('spend cheapest lets the engine choose the cheapest usable paid route, and the saved preference is restored', async () => {
  try {
    await mockRouting('media.route_prefs', { spend: 'cheapest' });
    const raw = await mockRouting('media.route', { kind: 'image' });
    assert.equal(raw.outcome, 'chosen');
    assert.equal(raw.basis, 'spend_preference');
    assert.equal(raw.chosen.provider, 'fal');
    assert.equal(raw.chosen.model, 'fal-ai/flux-2/klein/9b');
    assert.match(raw.note, /lowest-estimate usable paid route/);
  } finally {
    await mockRouting('media.route_prefs', { spend: 'ask' });
  }
});

test('a free usable route is chosen without spend, and its alternatives say why they are out', async () => {
  const raw = await mockRouting('media.route', { kind: 'upscale' });
  assert.equal(raw.outcome, 'chosen');
  assert.equal(raw.basis, 'free_route');
  assert.equal(raw.chosen.provider, 'realesrgan');
  assert.equal(raw.chosen.est_usd, 0);
  assert.equal(raw.chosen.states.qualified, null, 'a keyless route has no dated catalogue entry');
  const fal = raw.alternatives.find((a) => a.provider === 'fal');
  assert.ok(fal, 'paid alternative is listed');
  assert.equal(fal.eligible, true);
});

test('voice refuses to pick a missing local model and lists a not-installed reason on it', async () => {
  const raw = await mockRouting('media.route', { kind: 'voice' });
  assert.equal(raw.outcome, 'route_choice_required');
  const qts = raw.alternatives.find((a) => a.provider === 'local-qts');
  assert.equal(qts.states.installed, false);
  assert.equal(qts.states.usable, false);
  assert.match(qts.reason, /model not installed/);
  const fixes = fixHints(qts).map((h) => h.section);
  assert.deepEqual(fixes, ['Models']);
});

test('privacy this computer only makes a cloud-only kind a no_usable_route with every cloud reason, and the request tightens without saving', async () => {
  const raw = await mockRouting('media.route', { kind: 'image', privacy: 'local_only' });
  assert.equal(raw.outcome, 'no_usable_route');
  assert.equal(raw.chosen, null);
  assert.equal(raw.preferences.privacy, 'local_only');
  const fal = raw.alternatives.find((a) => a.provider === 'fal');
  assert.equal(fal.eligible, false);
  assert.match(fal.reason, /privacy preference excludes cloud routes/);
  const refusal = routeFromError('no_usable_route', raw, 'no usable image route');
  assert.equal(refusal.ok, true);
  assert.equal(refusal.view.status, 'none');
  const after = await mockRouting('media.route_prefs', {});
  assert.equal(after.preferences.privacy, 'any', 'the request did not save the preference');
});

test('a named provider and model is never overridden, even with no key', async () => {
  const raw = await mockRouting('media.route', { kind: 'image', provider: 'nanobanana', model: 'nano-banana-2' });
  assert.equal(raw.outcome, 'chosen');
  assert.equal(raw.basis, 'request');
  assert.equal(raw.chosen.provider, 'nanobanana');
  assert.match(raw.chosen.reason, /never overrides it; will be refused: no key for nanobanana/);
});

test('an unknown kind is refused with a code, as the engine refuses it', async () => {
  await assert.rejects(mockRouting('media.route', { kind: 'hologram' }), (error) => error.code === 'unknown_kind');
});

test('preferences read, save, and refuse unknown keys and values, as the engine does', async () => {
  const read = await mockRouting('media.route_prefs', {});
  assert.equal(read.saved_under, 'routing.preferences');
  assert.equal(read.preferences.spend, 'ask');
  try {
    const saved = await mockRouting('media.route_prefs', { privacy: 'local_only' });
    assert.equal(saved.preferences.privacy, 'local_only');
    await assert.rejects(mockRouting('media.route_prefs', { spend: 'sometimes' }), (error) => error.code === 'invalid_input');
    await assert.rejects(mockRouting('media.route_prefs', { colour: 'red' }), (error) => error.code === 'invalid_input');
  } finally {
    await mockRouting('media.route_prefs', { privacy: 'any' });
  }
});
