import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createShell, EVENTS, DEFAULT_QUIT_TIMEOUT_MS, asShellError,
  SURFACE_COMMAND, PREVIEW_COMMANDS, LEGAL_RECEIPT_COMMANDS, UPDATE_COMMANDS, UPDATE_PROGRESS_EVENT,
} from '../dist/index.js';

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

// ---- Menu state, print, open paths, associations, window spec --------------------------------

const noop = () => undefined;

test('menu.setChecked and menu.setState call the registered commands with their native argument names', async () => {
  const host = fakeHost({ handlers: { rk_shell_set_menu_item_checked: noop, rk_shell_set_menu_state: noop } });
  const shell = createShell(host.transport);
  await shell.menu.setChecked('view.grid', true);
  await shell.menu.setState([{ id: 'file.save', enabled: false }, { id: 'view.grid', checked: false }]);
  assert.deepEqual(host.calls, [
    { command: 'rk_shell_set_menu_item_checked', args: { id: 'view.grid', checked: true } },
    { command: 'rk_shell_set_menu_state', args: { updates: [{ id: 'file.save', enabled: false }, { id: 'view.grid', checked: false }] } },
  ]);
});

test('print sends the Windows print UI choice, defaulting to null, and onPrintError listens on the shell event', async () => {
  const host = fakeHost({ handlers: { rk_shell_print: noop } });
  const shell = createShell(host.transport);
  await shell.print();
  await shell.print('system');
  assert.deepEqual(host.calls.map((c) => c.args), [{ ui: null }, { ui: 'system' }]);
  const seen = [];
  await shell.onPrintError((m) => seen.push(m));
  host.emit(EVENTS.printError, 'webview2 refused');
  assert.deepEqual(seen, ['webview2 refused']);
  assert.equal(EVENTS.printError, 'rightkit-shell://print-error');
});

test('openPaths.ready drains the granted display paths and onOpen delivers later ones', async () => {
  const host = fakeHost({ handlers: { rk_shell_open_paths_ready: () => ['C:\docs\a.md'] } });
  const shell = createShell(host.transport);
  const later = [];
  await shell.openPaths.onOpen((e) => later.push(...e.paths));
  assert.deepEqual(await shell.openPaths.ready(), ['C:\docs\a.md']);
  host.emit(EVENTS.openPaths, { paths: ['/tmp/b.md'] });
  assert.deepEqual(later, ['/tmp/b.md']);
  assert.equal(EVENTS.openPaths, 'rightkit-shell://open-paths');
});

test('associations pass only group ids and a timeout', async () => {
  const status = { groups: [{ id: 'docs', extensions: ['md'], handlers: [], allSet: false }] };
  const host = fakeHost({
    handlers: {
      rk_shell_associations_status: () => status,
      rk_shell_associations_set: () => ({ groups: status.groups, allSet: false, userActionRequired: true }),
      rk_shell_open_default_apps: noop,
    },
  });
  const shell = createShell(host.transport);
  assert.deepEqual(await shell.associations.status(), status);
  await shell.associations.status(['docs']);
  assert.equal((await shell.associations.set(['docs'], 5000)).userActionRequired, true);
  await shell.associations.set();
  await shell.associations.openDefaultApps();
  assert.deepEqual(host.calls.map((c) => [c.command, c.args]), [
    ['rk_shell_associations_status', { groups: null }],
    ['rk_shell_associations_status', { groups: ['docs'] }],
    ['rk_shell_associations_set', { groups: ['docs'], timeoutMs: 5000 }],
    ['rk_shell_associations_set', { groups: null, timeoutMs: null }],
    ['rk_shell_open_default_apps', undefined],
  ]);
});

test('window.open forwards every WindowSpec field and the tray accepts check and submenu entries as given', async () => {
  const host = fakeHost({ handlers: { rk_shell_open_window: (a) => a.spec.label, rk_shell_set_tray: noop, rk_shell_open_login_items_settings: noop } });
  const shell = createShell(host.transport);
  const spec = { label: 'setup', title: 'Setup', path: 'setup.html', width: 960, height: 640, focused: true, alwaysOnTop: false, minWidth: 800, minHeight: 500, decorations: false, center: true, fullscreen: false, skipTaskbar: true, x: 10, y: 20, parent: 'main', resizable: false, shadow: false, visible: false };
  assert.equal(await shell.window.open(spec), 'setup');
  const items = [{ kind: 'check', id: 'view.grid', label: 'Grid', checked: true }, { kind: 'submenu', label: 'Sort', entries: [{ kind: 'item', id: 'sort.name', label: 'Name' }, { kind: 'separator' }] }];
  await shell.tray.update({ items });
  await shell.autostart.openLoginItemsSettings();
  assert.deepEqual(host.calls[0].args, { spec });
  assert.deepEqual(host.calls[1].args, { update: { items } });
  assert.equal(host.calls[2].command, 'rk_shell_open_login_items_settings');
});

// ---- Surface, preview, legal receipt and update host commands -------------------------------

test('command and event constants carry the native names', () => {
  assert.equal(SURFACE_COMMAND, 'rk_shell_set_surface');
  assert.deepEqual({ ...PREVIEW_COMMANDS }, {
    register: 'rightkit_preview_register',
    revoke: 'rightkit_preview_revoke',
    revokeAll: 'rightkit_preview_revoke_all',
  });
  assert.deepEqual({ ...LEGAL_RECEIPT_COMMANDS }, {
    read: 'rightkit_legal_receipt_read',
    create: 'rightkit_legal_receipt_create',
    reset: 'rightkit_legal_receipt_reset',
  });
  assert.deepEqual({ ...UPDATE_COMMANDS }, {
    check: 'rightkit_update_check',
    status: 'rightkit_update_status',
    releaseStatus: 'rightkit_update_release_status',
    download: 'rightkit_update_download',
    install: 'rightkit_update_install',
    cancel: 'rightkit_update_cancel',
  });
  assert.equal(UPDATE_PROGRESS_EVENT, 'rightkit://update-progress');
  for (const table of [PREVIEW_COMMANDS, LEGAL_RECEIPT_COMMANDS, UPDATE_COMMANDS]) assert.ok(Object.isFrozen(table));
});

test('surface.set sends only the surface id and returns the host report', async () => {
  const report = { surface: 'setup', resized: true, revealed: true, skipped: 0 };
  const host = fakeHost({ handlers: { rk_shell_set_surface: () => report } });
  const shell = createShell(host.transport);
  assert.deepEqual(await shell.surface.set('setup'), report);
  assert.deepEqual(host.calls, [{ command: 'rk_shell_set_surface', args: { surface: 'setup' } }]);
});

test('preview wrappers send the kind and html, a token, and nothing else; the URL is the host answer', async () => {
  const handle = { token: 'a'.repeat(64), url: 'http://myapp-preview.localhost/' + 'a'.repeat(64) };
  const host = fakeHost({
    handlers: { rightkit_preview_register: () => handle, rightkit_preview_revoke: noop, rightkit_preview_revoke_all: noop },
  });
  const shell = createShell(host.transport);
  assert.deepEqual(await shell.preview.register('mdx', '<p>x</p>'), handle);
  await shell.preview.revoke(handle.token);
  await shell.preview.revokeAll();
  assert.deepEqual(host.calls.map((c) => [c.command, c.args]), [
    ['rightkit_preview_register', { kind: 'mdx', html: '<p>x</p>' }],
    ['rightkit_preview_revoke', { token: handle.token }],
    ['rightkit_preview_revoke_all', undefined],
  ]);
});

test('legal receipt wrappers send the declaration without a time and never a stored record', async () => {
  const record = { version: '1', documentsHash: 'h', acceptedAt: '2026-10-20T10:00:00.000Z', eligibilityBasis: 'individual' };
  const host = fakeHost({
    handlers: {
      rightkit_legal_receipt_read: () => null,
      rightkit_legal_receipt_create: () => record,
      rightkit_legal_receipt_reset: noop,
    },
  });
  const shell = createShell(host.transport);
  assert.equal(await shell.legal.read(), null);
  const documents = [{ id: 'terms', role: 'terms', version: '1', sha256: 'abc', material: true }];
  // Extra fields on the request (a time, a stored record) must not reach the host.
  const request = { version: '1', documentsHash: 'h', documents, eligibilityBasis: 'individual', surface: 'first_run', acceptedAt: 'forged', record };
  assert.deepEqual(await shell.legal.create(request), record);
  await shell.legal.reset();
  assert.deepEqual(host.calls[1].args, {
    request: { version: '1', documentsHash: 'h', documents, eligibilityBasis: 'individual', surface: 'first_run' },
  });
  assert.equal('write' in shell.legal, false);
  assert.deepEqual(host.commands(), ['rightkit_legal_receipt_read', 'rightkit_legal_receipt_create', 'rightkit_legal_receipt_reset']);
});

test('host-service rejections pass through as the host error envelope', async () => {
  const host = fakeHost({
    handlers: {
      rightkit_legal_receipt_create: () => {
        throw { code: 'legal_hash_mismatch', message: 'the documents shown are not the documents this app requires' };
      },
    },
  });
  const shell = createShell(host.transport);
  await assert.rejects(
    shell.legal.create({ version: '1', documentsHash: 'h', documents: [], eligibilityBasis: 'enterprise', surface: 'first_run' }),
    (e) => e.code === 'legal_hash_mismatch' && asShellError(e).code === 'legal_hash_mismatch',
  );
});

test('update wrappers carry only a handle id in, closed DTOs out, and progress on the update event', async () => {
  const check = { available: true, kind: 'update', version: '2.0.0', eligible: true, refusal: 'none', handleId: 'h1', checkedAt: 1, fresh: true };
  const host = fakeHost({
    handlers: {
      rightkit_update_check: () => check,
      rightkit_update_status: () => ({ state: 'idle', message: 'none' }),
      rightkit_update_download: () => ({ handleId: 'h1', state: 'downloaded', message: 'none' }),
      rightkit_update_install: () => ({ state: 'installing', message: 'none' }),
      rightkit_update_cancel: () => ({ cancelled: false }),
    },
  });
  const shell = createShell(host.transport);
  assert.deepEqual(await shell.update.check(), check);
  assert.equal((await shell.update.status()).state, 'idle');
  assert.equal((await shell.update.download('h1')).state, 'downloaded');
  assert.equal((await shell.update.install('h1')).state, 'installing');
  assert.deepEqual(await shell.update.cancel('h1'), { cancelled: false });
  assert.deepEqual(host.calls.map((c) => [c.command, c.args]), [
    ['rightkit_update_check', undefined],
    ['rightkit_update_status', undefined],
    ['rightkit_update_download', { handleId: 'h1' }],
    ['rightkit_update_install', { handleId: 'h1' }],
    ['rightkit_update_cancel', { handleId: 'h1' }],
  ]);
  const seen = [];
  const off = await shell.update.onProgress((p) => seen.push(p));
  host.emit('rightkit://update-progress', { handleId: 'h1', received: 5, total: 10 });
  off();
  host.emit('rightkit://update-progress', { handleId: 'h1', received: 10, total: 10 });
  assert.deepEqual(seen, [{ handleId: 'h1', received: 5, total: 10 }]);
  assert.equal(host.listenerCount('rightkit://update-progress'), 0);
});
