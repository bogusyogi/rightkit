import assert from 'node:assert/strict';
import { mkdtemp, open, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { BoundedEventLog, EventLogDurabilityError } from './event-log.mjs';

test('event log serializes structured append events', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-events-')); try {
    const log = new BoundedEventLog({ path: join(root, 'events.ndjson'), now: () => 0 });
    await Promise.all([log.append({ type: 'QUEUE', id: 'a' }), log.append({ type: 'RUNNING', id: 'a' })]); await log.flush();
    const events = (await readFile(log.path, 'utf8')).trim().split('\n').map(JSON.parse);
    assert.deepEqual(events.map(({ type }) => type), ['QUEUE', 'RUNNING']); assert.equal(events[0].at, '1970-01-01T00:00:00.000Z');
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('event log bounds retained NDJSON tail on event boundary', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-events-')); try {
    const path = join(root, 'events.ndjson'); const log = new BoundedEventLog({ path, maxBytes: 120 });
    await log.append({ type: 'A', payload: '1'.repeat(20) }); await log.append({ type: 'B', payload: '2'.repeat(20) });
    const contents = await readFile(path, 'utf8'); assert.ok(Buffer.byteLength(contents) <= 120); assert.doesNotThrow(() => contents.trim().split('\n').map(JSON.parse)); assert.equal(JSON.parse(contents.trim()).type, 'B');
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('event log rejects entries larger than retention bound', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-events-')); try { await assert.rejects(new BoundedEventLog({ path: join(root, 'events.ndjson'), maxBytes: 20 }).append({ type: 'LONG', payload: 'x'.repeat(30) }), /exceeds/); } finally { await rm(root, { recursive: true, force: true }); }
});

test('one failed append rejects its caller but cannot poison later lifecycle writes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-events-'));
  try {
    const path = join(root, 'events.ndjson'); let failOnce = true;
    const log = new BoundedEventLog({
      path,
      io: {
        open: async (target, flags) => {
          const handle = await open(target, flags);
          if (target === path && flags === 'a' && failOnce) {
            failOnce = false;
            return { ...handle, writeFile: async () => { throw new Error('injected write failure'); }, sync: handle.sync.bind(handle), close: handle.close.bind(handle) };
          }
          return handle;
        },
      },
    });
    await assert.rejects(log.append({ type: 'QUEUE', id: 'first' }), (error) => error instanceof EventLogDurabilityError && error.code === 'RK_EVENT_LOG_DURABILITY_UNCONFIRMED' && error.stage === 'file');
    const result = await log.appendResult({ type: 'TERMINAL', id: 'second' });
    assert.deepEqual(result, { event: { type: 'TERMINAL', id: 'second', at: result.event.at }, durable: true, fileSynced: true, directorySynced: true });
    assert.deepEqual((await readFile(path, 'utf8')).trim().split('\n').map(JSON.parse).map(({ id }) => id), ['second']);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('appendResult proves file plus directory fsync before success', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-events-'));
  try {
    const path = join(root, 'events.ndjson'); const opened = []; const synced = [];
    const log = new BoundedEventLog({
      path,
      io: {
        open: async (target, flags) => {
          const handle = await open(target, flags); opened.push({ target, flags });
          return { writeFile: handle.writeFile.bind(handle), sync: async () => { synced.push({ target, flags }); await handle.sync(); }, close: handle.close.bind(handle) };
        },
      },
    });
    const result = await log.appendResult({ type: 'RUNNING', id: 'durable' });
    assert.equal(result.durable, true); assert.equal(result.fileSynced, true); assert.equal(result.directorySynced, true);
    assert.deepEqual(opened.map(({ flags }) => flags), ['a', 'r']);
    assert.deepEqual(synced.map(({ flags }) => flags), ['a', 'r']);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('directory sync failure is typed, reloads visible state, then recovers queue', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-events-'));
  try {
    const path = join(root, 'events.ndjson'); let failOnce = true;
    const log = new BoundedEventLog({
      path,
      io: {
        open: async (target, flags) => {
          const handle = await open(target, flags);
          if (target === root && flags === 'r' && failOnce) {
            failOnce = false;
            return { sync: async () => { throw new Error('injected directory fsync failure'); }, close: handle.close.bind(handle) };
          }
          return handle;
        },
      },
    });
    await assert.rejects(log.append({ type: 'RUNNING', id: 'first' }), (error) => error instanceof EventLogDurabilityError && error.stage === 'directory' && error.persisted === 'unknown');
    await log.append({ type: 'TERMINAL', id: 'second' });
    assert.deepEqual((await readFile(path, 'utf8')).trim().split('\n').map(JSON.parse).map(({ id }) => id), ['first', 'second']);
  } finally { await rm(root, { recursive: true, force: true }); }
});
