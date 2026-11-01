import test from 'node:test';
import assert from 'node:assert/strict';
import {
  explainError,
  fmtBytes,
  installSummaryText
} from '../views/voices-logic.ts';
import { createMockVoicesEngine, mockWavDataUri } from '../views/voices-mock.ts';

// Deterministic clock: tests advance it to move the mock install along.
let now = 1_000_000;
const call = createMockVoicesEngine(() => now);

test('models_status reports the on-device engine not installed by default', () => {
  const s = call('studio.voices.models_status');
  assert.equal(s.engine, 'qts');
  assert.equal(s.ready, false);
  assert.equal(s.models_installed, false);
  assert.equal(s.files.length, 2);
  assert.equal(s.onnxruntime.present, false);
  assert.match(installSummaryText(s), /model files missing/);
});

test('install runs running → done and flips readiness; voices verify after', async () => {
  assert.equal(call('studio.voices.install').state, 'running');
  now += 2000;
  const st = call('studio.voices.install_status');
  assert.equal(st.state, 'running');
  assert.ok(st.downloaded_bytes > 0 && st.downloaded_bytes < st.total_bytes);
  // Verify is refused while the engine is not ready.
  assert.throws(() => call('studio.voices.verify', { id: 'vr-voice-1' }), /verify_pending/);
  // Clone is gated too.
  assert.throws(() => call('studio.voices.clone_local', { name: 'x', audio_path: '/a.wav', consent: true }), /not_installed/);
  now += 3000;
  assert.equal(call('studio.voices.install_status').state, 'done');
  assert.equal(call('studio.voices.models_status').ready, true);
});

test('clone_local requires consent, then creates a local voice record', () => {
  assert.throws(() => call('studio.voices.clone_local', { name: 'Narrator', audio_path: '/ref.wav', consent: false }), /consent_required/);
  const r = call('studio.voices.clone_local', { name: 'Narrator', audio_path: '/ref.wav', consent: true });
  assert.equal(r.provider, 'local');
  assert.equal(r.verified, false);
  const list = call('studio.voices.list', { provider: 'local' });
  assert.ok(list.voices.some((v) => v.id === r.id && v.kind === 'clone'));
});

test('rename, verify, preview, and delete cover the local voice lifecycle', () => {
  const r = call('studio.voices.clone_local', { name: 'Cloned Voice', audio_path: '/ref.wav', consent: true });
  call('studio.voices.rename', { id: r.id, name: 'Renamed Voice' });
  const list = call('studio.voices.list', { provider: 'local' });
  assert.equal(list.voices.find((v) => v.id === r.id).name, 'Renamed Voice');
  assert.equal(call('studio.voices.verify', { id: r.id }).verified, true);
  assert.equal(list.voices.find((v) => v.id === r.id).verified ?? true, true);
  const p = call('studio.voices.preview', { id: r.id });
  assert.ok(p.path.startsWith('data:audio/wav;base64,'));
  assert.throws(() => call('studio.voices.rename', { id: 'JBFqnCBsd6RMkjVDRZzb', name: 'x' }), /voice_not_found/);
  call('studio.voices.delete', { id: r.id });
  assert.throws(() => call('studio.voices.preview', { id: r.id }), /voice_not_found/);
});

test('mockWavDataUri produces a valid RIFF/WAVE header', () => {
  const uri = mockWavDataUri();
  const buf = Buffer.from(uri.split(',')[1], 'base64');
  assert.equal(buf.subarray(0, 4).toString(), 'RIFF');
  assert.equal(buf.subarray(8, 12).toString(), 'WAVE');
  assert.equal(buf.readUInt32LE(24), 8000);
  assert.ok(buf.length > 8000);
});

test('new error codes explain next steps', () => {
  assert.match(explainError('integrity', 'x').next ?? '', /Install/i);
  assert.match(explainError('transport', 'x').next ?? '', /resumes/i);
  assert.equal(fmtBytes(1784704320), '1.78 GB');
  assert.equal(fmtBytes(undefined), '0 KB');
});

test('export returns per-line files, a folder manifest dir, or a zip path', () => {
  const b = call('studio.voices.batch', { lines: ['one', 'two'], provider: 'elevenlabs', voice_id: 'x' });
  assert.throws(() => call('studio.voices.export', { batch_id: 'nope' }), /batch_not_found/);
  assert.throws(() => call('studio.voices.export', { batch_id: b.batch_id, format: 'ogg' }), /invalid_input/);
  const folder = call('studio.voices.export', { batch_id: b.batch_id, format: 'flac', normalize: true });
  assert.equal(folder.count, 2);
  assert.equal(folder.files[0].name, 'line-001.flac');
  assert.ok(folder.files[0].path.endsWith('/export-flac/line-001.flac'));
  assert.equal(folder.zip, null);
  assert.match(folder.note, /EBU R128/);
  const zipped = call('studio.voices.export', { batch_id: b.batch_id, format: 'm4a', zip: true });
  assert.ok(zipped.zip.endsWith(`${b.batch_id}-m4a.zip`));
});

