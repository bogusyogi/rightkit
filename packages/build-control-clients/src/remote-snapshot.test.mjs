import assert from 'node:assert/strict';
import { execFile as nodeExecFile } from 'node:child_process';
import { mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { buildSnapshot } from './remote-snapshot.mjs';
import { extractArchive, inspectArchive } from './remote-tar.mjs';

const execFile = promisify(nodeExecFile);

async function git(root, ...args) {
  return execFile('git', args, { cwd: root, env: { ...process.env, GIT_CONFIG_NOSYSTEM: '1' } });
}

async function fixtureRepository() {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-snapshot-fixture-'));
  await git(root, 'init', '--quiet');
  await git(root, 'config', 'user.name', 'Snapshot Test');
  await git(root, 'config', 'user.email', 'snapshot@example.invalid');
  await writeFile(join(root, '.gitignore'), 'ignored.txt\n');
  await writeFile(join(root, 'tracked.txt'), 'before\n');
  await writeFile(join(root, 'deleted.txt'), 'remove me\n');
  await git(root, 'add', '.');
  await git(root, 'commit', '--quiet', '-m', 'fixture');
  return root;
}

test('snapshot captures edits, deletions, and non-ignored untracked files without touching checkout state', async () => {
  const root = await fixtureRepository();
  const tempRoot = await mkdtemp(join(tmpdir(), 'rightkit-snapshot-temp-'));
  try {
    const beforeHead = (await git(root, 'rev-parse', 'HEAD')).stdout.trim();
    const beforeIndex = (await git(root, 'rev-parse', ':tracked.txt')).stdout.trim();
    await writeFile(join(root, 'tracked.txt'), 'after\n');
    await rm(join(root, 'deleted.txt'));
    await writeFile(join(root, 'new.txt'), 'new payload\n');
    await writeFile(join(root, 'ignored.txt'), 'must not enter snapshot\n');
    const beforeStatus = (await git(root, 'status', '--porcelain=v1')).stdout;

    const snapshot = await buildSnapshot({ workspace: root, tempRoot });
    const archive = await readFile(snapshot.archivePath);
    const inspected = inspectArchive(archive);
    const names = inspected.members.map(({ path }) => path);
    assert.deepEqual(names.sort(), ['.gitignore', 'new.txt', 'tracked.txt'].sort());
    const replica = await mkdtemp(join(tempRoot, 'replica-'));
    await extractArchive(archive, replica);
    assert.equal(await readFile(join(replica, 'tracked.txt'), 'utf8'), 'after\n');
    assert.equal(await readFile(join(replica, 'new.txt'), 'utf8'), 'new payload\n');
    await assert.rejects(stat(join(replica, 'deleted.txt')), { code: 'ENOENT' });
    await assert.rejects(stat(join(replica, 'ignored.txt')), { code: 'ENOENT' });
    assert.equal(snapshot.headHash, beforeHead);
    assert.equal((await git(root, 'rev-parse', 'HEAD')).stdout.trim(), beforeHead);
    assert.equal((await git(root, 'rev-parse', ':tracked.txt')).stdout.trim(), beforeIndex);
    assert.equal((await git(root, 'status', '--porcelain=v1')).stdout, beforeStatus);
  } finally {
    await rm(root, { recursive: true, force: true });
    await rm(tempRoot, { recursive: true, force: true });
  }
});

test('snapshot repository keys are stable for one root and distinct across roots', async () => {
  const first = await fixtureRepository();
  const second = await fixtureRepository();
  try {
    const a = await buildSnapshot({ workspace: first });
    const b = await buildSnapshot({ workspace: first });
    const c = await buildSnapshot({ workspace: second });
    assert.equal(a.key, b.key);
    assert.notEqual(a.key, c.key);
    await rm(a.scratchDir, { recursive: true, force: true });
    await rm(b.scratchDir, { recursive: true, force: true });
    await rm(c.scratchDir, { recursive: true, force: true });
  } finally {
    await rm(first, { recursive: true, force: true });
    await rm(second, { recursive: true, force: true });
  }
});
