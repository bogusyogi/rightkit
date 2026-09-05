import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { loadBrokerConfig } from './config.mjs';
import {
  buildConfig, defaultBounds, defaultCargoBuildJobs, discoverTools, ensureMacOSRustToolLoaderLinks, parseArgs, renderPlist, resolveVolumeId, rustcVersionTriple, shimTable, stageManagedTools, supportsClientSideSccache, whichUnmanaged,
} from './bootstrap-macos-cli.mjs';

const macOSOnly = { skip: process.platform === 'win32' ? 'macOS-only test' : false };

test('parseArgs accepts the documented flags & rejects anything else', () => {
  assert.deepEqual(parseArgs([]), { force: false, drainTimeoutMs: 0, timeoutMs: 120_000 });
  assert.deepEqual(parseArgs(['--force']), { force: true, drainTimeoutMs: 0, timeoutMs: 120_000 });
  assert.deepEqual(parseArgs(['--external-root', '/x', '--node', '/y', '--drain-timeout-ms', '5000', '--timeout-ms', '90000']), { force: false, drainTimeoutMs: 5000, timeoutMs: 90_000, externalRoot: '/x', nodePath: '/y' });
  assert.throws(() => parseArgs(['--drain-timeout-ms', '-1']), /non-negative integer/);
  assert.throws(() => parseArgs(['--timeout-ms', '99']), /at least 100/);
  assert.throws(() => parseArgs(['--bogus']), /unknown bootstrap argument/);
});

test('shimTable only maps shims for tools actually discovered', () => {
  assert.deepEqual(shimTable([{ id: 'cargo' }, { id: 'clang' }]), { cargo: 'cargo', cc: 'clang', 'c++': 'clang', clang: 'clang' });
  assert.deepEqual(shimTable([]), {});
});

test('discoverTools finds real, hashed tools on this machine’s PATH', macOSOnly, async () => {
  const tools = await discoverTools();
  // node itself proves the process ran & this machine has a real toolchain;
  // cargo is required by every other assertion this session has made about it.
  const cargo = tools.find((tool) => tool.id === 'cargo');
  assert.ok(cargo, 'cargo must be discoverable on PATH for every claim this session made about it to be testable');
  assert.match(cargo.sha256, /^[a-f0-9]{64}$/);
  assert.ok(cargo.path.startsWith('/'));
  for (const tool of tools) assert.equal(tool.root, tool.path.split('/').slice(0, -1).join('/'));
});

test('tool discovery skips RightKit agent shims & Cargo guard wrappers', macOSOnly, async (t) => {
  const originalPath = process.env.PATH;
  const scratch = mkdtempSync(join(tmpdir(), 'rk-bootstrap-recursion-'));
  t.after(() => { rmSync(scratch, { recursive: true, force: true }); process.env.PATH = originalPath; });
  const shim = join(scratch, 'shim'); const real = join(scratch, 'real'); mkdirSync(shim); mkdirSync(real);
  writeFileSync(join(shim, 'sccache'), '#!/bin/sh\n# rightkit.build-control.agent-shims.install.v1\nexit 1\n', { mode: 0o755 });
  writeFileSync(join(real, 'sccache'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  process.env.PATH = `${shim}:${real}:${originalPath}`;
  assert.equal(await whichUnmanaged('sccache'), realpathSync(join(real, 'sccache')));
});

// ImmutableToolRegistry rejects symlinks outright (a security property that
// stays), so discovery must never record one: Homebrew ships sccache as
// bin/sccache -> ../Cellar/…, and registering that path makes the broker refuse
// to start with ToolChangedError(unavailable). bootstrap-windows-cli.mjs
// resolves identically; `where.exe` cannot run here, so only macOS is asserted.
test('discoverTools records a symlinked tool as its realpath, not the link', macOSOnly, async (t) => {
  const originalPath = process.env.PATH;
  const scratch = mkdtempSync(join(tmpdir(), 'rk-symlink-tool-'));
  t.after(() => { rmSync(scratch, { recursive: true, force: true }); process.env.PATH = originalPath; });
  const cellar = join(scratch, 'Cellar', 'bin');
  const bin = join(scratch, 'bin');
  mkdirSync(cellar, { recursive: true }); mkdirSync(bin, { recursive: true });
  // macOS resolves /var itself to /private/var, so compare fully-resolved paths.
  const target = join(realpathSync(cellar), 'sccache');
  writeFileSync(target, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  symlinkSync(join('..', 'Cellar', 'bin', 'sccache'), join(bin, 'sccache'));

  process.env.PATH = `${bin}:${originalPath}`;
  const tools = await discoverTools();
  const sccache = tools.find((tool) => tool.id === 'sccache');
  assert.ok(sccache, 'the shadowing symlink must be the discovered sccache');
  assert.equal(sccache.path, target);
  assert.equal(sccache.root, realpathSync(cellar));
  assert.notEqual(sccache.path, join(bin, 'sccache'));
  assert.equal(lstatSync(sccache.path).isSymbolicLink(), false, 'a recorded tool path must never be a symlink');
  // Same inode, so resolving must not perturb the recorded digest.
  assert.equal(sccache.sha256, createHash('sha256').update(readFileSync(join(bin, 'sccache'))).digest('hex'));
});

test('staging preserves bundled tools & copies standalone Cargo tools', async (t) => {
  const scratch = mkdtempSync(join(tmpdir(), 'rk-stage-tools-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const rustcPath = join(scratch, 'toolchain', 'bin', 'rustc');
  const upgradePath = join(scratch, 'cargo-upgrade');
  mkdirSync(join(scratch, 'toolchain', 'bin'), { recursive: true });
  writeFileSync(rustcPath, '#!/bin/sh\n', { mode: 0o755 });
  writeFileSync(upgradePath, '#!/bin/sh\n', { mode: 0o755 });
  const tools = [
    { id: 'rustc', path: rustcPath, root: join(scratch, 'toolchain', 'bin'), sha256: createHash('sha256').update(readFileSync(rustcPath)).digest('hex') },
    { id: 'cargo-upgrade', path: upgradePath, root: scratch, sha256: createHash('sha256').update(readFileSync(upgradePath)).digest('hex') },
  ];
  const [rustc, upgrade] = await stageManagedTools(tools, join(scratch, 'external'));
  assert.equal(rustc.path, rustcPath);
  assert.equal(rustc.root, join(scratch, 'toolchain', 'bin'));
  assert.notEqual(upgrade.path, upgradePath);
  assert.equal(existsSync(upgrade.path), true);
});

test('macOS bootstrap supplies rust-objcopy its toolchain-root libLLVM companion', async (t) => {
  if (process.platform !== 'darwin') return t.skip('macOS-only toolchain layout');
  const scratch = mkdtempSync(join(tmpdir(), 'rk-rust-objcopy-loader-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const cargo = join(scratch, 'bin', 'cargo');
  const library = join(scratch, 'lib', 'libLLVM.dylib');
  const objcopy = join(scratch, 'lib', 'rustlib', 'aarch64-apple-darwin', 'bin', 'rust-objcopy');
  mkdirSync(join(scratch, 'bin'), { recursive: true });
  mkdirSync(join(scratch, 'lib'), { recursive: true });
  mkdirSync(join(scratch, 'lib', 'rustlib', 'aarch64-apple-darwin', 'bin'), { recursive: true });
  writeFileSync(cargo, 'cargo'); writeFileSync(library, 'llvm'); writeFileSync(objcopy, 'objcopy');

  const links = await ensureMacOSRustToolLoaderLinks([{ id: 'cargo', path: cargo }], ['aarch64-apple-darwin']);
  assert.equal(links.length, 1);
  assert.equal(realpathSync(links[0]), realpathSync(library));
  assert.equal((await ensureMacOSRustToolLoaderLinks([{ id: 'cargo', path: cargo }], ['aarch64-apple-darwin'])).length, 1);
});

test('buildConfig produces a config the broker’s own schema accepts', macOSOnly, async (t) => {
  const scratch = mkdtempSync(join(tmpdir(), 'rk-bootstrap-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const config = await buildConfig({ externalRoot: scratch });
  const configPath = join(scratch, 'config.json');
  writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
  // This is the actual validator the live broker runs on startup — if this
  // passes, a bootstrapped config is not just shaped right, it is accepted.
  const loaded = await loadBrokerConfig(configPath);
  assert.equal(loaded.broker.externalRoot.path, scratch);
  assert.equal(loaded.sccache.mode, loaded.defaults.sccacheToolId ? 'managed' : 'disabled');
  assert.equal(loaded.sccache.clientSide, loaded.sccache.mode === 'managed');
  assert.equal(loaded.bounds.buildConcurrency, 3);
  assert.equal(loaded.bounds.analysisConcurrency, 2);
  // macOS previously omitted the aggregate compile budget entirely, so the
  // broker never enforced it there; buildConfig() must now derive one like
  // Windows does.
  assert.ok(Number.isSafeInteger(loaded.bounds.compileBudgetUnits) && loaded.bounds.compileBudgetUnits >= 4);
  const nativeTriple = process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
  const otherTriple = nativeTriple === 'aarch64-apple-darwin' ? 'x86_64-apple-darwin' : 'aarch64-apple-darwin';
  assert.deepEqual(loaded.defaults.targetTriples, [nativeTriple, otherTriple, 'aarch64-apple-ios', 'aarch64-apple-ios-sim']);
  assert.ok(loaded.tools.length > 0, 'a real machine must discover at least one tool');
});

test('renderPlist escapes reserved XML characters in every substituted value', () => {
  const xml = renderPlist({ nodePath: '/a & b/node', brokerScript: '/x<y', configPath: '/c.json', home: '/home/a>b' });
  assert.ok(!xml.includes('/a & b/node'), 'raw ampersand must not appear unescaped');
  assert.match(xml, /<string>\/a &amp; b\/node<\/string>/);
  assert.match(xml, /<string>\/x&lt;y<\/string>/);
  assert.match(xml, /<key>Label<\/key>\s*<string>dev\.rightkit\.build-control-managed<\/string>/);
  // Exactly the three ProgramArguments the cutover CLI's own validator requires.
  const args = [...xml.matchAll(/<array>([\s\S]*?)<\/array>/g)][0][1].match(/<string>([\s\S]*?)<\/string>/g);
  assert.equal(args.length, 3);
});

test('resolveVolumeId & rustcVersionTriple degrade to a safe default instead of throwing', async () => {
  const volumeId = await resolveVolumeId('/definitely/not/a/real/volume/path/xyz');
  assert.ok(volumeId === null || typeof volumeId === 'string');
  const toolchain = await rustcVersionTriple();
  assert.ok(toolchain === null || /^stable-\S+-\S+$/.test(toolchain));
});

test('defaultBounds matches the doc-mandated shape (one field per bound, no drift)', () => {
  const bounds = defaultBounds();
  assert.equal(bounds.jobDeadlineMs, 0, 'the broker itself has no build timeout; right-release owns that');
  assert.equal(bounds.buildConcurrency, 3);
  assert.equal(bounds.workspaceDiscoveryTimeoutMs, 30_000);
  assert.ok(bounds.targetCacheCapBytes > 0);
  assert.equal(bounds.maintenanceConcurrency, undefined);
});

test('cargo build jobs are clamped at canonical workstation maximum', () => {
  // 8 logical / 4 performance cores with 8 GiB: memory binds first at 4, so
  // parallel links cannot push this host into swap.
  assert.equal(defaultCargoBuildJobs({ cpus: 8, totalMemoryBytes: 8 * 1024 ** 3 }), 4);
  assert.equal(defaultCargoBuildJobs({ cpus: 16, totalMemoryBytes: 64 * 1024 ** 3 }), 4);
  assert.equal(defaultCargoBuildJobs({ cpus: 2, totalMemoryBytes: 1 * 1024 ** 3 }), 1);
});

test('client-side sccache requires version 0.17 or newer', async () => {
  assert.equal(await supportsClientSideSccache('/sccache', async () => ({ stdout: 'sccache 0.16.0\n' })), false);
  assert.equal(await supportsClientSideSccache('/sccache', async () => ({ stdout: 'sccache 0.17.0\n' })), true);
});
