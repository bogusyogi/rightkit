import test from 'node:test';
import assert from 'node:assert/strict';

// The resolver hook must be registered before the .ts modules load, so these imports are dynamic.
await import('./ts-resolve.mjs');
const lock = await import('../views/panels/character-lock.ts');
const { mockEngine } = await import('../lib/browser-mock-api.ts');
const { errorCode, errorText } = await import('../lib/engine-error.ts');

// A rejection shaped as the app receives it from the engine: {code, message, details?}.
const refusal = (code, message, details) => (details ? { code, message, details } : { code, message });

const lockOf = (over = {}) => ({
  mode: 'lora',
  kind: 'image',
  model: 'fal/fal-ai/flux-2-lora',
  character: null,
  aesthetic_lora: null,
  loras: [],
  references: [],
  pending_upload: [],
  params: {},
  diagnostics: [],
  ...over
});

test('a switched route reads as one plain sentence, with or without reference images', () => {
  const withRefs = lockOf({ model: 'fal/fal-ai/flux-2-lora/edit', requested_model: 'fal/fal-ai/flux-2-lora', references: ['a.png'] });
  assert.equal(lock.lockRouteSwitch(withRefs).resolved, 'fal/fal-ai/flux-2-lora/edit');
  assert.equal(lock.lockSwitchSentence(withRefs), 'Using fal/fal-ai/flux-2-lora/edit because this character has reference images');
  const noRefs = lockOf({ model: 'fal/fal-ai/flux-2-lora', requested_model: 'fal/fal-ai/flux-2-lora/edit' });
  assert.equal(lock.lockSwitchSentence(noRefs), 'Using fal/fal-ai/flux-2-lora because this character has no reference images');
});

test('no switch when the resolved route is the one asked for, or the engine sent no requested route', () => {
  assert.equal(lock.lockRouteSwitch(lockOf({ requested_model: 'fal/fal-ai/flux-2-lora' })), null);
  assert.equal(lock.lockSwitchSentence(lockOf()), null);
});

test('an unconfirmed price is read from the route, else the lock field; absent is not a caveat', () => {
  assert.equal(lock.lockPriceUnconfirmed(lockOf({ route: { reconciled: false } })), true);
  assert.equal(lock.lockPriceUnconfirmed(lockOf({ route: { reconciled: true } })), false);
  assert.equal(lock.lockPriceUnconfirmed(lockOf({ route: { reconciled: null } })), false);
  assert.equal(lock.lockPriceUnconfirmed(lockOf({ reconciled: false })), true);
  assert.equal(lock.lockPriceUnconfirmed(lockOf()), false);
});

test('primary diagnostics drop the notices and the inconsistencies, which have their own place', () => {
  const inc = { code: 'served_mismatch', message: 'table disagrees' };
  const out = lock.lockPrimaryDiagnostics(lockOf({
    route: { inconsistencies: [inc] },
    diagnostics: [
      { severity: 'notice', message: 'LoRA route resolved to its sibling' },
      { severity: 'warning', message: 'table disagrees' },
      { severity: 'warning', message: '2 references available; using the first 1.' }
    ]
  }));
  assert.deepEqual(out, [{ severity: 'warning', message: '2 references available; using the first 1.' }]);
  assert.deepEqual(lock.lockInconsistencies(lockOf({ route: { inconsistencies: [inc, { message: '  ' }] } })), [inc]);
  assert.deepEqual(lock.lockInconsistencies(lockOf()), []);
});

test('lora_not_hosted says the LoRA must be at an https address and that the app cannot upload it', () => {
  const r = lock.lockRefusal(refusal('lora_not_hosted', 'The LoRA for dd/iris is "/x/iris.safetensors", not an https URL the provider can fetch, so it cannot be sent.'));
  assert.equal(r.code, 'lora_not_hosted');
  assert.match(r.sentence, /https/);
  assert.match(r.sentence, /cannot upload LoRA files yet/);
  assert.doesNotMatch(r.sentence, /upload:true/);
});

test('reference_route_unsupported lists the routes the engine names in its message', () => {
  const msg = 'The served FLUX.2 Pro route (fal-ai/flux-2-max) takes no reference images, and character "pia" has no trained LoRA, so its reference images cannot lock identity there; use a route that accepts reference images; the engine lists evolink/gpt-image-2, nanobanana/nano-banana-2.';
  const r = lock.lockRefusal(refusal('reference_route_unsupported', msg));
  assert.equal(r.sentence, 'This route does not take reference images, and this character has no trained LoRA, so its references cannot hold its identity here. Use one of these routes instead: evolink/gpt-image-2, nanobanana/nano-banana-2.');
  assert.equal(r.technical, msg);
});

test('reference_route_unsupported with no route list still gives a next step', () => {
  const r = lock.lockRefusal(refusal('reference_route_unsupported', 'takes no reference images'));
  assert.match(r.sentence, /Use a route that accepts reference images\.$/);
});

test('a structured route list is preferred over the message when the engine sends one', () => {
  const r = lock.lockRefusal(refusal('reference_route_unsupported', 'x; the engine lists a/b.', { routes: ['only/one'] }));
  assert.match(r.sentence, /Use one of these routes instead: only\/one\.$/);
});

test('too_many_references gives the maximum from the refusal', () => {
  const r = lock.lockRefusal(refusal('too_many_references', '2 reference images were supplied, but wavespeed/qwen-image takes at most 1.'));
  assert.match(r.sentence, /takes at most 1 reference image\. /);
  const many = lock.lockRefusal(refusal('too_many_references', '3 reference images were supplied, but x/y takes at most 4.'));
  assert.match(many.sentence, /takes at most 4 reference images\./);
  const none = lock.lockRefusal(refusal('too_many_references', 'too many'));
  assert.match(none.sentence, /fewer reference images/);
});

test('lora_unsupported_model and an unknown code: plain sentence, and unknown keeps the engine text', () => {
  assert.match(lock.lockRefusal(refusal('lora_unsupported_model', 'Model "x/y" does not support LoRA weights')).sentence, /does not take LoRA weights/);
  const other = lock.lockRefusal(refusal('route_not_served', 'x/y is in the studio table but the engine does not serve it'));
  assert.equal(other.code, 'route_not_served');
  assert.equal(other.sentence, 'x/y is in the studio table but the engine does not serve it');
  assert.equal(other.technical, other.sentence);
});

test('browser mock: a LoRA character with reference images switches to the edit route, with the price notice', async () => {
  const res = await mockEngine('command', { name: 'studio.characters.lock', input: { kind: 'image', provider: 'fal', model: 'fal-ai/flux-2-lora', character_id: 'dd/iris', fallback_to_references: true, upload: false } });
  assert.equal(res.model, 'fal/fal-ai/flux-2-lora/edit');
  assert.equal(res.requested_model, 'fal/fal-ai/flux-2-lora');
  assert.equal(lock.lockPriceUnconfirmed(res), true);
  assert.equal(res.route.reconciled_notice, 'price not yet confirmed by a live run');
  assert.equal(lock.lockInconsistencies(res).length, 1);
  assert.match(lock.lockSwitchSentence(res), /because this character has reference images$/);
  assert.equal(lock.lockPrimaryDiagnostics(res).some((d) => d.severity === 'notice'), false);
});

test('browser mock: each refusal kind comes back as the engine code, and reads as a sentence', async () => {
  const call = (input) => mockEngine('command', { name: 'studio.characters.lock', input: { kind: 'image', upload: false, fallback_to_references: true, ...input } });
  const cases = [
    [{ provider: 'fal', model: 'fal-ai/flux-2-local-lora', character_id: 'dd/iris' }, 'lora_not_hosted'],
    [{ provider: 'fal', model: 'fal-ai/flux-2-max', character_id: 'hr/pia' }, 'reference_route_unsupported'],
    [{ provider: 'wavespeed', model: 'qwen-image', character_id: 'dd/iris' }, 'too_many_references'],
    [{ provider: 'wavespeed', model: 'recraft-v4', character_id: 'dd/iris' }, 'lora_unsupported_model']
  ];
  for (const [input, code] of cases) {
    let caught = null;
    try {
      await call(input);
    } catch (e) {
      caught = e;
    }
    assert.ok(caught, `${code} is refused`);
    assert.equal(errorCode(caught), code);
    const view = lock.lockRefusal(caught);
    assert.equal(view.code, code);
    assert.ok(view.sentence.length > 40 && !view.sentence.startsWith(`${code}:`), `${code} reads as a sentence`);
    assert.equal(view.technical, errorText(caught));
  }
});

test('browser mock: the lock keeps the route it asked for when no sibling applies', async () => {
  const res = await mockEngine('command', { name: 'studio.characters.lock', input: { kind: 'image', provider: 'nanobanana', model: 'nano-banana-2', character_id: 'hr/pia', upload: false, fallback_to_references: true } });
  assert.equal(res.model, 'nanobanana/nano-banana-2');
  assert.equal(lock.lockRouteSwitch(res), null);
  assert.equal(lock.lockPriceUnconfirmed(res), false);
});
