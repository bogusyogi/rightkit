import test from 'node:test';
import assert from 'node:assert/strict';

// The resolver hook must be registered before the .ts modules load, so these imports are dynamic.
await import('./ts-resolve.mjs');
const { DEVICE_ALLOWANCE_SCOPE, allowanceStatusText, auditRowText, parseAllowanceAmount, sharedAllowanceText } = await import('../views/device-allowance-logic.ts');
const { formatUsd } = await import('../views/spend-reconcile-logic.ts');
const { mockEngine } = await import('../lib/browser-mock-api.ts');

const allowance = (params) => mockEngine('command', { name: 'spend.set_allowance', input: params });
const allowanceView = () => mockEngine('command', { name: 'spend.allowance' });

test('the scope line says the allowance applies to this device only', () => {
  assert.match(DEVICE_ALLOWANCE_SCOPE, /this device only/);
});

test('the shared-allowance copy repeats the engine reason and never claims a shared allowance', () => {
  const text = sharedAllowanceText({ available: false, reason: 'no sync service: each device enforces its own offline allowance' });
  assert.equal(text, 'No allowance is shared across computers yet (no sync service: each device enforces its own offline allowance).');
  assert.match(sharedAllowanceText({ available: true, reason: '' }), /available/);
});

test('the headline reads unset as "caps still apply", and set as amount, counted and remaining', () => {
  assert.equal(allowanceStatusText({ device_allowance_usd: null, counted_usd: null, remaining_usd: null }), 'No allowance is set for this device. The daily and monthly caps still apply.');
  assert.equal(
    allowanceStatusText({ device_allowance_usd: 5, counted_usd: 1.25, remaining_usd: 3.75 }),
    `${formatUsd(5)} on this device. ${formatUsd(1.25)} counted since it was set; ${formatUsd(3.75)} remaining.`
  );
});

test('the typed amount is a plain non-negative dollar figure, and an empty field is refused', () => {
  assert.deepEqual(parseAllowanceAmount('2.5'), { ok: true, value: 2.5 });
  assert.deepEqual(parseAllowanceAmount(' 0 '), { ok: true, value: 0 });
  assert.equal(parseAllowanceAmount('').ok, false);
  assert.equal(parseAllowanceAmount('-1').ok, false);
  assert.equal(parseAllowanceAmount('1e3').ok, false);
  assert.match(parseAllowanceAmount('abc').message, /dollar amount of 0 or more/);
});

test('an audit row reads as when, who, old to new, and the note', () => {
  const text = auditRowText({ id: 1, at: '2026-10-12T10:00:00Z', who: 'local user', device_id: 'd', old_usd: null, new_usd: 5, note: 'laptop' });
  assert.equal(text, `2026-10-12T10:00:00Z · local user: none → ${formatUsd(5)} (laptop)`);
  const cleared = auditRowText({ id: 2, at: 't', who: 'local user', device_id: 'd', old_usd: 5, new_usd: null, note: null });
  assert.equal(cleared, `t · local user: ${formatUsd(5)} → cleared`);
});

test('the browser mirror sets, clears and audits the device allowance like the engine', async () => {
  const before = await allowanceView();
  assert.equal(before.device_allowance_usd, null);
  assert.equal(before.counted_usd, null);
  assert.equal(before.shared_allowance.available, false);
  assert.match(before.shared_allowance.reason, /each device enforces its own offline allowance/);

  const set = await allowance({ device_allowance_usd: 3, note: '  laptop share  ' });
  assert.equal(set.device_allowance_usd, 3);
  assert.equal(set.counted_usd, 0);
  assert.equal(set.remaining_usd, 3);
  assert.equal(set.audit[0].note, 'laptop share');
  assert.equal(set.audit[0].old_usd, null);
  assert.equal(set.audit[0].who, 'local user');

  const cleared = await allowance({ device_allowance_usd: null });
  assert.equal(cleared.device_allowance_usd, null);
  assert.equal(cleared.set_at, null);
  assert.equal(cleared.audit.length, 2);
  assert.equal(cleared.audit[0].new_usd, null);
  assert.equal(cleared.audit[0].old_usd, 3);
});

test('the browser mirror refuses a missing, string or negative amount and a non-string note', async () => {
  await assert.rejects(allowance({}), /missing field: device_allowance_usd/);
  await assert.rejects(allowance({ device_allowance_usd: '3' }), /non-negative number or null/);
  await assert.rejects(allowance({ device_allowance_usd: -1 }), /non-negative number or null/);
  await assert.rejects(allowance({ device_allowance_usd: 1, note: 7 }), /note must be a string/);
});
