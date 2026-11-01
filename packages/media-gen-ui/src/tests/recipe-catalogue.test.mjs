import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

// The resolver hook must be registered before the .ts modules load, so these imports are dynamic.
await import('./ts-resolve.mjs');
const {
  errorsByNode,
  missingRoutesText,
  nodeDepths,
  orderNodes,
  paramsText,
  routeLabel,
  SERVED_TEXT,
  servedText,
  servedTone,
  unbuiltPlanned,
  validationSummary
} = await import('../views/recipe-catalogue-logic.ts');
const { mockRecipeCatalogue } = await import('../lib/browser-mock-api.ts');

const readJson = (rel) => JSON.parse(readFileSync(new URL(rel, import.meta.url), 'utf8'));
const CATALOGUE = '../../crates/genright-studio/data/recipes/catalogue/';
const vox = readJson(`${CATALOGUE}vox_director.json`);
const motion = readJson(`${CATALOGUE}motion_transfer.json`);
const index = readJson(`${CATALOGUE}_index.json`);

// --- dependency order and depth -------------------------------------------------------------

test('orderNodes places each node after the nodes it depends on and keeps declaration order otherwise', () => {
  const nodes = [
    { id: 'captions', depends_on: ['scenes'] },
    { id: 'scenes', depends_on: ['keyframes'] },
    { id: 'keyframes', depends_on: ['beats'] },
    { id: 'beats', depends_on: [] },
    { id: 'music', depends_on: [] }
  ];
  assert.deepEqual(orderNodes(nodes).map((n) => n.id), ['beats', 'music', 'keyframes', 'scenes', 'captions']);
});

test('orderNodes keeps the seeded vox_director order and never lists a node before its dependency', () => {
  const ids = orderNodes(vox.nodes).map((n) => n.id);
  assert.deepEqual(ids, ['script', 'beats', 'keyframes', 'narration', 'scenes', 'captions']);
  const at = Object.fromEntries(ids.map((id, i) => [id, i]));
  for (const n of vox.nodes) {
    for (const d of n.depends_on) assert.ok(at[d] < at[n.id], `${d} comes before ${n.id}`);
  }
});

test('orderNodes ignores a dependency on no node and appends a cycle in declaration order', () => {
  const nodes = [
    { id: 'a', depends_on: ['b'] },
    { id: 'b', depends_on: ['a'] },
    { id: 'c', depends_on: ['ghost'] }
  ];
  assert.deepEqual(orderNodes(nodes).map((n) => n.id), ['c', 'a', 'b']);
});

test('nodeDepths gives the seeded vox_director its indent levels and terminates on a cycle', () => {
  const depth = nodeDepths(vox.nodes);
  assert.deepEqual(depth, { script: 0, beats: 0, keyframes: 1, narration: 1, scenes: 2, captions: 3 });
  const cyclic = nodeDepths([
    { id: 'a', depends_on: ['b'] },
    { id: 'b', depends_on: ['a'] }
  ]);
  assert.equal(typeof cyclic.a, 'number');
  assert.equal(typeof cyclic.b, 'number');
});

// --- status and route wording --------------------------------------------------------------

test('every served status has text, and an unknown status shows its own word rather than nothing', () => {
  for (const status of ['served', 'unserved', 'ask', 'native', 'invalid_route']) {
    assert.ok(servedText(status).length > 0, status);
    assert.equal(servedText(status), SERVED_TEXT[status]);
  }
  assert.equal(servedText('served'), 'Available');
  assert.equal(servedText('unserved'), 'Not available');
  assert.equal(servedText('ask'), 'Chosen at run time');
  assert.equal(servedText('mystery'), 'mystery');
});

test('chip tone follows the status, and an ask or native node has no colour at all', () => {
  assert.equal(servedTone('served'), 'ok');
  assert.equal(servedTone('unserved'), 'bad');
  assert.equal(servedTone('invalid_route'), 'bad');
  assert.equal(servedTone('ask'), undefined);
  assert.equal(servedTone('native'), undefined);
});

test('route text names provider and model, or says the run asks, or says there is no provider route', () => {
  assert.equal(routeLabel({ provider: 'fal', model: 'fal-ai/x' }, 'served'), 'fal · fal-ai/x');
  assert.equal(routeLabel('ask', 'ask'), 'chosen at run time');
  assert.equal(routeLabel(null, 'native'), 'built-in step, no provider');
});

test('settings line reads as the spec states when the recipe definition did not check them', () => {
  assert.equal(paramsText(false), 'settings not checked against the recipe definition');
  assert.equal(paramsText(true), 'settings checked against the recipe definition');
});

test('missing routes read preferred first, joined in order', () => {
  assert.equal(
    missingRoutesText([
      { provider: 'fal', model: 'higgsfield/genjutsu/object-swap' },
      { provider: 'wavespeed', model: 'pixverse/swap' }
    ]),
    'fal · higgsfield/genjutsu/object-swap; wavespeed · pixverse/swap'
  );
});

test('validation summary says valid or counts the errors', () => {
  const e = { code: 'cycle', node: 'a', message: 'm' };
  assert.equal(validationSummary(true, []), 'Valid: no errors');
  assert.equal(validationSummary(false, [e]), '1 error');
  assert.equal(validationSummary(false, [e, e]), '2 errors');
});

// --- error-to-node mapping -----------------------------------------------------------------

test('errorsByNode files each error under the node it names and keeps the rest at graph level', () => {
  const errors = [
    { code: 'dangling_reference', node: 'beats', message: 'x' },
    { code: 'cycle', node: 'scenes', message: 'y' },
    { code: 'dangling_reference', node: 'ghost', message: 'z' },
    { code: 'invalid_graph', node: null, message: 'w' }
  ];
  const mapped = errorsByNode(errors, ['beats', 'scenes']);
  assert.deepEqual(Object.keys(mapped.byNode).sort(), ['beats', 'scenes']);
  assert.equal(mapped.byNode.beats[0].code, 'dangling_reference');
  assert.equal(mapped.byNode.scenes[0].code, 'cycle');
  assert.deepEqual(mapped.graph.map((e) => e.message), ['z', 'w']);
  const total = Object.values(mapped.byNode).flat().length + mapped.graph.length;
  assert.equal(total, errors.length, 'no error is dropped');
});

// --- the plan's not-yet-built list ---------------------------------------------------------

test('the not-yet-built list leaves out seeded graphs and keeps each reason', () => {
  const planned = unbuiltPlanned(index.plan_named);
  assert.equal(planned.length, index.plan_named.filter((p) => !p.seeded).length);
  assert.ok(planned.every((p) => !p.seeded && p.reason.length > 0));
  assert.ok(!planned.some((p) => p.name === 'Vox-style explainer'));
});

// --- browser mock: real seeded graphs, one invalid-graph error path ------------------------

test('mock listing returns the three seeded graphs, valid, with the unserved swap node and its routes named', async () => {
  const list = await mockRecipeCatalogue('studio.recipes.catalogue', {});
  assert.deepEqual(list.graphs.map((g) => g.id), ['motion_transfer', 'object_replacement', 'vox_director']);
  assert.ok(list.graphs.every((g) => g.valid === true && g.errors.length === 0));

  const swap = list.graphs.find((g) => g.id === 'object_replacement').nodes.find((n) => n.id === 'swap');
  assert.equal(swap.status, 'unserved');
  assert.equal(swap.missing_routes.length, 3, 'preferred route and both alternatives');

  const objectGraph = list.graphs.find((g) => g.id === 'object_replacement');
  assert.equal(objectGraph.missing_routes.length, 3);
  assert.ok(objectGraph.missing_routes.every((m) => m.node === 'swap'));

  const shot = list.graphs.find((g) => g.id === 'motion_transfer').nodes.find((n) => n.id === 'motion');
  assert.equal(shot.status, 'served');
  assert.equal(shot.params_checked, false);

  const voxNodes = Object.fromEntries(list.graphs.find((g) => g.id === 'vox_director').nodes.map((n) => [n.id, n.status]));
  assert.equal(voxNodes.script, 'native');
  assert.equal(voxNodes.captions, 'native');
  assert.equal(voxNodes.narration, 'served');
});

test('mock validate by id passes a seeded graph, and an unknown id is refused with its code', async () => {
  const ok = await mockRecipeCatalogue('studio.recipes.catalogue_validate', { id: 'motion_transfer' });
  assert.equal(ok.ok, true);
  assert.equal(ok.id, 'motion_transfer');
  await assert.rejects(mockRecipeCatalogue('studio.recipes.catalogue_validate', { id: 'nope' }), { code: 'not_found' });
});

test('mock validate of an invalid inline graph names each bad node, and the view maps those errors back to the nodes', async () => {
  const bad = structuredClone(motion);
  bad.nodes[0].depends_on = ['ghost'];
  bad.nodes.push({ id: 'upscale_it', kind: 'telepathy', route: 'ask', params: {}, depends_on: ['motion'] });

  const result = await mockRecipeCatalogue('studio.recipes.catalogue_validate', { graph: bad });
  assert.equal(result.ok, false);
  assert.ok(result.errors.some((e) => e.code === 'dangling_reference' && e.node === 'motion'));
  assert.ok(result.errors.some((e) => e.code === 'unknown_kind' && e.node === 'upscale_it'));

  const mapped = errorsByNode(result.errors, bad.nodes.map((n) => n.id));
  assert.equal(mapped.byNode.motion[0].code, 'dangling_reference');
  assert.equal(mapped.byNode.upscale_it[0].code, 'unknown_kind');
  assert.equal(mapped.graph.length, 0);
});
