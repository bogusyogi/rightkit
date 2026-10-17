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
import { LocalPublishError, batchCargoExecute, batchClippyInvocations, checkCrateLayers, deriveCrateLayers, localTestPlan, parseCrateLayers, parseLocalPublishArgs, runLocalPublish } from './local-publish.mjs';
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
    'package --locked --no-verify -p rightkit-demo',
    'metadata --format-version 1 --no-deps',
  ]);
  const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
  assert.equal(evidence.artifactSha256, crypto.createHash('sha256').update('crate-bytes').digest('hex'));
  assert.deepEqual(evidence.runs[0].features, ['--all-features']);
  const call = h.published.find((entry) => entry.crate);
  assert.equal(call.evidence, result.evidencePath);
  assert.equal(call.dryRun, false);
  assert.equal(call.trustEvidenceSteps, true);
  assert.equal(call.verifyOnPublish, true);
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
    'clippy -p rightkit-a --no-deps --all-targets -- -D warnings',
    'clippy -p rightkit-b --no-deps --all-targets --all-features -- -D warnings',
    'clippy -p rightkit-c --no-deps --all-targets --features x -- -D warnings',
    'clippy -p rightkit-d --no-deps --all-targets --features x -- -D warnings',
    'clippy -p rightkit-e --no-deps --all-targets -- -D warnings',
  ]);
  const shared = batchClippyInvocations(['rightkit-a', 'rightkit-f'], {});
  assert.deepEqual(shared.map((i) => i.args.join(' ')), ['clippy -p rightkit-a -p rightkit-f --no-deps --all-targets -- -D warnings']);
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
    'cargo clippy -p rightkit-a -p rightkit-b --no-deps --all-targets -- -D warnings',
    'cargo clippy -p rightkit-c --no-deps --all-targets -- -D warnings',
  ]);
  assert.ok(h.logs.findIndex((line) => /batch layer 1\/2/.test(line)) > h.logs.findIndex((line) => /batch secret-scan/.test(line)));
  for (const name of NAMES) {
    assert.equal(h.calls.filter((c) => c.startsWith(`cargo test -p ${name} `)).length, 1);
    assert.equal(h.calls.filter((c) => c === `cargo check -p ${name} --all-targets --all-features`).length, 1);
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
