import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { consumeNpm } from './consume.mjs';
import { validateVectors, validateSurface, validateContext, validateManifestPins, validateRuntime, pnpmInvocation, spawnPnpm, npmConsumerQualificationProblems, RECIPE_FILES, SOURCE_FILES } from './npm-consumer.mjs';
import { sha } from './upload.mjs';
const vectors = JSON.parse(fs.readFileSync(new URL('../../crates/rightkit-logs/tests/scrub-parity.v1.json', import.meta.url)));
const pkg = JSON.parse(fs.readFileSync(new URL('../../packages/logs/package.json', import.meta.url)));
const readme = fs.readFileSync(new URL('../../packages/logs/README.md', import.meta.url), 'utf8');
test('exact named surface rejects new exports & dependency closure', () => {
  assert.equal(validateSurface(pkg, readme), 9);
  assert.throws(() => validateSurface({ ...pkg, exports: { ...pkg.exports, './new': './new.js' } }, readme));
  assert.throws(() => validateSurface({ ...pkg, dependencies: { other: 'workspace:*' } }, readme));
  assert.throws(() => validateSurface(pkg, 'missing entry table'));
  assert.throws(() => validateSurface(pkg, readme.replace('; types ', '; `unexpectedAPI()`; types ')));
});
test('parity requires every named row, known schema & unambiguous strings', () => {
  assert.equal(validateVectors(vectors), 37);
  assert.throws(() => validateVectors({ ...vectors, schema: 2 }));
  assert.throws(() => validateVectors({ ...vectors, vectors: vectors.vectors.slice(1) }));
  const changed = structuredClone(vectors); changed.vectors[0].inputParts = ['other'];
  assert.throws(() => validateVectors(changed));
  const duplicate = structuredClone(vectors); duplicate.vectors[1].name = duplicate.vectors[0].name;
  assert.throws(() => validateVectors(duplicate));
});
test('native CI identity & credential rejection happen before execution', () => {
  const env = { GITHUB_ACTIONS: 'true', GITHUB_SHA: 'a'.repeat(40), GITHUB_RUN_ID: '1', GITHUB_RUN_ATTEMPT: '1', RUNNER_OS: 'Windows' };
  assert.doesNotThrow(() => validateContext(env, 'win32'));
  assert.throws(() => validateContext(env, 'linux'));
  assert.throws(() => validateContext({ ...env, RUNNER_OS: 'Linux' }, 'linux'), /native Windows\/macOS runner required/, 'PK11 logs never runs on Linux');
  assert.doesNotThrow(() => validateContext({ ...env, RUNNER_OS: 'Linux' }, 'linux', { linux: true }));
  assert.throws(() => validateContext({ ...env, RUNNER_OS: 'macOS' }, 'linux', { linux: true }));
  assert.throws(() => validateContext({ ...env, RUNNER_OS: 'Linux' }, 'darwin', { linux: true }));
  assert.throws(() => validateContext({ ...env, RUNNER_OS: 'Linux' }, 'freebsd', { linux: true }));
  assert.throws(() => validateContext({ ...env, GITHUB_ACTIONS: 'false' }, 'win32'));
  assert.throws(() => validateContext({ ...env, ACTIONS_ID_TOKEN_REQUEST_TOKEN: 'present' }, 'win32'));
  assert.throws(() => validateContext({ ...env, PGPASSWORD: 'fixture-only' }, 'win32'));
  assert.throws(() => validateContext({ ...env, GITHUB_SHA: '' }, 'win32'));
});
test('named caller isolates ambient credentials & preserves native identity and OIDC refusal', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-npm-env-test-'));
  const unit = { name: '@rightkit/logs', version: pkg.version, file: 'logs.tgz', sha256: 'a'.repeat(64), size: 100 };
  const env = { GITHUB_ACTIONS: 'true', GITHUB_SHA: 'a'.repeat(40), GITHUB_RUN_ID: '1', GITHUB_RUN_ATTEMPT: '1', RUNNER_OS: 'Windows', Path: 'fixture-path', SystemRoot: 'C:\\Windows', PGPASSWORD: 'fixture-only', NODE_OPTIONS: '--require=untrusted', npm_config_userconfig: 'untrusted' };
  try {
    for (const oidc of [false, true]) {
      let observed;
      consumeNpm({ units: [unit], packDir: scratchRoot, scratchRoot, env: { ...env, ...(oidc ? { ACTIONS_ID_TOKEN_REQUEST_TOKEN: 'fixture-only' } : {}) }, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 1, stderr: 'fixture portability failure' }), npmConsumer: options => {
        observed = options.env;
        return { ...unit, archiveSha256: unit.sha256, archiveSize: unit.size, qualification: 'R2a', verified: true };
      } });
      assert.ok(observed);
      if (oidc) assert.throws(() => validateContext(observed, 'win32'));
      else assert.doesNotThrow(() => validateContext(observed, 'win32'));
      for (const key of ['PGPASSWORD', 'NODE_OPTIONS', 'npm_config_userconfig']) assert.equal(Object.hasOwn(observed, key), false);
      for (const key of ['GITHUB_ACTIONS', 'GITHUB_SHA', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'RUNNER_OS', 'Path', 'SystemRoot']) assert.equal(observed[key], env[key]);
    }
    assert.equal(env.PGPASSWORD, 'fixture-only');
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});
test('packed metadata may omit pnpm pin while source, recipe & explicit packed pins stay strict', () => {
  const root = { packageManager: pkg.packageManager }, consumer = { packageManager: pkg.packageManager };
  const packed = { ...pkg }; delete packed.packageManager;
  assert.equal(validateManifestPins(root, pkg, consumer, packed, pkg.name, pkg.version), pkg.packageManager);
  for (const mutation of [
    [root, { ...pkg, packageManager: 'pnpm@1.0.0' }, consumer, packed],
    [root, pkg, { packageManager: 'pnpm@1.0.0' }, packed],
    [root, pkg, consumer, { ...packed, packageManager: 'pnpm@1.0.0' }],
    [root, pkg, consumer, { ...packed, packageManager: null }],
    [root, { ...pkg, version: '0.0.0' }, consumer, packed],
    [root, pkg, consumer, { ...packed, version: '0.0.0' }],
  ]) assert.throws(() => validateManifestPins(...mutation, pkg.name, pkg.version));
});
test('runtime receipt requires positive exact named counts', () => {
  const counts = { exportNames: 9, parity: 37, crashParity: vectors.vectors.filter(row => row.windowCrash).length, diagnostics: 10, crashCapture: 3, logsExample: 1 };
  const receipt = { schema: 1, recipe: 'PK11-npm-logs', passed: true, counts };
  assert.deepEqual(validateRuntime(receipt, vectors), counts);
  assert.throws(() => validateRuntime({ ...receipt, counts: { ...counts, parity: 36 } }, vectors));
  assert.throws(() => validateRuntime({ ...receipt, unknown: true }, vectors));
  assert.throws(() => validateRuntime({ ...receipt, passed: false }, vectors));
});
test('Windows wrapper rejects command expansion in archive paths', () => {
  const invocation = pnpmInvocation(['--version'], 'win32', { SystemRoot: 'C:\\Windows' });
  assert.equal(invocation.args.at(-1), 'pnpm "--version"');
  assert.equal(invocation.windowsVerbatimArguments, true);
  assert.equal(pnpmInvocation(['--version'], 'darwin').windowsVerbatimArguments, undefined);
  const observed = [];
  const spawn = (...args) => { observed.push(args); return { status: 0, stdout: 'ok', stderr: '' }; };
  const windowsCall = spawnPnpm(['--version'], { spawn, platform: 'win32', env: { SystemRoot: 'C:\\Windows' }, options: { cwd: 'C:\\scratch' } });
  assert.equal(observed[0][0], invocation.command);
  assert.deepEqual(observed[0][1], invocation.args);
  assert.equal(observed[0][2].windowsVerbatimArguments, true);
  assert.equal(windowsCall.result.stdout, 'ok');
  spawnPnpm(['--version'], { spawn, platform: 'darwin', env: {} });
  assert.equal(Object.hasOwn(observed[1][2], 'windowsVerbatimArguments'), false);
  assert.throws(() => pnpmInvocation(['file:C:\\temp\\%SECRET%'], 'win32'));
  assert.throws(() => pnpmInvocation(['file:C:\\temp\\x&whoami'], 'win32'));
});

function qualificationFixture() {
  const commit = 'a'.repeat(40), scratch = '/scratch/consumer', checkout = '/checkout';
  const commands = [], sourceBindings = {};
  const command = (stage, command, args, stdout, cwd = scratch) => ({ stage, command, args, stdout, stderr: '', cwd, status: 0 });
  for (const file of [...RECIPE_FILES, ...SOURCE_FILES]) {
    const bytes = fs.readFileSync(new URL(`../../${file}`, import.meta.url));
    sourceBindings[file] = { sha256: sha('sha256', bytes), gitBlob: sha('sha1', Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])) };
    commands.push(command('source', 'git', ['show', `${commit}:${file}`], bytes.toString('utf8'), checkout));
  }
  const recipe = { pkId: 'PK11', id: 'ci/consumers/npm-logs/consumer.mjs', files: Object.fromEntries(RECIPE_FILES.map(file => [file, sourceBindings[file].sha256])) };
  const recipeFiles = Object.fromEntries(RECIPE_FILES.map(file => [file, sourceBindings[file]]));
  const expected = { name: '@rightkit/logs', version: pkg.version, commit, runId: '2', runAttempt: '1', manifestRunId: '2', manifestRunAttempt: '1', manifestSha256: 'b'.repeat(64), archiveSha256: 'c'.repeat(64), archiveSize: 100, archiveIntegrity: `sha512-${'A'.repeat(86)}==`, host: 'macos', runnerOS: 'macOS', arch: 'arm64', packageManager: pkg.packageManager };
  const counts = { exportNames: 9, parity: 37, crashParity: vectors.vectors.filter(row => row.windowCrash).length, diagnostics: 10, crashCapture: 3, logsExample: 1 };
  const manifest = { name: 'rightkit-pk11-npm-consumer', private: true, type: 'module', packageManager: pkg.packageManager, dependencies: { '@rightkit/logs': 'file:/packs/logs.tgz' } };
  // Actual hosted pnpm-pack archive omits this source-only toolchain metadata.
  const packed = { ...pkg }; delete packed.packageManager;
  commands.push(command('archive-read', 'tar', ['-xOzf', 'logs.tgz', 'package/package.json'], JSON.stringify(packed), '/packs'));
  commands.push(command('archive-read', 'tar', ['-xOzf', 'logs.tgz', 'package/README.md'], readme, '/packs'));
  commands.push(command('toolchain', 'pnpm', ['--version'], `${pkg.packageManager.slice(5)}\n`));
  commands.push(command('install', 'pnpm', ['install', '--ignore-scripts', '--no-frozen-lockfile', '--ignore-workspace', '--store-dir', `${scratch}/store`], 'installed'));
  commands.push(command('runtime', '/usr/bin/node', ['consumer.mjs'], `${JSON.stringify({ schema: 1, recipe: 'PK11-npm-logs', passed: true, counts })}\n`));
  const lock = `lockfileVersion: 9\nresolution: file:/packs/logs.tgz\nintegrity: ${expected.archiveIntegrity}\n`;
  const report = { ...expected, schema: 1, kind: 'npm-consumer', qualification: 'R2a', recipeId: 'PK11-npm-logs', verified: true, commands, counts, sourceBindings: structuredClone(sourceBindings), paritySource: { path: SOURCE_FILES.at(-1), ...sourceBindings[SOURCE_FILES.at(-1)] }, recipe: { sourceCommit: commit, files: recipeFiles, sha256: sha('sha256', JSON.stringify(recipeFiles)) }, toolchain: { node: 'v24.0.0', pnpm: pkg.packageManager.slice(5), packageManager: pkg.packageManager }, lock, lockSha256: sha('sha256', lock), isolation: { checkout, scratch, originalArchive: '/packs/logs.tgz', home: `${scratch}/home`, userconfig: `${scratch}/home/.npmrc`, globalconfig: `${scratch}/home/global.npmrc`, store: `${scratch}/store`, systemRoot: 'C:\\Windows', ignoreScripts: true, inheritWorkspace: false, scriptLifecycle: 'disabled', manifest, consumerManifestSha256: sha('sha256', `${JSON.stringify(manifest, null, 2)}\n`) } };
  return { report, expected, recipe, sourceBindings };
}
test('complete command-bound named receipt admits & reviewed raw recipe inventory matches', () => {
  assert.deepEqual(npmConsumerQualificationProblems(qualificationFixture()), []);
  const inventory = JSON.parse(fs.readFileSync(new URL('../../ci/consumer-recipes.json', import.meta.url)));
  assert.deepEqual(inventory.npm['@rightkit/logs'], qualificationFixture().recipe);
});
test('receipt mutations fail closed across identity, stage, actual output, recipe & source', () => {
  const mutations = [
    fixture => { fixture.report.verified = false; },
    fixture => { fixture.report.archiveSize++; },
    fixture => { fixture.report.host = 'windows'; },
    fixture => { fixture.report.manifestSha256 = 'd'.repeat(64); },
    fixture => { fixture.report.toolchain.pnpm = '1.0.0'; },
    fixture => { const row = fixture.report.commands.find(command => command.stage === 'archive-read'); const packed = JSON.parse(row.stdout); packed.packageManager = 'pnpm@1.0.0'; row.stdout = JSON.stringify(packed); },
    fixture => { const row = fixture.report.commands.find(command => command.stage === 'archive-read'); const packed = JSON.parse(row.stdout); packed.version = '0.0.0'; row.stdout = JSON.stringify(packed); },
    fixture => { fixture.report.commands.pop(); },
    fixture => { fixture.report.commands[0].stage = 'unknown'; },
    fixture => { fixture.report.commands.at(-1).status = 1; },
    fixture => { fixture.report.commands.at(-1).stdout = '{}'; },
    fixture => { fixture.report.counts.parity = 36; },
    fixture => { fixture.report.recipe.sha256 = 'd'.repeat(64); },
    fixture => { fixture.recipe.files[RECIPE_FILES[0]] = 'd'.repeat(64); },
    fixture => { fixture.report.paritySource.gitBlob = 'e'.repeat(40); },
    fixture => { delete fixture.report.sourceBindings['packages/logs/src/crash.ts']; },
    fixture => { fixture.report.commands[0].stdout += 'forged'; },
    fixture => { fixture.report.lock = 'workspace:*'; },
    fixture => { fixture.report.isolation.inheritWorkspace = true; },
    fixture => { fixture.report.isolation.scriptLifecycle = 'enabled'; },
    fixture => { fixture.report.isolation.scratch = fixture.report.isolation.checkout; },
    fixture => { delete fixture.report.isolation.originalArchive; },
  ];
  for (const mutate of mutations) { const fixture = qualificationFixture(); mutate(fixture); assert.ok(npmConsumerQualificationProblems(fixture).length > 0); }
});
