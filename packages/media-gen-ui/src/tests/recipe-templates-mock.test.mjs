import test from 'node:test';
import assert from 'node:assert/strict';
import { mockRecipeTemplateApprove, mockRecipeTemplates } from '../lib/browser-mock-api.ts';
import { checkSlotFill } from '../views/recipe-templates-logic.ts';

const call = (name, input = {}) => mockRecipeTemplates(name, input);
const motionSlots = [
  { position: 1, name: 'subject_image', type: 'image', required: true },
  { position: 2, name: 'motion_video', type: 'video', required: true },
  { position: 3, name: 'identity_closeup', type: 'image', required: false },
  { position: 4, name: 'outfit_image', type: 'image', required: false }
];
const motionEntries = (extra = []) => [
  { type: 'image', value: '/p/face.png', slot: 1 },
  { type: 'video', value: '/p/move.mp4', slot: 2 },
  ...extra
];
const FAL_PRO = { provider: 'fal', model: 'fal-ai/kling-video/v3/pro/motion-control' };

test('templates list: motion_transfer is served, object_replacement is unavailable with its routes named', async () => {
  const list = await call('studio.recipes.templates');
  const motion = list.templates.find((t) => t.template === 'motion_transfer');
  const swap = list.templates.find((t) => t.template === 'object_replacement');
  assert.equal(motion.served, true);
  assert.equal(swap.served, false);
  assert.ok(swap.routes.every((r) => r.served === false));
  assert.equal(list.batches.character_reference_batch.max_items, 24);
  assert.equal(motion.moderation.generic[0].generic, true);
  assert.equal(motion.moderation.notes[0].generic, false);
});

test('compose: an unserved template refuses with route_not_served naming every route it needs', async () => {
  const r = await call('studio.recipes.template_compose', {
    template: 'object_replacement',
    slots: [
      { type: 'video', value: '/p/source.mp4', slot: 1 },
      { type: 'image', value: '/p/item.png', slot: 2 }
    ]
  });
  assert.equal(r.ok, false);
  assert.equal(r.code, 'route_not_served');
  assert.equal(r.missing_routes.length, 3);
});

test('compose: a slot out of order is refused with slot_order against its position', async () => {
  const r = await call('studio.recipes.template_compose', {
    template: 'motion_transfer',
    slots: [
      { type: 'image', value: '/p/face.png', slot: 2 },
      { type: 'video', value: '/p/move.mp4', slot: 2 }
    ]
  });
  assert.equal(r.ok, false);
  assert.equal(r.violations[0].code, 'slot_order');
  assert.equal(r.violations[0].position, 1);
});

test('compose: a served route composes the exact request with its mapped params and an estimate', async () => {
  const r = await call('studio.recipes.template_compose', { template: 'motion_transfer', slots: motionEntries(), ...FAL_PRO });
  assert.equal(r.ok, true);
  assert.equal(r.request.provider, 'fal');
  assert.equal(r.request.params.image_url, '/p/face.png');
  assert.equal(r.request.params.video_url, '/p/move.mp4');
  assert.match(r.request.prompt, /Keep the subject's face/);
  assert.equal(typeof r.est_usd, 'number');
  assert.equal(r.moderation.generic[0].generic, true);
});

test('compose: a route with one reference input refuses the second reference slot as slot_unservable, never dropped', async () => {
  const wan = { provider: 'wavespeed', model: 'wavespeed-ai/wan-2.2/animate-2' };
  const r = await call('studio.recipes.template_compose', {
    template: 'motion_transfer',
    slots: motionEntries([{ type: 'image', value: '/p/close.png', slot: 3 }]),
    ...wan
  });
  assert.equal(r.ok, false);
  assert.equal(r.code, 'slot_unservable');
  assert.equal(r.violations[0].position, 3);
});

test('preflight staged on the staged-capable route: draft at the lowest value, final waits for its approval', async () => {
  const pre = await call('studio.recipes.template_preflight', {
    template: 'motion_transfer',
    slots: motionEntries(),
    ...FAL_PRO,
    params: { resolution: '1080p' },
    staged: true
  });
  assert.equal(pre.ok, true);
  assert.equal(pre.drafts.length, 2);
  const [draft, final] = pre.drafts;
  assert.equal(draft.stage, 'draft');
  assert.equal(draft.request.params.resolution, '720p');
  assert.equal(final.stage, 'final');
  assert.equal(final.requires_approval_of, draft.draft_id);
  assert.equal(final.request.params.resolution, '1080p');
  assert.equal(final.request.prompt, draft.request.prompt);

  await assert.rejects(
    mockRecipeTemplateApprove(final.draft_id, final.request_hash),
    (e) => e.code === 'dependency_unapproved'
  );
  const approvedDraft = await mockRecipeTemplateApprove(draft.draft_id, draft.request_hash);
  assert.equal(approvedDraft.draft.status, 'approved');
  const approvedFinal = await mockRecipeTemplateApprove(final.draft_id, final.request_hash);
  assert.equal(approvedFinal.generate.params.resolution, '1080p');
});

test('preflight staged on a route without an enumerated resolution refuses with staged_unavailable', async () => {
  const r = await call('studio.recipes.template_preflight', {
    template: 'motion_transfer',
    slots: motionEntries(),
    provider: 'wavespeed',
    model: 'kwaivgi/kling-v3.0-pro/motion-control',
    params: { resolution: '1080p' },
    staged: true
  });
  assert.equal(r.ok, false);
  assert.equal(r.code, 'staged_unavailable');
});

test('approval refuses a changed draft (draft_changed) and a direct engine approve without the app token', async () => {
  const pre = await call('studio.recipes.template_preflight', { template: 'motion_transfer', slots: motionEntries(), ...FAL_PRO });
  const [draft] = pre.drafts;
  await assert.rejects(mockRecipeTemplateApprove(draft.draft_id, 'not-the-hash'), (e) => e.code === 'draft_changed');
  await assert.rejects(
    call('studio.recipes.template_approve', { draft_id: draft.draft_id }),
    (e) => e.code === 'approval_requires_app'
  );
});

test('character batch: the cap comes from the engine and an over-cap batch is refused', async () => {
  const scenarios = Array.from({ length: 25 }, (_, i) => `scene ${i}`);
  await assert.rejects(
    call('studio.recipes.character_batch', { character_id: 'c1', provider: 'fal', model: 'm', scenarios }),
    (e) => e.code === 'invalid_input' && /at most 24/.test(e.message)
  );
});

test('character batch: a route without character references refuses every item per item', async () => {
  const view = await call('studio.recipes.character_batch', {
    character_id: 'c1',
    provider: 'wavespeed',
    model: 'some/model',
    scenarios: ['a kitchen', 'a cafe']
  });
  assert.equal(view.counts.refused, 2);
  assert.equal(view.items[0].refusal.code, 'reference_route_unsupported');
  assert.equal(view.items[0].draft_id, null);
});

test('character batch: approve needs an https reference; local-only items are refused identity_not_attached', async () => {
  const local = await call('studio.recipes.character_batch', {
    character_id: 'c1',
    provider: 'fal',
    model: 'm',
    scenarios: ['a kitchen']
  });
  const [item] = local.items;
  assert.equal(item.approval_challenge.legacy_hash.length > 0, true);
  await assert.rejects(
    mockRecipeTemplateApprove(item.draft_id, item.approval_challenge.legacy_hash),
    (e) => e.code === 'identity_not_attached'
  );
});

test('character batch: https references make the item approvable; reject is final; state reads back from the engine', async () => {
  const view = await call('studio.recipes.character_batch', {
    character_id: 'c1',
    provider: 'fal',
    model: 'm',
    scenarios: ['a kitchen', 'a cafe'],
    references: ['https://cdn.example/face.png']
  });
  assert.equal(view.counts.pending, 2);
  const [first, second] = view.items;
  const approved = await mockRecipeTemplateApprove(first.draft_id, first.approval_challenge.legacy_hash);
  assert.equal(approved.draft.status, 'approved');

  const rejected = await call('studio.recipes.batch_review', {
    batch_id: view.batch_id,
    decisions: [{ item: second.index, decision: 'reject' }]
  });
  assert.equal(rejected.counts.approved, 1);
  assert.equal(rejected.counts.rejected, 1);
  assert.equal(rejected.items[1].state, 'rejected');

  await assert.rejects(
    call('studio.recipes.batch_review', { batch_id: view.batch_id, decisions: [{ item: second.index, decision: 'approve' }] }),
    (e) => e.code === 'decision_final' || e.code === 'approval_requires_app'
  );

  const readBack = await call('studio.recipes.batch_review', { batch_id: view.batch_id });
  assert.deepEqual(readBack.counts, rejected.counts);
});

test('approval challenge for a draft returns its request hash for the view to show', async () => {
  const pre = await call('studio.recipes.template_preflight', { template: 'motion_transfer', slots: motionEntries(), ...FAL_PRO });
  const [draft] = pre.drafts;
  const challenge = await call('studio.approvals.challenge', { kind: 'recipe_templates', draft_id: draft.draft_id });
  assert.equal(challenge.legacy_hash, draft.request_hash);
});

test('the slot check and the engine agree on a complete two-slot motion entry', async () => {
  const check = checkSlotFill(motionSlots, ['/p/face.png', '/p/move.mp4', '', '']);
  assert.equal(check.ready, true);
  const r = await call('studio.recipes.template_compose', { template: 'motion_transfer', slots: check.entries, ...FAL_PRO });
  assert.equal(r.ok, true);
});
