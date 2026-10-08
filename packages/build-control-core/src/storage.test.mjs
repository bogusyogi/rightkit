import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, rm, lstat, symlink, readdir, chmod, utimes, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { retentionPlan, pruneManaged, startStorageRun, withStorageLock, storageReport, moveToVolumeTrash, volumeRoot, noSymlinkPath, STORAGE_DEFAULTS, pruneLegacyQaTemp, trashRun } from './storage.mjs';
import { ownershipStorageFindings } from './storage-ownership.mjs';
import { deviceSupportPlan, connectedDeviceVersions, xcodeManagedArgs } from './xcode-storage.mjs';
const now = 2_000_000_000_000, day = 86400_000;
async function fixture(t) { const root = await mkdtemp(join(tmpdir(), 'rightkit-storage-')); t.after(() => rm(root, { recursive: true, force: true })); return root; }
async function row(root, app, id, createdAt, { kind = 'runs', ownerPid = 0, leaseUntil = 0 } = {}) { const path = join(root, kind, app, id); await mkdir(path, { recursive: true }); await writeFile(join(path, kind === 'runs' ? '.rightkit-run.json' : '.rightkit-artifact.json'), JSON.stringify({ schemaVersion: 1, app, [kind === 'runs' ? 'runId' : 'version']: id, createdAt, ownerPid, leaseUntil, ...(kind === 'artifacts' ? { completedAt: createdAt } : {}) })); await writeFile(join(path, 'payload'), 'x'.repeat(40)); return path; }
test('defaults are bounded', () => { assert.equal(STORAGE_DEFAULTS.runKeep, 5); assert.equal(STORAGE_DEFAULTS.runCapBytes, 20_000_000_000); assert.equal(STORAGE_DEFAULTS.ownershipThresholdBytes, 5_000_000_000); assert.equal(STORAGE_DEFAULTS.artifactKeep, 3); });
test('canonical macOS system aliases are trusted while arbitrary symlink ancestors fail closed', async t => { await assert.doesNotReject(noSymlinkPath('/var')); await assert.doesNotReject(noSymlinkPath('/tmp')); const root = await fixture(t); const target = join(root, 'target'); const link = join(root, 'link'); await mkdir(target); await symlink(target, link); await assert.rejects(noSymlinkPath(join(link, 'child')), /symlink/); });
test('nested symlink under canonical /private/var still fails closed', { skip: process.platform !== 'darwin' }, async t => {
  let root;
  for (const first of await readdir('/private/var/folders', { withFileTypes: true })) {
    if (!first.isDirectory()) continue;
    for (const second of await readdir(join('/private/var/folders', first.name), { withFileTypes: true })) {
      if (!second.isDirectory()) continue;
      try { root = await mkdtemp(join('/private/var/folders', first.name, second.name, 'T', 'rightkit-storage-var-')); break; } catch {}
    }
    if (root) break;
  }
  if (!root) throw new Error('no writable macOS user temp directory under /private/var');
  t.after(() => rm(root, { recursive: true, force: true }));
  const target = join(root, 'target'); const link = join(root, 'link'); await mkdir(target); await symlink(target, link);
  await assert.rejects(noSymlinkPath(join(link, 'child')), /symlink/);
});
test('run retention keeps newest five & all younger than 24h', async t => { const root = await fixture(t); for (let i = 0; i < 8; i++) await row(root, 'app', `r${i}`, now - (i + 2) * day); await row(root, 'app', 'young', now - 100); const plan = await retentionPlan({ managedRoot: root, app: 'app', now }); assert.equal(plan.remove.length, 4); assert.equal(plan.retain.length, 5); assert.ok(plan.retain.some(r => r.path.endsWith('young'))); });
test('run budget overrides newest count but protects live PID, lease & youth', async t => { const root = await fixture(t); await row(root, 'app', 'old', now - 4 * day); await row(root, 'app', 'live', now - 3 * day, { ownerPid: 101 }); await row(root, 'app', 'lease', now - 2 * day, { leaseUntil: now + day }); await row(root, 'app', 'young', now - 1); const plan = await retentionPlan({ managedRoot: root, app: 'app', now, capBytes: 1, alive: pid => pid === 101 }); assert.deepEqual(plan.remove.map(r => r.path.split('/').at(-1)), ['old']); assert.ok(plan.overCapBytes > 0); });
test('dry-run leaves roots & apply removes only eligible marked roots', async t => { const root = await fixture(t); const old = await row(root, 'app', 'old', now - 4 * day); const unknown = join(root, 'runs/app/unknown'); await mkdir(unknown); await writeFile(join(unknown, 'private'), 'data'); const plan = await pruneManaged({ managedRoot: root, app: 'app', now, keep: 0, apply: false }); assert.equal(plan.remove.length, 1); await lstat(old); await pruneManaged({ managedRoot: root, app: 'app', now, keep: 0, apply: true }); await assert.rejects(lstat(old), { code: 'ENOENT' }); assert.equal(await readFile(join(unknown, 'private'), 'utf8'), 'data'); });
test('artifacts retain latest three plus published; unknown publication preserves all', async t => { const root = await fixture(t); for (let i = 0; i < 5; i++) await row(root, 'app', `v${i}`, now - i * day, { kind: 'artifacts' }); assert.equal((await retentionPlan({ managedRoot: root, app: 'app', kind: 'artifacts', now })).remove.length, 0); await writeFile(join(root, 'artifacts/app/published.json'), JSON.stringify({ versions: ['v4'] })); const plan = await retentionPlan({ managedRoot: root, app: 'app', kind: 'artifacts', now }); assert.deepEqual(plan.remove.map(r => r.meta.version), ['v3']); });
test('path traversal, symlink ancestors & simultaneous retention fail closed', async t => { const root = await fixture(t); await assert.rejects(retentionPlan({ managedRoot: root, app: '../escape' })); const actual = join(root, 'actual'); await mkdir(actual); await symlink(actual, join(root, 'runs')); await assert.rejects(pruneManaged({ managedRoot: root, app: 'app', apply: true }), /symlink/); await withStorageLock(actual, () => assert.rejects(withStorageLock(actual, () => {}), /busy/)); });
test('managed run exports short socket-safe temp & retention owns its companion', async t => {
  const root = join(await fixture(t), 'long-managed-root-'.repeat(8));
  const run = await startStorageRun({ managedRoot: root, app: 'scraperight', runId: 'long-run-id-'.repeat(12) });
  const marker = join(run.root, '.rightkit-run.json');
  const active = JSON.parse(await readFile(marker));
  t.after(() => active.appRoot && rm(active.appRoot, { recursive: true, force: true }));
  assert.equal(run.env.RIGHTKIT_RUN_ROOT, run.root);
  assert.equal(run.env.TMP, run.env.TMPDIR); assert.equal(run.env.TEMP, run.env.TMPDIR);
  const socket = join(run.env.TMPDIR, `tsx-${process.getuid?.() ?? 501}`, '1234567890.pipe');
  if (process.platform !== 'win32') assert.ok(Buffer.byteLength(socket) < 104, socket);
  const binding = JSON.parse(await readFile(join(active.appRoot, '.rightkit-app.json')));
  assert.equal(binding.runRoot, await realpath(run.root));
  assert.equal(binding.runId, 'long-run-id-'.repeat(12));
  assert.equal((await pruneManaged({ managedRoot: root, app: 'scraperight', keep: 0, minAgeMs: 0, apply: true })).applied.length, 0);
  await lstat(run.env.TMPDIR);
  const args = await xcodeManagedArgs({ managedRoot: root, runRoot: run.root });
  assert.ok(args[1].startsWith(run.root));
  await run.finish();
  assert.equal(JSON.parse(await readFile(marker)).ownerPid, 0);
  await assert.rejects(xcodeManagedArgs({ managedRoot: root, runRoot: run.root }), /live/);
  await pruneManaged({ managedRoot: root, app: 'scraperight', keep: 0, minAgeMs: 0, apply: true });
  await assert.rejects(lstat(run.env.TMPDIR), { code: 'ENOENT' });
});
test('device support retains newest two, connected & recently used versions', async t => { const root = await fixture(t); for (const v of ['18.0', '19.0', '20.0', '21.0', '22.0']) await mkdir(join(root, `${v} (build)`)); const plan = await deviceSupportPlan({ directory: root, connectedVersions: ['18.0'], recent: { '19.0': now - day }, now }); assert.deepEqual(plan.remove.map(r => r.version), ['20.0']); assert.equal(plan.retain.length, 4); });
test('device discovery validates snapshots & current/deprecated properties', () => { assert.throws(() => connectedDeviceVersions({}), /incomplete/); assert.deepEqual(connectedDeviceVersions({ result: { devices: [{ connectionProperties: { tunnelState: 'connected' }, deviceProperties: { osVersionNumber: '18.0' } }, { properties: { connection: { tunnelState: 'connected' }, device: { osVersionNumber: '19.1' } } }] } }), ['18.0', '19.1']); assert.throws(() => connectedDeviceVersions({ result: { devices: [{ connectionProperties: { tunnelState: 'connected' } }] } }), /unavailable/); });
test('report aggregates managed/unmanaged categories & per-app offenders', async t => { const root = await fixture(t); await row(root, 'app', 'run', now); const app = join(root, 'repo'); await mkdir(join(app, '.cache'), { recursive: true }); await writeFile(join(app, '.cache/blob'), 'hello'); const report = await storageReport({ managedRoot: root, apps: [{ app: 'app', path: app }] }); assert.equal(report.totals.unmanagedBytes, 5); assert.ok(report.totals.managedBytes > 40); assert.equal(report.totals.byCategory['unmanaged:app-cache'], 5); assert.equal(report.errors.length, 0); });
test('ownership flags ignored local caches; excludes only ignored managed links', async t => { const root = await fixture(t); const repo = join(root, 'repo'), managed = join(root, 'managed'); await mkdir(join(repo, '.cache'), { recursive: true }); await mkdir(managed); await writeFile(join(repo, '.cache/blob'), '123456'); await symlink(managed, join(repo, 'managed')); const findings = await ownershipStorageFindings({ appRoot: repo, managedRoot: managed, thresholdBytes: 5, ignored: async () => true }); assert.equal(findings.length, 1); assert.ok(findings[0].path.endsWith('.cache')); });
test('Trash dry run is same-volume & never mutates source', async t => { const root = await fixture(t); const path = join(root, 'old.dmg'); await writeFile(path, 'installer'); const receipt = await moveToVolumeTrash(path, { volume: await volumeRoot(root), apply: false }); assert.equal(receipt.applied, false); assert.equal(await readFile(path, 'utf8'), 'installer'); await assert.rejects(moveToVolumeTrash(path, { volume: '/nonexistent', apply: true })); });

test('Trash apply moves one-off items in temp volume without deleting contents', async t => { const root = await fixture(t); const source = join(root, 'archive-old'); await mkdir(source); await writeFile(join(source, 'private'), 'preserved'); const receipt = await moveToVolumeTrash(source, { apply: true, locateVolume: async () => root, volume: root }); await assert.rejects(lstat(source), { code: 'ENOENT' }); assert.equal(await readFile(join(receipt.destination, 'private'), 'utf8'), 'preserved'); });
test('Trash creates shared sticky volume root & private current-user directory', async t => { const root = await fixture(t); const source = join(root, 'archive-old'); await writeFile(source, 'preserved'); const receipt = await moveToVolumeTrash(source, { apply: true, locateVolume: async () => root, volume: root }); const rootStat = await lstat(join(root, '.Trashes')); const userStat = await lstat(dirname(receipt.destination)); if (process.platform !== 'win32') { assert.equal(rootStat.mode & 0o1777, 0o1777); assert.equal(userStat.mode & 0o777, 0o700); } if (process.getuid) assert.equal(userStat.uid, process.getuid()); });
test('Trash accepts existing Finder-style 01333 root without changing it', async t => { const root = await fixture(t); const source = join(root, 'archive-old'); await writeFile(source, 'preserved'); const trashRoot = join(root, '.Trashes'); await mkdir(trashRoot); if (process.platform !== 'win32') await chmod(trashRoot, 0o1333); const receipt = await moveToVolumeTrash(source, { apply: true, locateVolume: async () => root, volume: root }); const rootStat = await lstat(trashRoot); if (process.platform !== 'win32') { assert.equal(rootStat.mode & 0o7777, 0o1333); await chmod(trashRoot, 0o1777); } assert.equal(await readFile(join(receipt.destination), 'utf8'), 'preserved'); });
test('Trash accepts unreadable sticky 01311 root & refuses non-sticky roots', { skip: process.platform === 'win32' }, async t => {
  const root = await fixture(t), trash = join(root, '.Trashes'), source = join(root, 'archive-old');
  await mkdir(trash); await chmod(trash, 0o1311);
  t.after(() => chmod(trash, 0o1777).catch(e => { if (e.code !== 'ENOENT') throw e; }));
  await writeFile(source, 'preserved');
  const receipt = await moveToVolumeTrash(source, { apply: true, locateVolume: async () => root });
  assert.equal((await lstat(trash)).mode & 0o7777, 0o1311);
  assert.equal((await lstat(dirname(receipt.destination))).mode & 0o777, 0o700);
  await chmod(trash, 0o777); await writeFile(source, 'keep');
  await assert.rejects(moveToVolumeTrash(source, { apply: true, locateVolume: async () => root }), /sticky/);
  assert.equal(await readFile(source, 'utf8'), 'keep');
  await chmod(trash, 0o1777);
});
test('Trash refuses symlinked fixture Trash roots before mutation', async t => { const root = await fixture(t); const source = join(root, 'archive-old'); const outside = join(root, 'outside'); await mkdir(source); await writeFile(join(source, 'private'), 'preserved'); await mkdir(outside); await symlink(outside, join(root, '.Trashes')); await assert.rejects(moveToVolumeTrash(source, { apply: true, locateVolume: async () => root, volume: root }), /symlink/); await lstat(source); });
test('Trash dry run refuses symlinked fixture Trash roots', async t => { const root = await fixture(t); const source = join(root, 'archive-old'); const outside = join(root, 'outside'); await writeFile(source, 'preserved'); await mkdir(outside); await symlink(outside, join(root, '.Trashes')); await assert.rejects(moveToVolumeTrash(source, { apply: false, locateVolume: async () => root, volume: root }), /symlink/); await lstat(source); });
test('Trash refuses symlinked user directory before mutation', async t => { const root = await fixture(t); const source = join(root, 'archive-old'); const outside = join(root, 'outside'); const uid = process.getuid?.(); if (uid === undefined) return; await writeFile(source, 'preserved'); await mkdir(outside); await mkdir(join(root, '.Trashes')); await chmod(join(root, '.Trashes'), 0o1777); await symlink(outside, join(root, '.Trashes', String(uid))); await assert.rejects(moveToVolumeTrash(source, { apply: true, locateVolume: async () => root, volume: root }), /symlink/); await lstat(source); });
test('new managed run refuses protected/unknown over-budget storage visibly', async t => { const root = await fixture(t); await row(root, 'app', 'young', now - 1); await assert.rejects(startStorageRun({ managedRoot: root, app: 'app', now, retention: { capBytes: 1 } }), /budget blocked/); });
test('incomplete artifact builds do not consume newest three completed slots', async t => { const root = await fixture(t); for (let i = 0; i < 3; i++) await row(root, 'app', `v${i}`, now - (i + 1) * day, { kind: 'artifacts' }); for (const id of ['pending1', 'pending2']) { const path = await row(root, 'app', id, now, { kind: 'artifacts' }); const file = join(path, '.rightkit-artifact.json'); const meta = JSON.parse(await readFile(file)); delete meta.completedAt; await writeFile(file, JSON.stringify(meta)); } await writeFile(join(root, 'artifacts/app/published.json'), JSON.stringify({ versions: [] })); const plan = await retentionPlan({ managedRoot: root, app: 'app', kind: 'artifacts', now }); assert.equal(plan.remove.length, 0); });
test('ordinary dependency symlinks below size threshold do not fail ownership gate', async t => { const root = await fixture(t); const repo = join(root, 'repo'), dependency = join(root, 'dependency'); await mkdir(repo); await mkdir(dependency); await writeFile(join(dependency, 'tiny'), 'x'); await symlink(dependency, join(repo, 'node_modules')); const findings = await ownershipStorageFindings({ appRoot: repo, managedRoot: join(root, 'managed'), thresholdBytes: 5 }); assert.equal(findings.length, 0); });
test('Xcode preparation records recent device use in managed temp storage', async t => { const root = await fixture(t); const { recordDeviceUse } = await import('./xcode-storage.mjs'); await recordDeviceUse({ managedRoot: root, versions: ['18.0'], now }); assert.equal(JSON.parse(await readFile(join(root, 'xcode/devices.json')))['18.0'], now); });
test('DeviceSupport apply moves only unused versions into fixture Trash', async t => { const root = await fixture(t); const directory = join(root, 'support'), managed = join(root, 'managed'); await mkdir(directory); for (const version of ['18.0', '19.0', '20.0', '21.0']) { await mkdir(join(directory, version)); await writeFile(join(directory, version, 'symbols'), version); } const { pruneDeviceSupport } = await import('./xcode-storage.mjs'); const plan = await pruneDeviceSupport({ managedRoot: managed, directory, connectedVersions: ['18.0'], apply: true, now, move: (path, options) => moveToVolumeTrash(path, { ...options, locateVolume: async () => root }) }); assert.equal(plan.moves.length, 1); assert.equal(await readFile(join(plan.moves[0].destination, 'symbols'), 'utf8'), '19.0'); await lstat(join(directory, '18.0')); await assert.rejects(lstat(join(directory, '19.0')), { code: 'ENOENT' }); });
test('malformed DeviceSupport usage history fails closed', async t => { const root = await fixture(t); const { deviceSupportPlan } = await import('./xcode-storage.mjs'); await assert.rejects(deviceSupportPlan({ directory: root, connectedVersions: [], recent: [] }), /invalid device/); });

async function legacyRoot(root, name) {
  const path = join(root, 'tmp', 'rightkit-qa', name);
  await mkdir(path, { recursive: true });
  await writeFile(join(path, 'app.log'), 'preserved log');
  return path;
}
async function ageLegacy(path) { const old = new Date(Date.now() - 3 * day); await utimes(path, old, old); }
test('legacy QA dry-run lists old unmarked roots; apply moves contents to volume Trash', async t => {
  const root = await fixture(t);
  const old = await legacyRoot(root, 'cutright-old'); await ageLegacy(old);
  const young = await legacyRoot(root, 'cutright-young');
  const live = await legacyRoot(root, 'cutright-live');
  await mkdir(join(live, 'native.lock')); await writeFile(join(live, 'native.lock/owner.json'), JSON.stringify({ pid: process.pid })); await ageLegacy(live);
  const dead = await legacyRoot(root, 'cutright-dead');
  await writeFile(join(dead, 'owner.json'), JSON.stringify({ pid: 999999 })); await ageLegacy(dead);
  const linked = join(root, 'tmp/rightkit-qa/cutright-link'); await symlink(old, linked);
  const move = (path, options) => moveToVolumeTrash(path, { ...options, locateVolume: async () => root });
  const options = { managedRoot: root, move, alive: pid => pid === process.pid };
  const dry = await pruneLegacyQaTemp(options);
  assert.deepEqual(dry.remove.map(r => r.path).sort(), [old, dead].sort());
  assert.ok(dry.skipped.some(r => r.path === live && r.reason === 'live-owner-or-lock'));
  assert.ok(dry.skipped.some(r => r.path === young && r.reason === 'younger-than-minimum'));
  assert.ok(dry.skipped.some(r => r.path === linked));
  await lstat(old);
  const applied = await pruneLegacyQaTemp({ ...options, apply: true });
  assert.equal(applied.moves.length, 2);
  for (const receipt of applied.moves) assert.equal(await readFile(join(receipt.destination, 'app.log'), 'utf8'), 'preserved log');
  await assert.rejects(lstat(old), { code: 'ENOENT' });
  await lstat(live); await lstat(young);
});
test('legacy QA unknown locks & nested symlinks remain protected', async t => {
  const root = await fixture(t);
  const locked = await legacyRoot(root, 'app-locked'); await mkdir(join(locked, 'native.lock')); await ageLegacy(locked);
  const linked = await legacyRoot(root, 'app-linked'); await symlink(root, join(linked, 'outside')); await ageLegacy(linked);
  const plan = await pruneLegacyQaTemp({ managedRoot: root });
  assert.equal(plan.remove.length, 0); assert.equal(plan.skipped.length, 2);
});
test('core retention cleans registered QA companions & preserves live owners', async t => {
  const root = await fixture(t), runRoot = await row(root, 'app', 'orphan', now - 4 * day);
  let temp = await realpath(tmpdir());
  if (process.platform === 'darwin' && !temp.startsWith('/private/')) temp = '/private/tmp';
  await mkdir(join(temp, 'rightkit-qa'), { recursive: true });
  const home = await mkdtemp(join(temp, 'rightkit-qa/app-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  await writeFile(join(home, '.rightkit-app.json'), JSON.stringify({ runRoot: await realpath(runRoot), app: 'app', runId: 'orphan' }));
  const marker = join(runRoot, '.rightkit-run.json'), meta = JSON.parse(await readFile(marker));
  await writeFile(marker, JSON.stringify({ ...meta, ownerPid: process.pid, appRoot: home }));
  assert.equal((await pruneManaged({ managedRoot: root, app: 'app', now, keep: 0, apply: true })).applied.length, 0);
  await lstat(home);
  await writeFile(marker, JSON.stringify({ ...meta, appRoot: home }));
  await pruneManaged({ managedRoot: root, app: 'app', now, keep: 0, apply: true });
  await assert.rejects(lstat(home), { code: 'ENOENT' });
});

test('run Trash lists old app cache folders & preserves youth, live PIDs, leases & unknown locks', async t => {
  const root = await fixture(t), cache = join(root, 'app/.cache');
  const make = async name => { const p = join(cache, name); await mkdir(p, { recursive: true }); await writeFile(join(p, 'log'), 'preserved'); return p; };
  const old = await make('run-old'), young = await make('run-young'), live = await make('run-live'), locked = await make('run-locked'), lease = await make('run-lease');
  await writeFile(join(live, 'app.pid'), String(process.pid));
  await mkdir(join(locked, 'app.lock'));
  await writeFile(join(lease, 'owner.json'), JSON.stringify({ pid: 0, leaseUntil: Date.now() + day }));
  for (const p of [old, live, locked, lease]) await ageLegacy(p);
  const move = (path, options) => moveToVolumeTrash(path, { ...options, locateVolume: async () => root });
  const options = { managedRoot: join(root, 'managed'), move };
  const dry = await trashRun({ ...options, path: old });
  assert.equal(dry.kind, 'run'); assert.equal(dry.applied, false); await lstat(old);
  for (const [path, reason] of [[young, 'younger-than-minimum'], [live, 'live-owner-or-lock'], [locked, 'unknown-lock-owner'], [lease, 'live-owner-or-lock']]) {
    assert.equal((await trashRun({ ...options, path, apply: true })).reason, reason);
    await lstat(path);
  }
  const receipt = await trashRun({ ...options, path: old, apply: true });
  assert.equal(await readFile(join(receipt.destination, 'log'), 'utf8'), 'preserved');
  await assert.rejects(lstat(old), { code: 'ENOENT' });
  await assert.rejects(trashRun({ ...options, path: root }), /requires/);
  await assert.rejects(trashRun({ ...options, path: young, minAgeMs: -1 }), /age/);
});

test('run Trash rejects incomplete markers, symlinks & changed owners; supports age override', async t => {
  const root = await fixture(t), run = await row(root, 'app', 'completed', now - 4 * day);
  const options = { managedRoot: root, path: run, now, alive: () => false, move: async () => ({ applied: false }) };
  assert.equal((await trashRun(options)).reason, 'incomplete-or-invalid-run');
  const marker = join(run, '.rightkit-run.json'), meta = JSON.parse(await readFile(marker));
  await writeFile(marker, JSON.stringify({ ...meta, completedAt: now - day / 2 }));
  await ageLegacy(run);
  assert.equal((await trashRun(options)).reason, 'younger-than-minimum');
  assert.equal((await trashRun({ ...options, minAgeMs: 0 })).kind, 'run');
  await writeFile(marker, JSON.stringify({ ...meta, ownerPid: 123, completedAt: now - 3 * day }));
  await ageLegacy(run);
  let checks = 0;
  await assert.rejects(trashRun({ ...options, apply: true, alive: pid => pid === 123 && ++checks > 2 }), /owner changed/);
  const link = join(root, 'linked'); await symlink(run, link);
  await assert.rejects(trashRun({ ...options, path: link }), /symlink/);
});

test('legacy QA companion Trash follows binding to live managed owner', async t => {
  const root = await fixture(t), run = await startStorageRun({ managedRoot: root, app: 'app' });
  const meta = JSON.parse(await readFile(join(run.root, '.rightkit-run.json')));
  t.after(() => rm(meta.appRoot, { recursive: true, force: true }));
  await ageLegacy(meta.appRoot);
  const plan = await trashRun({ path: meta.appRoot, managedRoot: root });
  assert.equal(plan.reason, 'live-owner-or-lock');
  await run.finish();
  const dry = await trashRun({ path: meta.appRoot, managedRoot: root });
  assert.equal(dry.skipped, undefined); assert.equal(dry.kind, 'run');
});
