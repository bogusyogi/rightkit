import assert from 'node:assert/strict';
import test from 'node:test';
import { decodePayload, encodePayload, generateRemoteRequestId, isTerminalRemoteState, RemoteProtocolError, validateRemoteCargoArgv, DEFAULT_REMOTE_DEADLINE_MS } from './remote-protocol.mjs';

const submitPayload = {
  v: 1, op: 'SUBMIT', requestId: 'rr-1-abcdef',
  repositoryKey: 'heardright-a1b2c3d4', digest: 'a'.repeat(64), bytes: 1024,
  argv: ['check'], createdAt: '2026-01-01T00:00:00.000Z',
};

const b64 = (value) => Buffer.from(JSON.stringify(value), 'utf8').toString('base64url');

test('valid SUBMIT payload decodes with defaults applied & freezes result', () => {
  const decoded = decodePayload(b64(submitPayload));
  assert.equal(decoded.op, 'SUBMIT');
  assert.equal(decoded.deadlineMs, DEFAULT_REMOTE_DEADLINE_MS);
  assert.deepEqual(decoded.argv, ['check']);
  assert.equal(Object.isFrozen(decoded), true);
});

test('encode/decode round-trips canonical bytes for all three operations', () => {
  for (const minimal of [{ v: 1, op: 'STATUS', requestId: 'rr-1-abcdef' }, { v: 1, op: 'CANCEL', requestId: 'rr-1-abcdef' }]) {
    const encoded = encodePayload(minimal);
    assert.match(encoded, /^[A-Za-z0-9_-]+$/);
    assert.equal(decodePayload(encoded).op, minimal.op);
  }
  const encoded = encodePayload({ ...submitPayload, deadlineMs: 60_000 });
  assert.deepEqual(decodePayload(encoded).argv, ['check']);
});

test('payload rejections: version, op, charset, unknown fields, identity patterns', () => {
  assert.throws(() => decodePayload(b64({ ...submitPayload, v: 2 })), RemoteProtocolError);
  assert.throws(() => decodePayload(b64({ ...submitPayload, op: 'WATCH' })), /SUBMIT, STATUS, or CANCEL/);
  assert.throws(() => decodePayload(Buffer.from('{bad json').toString('base64url')), RemoteProtocolError);
  assert.throws(() => decodePayload('not+base64url=='), /base64url/); // + and = are outside the alphabet
  assert.throws(() => decodePayload(b64({ ...submitPayload, extraField: 1 })), /unknown payload field/);
  assert.throws(() => decodePayload(b64({ ...submitPayload, requestId: '../escape' })), RemoteProtocolError);
  assert.throws(() => decodePayload(b64({ ...submitPayload, digest: 'zz' })), /digest/);
  assert.throws(() => decodePayload(b64({ ...submitPayload, bytes: 0 })), /bytes/);
  assert.throws(() => decodePayload(b64({ ...submitPayload, deadlineMs: 30_000 })), /deadlineMs/);
  assert.throws(() => decodePayload(b64({ ...submitPayload, deadlineMs: 25 * 60 * 60 * 1000 })), /deadlineMs/);
  // STATUS/CANCEL payloads must not carry submit-only fields.
  assert.throws(() => decodePayload(b64({ v: 1, op: 'STATUS', requestId: 'rr-1', argv: ['check'] })), /unknown payload field/);
});

test('remote cargo verb firewall admits registered read-only verbs only', () => {
  for (const verb of ['check', 'clippy', 'test', 'build']) assert.deepEqual([...validateRemoteCargoArgv([verb])], [verb]);
  assert.deepEqual([...validateRemoteCargoArgv(['test', '--', '--nocapture', 'filter'])], ['test', '--', '--nocapture', 'filter']);
  assert.deepEqual([...validateRemoteCargoArgv(['build', '--release'])], ['build', '--release']);
  for (const verb of ['fix', 'fmt', 'run', 'update', 'install', 'publish', 'clean', 'vendor']) {
    assert.throws(() => validateRemoteCargoArgv([verb]), (error) => error.code === 'REMOTE_COMMAND_DENIED');
  }
  assert.throws(() => validateRemoteCargoArgv([]), RemoteProtocolError);
});

test('remote cargo flag firewall denies source-mutating & authority-stealing flags', () => {
  for (const argv of [
    ['clippy', '--fix'],
    ['fix'],
    ['check', '--manifest-path=C:\\elsewhere\\Cargo.toml'],
    ['build', '--target-dir=D:\\elsewhere'],
    ['test', '-Zunstable'],
    ['check', '--config=profile.dev.opt-level=3'],
  ]) assert.throws(() => validateRemoteCargoArgv(argv), (error) => error.code === 'REMOTE_COMMAND_DENIED', JSON.stringify(argv));
});

test('request ids are unique, bounded, and pattern-safe', () => {
  const first = generateRemoteRequestId(() => 1_000);
  const second = generateRemoteRequestId(() => 2_000);
  assert.notEqual(first, second);
  assert.match(first, /^rr-\d+-[0-9a-f]{16}$/);
});

test('terminal-state helper matches the contract state set', () => {
  for (const state of ['TERMINAL', 'LOST', 'FAILED', 'CANCELLED', 'DEADLINE']) assert.equal(isTerminalRemoteState(state), true);
  for (const state of ['PREPARING', 'QUEUED', 'RUNNING']) assert.equal(isTerminalRemoteState(state), false);
});
