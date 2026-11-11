import { errorCode, errorDetails, errorText } from '../../lib/engine-error.js';
import { isUnconfirmedPrice } from '../../lib/gen-params.js';

// Logic for the character/LoRA panel: character catalog, references, aesthetic
// LoRAs, the generation lock (studio.characters.lock) and the gated fal training
// flow (train_preflight → train with approval_token → train_status).
// Kept free of React so it can be unit-loaded directly.

export interface CharacterSummary {
  id: string;
  character_id: string;
  venture: string;
  project_slug: string;
  brand: string;
  description: string;
  has_lora: boolean;
  lora_base_model: string | null;
  lora_scale: number;
  refs: string[];
  refs_count: number;
  training_photos: number;
  identifiable: boolean;
  has_consent: boolean;
}

export interface CharacterList {
  characters: CharacterSummary[];
  root: string;
  diagnostics: Array<{ severity: string; message: string }>;
}

export interface AestheticLora {
  id: string;
  name?: string;
  base_model: string;
  license?: string;
  commercial_ok?: boolean;
  vetting_status?: string;
  default_scale?: number;
  status?: string;
  usable?: boolean;
  reason?: string | null;
}

export interface LorasResult {
  aesthetic_loras: AestheticLora[];
  diagnostics: unknown[];
}

export interface ReferenceItem {
  path: string;
  label: string;
  source: string;
  character_id: string | null;
}

/** A studio table disagreeing with the engine about one route (`route.inconsistencies`). */
export interface RouteInconsistency {
  code?: string;
  route?: string;
  message: string;
}

export interface LockResult {
  mode: 'lora' | 'reference' | 'none';
  kind: string | null;
  /** The route the lock resolved to (`provider/model`). */
  model: string;
  /** The route the caller asked for (`provider/model`). Differs from `model` when the lock switched routes. */
  requested_model?: string;
  character: CharacterSummary | null;
  aesthetic_lora: AestheticLora | null;
  loras: Array<{ path: string; scale: number; base_model: string }>;
  references: string[];
  pending_upload: string[];
  params: Record<string, unknown>;
  diagnostics: Array<{ severity: string; message: string }>;
  /** The engine's word on the resolved route: whether its price has met a live run yet. */
  route?: {
    id?: string;
    served?: boolean;
    reconciled?: boolean | null;
    reconciled_notice?: string | null;
    inconsistencies?: RouteInconsistency[];
  };
  reconciled?: boolean | null;
  ready?: boolean;
}

/** A lock refusal in plain words. `code` and `technical` only sit in a Details disclosure. */
export interface LockRefusal {
  code: string | null;
  sentence: string;
  technical: string;
}

export interface Trainer {
  id: string;
  label: string;
  endpoint: string;
  base_model: string;
  usd_per_step: number;
  price_note?: string;
}

export interface TrainersResult {
  trainers: Trainer[];
  default_trainer: string;
  default_cap_usd: number;
  default_steps: number;
  photo_range: [number, number];
}

export interface TrainPlan {
  character_id: string;
  trainer: { id: string; label: string; endpoint: string; usd_per_step: number; price_note?: string };
  base_model: string;
  steps: number;
  trigger_word: string;
  predicted_cost_usd: number;
  cap_usd: number;
  exceeds_cap: boolean;
  dataset: { path: string; photo_count: number; caption_count: number; hash: string; extreme: boolean };
  daily_cap_ok: boolean;
  daily_cap_note?: unknown;
  blockers?: string[];
  /** Identifies this exact plan (character, trainer, steps, dataset, cost, cap). It approves nothing. */
  plan_id: string;
  /** The single-use challenge the app mints its approval token for; null while the plan has blockers. */
  approval_challenge?: { nonce: string; expires_unix: number } | null;
}

/** A failed training's error is the engine's `{code, message}`; older records may hold a plain string. */
export type TrainError = string | { code?: string; message?: string } | null;

export interface TrainStatus {
  training_id: string;
  status: string;
  character_id: string;
  predicted_cost_usd?: number;
  lora_url?: string | null;
  error?: TrainError;
  remote?: unknown;
  /** Present on a held training (status `outcome_unknown`). */
  reservation_id?: string;
  reserved_usd?: number;
  held_at?: string;
  held_cause?: { code?: string; message?: string } | null;
  resolve?: string[];
}

/** `train_status` input: a training_id, or a character_id for its latest training. `resolve` needs a training_id. */
export type TrainStatusInput = { training_id?: string; character_id?: string; resolve?: 'release' | 'abandon' };

/** Merge a lock result into the params a Generate run sends. */
export function lockParams(lock: LockResult): Record<string, unknown> {
  return { ...lock.params };
}

/** Readable text for a training's error, whichever shape the engine wrote. */
export function trainErrorText(error: TrainError | undefined): string {
  if (!error) return '';
  if (typeof error === 'string') return error;
  return error.message || error.code || '';
}

// ---------------------------------------------------------------------------
// Lock result in plain words: route switch, reconciled price, inconsistencies.
// ---------------------------------------------------------------------------

/** The route the lock switched to, when it differs from the one asked for; null otherwise. */
export function lockRouteSwitch(lock: LockResult): { requested: string; resolved: string } | null {
  if (!lock.requested_model || lock.requested_model === lock.model) return null;
  return { requested: lock.requested_model, resolved: lock.model };
}

/** "Using <model> because this character has reference images" (or has none), for a switched route. */
export function lockSwitchSentence(lock: LockResult): string | null {
  const sw = lockRouteSwitch(lock);
  if (!sw) return null;
  return `Using ${sw.resolved} because this character ${lock.references.length ? 'has reference images' : 'has no reference images'}`;
}

/** Whether the resolved route's price has met no live run yet. Read from the route, else the lock's own field. */
export function lockPriceUnconfirmed(lock: LockResult): boolean {
  return isUnconfirmedPrice({ reconciled: lock.route?.reconciled ?? lock.reconciled ?? undefined });
}

/** The studio-table disagreements with the engine, for a collapsed Details disclosure. */
export function lockInconsistencies(lock: LockResult): RouteInconsistency[] {
  return (lock.route?.inconsistencies ?? []).filter((i) => typeof i?.message === 'string' && i.message.trim() !== '');
}

/**
 * The lock's diagnostics that the page does not already show as sentences: a notice is replaced by the
 * route-switch and price sentences, and a warning that is also an inconsistency sits in Details instead.
 */
export function lockPrimaryDiagnostics(lock: LockResult): Array<{ severity: string; message: string }> {
  const listed = new Set(lockInconsistencies(lock).map((i) => i.message));
  return lock.diagnostics.filter((d) => d.severity !== 'notice' && !listed.has(d.message));
}

// ---------------------------------------------------------------------------
// Lock refusals in plain words. The engine's refusals carry a code and a message; the message names the
// routes or the maximum in its own words, so those two are read from it when no structured field is sent.
// ---------------------------------------------------------------------------

function routesFromRefusal(message: string, details: Record<string, unknown> | null): string[] {
  const fromDetails = Array.isArray(details?.routes) ? details.routes.filter((r): r is string => typeof r === 'string') : [];
  if (fromDetails.length) return fromDetails;
  const tail = /the engine lists (.+?)\.?$/.exec(message.trim());
  return tail ? tail[1].split(',').map((r) => r.trim()).filter(Boolean) : [];
}

function maxFromRefusal(message: string, details: Record<string, unknown> | null): number | null {
  if (typeof details?.max === 'number' && Number.isFinite(details.max)) return details.max;
  const m = /takes at most (\d+)/.exec(message);
  return m ? Number(m[1]) : null;
}

/** The plain sentence for a lock refusal, with the code and engine text kept for a Details disclosure. */
export function lockRefusal(error: unknown): LockRefusal {
  const code = errorCode(error);
  const details = errorDetails(error);
  const technical = errorText(error);
  const message = technical;
  let sentence: string;
  switch (code) {
    case 'lora_not_hosted':
      sentence = "This character's LoRA file is on this computer. The provider can only read a LoRA from a web address that starts with https, and the app cannot upload LoRA files yet. Host the file at an https address the provider can fetch, such as your own fal storage, put that address in the character's LoRA details, then lock again.";
      break;
    case 'reference_route_unsupported': {
      const routes = routesFromRefusal(message, details);
      sentence = `This route does not take reference images, and this character has no trained LoRA, so its references cannot hold its identity here. ${routes.length ? `Use one of these routes instead: ${routes.join(', ')}.` : 'Use a route that accepts reference images.'}`;
      break;
    }
    case 'too_many_references': {
      const max = maxFromRefusal(message, details);
      sentence = max === null
        ? 'This route takes fewer reference images than this character has. Remove some references, or pick a route that takes more.'
        : `This route takes at most ${max} reference image${max === 1 ? '' : 's'}. Remove some references from this character, or pick a route that takes more.`;
      break;
    }
    case 'lora_unsupported_model':
      sentence = "This model does not take LoRA weights, so this character's LoRA cannot be used on it. Pick a model that takes LoRA weights.";
      break;
    default:
      sentence = technical;
  }
  return { code, sentence, technical };
}

// ---------------------------------------------------------------------------
// Engine calls with a browser-QA fallback fixture.
// ---------------------------------------------------------------------------

async function call<T>(verb: string, input: Record<string, unknown> = {}): Promise<T> {
  const { studio } = await import('../../lib/api.js');
  return await studio<T>(`characters.${verb}`, input);
}

export const characters = {
  list: () => call<CharacterList>('list'),
  create: (input: Record<string, unknown>) => call<{ character: CharacterSummary }>('create', input),
  addImage: (input: Record<string, unknown>) => call<{ path: string; kind: string }>('add_image', input),
  references: (characterId?: string) => call<{ references: ReferenceItem[] }>('references', characterId ? { character_id: characterId } : {}),
  loras: (brand?: string) => call<LorasResult>('loras', brand ? { brand } : {}),
  baseLock: (provider?: string, model?: string) => call<{ base_model?: string | null; models?: unknown[]; base_models?: string[] }>('base_lock', provider && model ? { provider, model } : {}),
  lock: (input: Record<string, unknown>) => call<LockResult>('lock', input),
  trainers: () => call<TrainersResult>('trainers'),
  trainPreflight: (input: Record<string, unknown>) => call<TrainPlan>('train_preflight', input),
  /**
   * Start a training as the user: the app shell mints the approval token (`approve_draft`), so no
   * engine caller can spend on its own. `plan` is the preflight the user was shown; `input` is the
   * request it was made from.
   */
  train: async (plan: TrainPlan, input: Record<string, unknown>): Promise<TrainStatus> => {
    const { approveDraft } = await import('../../lib/api.js');
    return await approveDraft<TrainStatus>('character_training', plan.plan_id, plan.plan_id, input);
  },
  trainStatus: (input: TrainStatusInput) => call<TrainStatus>('train_status', { ...input })
};
