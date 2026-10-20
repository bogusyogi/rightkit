import './test-support/broker-guard.mjs';
import { tempRoot } from './test-support/temp-roots.mjs';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import { EventEmitter } from 'node:events';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { pathToFileURL } from 'node:url';
import { checkPublicationGate, provenGateSteps, publishGatedTarball } from './publication-gate.mjs';
import { LOCAL_WIN32_HOST_POLICY_REASON, LocalPublishError, batchCargoExecute, batchClippyInvocations, checkCrateLayers, consumerRows, deriveCrateLayers, localTestPlan, parseCrateLayers, parseLocalPublishArgs, runLocalPublish } from './local-publish.mjs';
import { runCargoAsync, spawnCollect } from './managed-cargo.mjs';

const HEAD = 'a'.repeat(40);
const LIBTEST = 'test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n';
const TOKEN = 'npm_FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE';
const results = await import(pathToFileURL(path.resolve(import.meta.dirname, '../../scripts/ci/test-results.mjs')).href);

function workspace({ kind = 'npm', assetGated = false, gateConfig = {} } = {}) {
  const root = tempRoot('rk-local-publish-test-');
  const dir = kind === 'npm' ? 'packages/demo' : 'crates/rightkit-demo';
  fs.mkdirSync(path.join(root, dir), { recursive: true });
  fs.mkdirSync(path.join(root, 'ci'), { recursive: true });
  fs.writeFileSync(path.join(root, 'ci', 'gate.config.json'), JSON.stringify(gateConfig));
  fs.writeFileSync(path.join(root, 'ci', 'asset-gated.json'), JSON.stringify(assetGated ? { npm: { '@rightkit/demo': 'x' }, crates: { 'rightkit-demo': 'x' } } : {}));
  if (kind === 'npm') fs.writeFileSync(path.join(root, dir, 'package.json'), JSON.stringify({ name: '@rightkit/demo', version: '1.2.3', scripts: { test: 'node --test' } }));
  return root;
}

function harness(root, kind, over = {}) {
  const calls = []; const logs = []; const envs = [];
  const unit = kind === 'npm' ? '@rightkit/demo' : 'rightkit-demo';
  const argv = kind === 'npm' ? ['--package', unit, '--local', '--workspace', root] : ['--crate', unit, '--local', '--workspace', root];
  const published = [];
  const deps = {
    log: (line) => logs.push(line),
    platform: 'darwin',
    arch: 'arm64',
    now: () => '2026-10-14T00:00:00.000Z',
    localBuildGranted: () => true,
    git: (args) => (args[0] === 'rev-parse' ? HEAD : ''),
    registryHas: async () => false,
    readCargoPackage: () => ({ name: 'rightkit-demo', version: '1.2.3' }),
    loadWorkspaceModules: async () => ({ results, checkNpmTarball: () => ({ problems: [] }) }),
    run: (command, args, options) => {
      calls.push({ command, args, env: options.env });
      envs.push(options.env);
      const text = `${command} ${args.join(' ')}`;
      if (args.includes('--pack-destination')) fs.writeFileSync(path.join(args[args.indexOf('--pack-destination') + 1], 'rightkit-demo-1.2.3.tgz'), 'tarball-bytes');
      if (command === 'cargo' && args[0] === 'metadata') {
        fs.mkdirSync(path.join(root, 'target', 'package'), { recursive: true });
        fs.writeFileSync(path.join(root, 'target', 'package', 'rightkit-demo-1.2.3.crate'), 'crate-bytes');
        return { status: 0, stdout: JSON.stringify({ target_directory: path.join(root, 'target') }), stderr: '' };
      }
      return { status: 0, stdout: /\btest\b/.test(text) ? LIBTEST : '', stderr: '' };
    },
    enforceGate: (options) => {
      const gate = checkPublicationGate({ ...options, git: (args) => (args[0] === 'rev-parse' ? HEAD : '') });
      assert.deepEqual(gate.missing, []);
      return gate;
    },
    publishGatedTarball: (info) => published.push({ ...info, bytes: fs.readFileSync(info.tarball, 'utf8') }),
    runCargoPublish: async (options) => { published.push(options); return { name: 'rightkit-demo', version: '1.2.3' }; },
    recordPublished: (entry) => published.push({ recorded: entry }),
    ...over,
  };
  return { run: () => runLocalPublish(kind, argv, deps), calls, logs, published, deps, envs, root };
}

const refused = (promise, pattern) => assert.rejects(promise, (error) => error instanceof LocalPublishError && pattern.test(error.message));

test('argument parsing demands --local and a well-formed unit', () => {
  assert.throws(() => parseLocalPublishArgs('npm', ['--package', '@rightkit/x']), /--local is required/);
  assert.throws(() => parseLocalPublishArgs('npm', ['--local']), /requires --package/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crate', 'Bad_Name']), /requires --crate/);
  assert.throws(() => parseLocalPublishArgs('npm', ['--local', '--package', '@rightkit/x', '--skip-tests']), /unknown publish npm --local option/);
  assert.throws(() => parseLocalPublishArgs('npm', ['--local', '--package', '@rightkit/x', '--evidence', 'e.json']), /unknown publish npm --local option/);
});

test('refuses without the local-build grant, off the Mac, on a dirty tree and for a published version', async () => {
  const root = workspace();
  await refused(harness(root, 'npm', { localBuildGranted: () => false }).run(), /local-build grant/);
  await refused(harness(root, 'npm', { platform: 'linux' }).run(), /Mac only/);
  await refused(harness(root, 'npm', { git: (args) => (args[0] === 'rev-parse' ? HEAD : ' M packages/demo/index.js') }).run(), /not clean/);
  await refused(harness(root, 'npm', { git: () => null }).run(), /cannot resolve HEAD/);
  await refused(harness(root, 'npm', { registryHas: async () => true }).run(), /already on the npm registry/);
  await assert.rejects(harness(root, 'npm', { registryHas: async () => { throw new Error('registry unreachable'); } }).run(), /registry unreachable/);
  assert.equal(fs.existsSync(path.join(root, 'release-evidence')), false);
});

test('refuses when a unit test command fails, and publishes nothing', async () => {
  const root = workspace({ kind: 'cargo' });
  const h = harness(root, 'cargo', { run: (command, args) => (args[0] === 'test' ? { status: 101, stdout: 'test result: FAILED. 1 passed; 1 failed; 0 ignored', stderr: 'boom' } : { status: 0, stdout: '', stderr: '' }) });
  await refused(h.run(), /step failed.*cargo test -p rightkit-demo/s);
  assert.deepEqual(h.published, []);
  assert.equal(fs.existsSync(path.join(root, 'release-evidence')), false);
});

test('failed gate output preserves stdout test names before long stderr', async () => {
  const root = workspace({ kind: 'cargo' });
  const marker = 'test peer::tests::cross_user_socket_is_rejected ... FAILED';
  const stderr = Array.from({ length: 50 }, (_, i) => `stderr line ${i}`).join('\n');
  const h = harness(root, 'cargo', { run: (command, args) => (args[0] === 'test'
    ? { status: 101, stdout: `${marker}\ntest result: FAILED. 1 passed; 1 failed; 0 ignored`, stderr }
    : { status: 0, stdout: '', stderr: '' }) });
  await assert.rejects(h.run(), (error) => error instanceof LocalPublishError
    && error.message.includes(`stdout:\n${marker}`)
    && error.message.includes('stderr:\nstderr line 0')
    && error.message.includes('stderr line 49'));
  assert.deepEqual(h.published, []);
  assert.equal(fs.existsSync(path.join(root, 'release-evidence')), false);
});

test('refuses when no test executed, or a skip outside an asset-gated unit', async () => {
  const root = workspace();
  const quiet = harness(root, 'npm', { run: (command, args) => { if (args.includes('--pack-destination')) throw new Error('must not pack'); return { status: 0, stdout: '', stderr: '' }; } });
  await refused(quiet.run(), /not qualified.*no test executed/s);
  const skipping = harness(root, 'npm', { run: (command, args) => ({ status: 0, stdout: args.includes('test') ? `${LIBTEST}SKIP needs chrome\n` : '', stderr: '' }) });
  await refused(skipping.run(), /skipped/);
  assert.equal(fs.existsSync(path.join(root, 'release-evidence')), false);
});

test('the test plan mirrors scripts/ci/gate.mjs (feature sets, serial run, clippy on every set)', () => {
  const config = { rust: { 'rightkit-demo': { featureSets: [['--all-features'], ['--features', 'async']], alsoSerial: true } } };
  const plan = localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'crates/rightkit-demo', config });
  assert.deepEqual(plan.steps.map((s) => s.args.join(' ')), [
    'test -p rightkit-demo --message-format=json-render-diagnostics --all-features --no-fail-fast -- --show-output',
    'test -p rightkit-demo --message-format=json-render-diagnostics --all-features --no-fail-fast -- --test-threads=1 --show-output',
    'test -p rightkit-demo --message-format=json-render-diagnostics --features async --no-fail-fast -- --show-output',
    'test -p rightkit-demo --message-format=json-render-diagnostics --features async --no-fail-fast -- --test-threads=1 --show-output',
    'clippy -p rightkit-demo --message-format=json-render-diagnostics --no-deps --all-targets --all-features -- -D warnings',
    'clippy -p rightkit-demo --message-format=json-render-diagnostics --no-deps --all-targets --features async -- -D warnings',
  ]);
  assert.deepEqual(plan.featureLabels, ['--all-features', '--features async']);
  assert.throws(() => localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'x', config: { rust: { 'rightkit-demo': { os: ['windows'] } } } }), /restricted to windows/);
  assert.throws(() => localTestPlan({ kind: 'npm', unit: '@rightkit/demo', dir: 'packages/demo', manifest: { scripts: {} } }), /no test script/);
});

test('the plan compiles every feature: --all-features by default, explicit sets for exclusive features, none when already tested', () => {
  const plan = (entry) => localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'crates/rightkit-demo', config: { rust: { 'rightkit-demo': entry } } }).steps.map((s) => s.args.join(' ')).filter((a) => a.startsWith('check'));
  assert.deepEqual(plan({}), ['check -p rightkit-demo --message-format=json-render-diagnostics --all-targets --all-features']);
  assert.deepEqual(plan({ checkFeatureSets: [['--features', 'a'], ['--features', 'b']] }), ['check -p rightkit-demo --message-format=json-render-diagnostics --all-targets --features a', 'check -p rightkit-demo --message-format=json-render-diagnostics --all-targets --features b']);
  assert.deepEqual(plan({ featureSets: [['--all-features']] }), []);
});

const WV2_ROW = { manifestPath: 'ci/consumers/wv2-0_39/Cargo.toml', args: ['--no-default-features', '--features', 'wv2-0_39'], kind: 'check', targets: ['x86_64-pc-windows-msvc'], locked: true };
const consumerPlan = (entry, extra = {}) => localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'crates/rightkit-demo', config: { rust: { 'rightkit-demo': entry } }, ...extra });

test('consumerRows: a valid row is normalised, defaults apply, and a crate without rows has none', () => {
  assert.deepEqual(consumerRows('rightkit-demo', {}), []);
  assert.deepEqual(consumerRows('rightkit-demo', { consumerRows: [WV2_ROW] }), [WV2_ROW]);
  assert.deepEqual(consumerRows('rightkit-demo', { consumerRows: [{ manifestPath: 'ci/consumers/a/b/Cargo.toml' }] }), [{ manifestPath: 'ci/consumers/a/b/Cargo.toml', kind: 'check', args: [], targets: [], locked: true }]);
});

test('consumerRows: refuses unknown fields and malformed values', () => {
  const bad = (row, pattern) => assert.throws(() => consumerRows('rightkit-demo', { consumerRows: [row] }), (error) => error instanceof LocalPublishError && pattern.test(error.message));
  bad({ ...WV2_ROW, extra: 1 }, /unknown field "extra"/);
  bad({ ...WV2_ROW, os: ['windows'] }, /unknown field "os"/);
  bad({ args: [] }, /manifestPath/);
  for (const manifestPath of ['Cargo.toml', '/abs/ci/consumers/x/Cargo.toml', 'ci/consumers/../../Cargo.toml', 'ci/consumers/x/../y/Cargo.toml', 'ci/consumers/./x/Cargo.toml', 'ci\\consumers\\x\\Cargo.toml', 'crates/x/Cargo.toml', 'ci/consumers/x/Cargo.lock', 'ci/consumers/Cargo.toml']) bad({ ...WV2_ROW, manifestPath }, /manifestPath/);
  bad({ ...WV2_ROW, kind: 'test' }, /kind must be/);
  bad({ ...WV2_ROW, kind: 'clippy' }, /kind must be/);
  bad({ ...WV2_ROW, locked: false }, /locked must be true/);
  bad({ ...WV2_ROW, locked: 'yes' }, /locked must be true/);
  bad({ ...WV2_ROW, args: '--features x' }, /args must be an array/);
  bad({ ...WV2_ROW, args: ['--features', 3] }, /args must be an array/);
  for (const arg of ['--manifest-path', '--manifest-path=x', '--target=x86_64-pc-windows-msvc', '--locked', '--offline', '-p', '-pfoo', '--package', '--workspace', '--message-format=json', '--jobs=2', '-j2', '-Zunstable-options', '--release', '--']) bad({ ...WV2_ROW, args: [arg] }, /args may not contain/);
  bad({ ...WV2_ROW, targets: 'x86_64-pc-windows-msvc' }, /targets must be an array/);
  bad({ ...WV2_ROW, targets: ['not a triple'] }, /target triples/);
  bad({ ...WV2_ROW, targets: ['x86_64-pc-windows-msvc', 'x86_64-pc-windows-msvc'] }, /twice/);
  assert.throws(() => consumerRows('rightkit-demo', { consumerRows: [WV2_ROW, WV2_ROW] }), /repeats an earlier row/);
  assert.throws(() => consumerRows('rightkit-demo', { consumerRows: 'x' }), /consumerRows must be an array/);
  assert.throws(() => consumerRows('rightkit-demo', { consumerRows: [null] }), /must be an object/);
  assert.throws(() => consumerRows('rightkit-demo', { consumerRows: [[]] }), /must be an object/);
});

test('consumerRows: the plan runs each row as a locked --manifest-path check after the crate checks, one per target, for the owning crate only', () => {
  const steps = (entry, unit = 'rightkit-demo', extra = {}) => localTestPlan({ kind: 'cargo', unit, dir: `crates/${unit}`, config: { rust: { 'rightkit-demo': entry } }, ...extra }).steps.map((s) => s.args.join(' '));
  const entry = { consumerRows: [WV2_ROW, { manifestPath: 'ci/consumers/second/Cargo.toml', targets: ['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc'] }] };
  assert.deepEqual(steps(entry).filter((a) => a.includes('--manifest-path')), [
    'check --manifest-path ci/consumers/wv2-0_39/Cargo.toml --locked --target x86_64-pc-windows-msvc --no-default-features --features wv2-0_39',
    'check --manifest-path ci/consumers/second/Cargo.toml --locked --target x86_64-pc-windows-msvc',
    'check --manifest-path ci/consumers/second/Cargo.toml --locked --target aarch64-pc-windows-msvc',
  ]);
  assert.match(steps(entry).at(-3), /^check --manifest-path ci\/consumers\/wv2-0_39/);
  assert.deepEqual(steps({ consumerRows: [{ manifestPath: 'ci/consumers/x/Cargo.toml' }] }).at(-1), 'check --manifest-path ci/consumers/x/Cargo.toml --locked');
  // Another crate reads its own entry: it gets no row from this one, and its plan carries none.
  assert.deepEqual(steps(entry, 'rightkit-other').filter((a) => a.includes('--manifest-path')), []);
  assert.deepEqual(consumerPlan({}).consumerRows, []);
  assert.deepEqual(consumerPlan({ consumerRows: [WV2_ROW] }).consumerRows, [WV2_ROW]);
  // The rows are not tests, so no test result is demanded of them.
  assert.ok(consumerPlan({ consumerRows: [WV2_ROW] }).steps.every((s) => s.test === (s.args[0] === 'test')));
  // Batch mode lints in the batch but still runs the consumer rows per crate.
  assert.equal(consumerPlan({ consumerRows: [WV2_ROW] }, { batch: true }).steps.filter((s) => s.args.includes('--manifest-path')).length, 1);
  // A malformed row stops the plan, so a batch refuses before anything runs.
  assert.throws(() => consumerPlan({ consumerRows: [{ ...WV2_ROW, extra: true }] }), /unknown field/);
});

test('consumerRows: a cargo publish runs the row with the same evidence recording, and refuses a missing manifest, lock or dirty consumer directory', async () => {
  const gateConfig = { rust: { 'rightkit-demo': { consumerRows: [WV2_ROW] } } };
  const expected = 'cargo check --manifest-path ci/consumers/wv2-0_39/Cargo.toml --locked --target x86_64-pc-windows-msvc --no-default-features --features wv2-0_39';
  const withConsumer = (lock = true) => {
    const root = workspace({ kind: 'cargo', gateConfig });
    fs.mkdirSync(path.join(root, 'ci', 'consumers', 'wv2-0_39'), { recursive: true });
    fs.writeFileSync(path.join(root, 'ci', 'consumers', 'wv2-0_39', 'Cargo.toml'), '[package]\n');
    if (lock) fs.writeFileSync(path.join(root, 'ci', 'consumers', 'wv2-0_39', 'Cargo.lock'), '');
    return root;
  };
  const root = withConsumer();
  const h = harness(root, 'cargo');
  const result = await h.run();
  const call = h.calls.find((c) => c.args.includes('--manifest-path'));
  assert.deepEqual([call.command, call.args.join(' ')], ['cargo', expected.slice('cargo '.length)]);
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.ok(evidence.runs[0].steps.some((step) => step.command === expected && step.ok === true && step.executed === undefined));
  assert.match(evidence.runs[0].command, /cargo check --manifest-path ci\/consumers\/wv2-0_39\/Cargo\.toml --locked --target x86_64-pc-windows-msvc/);
  // A failing row refuses the publish and writes no evidence.
  const failing = harness(withConsumer(), 'cargo', { run: (command, args) => (args.includes('--manifest-path') ? { status: 101, stdout: '', stderr: 'error[E0599]' } : { status: 0, stdout: /\btest\b/.test(args.join(' ')) ? LIBTEST : '', stderr: '' }) });
  await refused(failing.run(), /step failed.*--manifest-path ci\/consumers\/wv2-0_39\/Cargo\.toml/s);
  assert.deepEqual(failing.published, []);
  assert.equal(fs.existsSync(path.join(failing.root, 'release-evidence')), false);
  // Missing lock, and an uncommitted change in the consumer directory, both refuse before any command runs.
  const noLock = harness(withConsumer(false), 'cargo');
  await refused(noLock.run(), /consumer row needs ci\/consumers\/wv2-0_39\/Cargo\.lock/);
  assert.deepEqual(noLock.calls, []);
  const dirty = harness(withConsumer(), 'cargo', { git: (args) => (args[0] === 'rev-parse' ? HEAD : args.includes('ci/consumers/wv2-0_39') ? ' M ci/consumers/wv2-0_39/Cargo.lock' : '') });
  await refused(dirty.run(), /not clean for the consumer rows/);
  assert.deepEqual(dirty.calls, []);
});

test('npm: writes schema-valid darwin-only evidence the gate accepts and publishes the packed tarball', async () => {
  const root = workspace();
  const h = harness(root, 'npm');
  const result = await h.run();
  assert.deepEqual(h.calls.map((c) => `${c.command} ${c.args.join(' ')}`).slice(0, 2), ['pnpm -r --workspace-concurrency=1 run build', 'pnpm --dir packages/demo run test']);
  assert.equal(h.envs[0].RIGHTKIT_REQUIRE_ASSETS, '1');
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.equal(result.evidencePath, path.join(root, 'release-evidence', 'gate', 'npm-rightkit-demo.json'));
  const bytes = Buffer.from('tarball-bytes');
  assert.equal(evidence.artifactSha256, crypto.createHash('sha256').update(bytes).digest('hex'));
  assert.deepEqual([evidence.schema, evidence.kind, evidence.name, evidence.version, evidence.commit], [1, 'npm', '@rightkit/demo', '1.2.3', HEAD]);
  assert.equal(evidence.hostPolicy, 'darwin-only');
  assert.match(evidence.hostPolicyReason, /local-build grant/);
  assert.equal(evidence.runs.length, 1);
  assert.deepEqual([evidence.runs[0].host.os, evidence.runs[0].operator, evidence.runs[0].failed, evidence.runs[0].passed], ['darwin', 'agent (local-build grant)', 0, 12]);
  assert.match(evidence.runs[0].command, /pnpm --dir packages\/demo run test/);
  const gate = checkPublicationGate({ kind: 'npm', name: '@rightkit/demo', version: '1.2.3', packagePath: path.join(root, 'packages/demo'), evidencePath: result.evidencePath, git: (args) => (args[0] === 'rev-parse' ? HEAD : '') });
  assert.deepEqual(gate.missing, []);
  assert.equal(h.published.length, 1);
  assert.equal(h.published[0].bytes, 'tarball-bytes');
  assert.match(h.published[0].tarball, /rightkit-demo-1\.2\.3\.tgz$/);
  assert.equal(h.published[0].evidencePath, result.evidencePath);
});

test('asset-gated unit: RIGHTKIT_REQUIRE_ASSETS=0 and the skipped count is recorded, not hidden', async () => {
  const root = workspace({ assetGated: true });
  const h = harness(root, 'npm', { run: (command, args, options) => { if (args.includes('--pack-destination')) fs.writeFileSync(path.join(args[args.indexOf('--pack-destination') + 1], 'rightkit-demo-1.2.3.tgz'), 'x'); return { status: 0, stdout: args.includes('test') ? `${LIBTEST}SKIP needs sibling\n` : '', stderr: '' }; } });
  const result = await h.run();
  const run = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8')).runs[0];
  assert.equal(run.requireAssets, '0');
  assert.equal(run.assetGated, true);
  assert.equal(run.skipped, 1);
});

test('npm: the Keychain token never reaches the output, even when npm echoes it', async () => {
  const root = workspace();
  const written = [];
  const real = process.stdout.write.bind(process.stdout);
  const spawn = (command, args) => {
    if (command === 'npm' && args[0] === 'publish') return { status: 1, stdout: `auth failed for ${TOKEN}`, stderr: '' };
    return { status: 0, stdout: '', stderr: '' };
  };
  const h = harness(root, 'npm', {
    publishGatedTarball: (info) => publishGatedTarball(info, { readToken: () => TOKEN, spawnSync: spawn, recordPublished() {}, sleep() {}, verifyTimeoutMs: 0 }),
  });
  process.stdout.write = (chunk, ...rest) => { written.push(String(chunk)); return real(chunk, ...rest); };
  let message = '';
  try { await h.run(); } catch (error) { message = error.message; } finally { process.stdout.write = real; }
  assert.match(message, /npm publish failed/);
  assert.ok(!message.includes(TOKEN));
  assert.ok(!written.join('').includes(TOKEN));
  assert.ok(!h.logs.join('\n').includes(TOKEN));
});

test('cargo: the same gate and publish flow run on the evidence it wrote', async () => {
  const root = workspace({ kind: 'cargo', gateConfig: { rust: { 'rightkit-demo': { featureSets: [['--all-features']] } } } });
  const h = harness(root, 'cargo');
  const result = await h.run();
  assert.deepEqual(h.calls.map((c) => c.args.join(' ')), [
    'test -p rightkit-demo --message-format=json-render-diagnostics --all-features --no-fail-fast -- --show-output',
    'clippy -p rightkit-demo --message-format=json-render-diagnostics --no-deps --all-targets --all-features -- -D warnings',
    'package --locked --no-verify -p rightkit-demo',
    'metadata --offline --format-version 1 --no-deps',
  ]);
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.equal(evidence.artifactSha256, crypto.createHash('sha256').update('crate-bytes').digest('hex'));
  assert.deepEqual(evidence.runs[0].features, ['--all-features']);
  const call = h.published.find((entry) => entry.crate);
  assert.equal(call.evidence, result.evidencePath);
  assert.equal(call.dryRun, false);
  assert.equal(call.trustEvidenceSteps, true);
  assert.equal(call.verifyOnPublish, true);
  assert.equal(call.offlineReadOnly, true);
  assert.deepEqual(h.published.find((entry) => entry.recorded).recorded, { registry: 'cargo', name: 'rightkit-demo', version: '1.2.3' });
});

const IGNORED = 'test result: ok. 12 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n';

test('an ignored test is refused unless the unit is named, by flag or by a written reason in ci/gate.config.json', async () => {
  const runFor = (root) => (command, args) => {
    if (command === 'cargo' && args[0] === 'metadata') {
      fs.mkdirSync(path.join(root, 'target', 'package'), { recursive: true });
      fs.writeFileSync(path.join(root, 'target', 'package', 'rightkit-demo-1.2.3.crate'), 'c');
      return { status: 0, stdout: JSON.stringify({ target_directory: path.join(root, 'target') }), stderr: '' };
    }
    return { status: 0, stdout: command === 'cargo' && args[0] === 'test' ? IGNORED : '', stderr: '' };
  };
  const plain = workspace({ kind: 'cargo' });
  await refused(harness(plain, 'cargo', { run: runFor(plain) }).run(), /1 ignored test\(s\); an ignored test is not a pass/);
  const reasoned = workspace({ kind: 'cargo', gateConfig: { rust: { 'rightkit-demo': { allowIgnored: 'the re-exec helper is ignored on purpose' } } } });
  const result = await harness(reasoned, 'cargo', { run: runFor(reasoned) }).run();
  const evidenceRun = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8')).runs[0];
  assert.deepEqual([evidenceRun.ignored, evidenceRun.allowIgnored, evidenceRun.allowIgnoredReason], [1, true, 'the re-exec helper is ignored on purpose']);
  // A blank reason is not a reason.
  const blank = workspace({ kind: 'cargo', gateConfig: { rust: { 'rightkit-demo': { allowIgnored: '  ' } } } });
  await refused(harness(blank, 'cargo', { run: runFor(blank) }).run(), /ignored test/);
});

// ---- batch mode ----
const TREE = 'b'.repeat(40);
const LAYER_FILE = '# layer 1\nrightkit-a\nrightkit-b\n\n# layer 2\nrightkit-c\n';

test('crates files and --crates parse; layers are checked or derived from the in-batch dependencies', () => {
  assert.deepEqual(parseCrateLayers(LAYER_FILE), [['rightkit-a', 'rightkit-b'], ['rightkit-c']]);
  assert.deepEqual(parseCrateLayers('\n\nrightkit-a\n\n\n\nrightkit-b # trailing\n'), [['rightkit-a'], ['rightkit-b']]);
  assert.throws(() => parseCrateLayers('Bad_Name\n'), /invalid crate name/);
  assert.throws(() => parseCrateLayers('rightkit-a\n\nrightkit-a\n'), /more than once/);
  assert.throws(() => parseCrateLayers('# nothing\n'), /names no crate/);
  const deps = { 'rightkit-c': ['rightkit-a'], 'rightkit-b': [] };
  const dependsOn = (name) => deps[name] ?? [];
  assert.deepEqual(deriveCrateLayers(['rightkit-c', 'rightkit-a', 'rightkit-b'], dependsOn), [['rightkit-a', 'rightkit-b'], ['rightkit-c']]);
  assert.throws(() => deriveCrateLayers(['rightkit-a', 'rightkit-b'], (name) => (name === 'rightkit-a' ? ['rightkit-b'] : ['rightkit-a'])), /cycle/);
  assert.deepEqual(checkCrateLayers([['rightkit-a', 'rightkit-b'], ['rightkit-c']], dependsOn), [['rightkit-a', 'rightkit-b'], ['rightkit-c']]);
  assert.throws(() => checkCrateLayers([['rightkit-a', 'rightkit-c']], dependsOn), /rightkit-c \(layer 1\) depends on rightkit-a \(layer 1\)/);
  assert.throws(() => checkCrateLayers([['rightkit-c'], ['rightkit-a']], dependsOn), /must be in an earlier layer/);
  const parsed = parseLocalPublishArgs('cargo', ['--local', '--crates', 'rightkit-a,rightkit-b', '--concurrency', '2', '--allow-ignored-units', 'rightkit-a'], '/w');
  assert.deepEqual([parsed.batch.crates, parsed.concurrency, parsed.allowIgnoredUnits, parsed.unit], [['rightkit-a', 'rightkit-b'], 2, ['rightkit-a'], undefined]);
  assert.equal(parseLocalPublishArgs('cargo', ['--local', '--crates-file', 'list.txt'], '/w').batch.file, path.resolve('/w', 'list.txt'));
  assert.equal(parseLocalPublishArgs('cargo', ['--local', '--crate', 'rightkit-a']).concurrency, 4);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crate', 'rightkit-a', '--crates', 'rightkit-b']), /cannot be combined/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crates', 'rightkit-a', '--crates-file', 'f']), /not both/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crates', 'rightkit-a,rightkit-a']), /more than once/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crates', 'rightkit-a', '--concurrency', '0']), /--concurrency/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crates', 'rightkit-a', '--concurrency', '9']), /--concurrency/);
  assert.throws(() => parseLocalPublishArgs('npm', ['--local', '--crates', 'a']), /unknown publish npm --local option/);
});

test('clippy for a batch keeps every per-crate flag: shared -p list for default features, one invocation per crate otherwise', () => {
  const features = { 'rightkit-a': [], 'rightkit-b': ['--all-features'], 'rightkit-c': ['--features', 'x'], 'rightkit-d': ['--features', 'x'], 'rightkit-e': [] };
  const invocations = batchClippyInvocations(Object.keys(features), { featuresOf: (unit) => features[unit], assetGatedOf: (unit) => unit === 'rightkit-e' });
  assert.deepEqual(invocations.map((i) => i.args.join(' ')), [
    'clippy -p rightkit-a --message-format=json-render-diagnostics --no-deps --all-targets -- -D warnings',
    'clippy -p rightkit-b --message-format=json-render-diagnostics --no-deps --all-targets --all-features -- -D warnings',
    'clippy -p rightkit-c --message-format=json-render-diagnostics --no-deps --all-targets --features x -- -D warnings',
    'clippy -p rightkit-d --message-format=json-render-diagnostics --no-deps --all-targets --features x -- -D warnings',
    'clippy -p rightkit-e --message-format=json-render-diagnostics --no-deps --all-targets -- -D warnings',
  ]);
  // Every feature set of a crate is linted: shared sets still share one -p list, other sets run per crate, duplicates collapse.
  const sets = { 'rightkit-a': [[], ['--features', 'x']], 'rightkit-b': [[], ['--all-features']], 'rightkit-c': [['--features', 'x'], ['--features', 'x']] };
  assert.deepEqual(batchClippyInvocations(Object.keys(sets), { featureSetsOf: (unit) => sets[unit] }).map((i) => i.args.join(' ')), [
    'clippy -p rightkit-a -p rightkit-b --message-format=json-render-diagnostics --no-deps --all-targets -- -D warnings',
    'clippy -p rightkit-a --message-format=json-render-diagnostics --no-deps --all-targets --features x -- -D warnings',
    'clippy -p rightkit-b --message-format=json-render-diagnostics --no-deps --all-targets --all-features -- -D warnings',
    'clippy -p rightkit-c --message-format=json-render-diagnostics --no-deps --all-targets --features x -- -D warnings',
  ]);
  const shared = batchClippyInvocations(['rightkit-a', 'rightkit-f'], {});
  assert.deepEqual(shared.map((i) => i.args.join(' ')), ['clippy -p rightkit-a -p rightkit-f --message-format=json-render-diagnostics --no-deps --all-targets -- -D warnings']);
  assert.deepEqual(localTestPlan({ kind: 'cargo', unit: 'rightkit-a', dir: 'crates/rightkit-a', config: {}, batch: true }).steps.map((s) => s.args[0]), ['test', 'check']);
});

function batchHarness(names, { flags = [], listArgs, over = {}, gateConfig = {}, testOutput = LIBTEST } = {}) {
  const root = tempRoot('rk-local-batch-test-');
  for (const name of names) fs.mkdirSync(path.join(root, 'crates', name), { recursive: true });
  fs.mkdirSync(path.join(root, 'ci'), { recursive: true });
  fs.writeFileSync(path.join(root, 'ci', 'gate.config.json'), JSON.stringify(gateConfig));
  fs.writeFileSync(path.join(root, 'ci', 'asset-gated.json'), '{}');
  const calls = []; const events = []; const logs = []; const recorded = [];
  const gitFor = (args) => (args[0] === 'rev-parse' ? (args[1] === 'HEAD' ? HEAD : TREE) : '');
  const state = { active: 0, max: 0 };
  const deps = {
    log: (line) => logs.push(line), platform: 'darwin', arch: 'arm64', now: () => '2026-10-17T00:00:00.000Z',
    localBuildGranted: () => true, git: gitFor, registryHas: async () => false,
    readCargoPackage: (dir) => ({ name: path.basename(dir), version: '1.0.0' }),
    loadWorkspaceModules: async () => ({ results, checkNpmTarball: () => ({ problems: [] }) }),
    scanSecrets: () => [],
    batchDependencies: (unit) => (unit === 'rightkit-c' ? ['rightkit-a'] : []),
    run: async (command, args) => {
      calls.push(`${command} ${args.join(' ')}`);
      if (args[0] === 'package') {
        const unit = args[args.indexOf('-p') + 1];
        fs.mkdirSync(path.join(root, 'target', 'package'), { recursive: true });
        fs.writeFileSync(path.join(root, 'target', 'package', `${unit}-1.0.0.crate`), `crate-${unit}`);
      }
      if (args[0] === 'metadata') return { status: 0, stdout: JSON.stringify({ target_directory: path.join(root, 'target') }), stderr: '' };
      return { status: 0, stdout: args[0] === 'test' ? testOutput : '', stderr: '' };
    },
    enforceGate: (options) => {
      const gate = checkPublicationGate({ ...options, git: gitFor });
      assert.deepEqual(gate.missing, []);
      return gate;
    },
    runCargoPublish: async (options) => {
      state.active += 1; state.max = Math.max(state.max, state.active);
      events.push(`start ${options.crate}`);
      await new Promise((resolve) => setTimeout(resolve, 5));
      events.push(`done ${options.crate}`);
      state.active -= 1;
      return { name: options.crate, version: '1.0.0' };
    },
    recordPublished: (entry) => recorded.push(entry.name),
    ...over,
  };
  const args = [...(listArgs ? listArgs(root) : ['--crates', names.join(',')]), '--local', '--workspace', root, ...flags];
  return { run: () => runLocalPublish('cargo', args, deps), root, calls, events, logs, recorded, state };
}

const NAMES = ['rightkit-a', 'rightkit-b', 'rightkit-c'];
const gitForBatch = (args) => (args[0] === 'rev-parse' ? (args[1] === 'HEAD' ? HEAD : TREE) : '');
const evidencePathOf = (root, name) => path.join(root, 'release-evidence', 'gate', `cargo-${name}.json`);

test('batch: fmt, clippy and the secret scan run once before any publish; every crate keeps its own tests, check, evidence and publish', async () => {
  const h = batchHarness(NAMES);
  const result = await h.run();
  assert.deepEqual(result.results.map((r) => r.name).sort(), NAMES);
  assert.equal(h.calls[0], 'cargo fmt --all --check');
  assert.equal(h.calls.filter((c) => c.includes(' fmt ')).length, 1);
  assert.deepEqual(h.calls.filter((c) => c.includes(' clippy ')), [
    'cargo clippy -p rightkit-a -p rightkit-b --message-format=json-render-diagnostics --no-deps --all-targets -- -D warnings',
    'cargo clippy -p rightkit-c --message-format=json-render-diagnostics --no-deps --all-targets -- -D warnings',
  ]);
  assert.ok(h.logs.findIndex((line) => /batch layer 1\/2/.test(line)) > h.logs.findIndex((line) => /batch secret-scan/.test(line)));
  for (const name of NAMES) {
    assert.equal(h.calls.filter((c) => c.startsWith(`cargo test -p ${name} `)).length, 1);
    assert.equal(h.calls.filter((c) => c === `cargo check -p ${name} --message-format=json-render-diagnostics --all-targets --all-features`).length, 1);
    assert.equal(h.calls.filter((c) => c.startsWith('cargo package') && c.includes(`-p ${name}`)).length, 1);
    const evidence = JSON.parse(fs.readFileSync(evidencePathOf(h.root, name), 'utf8'));
    const run = evidence.runs[0];
    assert.equal(run.commit, HEAD);
    assert.deepEqual(run.steps.map((s) => s.command.split(' ')[1]), ['test', 'check']);
    assert.deepEqual([run.batch.commit, run.batch.tree, run.batch.crates], [HEAD, TREE, NAMES]);
    assert.deepEqual(run.batch.checks.map((c) => c.name), ['fmt', 'secret-scan', 'clippy', 'clippy']);
    assert.ok(run.batch.checks.every((c) => c.ok === true && c.commit === HEAD && c.tree === TREE));
    assert.match(run.command, /cargo fmt --all --check/);
    assert.match(run.command, /cargo clippy -p rightkit-a -p rightkit-b/);
    // The gate accepts the written evidence, and it proves tests, clippy, fmt and the secret scan for this commit.
    const gate = checkPublicationGate({ kind: 'cargo', name, version: '1.0.0', packagePath: path.join(h.root, 'crates', name), evidencePath: evidencePathOf(h.root, name), git: gitForBatch });
    assert.deepEqual(gate.missing, []);
    assert.deepEqual(provenGateSteps(evidence), { tests: true, clippy: true, fmt: true, secretScan: true });
  }
  assert.deepEqual([...h.recorded].sort(), NAMES);
});

test('batch: crates of a layer overlap, and the next layer starts only after the previous one is done', async () => {
  const h = batchHarness(NAMES);
  await h.run();
  assert.equal(h.state.max, 2, 'a and b publish concurrently; c never overlaps them');
  const at = (event) => h.events.indexOf(event);
  assert.ok(at('start rightkit-c') > at('done rightkit-a') && at('start rightkit-c') > at('done rightkit-b'));
});

test('batch: --concurrency 1 serializes; a crates file fixes the layers and is checked against the dependencies', async () => {
  const serial = batchHarness(NAMES, { flags: ['--concurrency', '1'] });
  await serial.run();
  assert.equal(serial.state.max, 1);
  const filed = batchHarness(NAMES, { listArgs: (root) => { const file = path.join(root, 'layers.txt'); fs.writeFileSync(file, LAYER_FILE); return ['--crates-file', file]; } });
  await filed.run();
  assert.deepEqual(filed.events.filter((e) => e.startsWith('done')).slice(0, 2).sort(), ['done rightkit-a', 'done rightkit-b']);
  assert.equal(filed.events.at(-1), 'done rightkit-c');
  const wrong = batchHarness(NAMES, { listArgs: (root) => { const file = path.join(root, 'layers.txt'); fs.writeFileSync(file, 'rightkit-a\nrightkit-c\n\nrightkit-b\n'); return ['--crates-file', file]; } });
  await refused(wrong.run(), /must be in an earlier layer/);
  assert.deepEqual(wrong.calls, []);
});

test('batch: any failing batch check or precondition publishes nothing', async () => {
  const cases = [
    ['fmt', { run: async (command, args) => (args[0] === 'fmt' ? { status: 1, stdout: '', stderr: 'Diff in src/lib.rs' } : { status: 0, stdout: LIBTEST, stderr: '' }) }, /batch fmt failed.*Diff in src\/lib\.rs/s],
    ['clippy', { run: async (command, args) => (args[0] === 'clippy' ? { status: 101, stdout: '', stderr: 'error: unused' } : { status: 0, stdout: LIBTEST, stderr: '' }) }, /batch clippy failed.*unused/s],
    ['secret scan', { scanSecrets: (dir) => (dir.endsWith('rightkit-b') ? ['.env'] : []) }, /batch secret scan rejected: rightkit-b: \.env/],
    ['registry', { registryHas: async ({ name }) => name === 'rightkit-b' }, /already on the crates\.io registry: rightkit-b@1\.0\.0/],
    ['dirty', { git: (args) => (args[0] === 'status' ? ' M crates/rightkit-a/src/lib.rs' : gitForBatch(args)) }, /tree is not clean/],
    ['moved HEAD', { git: (() => { let heads = 0; return (args) => (args[0] === 'rev-parse' && args[1] === 'HEAD' && heads++ >= 1 ? 'c'.repeat(40) : gitForBatch(args)); })() }, /HEAD moved/],
    ['moved tree', { git: (() => { let trees = 0; return (args) => (args[0] === 'rev-parse' && args[1] !== 'HEAD' && trees++ >= 1 ? 'd'.repeat(40) : gitForBatch(args)); })() }, /differs from the batch tree/],
  ];
  for (const [label, over, pattern] of cases) {
    const h = batchHarness(['rightkit-a', 'rightkit-b'], { over });
    await refused(h.run(), pattern);
    assert.deepEqual(h.events, [], `${label}: nothing may be published`);
    assert.equal(fs.existsSync(path.join(h.root, 'release-evidence')), false, `${label}: no evidence`);
  }
});

test('batch: a crate failure stops the next layer, lets the running sibling finish and reports what published', async () => {
  const h = batchHarness(NAMES, {
    over: { runCargoPublish: async (options) => { if (options.crate === 'rightkit-a') throw new Error('upload refused'); return { name: options.crate, version: '1.0.0' }; } },
  });
  await refused(h.run(), /batch stopped in layer 1\/2; published so far: rightkit-b@1\.0\.0\n- rightkit-a: upload refused/);
  assert.equal(h.calls.some((c) => c.startsWith('cargo test -p rightkit-c ')), false, 'layer 2 never started');
});

test('batch: ignored tests are refused unless the unit is named in --allow-ignored-units', async () => {
  await refused(batchHarness(['rightkit-a'], { testOutput: IGNORED }).run(), /1 ignored test\(s\); an ignored test is not a pass/);
  const allowed = batchHarness(['rightkit-a', 'rightkit-b'], { testOutput: IGNORED, flags: ['--allow-ignored-units', 'rightkit-a,rightkit-b'] });
  await allowed.run();
  assert.equal(JSON.parse(fs.readFileSync(evidencePathOf(allowed.root, 'rightkit-a'), 'utf8')).runs[0].allowIgnored, true);
  const partly = batchHarness(['rightkit-a', 'rightkit-b'], { testOutput: IGNORED, flags: ['--allow-ignored-units', 'rightkit-a'] });
  await refused(partly.run(), /rightkit-b: .*ignored test/s);
});

test('batch: versions-file writes never overlap', async () => {
  let inside = 0; let overlapped = false;
  const h = batchHarness(['rightkit-a', 'rightkit-b'], { over: { recordPublished: async () => { inside += 1; if (inside > 1) overlapped = true; await new Promise((resolve) => setTimeout(resolve, 5)); inside -= 1; } } });
  await h.run();
  assert.equal(overlapped, false);
});

test('batch: the secret scan covers every crate directory and the receipt records it', async () => {
  const scanned = [];
  const h = batchHarness(['rightkit-a', 'rightkit-b'], { over: { scanSecrets: (dir) => { scanned.push(path.basename(dir)); return []; } } });
  await h.run();
  assert.deepEqual(scanned, ['rightkit-a', 'rightkit-b']);
  const check = JSON.parse(fs.readFileSync(evidencePathOf(h.root, 'rightkit-a'), 'utf8')).runs[0].batch.checks.find((c) => c.name === 'secret-scan');
  assert.deepEqual(check.crates, ['rightkit-a', 'rightkit-b']);
});

// ---- the async Cargo runner ----
const fakeChild = ({ out = '', err = '', code = 0, hang = false } = {}) => {
  const child = new EventEmitter();
  child.stdout = new EventEmitter(); child.stderr = new EventEmitter();
  child.kill = () => { setImmediate(() => child.emit('close', null, 'SIGKILL')); };
  if (!hang) setImmediate(() => { child.stdout.emit('data', Buffer.from(out)); child.stderr.emit('data', Buffer.from(err)); child.emit('close', code, null); });
  return child;
};

test('spawnCollect resolves status, output and errors without rejecting', async () => {
  const seen = [];
  const good = await spawnCollect('x', ['a'], { cwd: '/w', encoding: 'utf8', stdio: 'inherit' }, { spawnAsync: (command, args, options) => { seen.push(options.stdio); return fakeChild({ out: 'out', err: 'err' }); } });
  assert.deepEqual([good.status, good.stdout, good.stderr, good.error], [0, 'out', 'err', undefined]);
  assert.deepEqual(seen, [['ignore', 'pipe', 'pipe']]);
  assert.equal((await spawnCollect('x', [], {}, { spawnAsync: () => fakeChild({ code: 3 }) })).status, 3);
  assert.match((await spawnCollect('x', [], {}, { spawnAsync: () => { throw new Error('ENOENT'); } })).error.message, /ENOENT/);
  assert.match((await spawnCollect('x', [], { maxBuffer: 2 }, { spawnAsync: () => fakeChild({ out: 'abcdef' }) })).error.message, /maxBuffer/);
  assert.match((await spawnCollect('x', [], { timeout: 5 }, { spawnAsync: () => fakeChild({ hang: true }) })).error.message, /timed out/);
});

test('runCargoAsync takes the same invocation decision as runCargoSync, then runs it asynchronously', async () => {
  const spawned = [];
  const spawnAsync = async (command, args) => { spawned.push([command, ...args]); return { status: 0, stdout: 'x', stderr: '' }; };
  const linux = await runCargoAsync(['test', '-p', 'a'], { env: { PATH: '/usr/bin' } }, { platform: 'linux', exists: () => false, spawnAsync });
  assert.deepEqual([linux.status, linux.stdout, spawned], [0, 'x', [['cargo', 'test', '-p', 'a']]]);
  // The Mac never falls back to direct Cargo: no client means unavailable, and nothing is spawned.
  spawned.length = 0;
  const mac = await runCargoAsync(['test'], { env: { PATH: '/usr/bin' } }, { platform: 'darwin', exists: () => false, spawnAsync });
  assert.equal(mac.error.code, 'RK_BROKER_UNAVAILABLE');
  assert.deepEqual(spawned, []);
});

test('batchCargoExecute echoes inherited output under the crate name and leaves captured output to the caller', async () => {
  const lines = [];
  const sink = { write: (line) => lines.push(line) };
  const seen = [];
  const execute = batchCargoExecute('rightkit-a', { run: async (args, options) => { seen.push(options.timeout > 0); return { status: 0, stdout: 'one\ntwo\n', stderr: 'warn\n' }; }, out: sink, err: sink });
  const inherited = await execute('cargo', ['publish'], { cwd: '/w', stdio: 'inherit' });
  assert.equal(inherited.status, 0);
  assert.deepEqual(lines, ['[rightkit-a] one\n', '[rightkit-a] two\n', '[rightkit-a] warn\n']);
  lines.length = 0;
  const captured = await execute('cargo', ['metadata'], { cwd: '/w', stdio: 'pipe' });
  assert.equal(captured.stdout, 'one\ntwo\n');
  assert.deepEqual([lines, seen], [[], [true, true]]);
});

// ---- Windows host (win32-only policy): crates only, same gate ----
test('win32: a crate publishes from Windows with win32-only evidence the unchanged gate accepts, and no token is passed', async () => {
  const root = workspace({ kind: 'cargo', gateConfig: { rust: { 'rightkit-demo': { os: ['windows'], featureSets: [['--all-features']] } } } });
  const h = harness(root, 'cargo', { platform: 'win32', arch: 'x64' });
  const result = await h.run();
  assert.ok(h.logs.some((line) => /local-build grant, win32-only policy\)/.test(line)));
  assert.ok(!h.logs.some((line) => /darwin-only/.test(line)));
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.equal(evidence.hostPolicy, 'win32-only');
  assert.equal(evidence.hostPolicyReason, LOCAL_WIN32_HOST_POLICY_REASON);
  assert.equal(evidence.runs.length, 1);
  assert.deepEqual([evidence.runs[0].host.os, evidence.runs[0].host.arch, evidence.runs[0].operator, evidence.runs[0].failed, evidence.runs[0].passed], ['win32', 'x64', 'agent (local-build grant)', 0, 12]);
  assert.equal(evidence.artifactSha256, crypto.createHash('sha256').update('crate-bytes').digest('hex'));
  const gate = checkPublicationGate({ kind: 'cargo', name: 'rightkit-demo', version: '1.2.3', packagePath: path.join(root, 'crates/rightkit-demo'), evidencePath: result.evidencePath, git: (args) => (args[0] === 'rev-parse' ? HEAD : '') });
  assert.deepEqual(gate.missing, []);
  // The evidence proves its tests and clippy on the Windows host, so the publisher need not repeat them.
  assert.deepEqual(provenGateSteps(evidence), { tests: true, clippy: true, fmt: false, secretScan: false });
  // Windows publishes with the credential cargo stores itself: no argument carries a token, none is logged.
  assert.ok(h.calls.every((c) => !c.args.some((arg) => /--token|^cargo_/i.test(arg))));
  const call = h.published.find((entry) => entry.crate);
  assert.equal(call.evidence, result.evidencePath);
  assert.equal(call.trustEvidenceSteps, true);
  assert.ok(!('token' in call));
});

test('win32: refused without the grant, for npm (Keychain token and signed helper are Mac only) and on other hosts; nothing is written', async () => {
  const cargoRoot = workspace({ kind: 'cargo' });
  await refused(harness(cargoRoot, 'cargo', { platform: 'win32', localBuildGranted: () => false }).run(), /local-build grant/);
  await refused(harness(cargoRoot, 'cargo', { platform: 'linux' }).run(), /Mac only.*win32-only.*this host is linux/);
  assert.equal(fs.existsSync(path.join(cargoRoot, 'release-evidence')), false);
  const npmRoot = workspace();
  const npm = harness(npmRoot, 'npm', { platform: 'win32' });
  await refused(npm.run(), /npm --local needs the macOS Keychain npm token and runs on the Mac only.*this host is win32/);
  assert.deepEqual(npm.calls, []);
  assert.deepEqual(npm.published, []);
  assert.equal(fs.existsSync(path.join(npmRoot, 'release-evidence')), false);
});

test('win32: the gate still stops a failing test, a skipped test and a registry-published version on Windows', async () => {
  const root = workspace({ kind: 'cargo' });
  const failing = harness(root, 'cargo', { platform: 'win32', run: (command, args) => (args[0] === 'test' ? { status: 101, stdout: 'test result: FAILED. 1 passed; 1 failed; 0 ignored', stderr: 'boom' } : { status: 0, stdout: '', stderr: '' }) });
  await refused(failing.run(), /step failed.*cargo test -p rightkit-demo/s);
  const skipped = harness(root, 'cargo', { platform: 'win32', run: (command, args) => ({ status: 0, stdout: args[0] === 'test' ? `${LIBTEST}SKIP needs sibling\n` : '', stderr: '' }) });
  await refused(skipped.run(), /not qualified/);
  await refused(harness(root, 'cargo', { platform: 'win32', registryHas: async () => true }).run(), /already on the crates\.io registry/);
  assert.deepEqual([failing.published, skipped.published], [[], []]);
  assert.equal(fs.existsSync(path.join(root, 'release-evidence')), false);
});

test('win32: localTestPlan honours the gate config host names for each platform', () => {
  const only = (os, platform) => () => localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'x', config: { rust: { 'rightkit-demo': { os } } }, platform });
  assert.doesNotThrow(only(['windows'], 'win32'));
  assert.doesNotThrow(only(['macos', 'windows'], 'win32'));
  assert.throws(only(['macos'], 'win32'), /restricted to macos by ci\/gate\.config\.json; it cannot be qualified on this Windows host/);
  assert.throws(only(['windows'], 'darwin'), /restricted to windows.*this Mac/);
  assert.doesNotThrow(only(['macos'], 'darwin'));
});

test('win32 batch: win32-only evidence per crate with the batch receipt, gate accepted, an OS-restricted crate refused up front', async () => {
  const h = batchHarness(NAMES, { over: { platform: 'win32', arch: 'x64' } });
  const result = await h.run();
  assert.deepEqual(result.results.map((r) => r.name).sort(), NAMES);
  assert.ok(h.logs.some((line) => /cargo batch: 3 crate\(s\).*local-build grant, win32-only policy\)/.test(line)));
  for (const name of NAMES) {
    const evidence = JSON.parse(fs.readFileSync(evidencePathOf(h.root, name), 'utf8'));
    assert.equal(evidence.hostPolicy, 'win32-only');
    assert.deepEqual([evidence.runs.length, evidence.runs[0].host.os], [1, 'win32']);
    const gate = checkPublicationGate({ kind: 'cargo', name, version: '1.0.0', packagePath: path.join(h.root, 'crates', name), evidencePath: evidencePathOf(h.root, name), git: gitForBatch });
    assert.deepEqual(gate.missing, []);
    assert.deepEqual(provenGateSteps(evidence), { tests: true, clippy: true, fmt: true, secretScan: true });
  }
  const restricted = batchHarness(NAMES, { over: { platform: 'win32' }, gateConfig: { rust: { 'rightkit-b': { os: ['macos'] } } } });
  await refused(restricted.run(), /rightkit-b is restricted to macos.*this Windows host/);
  assert.deepEqual(restricted.calls, []);
});


test('Cargo JSON compiler artifacts preserve libtest counts, failures and skip evidence', () => {
  const scanner = new results.ResultScanner();
  const artifact = JSON.stringify({ reason: 'compiler-artifact', package_id: 'rightkit-demo 1.2.3', filenames: ['/scratch/target/debug/deps/demo'], executable: '/scratch/target/debug/deps/demo', fresh: true });
  scanner.push(`${artifact}\n${JSON.stringify({ reason: 'build-finished', success: true })}\ntest result: ok. 7 passed; 0 failed; 1 ignored;`);
  scanner.push(' 0 measured; 0 filtered out\n[fixture] SKIP unavailable asset\n');
  assert.deepEqual(scanner.finish(), { executed: 7, failed: 0, ignored: 1, skipped: 1, binaries: 1 });
  const failure = new results.ResultScanner();
  failure.push(`${artifact}\ntest result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n`);
  assert.equal(failure.finish().failed, 1);
});

test('structured output remains a Cargo option before libtest or clippy arguments', () => {
  const plan = localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'crates/rightkit-demo', config: { rust: { 'rightkit-demo': { alsoSerial: true } } } });
  for (const step of plan.steps) {
    const index = step.args.indexOf('--message-format=json-render-diagnostics');
    assert.ok(index > 0);
    const separator = step.args.indexOf('--');
    assert.ok(separator < 0 || index < separator);
    assert.equal(step.args.filter((arg) => arg.startsWith('--message-format')).length, 1);
  }
});

test('qualification-only requires an explicit new bundle and one cargo crate', () => {
  const base = ['--local', '--crate', 'rightkit-demo', '--qualify-only'];
  assert.throws(() => parseLocalPublishArgs('cargo', base), /requires --bundle/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crates', 'rightkit-demo', '--qualify-only', '--bundle', 'out']), /requires one --crate/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crate', 'rightkit-demo', '--bundle', 'out']), /requires --qualify-only/);
  const parsed = parseLocalPublishArgs('cargo', [...base, '--bundle', 'out'], '/scratch');
  assert.equal(parsed.qualifyOnly, true);
  assert.equal(parsed.bundleDir, path.resolve('/scratch', 'out'));
});

test('Windows qualification verifies package, exports durable bytes and evidence, and uploads nothing', async () => {
  const root = workspace({ kind: 'cargo' });
  const h = harness(root, 'cargo', { platform: 'win32', arch: 'x64' });
  const bundle = path.join(root, 'qualified');
  const argv = ['--local', '--crate', 'rightkit-demo', '--workspace', root, '--qualify-only', '--bundle', bundle];
  const result = await runLocalPublish('cargo', argv, h.deps);
  assert.equal(result.qualifiedOnly, true);
  assert.equal(fs.readFileSync(result.artifactFile, 'utf8'), 'crate-bytes');
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.equal(evidence.hostPolicy, 'win32-only');
  assert.equal(evidence.runs[0].host.os, 'win32');
  assert.ok(evidence.runs[0].steps.some((step) => step.ok && step.command === 'cargo package --locked -p rightkit-demo'));
  assert.ok(h.calls.some(({ args }) => args.join(' ') === 'package --locked -p rightkit-demo'));
  assert.equal(h.published.length, 0);
  const commandCount = h.calls.length;
  await refused(runLocalPublish('cargo', argv, h.deps), /bundle already exists/);
  assert.equal(h.calls.length, commandCount);
});

test('single-crate qualify-only with --jobs: cargo gets --jobs after the subcommand, evidence records jobs, recorded steps stay canonical', async () => {
  const root = workspace({ kind: 'cargo' });
  const h = harness(root, 'cargo', { platform: 'win32', arch: 'x64' });
  const bundle = path.join(root, 'qualified');
  const result = await runLocalPublish('cargo', ['--local', '--crate', 'rightkit-demo', '--workspace', root, '--qualify-only', '--bundle', bundle, '--jobs', '3'], h.deps);
  for (const sub of ['test', 'clippy', 'check', 'package']) {
    const calls = h.calls.filter(({ command, args }) => command === 'cargo' && args[0] === sub);
    assert.ok(calls.length > 0, sub);
    for (const { args } of calls) assert.deepEqual(args.slice(1, 3), ['--jobs', '3']);
  }
  for (const sub of ['fmt', 'metadata']) for (const { args } of h.calls.filter(({ command, args: a }) => command === 'cargo' && a[0] === sub)) assert.equal(args.includes('--jobs'), false);
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.equal(evidence.runs[0].jobs, 3);
  assert.ok(evidence.runs[0].steps.every((step) => !step.command.includes('--jobs')));
  assert.ok(evidence.runs[0].steps.some((step) => step.command === 'cargo package --locked -p rightkit-demo'));
  assert.equal(provenGateSteps(evidence).tests, true);
  assert.equal(provenGateSteps(evidence).clippy, true);
  const bad = structuredClone(evidence); bad.runs[0].jobs = 99;
  const gate = checkPublicationGate({ kind: 'cargo', name: 'rightkit-demo', version: '1.2.3', packagePath: root, evidencePath: 'x', git: (args) => (args[0] === 'rev-parse' ? HEAD : ''), readFile: () => JSON.stringify(bad) });
  assert.ok(gate.missing.some((line) => /jobs \(must be an integer from 1 to 64\)/.test(line)));
});


test('jobs bounds preserve qualification-only scope', () => {
  const argv = ['--local', '--crate', 'rightkit-demo', '--qualify-only', '--bundle', 'out'];
  for (const bad of ['0', '-1', '65', '2.5', 'x', '', '1e1']) assert.throws(() => parseLocalPublishArgs('cargo', [...argv, '--jobs', bad]), /--jobs must be/);
  assert.throws(() => parseLocalPublishArgs('cargo', ['--local', '--crate', 'rightkit-demo', '--jobs', '4']), /--jobs requires --qualify-only/);
  assert.equal(parseLocalPublishArgs('cargo', [...argv, '--jobs', '12']).jobs, 12);
});
