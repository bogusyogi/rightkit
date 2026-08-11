import assert from 'node:assert/strict';
import test from 'node:test';
import { dispatchHookEvent, normalizeHookEvent } from '../lib/runtime.mjs';

test('normalizes host spellings without interpreting policy', () => {
  const event = normalizeHookEvent({ hook_event_name: 'PreToolUse', thread_id: 'thread-1', tool_name: 'Write' });
  assert.equal(event.event, 'PreToolUse');
  assert.equal(event.sessionId, 'thread-1');
  assert.equal(event.toolName, 'Write');
});

test('dispatches modules in declared order', async () => {
  const order = [];
  const result = await dispatchHookEvent({ event: 'Stop' }, [
    { id: 'memory', handle: () => order.push('memory') },
    { id: 'arcane', handle: () => order.push('arcane') },
  ]);
  assert.deepEqual(order, ['memory', 'arcane']);
  assert.equal(result.status, 'ok');
  assert.deepEqual(result.results.map(({ id }) => id), ['memory', 'arcane']);
});

test('returns typed module timeout without inventing allow or deny policy', async () => {
  const result = await dispatchHookEvent({ event: 'SessionStart' }, [{
    id: 'slow',
    handle: () => new Promise(() => {}),
  }], { timeoutMs: 5 });
  assert.equal(result.status, 'error');
  assert.match(result.results[0].error, /timed out/);
});
