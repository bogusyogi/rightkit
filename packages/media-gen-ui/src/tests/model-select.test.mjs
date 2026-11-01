import test from 'node:test';
import assert from 'node:assert/strict';
import { pickModel, pickOtherProviderModel, noModelMessage, priceCaption } from '../lib/model-select.ts';

const MODELS = [
  { provider: 'fal', model: 'fal-ai/flux-2/klein/9b', kind: 'image', label: 'FLUX.2 klein 9B', est_usd: 0.0065 },
  { provider: 'nanobanana', model: 'nano-banana-2', kind: 'image', label: 'Nano Banana 2', est_usd: 0.06 },
  { provider: 'elevenlabs', model: 'eleven_v4_turbo', kind: 'voice', label: 'Eleven v4 Turbo', est_usd: 0.02 },
  { provider: 'wavespeed', model: 'elevenlabs-turbo-v2.5', kind: 'voice', label: 'ElevenLabs Turbo v2.5 TTS', est_usd: 0.01 },
  { provider: 'wavespeed', model: 'kling-3-pro', kind: 'video', label: 'Kling 3.0 Pro', est_usd: 0.56, default_duration_s: 5 }
];

test('pickModel returns the exact provider and model when the engine lists it', () => {
  const pick = pickModel(MODELS, { kind: 'voice', provider: 'wavespeed', model: 'elevenlabs-turbo-v2.5' });
  assert.equal(pick.provider, 'wavespeed');
  assert.equal(pick.model, 'elevenlabs-turbo-v2.5');
});

test('pickModel falls back to the requested provider when the id is no longer listed', () => {
  const pick = pickModel(MODELS, { kind: 'voice', provider: 'wavespeed', model: 'retired-voice' });
  assert.equal(pick.model, 'elevenlabs-turbo-v2.5');
});

test('pickModel falls back to the first model of the kind when the provider is not listed', () => {
  const pick = pickModel(MODELS, { kind: 'image', provider: 'evolink', model: 'flux-2-pro' });
  assert.equal(pick.model, 'fal-ai/flux-2/klein/9b');
});

test('pickModel never returns a model of another kind', () => {
  assert.equal(pickModel(MODELS, { kind: 'music', provider: 'fal', model: 'fal-ai/flux-2/klein/9b' }), null);
  assert.equal(pickModel([], { kind: 'image' }), null);
});

test('noModelMessage names the kind for the empty state', () => {
  assert.equal(noModelMessage('image'), 'No image model available');
});

test('priceCaption labels the engine estimate as est. with its default duration', () => {
  assert.equal(priceCaption({ est_usd: 0.56, default_duration_s: 5 }, 'clip'), 'est. $0.56 · 5 s');
});

test('priceCaption shows the unit when the model has no default duration', () => {
  assert.equal(priceCaption({ est_usd: 0.06, default_duration_s: null }, 'image'), 'est. $0.060 / image');
  assert.equal(priceCaption({ est_usd: 0.3 }, 'video'), 'est. $0.30 / video');
});

test('priceCaption never shows a starting or free figure for an unpriced model', () => {
  assert.equal(priceCaption({ est_usd: 0 }, 'image'), 'price unknown');
  assert.equal(priceCaption({ est_usd: Number.NaN }, 'image'), 'price unknown');
});

test('pickOtherProviderModel offers the first engine route of the kind away from the refusing provider', () => {
  const pick = pickOtherProviderModel(MODELS, { kind: 'voice', excludeProvider: 'elevenlabs' });
  assert.equal(pick.provider, 'wavespeed');
  assert.equal(pick.model, 'elevenlabs-turbo-v2.5');
});

test('pickOtherProviderModel never invents a route the engine does not list', () => {
  assert.equal(pickOtherProviderModel(MODELS.filter((m) => m.provider !== 'wavespeed'), { kind: 'voice', excludeProvider: 'elevenlabs' }), null);
  assert.equal(pickOtherProviderModel([], { kind: 'voice', excludeProvider: 'elevenlabs' }), null);
});
