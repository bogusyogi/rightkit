// Browser-mode (no Tauri) stand-in for studio.video_templates.*. The lists come
// from the same packaged JSON the engine embeds; preflight and approve only
// imitate the engine's shapes so the panel can be exercised in QA.
import type { Draft, Recipe, RecipeLibrary } from './video-quality';

type Json = Record<string, any>;

const DATA = '../../../crates/genright-studio/data/video_templates/';
const loaders: Record<string, () => Promise<{ default: Json }>> = {
  camera_moves: () => import('../../studio-data/video_templates/camera_moves.json'),
  ad_patterns: () => import('../../studio-data/video_templates/ad_patterns.json'),
  aesthetics: () => import('../../studio-data/video_templates/aesthetics.json'),
  audio: () => import('../../studio-data/video_templates/audio.json'),
  transitions: () => import('../../studio-data/video_templates/transitions.json'),
  text_overlays: () => import('../../studio-data/video_templates/text_overlays.json'),
  qa_rubrics: () => import('../../studio-data/video_templates/qa_rubrics.json'),
  subject_motion: () => import('../../studio-data/video_templates/subject_motion.json'),
  scene_motion: () => import('../../studio-data/video_templates/scene_motion.json'),
  continuity: () => import('../../studio-data/video_templates/continuity.json')
};
void DATA;

function item(slug: string, it: Json, extras: Json = {}): Recipe {
  return {
    ...it,
    ...extras,
    slug,
    name: it.name || slug,
    prompt_fragment: it.prompt_fragment || it.fragment || '',
    description: it.description || it.use || ''
  } as Recipe;
}

async function library(id: string): Promise<RecipeLibrary> {
  const load = loaders[id];
  if (!load) throw new Error(`Unsupported video recipe library "${id}"`);
  const json = (await load()).default;
  const items: Recipe[] = [];
  const categories: NonNullable<RecipeLibrary['categories']> = [];
  if (json.categories) {
    for (const [ck, group] of Object.entries<Json>(json.categories)) {
      const label = group._label || ck;
      categories.push({ key: ck, label, use_when: group._use_when ?? null, count: Object.keys(group.items || {}).length });
      for (const [ik, it] of Object.entries<Json>(group.items || {})) items.push(item(ik, it, { category: ck, category_label: label }));
    }
  } else {
    for (const [ik, it] of Object.entries<Json>(json.items || {})) items.push(item(ik, it, it.kind ? { namespace: it.kind } : {}));
  }
  return { library: id, count: items.length, principle: json._meta?.principle ?? null, categories, brand_defaults: json._brand_defaults ?? null, items };
}

async function builtinTemplates(venture: string) {
  const json = (await import('../../studio-data/video_templates/shot_contract_templates.json')).default as Json;
  return (json.templates as Json[]).filter((t) => t.ventures.includes('*') || t.ventures.includes(venture));
}

const drafts = new Map<string, Draft>();
let seq = 0;

export async function mockVideoQuality(verb: string, input: Record<string, unknown>): Promise<unknown> {
  switch (verb) {
    case 'templates': {
      const venture = String(input.venture || '').toLowerCase();
      if (!['dd', 'rh', 'ts', 'hr', 'kdp', 'dv', 'na'].includes(venture)) throw new Error(`Invalid video venture "${String(input.venture ?? '')}"`);
      const templates = (await builtinTemplates(venture)).map((t) => ({ slug: t.slug, name: t.name, path: null, summary: t.summary, source: 'builtin' as const }));
      return { venture, templates, skipped: [] };
    }
    case 'camera_moves': {
      const lib = await library('camera_moves');
      return { ...lib, moves: lib.items };
    }
    case 'ad_patterns': {
      const lib = await library('ad_patterns');
      return { ...lib, patterns: lib.items };
    }
    case 'recipes':
      return library(String(input.library || ''));
    case 'preflight': {
      for (const k of ['prompt', 'brand', 'project_slug', 'provider', 'model', 'aspect', 'duration']) {
        if (input[k] === undefined || input[k] === '') throw new Error(`invalid_input: ${k} is required`);
      }
      if (!input.shot_contract_template) throw new Error('invalid_input: Either shot_contract_template or shot_contract_inline is required');
      if (!input.start_frame_url) throw new Error('invalid_input: start_frame_url is required');
      const model = String(input.model);
      const needsEnd = /kling|seedance|flf|first-last/.test(model);
      if (needsEnd && !input.end_frame_url) throw new Error(`invalid_input: end_frame_url is required for ${model} (rule 1: storyboard start AND end frames)`);
      const brand = String(input.brand).toUpperCase();
      const tpl = (await builtinTemplates(brand.toLowerCase())).find((t) => t.slug === input.shot_contract_template);
      if (!tpl) throw new Error(`not_found: shot_contract_template "${String(input.shot_contract_template)}" not found for venture "${brand.toLowerCase()}"`);
      const c = tpl.shot_contract as Json;
      const moves = await library('camera_moves');
      const move = moves.items.find((m) => m.slug === (input.camera_move || 'eye_level_static'));
      const contractPrompt = [
        `Animate the source frame. ${c.primary_action}.`,
        `Source frame already shows: ${c.source_frame_read}`,
        `Device role: ${c.device_role}. Motion budget: ${c.motion_budget}.`,
        ...(c.time_segments as Json[]).map((s) => `From ${s.start_s}-${s.end_s}s: camera ${s.camera} Action: ${s.action} Forbidden: ${s.forbidden_zone}`),
        `Lighting: ${c.lighting_lock}`,
        `Negative: ${(c.negative_prompt as string[]).join(', ')}.`
      ].join('\n');
      const composed = `A shot of ${c.subject_focus}. The camera: ${(move?.prompt_fragment || '').replace('[SUBJECT]', String(c.subject_focus))}. Action: ${c.primary_action} over ${input.duration} seconds.`;
      const params: Record<string, unknown> = { image_url: input.start_frame_url, aspect: input.aspect, duration: input.duration };
      if (input.end_frame_url) params.end_image_url = input.end_frame_url;
      if (input.resolution) params.resolution = input.resolution;
      seq += 1;
      const draft: Draft = {
        draft_id: `video-20261006000000-${String(input.project_slug)}-${seq.toString(16).padStart(8, '0')}`,
        status: 'draft',
        estimated_cost_usd: Number((0.112 * Number(input.duration)).toFixed(4)),
        composed_prompt: composed,
        contract_prompt: contractPrompt,
        warnings: needsEnd ? [] : [`${model} takes no end frame; single-frame image-to-video drifts (rule 1). Prefer a first/last-frame model for anchored shots.`],
        route: { model, family: /kling/.test(model) ? 'kling' : /seedance/.test(model) ? 'seedance' : 'veo', ref_kind: needsEnd ? 'flf' : 'motion_control', end_frame: needsEnd ? 'required' : 'unsupported' },
        request: { composer_meta: { defaults_used: input.aesthetic ? {} : { aesthetic: { key: 'documentary', source: 'fallback' } } } },
        engine_request: { kind: 'video', provider: String(input.provider), model, prompt: contractPrompt, params },
        approval: { required: true, approved: false, approved_at: null },
        fingerprint: `qa-${seq.toString(16).padStart(8, '0')}`,
        browser_mock: true
      };
      drafts.set(draft.draft_id, draft);
      return draft;
    }
    // The engine command approves nothing without an app-minted token (`operator_approved` is ignored).
    case 'approve':
      throw new Error('approval_requires_app: Approval is a user action in the App app. Approve this draft in the app; an agent, the plain engine CLI or `operator_approved: true` cannot approve it.');
    case 'approve_draft': {
      const draft = drafts.get(String(input.draft_id));
      if (!draft) throw new Error(`not_found: video draft not found: ${String(input.draft_id)}`);
      if (draft.fingerprint !== input.displayed_hash) throw new Error('draft_changed: The draft is not the one that was shown; preflight again.');
      const approved: Draft = {
        ...draft,
        status: 'approved',
        approval: { required: true, approved: true, approved_at: new Date().toISOString(), approved_by: 'qa-mock', channel: 'app' }
      };
      drafts.set(approved.draft_id, approved);
      return approved;
    }
    default:
      throw new Error(`Unsupported browser QA mock: studio.video_templates.${verb}`);
  }
}
