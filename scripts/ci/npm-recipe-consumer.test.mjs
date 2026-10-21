import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { consumeNpm } from './consume.mjs';
import { sha } from './upload.mjs';
import { RUNNER_FILE, recipeDirectory, sourceFiles, hasReviewedNpmEntry, missingRequiredHosts, allowedHosts, hostForPlatform, RUNNABLE_HOSTS, runtimeEnvironment, consumerChildEnv, runNpmRecipeConsumer, validateRecipeEntry, validatePackedClosure, validateConsumerManifest, validateLockClosure, validateMountResult, scratchManifest, scratchWorkspace, validateWorkspaceBinding, consumerInstallArgs, closureAgeExceptions, validateClosureApproval, validateClosureProvenance, CLOSURE_APPROVAL, CLOSURE_PROVENANCE_SCRIPT, QA_CLOSURE_APPROVAL, QA_CLOSURE_PROVENANCE_SCRIPT, closureProvenanceScript, npmRecipeQualificationProblems } from './npm-recipe-consumer.mjs';

const read = file => fs.readFileSync(new URL(`../../${file}`, import.meta.url));
const inventory = JSON.parse(read('ci/consumer-recipes.json'));
const rootPackage = JSON.parse(read('package.json'));
const pin = rootPackage.packageManager;
const reviewedNames = Object.keys(inventory.npm).filter(name => name !== '@rightkit/logs');
const clone = value => structuredClone(value);
const NAME = '@rightkit/settings-ui';
// Closed entries retain their authenticated historical runner; this never admits fresh execution.
const CLOSED_RUNNER_HASH = 'b0d909590e65a2189520f1961c7e3278a902b9171dd47b7e71c3656f1bbac7dd';
const CLOSED_NAMES = new Set(['@rightkit/app-update', '@rightkit/onboarding', '@rightkit/qa-chrome', '@rightkit/tauri-shell', '@rightkit/legal', '@rightkit/doc-render', '@rightkit/tauri']);
const inventoryHashMatches = (name, file, hash) => sha('sha256', read(file)) === hash || (file === RUNNER_FILE && CLOSED_NAMES.has(name) && hash === CLOSED_RUNNER_HASH);

test('every committed reviewed npm recipe matches its inventory entry, package manifest & closure', () => {
  assert.ok(reviewedNames.length >= 2);
  for (const name of reviewedNames) {
    const entry = validateRecipeEntry(inventory.npm[name], name);
    const dir = recipeDirectory(name);
    const onDisk = fs.readdirSync(new URL(`../../${dir}`, import.meta.url)).map(file => `${dir}/${file}`);
    assert.deepEqual(onDisk.sort(), Object.keys(entry.files).filter(file => file !== RUNNER_FILE).sort(), `${name} directory is exactly the reviewed file set`);
    for (const [file, hash] of Object.entries(entry.files)) assert.ok(inventoryHashMatches(name, file, hash), `${name} ${file} full-file hash or exact closed runner`);
    const source = JSON.parse(read(`packages/${name.slice(10)}/package.json`));
    assert.doesNotThrow(() => validatePackedClosure(source, entry, name, source.version));
    assert.doesNotThrow(() => validateConsumerManifest(JSON.parse(read(`${dir}/package.json`)), entry, pin));
    assert.equal(hasReviewedNpmEntry(inventory, name), true);
  }
  assert.equal(hasReviewedNpmEntry(inventory, '@rightkit/logs'), false, 'logs keeps its named runner');
});

test('unreviewed recipe is refused, never skipped or defaulted', () => {
  assert.equal(hasReviewedNpmEntry(inventory, '@rightkit/unreviewed'), false);
  assert.throws(() => validateRecipeEntry(undefined, '@rightkit/unreviewed'), /missing reviewed PK example/);
  assert.throws(() => validateRecipeEntry({ ...inventory.npm[NAME], extra: true }, NAME), /missing reviewed PK example/);
  for (const bad of ['../x', '@evil/pkg', 'plain', '@rightkit/A', '@rightkit/a/b']) assert.throws(() => recipeDirectory(bad));
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-recipe-dispatch-'));
  try {
    let invoked = 0;
    const rows = consumeNpm({ units: [{ name: '@rightkit/unreviewed', version: '1.0.0', file: 'x.tgz' }], packDir: scratchRoot, scratchRoot, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 0, stdout: '{}', stderr: '' }), npmRecipeConsumer: () => { invoked++; }, npmConsumer: () => { invoked++; } });
    const row = rows.get('@rightkit/unreviewed');
    assert.equal(invoked, 0);
    assert.equal(row.report.qualification, undefined);
    assert.ok(row.problems.some(problem => problem.includes('missing reviewed PK example')));
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});

test('reviewed entries dispatch to the recipe runner; logs still dispatches to the named runner', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-recipe-dispatch-'));
  try {
    const calls = [];
    const unit = { name: NAME, version: '0.7.0', file: 'x.tgz', sha256: 'a'.repeat(64), size: 10 };
    const logs = { name: '@rightkit/logs', version: '0.1.6', file: 'l.tgz', sha256: 'b'.repeat(64), size: 11 };
    const rows = consumeNpm({ units: [unit, logs], packDir: scratchRoot, scratchRoot, npm: { command: 'npm', prefix: [], shell: false }, spawn: command => command === 'tar' ? { status: 0, stdout: read('packages/settings-ui/package.json') } : { status: 1, stderr: 'fixture portability failure' },
      npmRecipeConsumer: options => { calls.push(['recipe', options.name]); return { ...unit, archiveSha256: unit.sha256, archiveSize: unit.size, qualification: 'R2a', verified: true }; },
      npmConsumer: options => { calls.push(['logs', options.name]); return { ...logs, archiveSha256: logs.sha256, archiveSize: logs.size, qualification: 'R2a', verified: true }; } });
    assert.deepEqual(calls, [['recipe', NAME], ['logs', '@rightkit/logs']]);
    // Generic portability failures are never masked by a passing recipe.
    assert.equal(rows.get(NAME).report.verified, false);
    assert.ok(rows.get(NAME).problems.some(problem => problem.includes('fixture portability failure')));
    // A runner that throws is a problem, not a pass.
    const failed = consumeNpm({ units: [unit], packDir: scratchRoot, scratchRoot, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 0, stdout: '{}', stderr: '' }), npmRecipeConsumer: () => { throw new Error('mount failed'); } });
    assert.ok(failed.get(NAME).problems.some(problem => problem.includes('mount failed')));
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});

test('dependency outside the declared closure is refused (packed, consumer manifest and lock)', () => {
  const entry = inventory.npm[NAME];
  const source = JSON.parse(read('packages/settings-ui/package.json'));
  assert.throws(() => validatePackedClosure({ ...source, dependencies: { 'left-pad': '1.0.0' } }, entry, NAME, source.version), /outside reviewed closure: left-pad/);
  assert.throws(() => validatePackedClosure({ ...source, peerDependencies: { ...source.peerDependencies, '@rightkit/sync-worker': '*' } }, entry, NAME, source.version), /outside reviewed closure/);
  assert.throws(() => validatePackedClosure({ ...source, exports: { ...source.exports, './extra': './x.js' } }, entry, NAME, source.version), /export surface/);
  assert.throws(() => validatePackedClosure({ ...source, version: '9.9.9' }, entry, NAME, source.version), /identity/);
  const peerless = clone(source); delete peerless.peerDependencies['@rightkit/logs'];
  assert.throws(() => validatePackedClosure(peerless, entry, NAME, source.version), /declared-only/);
  const consumer = JSON.parse(read(`${recipeDirectory(NAME)}/package.json`));
  assert.throws(() => validateConsumerManifest({ ...consumer, dependencies: { ...consumer.dependencies, lodash: '4.17.21' } }, entry, pin), /differ from reviewed closure/);
  assert.throws(() => validateConsumerManifest({ ...consumer, dependencies: { ...consumer.dependencies, jsdom: '^30.1.3' } }, entry, pin), /differ from reviewed closure/);
  assert.throws(() => validateConsumerManifest({ ...consumer, scripts: { postinstall: 'node x' } }, entry, pin), /scripts/);
  assert.throws(() => validateConsumerManifest({ ...consumer, packageManager: 'pnpm@1.0.0' }, entry, pin), /pin/);
  const archive = '/packs/settings-ui.tgz';
  const lock = extra => ['lockfileVersion: \'9.0\'', 'packages:', `  '${NAME}@file:${archive}':`, '    resolution: {}', ...Object.entries(entry.closure.install).filter(([dep]) => dep.startsWith('@rightkit/')).map(([dep, version]) => `  '${dep}@${version}':`), ...extra].join('\n');
  assert.equal(validateLockClosure(lock([]), entry, NAME, 'settings-ui.tgz'), 5);
  assert.throws(() => validateLockClosure(lock(["  '@rightkit/logs@0.1.6':"]), entry, NAME, 'settings-ui.tgz'), /outside reviewed closure: @rightkit\/logs/);
  assert.throws(() => validateLockClosure(lock(["  '@rightkit/theme@0.3.2':"]), entry, NAME, 'settings-ui.tgz'), /outside reviewed closure: @rightkit\/theme/);
  assert.throws(() => validateLockClosure(lock(["  '@rightkit/legal@link:../legal':"]), entry, NAME, 'settings-ui.tgz'), /foreign dependency source/);
  assert.throws(() => validateLockClosure(lock([]).replace('file:', 'registry:'), entry, NAME, 'settings-ui.tgz'), /supplied archive/);
});

test('mount result must echo the reviewed recipe & bind the exact archive', () => {
  const entry = inventory.npm[NAME];
  const good = { schemaVersion: 1, recipeId: entry.recipeId, ok: true, nativeHostQualified: false, errors: [], scenarios: Array.from({ length: entry.scenarios }, (_, i) => ({ name: `s${i}`, ok: true, evidence: {}, error: null })), artifact: { path: '/p/a.tgz', sha256: 'a'.repeat(64), version: '0.7.0', resolution: 'packed', installationBinding: 'all-archive-files-sha256', archiveFileCount: 3 } };
  const ctx = { sha256: 'a'.repeat(64), version: '0.7.0', archive: '/p/a.tgz' };
  assert.deepEqual(validateMountResult(good, entry, ctx), { scenarios: entry.scenarios, archiveFileCount: 3 });
  for (const mutate of [r => { r.recipeId = 'other'; }, r => { r.ok = false; }, r => { r.nativeHostQualified = true; }, r => { r.scenarios.pop(); }, r => { r.scenarios[0].ok = false; }, r => { r.errors = ['x']; }, r => { r.artifact.sha256 = 'b'.repeat(64); }, r => { r.artifact.version = '0.7.1'; }, r => { r.artifact.resolution = 'fetched'; }, r => { r.artifact.installationBinding = 'runner'; }, r => { r.artifact.archiveFileCount = 0; }, r => { r.artifact.path = '/other.tgz'; }]) {
    const bad = clone(good); mutate(bad); assert.throws(() => validateMountResult(bad, entry, ctx));
  }
});

function qualificationFixture(name = NAME) {
  const NAME = name;
  const entry = clone(inventory.npm[NAME]); const dir = recipeDirectory(NAME);
  // Model a fresh current-runner qualification without rewriting any closed inventory entry.
  entry.files[RUNNER_FILE] = sha('sha256', read(RUNNER_FILE));
  const candidateInventory = clone(inventory); candidateInventory.npm[NAME] = clone(entry);
  const commit = 'a'.repeat(40), scratch = '/scratch/consumer', checkout = '/checkout', packs = '/packs', archive = `${packs}/${name.slice(10)}.tgz`;
  const source = JSON.parse(read(`packages/${name.slice(10)}/package.json`)); const version = source.version;
  const packed = { ...source }; delete packed.packageManager;
  const consumer = JSON.parse(read(`${dir}/package.json`));
  const integrityValue = `sha512-${'A'.repeat(86)}==`;
  const command = (stage, name, args, stdout, cwd = scratch) => ({ stage, command: name, args, stdout, stderr: '', cwd, status: 0 });
  const bound = [...Object.keys(entry.files), ...sourceFiles(NAME)];
  const commands = [], sourceBindings = {};
  for (const file of bound) {
    const bytes = file === 'ci/consumer-recipes.json' ? Buffer.from(JSON.stringify(candidateInventory)) : read(file);
    sourceBindings[file] = { sha256: sha('sha256', bytes), gitBlob: sha('sha1', Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])) };
    commands.push(command('source', 'git', ['show', `${commit}:${file}`], bytes.toString('utf8'), checkout));
  }
  commands.push(command('archive-read', 'tar', ['-xOzf', `${name.slice(10)}.tgz`, 'package/package.json'], JSON.stringify(packed), packs));
  commands.push(command('archive-read', 'tar', ['-xOzf', `${name.slice(10)}.tgz`, 'package/README.md'], 'readme', packs));
  commands.push(command('toolchain', 'pnpm', ['--version'], `${pin.slice(5)}\n`));
  commands.push(command('install', 'pnpm', consumerInstallArgs(`${scratch}/store`), 'installed'));
  const mount = { schemaVersion: 1, recipeId: entry.recipeId, ok: true, nativeHostQualified: false, errors: [], scenarios: Array.from({ length: entry.scenarios }, (_, i) => ({ name: `s${i}`, ok: true, evidence: {}, error: null })), artifact: { path: archive, sha256: 'c'.repeat(64), version, resolution: 'packed', installationBinding: 'all-archive-files-sha256', archiveFileCount: 3 } };
  commands.push(command('runtime', '/usr/bin/node', ['consumer.mjs'], `${JSON.stringify(mount)}\n`));
  const lock = ['lockfileVersion: \'9.0\'', 'packages:', `  '${NAME}@file:${archive}':`, `    resolution: {integrity: ${integrityValue}, tarball: file:${archive}}`, ...Object.entries(entry.closure.install).filter(([dep]) => dep.startsWith('@rightkit/')).map(([dep, v]) => `  '${dep}@${v}':`)].join('\n');
  const manifest = scratchManifest(consumer, NAME, archive, pin);
  const expected = { name: NAME, version, commit, runId: '2', runAttempt: '1', manifestRunId: '2', manifestRunAttempt: '1', manifestSha256: 'b'.repeat(64), archiveSha256: 'c'.repeat(64), archiveSize: 100, archiveIntegrity: integrityValue, host: 'macos', runnerOS: 'macOS', arch: 'arm64', packageManager: pin, archivePath: archive };
  const recipeFiles = Object.fromEntries(Object.keys(entry.files).map(file => [file, sourceBindings[file]]));
  const report = { ...expected, archivePath: undefined, schema: 1, kind: 'npm-consumer', qualification: 'R2a', recipeId: entry.recipeId, pkId: entry.pkId, verified: true, commands, sourceBindings: clone(sourceBindings), recipe: { sourceCommit: commit, files: recipeFiles, sha256: sha('sha256', JSON.stringify(recipeFiles)) }, toolchain: { node: 'v26.8.1', pnpm: pin.slice(5), packageManager: pin }, lock, lockSha256: sha('sha256', lock), mount, counts: { scenarios: entry.scenarios, archiveFileCount: 3 },
    isolation: { checkout, scratch, originalArchive: archive, home: `${scratch}/home`, userconfig: `${scratch}/home/.npmrc`, globalconfig: `${scratch}/home/global.npmrc`, store: `${scratch}/store`, systemRoot: 'C:\\Windows', ignoreScripts: true, inheritWorkspace: false, scriptLifecycle: 'disabled', manifest, consumerManifestSha256: sha('sha256', `${JSON.stringify(manifest, null, 2)}\n`) } };
  const config = scratchWorkspace(entry, NAME);
  report.workspace = { file: `${scratch}/pnpm-workspace.yaml`, before: config, beforeSha256: sha('sha256', config), after: config, afterSha256: sha('sha256', config) };
  delete report.archivePath;
  if (closureAgeExceptions(entry, name).length) {
    report.closureProvenance = { archiveBase64: Buffer.from('unapproved fixture dependency').toString('base64'), artifact: {} };
    commands.push(command('closure-provenance', '/usr/bin/node', ['--input-type=module', '-e', closureProvenanceScript(name)], JSON.stringify(report.closureProvenance)));
  }
  return { report, expected, recipe: entry, sourceBindings };
}

test('complete command-bound recipe receipt admits against the verifier-revision entry', () => {
  assert.deepEqual(npmRecipeQualificationProblems(qualificationFixture()), []);
});

test('hash mismatch, unreviewed entry and substituted evidence fail closed', () => {
  const mutations = {
    'reviewed hash differs from candidate blob': f => { f.recipe.files[`${recipeDirectory(NAME)}/consumer.mjs`] = 'd'.repeat(64); },
    'runner hash differs': f => { f.recipe.files[RUNNER_FILE] = 'd'.repeat(64); },
    'trusted blob differs from report binding': f => { f.sourceBindings[`${recipeDirectory(NAME)}/consumer.mjs`].sha256 = 'd'.repeat(64); },
    'candidate inventory differs from verifier entry': f => { const row = f.report.commands.find(c => c.args[1]?.endsWith(':ci/consumer-recipes.json')); const doc = JSON.parse(row.stdout); doc.npm[NAME].closure.install['@rightkit/theme'] = '9.9.9'; row.stdout = JSON.stringify(doc); },
    'recipe file added': f => { f.recipe.files['ci/consumers/npm/settings-ui/extra.mjs'] = 'e'.repeat(64); },
    'entry removed': f => { f.recipe = undefined; },
    'foreign recipe id': f => { f.report.recipeId = 'other-recipe'; },
    'unverified': f => { f.report.verified = false; },
    'archive size': f => { f.report.archiveSize++; },
    'pnpm pin': f => { f.report.toolchain.pnpm = '1.0.0'; },
    'packed dependency outside closure': f => { const row = f.report.commands.find(c => c.stage === 'archive-read'); const p = JSON.parse(row.stdout); p.dependencies = { sneaky: '1.0.0' }; row.stdout = JSON.stringify(p); },
    'lock resolves unreviewed @rightkit package': f => { f.report.lock += "\n  '@rightkit/logs@0.1.6':"; f.report.lockSha256 = sha('sha256', f.report.lock); },
    'lock digest': f => { f.report.lockSha256 = 'f'.repeat(64); },
    'scenario count': f => { const row = f.report.commands.find(c => c.stage === 'runtime'); const m = JSON.parse(row.stdout); m.scenarios.pop(); row.stdout = JSON.stringify(m); },
    'runtime output vs report': f => { f.report.mount.ok = false; },
    'scratch manifest dependency added': f => { f.report.isolation.manifest.dependencies.sneaky = '1.0.0'; },
    'isolated workspace missing': f => { delete f.report.workspace; },
    'foreign workspace path': f => { f.report.workspace.file = '/checkout/pnpm-workspace.yaml'; },
    'workspace config digest': f => { f.report.workspace.afterSha256 = 'f'.repeat(64); },
    'ambient workspace ignored': f => { f.report.commands.find(c => c.stage === 'install').args.splice(3, 0, '--ignore-workspace'); },
    'age policy bypass argument': f => { f.report.commands.find(c => c.stage === 'install').args.push('--minimum-release-age=0'); },
    'trust policy bypass argument': f => { f.report.commands.find(c => c.stage === 'install').args.push('--trust-policy=off'); },
    'pnpm auto-generated age exception': f => { f.report.workspace.after += "minimumReleaseAgeExclude:\n  - '@rightkit/logs@0.1.6'\n"; f.report.workspace.afterSha256 = sha('sha256', f.report.workspace.after); },
    'forged matching workspace digests': f => { f.report.workspace.before += 'trustPolicy: off\n'; f.report.workspace.after = f.report.workspace.before; f.report.workspace.beforeSha256 = f.report.workspace.afterSha256 = sha('sha256', f.report.workspace.before); },
    'lifecycle scripts enabled': f => { f.report.isolation.ignoreScripts = false; },
    'scratch inside checkout': f => { f.report.isolation.scratch = '/checkout/scratch'; },
    'missing stage': f => { f.report.commands.pop(); },
    'failed command': f => { f.report.commands.at(-1).status = 1; },
    'forged source output': f => { f.report.commands[0].stdout += 'forged'; },
  };
  for (const [label, mutate] of Object.entries(mutations)) {
    const fixture = qualificationFixture(); mutate(fixture);
    assert.ok(npmRecipeQualificationProblems(fixture).length > 0, label);
  }
});

test('install child env disables peer auto-install through pnpm_config_ and isolates credentials', () => {
  const home = path.join(os.tmpdir(), 'rk-env-home');
  const child = consumerChildEnv({ PATH: 'p', Path: 'p', GITHUB_ACTIONS: 'true', GITHUB_SHA: 'a'.repeat(40), RUNNER_OS: 'Windows', ACTIONS_ID_TOKEN_REQUEST_TOKEN: 't', NODE_AUTH_TOKEN: 'x', NODE_OPTIONS: '--require=evil', RIGHTKIT_CHROME_FOR_TESTING_ROOT: '/cft', pnpm_config_auto_install_peers: 'true', npm_config_userconfig: '/ambient' }, home);
  assert.equal(child.pnpm_config_auto_install_peers, 'false', 'pnpm 11 ignores the userconfig line; the env variable must win over an ambient value');
  assert.equal(child.npm_config_ignore_scripts, 'true');
  assert.equal(child.npm_config_userconfig, path.join(home, '.npmrc'));
  assert.equal(child.HOME, home);
  for (const key of ['GITHUB_ACTIONS', 'GITHUB_SHA', 'RUNNER_OS', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN', 'NODE_AUTH_TOKEN', 'NODE_OPTIONS', 'RIGHTKIT_CHROME_FOR_TESTING_ROOT']) assert.equal(Object.hasOwn(child, key), false, key);
  // The runner uses this helper for every pnpm call, and the userconfig line is kept as a second guard.
  const source = fs.readFileSync(new URL('./npm-recipe-consumer.mjs', import.meta.url), 'utf8');
  assert.ok(source.includes('const child = consumerChildEnv(env, home);') && source.includes('auto-install-peers=false'));
});

test('react-shell runtime peer is an explicit exact closure pin', () => {
  for (const name of ['@rightkit/onboarding', '@rightkit/settings-ui']) assert.equal(inventory.npm[name].closure.install['@rightkit/platform-ui'], '0.4.0');
});

test('qa-chrome lists Linux as a required, runnable host of its own; every other unit stays macOS & Windows', () => {
  assert.deepEqual(RUNNABLE_HOSTS, ['linux', 'macos', 'windows']);
  assert.deepEqual(missingRequiredHosts('@rightkit/qa-chrome'), []);
  assert.deepEqual(missingRequiredHosts(NAME), []);
  assert.deepEqual(allowedHosts('@rightkit/qa-chrome'), ['linux', 'macos', 'windows']);
  for (const name of reviewedNames.filter(name => name !== '@rightkit/qa-chrome')) assert.deepEqual(allowedHosts(name), ['macos', 'windows'], name);
  assert.deepEqual(['win32', 'darwin', 'linux', 'freebsd', 'sunos'].map(hostForPlatform), ['windows', 'macos', 'linux', undefined, undefined]);
});

// Runs fn with process.platform replaced, then restores it.
function withPlatform(platform, fn) {
  const original = Object.getOwnPropertyDescriptor(process, 'platform');
  Object.defineProperty(process, 'platform', { value: platform, configurable: true });
  try { return fn(); } finally { Object.defineProperty(process, 'platform', original); }
}

test('the runner labels Linux as its own host, only for a unit whose reviewed hosts name it', () => {
  const env = runnerOS => ({ GITHUB_ACTIONS: 'true', GITHUB_SHA: 'a'.repeat(40), GITHUB_RUN_ID: '1', GITHUB_RUN_ATTEMPT: '1', RUNNER_OS: runnerOS });
  const run = (name, runnerOS) => runNpmRecipeConsumer({ packDir: path.join(os.tmpdir(), 'rk-no-such-pack'), name, reportFile: path.join(os.tmpdir(), 'r.json'), env: env(runnerOS) });
  // Passes every host gate and only then fails on the absent sealed manifest (ENOENT), never on a host refusal.
  assert.throws(() => withPlatform('linux', () => run('@rightkit/qa-chrome', 'Linux')), /ENOENT/);
  assert.throws(() => withPlatform('darwin', () => run('@rightkit/qa-chrome', 'macOS')), /ENOENT/);
  assert.throws(() => withPlatform('win32', () => run('@rightkit/qa-chrome', 'Windows')), /ENOENT/);
  // A unit that does not list Linux is refused there, with or without a matching RUNNER_OS.
  assert.throws(() => withPlatform('linux', () => run(NAME, 'Linux')), /native Windows\/macOS runner required/);
  assert.throws(() => withPlatform('linux', () => run(NAME, 'macOS')), /native Windows\/macOS runner required/);
  // The runner OS must match the platform; Linux is never mislabelled macOS and an unknown platform is refused.
  assert.throws(() => withPlatform('linux', () => run('@rightkit/qa-chrome', 'macOS')), /runner required/);
  assert.throws(() => withPlatform('linux', () => run('@rightkit/qa-chrome', undefined)), /runner required/);
  assert.throws(() => withPlatform('darwin', () => run('@rightkit/qa-chrome', 'Linux')), /runner required/);
  assert.throws(() => withPlatform('freebsd', () => run('@rightkit/qa-chrome', 'Linux')), /runner required/);
});

test('admission accepts a Linux receipt only for a Linux-listed unit and only with the Linux runner OS', () => {
  const identity = (name, host, runnerOS) => {
    const fixture = qualificationFixture();
    const report = { ...fixture.report, name, host, runnerOS };
    return npmRecipeQualificationProblems({ report, expected: { ...fixture.expected, name, host, runnerOS }, recipe: inventory.npm[name], sourceBindings: fixture.sourceBindings }).filter(problem => problem.includes('malformed candidate/native identity'));
  };
  assert.deepEqual(identity('@rightkit/qa-chrome', 'linux', 'Linux'), []);
  assert.deepEqual(identity('@rightkit/qa-chrome', 'macos', 'macOS'), []);
  assert.deepEqual(identity('@rightkit/qa-chrome', 'windows', 'Windows'), []);
  assert.equal(identity('@rightkit/qa-chrome', 'linux', 'macOS').length, 1, 'Linux host with the macOS runner OS');
  assert.equal(identity('@rightkit/qa-chrome', 'macos', 'Linux').length, 1, 'macOS host with the Linux runner OS');
  assert.equal(identity('@rightkit/qa-chrome', 'freebsd', 'Linux').length, 1, 'unknown host');
  assert.equal(identity(NAME, 'linux', 'Linux').length, 1, 'a unit whose reviewed hosts omit Linux');
  assert.deepEqual(identity(NAME, 'macos', 'macOS'), []);
  assert.deepEqual(identity(NAME, 'windows', 'Windows'), []);
});

test('Chrome for Testing root must be explicit, absolute, canonical, existing and outside checkout & scratch', () => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'rk-cft-'))); const checkout = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'rk-checkout-'))); const scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'rk-scratch-')));
  const name = '@rightkit/qa-chrome'; const key = 'RIGHTKIT_CHROME_FOR_TESTING_ROOT';
  try {
    assert.deepEqual(runtimeEnvironment(name, { [key]: fs.realpathSync(root), UNRELATED: 'x' }, { checkout, scratch }), { [key]: fs.realpathSync(root) });
    assert.deepEqual(runtimeEnvironment(NAME, { [key]: root }, { checkout, scratch }), {}, 'other units receive nothing');
    for (const bad of [undefined, '', 'relative/path', path.join(root, 'absent'), path.join(checkout, 'cft'), scratch]) assert.throws(() => runtimeEnvironment(name, bad === undefined ? {} : { [key]: bad }, { checkout, scratch }), undefined, String(bad));
    fs.mkdirSync(path.join(checkout, 'cft'));
    assert.throws(() => runtimeEnvironment(name, { [key]: path.join(checkout, 'cft') }, { checkout, scratch }), /outside checkout/);
    const link = path.join(checkout, 'link'); fs.symlinkSync(root, link, 'junction');
    assert.throws(() => runtimeEnvironment(name, { [key]: link }, { checkout, scratch }), /canonical|outside checkout/);
  } finally { for (const dir of [root, checkout, scratch]) fs.rmSync(dir, { recursive: true, force: true }); }
});

test('consume forwards only the reviewed runtime variable to the unit that declares it', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-recipe-env-'));
  try {
    const seen = {};
    const ok = unit => ({ ...unit, archiveSha256: unit.sha256, archiveSize: unit.size, qualification: 'R2a', verified: true });
    const units = [{ name: '@rightkit/qa-chrome', version: '0.1.9', file: 'q.tgz', sha256: 'a'.repeat(64), size: 1 }, { name: NAME, version: '0.7.0', file: 's.tgz', sha256: 'b'.repeat(64), size: 2 }];
    consumeNpm({ units, packDir: scratchRoot, scratchRoot, env: { PATH: 'p', RIGHTKIT_CHROME_FOR_TESTING_ROOT: '/cft', NPM_TOKEN: 'secret', RIGHTKIT_OTHER: 'x' }, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 1, stderr: 'portability' }), npmRecipeConsumer: options => { seen[options.name] = options.env; return ok(units.find(unit => unit.name === options.name)); } });
    assert.equal(seen['@rightkit/qa-chrome'].RIGHTKIT_CHROME_FOR_TESTING_ROOT, '/cft');
    assert.equal(Object.hasOwn(seen[NAME], 'RIGHTKIT_CHROME_FOR_TESTING_ROOT'), false);
    for (const env of Object.values(seen)) for (const key of ['NPM_TOKEN', 'RIGHTKIT_OTHER']) assert.equal(Object.hasOwn(env, key), false);
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});


test('isolated workspace overrides are deterministic exact reviewed RightKit pins only', () => {
  const name = NAME; const entry = clone(inventory.npm[name]);
  delete entry.importClosures; entry.closure.declaredOnly = [];
  entry.closure.install = { react: '19.1.0', '@rightkit/logs': '0.1.6', '@rightkit/app-update': '0.4.0' };
  const config = 'packages: []\noverrides:\n  "@rightkit/app-update": "0.4.0"\n  "@rightkit/logs": "0.1.6"\n';
  assert.equal(scratchWorkspace(entry, name), config);
  entry.closure.install = Object.fromEntries(Object.entries(entry.closure.install).reverse());
  assert.equal(scratchWorkspace(entry, name), config, 'inventory object insertion order does not alter config bytes');
  entry.closure.install['@rightkit/logs'] = '^0.1.6';
  assert.throws(() => scratchWorkspace(entry, name), /exact registry version/);
  entry.closure.install = { react: '19.1.0' };
  assert.equal(scratchWorkspace(entry, name), 'packages: []\noverrides: {}\n');
});

test('workspace binding rejects age/trust policy changes even with recomputed digests', () => {
  const entry = inventory.npm[NAME]; const scratch = '/scratch/consumer';
  const config = scratchWorkspace(entry, NAME); const digest = sha('sha256', config);
  const good = { file: `${scratch}/pnpm-workspace.yaml`, before: config, beforeSha256: digest, after: config, afterSha256: digest };
  assert.equal(validateWorkspaceBinding(good, entry, NAME, scratch, path.posix), true);
  for (const addition of ["minimumReleaseAgeExclude:\n  - '@rightkit/logs@0.1.6'\n", 'minimumReleaseAge: 0\n', 'trustPolicy: off\n', 'trustPolicyExclude: ["@rightkit/logs"]\n', 'packages: ["../*"]\n']) {
    const bad = clone(good); bad.after += addition; bad.afterSha256 = sha('sha256', bad.after);
    assert.throws(() => validateWorkspaceBinding(bad, entry, NAME, scratch, path.posix), /pnpm changed policy\/config/);
    bad.before = bad.after; bad.beforeSha256 = bad.afterSha256;
    assert.throws(() => validateWorkspaceBinding(bad, entry, NAME, scratch, path.posix), /exact reviewed overrides/);
  }
  assert.throws(() => validateWorkspaceBinding({ ...good, unreviewed: true }, entry, NAME, scratch, path.posix));
  assert.deepEqual(consumerInstallArgs('/scratch/store'), ['install', '--ignore-scripts', '--no-frozen-lockfile', '--store-dir', '/scratch/store']);
});


test('tauri age exception is singleton reviewed dependency bytes, never another closure or unit', () => {
  const name = '@rightkit/tauri'; const entry = clone(inventory.npm[name]);
  assert.deepEqual(closureAgeExceptions(entry, name), ['@rightkit/logs@0.1.6']);
  assert.equal(entry.closure.install['@tauri-apps/plugin-dialog'], '2.8.3');
  const config = scratchWorkspace(entry, name); const digest = sha('sha256', config);
  assert.equal(validateWorkspaceBinding({ file: '/scratch/pnpm-workspace.yaml', before: config, after: config, beforeSha256: digest, afterSha256: digest }, entry, name, '/scratch', path.posix), true);
  assert.match(scratchWorkspace(entry, name), /^packages: \[\]\nminimumReleaseAgeExclude:\n  - "@rightkit\/logs@0\.1\.6"\noverrides:/);
  assert.deepEqual(closureAgeExceptions(inventory.npm[NAME], NAME), []);
  const approvals = [{ registry: 'npm', ...CLOSURE_APPROVAL, reviewer: 'reviewed', reviewRef: 'reviewed-source' }];
  assert.doesNotThrow(() => validateClosureApproval(approvals, entry, name));
  for (const bad of [[], [...approvals, ...approvals], [{ ...approvals[0], sha256: 'f'.repeat(64) }], [{ ...approvals[0], reviewer: '' }]]) assert.throws(() => validateClosureApproval(bad, entry, name), /approval/);
  entry.closure.install['@rightkit/logs'] = '0.1.5';
  assert.throws(() => closureAgeExceptions(entry, name), /exact selected reviewed closure/);
  entry.closure.install['@rightkit/logs'] = '0.1.6';
  entry.closure.install['@tauri-apps/plugin-dialog'] = '2.8.2';
  assert.throws(() => closureAgeExceptions(entry, name), /exact selected reviewed closure/);
  delete entry.closure.install['@tauri-apps/plugin-dialog'];
  assert.throws(() => closureAgeExceptions(entry, name), /exact selected reviewed closure/);
  entry.closure.install['@tauri-apps/plugin-dialog'] = '2.8.3'; entry.closure.install.react = '19.1.0';
  assert.throws(() => closureAgeExceptions(entry, name), /exact selected reviewed closure/);
  assert.throws(() => validateClosureProvenance({ archiveBase64: Buffer.from('substituted archive').toString('base64'), artifact: {} }, '', inventory.npm[name], name, '/scratch', path.posix), /approved bytes/);
  assert.throws(() => validateClosureProvenance({}, '', inventory.npm[NAME], NAME, '/scratch', path.posix), /unexpected dependency/);
  assert.ok(CLOSURE_PROVENANCE_SCRIPT.includes("import { validateInstalledArtifact } from './provenance.mjs'"));
});


test('historical runner inventory allowance is exact & never applies to QA, future units or other files', () => {
  for (const name of CLOSED_NAMES) assert.equal(inventory.npm[name].files[RUNNER_FILE], CLOSED_RUNNER_HASH, name);
  assert.equal(inventoryHashMatches('@rightkit/settings-ui', RUNNER_FILE, CLOSED_RUNNER_HASH), false);
  for (const name of ['@rightkit/qa', '@rightkit/future']) assert.equal(inventoryHashMatches(name, RUNNER_FILE, CLOSED_RUNNER_HASH), false);
  assert.equal(inventoryHashMatches(NAME, RUNNER_FILE, 'f'.repeat(64)), false);
  assert.equal(inventoryHashMatches(NAME, `${recipeDirectory(NAME)}/consumer.mjs`, CLOSED_RUNNER_HASH), false);
  const fixture = qualificationFixture(); fixture.recipe.files[RUNNER_FILE] = CLOSED_RUNNER_HASH;
  assert.ok(npmRecipeQualificationProblems(fixture).some(problem => problem.includes(`reviewed recipe differs from candidate blob ${RUNNER_FILE}`)), 'fresh qualification still refuses stale runner inventory');
});

test('QA age exception requires exact approved Chrome package, version, archive & reviewed closure', () => {
  const name = '@rightkit/qa'; const entry = clone(inventory.npm[name]);
  assert.deepEqual(closureAgeExceptions(entry, name), ['@rightkit/qa-chrome@0.1.10']);
  assert.deepEqual(sourceFiles(name), ['ci/consumer-recipes.json', 'package.json', 'packages/qa/package.json', 'ci/approved-artifacts.json']);
  const approved = [{ registry: 'npm', ...QA_CLOSURE_APPROVAL, reviewer: 'reviewed', reviewRef: 'reviewed-source' }];
  assert.doesNotThrow(() => validateClosureApproval(approved, entry, name));
  assert.doesNotThrow(() => validateClosureApproval(JSON.parse(read('ci/approved-artifacts.json')).artifacts, entry, name));
  for (const rows of [[], [...approved, ...approved], [{ ...approved[0], name: '@rightkit/logs' }], [{ ...approved[0], version: '0.1.9' }], [{ ...approved[0], sha256: 'f'.repeat(64) }], [{ ...approved[0], registry: 'crates' }], [{ ...approved[0], reviewer: ' ' }], [{ ...approved[0], reviewRef: '' }]]) assert.throws(() => validateClosureApproval(rows, entry, name), /approval/);
  for (const install of [{ '@rightkit/qa-chrome': '0.1.9' }, { '@rightkit/qa-chrome': '0.1.11' }, { '@rightkit/logs': '0.1.10' }, {}, { '@rightkit/qa-chrome': '0.1.10', react: '19.3.1' }]) {
    const bad = clone(entry); bad.closure.install = install;
    assert.throws(() => closureAgeExceptions(bad, name), /exact selected reviewed closure/);
  }
  for (const other of reviewedNames.filter(unit => !['@rightkit/qa', '@rightkit/tauri'].includes(unit))) assert.deepEqual(closureAgeExceptions(inventory.npm[other], other), [], other);
});

test('QA workspace permits only exact selected Chrome age exception & rejects config substitution', () => {
  const name = '@rightkit/qa'; const entry = inventory.npm[name]; const scratch = '/scratch/consumer';
  const config = 'packages: []\nminimumReleaseAgeExclude:\n  - "@rightkit/qa-chrome@0.1.10"\noverrides:\n  "@rightkit/qa-chrome": "0.1.10"\n';
  assert.equal(scratchWorkspace(entry, name), config);
  const digest = sha('sha256', config);
  const good = { file: `${scratch}/pnpm-workspace.yaml`, before: config, after: config, beforeSha256: digest, afterSha256: digest };
  assert.equal(validateWorkspaceBinding(good, entry, name, scratch, path.posix), true);
  for (const changed of [config.replace('@0.1.10', '@0.1.9'), config.replace('@0.1.10', ''), config.replace('  - "@rightkit/qa-chrome@0.1.10"\n', ''), `${config}minimumReleaseAge: 0\n`, `${config}trustPolicy: off\n`, `${config}  - "@rightkit/logs@0.1.6"\n`]) {
    const bad = { ...good, after: changed, afterSha256: sha('sha256', changed) };
    assert.throws(() => validateWorkspaceBinding(bad, entry, name, scratch, path.posix), /exact reviewed overrides/);
    bad.before = changed; bad.beforeSha256 = bad.afterSha256;
    assert.throws(() => validateWorkspaceBinding(bad, entry, name, scratch, path.posix), /exact reviewed overrides/);
  }
});

test('QA provenance command selects exact Chrome registry bytes & keeps Tauri command unchanged', () => {
  assert.equal(closureProvenanceScript('@rightkit/qa'), QA_CLOSURE_PROVENANCE_SCRIPT);
  assert.equal(closureProvenanceScript('@rightkit/tauri'), CLOSURE_PROVENANCE_SCRIPT);
  assert.equal(sha('sha256', CLOSURE_PROVENANCE_SCRIPT), '9f9ce9a2ef8fa8882d4c06950476b7dff407a0c537e6f9330e4c2252c81c6599', 'existing Tauri provenance command remains byte-for-byte stable');
  assert.ok(QA_CLOSURE_PROVENANCE_SCRIPT.includes(JSON.stringify(QA_CLOSURE_APPROVAL)));
  assert.ok(QA_CLOSURE_PROVENANCE_SCRIPT.includes("fetch('https://registry.npmjs.org/@rightkit/qa-chrome/-/qa-chrome-0.1.10.tgz', { redirect: 'error', signal: AbortSignal.timeout(60000) })"));
  assert.ok(QA_CLOSURE_PROVENANCE_SCRIPT.includes("path.resolve('closure-qa-chrome.tgz')"));
  assert.ok(QA_CLOSURE_PROVENANCE_SCRIPT.includes("import { validateInstalledArtifact } from './provenance.mjs'"));
  assert.ok(!QA_CLOSURE_PROVENANCE_SCRIPT.includes('closure-logs.tgz'));
  assert.ok(!QA_CLOSURE_PROVENANCE_SCRIPT.includes('https://registry.npmjs.org/@rightkit/logs/'));
  const name = '@rightkit/qa'; const entry = inventory.npm[name];
  assert.throws(() => validateClosureProvenance(undefined, '', entry, name, '/scratch', path.posix), /malformed/);
  assert.throws(() => validateClosureProvenance({ archiveBase64: Buffer.from('substituted Chrome archive').toString('base64'), artifact: { ...QA_CLOSURE_APPROVAL, installationBinding: 'all-archive-files-sha256' } }, '', entry, name, '/scratch', path.posix), /approved bytes/);
});


test('QA admission binds approval source, exact provenance command, output & config before accepting dependency bytes', () => {
  // Exact approved tarball is intentionally not duplicated in unit fixtures; root integration checks it.
  const fixture = qualificationFixture('@rightkit/qa');
  assert.deepEqual(npmRecipeQualificationProblems(fixture), ['npm recipe: actual packed/consumer/lock output invalid: registry dependency differs from approved bytes']);
  const mutations = [
    ['dependency provenance command differs', f => { f.report.commands.find(c => c.stage === 'closure-provenance').args[2] = CLOSURE_PROVENANCE_SCRIPT; }],
    ['dependency provenance command differs', f => { f.report.commands.find(c => c.stage === 'closure-provenance').cwd = '/elsewhere'; }],
    ['dependency provenance output/report mismatch', f => { f.report.closureProvenance.artifact = { sha256: QA_CLOSURE_APPROVAL.sha256 }; }],
    ['missing/duplicate closure-provenance stage', f => { f.report.commands = f.report.commands.filter(c => c.stage !== 'closure-provenance'); }],
    ['unknown/missing source binding', f => { delete f.report.sourceBindings['ci/approved-artifacts.json']; }],
    ['exact selected dependency bytes lack reviewer approval', f => {
      const file = 'ci/approved-artifacts.json'; const row = f.report.commands.find(c => c.stage === 'source' && c.args[1].endsWith(`:${file}`));
      const approval = JSON.parse(row.stdout); approval.artifacts = approval.artifacts.filter(a => a.name !== '@rightkit/qa-chrome'); row.stdout = JSON.stringify(approval);
      const bytes = Buffer.from(row.stdout); const binding = { sha256: sha('sha256', bytes), gitBlob: sha('sha1', Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])) };
      f.sourceBindings[file] = binding; f.report.sourceBindings[file] = clone(binding);
    }],
    ['exact reviewed overrides', f => { f.report.workspace.after += 'minimumReleaseAge: 0\n'; f.report.workspace.afterSha256 = sha('sha256', f.report.workspace.after); }],
  ];
  for (const [reason, mutate] of mutations) {
    const changed = qualificationFixture('@rightkit/qa'); mutate(changed);
    assert.ok(npmRecipeQualificationProblems(changed).some(problem => problem.includes(reason)), reason);
  }
});
