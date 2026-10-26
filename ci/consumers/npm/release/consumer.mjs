import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync, statSync } from 'node:fs';
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
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  // @rightkit/release has no `main` and no index export: its surface is the two bins plus the explicit `exports` map
  // (one "./<file>" entry per supported subpath). Resolve the installed bin file directly; never import the package root.
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
    assert.deepEqual(installed.bin, { 'right-release': 'cli/right-release.mjs', 'rightkit-gate': 'bin/rightkit-gate.mjs', 'rightkit-agent-hook': 'bin/rightkit-agent-hook.mjs' });
    assert.ok(readFileSync(path.join(packageRoot, 'bin', 'rightkit-gate.mjs'), 'utf8').startsWith('#!/usr/bin/env node'));
    assert.ok(readFileSync(path.join(packageRoot, 'bin', 'rightkit-agent-hook.mjs'), 'utf8').startsWith('#!/usr/bin/env node'));
    assert.ok(readFileSync(path.join(packageRoot, 'cli', 'right-release.mjs'), 'utf8').startsWith('#!/usr/bin/env node'));
    return { bins: ['right-release', 'rightkit-gate', 'rightkit-agent-hook'], binTargets: ['cli/right-release.mjs', 'bin/rightkit-gate.mjs', 'bin/rightkit-agent-hook.mjs'], exportsPresent: true };
  });

  await run('installed-exports-map-explicit-and-enforced', async () => {
    // The map is explicit: "./<file>" keys mapped to themselves, no index, no wildcard; every bin target and package.json are listed.
    const map = installed.exports;
    assert.ok(map && typeof map === 'object' && !Array.isArray(map));
    const keys = Object.keys(map);
    assert.ok(keys.length >= 20);
    assert.equal(Object.hasOwn(map, '.'), false);
    for (const key of keys) {
      assert.match(key, /^\.\/[A-Za-z0-9_][A-Za-z0-9_./-]*$/);
      assert.equal(map[key], key);
      assert.ok(statSync(path.join(packageRoot, key.slice(2))).isFile());
    }
    for (const target of Object.values(installed.bin)) assert.ok(keys.includes(`./${target}`));
    assert.ok(keys.includes('./package.json'));
    // Node resolves a listed subpath into the installed package and refuses the internals and the package root.
    const inside = specifier => { const file = realpathSync(fileURLToPath(import.meta.resolve(specifier))); assert.ok(file.startsWith(`${packageRoot}${path.sep}`)); return file; };
    assert.ok(inside('@rightkit/release/cargo-target.mjs').endsWith('cargo-target.mjs'));
    assert.ok(inside('@rightkit/release/cli/right-release.mjs').endsWith('right-release.mjs'));
    const refused = [];
    for (const specifier of ['@rightkit/release', '@rightkit/release/release.mjs', '@rightkit/release/build-release.mjs', '@rightkit/release/sign-macos.mjs', '@rightkit/release/ownership.mjs']) {
      assert.throws(() => import.meta.resolve(specifier), error => error.code === 'ERR_PACKAGE_PATH_NOT_EXPORTED');
      refused.push(specifier);
    }
    const cargoTarget = await import('@rightkit/release/cargo-target.mjs');
    assert.equal(typeof cargoTarget.resolveTargetRoot, 'function');
    return { exportedSubpaths: keys.length, resolved: ['@rightkit/release/cargo-target.mjs', '@rightkit/release/cli/right-release.mjs'], refused, importedFunction: 'resolveTargetRoot' };
  });

  await run('installed-cli-version-smoke', async () => {
    const smoke = spawnSync(process.execPath, [binPath, '--version'], { cwd: fixtureRoot, encoding: 'utf8', timeout: 120_000, windowsHide: true });
    assert.equal(smoke.status, 0);
    assert.equal(String(smoke.stdout ?? '').trim(), version);
    return { command: 'right-release --version', exitCode: 0, stdout: version };
  });

  await run('installed-update-command-runs-pinned-updater', async () => {
    // `right-release update` runs the right-update bin of the @rightkit/outdated this package pins. The pin is an exact version
    // that is also the consumer's reviewed closure pin; pnpm links no bin of it at the app root, which is why the command exists.
    const consumerManifest = JSON.parse(readFileSync(path.join(fixtureRoot, 'package.json'), 'utf8'));
    const pin = installed.dependencies?.['@rightkit/outdated'];
    assert.match(pin ?? '', /^\d+\.\d+\.\d+$/);
    assert.equal(pin, consumerManifest.dependencies['@rightkit/outdated']);
    const smoke = spawnSync(process.execPath, [binPath, 'update', '--version'], { cwd: fixtureRoot, encoding: 'utf8', timeout: 120_000, windowsHide: true });
    assert.equal(smoke.status, 0);
    assert.equal(String(smoke.stdout ?? '').trim(), pin);
    return { command: 'right-release update --version', exitCode: 0, stdout: pin };
  });

  await run('smol-toml-closure-installed-and-loads', async () => {
    assert.deepEqual(Object.keys(installed.dependencies).sort(), ['@rightkit/outdated', 'smol-toml']);
    assert.equal(installed.dependencies['smol-toml'], '1.9.1');
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
