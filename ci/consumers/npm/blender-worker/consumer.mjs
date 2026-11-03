import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'blender-worker-export-surface-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only; no untrusted message is serialized.
let stage = 'artifact-provenance';
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/blender-worker');
  const manifestUrl = import.meta.resolve('@rightkit/blender-worker/package.json');
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/blender-worker', rootResolvedUrl: root,
    entries: { '@rightkit/blender-worker': root, '@rightkit/blender-worker/package.json': manifestUrl },
  });
  result.artifact = provenance.artifact;
  const worker = await import(provenance.entries['@rightkit/blender-worker']);
  const installed = JSON.parse(readFileSync(fileURLToPath(provenance.entries['@rightkit/blender-worker/package.json']), 'utf8'));
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };

  await run('installed-manifest-and-exact-export-surface', async () => {
    assert.equal(installed.name, '@rightkit/blender-worker');
    assert.equal(installed.version, version);
    assert.equal(installed.type, 'module');
    assert.deepEqual(Object.keys(installed.exports).sort(), ['./package.json', '.']);
    assert.deepEqual(installed.exports['./package.json'], './package.json');
    for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies']) assert.equal(installed[field], undefined);
    assert.deepEqual(Object.keys(worker).sort(), ['workerDir']);
    return { exports: ['.', './package.json'], dependencies: 0, moduleExports: 1 };
  });

  await run('shipped-worker-source-present', async () => {
    const dir = worker.workerDir;
    assert.equal(typeof dir, 'string');
    assert.ok(path.isAbsolute(dir) && path.basename(dir) === 'worker', 'workerDir is the absolute worker directory');
    const real = realpathSync(dir);
    assert.ok(statSync(real).isDirectory(), 'workerDir is a directory');
    assert.ok(existsSync(path.join(real, 'render_job.py')), 'render_job.py ships with the worker source');
    assert.ok(existsSync(path.join(real, 'worker-manifest.json')), 'worker-manifest.json ships with the worker source');
    return { workerSource: true, renderJob: true, manifest: true };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
