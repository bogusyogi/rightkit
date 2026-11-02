import { errorMessage } from '../../lib/engine-error.js';

// Logic for the image quality panel: intents, contract, recipe picks and the
// guarded preflight → approve flow (studio.recipes.*). Kept free of React.

export interface IntentInfo {
  model: string;
  label: string;
  use: string;
  estimated_cost: string;
}

export interface IntentsResult {
  intents: Record<string, IntentInfo>;
  reference_limit: number;
  brands: string[];
}

export interface LibraryItem {
  key: string;
  name?: string | null;
  use?: string | null;
  fragment?: string | null;
}

export interface Library {
  id: string;
  slot: string;
  subcategory?: string | null;
  principle?: string | null;
  brand_defaults?: Record<string, string | null> | null;
  items: LibraryItem[];
}

export interface LibrariesResult {
  libraries: Library[];
  models: Array<{ slug: string; backend?: string | null; has_contract?: boolean; reference_max?: number | null }>;
}

export interface ImageDraft {
  ok?: boolean;
  draft_id: string;
  status: string;
  request: { contract?: string; intent?: string; brand?: string };
  rendered?: { prompt?: string; params?: Record<string, unknown>; engine_params?: Record<string, unknown>; pipeline?: string };
  cost?: { est_usd: number | null; per_image_usd: number | null; count: number; source: string };
  warnings: string[];
  request_hash?: string;
  /** Single-use approval challenge from preflight; the app shell uses it, the UI never does. */
  approval_challenge?: { nonce: string; input_hash: string; legacy_hash: string; expires_unix: number };
  manifest_path?: string;
  // Error payloads from the composer/contract layers come back as a body, not a throw.
  message?: string;
  violations?: Array<{ rule?: string; detail?: string }>;
}

export interface ApproveResult {
  ok: boolean;
  draft: ImageDraft;
  generate: { kind: string; prompt: string; params: Record<string, unknown>; contract: string };
}

export const RECIPE_SLOTS = ['aesthetic', 'composition', 'lens', 'lighting', 'depth', 'color', 'skin'] as const;
export type RecipeSlot = (typeof RECIPE_SLOTS)[number];

export interface QualityForm {
  brand: string;
  intent: string;
  contract: string;
  aspect: string;
  projectSlug: string;
  recipes: Partial<Record<RecipeSlot, string>>;
  seed: string;
  steps: string;
  guidance: string;
  negative: string;
}

function num(v: string): number | undefined {
  const t = v.trim();
  if (!t) return undefined;
  const n = Number(t);
  return Number.isFinite(n) ? n : undefined;
}

/** Build the `studio.recipes.preflight` input from the panel + Generate prompt. */
export function buildPreflightInput(f: QualityForm, prompt: string): Record<string, unknown> {
  const input: Record<string, unknown> = {
    prompt,
    intent: f.intent || 'mid',
    brand: f.brand || 'NA',
    aspect: f.aspect || '1:1',
    project_slug: f.projectSlug
  };
  if (f.contract) input.contract = f.contract;
  const recipes: Record<string, unknown> = {};
  for (const slot of RECIPE_SLOTS) {
    const v = f.recipes[slot];
    if (v) recipes[slot] = v;
  }
  if (Object.keys(recipes).length) input.recipes = recipes;
  const params: Record<string, unknown> = {};
  const seed = num(f.seed);
  const steps = num(f.steps);
  const guidance = num(f.guidance);
  if (seed !== undefined) params.seed = seed;
  if (steps !== undefined) params.num_inference_steps = steps;
  if (guidance !== undefined) params.guidance_scale = guidance;
  if (f.negative.trim()) params.negative_prompt = f.negative.trim();
  if (Object.keys(params).length) input.generation_params = params;
  return input;
}

export interface ImageGuardInput {
  enabled: boolean;
  draft: ImageDraft | null;
  approved: ApproveResult | null;
  prompt: string;
}

/** Why Generate stays blocked, or null when an approved draft is what would be sent. */
export function guardReason(g: ImageGuardInput): string | null {
  if (!g.enabled) return null;
  if (!g.approved) return g.draft ? 'Approve the preflight to unlock Generate.' : 'Image quality gate is on: run the preflight and approve it before generating.';
  if (g.prompt !== g.approved.generate.prompt) return 'The prompt changed after approval. Run the preflight again.';
  return null;
}

// ---------------------------------------------------------------------------
// Engine calls. In a browser (no Tauri) the shared mock does not know
// studio.recipes.*, so a bundled fixture answers instead.
// ---------------------------------------------------------------------------

async function call<T>(verb: string, input: Record<string, unknown> = {}): Promise<T> {
  try {
    const { studio } = await import('../../lib/api.js');
    return await studio<T>(verb, input);
  } catch (error) {
    if (import.meta.env?.DEV && /Unsupported browser QA mock/i.test(errorMessage(error))) {
      const { mockImageQuality } = await import('./image-quality-mock.js');
      return (await mockImageQuality(verb, input)) as T;
    }
    throw error;
  }
}

export const imageQuality = {
  intents: () => call<IntentsResult>('catalog.intents'),
  libraries: () => call<LibrariesResult>('recipes.libraries'),
  contractFor: (model: string) => call<{ model: string; contract: string | null }>('catalog.contract_for', { model }),
  preflight: (input: Record<string, unknown>) => call<ImageDraft>('recipes.preflight', input),
  draft: (draftId: string) => call<ImageDraft>('recipes.draft', { draft_id: draftId }),
  /**
   * The user's approval, from the approve button only: the app shell mints the engine's approval
   * token (`approve_draft`). `displayedHash` is the draft's `request_hash` as shown.
   */
  approve: async (draftId: string, displayedHash: string): Promise<ApproveResult> => {
    try {
      const { approveDraft } = await import('../../lib/api.js');
      return await approveDraft<ApproveResult>('recipes', draftId, displayedHash);
    } catch (error) {
      if (import.meta.env?.DEV && /Unsupported browser QA mock/i.test(errorMessage(error))) {
        const { mockImageQuality } = await import('./image-quality-mock.js');
        return (await mockImageQuality('approve_draft', { draft_id: draftId, displayed_hash: displayedHash })) as ApproveResult;
      }
      throw error;
    }
  }
};
