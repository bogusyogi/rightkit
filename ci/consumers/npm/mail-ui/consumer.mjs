import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'mail-ui-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./components/AccountSwitcher", "./components/Icons", "./hooks/dialog", "./lib/ipc", "./lib/ipc/agentApprovals", "./lib/mail", "./lib/preferences", "./lib/visibleText", "./package.json", "./store/index", "./store/types", "./styles.css"];
const expected = ["setHostAppName", "hostAppName", "AccountSwitcher", "mailWorkspaceHost", "mailWorkspaceClient", "MailWorkspaceProvider", "CalendarView", "TasksView", "DriveView", "ContactsView", "MeetView"];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/mail-ui');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/mail-ui', rootResolvedUrl: root, entries: { '@rightkit/mail-ui': root } });
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
    return { checked: expected, exportKeys: surface.length, providerSuppliesHost: true };
  });

  assert.equal(result.scenarios.length, 1);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
