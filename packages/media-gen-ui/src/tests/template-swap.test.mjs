import test from 'node:test';
import assert from 'node:assert/strict';
import {
  alternatives,
  canApprove,
  canImport,
  canPreflight,
  importInput,
  mediaTypeOf,
  preflightInput,
  runRequest,
  slotProblems,
  uploadPaths,
  usableRoutes
} from '../views/template-swap-logic.ts';

const slots = [
  { name: 'face', label: 'Your face', type: 'image', required: true },
  { name: 'extra', label: 'Extra', type: 'image', required: false }
];
const pre = (paths = []) => ({
  ok: true,
  route: { provider: 'nanobanana', model: 'nano-banana-2' },
  est_usd: 0.04,
  considered: [
    { route: 'fal/b', usable: false, reason: 'no key' },
    { route: 'evolink/gpt-image-2', usable: true, est_usd: 0.09 },
    { route: 'nanobanana/nano-banana-2', usable: true, est_usd: 0.04 }
  ],
  draft: { draft_id: 'd1', request_hash: 'h' },
  upload_confirmation: { required: paths.length > 0, paths, confirm_field: 'confirm_fal_upload' }
});

test('media type by extension', () => {
  assert.equal(mediaTypeOf('/a/b.PNG'), 'image');
  assert.equal(mediaTypeOf('https://x.test/v.mp4?x=1'), 'video');
  assert.equal(mediaTypeOf('/a/b.txt'), null);
});

test('slot problems: required missing and wrong media', () => {
  assert.equal(canPreflight(slots, {}), false);
  assert.match(slotProblems(slots, {})[0].text, /required/);
  assert.match(slotProblems(slots, { face: '/a.mp4' })[0].text, /image/);
  assert.equal(canPreflight(slots, { face: '/a.png' }), true);
});

test('preflight input sends only filled slots and splits the route', () => {
  const i = preflightInput('t', slots, { face: ' /a.png ', extra: '' }, 'fal/bytedance/seedream');
  assert.deepEqual(i.slots, { face: '/a.png' });
  assert.equal(i.provider, 'fal');
  assert.equal(i.model, 'bytedance/seedream');
  assert.equal('provider' in preflightInput('t', slots, { face: '/a.png' }), false);
});

test('alternatives exclude the chosen route, usable cheapest first', () => {
  assert.deepEqual(alternatives(pre()).map((c) => c.route), ['evolink/gpt-image-2', 'fal/b']);
  assert.deepEqual(usableRoutes(pre()), ['evolink/gpt-image-2', 'nanobanana/nano-banana-2']);
});

test('consent gates approval and confirm_fal_upload only when ticked', () => {
  const p = pre(['/a.png']);
  assert.deepEqual(uploadPaths(p), ['/a.png']);
  assert.equal(canApprove(p, false), false);
  assert.equal(canApprove(p, true), true);
  assert.equal(canApprove(pre(), false), true);
  assert.equal(runRequest({ kind: 'image' }, p, true).confirm_fal_upload, true);
  assert.equal('confirm_fal_upload' in runRequest({ kind: 'image' }, p, false), false);
  assert.equal('confirm_fal_upload' in runRequest({ kind: 'image' }, pre(), true), false);
});

test('import input chooses path or url', () => {
  assert.deepEqual(importInput('/a.png', '', ' Swap '), { path: '/a.png', prompt: 'Swap' });
  assert.deepEqual(importInput('https://x.test/a.mp4', 'N', 'p'), { url: 'https://x.test/a.mp4', prompt: 'p', name: 'N' });
  assert.equal(canImport('/a.png', ''), false);
  assert.equal(canImport('/a.txt', 'p'), false);
});
