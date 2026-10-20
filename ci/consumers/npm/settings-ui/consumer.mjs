import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const result = { recipeId: 'settings-ui-react-memory-mount', schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
let dom, cleanup;
try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/settings-ui', rootResolvedUrl: import.meta.resolve('@rightkit/settings-ui'), entries: { '@rightkit/settings-ui/core': import.meta.resolve('@rightkit/settings-ui/core'), '@rightkit/settings-ui/standard': import.meta.resolve('@rightkit/settings-ui/standard') } });
  result.artifact = provenance.artifact;
  const { JSDOM } = await import(fixtureEntry(import.meta.url, import.meta.resolve('jsdom')));
  dom = new JSDOM('<!doctype html><html><body></body></html>', { url: 'https://consumer.invalid/', pretendToBeVisual: true });
  for (const key of ['window', 'document', 'navigator', 'HTMLElement', 'Element', 'Node', 'MutationObserver', 'getComputedStyle']) {
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value: key === 'getComputedStyle' ? dom.window.getComputedStyle.bind(dom.window) : dom.window[key] });
  }
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const testing = await import(fixtureEntry(import.meta.url, import.meta.resolve('@testing-library/react')));
  const { render, screen, waitFor, act } = testing;
  cleanup = testing.cleanup;
  const userEvent = (await import(fixtureEntry(import.meta.url, import.meta.resolve('@testing-library/user-event')))).default;
  const { SettingsPanel, SettingRow, SettingsGroup } = await import(provenance.entries['@rightkit/settings-ui/core']);
  const { licenseSection } = await import(provenance.entries['@rightkit/settings-ui/standard']);
  const h = React.createElement;
  const secret = 'fixture-private-license-token-never-render';
  function memorySettings() {
    const values = { theme: 'light', volume: 10, otherWindow: 'old', retained: { nested: 'keep' } };
    let revision = 1, failNext = false, loads = 0;
    const listeners = new Set(), patches = [];
    const snapshot = () => ({ values: structuredClone(values), revision });
    return { values, patches, listeners, get loads() { return loads; }, failNext: () => { failNext = true; },
      external(patch) { Object.assign(values, structuredClone(patch)); revision++; for (const notify of listeners) notify(Object.keys(patch)); },
      backend: {
        load: async () => { loads++; return snapshot(); },
        patch: async (patch, expectedRevision) => {
          patches.push({ patch: structuredClone(patch), expectedRevision, failed: failNext });
          assert.equal(expectedRevision, revision, 'patch must use latest loaded revision');
          if (failNext) { failNext = false; throw new Error('fixture disk full'); }
          for (const [key, value] of Object.entries(patch)) { if (value === null) delete values[key]; else values[key] = structuredClone(value); }
          revision++;
          return snapshot();
        },
        subscribe: async notify => { listeners.add(notify); return () => listeners.delete(notify); },
      },
    };
  }
  const safeView = active => ({ mode: 'configured', status: active ? 'active' : 'none', lifecycle: active ? 'lifetime' : 'unlicensed', tier: active ? 'pro' : null, features: active ? ['pro'] : [], kind: active ? 'lifetime' : null, stage: active ? 'final' : null, validUntil: null, confirmationDueAt: null, staleSince: null, licenseIdSuffix: active ? '0001' : null, busy: false, unresolved: false, revisionState: active ? 'active' : 'none', message: 'none' });
  function memoryLicense() {
    let active = false, activateCalls = 0, statusCalls = 0;
    const returned = [];
    const view = () => { const value = safeView(active); assert(!JSON.stringify(value).includes(secret)); returned.push(value); return value; };
    return { returned, get activateCalls() { return activateCalls; }, get statusCalls() { return statusCalls; }, client: {
      status: async () => { statusCalls++; return view(); },
      activate: async key => { assert.equal(key, secret); activateCalls++; active = true; return view(); },
      deactivate: async () => { active = false; return view(); },
    } };
  }
  const settings = memorySettings(), license = memoryLicense();
  const custom = { id: 'preferences', label: 'Preferences', order: 0, entries: [{ id: 'theme', label: 'Theme' }], render: ctx => h(SettingsGroup, { title: 'Preferences controls' },
    h(SettingRow, { id: 'theme', label: 'Theme' }, h('output', { 'data-testid': 'theme-value' }, String(ctx.values.theme)), h('button', { type: 'button', onClick: () => void ctx.set('theme', 'dark') }, 'Save dark')),
    h(SettingRow, { id: 'volume', label: 'Volume' }, h('output', { 'data-testid': 'volume-value' }, String(ctx.values.volume)), h('button', { type: 'button', onClick: () => void ctx.patch({ volume: 75, nickname: 'saved' }) }, 'Save preferences')),
    h('output', { 'data-testid': 'other-window' }, String(ctx.values.otherWindow)),
    h('output', { 'data-testid': 'retained-value' }, JSON.stringify(ctx.values.retained))) };
  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }
  const user = userEvent.setup();
  const mounted = render(h(SettingsPanel, { backend: settings.backend, sections: [custom, licenseSection({ client: license.client, confirmDeactivate: false })], defaultActiveId: 'preferences', title: 'Consumer settings' }));
  await run('load-set-patch-subscription', async () => {
    await screen.findByTestId('theme-value');
    await waitFor(() => assert.equal(screen.getByTestId('theme-value').textContent, 'light'));
    assert.equal(screen.getByTestId('volume-value').textContent, '10');
    await waitFor(() => assert.equal(settings.listeners.size, 1));
    await user.click(screen.getByRole('button', { name: 'Save dark' }));
    await waitFor(() => assert.equal(settings.values.theme, 'dark'));
    await user.click(screen.getByRole('button', { name: 'Save preferences' }));
    await waitFor(() => assert.equal(settings.values.volume, 75));
    assert.equal(settings.values.nickname, 'saved');
    const before = settings.loads;
    await act(async () => settings.external({ otherWindow: 'remote', volume: 50 }));
    await waitFor(() => assert.equal(screen.getByTestId('other-window').textContent, 'remote'));
    await waitFor(() => assert.equal(screen.getByTestId('volume-value').textContent, '50'));
    assert(settings.loads > before, 'subscription must trigger reload');
    assert.deepEqual(settings.values.retained, { nested: 'keep' });
    return { loadCalls: settings.loads, successfulPatches: settings.patches.length, externalValue: settings.values.otherWindow };
  });
  await run('save-failure-visible-rollback-retry-retains-unrelated', async () => {
    settings.failNext();
    await user.click(screen.getByRole('button', { name: 'Save preferences' }));
    const alert = await screen.findByRole('alert');
    assert.match(alert.textContent, /fixture disk full/);
    await waitFor(() => assert.equal(screen.getByTestId('volume-value').textContent, '50'));
    assert.equal(settings.values.volume, 50);
    assert.equal(settings.values.otherWindow, 'remote');
    assert.deepEqual(settings.values.retained, { nested: 'keep' });
    await user.click(screen.getByRole('button', { name: 'Save preferences' }));
    await waitFor(() => assert.equal(settings.values.volume, 75));
    await waitFor(() => assert.equal(screen.queryByRole('alert'), null));
    assert.equal(settings.values.otherWindow, 'remote');
    assert.equal(settings.values.theme, 'dark');
    assert.deepEqual(settings.values.retained, { nested: 'keep' });
    return { state: structuredClone(settings.values), patches: structuredClone(settings.patches) };
  });
  await run('license-public-view-activation-no-token-return-or-persistence', async () => {
    await user.click(screen.getByRole('button', { name: 'Licence', exact: true }));
    const input = await screen.findByRole('textbox', { name: 'Licence key' });
    await waitFor(() => assert(license.statusCalls > 0));
    await user.type(input, secret);
    await user.click(screen.getByRole('button', { name: 'Activate', exact: true }));
    await screen.findByRole('button', { name: 'Deactivate', exact: true });
    assert.equal(license.activateCalls, 1);
    assert.match(screen.getByRole('status').textContent, /lifetime licence is active/i);
    assert.equal(screen.queryByRole('textbox', { name: 'Licence key' }), null);
    assert(!document.body.textContent.includes(secret));
    assert(!JSON.stringify(settings.values).includes(secret));
    assert(!JSON.stringify(license.returned).includes(secret));
    for (const value of license.returned) for (const key of ['token', 'rawToken', 'licenseKey', 'key']) assert(!(key in value));
    await user.click(screen.getByRole('button', { name: 'Deactivate', exact: true }));
    assert.equal((await screen.findByRole('textbox', { name: 'Licence key' })).value, '');
    return { statusCalls: license.statusCalls, activateCalls: license.activateCalls, views: license.returned, tokenReturned: false, tokenPersisted: false };
  });
  await run('unmount-unsubscribes-remount-retains-saved-state', async () => {
    mounted.unmount();
    await waitFor(() => assert.equal(settings.listeners.size, 0));
    const remount = render(h(SettingsPanel, { backend: settings.backend, sections: [custom], title: 'Reloaded settings' }));
    await waitFor(() => assert.equal(screen.getByTestId('theme-value').textContent, 'dark'));
    assert.equal(screen.getByTestId('volume-value').textContent, '75');
    assert.equal(screen.getByTestId('other-window').textContent, 'remote');
    remount.unmount();
    await waitFor(() => assert.equal(settings.listeners.size, 0));
    return { subscriptionsAfterUnmount: settings.listeners.size, persisted: structuredClone(settings.values) };
  });
  assert.equal(result.scenarios.length, 4);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
} finally {
  cleanup?.();
  dom?.window.close();
}
process.stdout.write(`${JSON.stringify(result)}\n`);
