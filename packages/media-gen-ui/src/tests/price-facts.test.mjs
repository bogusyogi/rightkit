import test from 'node:test';
import assert from 'node:assert/strict';

// The resolver hook must be registered before the .ts modules load, so these imports are dynamic.
await import('./ts-resolve.mjs');
const facts = await import('../lib/gen-params.ts');
const { mockEngine } = await import('../lib/browser-mock-api.ts');

const SENTENCE = "Price from the provider's page, not yet confirmed by a real run";
const ROUNDING = 'rounded up as an assumption';

test('media modalities answer maps reconciled and assumed rounding per provider/model', () => {
  const raw = {
    modalities: [
      {
        kind: 'image',
        providers: [
          {
            id: 'evolink',
            models: [
              { provider: 'evolink', model: 'gpt-image-2', kind: 'image', reconciled: false, pricing: { kind: 'per_megapixel', rounding: 'assumed' } },
              { provider: 'evolink', model: 'plain', kind: 'image' }
            ]
          }
        ]
      },
      {
        kind: 'video',
        providers: [{ id: 'wavespeed', models: [{ model: 'kling-3-pro', kind: 'video', reconciled: true, pricing: { kind: 'per_second', rounding: 'stated' } }] }]
      }
    ]
  };
  assert.deepEqual(facts.priceFactsFromModalities(raw), {
    'evolink/gpt-image-2': { reconciled: false, rounding: 'assumed' },
    'wavespeed/kling-3-pro': { reconciled: true, rounding: 'stated' }
  });
});

test('a model with no stated fact gets no entry, and a malformed answer reads as nothing', () => {
  assert.deepEqual(facts.priceFactsFromModalities({ modalities: [{ providers: [{ id: 'fal', models: [{ model: 'x' }] }] }] }), {});
  assert.deepEqual(facts.priceFactsFromModalities(null), {});
  assert.deepEqual(facts.priceFactsFromModalities({ modalities: 'nope' }), {});
  assert.deepEqual(facts.priceFactsFromModalities({ modalities: [{ providers: [{ id: 'fal', models: [{ model: 'y', reconciled: 'no', pricing: { rounding: 'guess' } }] }] }] }), {});
});

test('the rounding is read only from a stated or assumed value', () => {
  const out = facts.priceFactsFromModalities({ modalities: [{ providers: [{ id: 'fal', models: [{ model: 'z', pricing: { rounding: 'stated' } }] }] }] });
  assert.deepEqual(out, { 'fal/z': { rounding: 'stated' } });
});

test('caveat sentences: none when the engine states nothing, in display order when both apply', () => {
  assert.deepEqual(facts.priceFactNotes(undefined), []);
  assert.deepEqual(facts.priceFactNotes({ reconciled: true }), []);
  assert.deepEqual(facts.priceFactNotes({ rounding: 'stated' }), []);
  assert.deepEqual(facts.priceFactNotes({ reconciled: false }), [SENTENCE]);
  assert.deepEqual(facts.priceFactNotes({ rounding: 'assumed' }), [ROUNDING]);
  assert.deepEqual(facts.priceFactNotes({ reconciled: false, rounding: 'assumed' }), [SENTENCE, ROUNDING]);
  assert.equal(facts.isUnconfirmedPrice({ reconciled: false }), true);
  assert.equal(facts.isUnconfirmedPrice(undefined), false);
  assert.equal(facts.isAssumedRounding({ rounding: 'assumed' }), true);
});

test('the wording is the one the UI shows', () => {
  assert.equal(facts.UNCONFIRMED_PRICE_SENTENCE, SENTENCE);
  assert.equal(facts.ASSUMED_ROUNDING_SENTENCE, ROUNDING);
  assert.equal(facts.UNCONFIRMED_PRICE_LABEL, 'Unconfirmed price');
});

test('browser mock: modalities carries the facts, and the models rows do not (as the engine)', async () => {
  const answer = await mockEngine('command', { name: 'media.modalities', input: {} });
  const map = facts.priceFactsFromModalities(answer);
  assert.deepEqual(map['evolink/gpt-image-2'], { reconciled: false, rounding: 'assumed' });
  assert.deepEqual(map['wavespeed/kling-3-pro'], { reconciled: false });
  assert.equal(map['fal/fal-ai/flux-2/klein/9b'].reconciled, true);

  const rows = await mockEngine('command', { name: 'models', input: {} });
  assert.ok(rows.length > 0);
  assert.ok(rows.every((row) => !('reconciled' in row) && !('pricing' in row)), 'models rows carry no price facts');
});
