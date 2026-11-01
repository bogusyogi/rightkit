// Browser-QA stand-in for the studio.catalog/recipes engine commands. Mirrors
// crates/genright-studio/data shapes; never sends anything anywhere.
import type { ApproveResult, ImageDraft, IntentsResult, LibrariesResult } from './image-quality';

const INTENTS: IntentsResult = {
  reference_limit: 14,
  brands: ['DD', 'RH', 'TS', 'HR', 'DV', 'KDP', 'NA'],
  intents: {
    bottom: { model: 'flux-2-dev', label: 'Bottom', use: 'Drafts, blog heroes, b-roll, lifestyle imagery without text', estimated_cost: '$0.012/MP' },
    mid: { model: 'flux-2-pro', label: 'Mid', use: 'Sharper photoreal, multi-element scenes, product shots', estimated_cost: '$0.025/MP' },
    premium: { model: 'nano-banana-pro', label: 'Premium', use: 'Text on image, photoreal humans, brand UI, ads with copy', estimated_cost: '$0.04-$0.09/image' },
    cartoon: { model: 'qwen-image', label: 'Cartoon', use: 'Soft painterly cartoon scenes and illustrated mood pieces', estimated_cost: '$0.02/image' }
  }
};

const LIBRARIES: LibrariesResult = {
  libraries: [
    { id: 'aesthetics', slot: 'aesthetic', subcategory: 'Aesthetics', principle: 'Lead the prompt with the look.', items: [
      { key: 'editorial-studio', name: 'Editorial studio', use: 'clean editorial studio photograph' },
      { key: 'commercial-hero', name: 'Commercial hero', use: 'polished commercial product photograph' }
    ] },
    { id: 'composition', slot: 'composition', subcategory: 'Composition', principle: 'Where the subject sits.', items: [
      { key: 'rule-of-thirds', name: 'Rule of thirds', use: 'subject on the left third' },
      { key: 'center-hero', name: 'Center hero', use: 'centered hero framing' }
    ] },
    { id: 'lighting', slot: 'lighting', subcategory: 'Lighting', principle: 'Light quality and direction.', items: [
      { key: 'soft-window', name: 'Soft window', use: 'soft window light' },
      { key: 'dramatic-rim', name: 'Dramatic rim', use: 'dramatic rim lighting' }
    ] }
  ],
  models: [
    { slug: 'flux-2-dev', backend: 'fal', has_contract: true, reference_max: 4 },
    { slug: 'flux-2-pro', backend: 'fal', has_contract: true, reference_max: 4 },
    { slug: 'nano-banana-pro', backend: 'nanobanana', has_contract: true, reference_max: 8 },
    { slug: 'qwen-image', backend: 'local', has_contract: true, reference_max: 2 }
  ]
};

const drafts = new Map<string, ImageDraft>();

function makeDraft(input: Record<string, unknown>): ImageDraft {
  const recipes = (input.recipes ?? {}) as Record<string, string>;
  const params = (input.generation_params ?? {}) as Record<string, unknown>;
  const contract = String(input.contract || INTENTS.intents[String(input.intent || 'mid')]?.model || 'flux-2-pro');
  const picks = RECIPE_KEYS.map((k) => recipes[k]).filter(Boolean);
  const prompt = [
    String(input.prompt || '').trim() || 'studio subject',
    picks.length ? `styled with ${picks.join(', ')}` : '',
    `aspect ${String(input.aspect || '1:1')}`
  ].filter(Boolean).join(', ');
  const id = `image-qa-${String(drafts.size + 1).padStart(4, '0')}`;
  const draft: ImageDraft = {
    ok: true,
    draft_id: id,
    status: 'draft',
    request: { contract, intent: String(input.intent || 'mid'), brand: String(input.brand || 'NA') },
    rendered: {
      prompt,
      params,
      engine_params: params,
      pipeline: picks.length ? 'recipe-composer' : 'raw-prompt'
    },
    cost: { est_usd: 0.025, per_image_usd: 0.025, count: 1, source: 'contract_invoice' },
    warnings: [],
    request_hash: `qa-${id}`
  };
  return draft;
}

const RECIPE_KEYS = ['aesthetic', 'composition', 'lens', 'lighting', 'depth', 'color', 'skin'];

export async function mockImageQuality(verb: string, input: Record<string, unknown>): Promise<unknown> {
  switch (verb) {
    case 'catalog.intents':
      return INTENTS;
    case 'recipes.libraries':
      return LIBRARIES;
    case 'catalog.contract_for':
      return { model: input.model, contract: LIBRARIES.models.find((m) => m.slug === input.model)?.slug ?? null };
    case 'recipes.preflight': {
      if (!String(input.prompt ?? '').trim()) throw new Error('invalid_input: Prompt is required for Image Studio preflight');
      const d = makeDraft(input);
      drafts.set(d.draft_id, d);
      return { ...d, browser_mock: true };
    }
    case 'recipes.draft': {
      const d = drafts.get(String(input.draft_id));
      if (!d) throw new Error(`draft_not_found: Image run draft not found: ${String(input.draft_id)}`);
      return { ...d, matches_hash: true };
    }
    // The engine command approves nothing without an app-minted token (`operator_approved` is ignored).
    case 'recipes.approve':
      throw new Error('approval_requires_app: Approval is a user action in the App app. Approve this draft in the app; an agent, the plain engine CLI or `operator_approved: true` cannot approve it.');
    case 'approve_draft': {
      const d = drafts.get(String(input.draft_id));
      if (!d) throw new Error(`draft_not_found: Image run draft not found: ${String(input.draft_id)}`);
      if (d.request_hash !== input.displayed_hash) throw new Error('draft_changed: The draft is not the one that was shown; preflight again.');
      const approved: ImageDraft = { ...d, status: 'approved' };
      drafts.set(d.draft_id, approved);
      const result: ApproveResult = {
        ok: true,
        draft: approved,
        generate: {
          kind: 'image',
          prompt: approved.rendered?.prompt ?? '',
          params: approved.rendered?.engine_params ?? {},
          contract: String(approved.request.contract)
        }
      };
      return result;
    }
    default:
      throw new Error(`mockImageQuality: unknown verb ${verb}`);
  }
}
