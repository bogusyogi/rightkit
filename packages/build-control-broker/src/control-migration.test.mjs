import net from 'node:net';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, mkdir, readFile, readlink, rm, symlink, writeFile, lstat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, posix } from 'node:path';
import { ensureLegacyControlLinks, migrateControlPlane, parseControlMigrationArgs, relocateControlConfig } from './control-migration.mjs';
import { restoreRuntimeSelection, snapshotRuntimeSelection } from './runtime.mjs';
import { validateBrokerConfig } from './config.mjs';

const macOSLinkReplacement = { skip: process.platform !== 'darwin' && 'Mac control migration atomically replaces POSIX symlinks; Windows roots do not migrate' };

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'rk-control-migrate-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const oldRoot = join(root, 'data'); const controlRoot = join(root, 'internal');
  await mkdir(join(oldRoot, 'runtime-abc'), { recursive: true });
  await writeFile(join(oldRoot, 'runtime-abc', 'module.mjs'), 'runtime bytes');
  await symlink('runtime-abc', join(oldRoot, 'runtime-current'));
  const config = { schemaVersion: 2, nodePath: '/pinned/node', broker: { externalRoot: { path: oldRoot, volumeId: 'keep-volume', mounted: true, writable: true, internal: false }, capacityBytes: 1024, stateFile: join(oldRoot, 'state.db'), activationFencePath: join(oldRoot, 'state', 'operator-fence.json'), receiptsDir: join(oldRoot, 'receipts') }, unixSocket: join(oldRoot, 'broker.sock'), defaults: { cargoBuildJobs: 7 }, ownership: { mode: 'enforce' }, tools: [], shims: {} };
  await writeFile(join(oldRoot, 'config.json'), JSON.stringify(config));
  for (const name of ['state.db', 'state.db-wal']) await writeFile(join(oldRoot, name), name);
  for (const name of ['agent-policy', 'ownership-install', 'signing-agent']) { await mkdir(join(oldRoot, name)); await writeFile(join(oldRoot, name, 'keep'), name); }
  return { oldRoot, controlRoot, dataRoot: oldRoot, home: join(root, 'home'), config };
}

test('migration defaults to dry-run, preserves live bytes & prints internal plist', async (t) => {
  const f = await fixture(t); const before = await readFile(join(f.oldRoot, 'config.json'), 'utf8');
  assert.deepEqual(parseControlMigrationArgs([]), { apply: false });
  const command = spawnSync(process.execPath, [fileURLToPath(new URL('./control-migration.mjs', import.meta.url)), '--old-root', f.oldRoot, '--control-root', f.controlRoot], { encoding: 'utf8' });
  assert.equal(command.status, 0, command.stderr);
  assert.match(command.stdout, /"dryRun": true/);
  assert.match(command.stdout, /<plist version="1.0">/);
  const plan = await migrateControlPlane(f);
  assert.equal(plan.dryRun, true);
  assert.equal(await lstat(f.controlRoot).catch((e) => e.code), 'ENOENT');
  assert.equal(await readFile(join(f.oldRoot, 'config.json'), 'utf8'), before);
  assert.equal(plan.config.broker.externalRoot.path, f.oldRoot);
  assert.equal(plan.config.defaults.cargoBuildJobs, 7);
  assert.equal(plan.config.ownership.mode, 'enforce');
  assert.equal(Object.hasOwn(plan.config, 'nodePath'), false);
  assert.match(plan.plist, /<string>\/pinned\/node<\/string>/);
  assert.equal(plan.config.broker.stateFile, join(f.controlRoot, 'state.db'));
  assert.equal(plan.config.broker.receiptsDir, join(f.oldRoot, 'receipts'));
  assert.ok(plan.plist.includes(join(f.controlRoot, 'runtime-current')));
  assert.ok(plan.plist.includes(posix.join(f.controlRoot, 'config.json')));
  assert.ok(plan.copies.some((copy) => copy.from.endsWith('state.db-wal')));
  validateBrokerConfig(plan.config);
});

test('applying migration copies control only, rehomes runtime pointer & refuses overwrites', macOSLinkReplacement, async (t) => {
  const f = await fixture(t);
  await symlink(join(f.oldRoot, 'runtime-abc', 'module.mjs'), join(f.oldRoot, 'runtime-abc', 'absolute-link'));
  const result = await migrateControlPlane({ ...f, apply: true });
  assert.equal(result.dryRun, false);
  assert.equal(await readlink(join(f.controlRoot, 'runtime-current')), 'runtime-abc');
  assert.equal(await readlink(join(f.oldRoot, 'runtime-current')), join(f.controlRoot, 'runtime-current'));
  assert.equal(await readlink(join(f.oldRoot, 'broker.sock')), join(f.controlRoot, 'broker.sock'));
  assert.equal(await readFile(join(f.controlRoot, 'runtime-current', 'absolute-link'), 'utf8'), 'runtime bytes');
  assert.equal(await readlink(join(f.controlRoot, 'runtime-abc', 'absolute-link')), 'module.mjs');
  assert.equal(await readFile(join(f.controlRoot, 'agent-policy', 'keep'), 'utf8'), 'agent-policy');
  assert.equal(await lstat(join(f.controlRoot, 'receipts')).catch((e) => e.code), 'ENOENT');
  await assert.rejects(migrateControlPlane({ ...f, apply: true }), /already exists/);
  const second = relocateControlConfig(result.config, f);
  assert.equal(second.broker.stateFile, result.config.broker.stateFile);
  assert.equal(second.broker.activationFencePath, result.config.broker.activationFencePath);
});

test('migration saves original config bytes with copied prior runtime for rollback', macOSLinkReplacement, async (t) => {
  const f = await fixture(t);
  delete f.config.nodePath;
  const bytes = Buffer.from(`\uFEFF${JSON.stringify(f.config, null, 4)}\n\n`);
  await writeFile(join(f.oldRoot, 'config.json'), bytes);
  await migrateControlPlane({ ...f, apply: true, nodePath: '/plist-only/node' });
  const configPath = join(f.controlRoot, 'config.json');
  const snapshot = await snapshotRuntimeSelection({ root: f.controlRoot, configPath });
  assert.equal(snapshot.prior, 'runtime-abc');
  assert.deepEqual(Buffer.from(snapshot.configBytes, 'base64'), bytes);
  await restoreRuntimeSelection(snapshot);
  assert.deepEqual(await readFile(configPath), bytes);
  validateBrokerConfig(JSON.parse(bytes.toString().replace(/^\uFEFF/, '')));
});

test('legacy compatibility links replace only symlinks & are idempotent', macOSLinkReplacement, async (t) => {
  const f = await fixture(t);
  await ensureLegacyControlLinks(f);
  await ensureLegacyControlLinks(f);
  assert.equal(await readlink(join(f.oldRoot, 'runtime-current')), join(f.controlRoot, 'runtime-current'));
  assert.equal(await readlink(join(f.oldRoot, 'broker.sock')), join(f.controlRoot, 'broker.sock'));
});

for (const name of ['broker.sock', 'runtime-current']) test(`compatibility refuses non-symlink ${name} before changing either path`, async (t) => {
  const f = await fixture(t);
  if (name === 'runtime-current') await rm(join(f.oldRoot, name));
  await writeFile(join(f.oldRoot, name), 'foreign bytes');
  await assert.rejects(ensureLegacyControlLinks(f), /must be a symlink/);
  assert.equal(await readFile(join(f.oldRoot, name), 'utf8'), 'foreign bytes');
  if (name === 'broker.sock') {
    await assert.rejects(migrateControlPlane({ ...f, apply: true }), /must be a symlink/);
    assert.equal(await lstat(f.controlRoot).catch((e) => e.code), 'ENOENT');
    assert.equal(await readlink(join(f.oldRoot, 'runtime-current')), 'runtime-abc');
  } else assert.equal(await lstat(join(f.oldRoot, 'broker.sock')).catch((e) => e.code), 'ENOENT');
});

test('migration refuses runtime links escaping copied tree before mutation', async (t) => {
  const f = await fixture(t); await symlink('/Volumes/D/outside', join(f.oldRoot, 'runtime-abc', 'escape'));
  await assert.rejects(migrateControlPlane({ ...f, apply: true }), /link escapes/);
  assert.equal(await lstat(f.controlRoot).catch((e) => e.code), 'ENOENT');
});


test('migration leaves internal signing state in place and never copies a live socket', { skip: process.platform === 'win32' }, async (t) => {
  const f = await fixture(t); await rm(join(f.oldRoot, 'signing-agent'), { recursive: true });
  const directory = join(f.home, 'Library', 'Application Support', 'Orthic Labs', 'shared', 'signing-agent');
  await mkdir(directory, { recursive: true }); await writeFile(join(directory, 'token'), 'fixture-state');
  const socket = join(directory, 'agent.sock');
  // Keep Unix socket below sockaddr_un length even on hosts with external TMPDIR.
  const marker = net.createServer();
  const shortSocket = join(f.oldRoot, 'signing-agent.sock');
  await new Promise((resolve) => marker.listen(shortSocket, resolve));
  await (await import('node:fs/promises')).rename(shortSocket, socket);
  t.after(() => new Promise((resolve) => marker.close(resolve)));
  const plan = await migrateControlPlane({ ...f, apply: true });
  assert.ok(!plan.copies.some((copy) => copy.from === directory || copy.to.endsWith('signing-agent')));
  assert.equal(await readFile(join(directory, 'token'), 'utf8'), 'fixture-state');
  assert.equal(await lstat(join(f.controlRoot, 'signing-agent', 'agent.sock')).catch((e) => e.code), 'ENOENT');
});
