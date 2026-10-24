import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'outdated-check-apply-gate-land-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted error.
let stage = 'artifact-provenance';
let scratch = null;
try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  assert(path.isAbsolute(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert(['packed', 'fetched'].includes(resolution));
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/outdated');
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/outdated', rootResolvedUrl: root,
    entries: { '@rightkit/outdated': root },
  });
  result.artifact = provenance.artifact;
  const packageRoot = realpathSync(path.resolve(path.dirname(realpathSync(fileURLToPath(root))), '..'));
  const installed = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const dependency = specifier => import(fixtureEntry(import.meta.url, import.meta.resolve(specifier)));
  const outdated = await import(provenance.entries['@rightkit/outdated']);
  const { parse: parseToml } = await dependency('smol-toml');
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };

  // ---- shared fixtures. Every file and repository below lives in one private temp directory that is removed at the end.
  scratch = realpathSync.native(mkdtempSync(path.join(os.tmpdir(), 'rk-outdated-consumer-')));
  const NODE = process.execPath;
  const binPath = path.join(packageRoot, 'dist', 'bin.js');
  // Git stays hermetic: no user or system configuration, a fixed identity, no prompts. An empty file, not the null device.
  const gitConfig = path.join(scratch, 'gitconfig');
  writeFileSync(gitConfig, '');
  Object.assign(process.env, {
    GIT_CONFIG_GLOBAL: gitConfig, GIT_CONFIG_NOSYSTEM: '1', GIT_TERMINAL_PROMPT: '0',
    GIT_AUTHOR_NAME: 'Consumer', GIT_AUTHOR_EMAIL: 'consumer@example.invalid', GIT_COMMITTER_NAME: 'Consumer', GIT_COMMITTER_EMAIL: 'consumer@example.invalid',
  });
  let counter = 0;
  const makeDir = label => { const dir = path.join(scratch, `${label}-${++counter}`); mkdirSync(dir, { recursive: true }); return dir; };
  const writeTree = (dir, files) => {
    for (const [rel, content] of Object.entries(files)) {
      const file = path.join(dir, ...rel.split('/'));
      mkdirSync(path.dirname(file), { recursive: true });
      writeFileSync(file, typeof content === 'string' ? content : `${JSON.stringify(content, null, 2)}\n`);
    }
    return dir;
  };
  const tree = (label, files) => writeTree(makeDir(label), files);
  const git = (dir, ...args) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  const gitRepo = (label, files) => { const dir = tree(label, files); git(dir, 'init', '-q', '-b', 'main'); git(dir, 'add', '-A'); git(dir, 'commit', '-q', '-m', 'init'); return dir; };
  const read = (dir, rel) => readFileSync(path.join(dir, ...rel.split('/')), 'utf8');
  const sha = bytes => createHash('sha256').update(bytes).digest('hex');
  const ok = (stdout = '', stderr = '') => ({ code: 0, signal: null, timedOut: false, durationMs: 1, stdout, stderr });
  const failed = (stderr = 'boom', code = 1) => ({ code, signal: null, timedOut: false, durationMs: 1, stdout: '', stderr });
  // A run function that answers chosen commands and hands every other one (git, node) to the package's own runner.
  const fakeRun = intercept => {
    const calls = [];
    const runFn = async (argv, options) => { calls.push(argv); return (await intercept(argv, options)) ?? outdated.runCommand(argv, options); };
    return { run: runFn, calls };
  };
  const mockFetch = routes => {
    const calls = [];
    const fetch = async (url, init) => {
      calls.push(url);
      const route = routes[url];
      if (route === undefined) return new Response('not found', { status: 404 });
      const value = typeof route === 'function' ? route(url, init) : route;
      if (value instanceof Response) return value;
      if (typeof value === 'string' || value instanceof Uint8Array) return new Response(value, { status: 200 });
      return new Response(JSON.stringify(value), { status: 200, headers: { 'content-type': 'application/json' } });
    };
    return { fetch, calls };
  };
  const noExec = async () => ({ stdout: '{}' });
  const manifestOf = (sources, policy = { default: 'auto-land' }, gates) => ({ schemaVersion: 1, policy, ...(gates ? { gates } : {}), sources });
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const A64 = 'a'.repeat(64);
  const B64 = 'b'.repeat(64);
  const NOW = new Date('2026-10-11T00:00:00Z');
  const lockText = packages => `# This file is automatically @generated by Cargo.\nversion = 4\n\n${Object.entries(packages).map(([name, version]) => `[[package]]\nname = "${name}"\nversion = "${version}"\n`).join('\n')}`;
  // Stands in for `rustright cargo update -p <name> --precise <version>`: it moves one package in Cargo.lock.
  const cargoUpdate = (repo, failFor = []) => argv => {
    if (argv[0] !== 'rustright') return undefined;
    assert.deepEqual(argv.slice(0, 4), ['rustright', 'cargo', 'update', '-p']);
    const name = argv[4];
    const precise = argv[argv.indexOf('--precise') + 1];
    if (failFor.includes(name)) return failed(`error: package ID specification \`${name}\` did not match any packages`);
    const file = path.join(repo, 'Cargo.lock');
    writeFileSync(file, readFileSync(file, 'utf8').replace(new RegExp(`(name = "${name}"\\nversion = ")[^"]+`), `$1${precise}`));
    return ok();
  };

  // ---- 1. The installed package, its closure and its exact public surface.
  await run('installed-manifest-closure-and-exact-export-surface', async () => {
    assert.equal(installed.name, '@rightkit/outdated');
    assert.equal(installed.version, version);
    assert.equal(installed.type, 'module');
    assert.equal(installed.main, 'dist/index.js');
    assert.equal(installed.types, 'dist/index.d.ts');
    assert.deepEqual(installed.exports, { '.': { types: './dist/index.d.ts', import: './dist/index.js' } });
    // The packer may or may not strip the leading "./" of a bin target; either spelling is the same file.
    assert.deepEqual(Object.fromEntries(Object.entries(installed.bin).map(([key, target]) => [key, target.replace(/^\.\//, '')])), { 'right-update': 'dist/bin.js' });
    assert.deepEqual(installed.rightkit, { publishCheck: { binSmoke: { 'right-update': ['--version'] } } });
    assert.equal(installed.sideEffects, false);
    assert.match(installed.engines.node, /^>=26\./);
    for (const field of ['optionalDependencies', 'peerDependencies']) assert.equal(installed[field], undefined);
    assert.deepEqual(Object.keys(installed.dependencies).sort(), ['@rightkit/ci', 'smol-toml']);
    assert.equal(installed.dependencies['smol-toml'], '1.9.1');
    assert.ok(readFileSync(binPath, 'utf8').startsWith('#!/usr/bin/env node'));

    // The two runtime dependencies are the reviewed closure, and the package under test uses those very copies.
    const consumerManifest = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8'));
    const packageDirOf = (url, name) => {
      let dir = path.dirname(realpathSync(fileURLToPath(url)));
      for (;;) {
        const file = path.join(dir, 'package.json');
        if (existsSync(file) && JSON.parse(readFileSync(file, 'utf8')).name === name) return dir;
        const parent = path.dirname(dir);
        assert.notEqual(parent, dir, `${name} was not found above its entry`);
        dir = parent;
      }
    };
    const closureCopies = {};
    for (const [name, specifier] of [['@rightkit/ci', '@rightkit/ci/drift.mjs'], ['smol-toml', 'smol-toml']]) {
      const dir = packageDirOf(fixtureEntry(import.meta.url, import.meta.resolve(specifier)), name);
      assert.equal(JSON.parse(readFileSync(path.join(dir, 'package.json'), 'utf8')).version, consumerManifest.dependencies[name]);
      const sibling = path.resolve(packageRoot, '..', '..', ...name.split('/'));
      assert.equal(realpathSync(sibling), dir, `${name} used by the package is the reviewed copy`);
      closureCopies[name] = consumerManifest.dependencies[name];
    }

    const names = Object.keys(outdated).sort();
    assert.deepEqual(names, [
      'DEFAULT_HELD_PATTERN', 'LAND_MODES', 'MANIFEST_RELATIVE_PATH', 'PLATFORMS', 'POLICIES', 'RIGHTKIT_VERSIONS_DEFAULT', 'SCHEMA_VERSION', 'USER_AGENT',
      'artifactRoot', 'buildReport', 'bumpedRequirement', 'cargoDependencies', 'checkSources', 'commitMessage', 'compareVersions', 'decideBump', 'deliveryDir',
      'exitCodeFor', 'expandGlob', 'fetchJson', 'fetchText', 'fillPlaceholders', 'findVersionRefs', 'hostPlatform', 'isBreaking', 'isoDuration', 'launchdLabel',
      'loadManifest', 'mapLimit', 'parseLatest', 'parsePinned', 'parseRequirement', 'parseSwiftPackage', 'parseSwiftProjectYml', 'pnpmDependencies',
      'renderLaunchdPlist', 'renderTaskXml', 'replaceCargoVersion', 'resolvePolicy', 'rewriteToolInManifest', 'runApply', 'runCheck', 'runCli', 'runCommand',
      'runDiscard', 'runGateCommand', 'runLand', 'runSchedule', 'safeManifestGlob', 'sourceManifests', 'validateManifest', 'windowsCreateArgs',
      'windowsTaskArguments', 'windowsTaskName', 'writeHeldReport',
    ]);
    const constants = {
      DEFAULT_HELD_PATTERN: '/sign|secur|licen|credential/', LAND_MODES: ['commit', 'push'], MANIFEST_RELATIVE_PATH: '.rightkit/update.json',
      PLATFORMS: ['macos-arm64', 'macos-x86_64', 'windows-x86_64', 'windows-arm64', 'linux-x86_64'], POLICIES: ['auto-land', 'agent-pr', 'held'],
      RIGHTKIT_VERSIONS_DEFAULT: 'packages/release/rightkit-versions.json', SCHEMA_VERSION: 1,
    };
    for (const [name, value] of Object.entries(constants)) assert.deepEqual(outdated[name], value, name);
    assert.match(outdated.USER_AGENT, /^right-update\/\d+\.\d+\.\d+ \(\+https:\/\/github\.com\/[\w-]+\/rightkit\)$/);
    for (const name of names) if (!(name in constants) && name !== 'USER_AGENT') assert.equal(typeof outdated[name], 'function', name);

    // The type declarations ship and declare every runtime export and the public types.
    const indexTypes = readFileSync(path.join(packageRoot, 'dist', 'index.d.ts'), 'utf8');
    const valueNames = [...indexTypes.matchAll(/^export \{([^}]+)\} from "\.\/[\w-]+\.js";$/gm)].flatMap(match => match[1].split(',').map(name => name.trim()));
    const constNames = [...readFileSync(path.join(packageRoot, 'dist', 'types.d.ts'), 'utf8').matchAll(/^export declare const (\w+)/gm)].map(match => match[1]);
    assert.deepEqual([...valueNames, ...constNames].sort(), names);
    assert.match(indexTypes, /^export \* from "\.\/types\.js";$/m);
    const typeNames = [...indexTypes.matchAll(/^export type \{([^}]+)\} from/gm)].flatMap(match => match[1].split(',').map(name => name.trim())).sort();
    assert.deepEqual(typeNames, [
      'ApplyChange', 'ApplyDeps', 'ApplyOptions', 'ApplyOutcome', 'ApplyReport', 'CheckDeps', 'CliDeps', 'CliIo', 'DiscardOutcome', 'DriftFn', 'ExecFn', 'ExecResult',
      'FetchLike', 'GateDeps', 'GateOutcome', 'GateReport', 'LandDeps', 'LandOutcome', 'ManifestValidation', 'Placeholders', 'RunFn', 'RunOptions', 'RunResult',
      'ScheduleDeps', 'ScheduleResult', 'ScheduleSpec', 'ToolStage', 'Unapplied',
    ]);

    // The shipped JSON schema describes the same top level the validator enforces.
    const schema = JSON.parse(readFileSync(path.join(packageRoot, 'update.schema.json'), 'utf8'));
    assert.equal(schema.additionalProperties, false);
    assert.deepEqual(schema.required, ['schemaVersion', 'sources', 'policy']);
    assert.deepEqual(Object.keys(schema.properties).sort(), ['gates', 'policy', 'schemaVersion', 'sources']);
    assert.match(outdated.validateManifest({ ...manifestOf([]), extra: 1 }).errors.join(' '), /manifest: unknown field extra/);
    return { exports: ['.'], bin: 'right-update', moduleExports: names.length, publicTypes: typeNames.length, closure: closureCopies };
  });

  // ---- 2. The update manifest: validation, loading and its error messages.
  await run('manifest-validation-and-loading', async () => {
    const tool = (extra = {}) => ({
      kind: 'tool', name: 'helper', pin: '1.4.2',
      assets: { 'macos-arm64': { asset: 'helper-macos', checksum: `sha256:${A64}` }, 'linux-x86_64': { asset: 'helper.tar.gz', checksum: `sha256:${B64}`, extract: { member: 'bin/helper' }, maxBytes: 1024 } },
      upstream: { type: 'github-release', repo: 'acme/helper' },
      journey: { command: ['node', 'journey.mjs', '{candidate}', '{out}', '{delivery}', '{model}'], report: '{out}/report.json', inputs: { case: 'smoke' }, model: 'model-x', timeoutSec: 60 },
      ...extra,
    });
    const complete = {
      schemaVersion: 1,
      policy: { default: 'auto-land', land: 'push', held: ['pinned-name'], tier: { held: ['/^ffmpeg/', 'exact-name'] } },
      gates: [{ name: 'unit', command: ['pnpm', 'test'], cwd: 'packages/x', timeoutSec: 600 }, { name: 'lint', command: ['pnpm', 'lint'] }],
      sources: [
        { kind: 'cargo', manifest: 'Cargo.toml' }, { kind: 'cargo', glob: 'crates/*/Cargo.toml', policy: 'agent-pr' },
        { kind: 'pnpm', manifest: 'package.json' }, { kind: 'pnpm', glob: 'packages/*/package.json' },
        { kind: 'swiftpm', manifest: 'Package.swift' }, { kind: 'swiftpm', manifest: 'apps/app/project.yml' },
        { kind: 'rightkit' }, { kind: 'rightkit', versions: 'versions.json', policy: 'held' },
        tool(), tool({ name: 'mirrored', upstream: { type: 'url', latest: 'https://example.invalid/latest.txt', download: 'https://example.invalid/{version}/{asset}' } }),
      ],
    };
    const accepted = outdated.validateManifest(complete);
    assert.deepEqual(accepted.errors, []);
    assert.equal(accepted.valid, true);
    assert.deepEqual(accepted.manifest, complete);

    const withSource = source => manifestOf([source]);
    const refusals = [
      [null, /manifest must be a JSON object/],
      [[], /manifest must be a JSON object/],
      [{ ...manifestOf([]), schemaVersion: 2 }, /schemaVersion must be exactly 1/],
      [{ ...manifestOf([]), surprise: true }, /manifest: unknown field surprise/],
      [{ schemaVersion: 1, sources: [] }, /policy must be an object/],
      [manifestOf([], { default: 'sometimes' }), /policy\.default must be one of auto-land, agent-pr, held/],
      [manifestOf([], { default: 'held', land: 'force' }), /policy\.land must be one of commit, push/],
      [manifestOf([], { default: 'held', held: 'x' }), /policy\.held must be an array of dependency names/],
      [manifestOf([], { default: 'held', tier: { held: ['/(/'] } }), /policy\.tier\.held has an invalid regex: \/\(\//],
      [{ ...manifestOf([]), sources: {} }, /sources must be an array/],
      [withSource('x'), /sources\[0\] must be an object/],
      [withSource({ kind: 'maven' }), /sources\[0\]: unknown kind "maven" \(expected cargo, pnpm, swiftpm, rightkit, tool\)/],
      [withSource({ kind: 'cargo', manifest: 'Cargo.toml', glob: 'a/Cargo.toml' }), /set exactly one of manifest and glob/],
      [withSource({ kind: 'cargo' }), /set exactly one of manifest and glob/],
      [withSource({ kind: 'cargo', manifest: '../Cargo.toml' }), /manifest must be a relative path/],
      [withSource({ kind: 'cargo', manifest: path.resolve('Cargo.toml') }), /manifest must be a relative path/],
      [withSource({ kind: 'pnpm', glob: 'packages/**/package.json' }), /glob must be a relative pattern ending in package\.json, using \* inside a segment only/],
      [withSource({ kind: 'cargo', glob: 'crates/*/Cargo.lock' }), /glob must be a relative pattern ending in Cargo\.toml/],
      [withSource({ kind: 'swiftpm', manifest: 'Podfile' }), /manifest must be Package\.swift or project\.yml/],
      [withSource({ kind: 'rightkit', versions: '/abs/versions.json' }), /versions must be a relative path/],
      [withSource({ kind: 'cargo', manifest: 'Cargo.toml', policy: 'sometimes' }), /sources\[0\]\.policy must be one of/],
      [withSource({ kind: 'cargo', manifest: 'Cargo.toml', extra: 1 }), /sources\[0\]: unknown field extra/],
      [{ ...manifestOf([]), gates: [{ name: 'a', command: ['x'] }, { name: 'a', command: ['y'] }] }, /gates\[1\]\.name duplicates a/],
      [{ ...manifestOf([]), gates: [{ name: 'bad name', command: ['x'] }] }, /gates\[0\]\.name must match/],
      [{ ...manifestOf([]), gates: [{ name: 'a', command: [] }] }, /gates\[0\]\.command must be a non-empty array of strings/],
      [{ ...manifestOf([]), gates: [{ name: 'a', command: ['x'], timeoutSec: 0 }] }, /gates\[0\]\.timeoutSec must be an integer from 1 to 86400/],
      [{ ...manifestOf([]), gates: [{ name: 'a', command: ['x'], cwd: '../up' }] }, /gates\[0\]\.cwd must be a relative path/],
      [withSource(tool({ journey: { command: ['node', '{nope}'], report: '{out}/r.json' } })), /unknown placeholder \{nope\} \(allowed: \{candidate\}, \{out\}, \{delivery\}, \{model\}\)/],
      [withSource(tool({ assets: { 'macos-arm64': { asset: 'a', checksum: 'sha256:abc' } } })), /checksum must be sha256:<64 lowercase hex>/],
      [withSource(tool({ assets: { 'plan9-arm': { asset: 'a', checksum: `sha256:${A64}` } } })), /unknown platform/],
      [withSource(tool({ assets: {} })), /assets must be a non-empty object keyed by platform/],
      [withSource(tool({ upstream: { type: 'url', latest: 'http://example.invalid/latest', download: 'https://example.invalid/{version}' } })), /upstream\.latest must be an https URL/],
      [withSource(tool({ upstream: { type: 'url', latest: 'https://example.invalid/latest', download: 'https://example.invalid/fixed' } })), /upstream\.download must contain \{version\}/],
      [withSource(tool({ upstream: { type: 'github-release', repo: 'not a repo' } })), /upstream\.repo must be owner\/repo/],
      [withSource(tool({ upstream: { type: 'ftp' } })), /upstream\.type must be one of github-release, url/],
      [withSource(tool({ pin: '../1.0' })), /pin must be a version string/],
      [withSource(tool({ journey: { command: ['node'], report: 'r.json', model: 'bad model!' } })), /journey\.model must be a model id/],
      [withSource(tool({ journey: { command: ['node'], report: 'r.json', timeoutSec: 90000 } })), /journey\.timeoutSec must be an integer from 1 to 86400/],
      [withSource(tool({ assets: { 'macos-arm64': { asset: 'a', checksum: `sha256:${A64}`, extract: { member: '../escape' } } } })), /extract\.member must be a relative path inside the archive/],
    ];
    for (const [candidate, pattern] of refusals) {
      const outcome = outdated.validateManifest(candidate);
      assert.equal(outcome.valid, false, pattern.source);
      assert.equal(outcome.manifest, undefined);
      assert.match(outcome.errors.join('\n'), pattern);
    }

    // The glob rule on its own.
    assert.equal(outdated.safeManifestGlob('crates/*/Cargo.toml', 'Cargo.toml'), true);
    assert.equal(outdated.safeManifestGlob('Cargo.toml', 'Cargo.toml'), true);
    const badGlobs = ['crates/**/Cargo.toml', 'crates/?/Cargo.toml', 'crates/[ab]/Cargo.toml', 'crates/{a,b}/Cargo.toml', '/crates/*/Cargo.toml', '../crates/*/Cargo.toml', 'crates//Cargo.toml', 'crates/*/package.json', '', 5, null];
    for (const bad of badGlobs) assert.equal(outdated.safeManifestGlob(bad, 'Cargo.toml'), false, String(bad));

    // Loading never throws: an unreadable, malformed or invalid file becomes an error message.
    const empty = makeDir('manifest-empty');
    assert.match(outdated.loadManifest(empty).errors[0], /^cannot read .*update\.json: /);
    const malformed = tree('manifest-malformed', { '.rightkit/update.json': '{ "schemaVersion": ' });
    assert.match(outdated.loadManifest(malformed).errors[0], /update\.json is not valid JSON: /);
    const wrong = tree('manifest-wrong', { '.rightkit/update.json': { schemaVersion: 1, sources: [] } });
    assert.deepEqual(outdated.loadManifest(wrong), { valid: false, errors: ['policy must be an object'] });
    const good = tree('manifest-good', { '.rightkit/update.json': complete });
    const loaded = outdated.loadManifest(good);
    assert.equal(loaded.valid, true);
    assert.deepEqual(loaded.manifest, complete);
    return { sourceKinds: 5, refusalsChecked: refusals.length, globRefusals: badGlobs.length, loadOutcomes: ['unreadable', 'malformed', 'invalid', 'valid'] };
  });

  // ---- 3. Version comparison, policy resolution and bump rules.
  await run('version-comparison-policy-and-bump-rules', async () => {
    const compare = (pinned, latest) => outdated.compareVersions(pinned, latest);
    // Precision aware: a pin is compared only on the components it names.
    for (const [pinned, latest, status] of [
      ['1.2.3', '1.2.3', 'current'], ['1.2.3', '1.2.4', 'outdated'], ['1.2', '1.2.5', 'current'], ['1.2', '1.3.0', 'outdated'], ['1', '1.9.9', 'current'], ['1', '2.0.0', 'outdated'],
      ['^1.2.3', '1.2.3', 'current'], ['~0.4', '0.5.0', 'outdated'], ['>=1.0', '1.0.0', 'current'], ['=1.2.3', '1.2.4', 'outdated'], ['v1.2.3', 'v1.2.4', 'outdated'],
      ['1.x', '1.9.0', 'current'], ['1.x', '2.0.0', 'outdated'], ['1.2.*', '1.2.9', 'current'], ['1.2.3-beta.1', '1.2.3', 'current'], ['1.2.3', '1.2.4-rc.1', 'outdated'],
      [' 1.2.3 ', '1.2.4', 'outdated'], ['2025.10.01', '2025.11.02', 'outdated'],
    ]) assert.equal(compare(pinned, latest).status, status, `${pinned} against ${latest}`);
    assert.deepEqual(compare('1.2.3', '1.2.2'), { status: 'current', reason: 'pin is ahead of upstream latest' });
    assert.deepEqual(compare('nightly', 'v nightly'.replace(' ', '')), { status: 'current' });
    assert.deepEqual(compare('stable', 'stable'), { status: 'current' });
    for (const [pinned, latest] of [['abc', '1.0.0'], ['1.0.0', 'latest'], ['nightly', 'weekly'], ['>=1 <2', '1.5.0'], ['*', '1.0.0']]) {
      assert.deepEqual(compare(pinned, latest), { status: 'unknown', reason: 'version format is not comparable' }, `${pinned} against ${latest}`);
    }
    assert.deepEqual(outdated.parsePinned('^1.2.3'), [1, 2, 3]);
    assert.deepEqual(outdated.parsePinned('1.*'), [1]);
    assert.deepEqual(outdated.parsePinned('~0.4'), [0, 4]);
    for (const bad of ['*', '1.2.3.4', 'abc', '']) assert.equal(outdated.parsePinned(bad), null, bad);
    assert.deepEqual(outdated.parseLatest('v1.2'), [1, 2, 0]);
    assert.deepEqual(outdated.parseLatest('3'), [3, 0, 0]);
    assert.deepEqual(outdated.parseLatest('1.2.3-beta+build'), [1, 2, 3]);
    assert.equal(outdated.parseLatest('latest'), null);

    // Policy precedence: names held in the config, then an explicit policy, then the tier patterns, then the default.
    const config = { default: 'auto-land', held: ['pinned-name'], tier: { held: ['/^ffmpeg/', 'exact-name'] } };
    const policy = (name, explicit, over = config) => outdated.resolvePolicy(name, explicit, over);
    assert.equal(policy('pinned-name', 'auto-land'), 'held');
    assert.equal(policy('alpha', 'agent-pr'), 'agent-pr');
    assert.equal(policy('alpha', undefined), 'auto-land');
    assert.equal(policy('alpha', undefined, { default: 'agent-pr' }), 'agent-pr');
    for (const name of ['rightkit-license', 'sign-tool', 'secure-store', 'my-credentials', 'ffmpeg-helper', 'exact-name']) assert.equal(policy(name, undefined), 'held', name);
    assert.equal(policy('ffmpeg-helper', 'auto-land'), 'auto-land');
    assert.equal(policy('exact-name-not', undefined), 'auto-land');

    // Bump rules: what apply may rewrite, and what it hands to a migration.
    const requirement = raw => outdated.parseRequirement(raw);
    assert.deepEqual(requirement('1.2.3'), { raw: '1.2.3', prefix: '', parts: [1, 2, 3] });
    assert.deepEqual(requirement('^1.2'), { raw: '^1.2', prefix: '^', parts: [1, 2] });
    assert.deepEqual(requirement(' ~4 '), { raw: ' ~4 ', prefix: '~', parts: [4] });
    assert.equal(requirement('>=1.0').prefix, '>=');
    assert.equal(requirement('v2.0.1').prefix, 'v');
    for (const bad of ['*', '1.x', '1.2.3-beta', '>=1 <2', '', 'latest', '1.2.3.4']) assert.equal(requirement(bad), null, bad);
    const breaking = (raw, latest) => outdated.isBreaking(requirement(raw), latest);
    assert.equal(breaking('1.2.3', [2, 0, 0]), true);
    assert.equal(breaking('1.2.3', [1, 9, 0]), false);
    assert.equal(breaking('0.3.1', [0, 4, 0]), true);
    assert.equal(breaking('0.3.1', [0, 3, 9]), false);
    assert.equal(breaking('0.0.5', [0, 0, 6]), true);
    assert.equal(outdated.bumpedRequirement(requirement('^1.2'), [1, 9, 3]), '^1.9');
    assert.equal(outdated.bumpedRequirement(requirement('1'), [1, 9, 3]), '1');
    assert.equal(outdated.bumpedRequirement(requirement('~1.2.3'), [1, 2, 9]), '~1.2.9');
    assert.deepEqual(outdated.decideBump('1.2.3', '1.2.9'), { action: 'apply', newRequirement: '1.2.9', fullVersion: '1.2.9' });
    assert.deepEqual(outdated.decideBump('^1.2', '1.9.1'), { action: 'apply', newRequirement: '^1.9', fullVersion: '1.9.1' });
    assert.equal(outdated.decideBump('1.2.3', '2.0.0').action, 'needs-migration');
    assert.match(outdated.decideBump('1.2.3', '2.0.0').reason, /breaking bump 1\.2\.3 -> 2\.0\.0 \(major\)/);
    assert.match(outdated.decideBump('0.3.1', '0.4.0').reason, /breaking bump 0\.3\.1 -> 0\.4\.0 \(0\.x\)/);
    assert.match(outdated.decideBump('*', '1.0.0').reason, /wildcard, range or suffix/);
    assert.match(outdated.decideBump('1.0.0', '1.1.0-rc.1').reason, /pre-release or build version/);
    assert.match(outdated.decideBump('1.0.0', 'nightly').reason, /not a plain numeric version/);
    assert.equal(outdated.decideBump('1.0.0', 'nightly').action, 'skip');

    // Platform ids, the artifact root and delivery directories.
    for (const [platform, arch, expected] of [['darwin', 'arm64', 'macos-arm64'], ['darwin', 'x64', 'macos-x86_64'], ['win32', 'x64', 'windows-x86_64'], ['win32', 'arm64', 'windows-arm64'], ['linux', 'x64', 'linux-x86_64'], ['linux', 'arm64', null], ['freebsd', 'x64', null]]) {
      assert.equal(outdated.hostPlatform(platform, arch), expected, `${platform}-${arch}`);
    }
    assert.equal(outdated.artifactRoot({ platform: 'darwin', env: {}, home: '/Users/me' }), '/Users/me/Library/Application Support/RightKit/artifacts');
    assert.equal(outdated.artifactRoot({ platform: 'linux', env: {}, home: '/home/me' }), '/home/me/.local/share/RightKit/artifacts');
    assert.equal(outdated.artifactRoot({ platform: 'linux', env: { XDG_DATA_HOME: '/data' }, home: '/home/me' }), '/data/RightKit/artifacts');
    assert.equal(outdated.artifactRoot({ platform: 'win32', env: {}, home: 'C:\\Users\\me' }), 'C:\\Users\\me\\AppData\\Local\\RightKit\\artifacts');
    assert.equal(outdated.artifactRoot({ platform: 'win32', env: { LOCALAPPDATA: 'D:\\L' }, home: 'C:\\Users\\me' }), 'D:\\L\\RightKit\\artifacts');
    const override = path.join(scratch, 'artifact-root');
    assert.equal(outdated.artifactRoot({ platform: 'darwin', env: { RIGHTKIT_ARTIFACT_DIR: override }, home: '/Users/me' }), override);
    assert.equal(outdated.deliveryDir('tool', '1.2.3', { platform: 'win32', env: { LOCALAPPDATA: 'D:\\L' } }), 'D:\\L\\RightKit\\artifacts\\tool\\1.2.3');
    assert.equal(outdated.deliveryDir('tool', '1.2.3', { env: { RIGHTKIT_ARTIFACT_DIR: override } }), path.join(override, 'tool', '1.2.3'));
    return { comparison: 'precision-aware', policyPrecedence: ['held names', 'explicit', 'tier patterns', 'default'], bumpDecisions: ['apply', 'needs-migration', 'skip'], platforms: outdated.PLATFORMS };
  });

  // ---- 4. Position-aware Cargo edits, manifest globs and the pnpm and Swift readers.
  await run('toml-edit-glob-and-manifest-parsers', async () => {
    const cargo = [
      '[package]', 'name = "demo"', 'version = "0.1.0"   # the crate, not a dependency', '',
      '[dependencies]', 'plain = "1.0.0"   # keep this comment', 'inline = { version = "~2.1", features = ["a", "b"], default-features = false }',
      'renamed = { package = "real-name", version = "3.0.0" }', 'dotted.version = "4.0.0"', "literal = '5.0.0'", 'local = { path = "../local" }',
      'escaped = { version = "7\\u002e0.0" }', 'duplicate = "1.0.0"', '',
      '[dependencies.table-form]', 'version = "8.0.0"', 'features = ["x"]', '',
      '[dev-dependencies]', 'dev = "9.0.0"', 'duplicate = "1.0.0"', '',
      '[build-dependencies]', 'build = "10.0.0"', '',
      '[workspace.dependencies]', 'shared = "11.0.0"', '',
      "[target.'cfg(windows)'.dependencies]", 'win = "12.0.0"', '',
      '[target.x86_64-unknown-linux-gnu.dev-dependencies]', 'linux = "13.0.0"', '',
      '[features]', 'default = ["plain"]', '', '[[bin]]', 'name = "demo"', '',
    ].join('\n');
    const refs = outdated.findVersionRefs(cargo);
    for (const ref of refs) assert.equal(cargo.slice(ref.start, ref.end), ref.spec, `${ref.name} offsets`);
    assert.deepEqual(refs.map(ref => [ref.name, ref.keyName, ref.spec, ref.table]), [
      ['plain', 'plain', '1.0.0', 'dependencies'], ['inline', 'inline', '~2.1', 'dependencies'], ['real-name', 'renamed', '3.0.0', 'dependencies'],
      ['dotted', 'dotted', '4.0.0', 'dependencies'], ['literal', 'literal', '5.0.0', 'dependencies'], ['duplicate', 'duplicate', '1.0.0', 'dependencies'],
      ['table-form', 'table-form', '8.0.0', 'dependencies'], ['dev', 'dev', '9.0.0', 'dev-dependencies'], ['duplicate', 'duplicate', '1.0.0', 'dev-dependencies'],
      ['build', 'build', '10.0.0', 'build-dependencies'], ['shared', 'shared', '11.0.0', 'workspace.dependencies'],
      ['win', 'win', '12.0.0', 'target.cfg(windows).dependencies'], ['linux', 'linux', '13.0.0', 'target.x86_64-unknown-linux-gnu.dev-dependencies'],
    ]);
    // Edits change only the quoted text. The comment, the table order and every other byte stay.
    const one = outdated.replaceCargoVersion(cargo, 'plain', '1.0.0', '1.0.7');
    assert.equal(one.count, 1);
    assert.equal(one.text, cargo.replace('plain = "1.0.0"', 'plain = "1.0.7"'));
    const renamed = outdated.replaceCargoVersion(cargo, 'real-name', '3.0.0', '3.4.0');
    assert.equal(renamed.count, 1);
    assert.equal(renamed.text, cargo.replace('version = "3.0.0"', 'version = "3.4.0"'));
    assert.equal(outdated.replaceCargoVersion(cargo, 'renamed', '3.0.0', '3.4.0').count, 0, 'the key of a renamed dependency is not its package name');
    assert.equal(outdated.replaceCargoVersion(cargo, 'plain', '9.9.9', '1.0.7').count, 0, 'a stale old spec matches nothing');
    const both = outdated.replaceCargoVersion(cargo, 'duplicate', '1.0.0', '1.1.0');
    assert.equal(both.count, 2);
    assert.equal(both.text.split('duplicate = "1.1.0"').length - 1, 2);
    assert.equal(outdated.replaceCargoVersion(cargo, 'win', '12.0.0', '^12.1').text.includes('win = "^12.1"'), true);
    assert.throws(() => outdated.replaceCargoVersion(cargo, 'plain', '1.0.0', '1.0.0"\n[evil]'), /refusing unsafe version text/);
    assert.throws(() => outdated.findVersionRefs('[dependencies\nx = "1"'), /expected \] in table header/);
    assert.throws(() => outdated.findVersionRefs('[dependencies]\nx = "unterminated\n'), /unterminated string/);
    assert.throws(() => outdated.findVersionRefs('[dependencies]\nx 1\n'), /expected = after key/);
    assert.deepEqual(outdated.findVersionRefs(''), []);

    // The same text through a real TOML parser: dependency rows as `check` reads them.
    const document = parseToml(cargo);
    const rows = outdated.cargoDependencies(document);
    const byName = Object.fromEntries(rows.map(row => [row.name, row]));
    assert.equal(byName.plain.spec, '1.0.0');
    assert.equal(byName['real-name'].spec, '3.0.0');
    assert.deepEqual(byName.local, { name: 'local', spec: null, reason: 'path or git dependency' });
    assert.equal(byName['table-form'].spec, '8.0.0');
    assert.equal(byName.shared.spec, '11.0.0');
    assert.equal(byName.win.spec, '12.0.0');
    assert.equal(byName.linux.spec, '13.0.0');
    assert.deepEqual(outdated.cargoDependencies({ dependencies: { git: { git: 'https://example.invalid/g' }, bare: { features: ['x'] } } }),
      [{ name: 'git', spec: null, reason: 'path or git dependency' }, { name: 'bare', spec: null, reason: 'no version requirement' }]);
    assert.deepEqual(outdated.pnpmDependencies({
      dependencies: { a: '1.0.0', n: 5 }, devDependencies: { a: '2.0.0', b: '^1' }, optionalDependencies: { c: '~3' }, peerDependencies: { d: '1' },
    }), [{ name: 'a', spec: '1.0.0' }, { name: 'b', spec: '^1' }, { name: 'c', spec: '~3' }]);

    // Manifest globs: `*` inside one segment, files only, sorted, dot directories skipped unless the pattern names them.
    const repo = tree('glob', {
      'Cargo.toml': '', 'crates/a/Cargo.toml': '', 'crates/ab/Cargo.toml': '', 'crates/b/Cargo.toml': '', 'crates/.hidden/Cargo.toml': '',
      'crates/c/nested/Cargo.toml': '', 'crates/d/README.md': '', 'pkg/x-core/package.json': '', 'pkg/x-ui/package.json': '', 'pkg/y/package.json': '',
    });
    assert.deepEqual(outdated.expandGlob(repo, 'crates/*/Cargo.toml'), ['crates/a/Cargo.toml', 'crates/ab/Cargo.toml', 'crates/b/Cargo.toml']);
    assert.deepEqual(outdated.expandGlob(repo, 'crates/a*/Cargo.toml'), ['crates/a/Cargo.toml', 'crates/ab/Cargo.toml']);
    assert.deepEqual(outdated.expandGlob(repo, 'crates/.*/Cargo.toml'), ['crates/.hidden/Cargo.toml']);
    assert.deepEqual(outdated.expandGlob(repo, 'pkg/x-*/package.json'), ['pkg/x-core/package.json', 'pkg/x-ui/package.json']);
    assert.deepEqual(outdated.expandGlob(repo, '*/*/package.json'), ['pkg/x-core/package.json', 'pkg/x-ui/package.json', 'pkg/y/package.json']);
    assert.deepEqual(outdated.expandGlob(repo, 'crates/*/missing.toml'), []);
    assert.deepEqual(outdated.expandGlob(repo, 'nowhere/*/Cargo.toml'), []);
    assert.deepEqual(outdated.sourceManifests(repo, { kind: 'cargo', manifest: 'Cargo.toml' }), ['Cargo.toml']);
    assert.deepEqual(outdated.sourceManifests(repo, { kind: 'pnpm', glob: 'pkg/*/package.json' }), ['pkg/x-core/package.json', 'pkg/x-ui/package.json', 'pkg/y/package.json']);

    // Swift: Package.swift and XcodeGen project.yml requirements.
    const swift = [
      '// swift-tools-version:5.9', 'import PackageDescription', 'let package = Package(', '  name: "Demo",', '  dependencies: [',
      '    .package(url: "https://github.com/acme/widgets.git", from: "1.2.0"),', '    .package(url: "https://github.com/acme/gizmo", exact: "2.0.0"),',
      '    .package(url: "https://github.com/acme/branchy", branch: "main"),', '    .package(url: "https://gitlab.com/acme/elsewhere", from: "1.0.0"),',
      '    .package(path: "../local"),', '  ],', '  targets: []', ')', '',
    ].join('\n');
    assert.deepEqual(outdated.parseSwiftPackage(swift), [
      { name: 'widgets', url: 'https://github.com/acme/widgets.git', requirement: '1.2.0' }, { name: 'gizmo', url: 'https://github.com/acme/gizmo', requirement: '2.0.0' },
      { name: 'branchy', url: 'https://github.com/acme/branchy', requirement: null, reason: 'branch or revision pin' },
      { name: 'elsewhere', url: 'https://gitlab.com/acme/elsewhere', requirement: '1.0.0' },
    ]);
    const projectYml = [
      'name: Demo', 'packages:', '  Widgets:', '    url: https://github.com/acme/widgets', '    from: 1.2.0', '  Gizmo:', '    url: "https://github.com/acme/gizmo.git"',
      '    exactVersion: "2.0.0"', '  Branchy:', '    url: https://github.com/acme/branchy', '    branch: main', '',
    ].join('\n');
    assert.deepEqual(outdated.parseSwiftProjectYml(projectYml), [
      { name: 'widgets', url: 'https://github.com/acme/widgets', requirement: '1.2.0' }, { name: 'gizmo', url: 'https://github.com/acme/gizmo.git', requirement: '2.0.0' },
      { name: 'branchy', url: 'https://github.com/acme/branchy', requirement: null, reason: 'unsupported version requirement' },
    ]);
    return { cargoRefs: refs.length, editedByteRanges: 'version strings only', globs: 'sorted files, dot directories skipped', parsers: ['cargo', 'pnpm', 'Package.swift', 'project.yml'] };
  });

  // ---- 5. `check` for cargo and pnpm sources, with the registries answered by test doubles.
  await run('check-cargo-and-pnpm-sources', async () => {
    const crate = (version, key = 'max_stable_version') => ({ crate: { [key]: version } });
    const crates = name => `https://crates.io/api/v1/crates/${name}`;
    const npm = name => `https://registry.npmjs.org/${encodeURIComponent(name)}/latest`;
    const cargoToml = [
      '[package]', 'name = "demo"', 'version = "0.1.0"', '',
      '[dependencies]', 'alpha = "1.0.0"', 'beta = { version = "0.3.1", features = ["x"] }', 'gamma = { path = "../gamma", version = "0.1.0" }',
      'delta = { git = "https://example.invalid/delta" }', 'renamed = { package = "epsilon", version = "2.1" }', 'wild = "*"', 'secure-store = "1.0.0"', '',
      '[dev-dependencies]', 'zeta = "~4.2.0"', '', '[build-dependencies]', 'eta = "1"', '', '[workspace.dependencies]', 'theta = "5.0.0"', '',
      "[target.'cfg(windows)'.dependencies]", 'iota = "0.9.0"', '',
    ].join('\n');
    const repo = gitRepo('check-cargo', {
      '.rightkit/update.json': manifestOf([{ kind: 'cargo', manifest: 'Cargo.toml' }, { kind: 'cargo', glob: 'crates/*/Cargo.toml' }, { kind: 'pnpm', manifest: 'web/package.json' }, { kind: 'cargo', glob: 'missing/*/Cargo.toml' }], { default: 'auto-land', held: ['alpha'] }),
      'Cargo.toml': cargoToml,
      'crates/one/Cargo.toml': '[dependencies]\nalpha = "1.0.0"\nonly-one = "3.0.0"\n',
      'crates/two/Cargo.toml': '[dependencies]\nalpha = "1.0.0"\n',
      'web/package.json': { name: 'web', dependencies: { left: '^1.0.0', right: '^2.0', local: 'workspace:*', linked: 'file:../x', gone: '1.0.0' }, devDependencies: { dev: '~3.1.0', left: '9.9.9' }, optionalDependencies: { opt: '0.5.0' } },
    });
    const routes = {
      [crates('alpha')]: crate('1.4.0'), [crates('beta')]: crate('0.4.0'), [crates('epsilon')]: crate('2.1.9'), [crates('secure-store')]: crate('1.0.0'),
      [crates('zeta')]: crate('4.2.0'), [crates('eta')]: crate('1.9.9'), [crates('theta')]: crate('5.0.0', 'max_version'), [crates('only-one')]: crate('3.1.0'),
      [npm('right')]: { version: '2.0.4' }, [npm('dev')]: { version: '3.2.0' }, [npm('opt')]: { version: '0.5.0' },
    };
    const { fetch, calls } = mockFetch(routes);
    // `pnpm outdated --json` exits non-zero when anything is outdated and still prints the answer.
    const execCalls = [];
    const exec = async (file, args, options) => { execCalls.push({ file, args, cwd: options.cwd }); return { stdout: JSON.stringify({ left: { current: '1.0.0', latest: '1.5.0' } }), error: 'exit 1' }; };
    const loaded = outdated.loadManifest(repo);
    const entries = await outdated.checkSources(repo, loaded.manifest, { fetch, exec });
    const row = (kind, name, source) => { const found = entries.filter(entry => entry.kind === kind && entry.name === name && (source === undefined || entry.source === source)); assert.equal(found.length, 1, `${kind}:${name}`); return found[0]; };

    assert.deepEqual(row('cargo', 'alpha', 'Cargo.toml'), { name: 'alpha', kind: 'cargo', source: 'Cargo.toml', pinned: '1.0.0', latest: '1.4.0', status: 'outdated', policy: 'held' });
    assert.deepEqual(row('cargo', 'beta'), { name: 'beta', kind: 'cargo', source: 'Cargo.toml', pinned: '0.3.1', latest: '0.4.0', status: 'outdated', policy: 'auto-land' });
    assert.deepEqual(row('cargo', 'gamma'), { name: 'gamma', kind: 'cargo', source: 'Cargo.toml', pinned: null, latest: null, status: 'unknown', policy: 'auto-land', reason: 'path or git dependency' });
    assert.equal(row('cargo', 'delta').reason, 'path or git dependency');
    assert.equal(row('cargo', 'epsilon').status, 'current', 'a renamed dependency is looked up by its package name, and a two-part pin matches 2.1.9');
    assert.equal(row('cargo', 'epsilon').pinned, '2.1');
    assert.deepEqual([row('cargo', 'wild').status, row('cargo', 'wild').reason], ['unknown', 'non-registry version requirement']);
    assert.deepEqual([row('cargo', 'secure-store').status, row('cargo', 'secure-store').policy], ['current', 'held']);
    assert.equal(row('cargo', 'zeta').status, 'current');
    assert.equal(row('cargo', 'eta').status, 'current');
    assert.equal(row('cargo', 'theta').latest, '5.0.0', 'max_version is the fallback when there is no stable version');
    assert.equal(row('cargo', 'iota').error, 'crates.io lookup failed: GET https://crates.io/api/v1/crates/iota returned HTTP 404');
    assert.equal(row('cargo', 'only-one', 'crates/one/Cargo.toml').status, 'outdated');
    // The glob covers both crates, and one lookup serves every manifest that names the same crate.
    assert.deepEqual(entries.filter(entry => entry.name === 'alpha').map(entry => entry.source), ['Cargo.toml', 'crates/one/Cargo.toml', 'crates/two/Cargo.toml']);
    assert.equal(calls.filter(url => url === crates('alpha')).length, 1);
    assert.ok(calls.every(url => url in routes || [crates('iota'), npm('gone')].includes(url)), 'no request outside the registries');

    // pnpm: `pnpm outdated` answers first; the registry is the fallback; non-registry specs are not looked up.
    assert.deepEqual(execCalls, [{ file: 'pnpm', args: ['outdated', '--json'], cwd: path.join(repo, 'web') }]);
    assert.deepEqual(row('pnpm', 'left'), { name: 'left', kind: 'pnpm', source: 'web/package.json', pinned: '^1.0.0', latest: '1.5.0', status: 'outdated', policy: 'auto-land' });
    assert.equal(row('pnpm', 'right').status, 'current');
    assert.equal(row('pnpm', 'dev').latest, '3.2.0');
    assert.equal(row('pnpm', 'opt').status, 'current');
    assert.equal(row('pnpm', 'local').reason, 'non-registry version requirement');
    assert.equal(row('pnpm', 'linked').pinned, 'file:../x');
    assert.match(row('pnpm', 'gone').error, /^npm registry lookup failed: GET https:\/\/registry\.npmjs\.org\/gone\/latest returned HTTP 404$/);
    assert.equal(calls.some(url => url.includes('left')), false, 'pnpm already answered for left');

    // A glob that matches nothing is an error row, not a crash, and the other sources still ran.
    const empty = entries.find(entry => entry.source === 'missing/*/Cargo.toml');
    assert.deepEqual([empty.kind, empty.name, empty.error], ['cargo', 'missing/*/Cargo.toml', 'glob missing/*/Cargo.toml matched no files']);
    const report = outdated.buildReport(repo, entries);
    assert.equal(report.exitCode, 2);
    assert.equal(report.schemaVersion, 1);
    assert.deepEqual(report.errors, []);

    // Exit codes: 2 for any error, else 1 for any outdated, else 0.
    const entryOf = fields => ({ name: 'x', kind: 'cargo', source: 'Cargo.toml', pinned: '1', latest: '1', status: 'current', policy: 'auto-land', ...fields });
    assert.equal(outdated.exitCodeFor([entryOf({})]), 0);
    assert.equal(outdated.exitCodeFor([entryOf({}), entryOf({ status: 'unknown' })]), 0);
    assert.equal(outdated.exitCodeFor([entryOf({ status: 'outdated' })]), 1);
    assert.equal(outdated.exitCodeFor([entryOf({ status: 'outdated' }), entryOf({ error: 'x' })]), 2);
    assert.equal(outdated.exitCodeFor([entryOf({})], ['manifest invalid']), 2);

    // runCheck wraps load, check and report and never throws.
    const bad = await outdated.runCheck(makeDir('check-none'), { fetch, exec });
    assert.equal(bad.exitCode, 2);
    assert.equal(bad.entries.length, 0);
    assert.match(bad.errors[0], /cannot read/);
    const current = gitRepo('check-current', { '.rightkit/update.json': manifestOf([{ kind: 'cargo', manifest: 'Cargo.toml' }]), 'Cargo.toml': '[dependencies]\nalpha = "1.4"\n' });
    const clean = await outdated.runCheck(current, { fetch: mockFetch({ [crates('alpha')]: crate('1.4.2') }).fetch, exec: noExec });
    assert.deepEqual([clean.exitCode, clean.entries.map(entry => entry.status)], [0, ['current']]);
    return { cargoRows: entries.filter(entry => entry.kind === 'cargo').length, pnpmRows: entries.filter(entry => entry.kind === 'pnpm').length, registryRequests: calls.length, exitCodes: [0, 1, 2] };
  });

  // ---- 6. `check` for Swift packages, the RightKit versions file and external tools.
  await run('check-swiftpm-rightkit-and-tool-sources', async () => {
    const github = path => `https://api.github.com/repos/${path}`;
    const npm = name => `https://registry.npmjs.org/${encodeURIComponent(name)}/latest`;
    const asset = (name, checksum = A64) => ({ asset: name, checksum: `sha256:${checksum}` });
    const swiftPackage = [
      'import PackageDescription', 'let package = Package(name: "Demo", dependencies: [',
      '  .package(url: "https://github.com/acme/widgets.git", from: "1.2.0"),', '  .package(url: "https://github.com/acme/gizmo", exact: "2.0.0"),',
      '  .package(url: "https://github.com/acme/branchy", branch: "main"),', '  .package(url: "https://gitlab.com/acme/elsewhere", from: "1.0.0"),', '  .package(url: "https://github.com/acme/untagged", from: "1.0.0"),', '])', '',
    ].join('\n');
    const projectYml = ['packages:', '  Widgets:', '    url: https://github.com/acme/widgets', '    from: 1.2.0', ''].join('\n');
    const upstream = (type, extra) => ({ type, ...extra });
    const sources = [
      { kind: 'swiftpm', manifest: 'Package.swift' }, { kind: 'swiftpm', manifest: 'apps/project.yml' }, { kind: 'rightkit' },
      { kind: 'tool', name: 'current-tool', pin: '1.4.2', assets: { 'macos-arm64': asset('current-macos') }, upstream: upstream('url', { latest: 'https://example.invalid/current/latest.txt', download: 'https://example.invalid/current/{version}/{asset}' }), journey: { command: ['node', 'j.mjs'], report: '{out}/report.json' } },
      { kind: 'tool', name: 'newer-tool', pin: 'v2.0.0', assets: { 'macos-arm64': asset('newer-macos'), 'windows-x86_64': asset('newer.exe', B64) }, upstream: upstream('github-release', { repo: 'acme/newer' }), journey: { command: ['node', 'j.mjs'], report: '{out}/report.json' } },
      { kind: 'tool', name: 'short-tool', pin: '1.0.0', assets: { 'macos-arm64': asset('short-macos'), 'windows-x86_64': asset('short.exe', B64) }, upstream: upstream('github-release', { repo: 'acme/short' }), journey: { command: ['node', 'j.mjs'], report: '{out}/report.json' } },
      { kind: 'tool', name: 'odd-tool', pin: '1.0.0', assets: { 'macos-arm64': asset('odd-macos') }, upstream: upstream('url', { latest: 'https://example.invalid/odd/latest.txt', download: 'https://example.invalid/odd/{version}' }), journey: { command: ['node', 'j.mjs'], report: '{out}/report.json' }, policy: 'agent-pr' },
    ];
    const repo = gitRepo('check-sources', {
      '.rightkit/update.json': manifestOf(sources), 'Package.swift': swiftPackage, 'apps/project.yml': projectYml, '.rightgit.json': { schemaVersion: 1 },
      'packages/release/rightkit-versions.json': { npm: { '@rightkit/alpha': '0.1.0', '@rightkit/beta': '0.2.0', note: 5 } },
    });
    const validated = outdated.loadManifest(repo);
    assert.equal(validated.valid, true, validated.errors.join('; '));
    const { fetch, calls } = mockFetch({
      [github('acme/widgets/tags?per_page=100')]: [{ name: 'v1.3.0' }, { name: '1.10.2' }, { name: 'v1.9.0' }, { name: 'nightly' }, { name: '2.0.0-beta' }, { sha: 'no name' }],
      [github('acme/gizmo/tags?per_page=100')]: [{ name: '2.0.0' }], [github('acme/untagged/tags?per_page=100')]: [{ name: 'nightly' }],
      [npm('@rightkit/alpha')]: { version: '0.1.0' }, [npm('@rightkit/beta')]: { version: '0.3.0' },
      [github('acme/newer/releases/latest')]: { tag_name: 'v2.1.0', assets: [{ name: 'newer-macos' }, { name: 'newer.exe' }] },
      [github('acme/short/releases/latest')]: { tag_name: 'v1.1.0', assets: [{ name: 'short-macos' }] },
      'https://example.invalid/current/latest.txt': '1.4.2\n', 'https://example.invalid/odd/latest.txt': '<html>not a version</html>',
    });
    const drift = async () => ({ ok: true, driftDetected: true, visibility: { allowed: false }, errors: [] });
    const entries = await outdated.checkSources(repo, validated.manifest, { fetch, exec: noExec, drift });
    const row = (kind, name) => { const found = entries.filter(entry => entry.kind === kind && entry.name === name); assert.equal(found.length, 1, `${kind}:${name}`); return found[0]; };

    // Swift: the highest numeric tag wins by value, not by text; unsupported rows say why.
    assert.deepEqual(entries.filter(entry => entry.name === 'widgets').map(entry => [entry.source, entry.pinned, entry.latest, entry.status]),
      [['Package.swift', '1.2.0', '1.10.2', 'outdated'], ['apps/project.yml', '1.2.0', '1.10.2', 'outdated']]);
    assert.equal(row('swiftpm', 'gizmo').status, 'current');
    assert.deepEqual([row('swiftpm', 'branchy').status, row('swiftpm', 'branchy').reason], ['unknown', 'branch or revision pin']);
    assert.deepEqual([row('swiftpm', 'elsewhere').pinned, row('swiftpm', 'elsewhere').reason], ['1.0.0', 'non-GitHub package URL']);
    assert.equal(row('swiftpm', 'untagged').reason, 'no numeric release tags');
    assert.equal(calls.some(url => url.includes('gitlab')), false);

    // The RightKit versions file, and the workflow drift row for a repository with .rightgit.json.
    assert.equal(row('rightkit', '@rightkit/alpha').status, 'current');
    assert.equal(row('rightkit', '@rightkit/beta').status, 'outdated');
    assert.equal(row('rightkit', '@rightkit/beta').source, 'packages/release/rightkit-versions.json');
    assert.deepEqual(row('rightkit', 'rightgit-workflows'), { name: 'rightgit-workflows', kind: 'rightkit', source: '.rightgit.json', pinned: 'installed', latest: 'rendered', status: 'outdated', policy: 'auto-land', reason: 'visibility gate did not confirm a public repository' });
    const brokenDrift = await outdated.checkSources(repo, { ...validated.manifest, sources: [{ kind: 'rightkit' }] }, { fetch, exec: noExec, drift: async () => ({ ok: false, driftDetected: null, visibility: { allowed: true }, errors: ['render failed'] }) });
    assert.equal(brokenDrift.find(entry => entry.name === 'rightgit-workflows').error, 'drift check failed: render failed');
    const missingVersions = await outdated.checkSources(makeDir('check-no-versions'), { ...validated.manifest, sources: [{ kind: 'rightkit', versions: 'nope.json' }] }, { fetch, exec: noExec, drift });
    assert.match(missingVersions[0].error, /nope\.json/);

    // Tools: a plain-text latest, a GitHub release whose assets are all published, and one that is not.
    assert.deepEqual(row('tool', 'current-tool'), {
      name: 'current-tool', kind: 'tool', source: 'tool:current-tool', pinned: '1.4.2', latest: '1.4.2', status: 'current', policy: 'auto-land',
      platforms: [{ platform: 'macos-arm64', asset: 'current-macos', checksum: `sha256:${A64}` }],
    });
    assert.deepEqual([row('tool', 'newer-tool').pinned, row('tool', 'newer-tool').latest, row('tool', 'newer-tool').status], ['v2.0.0', 'v2.1.0', 'outdated']);
    assert.deepEqual(row('tool', 'newer-tool').platforms.map(platform => platform.platform), ['macos-arm64', 'windows-x86_64']);
    assert.equal(row('tool', 'short-tool').error, 'assets missing from upstream release: windows-x86_64:short.exe');
    assert.equal(row('tool', 'odd-tool').error, 'upstream lookup failed: latest URL did not return a version string');
    assert.equal(row('tool', 'odd-tool').policy, 'agent-pr');
    assert.equal(outdated.buildReport(repo, entries).exitCode, 2);
    return { swiftRows: entries.filter(entry => entry.kind === 'swiftpm').length, toolRows: entries.filter(entry => entry.kind === 'tool').length, requests: calls.length };
  });

  // ---- 7. Bounded HTTP, bounded concurrency and the bounded child-process runner.
  await run('bounded-http-and-process-runner', async () => {
    const seen = [];
    const answer = (status, body = '{"ok":true}', headers = {}) => async (url, init) => { seen.push({ url, init }); return new Response(body, { status, headers }); };
    assert.equal(await outdated.fetchText('https://example.invalid/a', { fetch: answer(200, 'hello') }), 'hello');
    assert.equal(seen[0].init.headers['user-agent'], outdated.USER_AGENT);
    assert.equal(seen[0].init.headers.accept, 'application/json');
    assert.ok(seen[0].init.signal instanceof AbortSignal);
    await outdated.fetchText('https://example.invalid/a', { fetch: answer(200, 'x'), accept: 'text/plain' });
    assert.equal(seen[1].init.headers.accept, 'text/plain');
    assert.deepEqual(await outdated.fetchJson('https://example.invalid/a', { fetch: answer(200) }), { ok: true });
    await assert.rejects(outdated.fetchJson('https://example.invalid/a', { fetch: answer(200, 'not json') }), { name: 'HttpError', message: 'GET https://example.invalid/a did not return JSON' });
    const before = seen.length;
    await assert.rejects(outdated.fetchText('https://example.invalid/a', { fetch: answer(500, 'no') }), { message: 'GET https://example.invalid/a returned HTTP 500' });
    assert.equal(seen.length - before, 1, 'a plain server error is not retried');

    // Rate limits and transient gateway errors retry with doubling waits, then give up after four attempts.
    for (const status of [429, 502, 503, 504]) {
      const waits = [];
      let attempts = 0;
      const flaky = async () => { attempts += 1; return attempts < 3 ? new Response('busy', { status }) : new Response('fine', { status: 200 }); };
      assert.equal(await outdated.fetchText('https://example.invalid/b', { fetch: flaky, sleep: async ms => { waits.push(ms); } }), 'fine');
      assert.deepEqual(waits, [1000, 2000], String(status));
    }
    const waits = [];
    let attempts = 0;
    await assert.rejects(outdated.fetchText('https://example.invalid/b', { fetch: async () => { attempts += 1; return new Response('busy', { status: 429 }); }, sleep: async ms => { waits.push(ms); } }), { message: 'GET https://example.invalid/b returned HTTP 429' });
    assert.deepEqual([attempts, waits], [4, [1000, 2000, 4000]]);

    // Size bound, declared and streamed; and a deadline that aborts a request that never answers.
    await assert.rejects(outdated.fetchText('https://example.invalid/c', { fetch: answer(200, 'x', { 'content-length': '5000' }), maxBytes: 1000 }), { message: 'GET https://example.invalid/c response exceeds 1000 bytes' });
    await assert.rejects(outdated.fetchText('https://example.invalid/c', { fetch: answer(200, new Uint8Array(2000)), maxBytes: 1000 }), { message: 'GET https://example.invalid/c response exceeds 1000 bytes' });
    assert.equal(await outdated.fetchText('https://example.invalid/c', { fetch: answer(200, new Uint8Array(1000)), maxBytes: 1000 }), '\u0000'.repeat(1000));
    let aborted = false;
    const hang = (url, init) => new Promise((resolve, reject) => init.signal.addEventListener('abort', () => { aborted = true; reject(Object.assign(new Error('aborted'), { name: 'AbortError' })); }));
    await assert.rejects(outdated.fetchText('https://example.invalid/d', { fetch: hang, timeoutMs: 25 }), { name: 'HttpError', message: 'GET https://example.invalid/d timed out' });
    assert.equal(aborted, true);

    // mapLimit keeps order and never has more than `limit` tasks in flight.
    let inFlight = 0;
    let peak = 0;
    const doubled = await outdated.mapLimit([5, 1, 4, 2, 3, 6, 7], 3, async value => { inFlight += 1; peak = Math.max(peak, inFlight); await sleep(value * 3); inFlight -= 1; return value * 2; });
    assert.deepEqual(doubled, [10, 2, 8, 4, 6, 12, 14]);
    assert.equal(peak, 3);
    assert.deepEqual(await outdated.mapLimit([], 4, async value => value), []);

    // The process runner: no shell, captured output, stdin, exit codes, spawn failures and a hard deadline.
    const here = makeDir('runner');
    const script = (name, source) => { const file = path.join(here, name); writeFileSync(file, source); return file; };
    const echo = script('echo.mjs', "process.stdout.write('out:' + process.argv.slice(2).join('|')); process.stderr.write('err'); let input = ''; process.stdin.setEncoding('utf8').on('data', chunk => { input += chunk; }).on('end', () => { process.stdout.write('\\nstdin:' + input); process.exitCode = Number(process.env.RK_RUNNER_EXIT ?? 0); });\n");
    const good = await outdated.runCommand([NODE, echo, 'a b', '$HOME', ';'], { cwd: here, timeoutMs: 20_000, stdin: 'hello', stdoutPath: path.join(here, 'logs', 'out.log'), stderrPath: path.join(here, 'logs', 'err.log') });
    assert.deepEqual([good.code, good.signal, good.timedOut, good.error], [0, null, false, undefined]);
    assert.equal(good.stdout, 'out:a b|$HOME|;\nstdin:hello');
    assert.equal(good.stderr, 'err');
    assert.equal(readFileSync(path.join(here, 'logs', 'out.log'), 'utf8'), good.stdout);
    assert.equal(readFileSync(path.join(here, 'logs', 'err.log'), 'utf8'), 'err');
    assert.ok(good.durationMs >= 0);
    process.env.RK_RUNNER_EXIT = '7';
    const nonzero = await outdated.runCommand([NODE, echo], { cwd: here, timeoutMs: 20_000, env: { ...process.env } });
    delete process.env.RK_RUNNER_EXIT;
    assert.deepEqual([nonzero.code, nonzero.timedOut], [7, false]);
    const flood = script('flood.mjs', "process.stdout.write('x'.repeat(5000) + 'END');\n");
    const tailed = await outdated.runCommand([NODE, flood], { cwd: here, timeoutMs: 20_000, captureBytes: 100, stdoutPath: path.join(here, 'flood.log') });
    assert.equal(tailed.stdout.length, 100);
    assert.ok(tailed.stdout.endsWith('xxxEND'));
    assert.equal(readFileSync(path.join(here, 'flood.log'), 'utf8').length, 5003);
    const missing = await outdated.runCommand(['rk-no-such-program-for-outdated'], { cwd: here, timeoutMs: 20_000 });
    assert.equal(missing.code, null);
    assert.equal(typeof missing.error, 'string');
    assert.equal((await outdated.runCommand([], { cwd: here, timeoutMs: 1000 })).error, 'empty command');
    const hangScript = script('hang.mjs', "setTimeout(() => {}, 60_000);\n");
    const timedOut = await outdated.runCommand([NODE, hangScript], { cwd: here, timeoutMs: 400 });
    assert.equal(timedOut.timedOut, true);
    // POSIX kills the process group with SIGKILL, so there is no exit code; Windows ends the tree with taskkill, which leaves a non-zero code.
    assert.notEqual(timedOut.code, 0);
    assert.ok(timedOut.durationMs < 20_000);
    return { retryStatuses: [429, 502, 503, 504], attempts: 4, concurrencyPeak: peak, runnerOutcomes: ['exit 0', 'exit 7', 'tail', 'spawn failure', 'empty', 'timeout'] };
  });

  // ---- 8. `apply`: every row is a small transaction, and nothing is committed.
  await run('apply-transactions-rollback-and-dry-run', async () => {
    const crates = name => `https://crates.io/api/v1/crates/${name}`;
    const tags = name => `https://api.github.com/repos/acme/${name}/tags?per_page=100`;
    const cargoToml = [
      '[package]', 'name = "demo"', 'version = "0.1.0"', '', '# keep this comment', '[dependencies]', 'alpha = "1.0.0"   # trailing comment',
      'beta = { version = "^1.2", features = ["x"] }', 'gamma = "0.3.1"', 'hold-me = "1.0.0"', 'fail-me = "1.0.0"', 'wild = "*"', '',
    ].join('\n');
    const cargoLock = lockText({ alpha: '1.0.0', beta: '1.2.0', gamma: '0.3.1', 'hold-me': '1.0.0', 'fail-me': '1.0.0' });
    const packageJson = { name: 'web', dependencies: { broken: '1.0.0', left: '^1.0.0', pinned: '2.0.0' }, devDependencies: { dev: '~3.1.0' } };
    const swiftPackage = [
      'import PackageDescription', 'let package = Package(name: "Demo", dependencies: [', '  .package(url: "https://github.com/acme/widgets", from: "1.2.0"),',
      '  .package(url: "https://github.com/acme/gizmo", exact: "2.0.0"),', '  .package(url: "https://github.com/acme/narrow", from: "0.9.0"),', '])', '',
    ].join('\n');
    const repo = gitRepo('apply', {
      '.rightkit/update.json': manifestOf([{ kind: 'cargo', manifest: 'Cargo.toml' }, { kind: 'pnpm', manifest: 'web/package.json' }, { kind: 'swiftpm', manifest: 'Package.swift' }], { default: 'auto-land', held: ['hold-me'] }),
      'Cargo.toml': cargoToml, 'Cargo.lock': cargoLock, 'web/package.json': packageJson, 'Package.swift': swiftPackage,
    });
    const originals = { 'Cargo.toml': cargoToml, 'Cargo.lock': cargoLock, 'web/package.json': `${JSON.stringify(packageJson, null, 2)}\n`, 'Package.swift': swiftPackage };
    const { fetch, calls: fetched } = mockFetch({
      [crates('alpha')]: { crate: { max_stable_version: '1.0.9' } }, [crates('beta')]: { crate: { max_stable_version: '1.9.0' } }, [crates('gamma')]: { crate: { max_stable_version: '0.4.0' } },
      [crates('hold-me')]: { crate: { max_stable_version: '2.0.0' } }, [crates('fail-me')]: { crate: { max_stable_version: '1.1.0' } },
      [tags('widgets')]: [{ name: '1.3.1' }, { name: '1.3.0' }], [tags('gizmo')]: [{ name: '2.0.0' }], [tags('narrow')]: [{ name: '0.10.0' }],
    });
    const exec = async () => ({ stdout: JSON.stringify({ broken: { latest: '1.2.0' }, left: { latest: '1.4.0' }, pinned: { latest: '2.0.3' }, dev: { latest: '3.2.0' } }), error: 'exit 1' });
    const pnpmAdd = (argv, options) => {
      if (argv[0] !== 'pnpm') return undefined;
      assert.deepEqual(argv.slice(0, 2), ['pnpm', 'add']);
      const spec = argv.at(-1);
      const name = spec.slice(0, spec.lastIndexOf('@'));
      const wanted = spec.slice(spec.lastIndexOf('@') + 1);
      writeFileSync(path.join(options.cwd, 'pnpm-lock.yaml'), `locked: ${name}@${wanted}\n`);
      if (name === 'broken') return failed('ERR_PNPM_NO_MATCHING_VERSION  No matching version found for broken@1.2.0');
      const prefix = argv.find(arg => arg.startsWith('--save-prefix='))?.slice('--save-prefix='.length) ?? '';
      const file = path.join(options.cwd, 'package.json');
      const pkg = JSON.parse(readFileSync(file, 'utf8'));
      pkg[argv.includes('-D') ? 'devDependencies' : 'dependencies'][name] = prefix + wanted;
      writeFileSync(file, `${JSON.stringify(pkg, null, 2)}\n`);
      return ok();
    };
    const { run: runFn, calls } = fakeRun((argv, options) => cargoUpdate(repo, ['fail-me'])(argv) ?? pnpmAdd(argv, options));
    const deps = { fetch, exec, run: runFn, now: () => NOW };
    const restoredToOriginals = () => { for (const [rel, text] of Object.entries(originals)) assert.equal(read(repo, rel), text, rel); assert.equal(git(repo, 'status', '--porcelain'), ''); };
    const effects = () => calls.filter(argv => argv[0] === 'rustright' || argv[0] === 'pnpm');

    // A dry run plans every row and touches nothing: no commands, no state directory, no report.
    const dry = await outdated.runApply({ repo, dryRun: true }, deps);
    assert.equal(dry.exitCode, 0);
    assert.equal(dry.reportPath, undefined);
    assert.equal(dry.report.dryRun, true);
    assert.deepEqual(dry.report.changes.map(change => [change.name, change.status]), [
      ['alpha', 'planned'], ['beta', 'planned'], ['fail-me', 'planned'], ['broken', 'planned'], ['left', 'planned'], ['pinned', 'planned'], ['dev', 'planned'], ['widgets', 'planned'],
    ]);
    assert.deepEqual(dry.report.changes[0].commands, [['rustright', 'cargo', 'update', '-p', 'alpha', '--precise', '1.0.9']]);
    assert.equal(effects().length, 0);
    assert.equal(existsSync(path.join(repo, '.rightkit', 'update')), false);
    restoredToOriginals();

    // --only narrows the rows; an unknown name is an error that changes nothing.
    const scoped = await outdated.runApply({ repo, dryRun: true, only: ['alpha'] }, deps);
    assert.deepEqual(scoped.report.changes.map(change => change.name), ['alpha']);
    const unknown = await outdated.runApply({ repo, dryRun: true, only: ['nope'] }, deps);
    assert.deepEqual([unknown.exitCode, unknown.errors], [2, ['--only names not found in the manifest: nope']]);

    // A file with uncommitted edits is never rewritten: the row fails and the edits stay exactly as they were.
    appendFileSync(path.join(repo, 'Cargo.toml'), '\n# uncommitted\n');
    const dirty = await outdated.runApply({ repo, only: ['alpha'] }, deps);
    assert.equal(dirty.exitCode, 1);
    assert.deepEqual(dirty.report.changes, []);
    assert.equal(dirty.report.failed[0].reason, 'refusing to edit files with uncommitted changes: Cargo.toml');
    assert.equal(read(repo, 'Cargo.toml'), `${cargoToml}\n# uncommitted\n`);
    assert.equal(effects().length, 0);
    git(repo, 'checkout', '--', 'Cargo.toml');

    // A failing package-manager command restores the files it had already touched, including a lockfile it created.
    const broken = await outdated.runApply({ repo, only: ['broken'] }, deps);
    assert.equal(broken.exitCode, 1);
    assert.match(broken.report.failed[0].reason, /^pnpm add --save-exact --ignore-scripts broken@1\.2\.0 failed: ERR_PNPM_NO_MATCHING_VERSION/);
    assert.equal(existsSync(path.join(repo, 'web', 'pnpm-lock.yaml')), false);
    restoredToOriginals();

    // The real apply: bumps what it may, reports what it must not, rolls back only the failed rows.
    const before = effects().length;
    const applied = await outdated.runApply({ repo }, deps);
    assert.equal(applied.exitCode, 1, 'done, but rows were rolled back');
    assert.deepEqual(applied.errors, []);
    const report = applied.report;
    assert.deepEqual([report.schemaVersion, report.tool, report.version, report.repo, report.createdAt, report.dryRun, report.landed], [1, 'right-update', version, repo, '2026-10-11T00:00:00.000Z', false, null]);
    assert.equal(report.baseCommit, git(repo, 'rev-parse', 'HEAD'));
    assert.deepEqual(report.changes.map(change => [change.kind, change.name, change.before, change.after, change.status]), [
      ['cargo', 'alpha', '1.0.0', '1.0.9', 'applied'], ['cargo', 'beta', '^1.2', '^1.9', 'applied'], ['pnpm', 'left', '^1.0.0', '^1.4.0', 'applied'],
      ['pnpm', 'pinned', '2.0.0', '2.0.3', 'applied'], ['pnpm', 'dev', '~3.1.0', '~3.2.0', 'applied'], ['swiftpm', 'widgets', '1.2.0', '1.3.1', 'applied'],
    ]);
    assert.deepEqual(report.changes.map(change => change.commands), [
      [['rustright', 'cargo', 'update', '-p', 'alpha', '--precise', '1.0.9']], [['rustright', 'cargo', 'update', '-p', 'beta', '--precise', '1.9.0']],
      [['pnpm', 'add', '--save-prefix=^', '--ignore-scripts', 'left@1.4.0']], [['pnpm', 'add', '--save-exact', '--ignore-scripts', 'pinned@2.0.3']],
      [['pnpm', 'add', '-D', '--save-prefix=~', '--ignore-scripts', 'dev@3.2.0']], [],
    ]);
    assert.deepEqual(report.needsMigration.map(row => [row.name, row.before, row.latest]), [['gamma', '0.3.1', '0.4.0'], ['narrow', '0.9.0', '0.10.0']]);
    assert.match(report.needsMigration[0].reason, /breaking bump 0\.3\.1 -> 0\.4\.0/);
    assert.deepEqual(report.held.map(row => [row.name, row.reason, row.policy]), [['hold-me', 'policy: held', 'held']]);
    assert.deepEqual(report.failed.map(row => row.name), ['fail-me', 'broken']);
    assert.match(report.failed[0].reason, /^rustright cargo update failed: error: package ID specification `fail-me` did not match any packages/);
    assert.deepEqual(report.skipped, []);
    assert.equal(effects().length - before, 7, 'five applied rows and two failed ones ran one command each; the Swift row runs none');

    // Exact bytes: only the version strings moved. The failed rows left their files as they were.
    const expectedToml = cargoToml.replace('alpha = "1.0.0"', 'alpha = "1.0.9"').replace('version = "^1.2"', 'version = "^1.9"');
    assert.equal(read(repo, 'Cargo.toml'), expectedToml);
    assert.equal(read(repo, 'Cargo.lock'), cargoLock.replace(/(name = "alpha"\nversion = ")1\.0\.0/, '$11.0.9').replace(/(name = "beta"\nversion = ")1\.2\.0/, '$11.9.0'));
    assert.deepEqual(JSON.parse(read(repo, 'web/package.json')), { name: 'web', dependencies: { broken: '1.0.0', left: '^1.4.0', pinned: '2.0.3' }, devDependencies: { dev: '~3.2.0' } });
    assert.equal(read(repo, 'Package.swift'), swiftPackage.replace('from: "1.2.0"', 'from: "1.3.1"'));
    const alpha = report.changes[0].files;
    assert.deepEqual(alpha.map(file => file.path), ['Cargo.toml', 'Cargo.lock']);
    assert.equal(alpha[0].sha256Before, sha(cargoToml));
    const left = report.changes[2].files.find(file => file.path === 'web/pnpm-lock.yaml');
    assert.equal(left.sha256Before, null, 'the lockfile did not exist before the first pnpm row created it');
    const last = report.changes[4].files.find(file => file.path === 'web/pnpm-lock.yaml');
    assert.equal(last.sha256Before, sha('locked: pinned@2.0.3\n'));
    assert.equal(last.sha256After, sha(read(repo, 'web/pnpm-lock.yaml')));
    assert.deepEqual(git(repo, 'diff', '--name-only').split('\n').sort(), ['Cargo.lock', 'Cargo.toml', 'Package.swift', 'web/package.json']);
    assert.equal(git(repo, 'ls-files', '--others', '--exclude-standard'), 'web/pnpm-lock.yaml', 'the state directory ignores itself, so it is not an untracked change');

    // The report on disk is the returned one, and the held rows are written next to it for an agent lane.
    assert.equal(applied.reportPath, path.join(repo, '.rightkit', 'update', 'apply-report.json'));
    assert.deepEqual(JSON.parse(read(repo, '.rightkit/update/apply-report.json')), report);
    const held = JSON.parse(read(repo, '.rightkit/update/held.json'));
    assert.deepEqual([held.schemaVersion, held.repo, held.needsMigration.length, held.held.length, held.failed.length], [1, repo, 2, 1, 2]);
    assert.equal(read(repo, '.rightkit/update/.gitignore'), '*\n');
    assert.ok(fetched.every(url => url.startsWith('https://crates.io/') || url.startsWith('https://api.github.com/')));

    // A second apply refuses to stack on an apply that was never landed, and discard never overwrites later edits.
    const stacked = await outdated.runApply({ repo }, deps);
    assert.equal(stacked.exitCode, 2);
    assert.match(stacked.errors[0], /^a previous apply \(2026-10-11T00:00:00\.000Z\) has 6 change\(s\) that are not landed; run `right-update land` or `right-update discard` first$/);
    appendFileSync(path.join(repo, 'Cargo.toml'), '# edited after apply\n');
    const refused = await outdated.runDiscard({ repo }, { run: runFn });
    assert.equal(refused.exitCode, 2);
    assert.match(refused.errors[0], /^Cargo\.toml changed after apply; refusing to discard over unrelated edits$/);
    assert.equal(read(repo, 'Cargo.toml'), `${expectedToml}# edited after apply\n`, 'a refused discard changes nothing');
    // Landing and discarding an apply whose rows share files (every cargo row shares Cargo.lock) is not asserted: see the README.
    // The repository is put back by hand instead.
    git(repo, 'checkout', '-q', '--', '.');
    rmSync(path.join(repo, 'web', 'pnpm-lock.yaml'), { force: true });
    rmSync(path.join(repo, '.rightkit', 'update'), { recursive: true, force: true });
    restoredToOriginals();
    return { applied: report.changes.length, needsMigration: report.needsMigration.length, held: report.held.length, rolledBack: report.failed.length, dryRunPlanned: dry.report.changes.length, commands: ['rustright cargo update', 'pnpm add'] };
  });

  // ---- 9. `gate`: repository gates and tool journeys run as real child processes, and every outcome has its own exit code.
  const fixture = {};
  await run('gate-journeys-and-exit-codes', async () => {
    const crates = name => `https://crates.io/api/v1/crates/${name}`;
    const macBytes = Buffer.from('fixtool-macos-binary-2.0.0');
    const winBytes = Buffer.from('fixtool-windows-binary-2.0.0');
    const toolSource = {
      kind: 'tool', name: 'fixtool', pin: '1.0.0',
      assets: { 'macos-arm64': { asset: 'fixtool-macos', checksum: `sha256:${A64}` }, 'windows-x86_64': { asset: 'fixtool.exe', checksum: `sha256:${B64}` } },
      upstream: { type: 'url', latest: 'https://example.invalid/fixtool/latest.txt', download: 'https://example.invalid/fixtool/{version}/{asset}' },
      journey: { command: [NODE, 'journey.mjs', '{candidate}', '{out}', '{delivery}', '{model}'], report: '{out}/report.json', inputs: { case: 'smoke' }, model: 'model-x', timeoutSec: 30 },
    };
    const gateScript = [
      "const name = process.argv[2];", "const mode = process.env[`RK_GATE_${name.toUpperCase()}`] ?? 'pass';", "console.log(`gate ${name} ${mode}`);", "console.error(`stderr ${name}`);",
      "if (mode === 'fail') process.exitCode = 7;", "if (mode === 'sleep') setTimeout(() => {}, 60_000);", '',
    ].join('\n');
    const journeyScript = [
      "import { readdirSync, writeFileSync } from 'node:fs';", "import path from 'node:path';", "const [candidate, out, delivery, model] = process.argv.slice(2);",
      "let raw = '';", "for await (const chunk of process.stdin) raw += chunk;", "const mode = process.env.RK_JOURNEY ?? 'pass';",
      "writeFileSync(path.join(out, 'seen.json'), JSON.stringify({ candidate, out, delivery, model, inputs: JSON.parse(raw), files: readdirSync(candidate), mode }));",
      "if (mode === 'exit3') process.exitCode = 3;",
      "else if (mode === 'garbage') writeFileSync(path.join(out, 'report.json'), 'not json');",
      "else if (mode === 'shape') writeFileSync(path.join(out, 'report.json'), JSON.stringify({ ok: 'yes' }));",
      "else if (mode !== 'none') writeFileSync(path.join(out, 'report.json'), JSON.stringify({ ok: mode === 'pass', cases: 1 }));", '',
    ].join('\n');
    const gates = [{ name: 'first', command: [NODE, 'gate.mjs', 'first'] }, { name: 'second', command: [NODE, 'gate.mjs', 'second'], timeoutSec: 3 }];
    const cargoToml = '[dependencies]\nalpha = "1.0.0"\n';
    const cargoLock = lockText({ alpha: '1.0.0' });
    const repo = gitRepo('pipeline', {
      '.rightkit/update.json': manifestOf([{ kind: 'cargo', manifest: 'Cargo.toml' }, toolSource], { default: 'auto-land' }, gates),
      'Cargo.toml': cargoToml, 'Cargo.lock': cargoLock, 'gate.mjs': gateScript, 'journey.mjs': journeyScript,
    });
    const artifacts = path.join(scratch, 'artifacts');
    const host = { platform: process.platform, env: { RIGHTKIT_ARTIFACT_DIR: artifacts } };
    const { fetch } = mockFetch({
      [crates('alpha')]: { crate: { max_stable_version: '1.0.5' } }, 'https://example.invalid/fixtool/latest.txt': '2.0.0\n',
      'https://example.invalid/fixtool/2.0.0/fixtool-macos': macBytes, 'https://example.invalid/fixtool/2.0.0/fixtool.exe': winBytes,
    });
    const { run: runFn, calls } = fakeRun(cargoUpdate(repo));
    const deps = { fetch, exec: noExec, run: runFn, now: () => NOW, toolPlatform: 'macos-arm64', host };
    Object.assign(fixture, { repo, deps, host, artifacts, macBytes, winBytes, cargoToml, cargoLock, manifestText: read(repo, '.rightkit/update.json') });

    // Placeholders are filled in one place and unknown text is left alone.
    assert.equal(outdated.fillPlaceholders('{candidate}|{out}|{delivery}|{model}|{other}|{out}', { candidate: 'C', out: 'O', delivery: 'D', model: 'M' }), 'C|O|D|M|{other}|O');

    // Apply bumps the crate and stages the tool: downloaded, hashed, and not yet trusted.
    const applied = await outdated.runApply({ repo }, deps);
    assert.equal(applied.exitCode, 0);
    assert.deepEqual(applied.report.changes.map(change => [change.kind, change.name, change.before, change.after, change.status]), [['cargo', 'alpha', '1.0.0', '1.0.5', 'applied'], ['tool', 'fixtool', '1.0.0', '2.0.0', 'staged']]);
    const stage = applied.report.changes[1].tool;
    assert.deepEqual([stage.platform, stage.tag, stage.version, stage.asset, stage.binary, stage.trust, stage.size], ['macos-arm64', '2.0.0', '2.0.0', 'fixtool-macos', 'fixtool-macos', 'observed', macBytes.length]);
    assert.equal(stage.sha256, sha(macBytes));
    assert.equal(stage.sourceUrl, 'https://example.invalid/fixtool/2.0.0/fixtool-macos');
    assert.equal(stage.candidateDir, '.rightkit/update/candidates/fixtool/2.0.0');
    assert.equal(stage.previousChecksum, `sha256:${A64}`);
    assert.deepEqual(stage.otherPlatforms, [{ platform: 'windows-x86_64', asset: 'fixtool.exe', size: winBytes.length, sha256: sha(winBytes), trust: 'observed', previousChecksum: `sha256:${B64}` }]);
    assert.deepEqual(readFileSync(path.join(repo, ...stage.candidateDir.split('/'), 'fixtool-macos')), macBytes);
    assert.equal(readdirSync(path.join(repo, ...stage.candidateDir.split('/'))).length, 1, 'only the host platform is stored; the others are hashed and dropped');
    assert.equal(calls.filter(argv => argv[0] === 'rustright').length, 1);

    let tick = 0;
    const gateRun = async (env = {}) => {
      for (const [key, value] of Object.entries(env)) process.env[key] = value;
      try { return await outdated.runGateCommand({ repo }, { run: runFn, host, now: () => new Date(NOW.getTime() + 1000 * ++tick) }); }
      finally { for (const key of Object.keys(env)) delete process.env[key]; }
    };
    const gateNames = outcome => outcome.report.gates.map(gate => [gate.name, gate.ok]);

    // Everything passes: both gates, then the journey, which saw the placeholders, its inputs on stdin and the staged candidate.
    const passing = await gateRun();
    assert.deepEqual([passing.exitCode, passing.errors, passing.report.passed, passing.report.exitCode], [0, [], true, 0]);
    assert.equal(passing.reportPath, path.join(repo, '.rightkit', 'update', 'gate-report.json'));
    assert.deepEqual(gateNames(passing), [['first', true], ['second', true]]);
    assert.equal(passing.report.applyReportSha256, sha(readFileSync(path.join(repo, '.rightkit', 'update', 'apply-report.json'))));
    assert.deepEqual(passing.report.warnings, []);
    assert.deepEqual(JSON.parse(read(repo, '.rightkit/update/gate-report.json')), passing.report);
    const journey = passing.report.journeys[0];
    assert.deepEqual([journey.name, journey.ok, journey.inconclusive, journey.exitCode, journey.timedOut], ['fixtool', true, false, 0, false]);
    const seen = JSON.parse(read(repo, `${journey.outDir}/seen.json`));
    assert.deepEqual(seen, {
      candidate: path.join(repo, '.rightkit', 'update', 'candidates', 'fixtool', '2.0.0'), out: path.join(repo, ...journey.outDir.split('/')),
      delivery: path.join(artifacts, 'fixtool', '2.0.0'), model: 'model-x', inputs: { case: 'smoke' }, files: ['fixtool-macos'], mode: 'pass',
    });
    assert.deepEqual(JSON.parse(read(repo, journey.reportPath)), { ok: true, cases: 1 });
    assert.equal(read(repo, passing.report.gates[0].stdoutPath).trim(), 'gate first pass');
    assert.equal(read(repo, passing.report.gates[0].stderrPath).trim(), 'stderr first');
    assert.match(passing.report.gates[1].stdoutPath, /^\.rightkit\/update\/gate\/2026[0-9]{4}T[0-9]{6}Z\/gates\/second\.stdout\.log$/);

    // A journey says ok:false (1), reports inconclusive (3), writes no report (3), or writes a report it cannot read (3).
    const cases = [
      ['fail', 1, false, '"ok": false'], ['exit3', 3, true, 'journey reported inconclusive (exit 3)'],
      ['none', 3, true, 'report not found: '], ['garbage', 3, true, 'report is not valid JSON: '], ['shape', 3, true, 'report must be a JSON object with a boolean "ok"'],
    ];
    for (const [mode, exitCode, inconclusive, detail] of cases) {
      const outcome = await gateRun({ RK_JOURNEY: mode });
      assert.deepEqual([outcome.exitCode, outcome.report.passed, outcome.report.journeys[0].ok, outcome.report.journeys[0].inconclusive], [exitCode, false, false, inconclusive], mode);
      assert.ok(outcome.report.journeys[0].detail.includes(detail), `${mode}: ${outcome.report.journeys[0].detail}`);
      assert.deepEqual(gateNames(outcome), [['first', true], ['second', true]], 'the gates run whatever the journey does');
    }

    // A failing gate does not stop the next one; a gate that outlives its deadline is killed; a failure outranks an inconclusive journey.
    const failing = await gateRun({ RK_GATE_FIRST: 'fail' });
    assert.deepEqual([failing.exitCode, gateNames(failing)], [1, [['first', false], ['second', true]]]);
    assert.deepEqual([failing.report.gates[0].exitCode, failing.report.gates[0].detail, failing.report.gates[0].timedOut], [7, 'exit 7', false]);
    assert.equal(read(repo, failing.report.gates[0].stdoutPath).trim(), 'gate first fail');
    const slow = await gateRun({ RK_GATE_SECOND: 'sleep' });
    assert.deepEqual([slow.exitCode, gateNames(slow)], [1, [['first', true], ['second', false]]]);
    assert.deepEqual([slow.report.gates[1].timedOut, slow.report.gates[1].detail], [true, 'timed out after 3s']);
    assert.notEqual(slow.report.gates[1].exitCode, 0, 'a killed gate never reports success (no exit code on POSIX, the taskkill code on Windows)');
    assert.ok(slow.report.gates[1].durationMs < 30_000);
    const both = await gateRun({ RK_GATE_FIRST: 'fail', RK_JOURNEY: 'exit3' });
    assert.equal(both.exitCode, 1, 'a failed gate outranks an inconclusive journey');

    // Refusals before anything runs: an unreadable apply-report, and a manifest that is gone.
    const reportFile = path.join(repo, '.rightkit', 'update', 'apply-report.json');
    const reportBytes = readFileSync(reportFile);
    writeFileSync(reportFile, '{ not json');
    const unreadable = await gateRun();
    assert.equal(unreadable.exitCode, 2);
    assert.match(unreadable.errors[0], /apply-report\.json is not valid JSON/);
    writeFileSync(reportFile, reportBytes);
    const noManifest = await outdated.runGateCommand({ repo: makeDir('gate-none') }, { run: runFn });
    assert.equal(noManifest.exitCode, 2);
    assert.match(noManifest.errors[0], /cannot read/);

    // The last run leaves a report that matches the current apply and passes: the next scenario lands it.
    const last = await gateRun();
    assert.equal(last.exitCode, 0);
    const runs = readdirSync(path.join(repo, '.rightkit', 'update', 'gate'));
    assert.equal(runs.length, tick, 'every run that started keeps its own directory of logs; the run refused for its unreadable apply-report started none');
    return { gateRuns: tick, journeyOutcomes: ['pass', ...cases.map(entry => entry[0])], exitCodes: [0, 1, 2, 3], gatesPerRun: 2 };
  });

  // ---- 10. `land`: one commit for the verified apply, the tool delivered with a receipt, and the manifest rewritten.
  await run('land-delivery-receipt-and-manifest-rewrite', async () => {
    const { repo, deps, host, artifacts, macBytes, winBytes, cargoToml, cargoLock, manifestText } = fixture;
    const apply = JSON.parse(read(repo, '.rightkit/update/apply-report.json'));
    const toolChange = apply.changes[1];
    const head = git(repo, 'rev-parse', 'HEAD');
    const landDeps = { run: deps.run, host, now: () => NOW };
    const appliedToml = cargoToml.replace('"1.0.0"', '"1.0.5"');

    // Land refuses anything that is not exactly what apply and gate saw.
    writeFileSync(path.join(repo, 'wip.txt'), 'work in progress');
    const foreign = await outdated.runLand({ repo }, landDeps);
    assert.equal(foreign.exitCode, 1);
    assert.match(foreign.refusals[0], /^working tree has changes outside the apply-report: wip\.txt$/);
    rmSync(path.join(repo, 'wip.txt'));
    writeFileSync(path.join(repo, 'Cargo.toml'), `${appliedToml}# tampered\n`);
    const tampered = await outdated.runLand({ repo }, landDeps);
    assert.equal(tampered.exitCode, 1);
    assert.match(tampered.refusals[0], /^Cargo\.toml changed since apply \(expected [0-9a-f]{64}, found [0-9a-f]{64}\)$/);
    writeFileSync(path.join(repo, 'Cargo.toml'), appliedToml);
    appendFileSync(path.join(repo, '.rightkit', 'update.json'), '\n');
    const manifestEdited = await outdated.runLand({ repo }, landDeps);
    assert.equal(manifestEdited.exitCode, 1);
    assert.match(manifestEdited.refusals[0], /outside the apply-report: \.rightkit\/update\.json$/);
    git(repo, 'checkout', '-q', '--', '.rightkit/update.json');
    assert.equal(git(repo, 'rev-parse', 'HEAD'), head, 'refusals commit nothing');
    assert.equal(existsSync(path.join(artifacts, 'fixtool')), false, 'and deliver nothing');

    // The pure helpers first: the commit message and the targeted manifest rewrite.
    const message = outdated.commitMessage(apply.changes);
    assert.equal(message, `deps: 2 updates via right-update\n\n- cargo alpha 1.0.0 -> 1.0.5 (Cargo.toml)\n- tool fixtool 1.0.0 -> 2.0.0 (sha256:${sha(macBytes).slice(0, 12)}, macos-arm64)\n\nCo-Authored-By: right-update <noreply@rightkit.dev>\n`);
    const expected = JSON.parse(manifestText);
    const expectedTool = expected.sources.find(source => source.kind === 'tool');
    expectedTool.pin = '2.0.0';
    expectedTool.assets['macos-arm64'].checksum = `sha256:${sha(macBytes)}`;
    expectedTool.assets['windows-x86_64'].checksum = `sha256:${sha(winBytes)}`;
    assert.equal(outdated.rewriteToolInManifest(manifestText, toolChange), `${JSON.stringify(expected, null, 2)}\n`);
    const minified = JSON.stringify(JSON.parse(manifestText));
    const compact = outdated.rewriteToolInManifest(minified, toolChange);
    assert.equal(compact.includes('\n'), false, 'a hand-formatted manifest keeps its formatting when every literal is unique');
    assert.deepEqual(JSON.parse(compact), expected);
    assert.throws(() => outdated.rewriteToolInManifest(manifestText.replace('"pin": "1.0.0"', '"pin": "9.9.9"'), toolChange), /pin is 9\.9\.9, apply-report expected 1\.0\.0/);
    assert.throws(() => outdated.rewriteToolInManifest(manifestText.replace(A64, 'c'.repeat(64)), toolChange), /checksum for fixtool macos-arm64 changed since apply/);
    assert.throws(() => outdated.rewriteToolInManifest(manifestText, { ...toolChange, name: 'ghost' }), /tool ghost not found in \.rightkit\/update\.json/);

    // Land: one commit that names exactly the files apply changed, the delivery directory, and the receipt.
    const landed = await outdated.runLand({ repo }, landDeps);
    assert.deepEqual([landed.exitCode, landed.errors, landed.refusals, landed.committed, landed.pushed, landed.changes, landed.held], [0, [], [], true, false, 2, 0]);
    assert.equal(landed.commit, git(repo, 'rev-parse', 'HEAD'));
    assert.notEqual(landed.commit, head);
    assert.equal(git(repo, 'rev-list', '--count', 'HEAD'), '2');
    assert.equal(git(repo, 'log', '-1', '--format=%B').trim(), message.trim());
    assert.deepEqual(git(repo, 'show', '--name-only', '--format=', 'HEAD').split('\n').sort(), ['.rightkit/update.json', 'Cargo.lock', 'Cargo.toml']);
    assert.equal(git(repo, 'status', '--porcelain'), '', 'the working tree is clean after landing');
    assert.equal(read(repo, 'Cargo.toml'), appliedToml);
    assert.equal(read(repo, 'Cargo.lock'), cargoLock.replace(/(name = "alpha"\nversion = ")1\.0\.0/, '$11.0.5'));
    assert.deepEqual(JSON.parse(read(repo, '.rightkit/update.json')), expected);
    const delivery = path.join(artifacts, 'fixtool', '2.0.0');
    assert.deepEqual(landed.delivered, [delivery]);
    assert.deepEqual(readFileSync(path.join(delivery, 'fixtool-macos')), macBytes);
    assert.deepEqual(JSON.parse(read(delivery, 'receipt.json')), {
      schemaVersion: 1, name: 'fixtool', version: '2.0.0', platform: 'macos-arm64', asset: 'fixtool-macos', binary: 'fixtool-macos', sha256: sha(macBytes), size: macBytes.length,
      sourceUrl: 'https://example.invalid/fixtool/2.0.0/fixtool-macos', deliveredAt: '2026-10-11T00:00:00.000Z', deliveredBy: 'right-update',
    });
    assert.equal(existsSync(path.join(repo, '.rightkit', 'update', 'candidates')), false, 'the staged candidate moved to the delivery directory');
    const marked = JSON.parse(read(repo, '.rightkit/update/apply-report.json'));
    assert.deepEqual(marked.landed, { commit: landed.commit, at: '2026-10-11T00:00:00.000Z' });
    assert.equal(JSON.parse(read(repo, '.rightkit/update/held.json')).held.length, 0);

    // Landing twice is refused, and the tool's new pin is what the next check compares against.
    const again = await outdated.runLand({ repo }, landDeps);
    assert.equal(again.exitCode, 1);
    assert.equal(again.refusals[0], `the apply-report was already landed in ${landed.commit}`);
    assert.equal(git(repo, 'rev-list', '--count', 'HEAD'), '2');
    const next = await outdated.runCheck(repo, { fetch: mockFetch({ [`https://crates.io/api/v1/crates/alpha`]: { crate: { max_stable_version: '1.0.5' } }, 'https://example.invalid/fixtool/latest.txt': '2.0.0' }).fetch, exec: noExec });
    assert.deepEqual([next.exitCode, next.entries.map(entry => [entry.name, entry.status])], [0, [['alpha', 'current'], ['fixtool', 'current']]]);
    return { commits: 2, files: ['.rightkit/update.json', 'Cargo.lock', 'Cargo.toml'], delivered: 'fixtool 2.0.0 with receipt', refusals: ['foreign file', 'tampered file', 'edited manifest', 'already landed'] };
  });

  // ---- Fixtures for the single-crate repositories used by the next scenarios.
  const cratesUrl = name => `https://crates.io/api/v1/crates/${name}`;
  const cargoRepo = (label, { policy = { default: 'auto-land' }, gates = [{ name: 'ok', command: [NODE, 'ok.mjs'] }], files = {} } = {}) => gitRepo(label, {
    '.rightkit/update.json': manifestOf([{ kind: 'cargo', manifest: 'Cargo.toml' }], policy, gates),
    'Cargo.toml': '[dependencies]\nalpha = "1.0.0"\n', 'Cargo.lock': lockText({ alpha: '1.0.0' }), 'ok.mjs': '// exits 0\n', 'fail.mjs': 'process.exit(1);\n', ...files,
  });
  const cargoDeps = (repo, latest = '1.0.5') => ({ fetch: mockFetch({ [cratesUrl('alpha')]: { crate: { max_stable_version: latest } } }).fetch, exec: noExec, run: fakeRun(cargoUpdate(repo)).run, now: () => NOW });

  // ---- 11. Land preconditions, discard, and pushing to a remote.
  await run('land-refusals-discard-and-push', async () => {
    const repo = cargoRepo('land-refusals');
    const deps = cargoDeps(repo);
    const originals = { 'Cargo.toml': read(repo, 'Cargo.toml'), 'Cargo.lock': read(repo, 'Cargo.lock') };
    const none = await outdated.runLand({ repo }, deps);
    assert.equal(none.exitCode, 1);
    assert.match(none.refusals[0], /^no usable apply-report: cannot read /);
    assert.equal(none.committed, false);
    const applied = await outdated.runApply({ repo }, deps);
    assert.deepEqual([applied.exitCode, applied.report.changes.length], [0, 1]);
    const early = await outdated.runLand({ repo }, deps);
    assert.deepEqual([early.exitCode, early.changes], [1, 1]);
    assert.match(early.refusals[0], /^no usable gate-report: cannot read /);
    const gated = await outdated.runGateCommand({ repo }, deps);
    assert.deepEqual([gated.exitCode, gated.report.passed, gated.report.gates.length, gated.report.journeys.length], [0, true, 1, 0]);
    const base = git(repo, 'rev-parse', 'HEAD');

    git(repo, 'commit', '-q', '--allow-empty', '-m', 'moved');
    const moved = await outdated.runLand({ repo }, deps);
    assert.equal(moved.exitCode, 1);
    assert.match(moved.refusals[0], new RegExp(`^HEAD moved since apply \\(${base} -> [0-9a-f]{40}\\); run apply again$`));
    git(repo, 'reset', '-q', '--soft', 'HEAD~1');
    git(repo, 'checkout', '-q', '-b', 'topic');
    const offBranch = await outdated.runLand({ repo }, deps);
    assert.equal(offBranch.refusals[0], 'current branch topic is not the default branch (main)');
    git(repo, 'checkout', '-q', 'main');
    git(repo, 'checkout', '-q', '--detach');
    assert.equal((await outdated.runLand({ repo }, deps)).refusals[0], 'HEAD is detached; land only commits on the default branch');
    git(repo, 'checkout', '-q', 'main');

    const gateFile = path.join(repo, '.rightkit', 'update', 'gate-report.json');
    const gateBytes = readFileSync(gateFile);
    const gateReport = JSON.parse(gateBytes.toString('utf8'));
    writeFileSync(gateFile, JSON.stringify({ ...gateReport, applyReportSha256: 'f'.repeat(64) }));
    assert.equal((await outdated.runLand({ repo }, deps)).refusals[0], 'the gate-report was produced for a different apply-report; run gate again');
    writeFileSync(gateFile, JSON.stringify({ ...gateReport, passed: false, exitCode: 1 }));
    assert.equal((await outdated.runLand({ repo }, deps)).refusals[0], 'the gate did not pass (exit 1)');
    writeFileSync(gateFile, gateBytes);
    assert.equal(git(repo, 'rev-list', '--count', 'HEAD'), '1', 'every refusal left the history alone');

    // Discard puts the files back byte for byte and removes both reports; a second discard has nothing to do.
    const discarded = await outdated.runDiscard({ repo }, deps);
    assert.deepEqual([discarded.exitCode, discarded.errors, [...discarded.restored].sort()], [0, [], ['Cargo.lock', 'Cargo.toml']]);
    for (const [rel, text] of Object.entries(originals)) assert.equal(read(repo, rel), text, rel);
    assert.equal(git(repo, 'status', '--porcelain'), '');
    assert.deepEqual([existsSync(path.join(repo, '.rightkit', 'update', 'apply-report.json')), existsSync(gateFile)], [false, false]);
    const nothing = await outdated.runDiscard({ repo }, deps);
    assert.equal(nothing.exitCode, 2);
    assert.match(nothing.errors[0], /^cannot read /);

    // policy.land "push": the commit goes to the remote of the current branch. A local bare repository stands in for it.
    const remote = path.join(makeDir('remote'), 'origin.git');
    git(scratch, 'init', '-q', '--bare', '-b', 'main', remote);
    const pushRepo = cargoRepo('land-push', { policy: { default: 'auto-land', land: 'push' } });
    git(pushRepo, 'remote', 'add', 'origin', remote);
    git(pushRepo, 'push', '-q', '-u', 'origin', 'main');
    const pushDeps = cargoDeps(pushRepo);
    assert.equal((await outdated.runApply({ repo: pushRepo }, pushDeps)).exitCode, 0);
    assert.equal((await outdated.runGateCommand({ repo: pushRepo }, pushDeps)).exitCode, 0);
    const pushed = await outdated.runLand({ repo: pushRepo }, pushDeps);
    assert.deepEqual([pushed.exitCode, pushed.committed, pushed.pushed, pushed.pushVia, pushed.errors], [0, true, true, 'git', []]);
    assert.equal(git(scratch, '--git-dir', remote, 'rev-parse', 'main'), pushed.commit);
    assert.equal(git(scratch, '--git-dir', remote, 'log', '-1', '--format=%s', 'main'), 'deps: 1 updates via right-update');

    // A push that fails leaves the commit local and says so, with exit 2.
    const strandedRepo = cargoRepo('land-push-fail', { policy: { default: 'auto-land', land: 'push' } });
    git(strandedRepo, 'remote', 'add', 'origin', path.join(scratch, 'nowhere.git'));
    git(strandedRepo, 'config', 'branch.main.remote', 'origin');
    git(strandedRepo, 'config', 'branch.main.merge', 'refs/heads/main');
    const strandedDeps = cargoDeps(strandedRepo);
    assert.equal((await outdated.runApply({ repo: strandedRepo }, strandedDeps)).exitCode, 0);
    assert.equal((await outdated.runGateCommand({ repo: strandedRepo }, strandedDeps)).exitCode, 0);
    const stranded = await outdated.runLand({ repo: strandedRepo }, strandedDeps);
    assert.deepEqual([stranded.exitCode, stranded.committed, stranded.pushed], [2, true, false]);
    assert.match(stranded.errors[0], /^git push failed \(the commit [0-9a-f]{40} is local\): /);
    assert.equal(stranded.commit, git(strandedRepo, 'rev-parse', 'HEAD'));
    return { refusals: ['no apply-report', 'no gate-report', 'HEAD moved', 'other branch', 'detached HEAD', 'gate for another apply', 'gate not passed'], discard: 'byte-for-byte', push: ['pushed to a bare remote', 'failed push keeps the local commit'] };
  });

  // ---- 12. The CLI entry: output formats, flag errors, and the scheduled `run` pipeline with its machine-readable events.
  await run('cli-dispatch-and-scheduled-run-pipeline', async () => {
    const drive = (repo, args, { latest = '1.0.5', routes } = {}) => {
      const out = [];
      const err = [];
      const { fetch } = mockFetch(routes ?? { [cratesUrl('alpha')]: { crate: { max_stable_version: latest } } });
      const { run: runFn, calls } = fakeRun(cargoUpdate(repo));
      const io = { stdout: text => out.push(text), stderr: text => err.push(text), deps: { fetch, run: runFn, exec: noExec }, now: () => NOW };
      return {
        go: () => outdated.runCli(args, io), out: () => out.join(''), err: () => err.join(''), calls,
        events: () => out.join('').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)),
      };
    };
    const subject = (events, name) => events.filter(event => event.event === name);

    // Version, help and usage.
    const none = drive(makeDir('cli'), []);
    assert.equal(await none.go(), 2);
    assert.match(none.out(), /^right-update: per-repo dependency updater\n\nUsage:\n/);
    for (const command of ['check', 'apply', 'gate', 'land', 'discard', 'run', 'schedule']) assert.match(none.out(), new RegExp(`\\n  right-update ${command} `), command);
    for (const flag of ['-h', '--help']) { const help = drive(makeDir('cli'), [flag]); assert.equal(await help.go(), 0); assert.equal(help.out(), none.out()); }
    for (const flag of ['--version', '-v']) { const versionRun = drive(makeDir('cli'), [flag]); assert.equal(await versionRun.go(), 0); assert.equal(versionRun.out(), `${version}\n`); }
    const unknown = drive(makeDir('cli'), ['frobnicate']);
    assert.equal(await unknown.go(), 2);
    assert.match(unknown.err(), /^right-update: unknown command: frobnicate\n\nright-update: per-repo dependency updater/);
    for (const [args, message] of [
      [['check', '--nope'], 'unknown argument: --nope'], [['check', 'extra'], 'unknown argument: extra'], [['check', '--repo'], '--repo requires a value'],
      [['check', '--repo', '--json'], '--repo requires a value'], [['apply', '--only'], '--only requires at least one name'], [['schedule', 'install', '--interval', 'abc'], '--interval must be a whole number of minutes'],
      [['check', '--json=1'], 'unknown argument: --json=1'],
    ]) {
      const refused = drive(makeDir('cli'), args);
      assert.equal(await refused.go(), 2, args.join(' '));
      assert.equal(refused.err(), `right-update: ${message}\n`, args.join(' '));
    }
    const badSchedule = drive(makeDir('cli'), ['schedule', 'bogus']);
    assert.equal(await badSchedule.go(), 2);
    assert.match(badSchedule.err(), /schedule needs one of: install, remove, status/);

    // check: the text table and the JSON report agree.
    const repo = cargoRepo('cli-steps');
    const checkText = drive(repo, ['check', '--repo', repo]);
    assert.equal(await checkText.go(), 1);
    assert.match(checkText.out(), /^right-update check: .* \(\.rightkit\/update\.json\)\n  status +kind +name +pinned +latest +policy *\n  outdated +cargo +alpha +1\.0\.0 +1\.0\.5 +auto-land *\n  0 current, 1 outdated, 0 unknown, exit 1\n$/);
    const checkJson = drive(repo, ['check', '--repo', repo, '--json']);
    assert.equal(await checkJson.go(), 1);
    assert.deepEqual(JSON.parse(checkJson.out()), { schemaVersion: 1, repo, exitCode: 1, errors: [], entries: [{ name: 'alpha', kind: 'cargo', source: 'Cargo.toml', pinned: '1.0.0', latest: '1.0.5', status: 'outdated', policy: 'auto-land' }] });
    const checkQuiet = drive(repo, ['check', '--repo', repo, '--quiet']);
    assert.equal(await checkQuiet.go(), 1);
    assert.deepEqual(checkQuiet.events().map(event => event.event), ['update.run.start', 'update.run.end']);
    assert.deepEqual(checkQuiet.events()[1], { event: 'update.run.end', ts: '2026-10-11T00:00:00.000Z', command: 'check', repo, exitCode: 1, durationMs: checkQuiet.events()[1].durationMs, entries: 1, outdated: 1 });
    const lookupFails = drive(repo, ['check', '--repo', repo], { routes: {} });
    assert.equal(await lookupFails.go(), 2);
    assert.match(lookupFails.out(), /error cargo:alpha: crates\.io lookup failed: GET https:\/\/crates\.io\/api\/v1\/crates\/alpha returned HTTP 404/);

    // apply, gate, land and discard one command at a time.
    const applyDry = drive(repo, ['apply', '--repo', repo, '--dry-run', '--json']);
    assert.equal(await applyDry.go(), 0);
    assert.deepEqual([JSON.parse(applyDry.out()).report.changes[0].status, JSON.parse(applyDry.out()).reportPath], ['planned', null]);
    const applyText = drive(repo, ['apply', '--repo', repo, '--only', 'alpha']);
    assert.equal(await applyText.go(), 0);
    assert.match(applyText.out(), /^right-update apply: .*\n  applied cargo alpha 1\.0\.0 -> 1\.0\.5 \[Cargo\.toml, Cargo\.lock\]\n  1 changed, 0 need migration, 0 held, 0 skipped, 0 failed, exit 0\n  report: .*apply-report\.json\n$/);
    const landEarly = drive(repo, ['land', '--repo', repo]);
    assert.equal(await landEarly.go(), 1);
    assert.match(landEarly.out(), /^right-update land\n  refused: no usable gate-report: /);
    const gateText = drive(repo, ['gate', '--repo', repo]);
    assert.equal(await gateText.go(), 0);
    assert.match(gateText.out(), /^right-update gate\n  gate ok   ok \(\d+ ms\)\n  passed, exit 0\n$/);
    const landText = drive(repo, ['land', '--repo', repo]);
    assert.equal(await landText.go(), 0);
    assert.match(landText.out(), /^right-update land\n  committed [0-9a-f]{40} \(not pushed\)\n  1 change\(s\), 0 held or awaiting migration, exit 0\n$/);
    assert.equal(git(repo, 'log', '-1', '--format=%s'), 'deps: 1 updates via right-update');
    const discardLanded = drive(repo, ['discard', '--repo', repo, '--json']);
    assert.equal(await discardLanded.go(), 2);
    assert.match(JSON.parse(discardLanded.out()).errors[0], /^already landed in [0-9a-f]{40}; nothing to discard$/);

    // run, success: check, apply, gate and land in one process, logged as JSON lines.
    const winner = cargoRepo('cli-run');
    const win = drive(winner, ['run', '--repo', winner, '--quiet']);
    assert.equal(await win.go(), 0);
    const events = win.events();
    assert.deepEqual(events.map(event => event.event), ['update.run.start', 'update.gate.result', 'update.land.result', 'update.run.end']);
    assert.deepEqual(events[0], { event: 'update.run.start', ts: '2026-10-11T00:00:00.000Z', command: 'run', repo: winner, version });
    assert.deepEqual([events[1].ok, events[1].exitCode, events[1].gates.map(gate => [gate.name, gate.ok, gate.exitCode]), events[1].journeys], [true, 0, [['ok', true, 0]], []]);
    assert.deepEqual([events[2].ok, events[2].committed, events[2].pushed, events[2].changes, events[2].commit], [true, true, false, 1, git(winner, 'rev-parse', 'HEAD')]);
    assert.deepEqual([events[3].command, events[3].exitCode, events[3].outcome], ['run', 0, `landed ${git(winner, 'rev-parse', 'HEAD')}`]);
    assert.equal(read(winner, 'Cargo.toml'), '[dependencies]\nalpha = "1.0.5"\n');
    assert.equal(git(winner, 'status', '--porcelain'), '');
    assert.equal(win.out().trim().split('\n').every(line => line.startsWith('{')), true, 'nothing but JSON lines in quiet mode');

    // run, nothing to do, and a plan only.
    const current = cargoRepo('cli-current');
    const nothing = drive(current, ['run', '--repo', current, '--quiet'], { latest: '1.0.0' });
    assert.equal(await nothing.go(), 0);
    assert.equal(subject(nothing.events(), 'update.run.end')[0].outcome, 'all current');
    assert.equal(git(current, 'rev-list', '--count', 'HEAD'), '1');
    const planned = cargoRepo('cli-dry');
    const plan = drive(planned, ['run', '--repo', planned, '--dry-run', '--quiet']);
    assert.equal(await plan.go(), 0);
    assert.equal(subject(plan.events(), 'update.run.end')[0].outcome, 'dry run: 1 change(s) planned');
    assert.equal(git(planned, 'status', '--porcelain'), '');

    // run, a gate fails: the applied changes are discarded, the tree is clean, nothing is committed, exit 1.
    const failing = cargoRepo('cli-gate-fails', { gates: [{ name: 'bad', command: [NODE, 'fail.mjs'] }] });
    const bad = drive(failing, ['run', '--repo', failing, '--quiet']);
    assert.equal(await bad.go(), 1);
    assert.deepEqual(bad.events().map(event => event.event), ['update.run.start', 'update.gate.result', 'update.run.end']);
    assert.deepEqual([bad.events()[1].ok, bad.events()[1].gates[0].exitCode], [false, 1]);
    assert.match(bad.events()[2].outcome, /^gate did not pass \(exit 1\); applied changes were discarded$/);
    assert.equal(read(failing, 'Cargo.toml'), '[dependencies]\nalpha = "1.0.0"\n');
    assert.equal(git(failing, 'status', '--porcelain'), '');
    assert.equal(git(failing, 'rev-list', '--count', 'HEAD'), '1');

    // run, a dirty tree is refused before anything is applied; a failed lookup is exit 2.
    const dirty = cargoRepo('cli-dirty');
    writeFileSync(path.join(dirty, 'wip.txt'), 'work in progress');
    const refused = drive(dirty, ['run', '--repo', dirty, '--quiet']);
    assert.equal(await refused.go(), 1);
    assert.match(refused.events().at(-1).outcome, /^refused: working tree is not clean \(wip\.txt\)$/);
    assert.equal(refused.calls.some(argv => argv[0] === 'rustright'), false);
    const unreachable = cargoRepo('cli-lookup');
    const broken = drive(unreachable, ['run', '--repo', unreachable, '--quiet'], { routes: {} });
    assert.equal(await broken.go(), 2);
    assert.match(broken.events().at(-1).outcome, /^check failed: alpha: crates\.io lookup failed/);
    return { commands: ['check', 'apply', 'gate', 'land', 'discard', 'run', 'schedule'], runOutcomes: ['landed', 'all current', 'dry run', 'gate failed and discarded', 'dirty tree refused', 'lookup failed'], flagErrors: 7 };
  });

  // ---- 13. The scheduled run: launchd and Task Scheduler definitions, and installers driven through injected platforms.
  await run('schedule-renderers-and-injected-installers', async () => {
    const repo = tree('schedule', { '.rightkit/update.json': manifestOf([]) });
    const label = outdated.launchdLabel(repo);
    assert.match(label, /^com\.rightkit\.update\.schedule-\d+-[0-9a-f]{8}$/);
    assert.equal(outdated.launchdLabel(repo), label, 'the label is a pure function of the repository path');
    assert.notEqual(outdated.launchdLabel(path.join(makeDir('elsewhere'), path.basename(repo))), label, 'the same folder name in another place gets another label');
    const slug = label.slice('com.rightkit.update.'.length);
    assert.equal(outdated.windowsTaskName(repo), `\\RightKit\\update-${slug}`);
    for (const [minutes, expected] of [[15, 'PT15M'], [60, 'PT1H'], [90, 'PT1H30M'], [1440, 'P1D'], [1500, 'P1DT1H'], [10080, 'P7D']]) assert.equal(outdated.isoDuration(minutes), expected);

    // launchd: argument order, interval, PATH, logs, and XML escaping of every value.
    const mac = { repo, intervalMinutes: 90, nodePath: '/opt/node/bin/node', binPath: '/opt/right-update/dist/bin.js', pathEnv: '/usr/local/bin:/usr/bin', home: '/Users/me', platform: 'darwin' };
    const plist = outdated.renderLaunchdPlist(mac);
    assert.ok(plist.startsWith('<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist'));
    const strings = block => [...block.matchAll(/<string>([^<]*)<\/string>/g)].map(match => match[1]);
    assert.deepEqual(strings(plist.match(/<key>ProgramArguments<\/key>\s*<array>([\s\S]*?)<\/array>/)[1]), ['/opt/node/bin/node', '/opt/right-update/dist/bin.js', 'run', '--repo', repo, '--quiet']);
    assert.match(plist, new RegExp(`<key>Label</key>\\s*<string>${label.replaceAll('.', '\\.')}</string>`));
    assert.match(plist, /<key>StartInterval<\/key>\s*<integer>5400<\/integer>/);
    assert.match(plist, /<key>RunAtLoad<\/key>\s*<false\/>/);
    assert.match(plist, /<key>PATH<\/key>\s*<string>\/usr\/local\/bin:\/usr\/bin<\/string>/);
    assert.equal(plist.split(`/Users/me/Library/Logs/RightKit/update/${slug}.log`).length - 1, 2, 'one log for stdout and stderr');
    const hostile = outdated.renderLaunchdPlist({ ...mac, repo: '/tmp/a&b<c>"d', nodePath: '/opt/n&n/node' });
    assert.ok(hostile.includes('/tmp/a&amp;b&lt;c&gt;&quot;d') && hostile.includes('/opt/n&amp;n/node'));
    assert.equal(hostile.includes('a&b'), false);

    // Task Scheduler: a hidden, per-user, least-privilege task from XML, with a quoted PowerShell command.
    const win = { repo, intervalMinutes: 90, nodePath: 'C:\\Program Files\\nodejs\\node.exe', binPath: 'C:\\tools\\right-update\\dist\\bin.js', pathEnv: 'C:\\Windows', home: 'C:\\Users\\me', platform: 'win32', localAppData: 'C:\\Users\\me\\AppData\\Local' };
    const args = outdated.windowsTaskArguments(win);
    assert.equal(args, `-WindowStyle Hidden -NonInteractive -NoProfile -Command "& 'C:\\Program Files\\nodejs\\node.exe' 'C:\\tools\\right-update\\dist\\bin.js' run --repo '${repo}' --quiet 2>&1 | Out-File -Append -Encoding utf8 -LiteralPath 'C:\\Users\\me\\AppData\\Local\\RightKit\\Logs\\update\\${slug}.log'"`);
    assert.ok(outdated.windowsTaskArguments({ ...win, repo: "/work/o'brien" }).includes("--repo '/work/o''brien'"), 'a quote in a path is doubled');
    for (const unsafe of ['/work/50%', '/work/a"b', '/work/a\nb']) assert.throws(() => outdated.windowsTaskArguments({ ...win, repo: unsafe }), /cannot be scheduled safely/);
    const taskXml = outdated.renderTaskXml(win, new Date('2026-10-11T08:30:00.123Z'));
    assert.ok(taskXml.startsWith('<?xml version="1.0" encoding="UTF-16"?>'));
    for (const fragment of ['<StartBoundary>2026-10-11T08:30:00</StartBoundary>', '<Interval>PT1H30M</Interval>', '<StopAtDurationEnd>false</StopAtDurationEnd>', '<LogonType>InteractiveToken</LogonType>', '<RunLevel>LeastPrivilege</RunLevel>',
      '<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>', '<StartWhenAvailable>true</StartWhenAvailable>', '<ExecutionTimeLimit>PT4H</ExecutionTimeLimit>', '<Hidden>true</Hidden>', '<Command>powershell.exe</Command>']) {
      assert.ok(taskXml.includes(fragment), fragment);
    }
    const escaped = args.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
    assert.ok(taskXml.includes(`<Arguments>${escaped}</Arguments>`));
    assert.ok(taskXml.includes(`<WorkingDirectory>${repo.replaceAll('&', '&amp;')}</WorkingDirectory>`));
    assert.deepEqual(outdated.windowsCreateArgs(win, 'X.xml'), ['schtasks', '/Create', '/TN', `\\RightKit\\update-${slug}`, '/XML', 'X.xml', '/F']);

    // Installers, with the platform and the process runner injected so nothing real is registered.
    const recorded = [];
    const script = makeDir('schedule-bin');
    writeTree(script, { 'bin.js': '// right-update bin\n' });
    const binFile = realpathSync.native(path.join(script, 'bin.js'));
    const drive = (platform, answers = {}) => {
      const home = makeDir(`home-${platform}`);
      const runner = async argv => { recorded.push(argv); return (answers[argv[1]] ?? answers[argv[0]] ?? ok)(argv); };
      return { home, deps: { run: runner, platform, home, env: { PATH: '/usr/local/bin:/usr/bin', LOCALAPPDATA: path.join(home, 'Local') }, nodePath: process.execPath, binPath: binFile, uid: 501 } };
    };
    const invalid = await outdated.runSchedule('install', { repo, intervalMinutes: 5 }, drive('darwin').deps);
    assert.deepEqual([invalid.exitCode, invalid.installed, invalid.message], [2, false, '--interval must be an integer from 15 to 10080 minutes']);
    assert.equal((await outdated.runSchedule('install', { repo, intervalMinutes: 20_000 }, drive('darwin').deps)).exitCode, 2);
    const bare = makeDir('schedule-bare');
    assert.equal((await outdated.runSchedule('install', { repo: bare }, drive('darwin').deps)).message, `${bare} has no .rightkit/update.json`);
    const linux = await outdated.runSchedule('status', { repo }, drive('linux').deps);
    assert.deepEqual([linux.exitCode, linux.message], [2, 'schedule is supported on macOS and Windows only, not linux']);

    const launchd = drive('darwin');
    const installed = await outdated.runSchedule('install', { repo, intervalMinutes: 90 }, launchd.deps);
    assert.deepEqual([installed.exitCode, installed.label, installed.installed, installed.loaded, installed.intervalMinutes, installed.files.length], [0, label, true, true, 90, 1]);
    assert.deepEqual(recorded.splice(0), [['launchctl', 'bootout', `gui/501/${label}`], ['launchctl', 'bootstrap', 'gui/501', installed.files[0]]]);
    assert.equal(readFileSync(installed.files[0], 'utf8'), outdated.renderLaunchdPlist({ repo, intervalMinutes: 90, nodePath: process.execPath, binPath: binFile, pathEnv: '/usr/local/bin:/usr/bin', home: launchd.home, platform: 'darwin' }));
    assert.equal(installed.message, `installed ${label} (every 90 min); log ${installed.log}`);
    const loaded = await outdated.runSchedule('status', { repo }, launchd.deps);
    assert.deepEqual([loaded.exitCode, loaded.installed, loaded.loaded, loaded.message], [0, true, true, `${label}: loaded`]);
    assert.deepEqual(recorded.splice(0), [['launchctl', 'print', `gui/501/${label}`]]);
    const unloaded = await outdated.runSchedule('status', { repo }, { ...launchd.deps, run: async argv => (argv[1] === 'print' ? failed('Could not find service') : ok()) });
    assert.deepEqual([unloaded.exitCode, unloaded.installed, unloaded.loaded, unloaded.message], [0, true, false, `${label}: plist present but not loaded`]);
    const removed = await outdated.runSchedule('remove', { repo }, launchd.deps);
    assert.deepEqual([removed.exitCode, removed.installed, removed.message, existsSync(installed.files[0])], [0, false, `removed ${label}`, false]);
    assert.equal((await outdated.runSchedule('remove', { repo }, launchd.deps)).message, `${label} was not installed`);
    const absent = await outdated.runSchedule('status', { repo }, launchd.deps);
    assert.deepEqual([absent.exitCode, absent.installed, absent.message], [1, false, `${label} is not installed`]);
    const refusing = drive('darwin', { bootstrap: () => failed('Bootstrap failed: 5: Input/output error') });
    const stuck = await outdated.runSchedule('install', { repo }, refusing.deps);
    assert.deepEqual([stuck.exitCode, stuck.installed, stuck.loaded, existsSync(stuck.files[0])], [2, true, false, true]);
    assert.match(stuck.message, /^wrote .*, but launchctl bootstrap failed: Bootstrap failed: 5/);
    const noBin = await outdated.runSchedule('install', { repo }, { ...launchd.deps, binPath: path.join(script, 'missing.js') });
    assert.match(noBin.message, /^right-update bin script not found: /);
    recorded.length = 0;

    const task = drive('win32');
    const taskInstalled = await outdated.runSchedule('install', { repo, intervalMinutes: 90 }, task.deps);
    const taskName = `\\RightKit\\update-${slug}`;
    assert.deepEqual([taskInstalled.exitCode, taskInstalled.label, taskInstalled.installed, taskInstalled.intervalMinutes], [0, taskName, true, 90]);
    assert.deepEqual(recorded.splice(0), [['schtasks', '/Create', '/TN', taskName, '/XML', taskInstalled.files[0], '/F']]);
    const hostPath = file => (process.platform === 'win32' ? file : file.replaceAll('\\', '/'));
    const xmlBytes = readFileSync(hostPath(taskInstalled.files[0]));
    assert.deepEqual([...xmlBytes.subarray(0, 2)], [0xff, 0xfe], 'schtasks reads UTF-16 with a byte order mark');
    const xmlText = xmlBytes.subarray(2).toString('utf16le');
    assert.ok(xmlText.includes('<Interval>PT1H30M</Interval>') && xmlText.includes('<Command>powershell.exe</Command>') && xmlText.includes(`--repo '${repo}'`.replaceAll('&', '&amp;')));
    const queried = await outdated.runSchedule('status', { repo }, drive('win32', { '/Query': () => ok('TaskName: x\nStatus: Ready\n') }).deps);
    assert.deepEqual([queried.exitCode, queried.installed, queried.message], [0, true, 'TaskName: x\nStatus: Ready']);
    const missingTask = await outdated.runSchedule('status', { repo }, drive('win32', { '/Query': () => failed('ERROR: The system cannot find the file specified.') }).deps);
    assert.deepEqual([missingTask.exitCode, missingTask.installed, missingTask.message], [1, false, `task ${taskName} is not installed`]);
    assert.equal((await outdated.runSchedule('remove', { repo }, task.deps)).message, `removed task ${taskName}`);
    assert.deepEqual(recorded.splice(0), [['schtasks', '/Query', '/TN', taskName, '/FO', 'LIST', '/V'], ['schtasks', '/Query', '/TN', taskName, '/FO', 'LIST', '/V'], ['schtasks', '/Delete', '/TN', taskName, '/F']]);
    const noTask = await outdated.runSchedule('remove', { repo }, drive('win32', { '/Delete': () => failed('ERROR: not found') }).deps);
    assert.deepEqual([noTask.exitCode, noTask.message], [1, `task ${taskName} was not installed`]);
    const createFails = await outdated.runSchedule('install', { repo }, drive('win32', { '/Create': () => failed('Access is denied.') }).deps);
    assert.deepEqual([createFails.exitCode, createFails.installed, createFails.message], [2, false, 'schtasks /Create failed: Access is denied.']);

    // Through the CLI: `schedule install` prints the same result as JSON.
    const out = [];
    const viaCli = drive('darwin');
    const code = await outdated.runCli(['schedule', 'install', '--repo', repo, '--interval', '90', '--json'], { stdout: text => out.push(text), stderr: () => undefined, schedule: viaCli.deps });
    assert.equal(code, 0);
    const printed = JSON.parse(out.join(''));
    assert.deepEqual([printed.action, printed.exitCode, printed.label, printed.intervalMinutes, printed.platform], ['install', 0, label, 90, 'darwin']);
    return { launchd: ['install', 'status', 'remove', 'bootstrap failure'], taskScheduler: ['install', 'status', 'remove', 'create failure'], rendered: ['plist', 'task xml', 'powershell arguments'], intervals: 6 };
  });

  // ---- 14. The installed bin as a process: exit codes and output, with no network needed.
  await run('installed-bin-process-boundaries', async () => {
    let invocations = 0;
    const bin = (args, cwd = scratch) => {
      invocations += 1;
      const outcome = spawnSync(NODE, [binPath, ...args], { cwd, encoding: 'utf8', timeout: 60_000, windowsHide: true, env: process.env });
      return { code: outcome.status, signal: outcome.signal, stdout: outcome.stdout, stderr: outcome.stderr };
    };
    assert.deepEqual(bin(['--version']), { code: 0, signal: null, stdout: `${version}\n`, stderr: '' });
    const usage = bin([]);
    assert.equal(usage.code, 2);
    assert.match(usage.stdout, /^right-update: per-repo dependency updater\n/);
    assert.equal(bin(['--help']).code, 0);
    assert.equal(bin(['--help']).stdout, usage.stdout);
    const unknown = bin(['bogus']);
    assert.deepEqual([unknown.code, unknown.stdout], [2, '']);
    assert.match(unknown.stderr, /^right-update: unknown command: bogus\n/);

    // A repository whose dependencies all come from a path or a git URL needs no lookups, so the real binary runs offline.
    const repo = gitRepo('bin-repo', {
      '.rightkit/update.json': manifestOf([{ kind: 'cargo', manifest: 'Cargo.toml' }]),
      'Cargo.toml': '[dependencies]\nlocal = { path = "../local", version = "0.1.0" }\nupstream = { git = "https://example.invalid/up" }\n',
    });
    const text = bin(['check', '--repo', repo]);
    assert.equal(text.code, 0);
    assert.match(text.stdout, /\n  unknown +cargo +local +- +- +auto-land *\n  unknown +cargo +upstream +- +- +auto-land *\n  0 current, 0 outdated, 2 unknown, exit 0\n$/);
    const json = JSON.parse(bin(['check', '--json'], repo).stdout);
    assert.deepEqual([json.repo, json.exitCode, json.entries.map(entry => [entry.name, entry.status, entry.reason])], [repo, 0, [['local', 'unknown', 'path or git dependency'], ['upstream', 'unknown', 'path or git dependency']]]);
    const quiet = bin(['check', '--repo', repo, '--quiet']).stdout.trim().split('\n').map(line => JSON.parse(line));
    assert.deepEqual(quiet.map(event => [event.event, event.command]), [['update.run.start', 'check'], ['update.run.end', 'check']]);
    assert.equal(quiet[0].version, version);
    const planned = bin(['apply', '--repo', repo, '--dry-run', '--json']);
    assert.equal(planned.code, 0);
    assert.deepEqual([JSON.parse(planned.stdout).report.dryRun, JSON.parse(planned.stdout).report.changes], [true, []]);
    assert.equal(existsSync(path.join(repo, '.rightkit', 'update')), false, 'a dry run writes no state');
    const gated = bin(['gate', '--repo', repo, '--json']);
    assert.equal(gated.code, 0);
    assert.deepEqual([JSON.parse(gated.stdout).report.passed, JSON.parse(gated.stdout).report.warnings], [true, ['the manifest declares no gates']]);
    assert.equal(bin(['land', '--repo', repo]).code, 1);
    assert.match(bin(['land', '--repo', repo]).stdout, /refused: no usable apply-report/);
    assert.equal(bin(['discard', '--repo', repo, '--json']).code, 2);
    const pipeline = bin(['run', '--repo', repo, '--quiet']);
    assert.equal(pipeline.code, 0);
    assert.equal(JSON.parse(pipeline.stdout.trim().split('\n').at(-1)).outcome, 'all current');

    // Errors name the problem and keep stdout and stderr apart.
    const empty = makeDir('bin-empty');
    const missing = bin(['check', '--repo', empty]);
    assert.equal(missing.code, 2);
    assert.match(missing.stdout, /error: cannot read .*update\.json/);
    const flag = bin(['check', '--nope']);
    assert.deepEqual([flag.code, flag.stdout, flag.stderr], [2, '', 'right-update: unknown argument: --nope\n']);
    assert.equal(bin(['schedule', 'bogus']).code, 2);
    return { invocations, exitCodes: [0, 1, 2], offline: true };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch (error) {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
} finally {
  // Removing the temporary repositories is best effort: a failure here must not hide the result below.
  try { if (scratch) rmSync(scratch, { recursive: true, force: true, maxRetries: 3 }); } catch { /* left to the OS temp cleaner */ }
}
process.stdout.write(`${JSON.stringify(result)}\n`);
