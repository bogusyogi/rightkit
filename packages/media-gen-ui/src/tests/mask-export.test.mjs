import test from 'node:test';
import assert from 'node:assert/strict';
import { isMaskEmpty, maskInputOf, maskRgba, startInputOf, toSourcePoint, withMaskParam, withStartParam } from '../lib/mask-export.ts';

const brush = (points, size = 6) => ({ tool: 'brush', size, points });
const px = (rgba, w, x, y) => Array.from(rgba.slice((y * w + x) * 4, (y * w + x) * 4 + 4));

test('mask has exactly the source dimensions', () => {
  const rgba = maskRgba([brush([{ x: 5, y: 5 }])], 37, 21);
  assert.equal(rgba.length, 37 * 21 * 4);
});

test('painted pixels are white, the rest black, all opaque and pure', () => {
  const rgba = maskRgba([brush([{ x: 10, y: 10 }])], 20, 20);
  assert.deepEqual(px(rgba, 20, 10, 10), [255, 255, 255, 255]);
  assert.deepEqual(px(rgba, 20, 0, 19), [0, 0, 0, 255]);
  for (let i = 0; i < rgba.length; i += 4) {
    assert.ok(rgba[i] === 0 || rgba[i] === 255);
    assert.equal(rgba[i + 3], 255);
  }
});

test('eraser removes paint and invert swaps colours', () => {
  const strokes = [brush([{ x: 10, y: 10 }], 8), { tool: 'eraser', size: 8, points: [{ x: 10, y: 10 }] }];
  assert.deepEqual(px(maskRgba(strokes, 20, 20), 20, 10, 10), [0, 0, 0, 255]);
  assert.deepEqual(px(maskRgba(strokes, 20, 20, true), 20, 10, 10), [255, 255, 255, 255]);
});

test('a stroke interpolates between points', () => {
  const rgba = maskRgba([brush([{ x: 2, y: 5 }, { x: 28, y: 5 }], 4)], 30, 10);
  assert.deepEqual(px(rgba, 30, 15, 5), [255, 255, 255, 255]);
});

test('empty mask detection', () => {
  assert.equal(isMaskEmpty([]), true);
  assert.equal(isMaskEmpty([brush([{ x: 1, y: 1 }])]), false);
});

test('display position maps to source pixels and clamps', () => {
  const box = { left: 10, top: 20, width: 100, height: 50 };
  assert.deepEqual(toSourcePoint(60, 45, box, 1000, 500), { x: 500, y: 250 });
  assert.deepEqual(toSourcePoint(0, 0, box, 1000, 500), { x: 0, y: 0 });
});

test('mask param comes from the catalogue row, not a model name', () => {
  const row = { inputs: { mask: { param: 'mask_image_url' } } };
  assert.deepEqual(maskInputOf(row), { param: 'mask_image_url' });
  assert.equal(maskInputOf({ inputs: {} }), null);
  assert.deepEqual(withMaskParam({ a: 1 }, maskInputOf(row), '/x/m.png'), { a: 1, mask_image_url: '/x/m.png' });
  assert.deepEqual(withMaskParam({ a: 1 }, null, '/x/m.png'), { a: 1 });
});

test('start param sends the first reference as a single string, only without a reference list', () => {
  const row = { inputs: { start: { param: 'image_url' } } };
  const start = startInputOf(row);
  assert.deepEqual(start, { param: 'image_url' });
  assert.equal(startInputOf({ inputs: {} }), null);
  assert.deepEqual(withStartParam({ a: 1 }, start, ['/x/a.png', '/x/b.png'], false), { a: 1, image_url: '/x/a.png' });
  assert.deepEqual(withStartParam({ a: 1 }, start, ['/x/a.png'], true), { a: 1 });
  assert.deepEqual(withStartParam({ a: 1 }, start, [], false), { a: 1 });
  assert.deepEqual(withStartParam({ a: 1 }, null, ['/x/a.png'], false), { a: 1 });
});
