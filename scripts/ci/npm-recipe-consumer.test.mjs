import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { consumeNpm } from './consume.mjs';
import { sha } from './upload.mjs';
import { RUNNER_FILE, recipeDirectory, sourceFiles, hasReviewedNpmEntry, missingRequiredHosts, allowedHosts, hostForPlatform, RUNNABLE_HOSTS, runtimeEnvironment, consumerChildEnv, runNpmRecipeConsumer, validateRecipeEntry, validatePackedClosure, validateConsumerManifest, validateLockClosure, validateMountResult, scratchManifest, npmRecipeQualificationProblems } from './npm-recipe-consumer.mjs';

const read = file => fs.readFileSync(new URL(`../../${file}`, import.meta.url));
const inventory = JSON.parse(read('ci/consumer-recipes.json'));
const rootPackage = JSON.parse(read('package.json'));
const pin = rootPackage.packageManager;
const reviewedNames = Object.keys(inventory.npm).filter(name => name !== '@rightkit/logs');
const clone = value => structuredClone(value);
const NAME = '@rightkit/settings-ui';

test('every committed reviewed npm recipe matches its inventory entry, package manifest & closure', () => {
  assert.ok(reviewedNames.length >= 2);
  for (const name of reviewedNames) {
    const entry = validateRecipeEntry(inventory.npm[name], name);
    const dir = recipeDirectory(name);
    const onDisk = fs.readdirSync(new URL(`../../${dir}`, import.meta.url)).map(file => `${dir}/${file}`);
    assert.deepEqual(onDisk.sort(), Object.keys(entry.files).filter(file => file !== RUNNER_FILE).sort(), `${name} directory is exactly the reviewed file set`);
    for (const [file, hash] of Object.entries(entry.files)) assert.equal(sha('sha256', read(file)), hash, `${name} ${file} full-file hash`);
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

function qualificationFixture() {
  const entry = clone(inventory.npm[NAME]); const dir = recipeDirectory(NAME);
  const commit = 'a'.repeat(40), scratch = '/scratch/consumer', checkout = '/checkout', packs = '/packs', archive = `${packs}/settings-ui.tgz`;
  const source = JSON.parse(read('packages/settings-ui/package.json')); const version = source.version;
  const packed = { ...source }; delete packed.packageManager;
  const consumer = JSON.parse(read(`${dir}/package.json`));
  const integrityValue = `sha512-${'A'.repeat(86)}==`;
  const command = (stage, name, args, stdout, cwd = scratch) => ({ stage, command: name, args, stdout, stderr: '', cwd, status: 0 });
  const bound = [...Object.keys(entry.files), ...sourceFiles(NAME)];
  const commands = [], sourceBindings = {};
  for (const file of bound) {
    const bytes = read(file);
    sourceBindings[file] = { sha256: sha('sha256', bytes), gitBlob: sha('sha1', Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])) };
    commands.push(command('source', 'git', ['show', `${commit}:${file}`], bytes.toString('utf8'), checkout));
  }
  commands.push(command('archive-read', 'tar', ['-xOzf', 'settings-ui.tgz', 'package/package.json'], JSON.stringify(packed), packs));
  commands.push(command('archive-read', 'tar', ['-xOzf', 'settings-ui.tgz', 'package/README.md'], 'readme', packs));
  commands.push(command('toolchain', 'pnpm', ['--version'], `${pin.slice(5)}\n`));
  commands.push(command('install', 'pnpm', ['install', '--ignore-scripts', '--no-frozen-lockfile', '--ignore-workspace', '--store-dir', `${scratch}/store`], 'installed'));
  const mount = { schemaVersion: 1, recipeId: entry.recipeId, ok: true, nativeHostQualified: false, errors: [], scenarios: Array.from({ length: entry.scenarios }, (_, i) => ({ name: `s${i}`, ok: true, evidence: {}, error: null })), artifact: { path: archive, sha256: 'c'.repeat(64), version, resolution: 'packed', installationBinding: 'all-archive-files-sha256', archiveFileCount: 3 } };
  commands.push(command('runtime', '/usr/bin/node', ['consumer.mjs'], `${JSON.stringify(mount)}\n`));
  const lock = ['lockfileVersion: \'9.0\'', 'packages:', `  '${NAME}@file:${archive}':`, `    resolution: {integrity: ${integrityValue}, tarball: file:${archive}}`, ...Object.entries(entry.closure.install).filter(([dep]) => dep.startsWith('@rightkit/')).map(([dep, v]) => `  '${dep}@${v}':`)].join('\n');
  const manifest = scratchManifest(consumer, NAME, archive, pin);
  const expected = { name: NAME, version, commit, runId: '2', runAttempt: '1', manifestRunId: '2', manifestRunAttempt: '1', manifestSha256: 'b'.repeat(64), archiveSha256: 'c'.repeat(64), archiveSize: 100, archiveIntegrity: integrityValue, host: 'macos', runnerOS: 'macOS', arch: 'arm64', packageManager: pin, archivePath: archive };
  const recipeFiles = Object.fromEntries(Object.keys(entry.files).map(file => [file, sourceBindings[file]]));
  const report = { ...expected, archivePath: undefined, schema: 1, kind: 'npm-consumer', qualification: 'R2a', recipeId: entry.recipeId, pkId: entry.pkId, verified: true, commands, sourceBindings: clone(sourceBindings), recipe: { sourceCommit: commit, files: recipeFiles, sha256: sha('sha256', JSON.stringify(recipeFiles)) }, toolchain: { node: 'v26.8.1', pnpm: pin.slice(5), packageManager: pin }, lock, lockSha256: sha('sha256', lock), mount, counts: { scenarios: entry.scenarios, archiveFileCount: 3 },
    isolation: { checkout, scratch, originalArchive: archive, home: `${scratch}/home`, userconfig: `${scratch}/home/.npmrc`, globalconfig: `${scratch}/home/global.npmrc`, store: `${scratch}/store`, systemRoot: 'C:\\Windows', ignoreScripts: true, inheritWorkspace: false, scriptLifecycle: 'disabled', manifest, consumerManifestSha256: sha('sha256', `${JSON.stringify(manifest, null, 2)}\n`) } };
  delete report.archivePath;
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
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-cft-')); const checkout = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-checkout-')); const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-scratch-'));
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
