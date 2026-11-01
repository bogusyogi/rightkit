import test from 'node:test';
import assert from 'node:assert/strict';
import { EngineError, errorCode, errorDetails, errorMessage, errorText, toError } from '../lib/engine-error.ts';

test('an engine object reads as "code: message" and keeps its code and details', () => {
  const rejection = { code: 'spend_cap', message: 'daily cap reached', details: { cap_usd: 5 } };
  assert.equal(errorMessage(rejection), 'spend_cap: daily cap reached');
  assert.equal(errorCode(rejection), 'spend_cap');
  assert.deepEqual(errorDetails(rejection), { cap_usd: 5 });
  assert.equal(errorText(rejection), 'daily cap reached');
  assert.ok(toError(rejection) instanceof EngineError);
});

test('an object with a message but no code reads as the message', () => {
  assert.equal(errorMessage({ message: 'boom' }), 'boom');
  assert.equal(errorCode({ message: 'boom' }), null);
  assert.deepEqual(errorDetails({ message: 'boom', details: ['not', 'an', 'object'] }), null);
});

test('a nested {error: {...}} wrapper reads as the error it wraps', () => {
  const rejection = { error: { code: 'no_key', message: 'add a key in Settings' } };
  assert.equal(errorMessage(rejection), 'no_key: add a key in Settings');
  assert.equal(errorCode(rejection), 'no_key');
  assert.equal(errorMessage({ error: 'plain inner text' }), 'plain inner text');
});

test('an object with a code but an empty message keeps the code', () => {
  assert.equal(errorMessage({ code: 'job_not_found', message: '' }), 'job_not_found: Unknown error');
  assert.equal(errorCode({ code: 'job_not_found', message: '' }), 'job_not_found');
});

test('an unrecognised object falls back to its JSON, not [object Object]', () => {
  assert.equal(errorMessage({ status: 503 }), '{"status":503}');
});

test('a circular object does not throw and still reads as text', () => {
  const circular = {};
  circular.self = circular;
  assert.equal(errorMessage(circular), 'Unknown error');
});

test('a string reads as is, and a leading code is split off', () => {
  assert.equal(errorMessage('plain text'), 'plain text');
  assert.equal(errorMessage('invalid_request: seed must be 0'), 'invalid_request: seed must be 0');
  assert.equal(errorCode('invalid_request: seed must be 0'), 'invalid_request');
  assert.equal(errorText('invalid_request: seed must be 0'), 'seed must be 0');
});

test('a plain Error keeps its message and carries no code', () => {
  const err = new Error('Unsupported browser QA mock: x');
  assert.equal(errorMessage(err), 'Unsupported browser QA mock: x');
  assert.equal(errorCode(err), null);
  assert.equal(errorText(err), 'Unsupported browser QA mock: x');
});

test('an Error whose message starts with its code is split like an engine error', () => {
  const err = Object.assign(new Error('no_key: add a key'), { code: 'no_key' });
  assert.equal(errorMessage(err), 'no_key: add a key');
  assert.equal(errorText(err), 'add a key');
});

test('empty, blank, and missing rejections never read as empty text', () => {
  for (const rejection of ['', '   ', null, undefined, new Error(''), {}, { message: '' }, { message: { nested: true } }]) {
    const text = errorMessage(rejection);
    assert.ok(text.length > 0, `empty text for ${String(rejection)}`);
    assert.ok(!text.includes('[object Object]'), `object text for ${String(rejection)}`);
    assert.ok(errorText(rejection).length > 0, `empty errorText for ${String(rejection)}`);
  }
  assert.equal(errorMessage(null), 'Unknown error');
  assert.equal(errorMessage(''), 'Unknown error');
});
