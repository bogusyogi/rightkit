import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const recipeId = 'issues-tracker-cli-and-module-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted CLI or module error.
let stage = 'artifact-provenance';
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert(['packed', 'fetched'].includes(resolution));
  assert.equal(version, '0.1.0');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  // @rightkit/issues-tracker has no `exports` and no `main`: its surface is the `right-issues` bin
  // (cli/right-issues.mjs) and the shipped tracker.mjs module. Resolve both installed files directly;
  // never import the package root.
  const fixtureRoot = path.dirname(fileURLToPath(import.meta.url));
  const packageDir = path.join(fixtureRoot, 'node_modules', '@rightkit', 'issues-tracker');
  const binPath = path.join(packageDir, 'cli', 'right-issues.mjs');
  const binUrl = pathToFileURL(binPath).href;
  const trackerUrl = pathToFileURL(path.join(packageDir, 'tracker.mjs')).href;
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/issues-tracker', rootResolvedUrl: binUrl,
    entries: { '@rightkit/issues-tracker/cli/right-issues.mjs': binUrl, '@rightkit/issues-tracker/tracker.mjs': trackerUrl },
  });
  result.artifact = provenance.artifact;
  const packageRoot = realpathSync(path.resolve(path.dirname(realpathSync(fileURLToPath(binUrl))), '..'));
  const installed = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };

  await run('installed-bin-manifest-bound-to-archive', async () => {
    assert.equal(installed.name, '@rightkit/issues-tracker');
    assert.equal(installed.version, version);
    assert.equal('exports' in installed, false);
    assert.equal('main' in installed, false);
    assert.deepEqual(installed.bin, { 'right-issues': 'cli/right-issues.mjs' });
    assert.equal(installed.dependencies, undefined);
    assert.ok(readFileSync(path.join(packageRoot, 'cli', 'right-issues.mjs'), 'utf8').startsWith('#!/usr/bin/env node'));
    return { bin: 'right-issues', binTarget: 'cli/right-issues.mjs', exportsPresent: false, dependencies: 0 };
  });

  await run('installed-cli-version-smoke', async () => {
    const smoke = spawnSync(process.execPath, [binPath, '--version'], { cwd: fixtureRoot, encoding: 'utf8', timeout: 120_000, windowsHide: true });
    assert.equal(smoke.status, 0);
    assert.equal(String(smoke.stdout ?? '').trim(), version);
    return { command: 'right-issues --version', exitCode: 0, stdout: version };
  });

  await run('tracker-module-renders-escaped-self-contained-page', async () => {
    const tracker = await import(provenance.entries['@rightkit/issues-tracker/tracker.mjs']);
    const now = new Date('2026-10-21T12:00:00Z');
    const issues = [
      { number: 1, title: '</script>', state: 'OPEN', labels: ['needs-decision'], updatedAt: '2026-10-20T12:00:00Z', closedAt: null, url: 'https://github.com/o/r/issues/1', body: '' },
      { number: 2, title: 'Closed one', state: 'CLOSED', labels: [], updatedAt: '2026-10-20T12:00:00Z', closedAt: '2026-10-20T12:00:00Z', url: 'https://github.com/o/r/issues/2', body: '' },
    ];
    assert.equal(tracker.statusOf(issues[0]), 'decision');
    assert.equal(tracker.statusOf(issues[1]), 'done');
    const model = tracker.buildModel({ repo: 'o/r<b>', now, issues });
    const html = tracker.renderHtml(model, { who: 'Adrian' });
    assert.match(html, /<title>r&lt;b&gt; issues<\/title>/);
    assert.equal((html.match(/<\/script>/g) || []).length, 1);
    assert.ok(!/(src|href)=["']https?:/i.test(html.replace(/https:\/\/github\.com\/o\/r\/issues\/\d/g, '')));
    assert.equal(tracker.renderHtml(tracker.buildModel({ repo: 'o/r<b>', now, issues }), { who: 'Adrian' }), html);
    return { issues: issues.length, titleEscaped: true, scriptTagsClosed: 1, externalResources: false, deterministic: true };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
