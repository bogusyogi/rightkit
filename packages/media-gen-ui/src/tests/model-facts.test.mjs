import test from 'node:test';
import assert from 'node:assert/strict';
import {
  avatarCaps, buildMediaParams, buildParams, imageCaps, isKeyless, lipsyncCaps, musicCaps, preferredProvider, sfxCaps, upscaleCaps, videoCaps, voiceIdSpec
} from '../lib/gen-params.ts';

const row = (o) => ({ provider: 'p', model: 'm', ...o });

test('video: engine facts decide frames, params and resolution defaults', () => {
  const m = row({
    kind: 'video',
    inputs: { mode: 'image', start_image: 'required', start_image_param: 'start_image_url', end_image: 'optional', end_image_param: 'last_image' },
    settings: { aspect: { param: 'aspect_ratio', values: ['adaptive', '16:9'] }, resolution: { param: 'resolution', values: ['480p', '720p'], required: true, default: null } }
  });
  const caps = videoCaps(m);
  assert.equal(caps.imageUrl, 'required');
  assert.equal(caps.endImage, 'optional');
  assert.equal(caps.defaultResolution, '480p', 'required with no engine default starts at the first value');
  const built = buildParams(m, { aspect: '16:9', resolution: '720p', imageUrl: 'https://a/x.png', endImageUrl: 'https://a/y.png' });
  assert.deepEqual(built, { ok: true, params: { aspect_ratio: '16:9', resolution: '720p', start_image_url: 'https://a/x.png', last_image: 'https://a/y.png' } });
  assert.equal(buildParams(m, {}).ok, false, 'a required start frame is checked');
  assert.equal(buildParams(row({ ...m, inputs: { ...m.inputs, start_image: 'none' } }), { imageUrl: 'https://a/x.png' }).params?.start_image_url, undefined);
});

test('video: a row without facts keeps the old provider rule', () => {
  assert.equal(videoCaps(row({ provider: 'fal', model: 'fal-ai/veo3.1/fast/image-to-video', kind: 'video' })).imageUrl, 'required');
  assert.deepEqual(videoCaps(row({ provider: 'wavespeed', model: 'seedance-2-fast', kind: 'video' })).resolutions, ['480p', '720p', '1080p']);
});

test('image: seed and aspects come from settings', () => {
  const m = row({ kind: 'image', settings: { aspect: { param: 'aspect', values: ['1:1'] }, seed: { param: 'seed' } } });
  assert.equal(imageCaps(m).seed, true);
  assert.equal(imageCaps(row({ kind: 'image', settings: {} })).seed, false);
  assert.equal(imageCaps(row({ provider: 'fal', kind: 'image' })).seed, true, 'old rule');
});

test('avatar: mode picks the input shape and params', () => {
  const local = avatarCaps(row({ kind: 'avatar', inputs: { mode: 'image_and_audio', image: { param: 'image_path', accepts: 'path' }, audio: { param: 'audio_path', accepts: 'path' } }, settings: {} }));
  assert.equal(local.scripted, false);
  const m = row({ kind: 'avatar', inputs: { mode: 'image_and_audio', image: { param: 'image_path', accepts: 'path' }, audio: { param: 'audio_path', accepts: 'path' } }, settings: {} });
  assert.deepEqual(buildParams(m, { imageUrl: '/a.png', audioUrl: '/a.wav' }), { ok: true, params: { image_path: '/a.png', audio_path: '/a.wav' } });
  assert.equal(avatarCaps(row({ provider: 'heygen', kind: 'avatar' })).scripted, true);
});

test('media: upscale, lipsync, sfx and music read their params from the row', () => {
  assert.equal(upscaleCaps(row({ kind: 'upscale', inputs: { mode: 'video', video: { param: 'video_url' } } })).input, 'video');
  assert.equal(upscaleCaps(row({ model: 'fal-ai/topaz/upscale/video', kind: 'upscale' })).input, 'video', 'old rule');
  assert.equal(lipsyncCaps(row({ kind: 'lipsync', inputs: {}, settings: {} })).syncMode, null);
  assert.equal(sfxCaps(row({ kind: 'sfx', inputs: { mode: 'video', video: { param: 'video_url' } }, settings: {} })).mode, 'video');
  const music = row({ kind: 'music', settings: { length: { param: 'music_length_ms', unit: 'ms', min: 3000, max: 600000, integer: true } } });
  assert.equal(musicCaps(music).length.param, 'music_length_ms');
  const form = { prompt: 'x', secs: '10', imageUrl: '', videoUrl: '', audioUrl: '', scale: '4', syncMode: '', loop: false };
  assert.deepEqual(buildMediaParams(music, form), { ok: true, params: { music_length_ms: 10000 } });
  assert.equal(buildMediaParams(music, { ...form, secs: '2' }).ok, false);
  assert.deepEqual(buildMediaParams(row({ kind: 'music', provider: 'fal' }), form), { ok: true, params: { seconds_total: 10 } }, 'old rule');
});

test('keyless, default and voice id follow the engine', () => {
  assert.equal(isKeyless({ provider: 'local-image', needs_key: false }), true);
  assert.equal(isKeyless({ provider: 'local-qts' }), true, 'old rule');
  assert.equal(isKeyless({ provider: 'fal', needs_key: true }), false);
  const rows = [row({ kind: 'avatar', provider: 'a', default: false }), row({ kind: 'avatar', provider: 'b', default: true })];
  assert.equal(preferredProvider('avatar', rows), 'b');
  assert.equal(preferredProvider('image', rows), undefined);
  assert.equal(preferredProvider('avatar', [row({ kind: 'avatar' })]), 'heygen', 'old rule');
  assert.equal(voiceIdSpec(row({ kind: 'voice', inputs: { mode: 'script' } })), null);
});
