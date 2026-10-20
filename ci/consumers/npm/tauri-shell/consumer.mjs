import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const gaps = ['settings/logs/license adapters are not tauri-shell exports', 'raw update/legal replies are not runtime validated', 'asShellError preserves arbitrary typed envelopes', 'onQuitRequested unsubscribe removes handler but retains shared host listener'];
const result = { schemaVersion: 1, recipeId: 'tauri-shell-installed-transport-consumer', ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], qualificationGaps: gaps, rendererSafetyQualified: false, nativeHostQualified: false, errors: [] };
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted host error.
let stage = 'artifact-provenance';
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(path.isAbsolute(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert(['packed', 'fetched'].includes(resolution));
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/tauri-shell', rootResolvedUrl: import.meta.resolve('@rightkit/tauri-shell'), entries: { '@rightkit/tauri-shell': import.meta.resolve('@rightkit/tauri-shell') } });
  result.artifact = provenance.artifact;
  const api = await import(provenance.entries['@rightkit/tauri-shell']);
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };
  function memoryHost(handlers = {}) {
    const calls = [], listeners = new Map();
    const transport = {
      invoke: async (command, args) => {
        calls.push({ command, args });
        if (!Object.hasOwn(handlers, command)) throw new Error(`Command ${command} not found`);
        return handlers[command](args);
      },
      listen: async (event, handler) => {
        if (!listeners.has(event)) listeners.set(event, new Set());
        listeners.get(event).add(handler);
        return () => listeners.get(event).delete(handler);
      },
    };
    return { transport, calls, emit: (event, payload) => { for (const fn of [...(listeners.get(event) ?? [])]) fn(payload); }, count: event => listeners.get(event)?.size ?? 0 };
  }
  await run('menu-bridge-open-path-subscription-cleanup', async () => {
    const host = memoryHost({ rk_shell_open_paths_ready: () => ['fixture/document.txt'], rk_shell_set_menu_state: () => undefined });
    const shell = api.createShell(host.transport), actions = [], opened = [];
    const offBridge = await shell.bridge().onMenuAction(value => actions.push(value));
    const offOpen = await shell.openPaths.onOpen(value => opened.push(value));
    try {
      host.emit(api.EVENTS.menu, { id: 'save' }); host.emit(api.EVENTS.tray, { kind: 'menu', id: 'compose' });
      host.emit(api.EVENTS.tray, { kind: 'click' });
      host.emit(api.EVENTS.deepLink, { raw: 'fixture://open', scheme: 'fixture', host: 'open', segments: [], query: {} });
      assert.deepEqual(actions, ['save', 'compose', 'deep-link:fixture://open']);
      assert.deepEqual(await shell.openPaths.ready(), ['fixture/document.txt']);
      host.emit(api.EVENTS.openPaths, { paths: ['fixture/second.txt'] });
      assert.deepEqual(opened, [{ paths: ['fixture/second.txt'] }]);
      const updates = [{ id: 'save', enabled: false, checked: true }];
      await shell.menu.setState(updates);
      assert.deepEqual(host.calls.at(-1), { command: 'rk_shell_set_menu_state', args: { updates } });
    } finally { offBridge(); offOpen(); }
    for (const event of [api.EVENTS.menu, api.EVENTS.tray, api.EVENTS.deepLink, api.EVENTS.openPaths]) assert.equal(host.count(event), 0);
    host.emit(api.EVENTS.menu, { id: 'after-unsubscribe' });
    assert.equal(actions.length, 3);
    return { actions, liveListenersAfterStop: 0, openPathEvents: opened.length };
  });
  await run('legal-declaration-projection-no-renderer-timestamp', async () => {
    let stored = null;
    const record = { version: 'fixture-v1', documentsHash: 'a'.repeat(64), acceptedAt: '2026-10-20T00:00:00.000Z', eligibilityBasis: 'individual' };
    const host = memoryHost({ [api.LEGAL_RECEIPT_COMMANDS.read]: () => stored, [api.LEGAL_RECEIPT_COMMANDS.create]: () => (stored = record), [api.LEGAL_RECEIPT_COMMANDS.reset]: () => { stored = null; } });
    const shell = api.createShell(host.transport);
    assert.equal(await shell.legal.read(), null);
    const declaration = { version: record.version, documentsHash: record.documentsHash, documents: [{ id: 'terms', role: 'terms', sha256: 'b'.repeat(64) }], eligibilityBasis: 'individual', surface: 'first_run' };
    assert.deepEqual(await shell.legal.create({ ...declaration, acceptedAt: 'forged', token: 'fixture-private-marker', record: { acceptedAt: 'forged' } }), record);
    assert.deepEqual(host.calls.at(-1), { command: api.LEGAL_RECEIPT_COMMANDS.create, args: { request: declaration } });
    assert.equal('write' in shell.legal, false);
    assert.deepEqual(await shell.legal.read(), record); await shell.legal.reset(); assert.equal(await shell.legal.read(), null);
    return { projectedDeclaration: true, timestampSource: 'memory-host-fixture', nativeReceiptAuthorityProven: false };
  });
  await run('update-command-shapes-cancel-refusal-progress-cleanup', async () => {
    let state = 'available';
    const check = { available: true, kind: 'patch', version: '2.0.0', eligible: true, refusal: 'none', handleId: 'fixture_1', checkedAt: 1, fresh: true };
    const host = memoryHost({
      [api.UPDATE_COMMANDS.check]: () => check,
      [api.UPDATE_COMMANDS.status]: () => ({ state, message: 'none' }),
      [api.UPDATE_COMMANDS.download]: () => { state = 'downloaded'; return { handleId: 'fixture_1', state, message: 'none' }; },
      [api.UPDATE_COMMANDS.cancel]: () => { state = 'idle'; return { cancelled: true }; },
      [api.UPDATE_COMMANDS.install]: () => ({ state: 'refused', reason: 'stale', message: 'updateRefused' }),
    });
    const shell = api.createShell(host.transport), progress = [];
    const off = await shell.update.onProgress(value => progress.push(value));
    try {
      assert.deepEqual(await shell.update.check(), check);
      assert.equal((await shell.update.status()).state, 'available');
      assert.equal((await shell.update.download('fixture_1')).state, 'downloaded');
      host.emit(api.UPDATE_PROGRESS_EVENT, { handleId: 'fixture_1', received: 3, total: 10 });
      assert.deepEqual(progress, [{ handleId: 'fixture_1', received: 3, total: 10 }]);
      assert.deepEqual(await shell.update.cancel('fixture_1'), { cancelled: true });
      assert.equal((await shell.update.status()).state, 'idle');
      assert.deepEqual(await shell.update.install('fixture_1'), { state: 'refused', reason: 'stale', message: 'updateRefused' });
      for (const call of host.calls.filter(call => [api.UPDATE_COMMANDS.download, api.UPDATE_COMMANDS.cancel, api.UPDATE_COMMANDS.install].includes(call.command))) assert.deepEqual(call.args, { handleId: 'fixture_1' });
    } finally { off(); }
    assert.equal(host.count(api.UPDATE_PROGRESS_EVENT), 0);
    host.emit(api.UPDATE_PROGRESS_EVENT, { handleId: 'fixture_1', received: 4 }); assert.equal(progress.length, 1);
    return { cancelled: true, refusal: 'stale', progressEvents: 1, liveListenersAfterStop: 0 };
  });
  await run('missing-capability-typed-refusal-no-side-effects', async () => {
    const host = memoryHost(), shell = api.createShell(host.transport);
    await assert.rejects(shell.onQuitRequested(() => {}), error => error.code === 'unsupported');
    await assert.rejects(shell.notifications.post({ title: 'fixture', bypassDoNotDisturb: true }), error => error.code === 'unsupported');
    await assert.rejects(shell.tray.onCustom(api.EVENTS.tray, () => {}), error => error.code === 'invalid');
    assert.equal(host.count(api.EVENTS.quitRequested), 0);
    assert.deepEqual(host.calls.map(call => call.command), ['rk_shell_capabilities']);
    return { unsupported: 2, invalidReservedEvent: true, nativeSideEffects: 0 };
  });
  await run('raw-boundary-gap-detection-fails-closed-qualification', async () => {
    const marker = 'fixture-private-marker';
    const unknown = { state: 'unknown-state', token: marker };
    const host = memoryHost({ [api.UPDATE_COMMANDS.status]: () => unknown });
    const shell = api.createShell(host.transport);
    assert.strictEqual(await shell.update.status(), unknown);
    const envelope = { code: 'unknown-code', message: marker, token: marker };
    assert.strictEqual(api.asShellError(envelope), envelope);
    for (const key of ['settings', 'logs', 'license']) assert.equal(key in shell, false);
    return { unknownDtoRejectedByShell: false, typedEnvelopeSanitizedByShell: false, gapConfirmedWithoutLoggingPayload: true };
  });
  stage = 'qualification-mode';
  const mode = process.env.RIGHTKIT_CONSUMER_REQUIREMENT ?? 'transport';
  assert(['transport', 'renderer-safe-host-services'].includes(mode));
  if (mode === 'renderer-safe-host-services') throw new Error('unsupported qualification scope');
  result.ok = true;
} catch {
  result.errors.push({ stage, code: stage === 'qualification-mode' ? 'unsupported-qualification-scope' : 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
