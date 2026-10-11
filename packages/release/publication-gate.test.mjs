import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import test from 'node:test';
import { checkPublicationGate, enforcePublicationGate } from './publication-gate.mjs';

const SHA = 'a'.repeat(40);
const run = (os, over = {}) => ({ host: { os, arch: 'arm64' }, commit: SHA, artifactSha256: 'b'.repeat(64), command: 'rightkit cargo test -p x --all-targets', passed: 12, failed: 0, features: ['default'], operator: 'adrdsouza', timestamp: '2026-10-06T00:00:00Z', ...over });
const evidence = (over = {}) => ({ schema: 1, kind: 'cargo', name: 'rightkit-sync', version: '0.1.0', commit: SHA, artifactSha256: 'b'.repeat(64), runs: [run('darwin'), run('win32')], ...over });
const base = (ev, { head = SHA, dirty = '' } = {}) => ({
  kind: 'cargo', name: 'rightkit-sync', version: '0.1.0', packagePath: '/p', evidencePath: '/e.json',
  git: (args) => (args[0] === 'rev-parse' ? head : dirty),
  readFile: () => { if (!ev) throw new Error('ENOENT'); return JSON.stringify(ev); },
});

test('gate passes with exact SHA, clean tree, macOS and Windows receipts', () => {
  assert.deepEqual(checkPublicationGate(base(evidence())), { ok: true, missing: [] });
});

test('gate reports every missing piece', () => {
  assert.match(checkPublicationGate(base(null)).missing.join('\n'), /evidence file missing/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin')] }))).missing.join('\n'), /no Windows host run receipt/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('win32')] }))).missing.join('\n'), /no macOS host run receipt/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32', { commit: 'c'.repeat(40) })] }))).missing.join('\n'), /Windows run receipt \d invalid: commit/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32', { failed: 1 })] }))).missing.join('\n'), /Windows run receipt \d invalid/);
  assert.match(checkPublicationGate(base(evidence(), { head: 'd'.repeat(40) })).missing.join('\n'), /!= current HEAD/);
  assert.match(checkPublicationGate(base(evidence(), { dirty: ' M src/lib.rs' })).missing.join('\n'), /not clean/);
});

test('enforce refuses a real publish and only reports under dry-run', () => {
  assert.throws(() => enforcePublicationGate(base(null)), /publication gate not satisfied/);
  let out = '';
  const result = enforcePublicationGate(base(null), { dryRun: true, write: (l) => { out += l; } });
  assert.equal(result.ok, false);
  assert.match(out, /would be refused/);
});

test('Swift packages need only a macOS receipt', () => {
  const swift = (runs) => checkPublicationGate({ ...base(evidence({ kind: 'swift', name: 'RightKitSwift', runs })), kind: 'swift', name: 'RightKitSwift' });
  assert.deepEqual(swift([run('darwin')]).missing, []);
  assert.match(swift([run('win32')]).missing.join('\n'), /no macOS host run receipt/);
});

const gitFor = ({ head = SHA, dirty = '', known = 'commit', ancestor = true, behind = '3', changed = '' } = {}) => (args) => {
  if (args[0] === 'rev-parse') return head;
  if (args[0] === 'status') return dirty;
  if (args[0] === 'cat-file') return known;
  if (args[0] === 'merge-base') return ancestor ? '' : null;
  if (args[0] === 'rev-list') return behind;
  if (args[0] === 'diff') return changed;
  return null;
};
const withGit = (ev, git, over = {}) => ({ ...base(ev), git, ...over });

test('refuses a receipt that does not name the publishing version, and an unknown publishing version', () => {
  assert.match(checkPublicationGate(withGit(evidence({ version: '0.0.9' }), gitFor())).missing.join('\n'), /evidence version 0\.0\.9 != 0\.1\.0/);
  // Before this check a missing version skipped the comparison entirely.
  assert.match(checkPublicationGate(withGit(evidence({ version: '0.0.9' }), gitFor(), { version: undefined })).missing.join('\n'), /publishing version is unknown/);
  assert.match(checkPublicationGate(withGit(evidence(), gitFor(), { version: '' })).missing.join('\n'), /publishing version is unknown/);
});

test('refuses a receipt for another unit', () => {
  assert.match(checkPublicationGate(withGit(evidence({ name: 'rightkit-other' }), gitFor())).missing.join('\n'), /evidence is for cargo:rightkit-other/);
  assert.match(checkPublicationGate(withGit(evidence({ kind: 'npm' }), gitFor())).missing.join('\n'), /evidence is for npm:rightkit-sync/);
});

test('a receipt commit behind HEAD is refused and the reason says whether the unit path changed', () => {
  const old = 'e'.repeat(40);
  const ev = evidence({ commit: old, runs: [run('darwin', { commit: old }), run('win32', { commit: old })] });
  const unchanged = checkPublicationGate(withGit(ev, gitFor({ changed: '' })));
  assert.equal(unchanged.ok, false);
  assert.match(unchanged.missing.join('\n'), /ancestor, 3 commit\(s\) behind HEAD; unit path unchanged since the receipt/);
  const changed = checkPublicationGate(withGit(ev, gitFor({ changed: 'src/lib.rs\nsrc/x.rs' })));
  assert.equal(changed.ok, false);
  assert.match(changed.missing.join('\n'), /unit path changed since the receipt: src\/lib\.rs, src\/x\.rs/);
});

test('a receipt commit that is not an ancestor, or not a commit object, is refused', () => {
  const other = 'e'.repeat(40);
  const ev = evidence({ commit: other, runs: [run('darwin', { commit: other }), run('win32', { commit: other })] });
  assert.match(checkPublicationGate(withGit(ev, gitFor({ ancestor: false }))).missing.join('\n'), /not an ancestor of HEAD/);
  assert.match(checkPublicationGate(withGit(ev, gitFor({ known: null }))).missing.join('\n'), /not a commit object/);
});

test('an unreadable HEAD and an unreadable status both refuse', () => {
  assert.match(checkPublicationGate(withGit(evidence(), () => null)).missing.join('\n'), /cannot read current commit SHA/);
  assert.match(checkPublicationGate(withGit(evidence(), () => null)).missing.join('\n'), /cannot read git status/);
});

test('a dirty unit path refuses even with a matching receipt', () => {
  assert.match(checkPublicationGate(withGit(evidence(), gitFor({ dirty: '?? src/new.rs' }))).missing.join('\n'), /tree is not clean/);
});

test('a host run must name arch and features, and any artifact hash it names must equal the evidence hash', () => {
  const noArch = run('win32'); delete noArch.host.arch;
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin'), noArch] }))).missing.join('\n'), /Windows run receipt \d invalid: host\.arch/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin', { features: [] }), run('win32')] }))).missing.join('\n'), /macOS run receipt \d invalid/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32', { features: undefined })] }))).missing.join('\n'), /Windows run receipt \d invalid/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32', { artifactSha256: 'c'.repeat(64) })] }))).missing.join('\n'), /Windows run receipt \d invalid/);
  assert.deepEqual(checkPublicationGate(base(evidence({ runs: [run('darwin', { artifactSha256: 'b'.repeat(64) }), run('win32')] }))), { ok: true, missing: [] });
});

test('a run receipt from another commit never satisfies its host, even when another host is fine', () => {
  const stale = checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32', { commit: 'c'.repeat(40) })] })));
  assert.equal(stale.ok, false);
  assert.match(stale.missing.join('\n'), /Windows run receipt \d invalid: commit/);
});

test('a malformed artifact hash is refused and a packed artifact whose bytes differ from the receipt is refused', () => {
  assert.match(checkPublicationGate(base(evidence({ artifactSha256: 'xyz' }))).missing.join('\n'), /artifactSha256 must be a 64-hex hash/);
  const bytes = Buffer.from('packed');
  assert.match(checkPublicationGate({ ...base(evidence()), artifactBytes: bytes }).missing.join('\n'), /artifact sha256 does not match evidence/);
  const hash = crypto.createHash('sha256').update(bytes).digest('hex');
  assert.equal(checkPublicationGate({ ...base(evidence({ artifactSha256: hash, runs: [run('darwin', { artifactSha256: hash }), run('win32', { artifactSha256: hash })] })), artifactBytes: bytes }).ok, true);
});

test('a run that omits artifactSha256 is refused and the message names the field', () => {
  const noHash = run('win32'); delete noHash.artifactSha256;
  const result = checkPublicationGate(base(evidence({ runs: [run('darwin'), noHash] })));
  assert.equal(result.ok, false);
  assert.match(result.missing.join('\n'), /Windows run receipt 0 invalid: artifactSha256 \(must equal the evidence hash\)/);
});

test('every missing run field is named', () => {
  const result = checkPublicationGate(base(evidence({ runs: [run('darwin', { command: '', operator: ' ', timestamp: undefined, passed: 0, failed: 2, features: [] }), run('win32')] })));
  assert.match(result.missing.join('\n'), /macOS run receipt 0 invalid: features, command, operator, timestamp, passed \(must be > 0\), failed \(must be 0\)/);
});

test('a failing run on a host is not masked by a passing run on the same host', () => {
  const masked = checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32'), run('win32', { failed: 3, command: 'rightkit cargo test --features x' })] })));
  assert.equal(masked.ok, false);
  assert.match(masked.missing.join('\n'), /Windows run receipt 1 invalid: failed \(must be 0\)/);
  const stale = checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32'), run('win32', { commit: 'c'.repeat(40) })] })));
  assert.match(stale.missing.join('\n'), /Windows run receipt \d invalid: commit/);
  assert.equal(checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32'), run('win32', { features: ['x'] })] }))).ok, true);
});
