import test from 'node:test';
import assert from 'node:assert/strict';

// The resolver hook must be registered before the .ts modules load, so these imports are dynamic.
await import('./ts-resolve.mjs');
const logic = await import('../views/rentals-logic.ts');
const { mockEngine } = await import('../lib/browser-mock-api.ts');

const {
  RENTAL_ENFORCEMENT_TEXT,
  dueText,
  enforceLines,
  formatDuration,
  limitRows,
  rentalActions,
  rentalFigures,
  rentalStatusLabel,
  rentalStatusTone,
  rentalWarnings,
  settlementText,
  usd,
} = logic;

const NOTABLE = ['renting', 'active', 'outcome_unknown', 'destroying', 'destroy_unconfirmed', 'destroyed_unsettled', 'settled', 'released'];

function entry(overrides = {}) {
  return {
    worker_id: 'rw-1',
    label: 'render',
    worker_state: 'running',
    rental_status: 'active',
    instance_id: 900101,
    elapsed_sec: 2400,
    rate_usd_hr: 0.34,
    accrued_usd: 0.2267,
    budget_usd: 2,
    max_duration_min: 60,
    idle_limit_min: null,
    idle_for_sec: null,
    due_in_sec: { duration: 1200, budget: 18000, idle: null },
    due: null,
    auto_destroyed: null,
    warnings: [],
    ...overrides,
  };
}

test('every notable status reads in words, and an unknown status passes through', () => {
  for (const status of NOTABLE) {
    const label = rentalStatusLabel(status);
    assert.ok(label.length > 0 && label !== status, `label for ${status}`);
  }
  assert.match(rentalStatusLabel('outcome_unknown'), /Outcome unknown/);
  assert.match(rentalStatusLabel('destroy_unconfirmed'), /not confirmed/);
  assert.match(rentalStatusLabel('destroyed_unsettled'), /charge not yet settled/);
  assert.equal(rentalStatusLabel('something_new'), 'something_new');
});

test('status tones flag the billing risks', () => {
  assert.equal(rentalStatusTone('outcome_unknown'), 'bad');
  assert.equal(rentalStatusTone('destroy_unconfirmed'), 'bad');
  assert.equal(rentalStatusTone('active'), 'run');
  assert.equal(rentalStatusTone('settled'), 'ok');
  assert.equal(rentalStatusTone('never_seen'), undefined);
});

test('durations and due-in text read plainly and never print NaN', () => {
  assert.equal(formatDuration(0), '0 s');
  assert.equal(formatDuration(45), '45 s');
  assert.equal(formatDuration(600), '10 min');
  assert.equal(formatDuration(3900), '1 h 5 min');
  assert.equal(formatDuration(7200), '2 h');
  assert.equal(formatDuration(-30), '0 s');
  assert.equal(formatDuration(null), 'unknown');
  assert.equal(formatDuration(Number.NaN), 'unknown');
  assert.equal(dueText(1200, 'unknown'), 'due in 20 min');
  assert.equal(dueText(-120, 'unknown'), 'past due by 2 min');
  assert.equal(dueText(null, 'time left unknown'), 'time left unknown');
});

test('usd reads two places, four below a cent, and unknown for missing', () => {
  assert.equal(usd(0.34), '$0.34');
  assert.equal(usd(0.0047), '$0.0047');
  assert.equal(usd(0), '$0.00');
  assert.equal(usd(null), 'unknown');
  assert.equal(usd(Number.NaN), 'unknown');
});

test('the actions offered follow the status: destroy, resolve, or nothing', () => {
  assert.deepEqual(rentalActions('active', 'idle'), [{ id: 'destroy', blocker: null }]);
  assert.deepEqual(rentalActions('destroy_unconfirmed', 'staged'), [{ id: 'destroy', blocker: null }]);
  assert.deepEqual(rentalActions('renting', null), [{ id: 'resolve', blocker: null }]);
  assert.deepEqual(rentalActions('outcome_unknown', 'configured'), [{ id: 'resolve', blocker: null }]);
  for (const status of ['destroying', 'destroyed_unsettled', 'settled', 'released']) {
    assert.deepEqual(rentalActions(status, 'terminated'), [], `no action for ${status}`);
  }
});

test('destroy is offered but blocked while the worker stages or retrieves', () => {
  const [staging] = rentalActions('active', 'staging');
  assert.equal(staging.id, 'destroy');
  assert.match(staging.blocker, /setting up/);
  const [retrieving] = rentalActions('active', 'retrieving');
  assert.match(retrieving.blocker, /copying results back/);
});

test('a running worker needs force on destroy, which the panel names in its confirm', () => {
  assert.equal(logic.destroyNeedsForce('running'), true);
  assert.equal(logic.destroyNeedsForce('idle'), false);
  assert.equal(logic.destroyNeedsForce(undefined), false);
});

test('outcome_unknown and renting lead with the provider-may-have-started warning', () => {
  const unknown = rentalWarnings(entry({ rental_status: 'outcome_unknown', warnings: ['the Vast create may have been delivered: an instance labelled for this worker may exist and bill; the spend reservation stays open until resolve_rental'] }));
  assert.match(unknown[0], /Outcome unknown: the provider may have started a rented worker that is charging/);
  assert.equal(unknown.length, 1, 'the engine warning repeats the derived one and is dropped');
  const creating = rentalWarnings(entry({ rental_status: 'renting' }));
  assert.match(creating[0], /the provider may have started a rented worker/);
});

test('destroy_unconfirmed says it may still be billing exactly once', () => {
  const warnings = rentalWarnings(entry({
    rental_status: 'destroy_unconfirmed',
    warnings: ['the Vast instance may still be billing: destroy was not confirmed by the provider; the spend reservation stays open', 'rental status is destroy_unconfirmed'],
  }));
  const billing = warnings.filter((w) => /may still be (billing|charging)/i.test(w));
  assert.equal(billing.length, 1);
  assert.ok(!warnings.some((w) => /^rental status is/.test(w)));
});

test('a healthy active rental carries no warnings, and limit warnings from the engine stay in plain words', () => {
  assert.deepEqual(rentalWarnings(entry()), []);
  const warnings = rentalWarnings(entry({ warnings: ['no hourly rate is known: the budget limit cannot be enforced'] }));
  assert.deepEqual(warnings, ['No hourly price is known, so the budget limit cannot be enforced.']);
});

test('engine sentences that name env vars, commands or keys show plain text only', () => {
  const plain = [
    'VAST_API_KEY is not set in this process, so the instance cannot be destroyed from here',
    'the Vast create may have been delivered: an instance labelled for this worker may exist and bill; the spend reservation stays open until resolve_rental',
    'no instance carries this worker\'s label; pass confirm_none:true only when sure the create never reached Vast (the reservation stays open until then)',
    'force:true destroyed a rental whose worker was running: any work in flight on the instance was interrupted',
  ].map((t) => logic.plainEngineText(t));
  for (const text of plain) {
    assert.doesNotMatch(text, /VAST_API_KEY|confirm_none|resolve_rental|force:true|rental|instance|billing|reservation/, text);
  }
  assert.equal(logic.plainEngineText('an engine sentence no rule knows'), 'an engine sentence no rule knows');
});

test('limit rows show due-in only for an active rental', () => {
  const active = entry();
  const rows = limitRows(active, rentalFigures(active));
  assert.equal(rows[0].name, 'Duration');
  assert.equal(rows[0].due, 'due in 20 min');
  assert.equal(rows[1].due, 'due in 5 h');
  assert.equal(rows[2].due, null, 'no idle limit when the worker is not idle');
  const settled = entry({ rental_status: 'settled', elapsed_sec: 2400 });
  for (const row of limitRows(settled, rentalFigures(settled, { elapsed_sec: 7344, actual_usd: 1.02, rate_usd_hr: 0.5 }))) {
    assert.equal(row.due, null, `no due-in for a settled ${row.name}`);
  }
});

test('a settled rental reads its final figures from the record, not the clock-based listing', () => {
  const figures = rentalFigures(entry({ rental_status: 'settled', elapsed_sec: 99999, accrued_usd: 9 }), { elapsed_sec: 7344, actual_usd: 1.02, rate_usd_hr: 0.5 });
  assert.equal(figures.live, false);
  assert.equal(figures.elapsedSec, 7344);
  assert.equal(figures.accruedUsd, 1.02);
  assert.equal(figures.rateUsdHr, 0.5);
});

test('settlement text names the overrun above the reservation', () => {
  assert.equal(settlementText(null), null);
  assert.equal(settlementText({ status: 'active' }), null);
  assert.equal(settlementText({ actual_usd: 1.02, reserved_usd: 1, overrun_usd: 0.02 }), 'charge settled at $1.02; $0.02 over the $1.00 reserved budget');
  assert.equal(settlementText({ actual_usd: 0.4, reserved_usd: 1, overrun_usd: 0 }), 'charge settled at $0.40');
  assert.match(settlementText({ actual_usd: 1, reserved_usd: 1, overrun_usd: 0, settled_on_declared_budget: true }), /declared budget/);
});

test('the limit-check report names each outcome, and a busy lock says nothing was checked', () => {
  const none = { at_epoch: 1, destroyed: [], failed: [], blocked: [], unresolved: [], in_progress: [], settled_only: [], persist_errors: [], busy: false };
  assert.deepEqual(enforceLines(none), ['Nothing was due: no rented worker is past a limit.']);
  assert.match(enforceLines({ ...none, busy: true })[0], /Nothing was checked/);
  const lines = enforceLines({
    ...none,
    blocked: [{ worker_id: 'rw-a', reason: 'duration', detail: 'x', error: 'VAST_API_KEY is not set in this process', billing_warning: 'the instance is past a limit and may still be billing' }],
    unresolved: [{ worker_id: 'rw-b', status: 'outcome_unknown', action: 'run resolve_rental' }],
    destroyed: [{ worker_id: 'rw-c', reason: 'budget', settlement: { actual_usd: 1.02, reserved_usd: 1, overrun_usd: 0.02 } }],
  });
  assert.ok(lines.some((l) => /Not destroyed rw-a \(past its duration limit\): App does not have your Vast\.ai API key/.test(l) && /may still be charging/.test(l)));
  assert.ok(lines.some((l) => /rw-b: outcome unknown\. Use Find at provider first/.test(l)));
  assert.ok(lines.some((l) => /Destroyed rw-c: past its budget limit\. Charge settled at \$1\.02/.test(l)));
  assert.ok(lines.every((l) => !/VAST_API_KEY|resolve_rental|confirm_none|billing/.test(l)), 'no developer names in the report');
});

test('the enforcement statement names the key requirement and the lack of an always-on service', () => {
  assert.match(RENTAL_ENFORCEMENT_TEXT, /VAST_API_KEY/);
  assert.match(RENTAL_ENFORCEMENT_TEXT, /nothing enforces any limit/);
});

test('the browser preview lists one rental in each notable status with the engine statement', async () => {
  const listing = await mockEngine('command', { name: 'studio.models.rentals', input: {} });
  assert.equal(listing.enforcement, RENTAL_ENFORCEMENT_TEXT);
  const statuses = new Set(listing.rentals.map((r) => r.rental_status));
  for (const status of NOTABLE) assert.ok(statuses.has(status), `preview has a ${status} rental`);
  const idle = listing.rentals.find((r) => r.worker_id === 'rw-rental-idle');
  assert.equal(idle.idle_limit_min, 10);
  assert.ok(typeof idle.due_in_sec.idle === 'number');
  assert.equal(idle.due, 'idle', 'the idle rental is past its limit in the preview');
  for (const row of listing.rentals) {
    assert.ok(Array.isArray(row.warnings), `warnings array on ${row.worker_id}`);
  }
});

test('the preview refuses a running teardown without force, and a resolve on an active rental, without changing either', async () => {
  await assert.rejects(
    mockEngine('command', { name: 'studio.models.worker.action', input: { worker_id: 'rw-rental-running', action: 'teardown' } }),
    (e) => e.code === 'bad_state' && /"force": true/.test(e.message),
  );
  await assert.rejects(
    mockEngine('command', { name: 'studio.models.worker.action', input: { worker_id: 'rw-rental-running', action: 'resolve_rental' } }),
    (e) => e.code === 'bad_state',
  );
});

test('the preview holds an unknown outcome: resolve without confirm_none is refused and changes nothing', async () => {
  await assert.rejects(
    mockEngine('command', { name: 'studio.models.worker.action', input: { worker_id: 'rw-rental-unknown', action: 'resolve_rental' } }),
    (e) => e.code === 'rental_unresolved',
  );
  const listing = await mockEngine('command', { name: 'studio.models.rentals', input: {} });
  assert.equal(listing.rentals.find((r) => r.worker_id === 'rw-rental-unknown').rental_status, 'outcome_unknown');
});

test('the preview lists rental records in the workers list, and the limit check reports blocked rentals without a key', async () => {
  const workers = await mockEngine('command', { name: 'studio.models.workers', input: {} });
  const rental = workers.workers.find((w) => w.worker_id === 'rw-rental-unsettled');
  assert.equal(rental.rental.status, 'destroyed_unsettled');
  const report = await mockEngine('command', { name: 'studio.models.rentals_enforce', input: {} });
  assert.equal(report.busy, false);
  assert.ok(report.unresolved.some((u) => u.worker_id === 'rw-rental-unknown'), 'an unknown outcome is reported, not destroyed');
  assert.ok(report.blocked.some((b) => b.worker_id === 'rw-rental-idle' && b.reason === 'idle' && /VAST_API_KEY/.test(b.error)), 'a due rental without a key is blocked');
});
