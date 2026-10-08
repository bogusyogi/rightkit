import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm, readFile, utimes, lstat } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { moveToVolumeTrash } from '../../build-control-core/src/storage.mjs';
import { parseStorageArgs, storageMain } from './storage-cli.mjs';
test('storage CLI requires explicit prune mode & rejects conflicting or unknown flags', () => { assert.throws(() => parseStorageArgs(['prune']), /requires/); assert.throws(() => parseStorageArgs(['prune', '--apply', '--dry-run']), /exactly/); assert.throws(() => parseStorageArgs(['report', '--apply']), /require prune/); assert.throws(() => parseStorageArgs(['report', '--unknown', 'x']), /invalid/); assert.equal(parseStorageArgs(['prune', '--dry-run']).apply, false); });
test('storage report & dry-run work without broker and preserve data', async t => { const root = await mkdtemp(join(tmpdir(), 'rightkit-storage-cli-')); t.after(() => rm(root, { recursive: true, force: true })); await mkdir(join(root, 'runs/app/unknown'), { recursive: true }); await writeFile(join(root, 'runs/app/unknown/blob'), 'real'); let out = ''; const io = { stdout: { write: s => out += s } }; assert.equal(await storageMain(['report', '--managed-root', root], io), 0); assert.ok(JSON.parse(out).totals.managedBytes > 0); out = ''; await storageMain(['prune', '--dry-run', '--managed-root', root, '--broker', 'skip'], io); assert.equal(JSON.parse(out).managed[0].remove.length, 0); assert.equal(await readFile(join(root, 'runs/app/unknown/blob'), 'utf8'), 'real'); });
test('one-off Trash refuses generic repositories & managed roots before mutation', async t => { const root = await mkdtemp(join(tmpdir(), 'rightkit-storage-cli-')); t.after(() => rm(root, { recursive: true, force: true })); const io = { stdout: { write() {} } }; await assert.rejects(storageMain(['prune', '--dry-run', '--managed-root', root, '--trash', '/some/repo'], io), /explicitly named/); await assert.rejects(storageMain(['prune', '--dry-run', '--managed-root', root, '--trash', root], io), /managed storage/); });
test('broker prune is routed to typed live singleton & busy fails visibly', async t => { const root = await mkdtemp(join(tmpdir(), 'rightkit-storage-cli-')); t.after(() => rm(root, { recursive: true, force: true })); let command; const io = { stdout: { write() {} }, brokerGc: async input => { command = input; return { type: 'STORAGE_GC', result: { status: 'BUSY' } }; } }; assert.equal(await storageMain(['prune', '--dry-run', '--managed-root', root], io), 1); assert.equal(command.dryRun, true); });

test('CLI prune discovers legacy QA temp roots & accepts their explicit Trash selection', async t => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-storage-cli-')); t.after(() => rm(root, { recursive: true, force: true }));
  const legacy = join(root, 'tmp/rightkit-qa/cutright-old'); await mkdir(legacy, { recursive: true }); await writeFile(join(legacy, 'app.log'), 'launch log');
  const old = new Date(Date.now() - 3 * 86400_000); await utimes(legacy, old, old);
  let output = '';
  const io = { stdout: { write: s => output += s }, move: (path, options) => moveToVolumeTrash(path, { ...options, locateVolume: async () => root }) };
  const args = ['--managed-root', root, '--broker', 'skip'];
  assert.equal(await storageMain(['prune', '--dry-run', ...args], io), 0);
  assert.equal(JSON.parse(output).legacyQa.remove[0].path, legacy); await lstat(legacy);
  output = '';
  assert.equal(await storageMain(['prune', '--apply', ...args, '--trash', legacy], io), 0);
  const receipt = JSON.parse(output).legacyQa.moves[0];
  assert.equal(await readFile(join(receipt.destination, 'app.log'), 'utf8'), 'launch log');
  await assert.rejects(lstat(legacy), { code: 'ENOENT' });
});

test('CLI run Trash dry-run lists completed cache folders & apply retains live or young runs', async t => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-storage-cli-')); t.after(() => rm(root, { recursive: true, force: true }));
  const cache = join(root, 'app/.cache'), old = join(cache, 'old-run'), live = join(cache, 'live-run'), young = join(cache, 'young-run');
  for (const p of [old, live, young]) { await mkdir(p, { recursive: true }); await writeFile(join(p, 'log'), 'app log'); }
  await writeFile(join(live, 'owner.json'), JSON.stringify({ pid: process.pid }));
  const aged = new Date(Date.now() - 3 * 86400_000); for (const p of [old, live]) await utimes(p, aged, aged);
  let output = '';
  const io = { stdout: { write: s => output += s }, move: (path, options) => moveToVolumeTrash(path, { ...options, locateVolume: async () => root }) };
  const args = ['--managed-root', join(root, 'managed'), '--broker', 'skip', '--trash-kind', 'run', '--trash', old, '--trash', live, '--trash', young];
  assert.equal(await storageMain(['prune', '--dry-run', ...args], io), 0);
  const dry = JSON.parse(output).trash; assert.equal(dry[0].applied, false); assert.equal(dry[1].reason, 'live-owner-or-lock'); assert.equal(dry[2].reason, 'younger-than-minimum');
  await lstat(old); output = '';
  assert.equal(await storageMain(['prune', '--apply', ...args], io), 0);
  const receipt = JSON.parse(output).trash[0]; assert.equal(await readFile(join(receipt.destination, 'log'), 'utf8'), 'app log');
  await lstat(live); await lstat(young);
  await assert.rejects(lstat(old), { code: 'ENOENT' });
  assert.equal(parseStorageArgs(['prune', '--dry-run', '--trash', young, '--trash-kind', 'run', '--min-age-hours', '12']).minAgeMs, 12 * 3600_000);
  for (const age of ['-1', 'NaN', 'Infinity']) assert.throws(() => parseStorageArgs(['prune', '--dry-run', '--min-age-hours', age]), /nonnegative/);
  assert.throws(() => parseStorageArgs(['prune', '--dry-run', '--trash-kind', 'repo', '--trash', cache]), /trash-kind/);
  await assert.rejects(storageMain(['prune', '--dry-run', '--trash', root, '--trash-kind', 'run', '--managed-root', join(root, 'managed'), '--broker', 'skip'], io), /managed storage/);
});
