import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { pathToFileURL } from 'node:url';
import { checkPublicationGate, publishGatedTarball } from './publication-gate.mjs';
import { LocalPublishError, localTestPlan, parseLocalPublishArgs, runLocalPublish } from './local-publish.mjs';

const HEAD = 'a'.repeat(40);
const LIBTEST = 'test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n';
const TOKEN = 'npm_FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE';
const results = await import(pathToFileURL(path.resolve(import.meta.dirname, '../../scripts/ci/test-results.mjs')).href);

function workspace({ kind = 'npm', assetGated = false, gateConfig = {} } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-local-publish-test-'));
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

test('refuses when no test executed, or a skip outside an asset-gated unit', async () => {
  const root = workspace();
  const quiet = harness(root, 'npm', { run: (command, args) => { if (args.includes('--pack-destination')) throw new Error('must not pack'); return { status: 0, stdout: '', stderr: '' }; } });
  await refused(quiet.run(), /not qualified.*no test executed/s);
  const skipping = harness(root, 'npm', { run: (command, args) => ({ status: 0, stdout: args.includes('test') ? `${LIBTEST}SKIP needs chrome\n` : '', stderr: '' }) });
  await refused(skipping.run(), /skipped/);
  assert.equal(fs.existsSync(path.join(root, 'release-evidence')), false);
});

test('the test plan mirrors scripts/ci/gate.mjs (feature sets, serial run, clippy on the first set)', () => {
  const config = { rust: { 'rightkit-demo': { featureSets: [['--all-features'], ['--features', 'async']], alsoSerial: true } } };
  const plan = localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'crates/rightkit-demo', config });
  assert.deepEqual(plan.steps.map((s) => s.args.join(' ')), [
    'test -p rightkit-demo --all-features --no-fail-fast -- --show-output',
    'test -p rightkit-demo --all-features --no-fail-fast -- --test-threads=1 --show-output',
    'test -p rightkit-demo --features async --no-fail-fast -- --show-output',
    'test -p rightkit-demo --features async --no-fail-fast -- --test-threads=1 --show-output',
    'clippy -p rightkit-demo --no-deps --all-targets --all-features -- -D warnings',
  ]);
  assert.deepEqual(plan.featureLabels, ['--all-features', '--features async']);
  assert.throws(() => localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'x', config: { rust: { 'rightkit-demo': { os: ['windows'] } } } }), /restricted to windows/);
  assert.throws(() => localTestPlan({ kind: 'npm', unit: '@rightkit/demo', dir: 'packages/demo', manifest: { scripts: {} } }), /no test script/);
});

test('the plan compiles every feature: --all-features by default, explicit sets for exclusive features, none when already tested', () => {
  const plan = (entry) => localTestPlan({ kind: 'cargo', unit: 'rightkit-demo', dir: 'crates/rightkit-demo', config: { rust: { 'rightkit-demo': entry } } }).steps.map((s) => s.args.join(' ')).filter((a) => a.startsWith('check'));
  assert.deepEqual(plan({}), ['check -p rightkit-demo --all-targets --all-features']);
  assert.deepEqual(plan({ checkFeatureSets: [['--features', 'a'], ['--features', 'b']] }), ['check -p rightkit-demo --all-targets --features a', 'check -p rightkit-demo --all-targets --features b']);
  assert.deepEqual(plan({ featureSets: [['--all-features']] }), []);
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
    'test -p rightkit-demo --all-features --no-fail-fast -- --show-output',
    'clippy -p rightkit-demo --no-deps --all-targets --all-features -- -D warnings',
    'package --locked -p rightkit-demo',
    'metadata --format-version 1 --no-deps',
  ]);
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.equal(evidence.artifactSha256, crypto.createHash('sha256').update('crate-bytes').digest('hex'));
  assert.deepEqual(evidence.runs[0].features, ['--all-features']);
  const call = h.published.find((entry) => entry.crate);
  assert.equal(call.evidence, result.evidencePath);
  assert.equal(call.dryRun, false);
  assert.deepEqual(h.published.find((entry) => entry.recorded).recorded, { registry: 'cargo', name: 'rightkit-demo', version: '1.2.3' });
});
