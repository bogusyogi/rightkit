import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createShell, EVENTS, DEFAULT_QUIT_TIMEOUT_MS } from '../dist/index.js';

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const flush = () => new Promise((resolve) => setImmediate(resolve));

/**
 * Fake Tauri transport. `caps` answers `rk_shell_capabilities`; `missing` lists commands the host
 * does not register (Tauri rejects those with a plain "Command X not found" string).
 */
function fakeHost({ caps = { quitDrain: true, notifyBypassDnd: true }, missing = [], handlers = {} } = {}) {
  const calls = [];
  const listeners = new Map();
  const table = {
    rk_shell_capabilities: () => caps,
    rk_shell_finish_quit: () => undefined,
    rk_shell_quit: () => undefined,
    rk_shell_notify: () => ({ delivered: true, suppressed: undefined, id: '1', sound: false }),
    ...handlers,
  };
  const transport = {
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (missing.includes(command) || !(command in table)) throw `Command ${command} not found`;
      return table[command](args);
    },
    listen: async (event, handler) => {
      let set = listeners.get(event);
      if (!set) listeners.set(event, (set = new Set()));
      set.add(handler);
      return () => {
        set.delete(handler);
      };
    },
  };
  return {
    transport,
    calls,
    commands: () => calls.map((c) => c.command),
    emit: (event, payload) => {
      for (const h of [...(listeners.get(event) ?? [])]) h(payload);
    },
    listenerCount: (event) => listeners.get(event)?.size ?? 0,
  };
}

// ---- 1. Before-quit drain -------------------------------------------------------------------

test('onQuitRequested fails with a typed unsupported error when the host lacks the drain', async () => {
  const host = fakeHost({ caps: { quitDrain: false, notifyBypassDnd: false } });
  const shell = createShell(host.transport);
  await assert.rejects(shell.onQuitRequested(() => {}), (e) => {
    assert.equal(e.code, 'unsupported');
    assert.match(e.message, /rk_shell_finish_quit/);
    assert.match(e.message, /rightkit-shell:\/\/quit-requested/);
    return true;
  });
  assert.equal(host.listenerCount(EVENTS.quitRequested), 0);
});

test('onQuitRequested reads as unsupported when the capability probe itself is not registered', async () => {
  const host = fakeHost({ missing: ['rk_shell_capabilities'] });
  const shell = createShell(host.transport);
  await assert.rejects(shell.onQuitRequested(() => {}), { code: 'unsupported' });
});

test('quit({ force: true }) quits at once and runs no drain handlers', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  let ran = false;
  await shell.onQuitRequested(() => {
    ran = true;
  });
  const report = await shell.quit({ force: true });
  assert.equal(ran, false);
  assert.equal(report.outcome, 'forced');
  assert.ok(host.commands().includes('rk_shell_quit'));
  assert.ok(!host.commands().includes('rk_shell_finish_quit'));
});

test('quit waits for every drain handler, then calls rk_shell_finish_quit once', async () => {
  const log = [];
  const host = fakeHost({
    handlers: {
      rk_shell_finish_quit: () => {
        log.push('finish');
      },
    },
  });
  const shell = createShell(host.transport);
  await shell.onQuitRequested(async () => {
    await delay(20);
    log.push('a');
  });
  await shell.onQuitRequested(() => {
    log.push('b-sync');
  });
  await shell.onQuitRequested(async () => {
    await delay(5);
    log.push('c');
  });
  const report = await shell.quit();
  assert.deepEqual(log, ['b-sync', 'c', 'a', 'finish']);
  assert.equal(report.outcome, 'drained');
  assert.equal(report.pending, 0);
  assert.deepEqual(report.errors, []);
  assert.equal(host.commands().filter((c) => c === 'rk_shell_finish_quit').length, 1);
  assert.ok(!host.commands().includes('rk_shell_quit'));
});

test('a rejecting drain handler does not stop quit, and its error is reported', async () => {
  const reports = [];
  const host = fakeHost();
  const shell = createShell(host.transport, { onQuitReport: (r) => reports.push(r) });
  let otherRan = false;
  await shell.onQuitRequested(() => {
    throw new Error('send failed');
  });
  await shell.onQuitRequested(async () => {
    await delay(5);
    otherRan = true;
  });
  const report = await shell.quit();
  assert.equal(otherRan, true);
  assert.equal(report.outcome, 'drained');
  assert.equal(report.errors.length, 1);
  assert.equal(report.errors[0].code, 'other');
  assert.match(report.errors[0].message, /send failed/);
  assert.equal(reports.length, 1);
  assert.deepEqual(reports[0], report);
  assert.ok(host.commands().includes('rk_shell_finish_quit'));
});

test('quit proceeds after the caller-set timeout when a handler never settles', async () => {
  const reports = [];
  const host = fakeHost();
  const shell = createShell(host.transport, { onQuitReport: (r) => reports.push(r) });
  await shell.onQuitRequested(() => new Promise(() => {}));
  const started = Date.now();
  const report = await shell.quit({ timeoutMs: 30 });
  assert.equal(report.outcome, 'timed-out');
  assert.equal(report.pending, 1);
  assert.ok(Date.now() - started < 2000);
  assert.ok(host.commands().includes('rk_shell_finish_quit'));
  assert.equal(reports.length, 1);
  assert.equal(reports[0].outcome, 'timed-out');
});

test('the default drain budget is 10 seconds and createShell can change it', async () => {
  assert.equal(DEFAULT_QUIT_TIMEOUT_MS, 10_000);
  const host = fakeHost();
  const shell = createShell(host.transport, { quitTimeoutMs: 25, onQuitReport: () => {} });
  await shell.onQuitRequested(() => new Promise(() => {}));
  const report = await shell.quit();
  assert.equal(report.outcome, 'timed-out');
});

test('invalid timeouts are rejected as invalid', async () => {
  const host = fakeHost();
  assert.throws(() => createShell(host.transport, { quitTimeoutMs: Number.NaN }), (e) => e.code === 'invalid');
  const shell = createShell(host.transport);
  await assert.rejects(shell.quit({ timeoutMs: -1 }), { code: 'invalid' });
  await assert.rejects(shell.quit({ timeoutMs: Number.POSITIVE_INFINITY }), { code: 'invalid' });
});

test('a host quit request runs the handlers once and finishes; unsubscribed handlers do not run', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  let kept = 0;
  let dropped = 0;
  await shell.onQuitRequested(() => {
    kept += 1;
  });
  const off = await shell.onQuitRequested(() => {
    dropped += 1;
  });
  off();
  off(); // second unsubscribe is a no-op
  host.emit(EVENTS.quitRequested, {});
  host.emit(EVENTS.quitRequested, {}); // a repeat while the drain is in flight joins it
  await flush();
  assert.equal(kept, 1);
  assert.equal(dropped, 0);
  assert.equal(host.commands().filter((c) => c === 'rk_shell_finish_quit').length, 1);
});

test('quit() on a host without the drain quits at once, as before', async () => {
  const host = fakeHost({ caps: { quitDrain: false, notifyBypassDnd: false } });
  const shell = createShell(host.transport);
  const report = await shell.quit();
  assert.deepEqual(host.commands().filter((c) => c !== 'rk_shell_capabilities'), ['rk_shell_quit']);
  assert.equal(report.outcome, 'immediate');
});

// ---- 2. Custom tray events -------------------------------------------------------------------

test('tray.onCustom listens on the app event name and delivers its payload', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  const got = [];
  const off = await shell.tray.onCustom('tray-compose', (p) => got.push(p));
  host.emit('tray-compose', null);
  off();
  host.emit('tray-compose', null);
  assert.deepEqual(got, [null]);
  assert.equal(host.listenerCount('tray-compose'), 0);
});

test('tray.onCustom rejects empty names and reserved shell event names as invalid', async () => {
  const shell = createShell(fakeHost().transport);
  await assert.rejects(shell.tray.onCustom('', () => {}), { code: 'invalid' });
  await assert.rejects(shell.tray.onCustom(EVENTS.tray, () => {}), { code: 'invalid' });
  await assert.rejects(shell.tray.onCustom('rightkit-shell://anything', () => {}), { code: 'invalid' });
});

test('tray.onEvent still forwards menu and click events unchanged', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  const got = [];
  await shell.tray.onEvent((e) => got.push(e));
  host.emit(EVENTS.tray, { kind: 'menu', id: 'tray-compose' });
  host.emit(EVENTS.tray, { kind: 'click' });
  assert.deepEqual(got, [{ kind: 'menu', id: 'tray-compose' }, { kind: 'click' }]);
});

// ---- 3. Notifications bypassing Do-Not-Disturb ----------------------------------------------

test('notifications.post without the option sends the request unchanged', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  await shell.notifications.post({ title: 'Hi', body: 'x' });
  const call = host.calls.find((c) => c.command === 'rk_shell_notify');
  assert.deepEqual(call.args, { request: { title: 'Hi', body: 'x' } });
});

test('notifications.post with bypassDoNotDisturb: false sends the request unchanged', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  await shell.notifications.post({ title: 'Hi', bypassDoNotDisturb: false });
  const call = host.calls.find((c) => c.command === 'rk_shell_notify');
  assert.deepEqual(call.args, { request: { title: 'Hi' } });
});

test('notifications.post with bypassDoNotDisturb sends the flag when the host supports it', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  const outcome = await shell.notifications.post({ title: 'Hi', bypassDoNotDisturb: true });
  const call = host.calls.find((c) => c.command === 'rk_shell_notify');
  assert.deepEqual(call.args, { request: { title: 'Hi', bypassDoNotDisturb: true } });
  assert.equal(outcome.delivered, true);
});

test('bypassDoNotDisturb is refused as unsupported and nothing is posted when the host lacks it', async () => {
  const host = fakeHost({ caps: { quitDrain: true, notifyBypassDnd: false } });
  const shell = createShell(host.transport);
  await assert.rejects(shell.notifications.post({ title: 'Hi', bypassDoNotDisturb: true }), (e) => {
    assert.equal(e.code, 'unsupported');
    assert.match(e.message, /notifyBypassDnd|bypass/i);
    return true;
  });
  assert.ok(!host.commands().includes('rk_shell_notify'));
});

// ---- Capability probe ------------------------------------------------------------------------

test('capabilities are probed once, and a missing probe command reads as no capabilities', async () => {
  const host = fakeHost();
  const shell = createShell(host.transport);
  assert.deepEqual(await shell.capabilities(), { quitDrain: true, notifyBypassDnd: true });
  await shell.capabilities();
  assert.equal(host.calls.filter((c) => c.command === 'rk_shell_capabilities').length, 1);

  const older = fakeHost({ missing: ['rk_shell_capabilities'] });
  assert.deepEqual(await createShell(older.transport).capabilities(), { quitDrain: false, notifyBypassDnd: false });
});

test('a capability probe failure other than a missing command propagates as a typed error', async () => {
  const host = fakeHost({
    handlers: {
      rk_shell_capabilities: () => {
        throw { code: 'io', message: 'broken' };
      },
    },
  });
  await assert.rejects(createShell(host.transport).capabilities(), { code: 'io', message: 'broken' });
});
