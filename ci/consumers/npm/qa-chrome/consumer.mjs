import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const result = {
  recipeId: 'qa-chrome-launch-close', schemaVersion: 1, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, browser: null, scenarios: [], errors: [],
};
const errorRecord = (error) => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error), code: error?.code ?? null });
const required = (name) => {
  const value = process.env[name];
  assert(value, `${name} is required`);
  return value;
};
let api;

async function assertClean(browser, report, beforeClones) {
  assert.equal(report.exited, true);
  assert.equal(report.profileRemoved, true);
  assert.equal(report.profileRemoval, 'removed');
  assert.equal(report.livenessUnknown, undefined);
  assert.deepEqual(report.leftoverPids, []);
  assert(!existsSync(browser.profileDir), 'profile directory remains');
  // null means unknown, never clean. No saved PID/PGID is probed or signalled.
  assert.deepEqual(api.probeProfileProcesses(browser.profileDir), [], 'profile process scan must succeed & be empty');
  assert.deepEqual(api.codeSignCloneEntries().filter(entry => !beforeClones.has(entry)), []);
}

async function scenario(name, leaveProfile) {
  const entry = { name, ok: false, close: null, recovery: null, error: null, cleanupError: null };
  result.scenarios.push(entry);
  const beforeClones = new Set(api.codeSignCloneEntries());
  let browser;
  try {
    browser = await api.launchChromeForTesting({ executablePath: result.browser.executablePath, profilePrefix: 'rk-r2-qa-chrome-' });
    assert(existsSync(browser.profileDir));
    const initial = api.probeProfileProcesses(browser.profileDir);
    assert(initial !== null && initial.length > 0, 'live profile ownership scan must succeed');
    const response = await fetch(`http://127.0.0.1:${browser.port}/json/version`, { signal: AbortSignal.timeout(10_000) });
    assert(response.ok, 'DevTools version endpoint failed');
    const version = await response.json();
    assert.equal(version.Browser.split('/').at(-1), api.PINNED_CHROME_FOR_TESTING.version, 'running Chrome must match package pin');
    assert.equal(version.webSocketDebuggerUrl, browser.wsEndpoint);
    entry.close = await browser.close(leaveProfile ? { profileRemoveMs: 0 } : {});
    // Public close caches its promise/report, including a report showing incomplete cleanup.
    assert.equal(await browser.close(), entry.close);
    if (leaveProfile) {
      assert.equal(entry.close.exited, true);
      assert.equal(entry.close.profileRemoved, false);
      assert.equal(entry.close.profileRemoval, 'not-attempted');
      assert.equal(entry.close.livenessUnknown, undefined);
      assert.deepEqual(entry.close.leftoverPids, []);
      assert(existsSync(browser.profileDir), 'zero removal phase must leave profile visible');
      // Explicit fresh close owns its original ChildProcess & retries profile cleanup safely.
      entry.recovery = await api.closeChromeGracefully({ child: browser.process, wsEndpoint: browser.wsEndpoint, profileDir: browser.profileDir });
      await assertClean(browser, entry.recovery, beforeClones);
    } else {
      await assertClean(browser, entry.close, beforeClones);
    }
    entry.ok = true;
  } catch (error) {
    entry.error = errorRecord(error);
    throw error;
  } finally {
    if (browser && !entry.ok) {
      try {
        entry.recovery = await api.closeChromeGracefully({ child: browser.process, wsEndpoint: browser.wsEndpoint, profileDir: browser.profileDir });
        await assertClean(browser, entry.recovery, beforeClones);
      } catch (error) {
        entry.cleanupError = errorRecord(error);
        result.errors.push({ phase: `${name}:cleanup`, ...entry.cleanupError });
      }
    }
  }
}

try {
  const artifactPath = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(artifactPath)).digest('hex'), sha256);
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive: artifactPath, sha256, version, resolution, packageName: '@rightkit/qa-chrome', rootResolvedUrl: import.meta.resolve('@rightkit/qa-chrome'), entries: { '@rightkit/qa-chrome': import.meta.resolve('@rightkit/qa-chrome') } });
  result.artifact = provenance.artifact;
  api = await import(provenance.entries['@rightkit/qa-chrome']);
  const suppliedRoot = required('RIGHTKIT_CHROME_FOR_TESTING_ROOT');
  assert(path.isAbsolute(suppliedRoot), 'managed CfT root must be absolute');
  const managedRoot = realpathSync(suppliedRoot);
  const managedEnv = { RIGHTKIT_CHROME_FOR_TESTING_ROOT: managedRoot };
  const current = realpathSync(api.managedCurrentDir({ env: managedEnv }));
  const platformKey = api.chromeForTestingPlatform();
  const marker = JSON.parse(readFileSync(path.join(current, 'install.json'), 'utf8'));
  const pin = api.PINNED_CHROME_FOR_TESTING;
  const files = pin.downloads.filter(item => item.platform === platformKey).map(item => ({ kind: item.kind ?? 'chrome', url: item.url, sha256: item.sha256 }));
  assert(files.some(item => item.kind === 'chrome'));
  assert(files.every(item => /^[a-f0-9]{64}$/.test(item.sha256)), 'host archive digest must be pinned');
  assert.deepEqual(marker, { version: pin.version, platform: platformKey, files }, 'managed install marker must match package pin');
  const executablePath = realpathSync(api.managedExecutable(current, platformKey));
  const executableRelative = path.relative(current, executablePath);
  assert(executableRelative && !path.isAbsolute(executableRelative) && executableRelative !== '..' && !executableRelative.startsWith(`..${path.sep}`), 'managed executable escapes current install');
  result.browser = { managedRoot, executablePath, platformKey, pinnedVersion: pin.version, installMarker: marker };
  await scenario('normal-close', false);
  await scenario('incomplete-cleanup-visible-then-recovered', true);
  result.ok = true;
} catch (error) {
  result.errors.push({ phase: 'qualification', ...errorRecord(error) });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
