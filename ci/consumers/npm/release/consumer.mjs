import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const recipeId = 'release-cli-version-smoke-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted CLI error.
let stage = 'artifact-provenance';
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert(['packed', 'fetched'].includes(resolution));
  assert.equal(version, '0.6.1');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  // @rightkit/release has no `exports` and no `main`: its only public surface is the
  // `right-release` bin (cli/right-release.mjs). Resolve that installed file directly;
  // never import the package root.
  const fixtureRoot = path.dirname(fileURLToPath(import.meta.url));
  const binPath = path.join(fixtureRoot, 'node_modules', '@rightkit', 'release', 'cli', 'right-release.mjs');
  const binUrl = pathToFileURL(binPath).href;
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/release', rootResolvedUrl: binUrl,
    entries: { '@rightkit/release/cli/right-release.mjs': binUrl },
  });
  result.artifact = provenance.artifact;
  const packageRoot = realpathSync(path.resolve(path.dirname(realpathSync(fileURLToPath(binUrl))), '..'));
  const installed = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };

  await run('installed-bin-manifest-bound-to-archive', async () => {
    assert.equal(installed.name, '@rightkit/release');
    assert.equal(installed.version, version);
    assert.equal('exports' in installed, false);
    assert.deepEqual(installed.bin, { 'right-release': 'cli/right-release.mjs', 'rightkit-gate': 'bin/rightkit-gate.mjs' });
    assert.ok(readFileSync(path.join(packageRoot, 'bin', 'rightkit-gate.mjs'), 'utf8').startsWith('#!/usr/bin/env node'));
    assert.ok(readFileSync(path.join(packageRoot, 'cli', 'right-release.mjs'), 'utf8').startsWith('#!/usr/bin/env node'));
    return { bins: ['right-release', 'rightkit-gate'], binTargets: ['cli/right-release.mjs', 'bin/rightkit-gate.mjs'], exportsPresent: false };
  });

  await run('installed-cli-version-smoke', async () => {
    const smoke = spawnSync(process.execPath, [binPath, '--version'], { cwd: fixtureRoot, encoding: 'utf8', timeout: 120_000, windowsHide: true });
    assert.equal(smoke.status, 0);
    assert.equal(String(smoke.stdout ?? '').trim(), version);
    return { command: 'right-release --version', exitCode: 0, stdout: version };
  });

  await run('smol-toml-closure-installed-and-loads', async () => {
    assert.deepEqual(installed.dependencies, { 'smol-toml': '1.9.1' });
    const releaseRequire = createRequire(pathToFileURL(path.join(packageRoot, 'package.json')));
    // smol-toml exports no ./package.json subpath: read its manifest from the resolved entry instead.
    const tomlRoot = path.resolve(path.dirname(releaseRequire.resolve('smol-toml')), '..');
    const tomlManifest = JSON.parse(readFileSync(path.join(tomlRoot, 'package.json'), 'utf8'));
    assert.equal(tomlManifest.name, 'smol-toml');
    assert.equal(tomlManifest.version, '1.9.1');
    const toml = releaseRequire('smol-toml');
    const parsed = toml.parse('name = "fixture"\ncount = 1\n');
    assert.equal(parsed.name, 'fixture');
    assert.equal(parsed.count, 1);
    return { dependency: 'smol-toml', dependencyVersion: tomlManifest.version, parsedFixture: true };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
