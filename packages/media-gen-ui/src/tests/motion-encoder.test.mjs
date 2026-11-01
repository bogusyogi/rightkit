import test from 'node:test';
import assert from 'node:assert/strict';
import { isH264Encoder } from '../lib/motion-encoder.ts';

test('H.264 encoders from the backend allowlist get the player', () => {
  assert.equal(isH264Encoder('h264_videotoolbox'), true);
  assert.equal(isH264Encoder('h264_mf'), true);
  assert.equal(isH264Encoder('libopenh264'), true);
});

test('mpeg4 and unknown or empty encoder names get the poster', () => {
  assert.equal(isH264Encoder('mpeg4'), false);
  assert.equal(isH264Encoder('h264'), false);
  assert.equal(isH264Encoder('libx264'), false);
  assert.equal(isH264Encoder(''), false);
  assert.equal(isH264Encoder(null), false);
  assert.equal(isH264Encoder(undefined), false);
});
