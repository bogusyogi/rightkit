import { errorMessage } from '../../lib/engine-error.js';

// Logic for the video shot-quality panel: engine calls (studio.video_templates.*),
// prompt helpers, the preflight request builder and the approval guard.
// Kept free of React so src/tests/video-quality.test.mjs can load it directly.

export const VENTURES = ['DD', 'RH', 'TS', 'HR', 'KDP', 'DV'] as const;
export type Venture = (typeof VENTURES)[number];
export const ASPECTS = ['9:16', '16:9'] as const;

export interface Recipe {
  slug: string;
  name: string;
  prompt_fragment: string;
  description: string;
  category?: string;
  category_label?: string;
  namespace?: string;
  beats?: Array<{ name: string; duration_s: number; what?: string }>;
  ideal_length_s?: number;
  best_for?: string[];
  good_for_anchored_flf?: boolean;
  ugc_friendly?: boolean;
  warning?: string;
}

export interface RecipeLibrary {
  library: string;
  count: number;
  principle?: string | null;
  categories?: Array<{ key: string; label: string; use_when?: string | null; count: number }>;
  brand_defaults?: Record<string, { item_key: string; status: string }> | null;
  items: Recipe[];
}

export interface TemplateInfo {
  slug: string;
  name: string;
  path: string | null;
  summary: string;
  source: 'builtin' | 'content';
}

export interface DraftEngineRequest {
  kind: 'video';
  provider: string;
  model: string;
  prompt: string;
  params: Record<string, unknown>;
}

export interface Draft {
  draft_id: string;
  status: 'draft' | 'approved';
  estimated_cost_usd: number | null;
  composed_prompt: string;
  contract_prompt: string;
  warnings: string[];
  route: { model: string; family: string; ref_kind: string; end_frame: string };
  request: { composer_meta?: { defaults_used?: Record<string, { key: string; source: string }> } };
  engine_request: DraftEngineRequest;
  approval: { required: boolean; approved: boolean; approved_at: string | null; approved_by?: string | null; channel?: string };
  /** The audit hash of the draft's engine request; the approval is bound to what the user saw. */
  fingerprint?: string;
  approval_challenge?: { nonce: string; input_hash: string; legacy_hash: string; expires_unix: number };
  manifest_path?: string;
  browser_mock?: boolean;
}

export const RECIPE_PICKERS = [
  { field: 'aesthetic', library: 'aesthetics', label: 'Aesthetic' },
  { field: 'subject_motion', library: 'subject_motion', label: 'Subject motion' },
  { field: 'scene_motion', library: 'scene_motion', label: 'Scene motion' },
  { field: 'transition', library: 'transitions', label: 'Transition' },
  { field: 'text_overlay', library: 'text_overlays', label: 'Text overlay' },
  { field: 'audio_recipe', library: 'audio', label: 'Audio recipe' },
  { field: 'qa_rubric', library: 'qa_rubrics', label: 'QA rubric' }
] as const;
export type RecipeField = (typeof RECIPE_PICKERS)[number]['field'];

/** Append a recipe fragment to the prompt on its own paragraph (CameraMovePicker.appendPromptFragment). */
export function appendPromptFragment(prompt: string, promptFragment: string): string {
  const base = prompt.trim();
  const fragment = promptFragment.trim();
  if (!fragment) return base;
  return base ? `${base}\n\n${fragment}` : fragment;
}

/** `hook(2s) → problem(3s) → …` for an ad pattern. */
export function beatLine(pattern: Pick<Recipe, 'beats'>): string {
  return (pattern.beats ?? []).map((b) => `${b.name}(${b.duration_s}s)`).join(' → ');
}

export function groupByCategory(items: Recipe[]): Array<{ key: string; label: string; items: Recipe[] }> {
  const groups: Array<{ key: string; label: string; items: Recipe[] }> = [];
  for (const item of items) {
    const key = item.category ?? '';
    let group = groups.find((g) => g.key === key);
    if (!group) {
      group = { key, label: item.category_label ?? (key || 'Other'), items: [] };
      groups.push(group);
    }
    group.items.push(item);
  }
  return groups;
}

export function parseOverrides(text: string): { ok: true; value: Record<string, unknown> | null } | { ok: false; error: string } {
  const trimmed = text.trim();
  if (!trimmed) return { ok: true, value: null };
  try {
    const value: unknown = JSON.parse(trimmed);
    if (!value || typeof value !== 'object' || Array.isArray(value)) return { ok: false, error: 'Overrides must be a JSON object.' };
    return { ok: true, value: value as Record<string, unknown> };
  } catch (e) {
    return { ok: false, error: `Overrides are not valid JSON: ${errorMessage(e)}` };
  }
}

export interface ShotForm {
  venture: Venture;
  projectSlug: string;
  provider: string | null;
  model: string | null;
  prompt: string;
  template: string;
  overrides: Record<string, unknown> | null;
  cameraMove: string;
  adPattern: string;
  recipes: Partial<Record<RecipeField, string>>;
  continuityLocks: string[];
  startFrame: string;
  endFrame: string;
  aspect: string;
  duration: string;
  resolution: string;
  cinemaStudio?: boolean;
}

/** The `studio.video_templates.preflight` input; blank fields are left out, never defaulted. */
export function buildPreflightInput(f: ShotForm): Record<string, unknown> {
  const input: Record<string, unknown> = {
    brand: f.venture,
    project_slug: f.projectSlug,
    provider: f.provider ?? '',
    model: f.model ?? '',
    prompt: f.prompt,
    aspect: f.aspect,
    duration: f.duration.trim() === '' ? undefined : Number(f.duration)
  };
  const put = (key: string, value: string | undefined) => {
    if (value && value.trim()) input[key] = value.trim();
  };
  put('shot_contract_template', f.template);
  put('camera_move', f.cameraMove);
  put('marketing_template', f.adPattern);
  put('resolution', f.resolution);
  put('start_frame_url', f.startFrame);
  put('end_frame_url', f.endFrame);
  for (const [field, value] of Object.entries(f.recipes)) put(field, value);
  if (f.continuityLocks.length) input.continuity_locks = f.continuityLocks;
  if (f.overrides && f.template) input.shot_contract_overrides = f.overrides;
  if (f.cinemaStudio) input.cinema_studio = true;
  return input;
}

export interface GuardInput {
  enabled: boolean;
  draft: Draft | null;
  provider: string | null;
  model: string | null;
  prompt: string;
}

/** Why Generate stays blocked, or null when the approved draft is exactly what would be sent. */
export function guardReason(g: GuardInput): string | null {
  if (!g.enabled) return null;
  if (!g.draft) return 'Shot guard is on: run the preflight and approve it before generating.';
  const sent = g.draft.engine_request;
  if (sent.provider !== g.provider || sent.model !== g.model) return 'The model changed after the preflight. Run the preflight again.';
  if (!g.draft.approval.approved) return 'Approve the preflight to unlock Generate.';
  if (g.prompt !== sent.prompt) return 'The prompt changed after approval. Run the preflight again.';
  return null;
}

/** Params the panel hands to Generate once a draft is approved (panel-owned keys). */
export function approvedParams(draft: Draft): Record<string, unknown> {
  return { ...draft.engine_request.params };
}

export function usd(n: number | null | undefined): string {
  return typeof n === 'number' && Number.isFinite(n) ? `$${n.toFixed(n < 1 ? 3 : 2)}` : 'unknown';
}

// ---------------------------------------------------------------------------
// Engine calls. In a browser (no Tauri) the engine mock does not know
// studio.video_templates.*, so the packaged data answers instead.
// ---------------------------------------------------------------------------

async function call<T>(verb: string, input: Record<string, unknown> = {}): Promise<T> {
  try {
    const { studio } = await import('../../lib/api.js');
    return await studio<T>(`video_templates.${verb}`, input);
  } catch (error) {
    if (import.meta.env?.DEV && /Unsupported browser QA mock/i.test(errorMessage(error))) {
      const { mockVideoQuality } = await import('./video-quality-mock.js');
      return (await mockVideoQuality(verb, input)) as T;
    }
    throw error;
  }
}

export const videoQuality = {
  templates: (venture: Venture) => call<{ venture: string; templates: TemplateInfo[]; skipped: unknown[] }>('templates', { venture: venture.toLowerCase() }),
  cameraMoves: () => call<RecipeLibrary & { moves: Recipe[] }>('camera_moves'),
  adPatterns: () => call<RecipeLibrary & { patterns: Recipe[] }>('ad_patterns'),
  recipes: (library: string) => call<RecipeLibrary>('recipes', { library }),
  preflight: (input: Record<string, unknown>) => call<Draft>('preflight', input),
  /**
   * The user's approval, from the approve button only: the app shell mints the engine's approval
   * token (`approve_draft`). `displayedHash` is the draft's `fingerprint` as shown.
   */
  approve: async (draftId: string, displayedHash: string): Promise<Draft> => {
    try {
      const { approveDraft } = await import('../../lib/api.js');
      return await approveDraft<Draft>('video_templates', draftId, displayedHash);
    } catch (error) {
      if (import.meta.env?.DEV && /Unsupported browser QA mock/i.test(errorMessage(error))) {
        const { mockVideoQuality } = await import('./video-quality-mock.js');
        return (await mockVideoQuality('approve_draft', { draft_id: draftId, displayed_hash: displayedHash })) as Draft;
      }
      throw error;
    }
  }
};
