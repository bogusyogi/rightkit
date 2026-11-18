import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'dictation-ui-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./onboarding", "./package.json", "./pill", "./settings"];
const expected = {
  '.': ['configureDictationHost', 'deliveryRow', 'deliveryFilterKey', 'deliveryQueryArgs', 'useDictationEvent', 'useMicDevices', 'useSettings'],
  './pill': ['configureDictationHost', 'usePillEvents', 'usePillAnimation', 'usePillGeometry'],
  './onboarding': ['useOnboardingRecording', 'useOnboardingRecognitionSetup'],
  './settings': [],
};

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/dictation-ui');
  const entries = Object.fromEntries(Object.keys(expected).map(key => [key, import.meta.resolve(`@rightkit/dictation-ui${key === '.' ? '' : key.slice(1)}`)]));
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/dictation-ui', rootResolvedUrl: root, entries });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('root-export-resolves-and-exposes-reviewed-names', async () => {
    assert.equal(manifest.name, '@rightkit/dictation-ui');
    assert.deepEqual(Object.keys(manifest.exports ?? {}).sort(), surface);
    const modules = Object.fromEntries(await Promise.all(Object.entries(provenance.entries).map(async ([key, url]) => [key, await import(url)])));
    for (const [entry, names] of Object.entries(expected)) for (const key of names) assert.ok(key in modules[entry], `missing ${entry} export ${key}`);
    // The delivery mapper preserves raw process name for host-side filtering.
    const row = modules['.'].deliveryRow({ delivery_id: 'd1', transcript: 't', delivered_at_ms: 1, outcome: { kind: 'pasted' }, target: { process_name: 'chrome.exe', window_title: 'Docs' } });
    assert.equal(row.origin.filterKey, 'chrome.exe');
    assert.equal(modules['.'].deliveryQueryArgs({ search: '', fromMs: null, untilMs: null, origin: row.origin.filterKey, variant: 'dictations', offset: 0, limit: 50 }).app, 'chrome.exe');
    return { checked: expected, exportKeys: surface.length, filterKey: row.origin.filterKey };
  });

  assert.equal(result.scenarios.length, 1);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
