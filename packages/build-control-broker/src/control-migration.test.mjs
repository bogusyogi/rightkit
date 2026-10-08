import net from 'node:net';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, mkdir, readFile, readlink, rm, symlink, writeFile, lstat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { migrateControlPlane, parseControlMigrationArgs, relocateControlConfig } from './control-migration.mjs';
import { validateBrokerConfig } from './config.mjs';

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
  assert.equal(plan.config.nodePath, '/pinned/node');
  assert.equal(plan.config.broker.stateFile, join(f.controlRoot, 'state.db'));
  assert.equal(plan.config.broker.receiptsDir, join(f.oldRoot, 'receipts'));
  assert.ok(plan.plist.includes(join(f.controlRoot, 'runtime-current')));
  assert.ok(plan.plist.includes(join(f.controlRoot, 'config.json')));
  assert.ok(plan.copies.some((copy) => copy.from.endsWith('state.db-wal')));
  validateBrokerConfig(plan.config);
});

test('applying migration copies control only, rehomes runtime pointer & refuses overwrites', async (t) => {
  const f = await fixture(t);
  await symlink(join(f.oldRoot, 'runtime-abc', 'module.mjs'), join(f.oldRoot, 'runtime-abc', 'absolute-link'));
  const result = await migrateControlPlane({ ...f, apply: true });
  assert.equal(result.dryRun, false);
  assert.equal(await readlink(join(f.controlRoot, 'runtime-current')), 'runtime-abc');
  assert.equal(await readFile(join(f.controlRoot, 'runtime-current', 'absolute-link'), 'utf8'), 'runtime bytes');
  assert.equal(await readlink(join(f.controlRoot, 'runtime-abc', 'absolute-link')), 'module.mjs');
  assert.equal(await readFile(join(f.controlRoot, 'agent-policy', 'keep'), 'utf8'), 'agent-policy');
  assert.equal(await lstat(join(f.controlRoot, 'receipts')).catch((e) => e.code), 'ENOENT');
  await assert.rejects(migrateControlPlane({ ...f, apply: true }), /already exists/);
  const second = relocateControlConfig(result.config, f);
  assert.equal(second.broker.stateFile, result.config.broker.stateFile);
  assert.equal(second.broker.activationFencePath, result.config.broker.activationFencePath);
});

test('migration refuses runtime links escaping copied tree before mutation', async (t) => {
  const f = await fixture(t); await symlink('/Volumes/D/outside', join(f.oldRoot, 'runtime-abc', 'escape'));
  await assert.rejects(migrateControlPlane({ ...f, apply: true }), /link escapes/);
  assert.equal(await lstat(f.controlRoot).catch((e) => e.code), 'ENOENT');
});


test('migration copies legacy signing state without copying live socket', { skip: process.platform === 'win32' }, async (t) => {
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
  assert.ok(plan.copies.some((copy) => copy.from === directory));
  assert.equal(await readFile(join(f.controlRoot, 'signing-agent', 'token'), 'utf8'), 'fixture-state');
  assert.equal(await lstat(join(f.controlRoot, 'signing-agent', 'agent.sock')).catch((e) => e.code), 'ENOENT');
});
