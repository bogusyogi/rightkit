import test from 'node:test';
import assert from 'node:assert/strict';
import {
  ALL,
  EYES_STATES,
  NO_FILTERS,
  NO_PROJECT_FILTER,
  NO_PROJECT_LABEL,
  activeFilterCount,
  callerLabel,
  matchesFilters,
  optionSourceItems,
  optionsFor,
  projectLabel,
  serverFilters,
  stateLabel,
  stateTone
} from '../lib/gallery-filters.ts';

const item = (project, brand, eyes = 'unknown') => ({
  type: 'image',
  project,
  brand,
  eyes,
  jury: 'unknown',
  metadata: 'complete',
  haystack: `${project} ${brand}`.toLowerCase()
});

test('eyes states include stale, and a stale item is not approved', () => {
  assert.ok(EYES_STATES.includes('stale'));
  const stale = item('shoot', 'DD', 'stale');
  assert.equal(matchesFilters(stale, { ...NO_FILTERS, eyes: 'approved' }), false);
  assert.equal(matchesFilters(stale, { ...NO_FILTERS, eyes: 'stale' }), true);
});

test('stale has a label and a warning tone', () => {
  assert.equal(stateLabel('stale'), 'stale (edited after approval)');
  assert.equal(stateLabel('rework'), 'rework');
  assert.equal(stateTone('stale'), 'warn');
});

test('an empty project shows as No project, a named project as itself', () => {
  assert.equal(NO_PROJECT_LABEL, 'No project');
  assert.equal(projectLabel(''), 'No project');
  assert.equal(projectLabel('  '), 'No project');
  assert.equal(projectLabel('knife-launch'), 'knife-launch');
});

test('a project filter sends only the project to the engine', () => {
  assert.deepEqual(serverFilters({ ...NO_FILTERS, project: 'knife-launch' }), { project: 'knife-launch' });
  assert.deepEqual(serverFilters({ ...NO_FILTERS, eyes: 'stale' }), { eyes: 'stale' });
  assert.deepEqual(serverFilters(NO_FILTERS), {});
});

test('No project is sent as no_project, never as an empty project', () => {
  assert.equal(NO_PROJECT_FILTER, '');
  assert.deepEqual(serverFilters({ ...NO_FILTERS, project: NO_PROJECT_FILTER }), { no_project: true });
  assert.deepEqual(serverFilters({ ...NO_FILTERS, project: NO_PROJECT_FILTER, brand: 'DD' }), { no_project: true, brand: 'DD' });
  assert.equal(matchesFilters(item('', 'NA'), { ...NO_FILTERS, project: NO_PROJECT_FILTER }), true, 'library asset matches');
  assert.equal(matchesFilters(item('knife-launch', 'DD'), { ...NO_FILTERS, project: NO_PROJECT_FILTER }), false, 'named project does not');
  assert.equal(activeFilterCount({ ...NO_FILTERS, project: NO_PROJECT_FILTER }), 1, 'an active filter');
});

test('the Project dropdown offers No project first, with its live count', () => {
  const items = [item('', 'NA'), item('', 'NA'), item('knife-launch', 'DD')];
  const options = optionsFor(items.map((i) => i.project), [NO_PROJECT_FILTER], projectLabel);
  assert.deepEqual(options.map((o) => [o.value, o.label, o.count]), [['', 'No project', 2], ['knife-launch', 'knife-launch', 1]]);
});

test('options keep every project and brand while a project filter narrows the loaded list', () => {
  const unfiltered = [item('knife-launch', 'DD'), item('bead-set', 'DD'), item('founder-story', 'RH'), item('stock', 'NA')];
  const narrowed = [item('knife-launch', 'DD')];

  const withoutSource = optionsFor(optionSourceItems(narrowed, unfiltered, false).map((i) => i.project), []);
  assert.deepEqual(withoutSource.map((o) => o.value), ['knife-launch'], 'loaded list alone shrinks the choices');

  const source = optionSourceItems(narrowed, unfiltered, true);
  assert.deepEqual(
    optionsFor(source.map((i) => i.project), []).map((o) => o.value),
    ['bead-set', 'founder-story', 'knife-launch', 'stock'],
    'unfiltered source keeps all projects'
  );
  const brands = optionsFor(source.map((i) => i.brand), ['DD', 'RH'], (c) => c);
  assert.deepEqual(brands.map((o) => [o.value, o.count]), [['DD', 2], ['RH', 1], ['NA', 1]], 'fixed brands plus unfiltered extras');
});

test('option counts come from the source they are computed over', () => {
  const unfiltered = [item('a', 'DD', 'approved'), item('a', 'DD', 'stale'), item('b', 'RH', 'approved')];
  const eyes = optionsFor(unfiltered.map((i) => i.eyes), EYES_STATES, stateLabel);
  const count = (v) => eyes.find((o) => o.value === v).count;
  assert.equal(count('approved'), 2);
  assert.equal(count('stale'), 1);
  assert.equal(eyes.find((o) => o.value === 'stale').label, 'stale (edited after approval)');
  assert.equal(optionSourceItems([], unfiltered, true).length, 3);
});

test('the loaded list is the source when no server filter is active', () => {
  const loaded = [item('a', 'DD')];
  assert.equal(optionSourceItems(loaded, [], false), loaded);
  assert.equal(ALL, 'all');
});

test('a caller filter is sent to the engine and matches only that caller', () => {
  const from = (caller) => ({ ...item('', 'NA'), caller });
  assert.deepEqual(serverFilters({ ...NO_FILTERS, caller: 'postright' }), { caller: 'postright' });
  assert.deepEqual(serverFilters(NO_FILTERS), {});
  assert.equal(matchesFilters(from('postright'), { ...NO_FILTERS, caller: 'postright' }), true);
  assert.equal(matchesFilters(from('cutright'), { ...NO_FILTERS, caller: 'postright' }), false);
  assert.equal(matchesFilters(from(''), { ...NO_FILTERS, caller: 'postright' }), false, 'a file no caller registered');
  assert.equal(matchesFilters(from(''), NO_FILTERS), true);
  assert.equal(activeFilterCount({ ...NO_FILTERS, caller: 'postright' }), 1);
});

test('caller options read "From <label>" using the label the caller gave', () => {
  const labels = new Map([['postright', 'App']]);
  assert.equal(callerLabel('postright', labels), 'From App');
  assert.equal(callerLabel('agent-7', labels), 'From agent-7', 'no recorded label falls back to the slug');
  const options = optionsFor(['postright', 'postright', 'cutright'], [], (slug) => callerLabel(slug, labels));
  assert.deepEqual(options.map((o) => [o.value, o.label, o.count]), [['cutright', 'From cutright', 1], ['postright', 'From App', 2]]);
});
