import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const RECIPE_ID = 'qa-public-exports-pure';
const EXPECTED_VERSION = '0.6.1';
const SPECIFIERS = ['@rightkit/qa', '@rightkit/qa/chrome', '@rightkit/qa/ts-hooks', '@rightkit/qa/ts-register'];
const result = {
  recipeId: RECIPE_ID, schemaVersion: 1, ok: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [], nativeHostQualified: false,
};
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error), stack: error?.stack ?? null });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const sha256Hex = text => createHash('sha256').update(text).digest('hex');
const scenario = async (name, operation) => {
  const entry = { name, ok: false, evidence: null, error: null };
  result.scenarios.push(entry);
  try { entry.evidence = await operation(); entry.ok = true; }
  catch (error) { entry.error = failure(error); throw error; }
};
let tmp;
try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(version, EXPECTED_VERSION);
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution,
    packageName: '@rightkit/qa', rootResolvedUrl: import.meta.resolve('@rightkit/qa'),
    entries: Object.fromEntries(SPECIFIERS.map(specifier => [specifier, import.meta.resolve(specifier)])),
  });
  result.artifact = provenance.artifact;
  const root = await import(provenance.entries['@rightkit/qa']);
  const chrome = await import(provenance.entries['@rightkit/qa/chrome']);
  const hooks = await import(provenance.entries['@rightkit/qa/ts-hooks']);
  tmp = mkdtempSync(path.join(os.tmpdir(), 'rk-qa-recipe-'));

  await scenario('root-contrast-and-focus-verdicts', () => {
    const black = root.contrastReport('#save', { found: true, color: 'rgb(0, 0, 0)', chain: ['rgb(255, 255, 255)'], fontSize: 16, fontWeight: 400 });
    assert.equal(black.ratio, 21);
    assert.equal(black.passes, true);
    assert.equal(black.min, 4.5);
    const grey = root.contrastReport('#docs', { found: true, color: 'rgb(118, 118, 118)', chain: ['rgb(255, 255, 255)'], fontSize: 16, fontWeight: 400 });
    assert.equal(grey.passes, true);
    const large = root.contrastReport('#title', { found: true, color: 'rgb(118, 118, 118)', chain: ['rgb(255, 255, 255)'], fontSize: 24, fontWeight: 400 });
    assert.equal(large.large, true);
    assert.equal(large.min, 3);
    assert.throws(
      () => root.contrastReport('#hint', { found: true, color: 'rgb(200, 200, 200)', chain: ['rgb(255, 255, 255)'] }),
      error => error instanceof root.ContrastFailure && error.name === 'ContrastFailure' && error.report.passes === false && error.report.ratio < 4.5,
    );
    const parsed = root.parseCssColor('rgb(0, 0, 0)');
    assert.deepEqual([parsed.r, parsed.g, parsed.b], [0, 0, 0]);
    assert.equal(root.parseCssColor('not-a-colour'), null);
    const ring = root.focusRingEvidence({ focused: true, matches: true, focusVisible: true, tag: 'button', role: 'button', name: 'Save', outlineStyle: 'solid', outlineWidth: '2px', outlineColor: 'rgb(0, 0, 0)', outlineOffset: '0px', boxShadow: 'none' });
    assert.equal(ring.visible, true);
    assert.equal(ring.kind, 'outline');
    const bare = root.focusRingEvidence({ focused: true, outlineStyle: 'none', outlineWidth: '0px', boxShadow: 'none' });
    assert.equal(bare.visible, false);
    assert.equal(bare.kind, 'none');
    assert.equal(root.boxShadowVisible('0 0 0 3px rgb(0, 0, 0)'), true);
    assert.equal(root.boxShadowVisible('none'), false);
    return { blackRatio: black.ratio, ringKind: ring.kind, bareKind: bare.kind };
  });

  await scenario('root-ax-query-assertions', () => {
    const nodes = [
      { role: 'button', name: '  Save   changes ' },
      { role: 'link', name: 'Docs' },
      { role: 'button', name: '', disabled: true },
    ];
    assert.equal(root.assertAx(nodes, { role: 'BUTTON', name: 'Save changes' }).length, 1);
    assert.equal(root.matchAxNodes(nodes, { role: 'button', named: true }).length, 1);
    assert.equal(root.matchAxNodes(nodes, { role: 'button', disabled: true }).length, 1);
    assert.throws(() => root.assertAx(nodes, { role: 'checkbox' }), /ax: expected 1 node\(s\) matching/);
    assert.throws(() => root.assertAx(nodes, {}), TypeError);
    return { nodes: nodes.length, buttons: root.matchAxNodes(nodes, { role: 'button' }).length };
  });

  await scenario('root-evidence-hashed-receipt', () => {
    const dir = path.join(tmp, 'evidence');
    mkdirSync(dir);
    const shot = path.join(dir, 'shot.txt');
    writeFileSync(shot, 'pixels-v1');
    const bundle = root.createEvidenceBundle({ app: 'fixture-app', platform: 'fixture', runId: 'run-1', startedAt: '2026-10-20T00:00:00.000Z' });
    const artifact = root.addEvidenceArtifact(bundle, { name: 'shot', path: shot }, new Date('2026-10-20T00:00:01.000Z'));
    assert.equal(artifact.sizeBytes, 'pixels-v1'.length);
    assert.equal(artifact.sha256, sha256Hex('pixels-v1'));
    assert.throws(() => root.addEvidenceCheck(bundle, { name: 'paints', status: 'passed', artifacts: ['missing'] }), /never hashed/);
    root.addEvidenceCheck(bundle, { name: 'paints', status: 'passed', artifacts: ['shot'] });
    root.finishEvidenceBundle(bundle, { status: 'passed', finishedAt: '2026-10-20T00:00:02.000Z' });
    assert.deepEqual(root.verifyEvidenceArtifacts(bundle), []);
    const receipt = path.join(dir, 'receipt.json');
    root.writeEvidenceBundle(receipt, bundle);
    const written = JSON.parse(readFileSync(receipt, 'utf8'));
    assert.equal(written.checks[0].artifacts[0], 'shot');
    writeFileSync(shot, 'pixels-v2');
    const drift = root.verifyEvidenceArtifacts(bundle);
    assert.equal(drift.length, 1);
    assert.equal(drift[0].name, 'shot');
    assert.throws(() => root.writeEvidenceBundle(path.join(dir, 'second.json'), bundle), /evidence changed/);
    return { checks: written.checks.length, artifacts: written.artifacts.length, drift: drift.length };
  });

  await scenario('root-contract-refusal-and-installed-stage', () => {
    const bad = { cargoToml: '', rustSource: '', packageScripts: { 'build:release': 'tauri build --features qa-native' } };
    const issues = root.validateLegacyWdioQaContract(bad);
    assert(issues.includes('Cargo.toml must declare tauri-plugin-wdio-webdriver'));
    assert(issues.some(issue => issue.includes('must not enable qa-native')));
    assert.throws(() => root.assertNativeQaContract(bad), /legacy WDIO QA contract failed/);
    const good = {
      cargoToml: 'tauri-plugin-wdio-webdriver = { version = "1", optional = true }\nqa-native = ["dep:tauri-plugin-wdio-webdriver"]\n',
      rustSource: '#[cfg(all(not(debug_assertions), feature = "qa-native"))]\ncompile_error!("qa-native is debug only");\n'
        + '#[cfg(all(debug_assertions, feature = "qa-native"))]\ntauri_plugin_wdio_webdriver::init();\n// RIGHTKIT_QA_NATIVE\n',
      packageScripts: {},
    };
    const acceptedIssues = root.validateLegacyWdioQaContract(good);
    assert.deepEqual(acceptedIssues, []);
    assert.equal(root.INSTALLED_QA_STAGE_ENV, 'RIGHT_RELEASE_QA_STAGE');
    assert.equal(root.isInstalledQaMode({ RIGHT_RELEASE_QA_STAGE: ' Installed ' }), true);
    assert.equal(root.isInstalledQaMode({}), false);
    return { refusedIssues: issues.length, acceptedIssues: acceptedIssues.length };
  });

  await scenario('root-detached-record-round-trip', () => {
    const dir = path.join(tmp, 'records');
    mkdirSync(dir);
    assert.equal(root.readDetachedRecord(path.join(dir, 'absent.json')), undefined);
    const recordPath = path.join(dir, 'dev.json');
    writeFileSync(recordPath, `${JSON.stringify({
      format: 'rightkit-dev-server/1', pid: 4242, startTime: '2026-10-20T00:00:00.000Z', command: 'fixture-dev-server',
      url: 'http://127.0.0.1:5173/', port: 5173, logPath: path.join(dir, 'dev.log'), recordedAt: '2026-10-20T00:00:01.000Z',
    })}\n`);
    const record = root.readDetachedRecord(recordPath);
    assert.equal(record.port, 5173);
    assert.equal(record.format, 'rightkit-dev-server/1');
    writeFileSync(path.join(dir, 'bad.json'), '{"format":"other"}\n');
    assert.throws(() => root.readDetachedRecord(path.join(dir, 'bad.json')), /malformed/);
    return { port: record.port, format: record.format };
  });

  await scenario('chrome-subpath-is-root-reexport-with-pinned-engine', () => {
    const names = Object.keys(chrome).sort();
    assert(names.length > 0);
    for (const name of names) assert.equal(root[name], chrome[name], `root re-export ${name}`);
    assert.equal(chrome.CHROME_FOR_TESTING_ENV, 'RIGHTKIT_CHROME_FOR_TESTING');
    assert.equal(typeof chrome.launchChromeForTesting, 'function');
    assert.match(chrome.PINNED_CHROME_FOR_TESTING.version, /^\d+\.\d+\.\d+/);
    return { exports: names.length, pinnedChromeVersion: chrome.PINNED_CHROME_FOR_TESTING.version };
  });

  await scenario('ts-hooks-extensionless-and-json-resolution', async () => {
    const dir = path.join(tmp, 'hooks');
    mkdirSync(dir);
    writeFileSync(path.join(dir, 'widget.ts'), 'export const x = 1;\n');
    writeFileSync(path.join(dir, 'data.json'), '{"ok":true}\n');
    const parentURL = pathToFileURL(path.join(dir, 'parent.mjs')).href;
    const context = { parentURL, conditions: [], importAttributes: {} };
    const resolved = await hooks.resolve('./widget', context, () => { throw new Error('TypeScript candidate must short-circuit'); });
    assert.equal(resolved.shortCircuit, true);
    assert(resolved.url.endsWith('/widget.ts'), resolved.url);
    const passthrough = await hooks.resolve('./absent', context, () => ({ url: 'passthrough:absent', shortCircuit: false }));
    assert.equal(passthrough.url, 'passthrough:absent');
    const loaded = await hooks.load(pathToFileURL(path.join(dir, 'data.json')).href, {}, () => { throw new Error('JSON must short-circuit'); });
    assert.equal(loaded.format, 'json');
    assert.deepEqual(JSON.parse(loaded.source), { ok: true });
    return { resolvedSuffix: '/widget.ts', jsonFormat: loaded.format };
  });

  // Last on purpose: importing this module registers a process-wide loader hook.
  await scenario('ts-register-import-last', async () => {
    const register = await import(provenance.entries['@rightkit/qa/ts-register']);
    assert.deepEqual(Object.keys(register), []);
    return { exports: 0 };
  });

  assert.equal(result.scenarios.length, 8);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
} finally {
  if (tmp) rmSync(tmp, { recursive: true, force: true });
}
process.stdout.write(`${JSON.stringify(result)}\n`);
