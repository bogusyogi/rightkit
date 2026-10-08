import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createJobCommand, storageManagedAgentGc } from './index.mjs';
import { SOURCE_RUNTIME_IDENTITY } from '../../build-control-core/src/index.mjs';
test('storage GC uses runtime-bound typed commands without build admission', async () => {
  assert.deepEqual(createJobCommand('STORAGE_GC'), { type: 'STORAGE_GC', protocol: 'rightkit.build-control.client.v2', runtime: SOURCE_RUNTIME_IDENTITY, dryRun: true });
  const socket = new EventEmitter(); let sent; socket.destroy = () => {}; socket.write = value => { sent = JSON.parse(value); queueMicrotask(() => socket.emit('data', `${JSON.stringify({ type: 'HANDSHAKE', runtime: SOURCE_RUNTIME_IDENTITY })}\n${JSON.stringify({ type: 'STORAGE_GC', result: { gc: {} } })}\n`)); };
  const connect = () => { queueMicrotask(() => socket.emit('connect')); return socket; };
  const receipt = await storageManagedAgentGc({ dryRun: false, connect, endpoint: 'test.sock', runtimeIdentity: SOURCE_RUNTIME_IDENTITY });
  assert.equal(sent.lead, true); assert.equal(sent.dryRun, false); assert.equal(receipt.type, 'STORAGE_GC');
});
