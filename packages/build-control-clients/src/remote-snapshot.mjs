// Mac coordinator snapshot builder for dual-host Rust routing
// (docs/RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md §3). Builds a content-addressed
// snapshot of the exact worktree state — tracked changes, deletions, and
// non-ignored untracked files — without touching the current index or
// worktree: every git invocation runs with GIT_INDEX_FILE pointed at a private
// temporary index, so repository refs, the real index, and the worktree are
// never modified. Only unreferenced content-addressed objects are added to the
// object store.

import { createHash } from 'node:crypto';
import { execFile as childExecFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, basename } from 'node:path';

const execFile = promisify(childExecFile);

export class SnapshotError extends Error {
  constructor(message, code) {
    super(message);
    this.name = 'SnapshotError';
    this.code = code;
  }
}

/** Deterministic repository key used on the Windows side for slot naming:
 * a slug of the checkout directory plus 8 hex of the canonical root hash. */
export function repositoryKey(workspaceRoot, { sha256 = (value) => createHash('sha256').update(value).digest('hex') } = {}) {
  const root = resolve(workspaceRoot);
  const slug = basename(root).toLowerCase().replace(/[^a-z0-9._-]/g, '').slice(0, 40) || 'workspace';
  return `${slug}-${sha256(root).slice(0, 8)}`;
}

async function git(args, cwd, indexFile) {
  try {
    return (await execFile('git', args, { cwd, maxBuffer: 256 * 1024 * 1024, env: indexFile ? { ...process.env, GIT_INDEX_FILE: indexFile } : process.env })).stdout;
  } catch (error) {
    throw new SnapshotError(`git ${args[0]} failed: ${(error.stderr ?? error.message ?? '').toString().trim().split('\n')[0]}`, 'SNAPSHOT_GIT_FAILED');
  }
}

/**
 * Build one exact snapshot of the current worktree.
 *
 * Steps (contract §3): temporary index from HEAD → add tracked changes,
 * deletions & non-ignored untracked files → write synthetic tree + commit
 * objects → archive synthetic commit → SHA-256 the archive bytes.
 *
 * Returns { archivePath, sha256, byteLength, treeHash, commitHash, headHash,
 * fileCount, key }. The caller owns cleanup of archivePath and the enclosing
 * temp directory.
 */
export async function buildSnapshot({ workspace = process.cwd(), tempRoot = tmpdir() } = {}) {
  const root = resolve(workspace);
  const scratch = await mkdtemp(join(tempRoot, 'rightkit-snapshot-'));
  const indexFile = join(scratch, 'index');
  const archivePath = join(scratch, 'snapshot.tar');
  try {
    const headHash = (await git(['rev-parse', '--verify', 'HEAD'], root)).trim();
    if (!/^[a-f0-9]{40,64}$/.test(headHash)) throw new SnapshotError('HEAD is not a commit', 'SNAPSHOT_NO_COMMIT');
    await git(['read-tree', 'HEAD'], root, indexFile);
    // Tracked modifications & deletions plus non-ignored untracked files; the
    // temp index keeps this off the real index entirely.
    await git(['add', '-A', '--'], root, indexFile);
    const staged = await git(['ls-files', '--stage'], root, indexFile);
    const entries = staged.split('\n').filter(Boolean);
    if (entries.some((entry) => entry.startsWith('160000 '))) {
      throw new SnapshotError('submodules are not supported by remote snapshots yet', 'SNAPSHOT_SUBMODULE');
    }
    const treeHash = (await git(['write-tree'], root, indexFile)).trim();
    const identity = {
      GIT_AUTHOR_NAME: 'RightKit Snapshot Coordinator',
      GIT_AUTHOR_EMAIL: 'rightkit@invalid',
      GIT_COMMITTER_NAME: 'RightKit Snapshot Coordinator',
      GIT_COMMITTER_EMAIL: 'rightkit@invalid',
    };
    const commitHash = (await execFile('git', ['commit-tree', treeHash, '-p', headHash, '-m', `rightkit remote snapshot ${new Date().toISOString()}`], {
      cwd: root, env: { ...process.env, ...identity },
    })).stdout.trim();
    await git(['archive', '--format=tar', '-o', archivePath, commitHash], root);
    const archive = await readFile(archivePath);
    return Object.freeze({
      archivePath,
      scratchDir: scratch,
      sha256: createHash('sha256').update(archive).digest('hex'),
      byteLength: archive.byteLength,
      treeHash,
      commitHash,
      headHash,
      fileCount: entries.length,
      key: repositoryKey(root),
    });
  } catch (error) {
    await rm(scratch, { recursive: true, force: true }).catch(() => {});
    throw error;
  }
}
