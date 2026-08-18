import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { loadBrokerConfig } from './config.mjs';
import {
  buildConfig, defaultBounds, defaultCargoBuildJobs, discoverTools, parseArgs, renderPlist, resolveVolumeId, rustcVersionTriple, shimTable,
} from './bootstrap-macos-cli.mjs';

test('parseArgs accepts the documented flags & rejects anything else', () => {
  assert.deepEqual(parseArgs([]), { force: false });
  assert.deepEqual(parseArgs(['--force']), { force: true });
  assert.deepEqual(parseArgs(['--external-root', '/x', '--node', '/y']), { force: false, externalRoot: '/x', nodePath: '/y' });
  assert.throws(() => parseArgs(['--bogus']), /unknown bootstrap argument/);
});

test('shimTable only maps shims for tools actually discovered', () => {
  assert.deepEqual(shimTable([{ id: 'cargo' }, { id: 'clang' }]), { cargo: 'cargo', cc: 'clang', 'c++': 'clang', clang: 'clang' });
  assert.deepEqual(shimTable([]), {});
});

test('discoverTools finds real, hashed tools on this machine’s PATH', async () => {
  const tools = await discoverTools();
  // node itself proves the process ran & this machine has a real toolchain;
  // cargo is required by every other assertion this session has made about it.
  const cargo = tools.find((tool) => tool.id === 'cargo');
  assert.ok(cargo, 'cargo must be discoverable on PATH for every claim this session made about it to be testable');
  assert.match(cargo.sha256, /^[a-f0-9]{64}$/);
  assert.ok(cargo.path.startsWith('/'));
  for (const tool of tools) assert.equal(tool.root, tool.path.split('/').slice(0, -1).join('/'));
});

// ImmutableToolRegistry rejects symlinks outright (a security property that
// stays), so discovery must never record one: Homebrew ships sccache as
// bin/sccache -> ../Cellar/…, and registering that path makes the broker refuse
// to start with ToolChangedError(unavailable). bootstrap-windows-cli.mjs
// resolves identically; `where.exe` cannot run here, so only macOS is asserted.
test('discoverTools records a symlinked tool as its realpath, not the link', async (t) => {
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

test('buildConfig produces a config the broker’s own schema accepts', async (t) => {
  const scratch = mkdtempSync(join(tmpdir(), 'rk-bootstrap-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const config = await buildConfig({ externalRoot: scratch });
  const configPath = join(scratch, 'config.json');
  writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
  // This is the actual validator the live broker runs on startup — if this
  // passes, a bootstrapped config is not just shaped right, it is accepted.
  const loaded = await loadBrokerConfig(configPath);
  assert.equal(loaded.broker.externalRoot.path, scratch);
  assert.equal(loaded.sccache.mode, 'disabled');
  assert.equal(loaded.bounds.buildConcurrency, 2);
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
  assert.ok(bounds.targetCacheCapBytes > 0);
  assert.ok(bounds.retentionMs > 0);
});

test('cargo build jobs are clamped by memory before performance cores', () => {
  // 8 logical / 4 performance cores with 8 GiB: memory binds first at 4, so
  // parallel links cannot push this host into swap.
  assert.equal(defaultCargoBuildJobs({ cpus: 8, totalMemoryBytes: 8 * 1024 ** 3 }), 4);
  assert.equal(defaultCargoBuildJobs({ cpus: 16, totalMemoryBytes: 64 * 1024 ** 3 }), 8);
  assert.equal(defaultCargoBuildJobs({ cpus: 2, totalMemoryBytes: 1 * 1024 ** 3 }), 1);
});
