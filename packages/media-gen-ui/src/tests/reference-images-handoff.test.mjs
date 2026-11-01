import test from 'node:test';
import assert from 'node:assert/strict';
import { clearHandoff, handoffFormValues, peekHandoff, setHandoff, takeHandoff } from '../lib/handoff.ts';
import {
  REFERENCE_MAX,
  addReference,
  referenceCaps,
  referencePathError,
  withReferenceParams
} from '../lib/gen-params.ts';

const image = (provider, model = 'm') => ({ provider, model, kind: 'image' });

test('reference capability: a row from an older engine (no references field) falls back to the provider rule', () => {
  assert.deepEqual(referenceCaps(image('nanobanana', 'nano-banana-2')), { param: 'image_urls', max: REFERENCE_MAX });
  assert.deepEqual(referenceCaps(image('evolink', 'gpt-image-2')), { param: 'image_urls', max: REFERENCE_MAX });
  assert.equal(referenceCaps(image('fal', 'fal-ai/flux-2/klein/9b')), null, 'fal reference support is per model');
  assert.equal(referenceCaps(image('wavespeed', 'qwen-image')), null);
});

test('reference capability: only image models, never video', () => {
  assert.equal(referenceCaps({ provider: 'nanobanana', model: 'x', kind: 'video' }), null);
});

test('reference capability: an engine-declared field overrides the provider rule', () => {
  assert.equal(referenceCaps({ ...image('nanobanana'), references: null }), null, 'declared none');
  assert.deepEqual(referenceCaps({ ...image('fal'), references: { param: 'refs', max: 3 } }), { param: 'refs', max: 3 });
});

test('reference capability: the engine declared limit and local upload carry through', () => {
  assert.deepEqual(
    referenceCaps({ ...image('evolink', 'gpt-image-2'), references: { param: 'image_urls', max: 16, local_upload: 'none' } }),
    { param: 'image_urls', max: 16, local_upload: 'none' },
    'declared max is the cap'
  );
  assert.deepEqual(
    referenceCaps({ ...image('nanobanana', 'nano-banana-2'), references: { param: 'image_urls', local_upload: 'fal_storage' } }),
    { param: 'image_urls', max: REFERENCE_MAX, local_upload: 'fal_storage' },
    'no engine max falls back to the composer cap'
  );
  assert.deepEqual(
    referenceCaps({ ...image('evolink', 'mj-v7'), references: { param: 'image_urls', local_upload: 'none' } }),
    { param: 'image_urls', max: REFERENCE_MAX, local_upload: 'none' }
  );
});

test('reference capability: a row without the field gets no references from an unknown provider', () => {
  assert.equal(referenceCaps({ ...image('fal', 'fal-ai/flux-2-max'), references: undefined }), null);
  assert.equal(referenceCaps({ ...image('wavespeed', 'recraft-v4'), references: undefined }), null);
});

test('referencePathError accepts URLs, absolute paths and file URLs; refuses bare names', () => {
  assert.equal(referencePathError('https://cdn.example.com/a.png'), null);
  assert.equal(referencePathError('/Users/me/a.png'), null);
  assert.equal(referencePathError('C:\\pics\\a.png'), null);
  assert.equal(referencePathError('file:///Users/me/a.png'), null);
  assert.ok(referencePathError('a.png'), 'a bare file name is refused');
  assert.ok(referencePathError('ftp://host/a.png'));
  assert.ok(referencePathError('   '));
});

test('addReference trims, strips file://, rejects duplicates and enforces the cap', () => {
  const one = addReference([], '  file:///Users/me/a.png ', 2);
  assert.deepEqual(one, { list: ['/Users/me/a.png'], error: null });
  const dup = addReference(one.list, '/Users/me/a.png', 2);
  assert.deepEqual(dup.list, one.list);
  assert.match(dup.error, /already/);
  const two = addReference(one.list, 'https://x.test/b.png', 2);
  assert.equal(two.list.length, 2);
  const full = addReference(two.list, '/c.png', 2);
  assert.deepEqual(full.list, two.list);
  assert.match(full.error, /at most 2/);
  const bad = addReference([], 'a.png', 2);
  assert.deepEqual(bad.list, []);
  assert.ok(bad.error);
});

test('withReferenceParams sends the list only to models that take references', () => {
  const params = { aspect: '1:1', resolution: '2K' };
  const caps = referenceCaps(image('nanobanana'));
  const sent = withReferenceParams(params, ['/a.png', 'https://x.test/b.png'], caps);
  assert.deepEqual(sent, { aspect: '1:1', resolution: '2K', image_urls: ['/a.png', 'https://x.test/b.png'] });
  assert.equal(params.image_urls, undefined, 'input is not mutated');
  assert.equal(withReferenceParams(params, ['/a.png'], null), params, 'no caps: params untouched');
  assert.equal(withReferenceParams(params, [], caps), params, 'no references: params untouched');
});

test('handoffFormValues maps an engine image hand-off onto composer fields', () => {
  const v = handoffFormValues({
    view: 'image',
    kind: 'image',
    prompt: 'Hero frame',
    provider: 'nanobanana',
    model: 'nano-banana-2',
    source: 'Production stack',
    params: { aspect: '9:16', resolution: '2K', image_urls: ['/Users/me/product.png', '  ', 7, 'https://x.test/p.png'] }
  });
  assert.equal(v.prompt, 'Hero frame');
  assert.deepEqual(v.model, { provider: 'nanobanana', model: 'nano-banana-2' });
  assert.equal(v.aspect, '9:16');
  assert.equal(v.resolution, '2K');
  assert.deepEqual(v.references, ['/Users/me/product.png', 'https://x.test/p.png']);
  assert.deepEqual(v.unapplied, []);
});

test('handoffFormValues maps video params and reports params it has no field for', () => {
  const v = handoffFormValues({
    view: 'video',
    kind: 'video',
    prompt: 'Orbit',
    params: { duration: 5, seed: 42, image_url: 'https://x.test/s.png', aspect: '16:9', mystery: true }
  });
  assert.equal(v.model, null);
  assert.equal(v.duration, 5);
  assert.equal(v.seed, '42');
  assert.equal(v.imageUrl, 'https://x.test/s.png');
  assert.deepEqual(v.unapplied, ['mystery']);
  assert.equal(handoffFormValues({ view: 'voice', kind: 'voice', prompt: 'Hi' }).duration, null);
});

test('a hand-off is taken once and only by its own view', () => {
  clearHandoff();
  setHandoff({ view: 'image', kind: 'image', prompt: 'A', params: { image_urls: ['/a.png'] } });
  assert.equal(takeHandoff('video'), null, 'another view does not consume it');
  assert.equal(peekHandoff('image')?.prompt, 'A', 'peek does not consume');
  const first = takeHandoff('image');
  assert.equal(first?.prompt, 'A');
  assert.equal(takeHandoff('image'), null, 'second take is empty');
  assert.equal(peekHandoff('image'), null);
});

test('reference capability: engine rows decide, with an explicit null for none', () => {
  assert.equal(referenceCaps({ ...image('nanobanana', 'nano-banana-2'), references: null }), null);
  assert.deepEqual(referenceCaps({ ...image('fal', 'x'), references: { param: 'image_urls', max: 3, local_upload: 'none' } }), { param: 'image_urls', max: 3, local_upload: 'none' });
  assert.equal(referenceCaps({ provider: 'fal', model: 'v', kind: 'video', references: { param: 'image_urls', max: 3 } }), null, 'video references are not composer references');
});
