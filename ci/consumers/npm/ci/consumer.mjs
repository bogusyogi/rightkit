import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const recipeId = 'ci-cli-manifest-render-shards-consumer';
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
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  // @rightkit/ci has no `exports` and no `main`: its public surface is the `right-git` bin (cli/right-git.mjs)
  // and the module files its `files` list ships (`*.mjs`, `policy`, `templates`). Resolve the installed files
  // directly; never import the package root.
  const fixtureRoot = path.dirname(fileURLToPath(import.meta.url));
  const packageDir = path.join(fixtureRoot, 'node_modules', '@rightkit', 'ci');
  const binPath = path.join(packageDir, 'cli', 'right-git.mjs');
  const binUrl = pathToFileURL(binPath).href;
  const moduleUrl = file => pathToFileURL(path.join(packageDir, ...file.split('/'))).href;
  const entries = {
    '@rightkit/ci/cli/right-git.mjs': binUrl,
    '@rightkit/ci/manifest.mjs': moduleUrl('manifest.mjs'),
    '@rightkit/ci/sync.mjs': moduleUrl('sync.mjs'),
    '@rightkit/ci/install.mjs': moduleUrl('install.mjs'),
    '@rightkit/ci/policy/visibility-gate.mjs': moduleUrl('policy/visibility-gate.mjs'),
  };
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/ci', rootResolvedUrl: binUrl, entries,
  });
  result.artifact = provenance.artifact;
  const packageRoot = realpathSync(path.resolve(path.dirname(realpathSync(fileURLToPath(binUrl))), '..'));
  const installed = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };
  const load = specifier => import(provenance.entries[specifier]);

  // The repository the fixtures render for. It is never created: every scenario reads only, and the last one
  // proves nothing was written.
  const repoPath = path.join(fixtureRoot, 'widgets');
  const rustHybrid = (extra = {}) => ({
    schemaVersion: 1, compile: 'github-actions-only', lanes: ['ci'], matrix: { os: ['windows-2025', 'macos-15'], node: ['24'] },
    packageManager: 'pnpm@11.25.0', templateVersion: '1.8.0', profile: 'rust-hybrid', gate: { path: 'scripts/gate.sh' }, ...extra,
  });
  const shards = [{ id: 'workspace' }, { id: 'linters' }, { id: 'rust-tools' }];
  const shardTokens = /matrix\.shard|ci-summary|^ {2}quick:|--group|--quick|QUICK_RESULT/m;
  const cli = args => spawnSync(process.execPath, [binPath, ...args], { cwd: fixtureRoot, encoding: 'utf8', timeout: 120_000, windowsHide: true });

  await run('installed-bin-manifest-bound-to-archive', async () => {
    assert.equal(installed.name, '@rightkit/ci');
    assert.equal(installed.version, version);
    assert.equal(installed.type, 'module');
    assert.equal('exports' in installed, false);
    assert.equal('main' in installed, false);
    assert.deepEqual(installed.bin, { 'right-git': 'cli/right-git.mjs' });
    for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies']) assert.equal(installed[field], undefined);
    assert.ok(readFileSync(path.join(packageRoot, 'cli', 'right-git.mjs'), 'utf8').startsWith('#!/usr/bin/env node'));
    // The `files` allowlist must ship every directory the bin and the renderer read at run time.
    for (const file of ['policy/visibility-gate.mjs', 'templates/ci.yml', 'templates/profiles/rust-hybrid/ci.yml', 'manifest.schema.json']) assert.ok(existsSync(path.join(packageRoot, ...file.split('/'))), file);
    return { bin: 'right-git', binTarget: 'cli/right-git.mjs', exportsPresent: false, dependencies: 0, templatesShipped: true };
  });

  await run('installed-cli-version-and-usage-smoke', async () => {
    const smoke = cli(['--version']);
    assert.equal(smoke.status, 0);
    assert.equal(String(smoke.stdout ?? '').trim(), version);
    const help = cli(['--help']);
    assert.equal(help.status, 0);
    assert.match(String(help.stdout), /^right-git <command> \[options\]/);
    for (const command of ['install', 'sync', 'drift', 'adopt', 'uninstall', 'artifacts']) assert.match(String(help.stdout), new RegExp(`^  ${command} `, 'm'));
    assert.equal(cli([]).status, 1);
    assert.equal(cli(['no-such-command']).status, 2);
    // A command without its repository argument prints usage and exits before any visibility or network check.
    assert.equal(cli(['install']).status, 2);
    assert.equal(cli(['sync']).status, 2);
    return { command: 'right-git --version', exitCode: 0, stdout: version, helpListsCommands: 6, noArgumentsExit: 1, unknownCommandExit: 2, missingRepositoryExit: 2 };
  });

  await run('manifest-validation-accepts-minimal-rust-hybrid-and-refuses-bad', async () => {
    const { validateManifest, KNOWN_LANES } = await load('@rightkit/ci/manifest.mjs');
    assert.ok(KNOWN_LANES.includes('ci'));
    assert.deepEqual(validateManifest(rustHybrid()), { valid: true, errors: [] });
    const refuse = (mutate, pattern) => {
      const manifest = rustHybrid(); mutate(manifest);
      const checked = validateManifest(manifest);
      assert.equal(checked.valid, false);
      assert.match(checked.errors.join('\n'), pattern);
    };
    refuse(manifest => { manifest.lanes = ['no-such-lane']; }, /unknown lane/);
    refuse(manifest => { manifest.schemaVersion = 2; }, /schemaVersion must be exactly 1/);
    refuse(manifest => { manifest.compile = 'local'; }, /compile must be exactly 'github-actions-only'/);
    refuse(manifest => { delete manifest.gate; }, /requires gate\.path/);
    refuse(manifest => { manifest.matrix.os = ['${{ matrix.x }}']; }, /matrix\.os must be a non-empty array of safe runner/);
    for (const bad of [null, []]) assert.deepEqual(validateManifest(bad), { valid: false, errors: ['manifest must be a JSON object'] });
    return { accepted: 'rust-hybrid ci', refused: 7 };
  });

  await run('render-lanes-render-ci-yml-for-manifests', async () => {
    const { renderLanes } = await load('@rightkit/ci/sync.mjs');
    const rendered = renderLanes(rustHybrid(), repoPath);
    assert.deepEqual(rendered.map(({ lane, filename }) => ({ lane, filename })), [{ lane: 'ci', filename: 'ci.yml' }]);
    const ci = rendered[0].content;
    assert.ok(ci.startsWith('# Managed by right-git'));
    assert.ok(!ci.includes('\r'));
    assert.match(ci, /^name: widgets-ci$/m);
    assert.match(ci, /^permissions:\n {2}contents: read$/m);
    assert.match(ci, /^ {2}private-repo-guard:$/m);
    // 0.3.2: one door. The ci job runs the gate itself, never the repository's gate script (gate.path only filters Rust changes).
    assert.match(ci, /^ {6}- run: pnpm exec rightkit-gate --to compile$/m);
    assert.doesNotMatch(ci, /bash \.\/scripts\/gate\.sh/);
    assert.doesNotMatch(ci, /\$\{[A-Z][A-Z0-9_]*\}/);
    const uses = [...ci.matchAll(/^\s*(?:- )?uses: (\S+)/gm)].map(match => match[1]);
    assert.ok(uses.length > 0);
    for (const action of uses) assert.match(action, /@[0-9a-f]{40}$/);
    assert.equal(renderLanes(rustHybrid(), repoPath)[0].content, ci);
    const nodeLanes = ['ci', 'qualification', 'package-smoke'];
    const node = renderLanes({ schemaVersion: 1, compile: 'github-actions-only', lanes: nodeLanes, matrix: { os: ['ubuntu-latest'], node: ['24'] }, packageManager: 'pnpm@11.25.0', templateVersion: '1.8.0' }, repoPath);
    assert.deepEqual(node.map(({ lane }) => lane), nodeLanes);
    assert.deepEqual(node.map(({ filename }) => filename), ['ci.yml', 'qualification.yml', 'package-smoke.yml']);
    for (const { content } of node) assert.ok(content.startsWith('# Managed by right-git'));
    return { rustHybridLanes: 1, nodeLanes: node.length, actionsPinnedToCommit: uses.length, deterministic: true };
  });

  await run('gate-shards-refused-ci-runs-one-door', async () => {
    const { validateManifest } = await load('@rightkit/ci/manifest.mjs');
    const errors = gate => validateManifest(rustHybrid({ gate })).errors.join('\n');
    assert.match(errors({ path: 'scripts/gate.sh', quick: true, shards }), /gate\.shards and gate\.quick are gone/);
    assert.match(errors({ path: 'scripts/gate.sh', shards }), /gate\.shards and gate\.quick are gone/);
    assert.match(errors({ path: 'scripts/gate.sh', quick: true }), /gate\.shards and gate\.quick are gone/);
    return { validationRefusals: 3 };
  });

  await run('gate-without-shards-renders-no-shard-tokens', async () => {
    const { renderLanes } = await load('@rightkit/ci/sync.mjs');
    for (const extra of [{}, { ciAllOs: true }, { ciAllOs: true, qaEvidencePath: 'qa-out' }]) {
      const ci = renderLanes(rustHybrid(extra), repoPath)[0].content;
      assert.doesNotMatch(ci, shardTokens);
      assert.match(ci, /^ {6}- run: pnpm exec rightkit-gate --to compile$/m);
      assert.doesNotMatch(ci, /bash \.\/scripts\/gate\.sh/);
    }
    return { manifests: 3, shardTokens: 0 };
  });

  await run('private-and-unconfirmed-repositories-refused-without-writing', async () => {
    const { installCommand } = await load('@rightkit/ci/install.mjs');
    const { planSync } = await load('@rightkit/ci/sync.mjs');
    const { parseOwnerRepo } = await load('@rightkit/ci/policy/visibility-gate.mjs');
    // Every process and network call is injected: the scenario never starts gh or git and never reaches a network.
    const offline = async () => { throw new Error('offline fixture'); };
    const gh = visibility => async command => { if (command !== 'gh') throw new Error('offline fixture'); return { stdout: JSON.stringify({ visibility }) }; };
    for (const visibility of ['private', 'internal']) {
      const refused = await installCommand(repoPath, { lanes: ['ci'] }, { execFile: gh(visibility), fetch: offline });
      assert.equal(refused.ok, false);
      assert.equal(refused.stage, 'visibility');
      assert.match(refused.errors.join('\n'), /public repos only \(invariant I1\)/);
    }
    const unknown = await installCommand(repoPath, { lanes: ['ci'] }, { execFile: offline, fetch: offline });
    assert.equal(unknown.ok, false);
    assert.equal(unknown.stage, 'visibility');
    assert.match(unknown.errors.join('\n'), /fail closed/);
    const plan = await planSync(repoPath, { execFile: gh('public'), fetch: offline }, rustHybrid());
    assert.equal(plan.ok, true);
    assert.equal(plan.stage, 'planned');
    assert.deepEqual(plan.changes.map(({ lane, filename, action }) => ({ lane, filename, action })), [{ lane: 'ci', filename: 'ci.yml', action: 'create' }]);
    assert.deepEqual(parseOwnerRepo('https://github.com/o/r.git'), { owner: 'o', repo: 'r' });
    assert.equal(parseOwnerRepo('https://github.com.evil.io/o/r'), null);
    assert.equal(parseOwnerRepo('https://github.com@evil.io/o/r'), null);
    assert.equal(existsSync(repoPath), false);
    return { refusedVisibilities: ['private', 'internal', 'unknown'], publicPlanCreates: 1, hostMatchExact: true, filesWritten: 0 };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
