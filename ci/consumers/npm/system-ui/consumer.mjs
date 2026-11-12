import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'system-ui-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./activityLease", "./api", "./components/Chart", "./host", "./metrics", "./package.json", "./styles.css", "./tauri", "./views/Storage", "./views/monitor.css", "./visible"];
const expected = ["useActivityLease", "isWindows", "api", "appsApi", "configureSystemHost", "systemHost", "Storage", "Cleanup", "Apps", "Activity", "Duplicates", "visibleInterval", "bytes"];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/system-ui');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/system-ui', rootResolvedUrl: root, entries: { '@rightkit/system-ui': root } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('root-export-resolves-and-exposes-reviewed-names', async () => {
    assert.equal(manifest.name, '@rightkit/system-ui');
    assert.deepEqual(Object.keys(manifest.exports ?? {}).sort(), surface);
    const mod = await import(provenance.entries['@rightkit/system-ui']);
    for (const key of expected) assert.ok(key in mod, `missing root export ${key}`);
    // No transport of its own: before the host is configured a call refuses; afterwards it reaches the host's client.
    for (const gone of ["Chat", "NearbyGroup", "LauncherLists", "devicesApi", "shareApi", "qrMatrix"]) assert.ok(!(gone in mod), `${gone} is not system UI`);
    assert.throws(() => mod.api.status(), /configureSystemHost/);
    const seen = [];
    const handler = { get: (_t, key) => async (...args) => { seen.push([key, args]); return key === 'status' ? { ok: true } : null; } };
    mod.configureSystemHost({ client: new Proxy({}, handler), apps: new Proxy({}, handler), subscribe: async () => () => {}, appName: 'Consumer', storagePrefix: 'consumer.' });
    assert.deepEqual(await mod.api.status(), { ok: true });
    await mod.appsApi.initialApp();
    assert.deepEqual(seen.map(([k]) => k), ['status', 'initialApp']);
    assert.equal(mod.systemHost().appName, 'Consumer');
    return { checked: expected, exportKeys: surface.length };
  });

  assert.equal(result.scenarios.length, 1);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
