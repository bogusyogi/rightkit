import assert from 'node:assert/strict';
import test from 'node:test';
import {
  ACCOUNT_MESSAGES, LICENSE_MESSAGES, LICENSE_MESSAGES_EN_US, accountDeletionMessage, acknowledgeExpiredTrialNotice,
  createAccountClient, createLicenseClient, createLicenseLifecycleMonitor, dialogForLicenseLifecycle,
  expiredTrialAcknowledgementKey, isExpiredTrialNoticeAcknowledged, licenseMessage, millisecondsUntilLicenseCheck,
  shouldRefreshLicenseFromServer, snapshotFromLicenseView,
} from '../dist/index.js';

const view = (over = {}) => ({
  mode: 'configured', status: 'active', lifecycle: 'trial', tier: 'pro', features: [], kind: 'trial', stage: 'x',
  validUntil: 1_700_000_000_999, confirmationDueAt: null, staleSince: null, licenseIdSuffix: 'abcd',
  busy: false, unresolved: false, revisionState: 'active', message: 'none', ...over,
});
const snap = (over) => ({ kind: 'expired_trial', isPro: false, entitlementId: 'abcd', expiresAtUnix: 100, ...over });
const memory = () => { const m = new Map(); return { getItem: k => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v), m }; };

test('snapshot projects times to seconds, busy keeps plan, unresolved is storage_error', () => {
  const s = snapshotFromLicenseView(view({ busy: true }));
  assert.equal(s.kind, 'active_trial');
  assert.equal(s.isPro, true);
  assert.equal(s.expiresAtUnix, 1_700_000_000);
  assert.equal(s.message, null);
  assert.deepEqual(snapshotFromLicenseView(view({ unresolved: true })), { kind: 'storage_error', isPro: false, message: null });
  assert.equal(snapshotFromLicenseView(view({ lifecycle: 'revoked' })).kind, 'free');
  assert.equal(snapshotFromLicenseView(view({ lifecycle: 'grace' })).kind, 'active_legacy');
});

test('snapshot message wording can be overridden', () => {
  const v = view({ message: 'unknown' });
  assert.equal(snapshotFromLicenseView(v).message, LICENSE_MESSAGES.unknown);
  assert.equal(snapshotFromLicenseView(v, { unknown: 'Oops.' }).message, 'Oops.');
});

test('refresh policy and timer boundaries', () => {
  for (const kind of ['verification_required', 'clock_repair_required', 'migration_required', 'active_legacy']) {
    assert.equal(shouldRefreshLicenseFromServer({ kind, isPro: false }, true), true);
    assert.equal(shouldRefreshLicenseFromServer({ kind, isPro: false }, false), false);
  }
  assert.equal(shouldRefreshLicenseFromServer({ kind: 'free', isPro: false }, true), false);
  assert.equal(millisecondsUntilLicenseCheck({ kind: 'free', isPro: false }, 0), 60_000);
  assert.equal(millisecondsUntilLicenseCheck({ kind: 'active_trial', isPro: true, expiresAtUnix: 10 }, 4_000), 6_000);
  assert.equal(millisecondsUntilLicenseCheck({ kind: 'active_trial', isPro: true, expiresAtUnix: 10 }, 99_000), 0);
  assert.equal(millisecondsUntilLicenseCheck({ kind: 'active_lifetime_provisional', isPro: true, confirmationDueAtUnix: Infinity }), 0);
});

test('dialog copy matches HeardRight with its configuration', () => {
  const o = { appName: 'HeardRight', paidTier: 'Pro', spelling: 'en-US' };
  assert.deepEqual(dialogForLicenseLifecycle(snap(), o), {
    title: 'Your Pro trial has expired', body: 'You can purchase Pro or continue using HeardRight for free.',
    primaryAction: 'Purchase Pro', secondaryAction: 'Continue for free',
  });
  assert.equal(dialogForLicenseLifecycle(snap({ kind: 'verification_required' }), o).body,
    'HeardRight needs one internet connection to confirm the purchase was not refunded. After confirmation, your lifetime license works offline.');
  const clock = dialogForLicenseLifecycle(snap({ kind: 'clock_repair_required' }), o);
  assert.equal(clock.body, 'Your device clock moved backwards. Correct the date and time, then connect once so HeardRight can verify the license safely.');
  assert.equal(clock.primaryAction, 'Retry license check');
  const mig = dialogForLicenseLifecycle(snap({ kind: 'migration_required' }), o);
  assert.deepEqual([mig.title, mig.body, mig.primaryAction, mig.secondaryAction], [
    'Connect once to update your license', 'HeardRight needs one internet connection to move this older license into secure offline lifecycle storage.',
    'Update license', 'Continue with Free']);
  assert.equal(dialogForLicenseLifecycle(snap({ kind: 'free' }), o), null);
  assert.match(dialogForLicenseLifecycle(snap({ kind: 'migration_required' }), { appName: 'A', paidTier: 'Pro' }).title, /licence/);
});

test('expired trial acknowledgement: key, legacy full-id match, host key and storage', () => {
  assert.equal(expiredTrialAcknowledgementKey(snap()), 'expired_trial:abcd:100');
  assert.equal(expiredTrialAcknowledgementKey(snap({ entitlementId: null })), 'expired_trial:none:100');
  assert.equal(expiredTrialAcknowledgementKey(snap({ kind: 'free' })), null);
  const storage = memory();
  const o = { storageKey: 'hr.expiredTrialNoticeAck.v1', storage };
  assert.equal(isExpiredTrialNoticeAcknowledged(snap(), o), false);
  acknowledgeExpiredTrialNotice(snap(), o);
  assert.equal(storage.m.get('hr.expiredTrialNoticeAck.v1'), 'expired_trial:abcd:100');
  assert.equal(isExpiredTrialNoticeAcknowledged(snap(), o), true);
  assert.equal(isExpiredTrialNoticeAcknowledged(snap({ expiresAtUnix: 101 }), o), false);
  // An earlier build stored the full id.
  storage.m.set('hr.expiredTrialNoticeAck.v1', 'expired_trial:LIC-0000-abcd:100');
  assert.equal(isExpiredTrialNoticeAcknowledged(snap(), o), true);
  storage.m.set('hr.expiredTrialNoticeAck.v1', 'expired_trial:LIC-0000-zzzz:100');
  assert.equal(isExpiredTrialNoticeAcknowledged(snap(), o), false);
  storage.m.set('hr.expiredTrialNoticeAck.v1', 'expired_trial:LIC-0000-abcd:99');
  assert.equal(isExpiredTrialNoticeAcknowledged(snap(), o), false);
  // A failing store never throws.
  const broken = { getItem() { throw new Error('x'); }, setItem() { throw new Error('x'); } };
  assert.equal(isExpiredTrialNoticeAcknowledged(snap(), { storageKey: 'k', storage: broken }), false);
  assert.doesNotThrow(() => acknowledgeExpiredTrialNotice(snap(), { storageKey: 'k', storage: broken }));
});

// A minimal window/document with fake timers.
function env() {
  const handlers = new Map();
  const timers = new Map();
  let nextId = 1;
  const doc = new Map();
  globalThis.window = {
    addEventListener: (n, f) => { handlers.set(n, [...(handlers.get(n) ?? []), f]); },
    removeEventListener: (n, f) => { handlers.set(n, (handlers.get(n) ?? []).filter(x => x !== f)); },
    setTimeout: (f, ms) => { timers.set(nextId, { f, ms }); return nextId++; },
    clearTimeout: id => { timers.delete(id); },
  };
  globalThis.document = {
    visibilityState: 'visible',
    addEventListener: (n, f) => { doc.set(n, f); },
    removeEventListener: n => { doc.delete(n); },
  };
  return { handlers, timers, doc, fire: n => (handlers.get(n) ?? []).forEach(f => f()), listeners: n => (handlers.get(n) ?? []).length };
}
const flush = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

test('monitor: order, online rules, afterChange before subscribers, timer clamp, single flight', async () => {
  const e = env();
  const calls = [];
  let current = view({ lifecycle: 'verification_required' });
  const client = {
    retryPendingReleases: async () => { calls.push('retry'); throw new Error('ignored'); },
    status: async () => { calls.push('status'); return current; },
    refresh: async () => { calls.push('refresh'); current = view({ lifecycle: 'lifetime' }); },
  };
  const monitor = createLicenseLifecycleMonitor({
    client, afterChange: async s => { calls.push(`after:${s.kind}`); },
  });
  const seen = [];
  monitor.subscribe(s => { seen.push(s.kind); calls.push(`pub:${s.kind}`); });
  assert.equal(monitor.get().kind, 'checking');
  // Not explicit: no server refresh.
  await monitor.refresh();
  assert.deepEqual(calls.slice(1), ['retry', 'status', 'after:verification_required', 'pub:verification_required']);
  // A free state arms the 60s fallback.
  assert.equal([...e.timers.values()].length, 1);
  assert.equal([...e.timers.values()][0].ms, 60_000);
  calls.length = 0;
  // Explicit: refresh in between, one in flight at a time.
  const a = monitor.refresh({ online: true });
  const b = monitor.refresh({ online: true });
  assert.equal(a, b);
  await a;
  assert.deepEqual(calls, ['retry', 'status', 'refresh', 'status', 'after:active_lifetime', 'pub:active_lifetime']);
  assert.deepEqual(seen, ['checking', 'verification_required', 'active_lifetime']);
  assert.equal(e.timers.size, 1);
  // Trial boundary under a second still waits at least 1 ms; a far boundary is capped at 60 s.
  current = view({ lifecycle: 'trial', validUntil: Date.now() + 10 * 3_600_000 });
  await monitor.refresh();
  assert.equal([...e.timers.values()][0].ms, 60_000);
  current = view({ lifecycle: 'trial', validUntil: Date.now() - 5000 });
  await monitor.refresh();
  assert.equal([...e.timers.values()][0].ms, 1);
  delete globalThis.window; delete globalThis.document;
});

test('monitor: failures publish storage_error, a failing afterChange included', async () => {
  const e = env();
  const monitor = createLicenseLifecycleMonitor({
    client: { retryPendingReleases: async () => undefined, status: async () => view(), refresh: async () => undefined },
    afterChange: async () => { throw new Error('sync failed'); },
  });
  const s = await monitor.refresh();
  assert.deepEqual(s, { kind: 'storage_error', isPro: false, message: 'sync failed' });
  assert.equal(monitor.get(), s);
  assert.equal(e.timers.size, 1);
  delete globalThis.window; delete globalThis.document;
});

test('monitor: start wires focus, visibility, host events and the client change event; stop undoes them', async () => {
  const e = env();
  let statusCalls = 0; let refreshCalls = 0; let onChangeCb; let unlistened = 0;
  const monitor = createLicenseLifecycleMonitor({
    client: {
      retryPendingReleases: async () => undefined,
      status: async () => { statusCalls++; return view({ lifecycle: 'active' }); },
      refresh: async () => { refreshCalls++; },
      onChange: async cb => { onChangeCb = cb; return () => { unlistened++; }; },
    },
    changeEventNames: ['hr-license-updated'],
  });
  const stop = monitor.start();
  await flush();
  assert.equal(statusCalls, 2);
  assert.equal(refreshCalls, 1); // start checks online; active_legacy asks the server
  e.fire('focus'); await flush();
  e.fire('hr-license-updated'); await flush();
  assert.equal(refreshCalls, 3);
  e.doc.get('visibilitychange')(); await flush();
  assert.equal(refreshCalls, 4);
  globalThis.document.visibilityState = 'hidden';
  e.doc.get('visibilitychange')(); await flush();
  assert.equal(refreshCalls, 4);
  // The client's own change event re-reads without asking the server.
  onChangeCb(view()); await flush();
  assert.equal(refreshCalls, 4);
  assert.equal(statusCalls, 9);
  stop();
  assert.equal(unlistened, 1);
  assert.equal(e.listeners('focus'), 0);
  assert.equal(e.listeners('hr-license-updated'), 0);
  assert.equal(e.doc.size, 0);
  assert.equal(e.timers.size, 0);
  delete globalThis.window; delete globalThis.document;
});

test('message overrides: defaults unchanged, overrides apply to both clients', async () => {
  assert.equal(licenseMessage('unknown'), 'Something went wrong.');
  assert.equal(licenseMessage('unknown', { unknown: 'Nope.' }), 'Nope.');
  assert.match(LICENSE_MESSAGES_EN_US.keyNotAccepted ?? Object.values(LICENSE_MESSAGES_EN_US)[0], /./);
  assert.ok(Object.values(LICENSE_MESSAGES_EN_US).every(t => !/\blicence/i.test(t)));
  const reject = async () => { throw { code: 'network' }; };
  const license = createLicenseClient({ invoke: reject, listen: async () => () => undefined, messages: { unknown: 'Own words.' } });
  await assert.rejects(license.status(), e => e.name === 'LicenseClientError');
  await assert.rejects(createLicenseClient({ invoke: async () => 5, listen: async () => () => undefined, messages: { unknown: 'Own words.' } }).status(), { message: 'Own words.' });
  await assert.rejects(createLicenseClient({ invoke: async () => 5, listen: async () => () => undefined }).status(), { message: LICENSE_MESSAGES.unknown });
  const account = createAccountClient({ invoke: async () => { throw { code: 'sessionExpired' }; }, messages: { sessionExpired: 'Sign in again, please.' } });
  await assert.rejects(account.status(), { message: 'Sign in again, please.' });
  await assert.rejects(createAccountClient({ invoke: async () => { throw { code: 'sessionExpired' }; } }).status(), { message: ACCOUNT_MESSAGES.sessionExpired });
  assert.throws(() => account.deleteAccount({}), { name: 'AccountClientError' });
});

test('accountDeletionMessage', () => {
  assert.equal(accountDeletionMessage({ outcome: 'deleted', localSessionCleared: true }), '');
  assert.equal(accountDeletionMessage({ outcome: 'refused', reason: 'activeSubscription' }), 'You have an active subscription. Cancel it before deleting your account, then try again.');
  assert.equal(accountDeletionMessage({ outcome: 'refused', reason: 'rateLimited' }), 'Too many attempts. Try again shortly.');
  assert.equal(accountDeletionMessage({ outcome: 'refused', reason: 'other' }), 'The account service declined the deletion. Try again later.');
  assert.equal(accountDeletionMessage({ outcome: 'indeterminate' }), "We couldn't confirm your account was deleted. Check your connection and try again.");
  assert.equal(accountDeletionMessage({ outcome: 'indeterminate' }, { indeterminate: 'x' }), 'x');
});
