import test from 'node:test';
import assert from 'node:assert/strict';
import { mockEngine, mockLibraryCommand } from '../lib/browser-mock-api.ts';

const search = (input) => mockLibraryCommand('library.search', input);
const importAsset = (input) => mockLibraryCommand('asset.import', input);
const refused = async (promise, code) => {
  await assert.rejects(promise, (error) => error.code === code, `expected ${code}`);
};

test('a free-text search returns hits whose snippet marks the match in brackets', async () => {
  const res = await search({ query: 'chemist' });
  assert.ok(res.total >= 1, 'chemist matches at least one asset');
  assert.ok(res.hits.every((hit) => hit.prompt_snippet.includes('[Chemist]')), res.hits[0].prompt_snippet);
  assert.deepEqual(res.not_indexed, ['transcript', 'ocr', 'identity', 'embedding']);
  assert.equal(res.index.rebuilt, false);
});

test('hits page with next_cursor, and the last page has none', async () => {
  const first = await search({ query: 'take' });
  assert.ok(first.total > 20, 'the seed has more than one page of "take"');
  assert.equal(first.hits.length, 20);
  assert.equal(first.next_cursor, 'o20');
  const second = await search({ query: 'take', cursor: first.next_cursor });
  assert.equal(second.hits.length, first.total - 20);
  assert.equal(second.next_cursor, null);
  assert.ok(second.hits.every((hit) => !first.hits.some((h) => h.asset_id === hit.asset_id)), 'pages do not repeat');
});

test('operators filter the hits and each hit carries its facts and scope', async () => {
  const res = await search({ query: 'origin:acquired' });
  assert.ok(res.total > 0);
  assert.ok(res.hits.every((hit) => hit.facts.origin.kind === 'acquired'));
  const cleared = await search({ query: 'rights:cleared kind:image' });
  assert.ok(cleared.hits.every((hit) => hit.facts.rights.status === 'cleared' && hit.kind === 'image'));
});

test('a bad query is invalid_input with a readable reason', async () => {
  await refused(search({ query: 'colour:red' }), 'invalid_input');
  await refused(search({ query: '(chemist' }), 'invalid_input');
  await refused(search({ query: '"open phrase' }), 'invalid_input');
  await refused(search({ query: '()' }), 'invalid_input');
  await refused(search({ query: '   ' }), 'invalid_input');
  await refused(search({ query: '-' }), 'invalid_input');
  await refused(search({ query: 'origin:generatedish' }), 'invalid_input');
  await refused(search({ query: 'take', offset: 5 }), 'invalid_input');
  await refused(search({ query: 'take', cursor: 'bogus' }), 'invalid_input');
  await refused(search({ query: 'a'.repeat(257) }), 'invalid_input');
});

test('the import dry run reports what would be recorded and writes nothing', async () => {
  const before = (await search({ query: 'take' })).total;
  const dry = await importAsset({ path: '/Users/me/Pictures/dry-only.png', origin: 'supplied', evidence: 'authentic', dry_run: true });
  assert.equal(dry.dry_run, true);
  assert.equal(dry.would_import.facts.origin.kind, 'supplied');
  assert.equal(dry.would_import.facts.evidence, 'authentic');
  assert.equal((await search({ query: 'take' })).total, before);
});

test('a real import records the facts and scope, and the same file is refused the second time', async () => {
  const path = '/Users/me/Pictures/imported-once.png';
  const done = await importAsset({
    path,
    origin: 'acquired',
    rights: { status: 'cleared', licence_evidence: ['licence LS-99'], attribution: 'Stock, LS-99' },
    scope: { venture: 'orthic', channel: 'cutright-how-to', account: 'main' }
  });
  assert.equal(done.facts.origin.kind, 'acquired');
  assert.equal(done.facts.evidence, 'unknown');
  assert.equal(done.facts.rights.status, 'cleared');
  assert.deepEqual(done.scope, { venture: 'orthic', channel: 'cutright-how-to', account: 'main' });
  assert.equal(done.source_preserved, true);
  const acquired = await search({ query: 'origin:acquired' });
  assert.ok(acquired.hits.some((hit) => hit.asset_id === done.asset_id), 'the new asset is searchable at once');
  await refused(importAsset({ path, origin: 'supplied' }), 'already_in_library');
});

test('import refusals match the engine codes', async () => {
  const photo = '/Users/me/Pictures/refusal.png';
  await refused(importAsset({ path: photo, origin: 'generated' }), 'invalid_origin');
  await refused(importAsset({ path: photo }), 'invalid_origin');
  await refused(importAsset({ path: photo, origin: 'supplied', evidence: 'genuine' }), 'invalid_evidence');
  await refused(importAsset({ path: photo, origin: 'supplied', rights: { status: 'cleared' } }), 'invalid_rights');
  await refused(importAsset({ path: photo, origin: 'supplied', rights: { status: 'granted' } }), 'invalid_rights');
  await refused(importAsset({ path: photo, origin: 'supplied', scope: { venture: 'orthic' } }), 'invalid_scope');
  await refused(importAsset({ path: 'refusal.png', origin: 'supplied' }), 'invalid_request');
  await refused(importAsset({ path: photo, origin: 'supplied', colour: 'red' }), 'invalid_request');
  await refused(importAsset({ path: '/Users/me/Pictures/notes.txt', origin: 'supplied' }), 'unsupported_media');
});

test('gallery rows carry asset facts and scope, and an old row reads unknown', async () => {
  const res = await mockEngine('index_artifacts', { roots: [], offset: 0, limit: 200 });
  const puck = res.artifacts.find((a) => a.artifact_id === 'mock-puck-p02');
  assert.equal(puck.asset_facts.rights.status, 'cleared');
  assert.equal(puck.scope, null);
  const old = res.artifacts.find((a) => a.artifact_id === 'mock-unsorted-render');
  assert.equal(old.asset_facts.origin.kind, 'unknown');
  assert.equal(old.asset_facts.evidence, 'unknown');
  assert.equal(old.asset_facts.rights.status, 'unknown');
  const kdp = res.artifacts.find((a) => a.brand === 'KDP');
  assert.deepEqual(kdp.scope, { venture: 'orthic', channel: 'kdp-books', account: 'main' });
});
