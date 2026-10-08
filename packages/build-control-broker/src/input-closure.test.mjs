import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  INPUTS_MUTATED, INPUTS_UNVERIFIED, INPUTS_VERIFIED, InputClosureGuard, assertPromotableReceipt, diffInputSnapshots,
  digestInputClosure, gitListedClosure, normalizeClosurePath, recordInputClosure,
} from './input-closure.mjs';

async function tree(files = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rk-closure-'));
  for (const [path, body] of Object.entries(files)) { await mkdir(join(root, path, '..'), { recursive: true }); await writeFile(join(root, path), body); }
  return { root, close: () => rm(root, { recursive: true, force: true }) };
}
const SOURCES = ['Cargo.toml', 'Cargo.lock', 'src/lib.rs', 'pnpm-lock.yaml'];
// Closure listing is injected: a plain temp directory is not a git repository.
const guard = (options = {}) => new InputClosureGuard({ list: async () => SOURCES, ...options });
const base = () => ({ 'Cargo.toml': '[package]\n', 'Cargo.lock': 'v1\n', 'src/lib.rs': 'fn a() {}\n', 'pnpm-lock.yaml': 'lock\n' });

test('digest is stable for identical bytes and changes when any closure byte changes', async () => {
  const t = await tree(base());
  try {
    const a = await digestInputClosure(t.root, SOURCES);
    const b = await digestInputClosure(t.root, [...SOURCES].reverse());
    assert.equal(a.digest, b.digest, 'order of listing must not matter');
    await writeFile(join(t.root, 'src/lib.rs'), 'fn b() {}\n');
    const c = await digestInputClosure(t.root, SOURCES);
    assert.notEqual(a.digest, c.digest);
    assert.equal(a.count, 4);
  } finally { await t.close(); }
});

test('A -> B mutation that persists to completion is reported as changed (inputs-mutated)', async () => {
  const t = await tree(base());
  try {
    const g = guard();
    const snapshot = await g.snapshot(t.root);
    assert.equal(snapshot.status, INPUTS_VERIFIED);
    await writeFile(join(t.root, 'src/lib.rs'), 'fn tampered() {}\n');
    const verdict = await g.verify(snapshot);
    assert.equal(verdict.status, INPUTS_MUTATED);
    assert.deepEqual(verdict.changed, ['src/lib.rs']);
    assert.notEqual(verdict.preDigest, verdict.postDigest);
  } finally { await t.close(); }
});

test('added and removed inputs are named in the verdict', async () => {
  const t = await tree(base());
  try {
    const g = guard();
    const snapshot = await g.snapshot(t.root);
    // A new file joins the git-listed closure; a listed file disappears from it.
    const withNew = guard({ list: async () => [...SOURCES.filter((p) => p !== 'Cargo.lock'), 'build.rs'] });
    await writeFile(join(t.root, 'build.rs'), 'fn main() {}\n');
    const verdict = await withNew.verify(snapshot);
    assert.equal(verdict.status, INPUTS_MUTATED);
    assert.deepEqual(verdict.added, ['build.rs']);
    assert.deepEqual(verdict.removed, ['Cargo.lock']);
    assert.deepEqual(verdict.changed, []);
  } finally { await t.close(); }
});

test('A -> B -> A inside the snapshot window is NOT detected (documented limit of two-point checks)', async () => {
  const t = await tree(base());
  try {
    const g = guard();
    const snapshot = await g.snapshot(t.root);
    await writeFile(join(t.root, 'src/lib.rs'), 'fn b() {}\n');
    await writeFile(join(t.root, 'src/lib.rs'), 'fn a() {}\n');
    const verdict = await g.verify(snapshot);
    assert.equal(verdict.status, INPUTS_VERIFIED, 'a change reverted before completion leaves the digest unchanged');
  } finally { await t.close(); }
});

test('an extra authenticated digest (catalog/trust/approval) moving marks the closure mutated', async () => {
  const t = await tree(base());
  try {
    let catalog = 'catalog-1';
    const g = guard({ extraDigests: async () => ({ catalog }) });
    const snapshot = await g.snapshot(t.root);
    catalog = 'catalog-2';
    const verdict = await g.verify(snapshot);
    assert.equal(verdict.status, INPUTS_MUTATED); assert.deepEqual(verdict.extrasChanged, ['catalog']);
    assert.deepEqual(verdict.changed, []);
  } finally { await t.close(); }
});

test('symlinks are hashed as links and never followed; a retargeted link is a mutation', async () => {
  const t = await tree({ 'real.rs': 'one\n' });
  try {
    await symlink('real.rs', join(t.root, 'link.rs'));
    const g = guard({ list: async () => ['real.rs', 'link.rs'] });
    const snapshot = await g.snapshot(t.root);
    await rm(join(t.root, 'link.rs')); await symlink('other.rs', join(t.root, 'link.rs'));
    const verdict = await g.verify(snapshot);
    assert.equal(verdict.status, INPUTS_MUTATED);
    assert.deepEqual(verdict.changed, ['link.rs']);
  } finally { await t.close(); }
});

test('unreadable closure is recorded unverified, not verified and not a crash', async () => {
  const guard = new InputClosureGuard({ list: async () => { throw new Error('git unavailable'); } });
  const snapshot = await guard.snapshot('/nowhere');
  assert.equal(snapshot.status, INPUTS_UNVERIFIED); assert.match(snapshot.reason, /git unavailable/);
  const verdict = await guard.verify(snapshot);
  assert.equal(verdict.status, INPUTS_UNVERIFIED);
});

test('verify without a pre-spawn snapshot is unverified, never verified', async () => {
  const guard = new InputClosureGuard();
  assert.equal((await guard.verify(undefined)).status, INPUTS_UNVERIFIED);
});

test('path normalization rejects traversal, absolute paths, NUL and anything under .git', () => {
  for (const bad of ['../x', '/etc/passwd', 'a\0b', '', '.git/config', '.git', 'a/../../b']) {
    assert.throws(() => normalizeClosurePath(bad), (error) => error.code === 'RK_INPUT_PATH_INVALID', bad);
  }
  assert.equal(normalizeClosurePath('src/./lib.rs'), 'src/lib.rs');
});

test('gitListedClosure lists tracked and untracked-not-ignored files only, NUL-split, and never .git', async () => {
  const calls = [];
  const exec = async (command, args) => { calls.push([command, args]); return { stdout: 'b.rs\0a.rs\0Cargo.toml\0' }; };
  const listed = await gitListedClosure('/repo', { exec });
  assert.deepEqual(listed, ['Cargo.toml', 'a.rs', 'b.rs']);
  assert.deepEqual(calls[0][1], ['-C', '/repo', 'ls-files', '-z', '--cached', '--others', '--exclude-standard']);
});

test('receipts with a mutated closure are never promotable to evidence; unverified ones are refused too', () => {
  const mutated = recordInputClosure({ id: 'job-1' }, { status: INPUTS_MUTATED, changed: ['src/lib.rs'] });
  assert.throws(() => assertPromotableReceipt(mutated), (error) => error.code === 'RK_INPUTS_MUTATED');
  const unverified = recordInputClosure({ id: 'job-2' }, { status: INPUTS_UNVERIFIED, reason: 'x' });
  assert.throws(() => assertPromotableReceipt(unverified), (error) => error.code === 'RK_INPUTS_UNVERIFIED');
  const verified = recordInputClosure({ id: 'job-3' }, { status: INPUTS_VERIFIED });
  assert.equal(assertPromotableReceipt(verified), true);
  assert.equal(Object.isFrozen(mutated), true, 'recorded receipts stay frozen');
});
