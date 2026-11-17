import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'mail-ui-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./components/AccountSwitcher", "./components/Icons", "./compose-settings", "./hooks/dialog", "./lib/ipc", "./lib/ipc/agentApprovals", "./lib/mail", "./lib/preferences", "./lib/visibleText", "./package.json", "./sidebar", "./storage-settings", "./store/index", "./store/types", "./styles.css"];
const expected = ["setHostAppName", "hostAppName", "AccountSwitcher", "mailWorkspaceHost", "mailWorkspaceClient", "MailWorkspaceProvider", "CalendarView", "TasksView", "DriveView", "ContactsView", "MeetView", "normalizeAgentRow", "normalizeAgentAuditRow"];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/mail-ui');
  const sidebar = import.meta.resolve('@rightkit/mail-ui/sidebar');
  const composeSettings = import.meta.resolve('@rightkit/mail-ui/compose-settings');
  const storageSettings = import.meta.resolve('@rightkit/mail-ui/storage-settings');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/mail-ui', rootResolvedUrl: root, entries: { '@rightkit/mail-ui': root, '@rightkit/mail-ui/sidebar': sidebar, '@rightkit/mail-ui/compose-settings': composeSettings, '@rightkit/mail-ui/storage-settings': storageSettings } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('root-export-resolves-and-exposes-reviewed-names', async () => {
    assert.equal(manifest.name, '@rightkit/mail-ui');
    assert.deepEqual(Object.keys(manifest.exports ?? {}).sort(), surface);
    const mod = await import(provenance.entries['@rightkit/mail-ui']);
    const sidebarMod = await import(provenance.entries['@rightkit/mail-ui/sidebar']);
    const composeSettingsMod = await import(provenance.entries['@rightkit/mail-ui/compose-settings']);
    assert.equal(typeof composeSettingsMod.useComposeSettingsModel, 'function');
    const storageSettingsMod = await import(provenance.entries['@rightkit/mail-ui/storage-settings']);
    assert.equal(typeof storageSettingsMod.useStorageSettingsModel, 'function');
    for (const key of ['useSidebarModel', 'buildLabelTree', 'sidebarViewQueries', 'selectSidebarFolder']) assert.equal(typeof sidebarMod[key], 'function', `missing sidebar export ${key}`);
    assert.equal(sidebarMod.useSidebarModel, mod.useSidebarModel);
    assert.deepEqual(sidebarMod.sidebarViewQueries(new Date(2026, 0, 2)), { unread: 'is:unread', attachments: 'has:attachment', today: 'after:2026/01/02' });
    const folderSteps = [];
    sidebarMod.selectSidebarFolder('drafts', {
      setFolder: value => folderSteps.push(['folder', value]),
      setActiveLabel: value => folderSteps.push(['label', value]),
      setCategory: value => folderSteps.push(['category', value]),
      setView: value => folderSteps.push(['view', value]),
      setOpenThread: value => folderSteps.push(['thread', value]),
    });
    assert.deepEqual(folderSteps, [['folder', 'drafts'], ['label', null], ['category', null], ['view', 'inbox'], ['thread', null]]);
    for (const key of expected) assert.ok(key in mod, `missing root export ${key}`);
    // The workspace views and navs left mail-ui for @rightkit/workspace-ui; mail-ui supplies their host.
    for (const gone of ["useDrive", "CalendarNav", "DriveNav", "rangeFor"]) assert.ok(!(gone in mod), `${gone} must come from @rightkit/workspace-ui`);
    assert.equal(typeof mod.mailWorkspaceClient.listCalendars, 'function');
    assert.equal(typeof mod.mailWorkspaceHost.useState, 'function');
    // The workspace views mail-ui exports are ready to mount: they carry their own provider over mailWorkspaceHost,
    // where workspace-ui's bare views throw without one.
    const { createElement } = await import('react');
    const { renderToString } = await import('react-dom/server');
    const workspace = await import('@rightkit/workspace-ui');
    let seen = null;
    function Probe() { seen = workspace.useWorkspaceHost(); return null; }
    renderToString(createElement(mod.MailWorkspaceProvider, null, createElement(Probe)));
    assert.equal(seen, mod.mailWorkspaceHost);
    // A ready view gets past the provider (it may still need the app's own startup, e.g. preferences, to render).
    for (const view of ['CalendarView', 'TasksView', 'DriveView', 'ContactsView', 'MeetView']) {
      try { renderToString(createElement(mod[view])); }
      catch (error) { assert.notEqual(error.message, workspace.MISSING_WORKSPACE_HOST_PROVIDER, `${view} has no provider`); }
    }
    // The agent wire: gateway 0.3.0 says scopes/scope, older gateways accounts/account_id; both normalize alike.
    const agent = { id: 'a', name: 'A', profile: 'read_only', created_at: 1, revoked_at: null };
    for (const row of [mod.normalizeAgentRow({ ...agent, scopes: ['me@example.com'] }), mod.normalizeAgentRow({ ...agent, accounts: ['me@example.com'] })]) {
      assert.deepEqual(row.scopes, ['me@example.com']);
      assert.deepEqual(row.accounts, ['me@example.com']);
    }
    const audit = { id: 1, at: 1, agent_name: 'A', method: 'search', outcome: 'ok', detail: null };
    for (const row of [mod.normalizeAgentAuditRow({ ...audit, scope: 'me@example.com' }), mod.normalizeAgentAuditRow({ ...audit, account_id: 'me@example.com' })]) {
      assert.equal(row.scope, 'me@example.com');
      assert.equal(row.account_id, 'me@example.com');
    }
    const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    const values = new Map([['fixture.mode', 'custom-light'], ['fixture.font', 'custom-font']]);
    Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: {
      getItem: key => values.get(key) ?? null,
      setItem: (key, value) => {
        if (key === 'fixture.preset') throw new Error('preset write refused');
        values.set(key, value);
      },
    } });
    try {
      await mod.initPreferences({ load: async () => ({ revision: 0, values: {} }) });
      const { themeStore, bootTheme } = mod.configureMailAppearance({
        presets: [{ id: 'base-dark', mode: 'dark', tokens: {} }, { id: 'custom-light', mode: 'light', tokens: {} }],
        fonts: [{ id: 'base-font', stack: 'sans-serif' }, { id: 'custom-font', stack: 'serif' }],
        defaults: { dark: 'base-dark', light: 'custom-light', font: 'base-font' },
        storageKeys: { mode: 'fixture.mode', preset: 'fixture.preset', font: 'fixture.font' },
      });
      try {
        bootTheme();
        assert.equal(themeStore.read(), 'light');
        assert.equal(mod.useAppStore.getState().themeChoice, 'custom-light');
        assert.equal(mod.useAppStore.getState().fontChoice, 'custom-font');
        assert.equal(values.get('fixture.mode'), 'custom-light', 'failed migration must preserve the legacy value');
        themeStore.save('dark');
        assert.equal(themeStore.read(), 'dark');
        assert.equal(mod.useAppStore.getState().themeChoice, 'base-dark');
        mod.useAppStore.getState().pickTheme('auto');
        assert.equal(themeStore.read(), 'system');
        assert.equal(mod.useAppStore.getState().themeChoice, 'auto');
        assert.equal(values.get('fixture.mode'), 'custom-light', 'failed saves must leave the prior persisted choice intact');
      } finally {
        themeStore.dispose();
      }
    } finally {
      if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
      else delete globalThis.localStorage;
    }
    return { checked: expected, exportKeys: surface.length, providerSuppliesHost: true, agentWires: 2, hostAppearanceChoices: true };
  });

  assert.equal(result.scenarios.length, 1);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
