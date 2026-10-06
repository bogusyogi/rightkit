import assert from 'node:assert/strict';
import test from 'node:test';
import { checkPublicationGate, enforcePublicationGate } from './publication-gate.mjs';

const SHA = 'a'.repeat(40);
const run = (os, over = {}) => ({ host: { os, arch: 'arm64' }, commit: SHA, command: 'rightkit cargo test -p x --all-targets', passed: 12, failed: 0, features: ['default'], operator: 'adrdsouza', timestamp: '2026-10-06T00:00:00Z', ...over });
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
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32', { commit: 'c'.repeat(40) })] }))).missing.join('\n'), /Windows run receipt must name commit/);
  assert.match(checkPublicationGate(base(evidence({ runs: [run('darwin'), run('win32', { failed: 1 })] }))).missing.join('\n'), /Windows run receipt/);
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
