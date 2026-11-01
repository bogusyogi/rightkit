import test from 'node:test';
import assert from 'node:assert/strict';
import {
  APPROVAL_CODE_TEXT,
  approvalFailureText,
  batchRefusalText,
  buildComposeInput,
  withStaged,
  checkSlotFill,
  countStates,
  draftIdsNeedingHash,
  hashKnownFor,
  INITIAL_REVIEW_STATE,
  itemActionsFor,
  mediaTypeOf,
  moderationRows,
  refusalView,
  reviewReducer,
  scenarioLines,
  scenarioProblem,
  stagedSteps,
  stagePlanText,
  unservedRouteNames,
  violationsBySlot,
  violationText
} from '../views/recipe-templates-logic.ts';

const slot = (position, name, type, required = true) => ({
  position,
  name,
  type,
  engine_role: 'reference',
  required,
  max: 1,
  role: `${name} role`
});

const motionSlots = [
  slot(1, 'subject_image', 'image'),
  slot(2, 'motion_video', 'video'),
  slot(3, 'identity_closeup', 'image', false),
  slot(4, 'outfit_image', 'image', false)
];

const item = (over) => ({
  index: 1,
  scenario: 'a scene',
  state: 'pending',
  draft_id: 'd1',
  refusal: null,
  identity_attached: true,
  ...over
});

test('mediaTypeOf reads the engine extensions and ignores query strings and paths without an extension', () => {
  assert.equal(mediaTypeOf('/Users/me/face.PNG'), 'image');
  assert.equal(mediaTypeOf('https://cdn.example/clip.mp4?sig=1'), 'video');
  assert.equal(mediaTypeOf('/a.b/c'), null);
  assert.equal(mediaTypeOf('https://example.com/file'), null);
});

test('checkSlotFill sends filled slots in declared order and names each position', () => {
  const r = checkSlotFill(motionSlots, ['/p/face.png', '/p/move.mp4', '', '']);
  assert.equal(r.ready, true);
  assert.deepEqual(r.problems, []);
  assert.deepEqual(r.entries, [
    { type: 'image', value: '/p/face.png', slot: 1 },
    { type: 'video', value: '/p/move.mp4', slot: 2 }
  ]);
});

test('checkSlotFill flags a required slot left empty and never reports ready', () => {
  const r = checkSlotFill(motionSlots, ['/p/face.png', '', '/p/close.png', '']);
  assert.equal(r.ready, false);
  assert.deepEqual(r.problems.map((p) => [p.position, p.code]), [[2, 'slot_missing']]);
});

test('checkSlotFill refuses an optional gap before a later filled slot', () => {
  const r = checkSlotFill(motionSlots, ['/p/face.png', '/p/move.mp4', '', '/p/outfit.png']);
  assert.equal(r.ready, false);
  assert.deepEqual(r.problems.map((p) => [p.position, p.code]), [[3, 'slot_order']]);
});

test('checkSlotFill points a media type mismatch at its slot', () => {
  const r = checkSlotFill(motionSlots, ['/p/move.mp4', '/p/face.png']);
  assert.equal(r.ready, false);
  const types = r.problems.filter((p) => p.code === 'slot_type');
  assert.deepEqual(types.map((p) => p.position), [1, 2]);
  assert.match(types[0].text, /looks like a video/);
  assert.match(types[0].text, /slot 1 \(subject_image\) needs an image/);
});

test('checkSlotFill is not ready with no slots filled at all', () => {
  const r = checkSlotFill(motionSlots, ['', '', '', '']);
  assert.equal(r.ready, false);
  assert.deepEqual(r.entries, []);
});

test('violationsBySlot groups engine violations by position and keeps overflow apart', () => {
  const grouped = violationsBySlot(
    [
      { position: 2, slot: 'motion_video', code: 'slot_type', problem: 'motion_video takes video, got image' },
      { position: 3, slot: 'identity_closeup', code: 'slot_unservable', problem: '' },
      { position: 5, slot: '', code: 'slot_overflow', problem: '5 slots given but the template declares 4' }
    ],
    motionSlots
  );
  assert.deepEqual(grouped.bySlot[2], ['motion_video takes video, got image']);
  assert.deepEqual(grouped.bySlot[3], ['The chosen route has no input for this slot, so the request is refused rather than sent without it.']);
  assert.deepEqual(grouped.general, ['5 slots given but the template declares 4']);
});

test('violationText falls back to the code sentence when the engine gives no problem text', () => {
  assert.equal(violationText({ position: 1, slot: 'x', code: 'slot_order', problem: '' }), 'Slots are filled in order, starting from slot 1.');
  assert.equal(violationText({ position: 1, slot: 'x', code: 'weird_code', problem: '' }), 'weird_code');
});

test('stagedSteps: the final stays disabled until its draft stage is approved', () => {
  const drafts = [
    { draft_id: 'draft-1', stage: 'draft', requires_approval_of: null },
    { draft_id: 'final-1', stage: 'final', requires_approval_of: 'draft-1' }
  ];
  const before = stagedSteps(drafts, []);
  assert.equal(before[0].canApprove, true);
  assert.equal(before[1].canApprove, false);
  assert.match(before[1].blockedBy, /Approve the draft stage first/);

  const after = stagedSteps(drafts, ['draft-1']);
  assert.equal(after[0].approved, true);
  assert.equal(after[0].canApprove, false);
  assert.equal(after[1].canApprove, true);
  assert.equal(after[1].blockedBy, null);
  assert.equal(after[1].label, 'Final at the requested resolution');
});

test('stagedSteps: a single request is approvable once and not again', () => {
  const [step] = stagedSteps([{ draft_id: 's', stage: 'single', requires_approval_of: null }], ['s']);
  assert.equal(step.approved, true);
  assert.equal(step.canApprove, false);
  assert.equal(step.blockedBy, null);
});

test('stagePlanText explains one stage, two stages and unavailable plans', () => {
  assert.match(stagePlanText({ available: true, param: 'resolution', options: ['720p', '1080p'], lowest: '720p', requested: '720p', single_stage: true, identical_prompt: true, second_requires_first_approval: true }), /one request/);
  assert.match(stagePlanText({ available: true, param: 'resolution', options: ['720p', '1080p'], lowest: '720p', requested: '1080p', single_stage: false, identical_prompt: true, second_requires_first_approval: true }), /A draft at 720p, then the final at 1080p/);
  assert.equal(stagePlanText({ available: false, param: 'resolution', reason: 'no resolution input' }), 'Draft first is not available: no resolution input');
});

test('refusalView names each missing route for route_not_served', () => {
  const view = refusalView({
    ok: false,
    code: 'route_not_served',
    message: 'x',
    violations: [],
    missing_routes: [{ provider: 'fal', model: 'higgsfield/genjutsu/object-swap' }]
  });
  assert.equal(view.headline, 'Unavailable: no available route for this template.');
  assert.equal(view.details.length, 1);
  assert.match(view.details[0], /fal\/higgsfield\/genjutsu\/object-swap is not in the app's catalogue/);
});

test('refusalView shows staged_unavailable with the engine message', () => {
  const view = refusalView({ ok: false, code: 'staged_unavailable', message: 'wavespeed/x serves no resolution parameter', violations: [] });
  assert.equal(view.headline, 'Draft first is not available for this route.');
  assert.deepEqual(view.details, ['wavespeed/x serves no resolution parameter']);
});

test('approvalFailureText maps dependency_unapproved and keeps unknown engine text', () => {
  assert.equal(approvalFailureText('dependency_unapproved', 'engine text'), APPROVAL_CODE_TEXT.dependency_unapproved);
  assert.equal(approvalFailureText('some_new_code', 'engine text'), 'engine text');
  assert.equal(approvalFailureText(null, 'plain'), 'plain');
});

test('moderationRows labels generic notes and names repository sources', () => {
  const rows = moderationRows({
    notes: [{ text: 'Change the outfit image if refused.', generic: false, source: 'docs/plans/x.md:56' }],
    generic: [{ text: 'Faces of real people need consent.', generic: true, source: null }],
    note: ''
  });
  assert.equal(rows[0].label, "From the project's notes");
  assert.equal(rows[0].source, 'docs/plans/x.md:56');
  assert.equal(rows[1].generic, true);
  assert.equal(rows[1].label, 'General caution, not a provider rule');
  assert.equal(rows[1].source, null);
});

test('buildComposeInput sends a route only when one is chosen and resolution only when staged', () => {
  const template = { template: 'motion_transfer', slots: motionSlots };
  const auto = buildComposeInput(template, ['/p/face.png', '/p/move.mp4', '', ''], {});
  assert.deepEqual(auto, { template: 'motion_transfer', slots: auto.slots });
  assert.equal('provider' in auto, false);

  const routed = buildComposeInput(template, ['/p/face.png', '/p/move.mp4', '', ''], {
    route: { provider: 'fal', model: 'fal-ai/kling-video/v3/pro/motion-control' },
    staged: true,
    resolution: ' 1080p '
  });
  assert.equal(routed.provider, 'fal');
  assert.equal('staged' in routed, false);
  assert.deepEqual(routed.params, { resolution: '1080p' });
  assert.deepEqual(withStaged(routed, true).staged, true);
  assert.equal(withStaged(routed, false).staged, false);

  const notStaged = buildComposeInput(template, ['/p/face.png', '/p/move.mp4', '', ''], { resolution: '1080p' });
  assert.equal('params' in notStaged, false);
});

test('unservedRouteNames lists only the routes the catalogue does not serve', () => {
  const names = unservedRouteNames({
    template: 'object_replacement',
    title: '',
    kind: 'replace_object',
    served: false,
    routes: [
      { provider: 'fal', model: 'higgsfield/genjutsu/object-swap', served: false },
      { provider: 'wavespeed', model: 'pixverse/swap', served: false }
    ],
    slots: [],
    prompt: { lead: '', lock: '' },
    staged_resolution: null,
    moderation: { notes: [], generic: [], note: '' }
  });
  assert.deepEqual(names, ['fal/higgsfield/genjutsu/object-swap', 'wavespeed/pixverse/swap']);
});

test('itemActionsFor: pending items need a known hash before approval; decided and refused items allow nothing', () => {
  assert.deepEqual(itemActionsFor(item({}), false), { approve: false, reject: true, note: 'Getting the item ready for approval.' });
  assert.deepEqual(itemActionsFor(item({}), true), { approve: true, reject: true, note: null });
  const noIdentity = itemActionsFor(item({ identity_attached: false }), true);
  assert.equal(noIdentity.approve, true);
  assert.match(noIdentity.note, /approval will be refused/);
  assert.equal(itemActionsFor(item({ state: 'approved' }), true).approve, false);
  assert.equal(itemActionsFor(item({ state: 'rejected' }), true).reject, false);
  assert.equal(itemActionsFor(item({ state: 'refused', draft_id: null }), true).approve, false);
});

test('batchRefusalText reads reference_route_unsupported and falls back to the engine message', () => {
  assert.match(batchRefusalText({ code: 'reference_route_unsupported', message: 'raw' }), /does not take character references/);
  assert.equal(batchRefusalText({ code: 'other', message: 'raw text' }), 'raw text');
});

test('countStates tallies each state', () => {
  const counts = countStates([
    item({ index: 1, state: 'pending' }),
    item({ index: 2, state: 'approved' }),
    item({ index: 3, state: 'rejected' }),
    item({ index: 4, state: 'refused', draft_id: null })
  ]);
  assert.deepEqual(counts, { items: 4, pending: 1, approved: 1, rejected: 1, refused: 1 });
});

test('scenarioLines trims and drops blank lines; scenarioProblem enforces the cap from the template data', () => {
  const lines = scenarioLines(' ground shot \n\n  close up  \r\n');
  assert.deepEqual(lines, ['ground shot', 'close up']);
  assert.equal(scenarioProblem([], 24), 'Add at least one scenario line, one per line.');
  assert.equal(scenarioProblem(lines, 24), null);
  const many = Array.from({ length: 26 }, (_, i) => `line ${i}`);
  assert.equal(scenarioProblem(many, 24), 'Remove 2 lines: a batch takes at most 24 scenario lines.');
  assert.equal(scenarioProblem(['a', 'b'], 1), 'Remove 1 line: a batch takes at most 1 scenario line.');
});

const view = (items, over = {}) => ({
  ok: true,
  batch_id: 'chrbatch-1',
  template: 'character_reference_batch',
  character_id: 'c1',
  lock: {},
  review: { mode: 'data_only', compositing: 'x' },
  counts: countStates(items),
  items,
  ...over
});

test('reviewReducer: a new engine view clears busy and item errors and keeps the hashes', () => {
  let s = reviewReducer(INITIAL_REVIEW_STATE, { type: 'hash', draftId: 'd1', hash: 'abc' });
  s = reviewReducer(s, { type: 'busy', index: 1 });
  s = reviewReducer(s, { type: 'itemError', index: 1, message: 'identity_not_attached' });
  assert.equal(s.busyIndex, null);
  assert.equal(s.itemErrors[1], 'identity_not_attached');
  s = reviewReducer(s, { type: 'view', view: view([item({ state: 'approved' })]) });
  assert.equal(s.batchId, 'chrbatch-1');
  assert.deepEqual(s.itemErrors, {});
  assert.equal(s.hashes.d1, 'abc');
});

test('reviewReducer: busy marks one item and load errors set and clear', () => {
  let s = reviewReducer(INITIAL_REVIEW_STATE, { type: 'busy', index: 2 });
  assert.equal(s.busyIndex, 2);
  s = reviewReducer(s, { type: 'loadError', message: 'gone' });
  assert.equal(s.loadError, 'gone');
  s = reviewReducer(s, { type: 'reset' });
  assert.deepEqual(s, INITIAL_REVIEW_STATE);
});

test('draftIdsNeedingHash lists only pending drafts without a known hash', () => {
  const v = view([
    item({ index: 1, draft_id: 'a', state: 'pending' }),
    item({ index: 2, draft_id: 'b', state: 'pending' }),
    item({ index: 3, draft_id: 'c', state: 'approved' }),
    item({ index: 4, draft_id: null, state: 'refused' })
  ]);
  assert.deepEqual(draftIdsNeedingHash(v, { a: 'h1' }), ['b']);
  assert.deepEqual(draftIdsNeedingHash(null, {}), []);
  assert.equal(hashKnownFor(v.items[0], { a: 'h1' }), true);
  assert.equal(hashKnownFor(v.items[1], { a: 'h1' }), false);
});
