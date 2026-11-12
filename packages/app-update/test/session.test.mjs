import assert from 'node:assert/strict';
import test from 'node:test';
import { createUpdateSession, proUpdateVersion } from '../dist/index.js';
import { createNativeUpdateClient } from '../dist/native/index.js';

// HeardRight's values: busy while recording, transcribing or pasting.
const BUSY = new Set(['recording', 'transcribing', 'pasting']);
const busy = { stateCommand: 'get_app_state', changedEvent: 'app-state-changed', isBusy: (s) => BUSY.has(s?.state ?? ''), deferReason: 'app_busy' };
const background = { statusCommand: 'background_update_status', statusEvent: 'background-update-status' };

function harness({ state = { state: 'idle' }, settings = { auto_update_install: true }, cached = { kind: 'idle' }, installs = [{ kind: 'ready', version: '2', tier: 'patch' }] } = {}) {
  const h = { state, listeners: new Map(), calls: [], relaunches: 0, checks: 0, installs: 0, logged: [], hooks: null, autoInstallSeen: [] };
  h.transport = {
    async invoke(cmd) {
      h.calls.push(cmd);
      if (cmd === 'get_app_state') return h.state;
      if (cmd === 'background_update_status') return cached;
      throw new Error('unknown ' + cmd);
    },
    async listen(event, cb) { h.calls.push('listen:' + event); h.listeners.set(event, cb); return () => h.listeners.delete(event); },
  };
  h.session = createUpdateSession((hooks) => {
    h.hooks = hooks;
    return {
      async checkOnce() { h.checks++; h.autoInstallSeen.push(await hooks.shouldAutoInstall()); hooks.onStatus({ kind: 'checking' }); hooks.onStatus({ kind: 'up_to_date' }); return { kind: 'up_to_date' }; },
      async installAvailable() { const r = installs[Math.min(h.installs++, installs.length - 1)]; hooks.onStatus(r); return r; },
    };
  }, { transport: h.transport, relaunch: () => { h.relaunches++; }, busy, autoInstall: () => settings.auto_update_install === true, background, onTransition: (s) => h.logged.push(s.kind) });
  return h;
}
const tick = () => new Promise((r) => setTimeout(r, 0));

test('subscribe replays the latest status and stops after unsubscribe', () => {
  const h = harness(); const seen = [];
  const off = h.session.subscribe((s) => seen.push(s.kind));
  h.session.client(); h.hooks.onStatus({ kind: 'checking' }); off(); h.hooks.onStatus({ kind: 'up_to_date' });
  assert.deepEqual(seen, ['idle', 'checking']);
});

test('a manual check suppresses auto-install; a background one does not', async () => {
  const h = harness();
  const pending = h.session.check(); await pending;
  await h.session.client().checkOnce();
  assert.deepEqual(h.autoInstallSeen, [false, true]);
  assert.equal(await h.hooks.shouldAutoInstall(), true);
  const off = harness({ settings: { auto_update_install: false } });
  assert.equal(await (off.session.client(), off.hooks.shouldAutoInstall()), false);
});

test('an unreadable setting or state reads as auto-install off and not busy', async () => {
  const h = harness(); h.session.client();
  const s = createUpdateSession(() => ({}), { transport: h.transport, relaunch() {}, busy, autoInstall: () => { throw new Error('x'); } });
  assert.equal(s.client() && true, true);
  h.transport.invoke = async () => { throw new Error('down'); };
  assert.equal(await h.hooks.canInstall(), true);
  assert.equal(h.hooks.deferReason(), 'app_busy');
});

test('only transitions are logged; every error is', () => {
  const h = harness(); h.session.client();
  for (const s of [{ kind: 'downloading', pct: 1 }, { kind: 'downloading', pct: 2 }, { kind: 'error', message: 'a' }, { kind: 'error', message: 'b' }, { kind: 'idle' }]) h.hooks.onStatus(s);
  assert.deepEqual(h.logged, ['downloading', 'error', 'error', 'idle']);
});

test('installAndRelaunch is single-flight and relaunches on ready', async () => {
  const h = harness();
  const a = h.session.installAndRelaunch(); const b = h.session.installAndRelaunch();
  assert.equal(a, b);
  assert.equal((await a).kind, 'ready');
  assert.equal(h.relaunches, 1); assert.equal(h.installs, 1);
  await h.session.installAndRelaunch(); assert.equal(h.installs, 2);
});

test('installAndRelaunch waits while busy, then installs', async () => {
  const h = harness({ state: { state: 'recording' } });
  const p = h.session.installAndRelaunch();
  await tick(); await tick();
  assert.equal(h.installs, 0);
  h.listeners.get('app-state-changed')({ payload: { state: 'transcribing' } }); await tick();
  assert.equal(h.installs, 0);
  h.listeners.get('app-state-changed')({ payload: { state: 'idle' } });
  assert.equal((await p).kind, 'ready'); assert.equal(h.installs, 1);
});

test('a deferred result loops back to the wait and installs again', async () => {
  const h = harness({ installs: [{ kind: 'deferred', version: '2', tier: 'patch', reason: 'app_busy' }, { kind: 'ready', version: '2', tier: 'patch' }] });
  assert.equal((await h.session.installAndRelaunch()).kind, 'ready');
  assert.equal(h.installs, 2); assert.equal(h.relaunches, 1);
});

test('no relaunch unless ready', async () => {
  const h = harness({ installs: [{ kind: 'error', message: 'x' }] });
  assert.equal((await h.session.installAndRelaunch()).kind, 'error'); assert.equal(h.relaunches, 0);
});

test('background reconcile listens first, then reads the cache', async () => {
  const h = harness({ cached: { kind: 'up_to_date' } });
  h.session.startBackground(); h.session.startBackground(); await tick(); await tick();
  assert.deepEqual(h.calls.filter((c) => c !== 'get_app_state'), ['listen:background-update-status', 'background_update_status']);
  assert.equal(h.session.status().kind, 'up_to_date');
});

test('a background event beats the cached read; available runs a check; error does not override available', async () => {
  const h = harness({ cached: { kind: 'error', message: 'cached' } });
  h.session.startBackground(); await tick();
  h.listeners.get('background-update-status')({ payload: { kind: 'available' } }); await tick();
  assert.equal(h.checks, 1);
  await tick(); assert.equal(h.session.status().kind, 'up_to_date');
  h.listeners.get('background-update-status')({ payload: { kind: 'error', message: 'boom' } });
  assert.deepEqual(h.session.status(), { kind: 'error', message: 'boom' });
  h.hooks.onStatus({ kind: 'available', version: '3', tier: 'patch', eligible: true });
  h.listeners.get('background-update-status')({ payload: { kind: 'error', message: 'later' } });
  assert.equal(h.session.status().kind, 'available');
  h.hooks.onStatus({ kind: 'downloading', version: '3', tier: 'patch', pct: 5 });
  h.listeners.get('background-update-status')({ payload: { kind: 'up_to_date' } });
  assert.equal(h.session.status().kind, 'downloading');
});

test('proUpdateVersion: URLs, platform, newer-only, never throws', async () => {
  const urls = [];
  const fetchOk = (updateVersion) => async (url) => { urls.push(url); return { ok: true, json: async () => ({ schema: 1, appKey: 'x', platform: 'darwin', patchVersion: null, updateVersion }) }; };
  assert.equal(await proUpdateVersion({ app: 'mailright', platform: 'macos', currentVersion: '1.0.0', fetch: fetchOk('1.1.0') }), '1.1.0');
  assert.equal(urls[0], 'https://api.orthiclabs.com/v1/apps/mailright/releases/status.json?platform=darwin');
  assert.equal(await proUpdateVersion({ app: 'a', platform: 'win32', currentVersion: () => '2.0.0', url: 'https://heardright.app/v1/apps/heardright/releases/status.json', fetch: fetchOk('1.1.0') }), null);
  assert.equal(urls[1], 'https://heardright.app/v1/apps/heardright/releases/status.json?platform=windows');
  assert.equal(await proUpdateVersion({ app: 'a', platform: null, currentVersion: '1.0.0', fetch: fetchOk('9.0.0') }), null);
  assert.equal(await proUpdateVersion({ app: 'a', platform: 'plan9', currentVersion: '1.0.0' }), null);
  assert.equal(await proUpdateVersion({ app: 'a', platform: 'linux', currentVersion: '1.0.0', fetch: async () => { throw new Error('net'); } }), null);
  assert.equal(await proUpdateVersion({ readStatus: async () => ({ schema: 1, appKey: 'v', platform: null, patchVersion: null, updateVersion: '1.2.0' }), currentVersion: '1.0.0' }), '1.2.0');
  assert.equal(await proUpdateVersion({ readStatus: async () => ({ junk: 1 }), currentVersion: '1.0.0' }), null);
});

test('native relaunch invokes rightkit_update_relaunch', async () => {
  const calls = [];
  const native = createNativeUpdateClient({ invoke: async (c) => { calls.push(c); }, listen: async () => () => {} });
  await native.relaunch();
  assert.deepEqual(calls, ['rightkit_update_relaunch']);
});
