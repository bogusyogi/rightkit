// Browser-QA stand-in for studio.characters.*. Mirrors the Rust shapes; all
// state is in-memory and nothing leaves the page.
import type { CharacterList, CharacterSummary, LockResult, LorasResult, ReferenceItem, TrainPlan, TrainStatus, TrainersResult } from './character-lock.js';

const iris: CharacterSummary = {
  id: 'dd/iris',
  character_id: 'iris',
  venture: 'dd',
  project_slug: 'dd',
  brand: 'DD',
  description: 'A fictional studio identity with a trained LoRA.',
  has_lora: true,
  lora_base_model: 'FLUX.2-dev',
  lora_scale: 1,
  refs: ['/qa/characters/dd/iris/refs/front.png', '/qa/characters/dd/iris/refs/side.png'],
  refs_count: 2,
  training_photos: 18,
  identifiable: false,
  has_consent: false
};

const pia: CharacterSummary = {
  ...iris,
  id: 'hr/pia',
  character_id: 'pia',
  venture: 'hr',
  brand: 'HR',
  description: 'Presenter portrait, references only (no LoRA yet).',
  has_lora: false,
  lora_base_model: null,
  refs: ['/qa/characters/hr/pia/refs/a.png'],
  refs_count: 1,
  training_photos: 0
};

const chars = new Map<string, CharacterSummary>([[iris.id, iris], [pia.id, pia]]);
const trainings = new Map<string, TrainStatus>();
let seq = 0;

const LORAS: LorasResult = {
  aesthetic_loras: [
    { id: 'product-studio', name: 'Product studio', base_model: 'FLUX.1-dev', license: 'mit', commercial_ok: true, vetting_status: 'vetted', default_scale: 0.8, usable: true },
    { id: 'flux-fashion-noncommercial', name: 'Fashion editorial', base_model: 'FLUX.1-dev', license: 'cc-by-nc-4.0', commercial_ok: false, vetting_status: 'blocked_noncommercial', default_scale: 0.7, usable: false, reason: 'not cleared for commercial brand output' }
  ],
  diagnostics: []
};

const TRAINERS: TrainersResult = {
  trainers: [
    { id: 'flux-lora-fast-training', label: 'FLUX.1 dev LoRA fast training', endpoint: 'fal-ai/flux-lora-fast-training', base_model: 'FLUX.1-dev', usd_per_step: 0.002, price_note: '$2 per 1000-step run, scales linearly with steps' },
    { id: 'flux-2-trainer', label: 'FLUX.2 dev trainer', endpoint: 'fal-ai/flux-2-trainer', base_model: 'FLUX.2-dev', usd_per_step: 0.0064, price_note: '$0.0064 per step' }
  ],
  default_trainer: 'flux-lora-fast-training',
  default_cap_usd: 10,
  default_steps: 1000,
  photo_range: [15, 30]
};

function err(code: string, message: string, details?: Record<string, unknown>): Error {
  // `code` and `details` mirror the engine's error so toError reads them the same way.
  return Object.assign(new Error(`${code}: ${message}`), { code, ...(details ? { details } : {}) });
}

// QA scenario: a training whose submit may have reached fal, held as outcome_unknown until the user resolves it.
const HELD_TRAINING: TrainStatus = {
  training_id: 'train_qa_held',
  status: 'outcome_unknown',
  character_id: 'dd/iris',
  predicted_cost_usd: 2,
  reservation_id: 'train_qa_held_res',
  reserved_usd: 2,
  held_at: '2026-10-10T14:03:00Z',
  held_cause: { code: 'timeout', message: 'no answer from fal within the request window' },
  resolve: ['release', 'abandon']
};
trainings.set(HELD_TRAINING.training_id, HELD_TRAINING);

export async function mockCharacters(verb: string, input: Record<string, unknown>): Promise<unknown> {
  switch (verb) {
    case 'list':
      return { characters: [...chars.values()], root: '/qa/characters', diagnostics: [] } satisfies CharacterList;
    case 'create': {
      const cid = String(input.character_id || '');
      if (!/^[a-z0-9._-]+$/i.test(cid)) throw err('invalid_input', 'character_id must contain only letters, numbers, dots, underscores, or hyphens');
      const venture = String(input.venture || 'na').toLowerCase();
      const id = `${venture}/${cid}`;
      if (chars.has(id)) throw err('exists', `character ${id} already exists`);
      const c: CharacterSummary = {
        id, character_id: cid, venture, project_slug: String(input.project_slug || venture),
        brand: String(input.brand || 'NA'), description: String(input.description || ''),
        has_lora: false, lora_base_model: null, lora_scale: 1, refs: [], refs_count: 0,
        training_photos: 0, identifiable: input.identifiable === true,
        has_consent: input.consent === true
      };
      chars.set(id, c);
      return { character: c };
    }
    case 'add_image':
      return { path: `/qa/characters/${String(input.character_id || '_references')}/img-${seq++}.png`, kind: String(input.kind || 'reference') };
    case 'references': {
      const refs: ReferenceItem[] = [];
      const cid = input.character_id ? String(input.character_id) : null;
      if (cid) {
        const c = chars.get(cid);
        if (!c) throw err('not_found', `unknown character ${cid}`);
        for (const p of c.refs) refs.push({ path: p, label: p.split('/').pop() ?? p, source: 'character', character_id: c.id });
      }
      refs.push({ path: '/qa/characters/_references/shared.png', label: 'shared.png', source: 'shared', character_id: null });
      return { references: refs };
    }
    case 'loras':
      return LORAS;
    case 'base_lock':
      return { base_models: ['FLUX.1-dev', 'FLUX.2-dev', 'Qwen-Image'], models: [] };
    case 'lock': {
      const cid = input.character_id ? String(input.character_id) : null;
      const c = cid ? chars.get(cid) : undefined;
      if (cid && !c) throw err('not_found', `unknown character ${cid}`);
      const loras = c?.has_lora && c.lora_base_model ? [{ path: 'https://cdn.example.test/iris.safetensors', scale: c.lora_scale, base_model: c.lora_base_model }] : [];
      const aesthetic = input.aesthetic_lora_id ? LORAS.aesthetic_loras.find((l) => l.id === input.aesthetic_lora_id) : undefined;
      if (input.aesthetic_lora_id && !aesthetic) throw err('unknown_lora', `Unknown aesthetic LoRA "${String(input.aesthetic_lora_id)}"`);
      if (aesthetic?.vetting_status !== 'vetted' && aesthetic) throw err('lora_not_vetted', `Aesthetic LoRA "${aesthetic.id}" is not vetted for generation use`);
      if (aesthetic) loras.push({ path: `aesthetic/${aesthetic.id}`, scale: aesthetic.default_scale ?? 1, base_model: aesthetic.base_model });
      const refs = c && !c.has_lora ? c.refs : [];
      const result: LockResult = {
        mode: loras.length ? 'lora' : refs.length ? 'reference' : 'none',
        kind: typeof input.kind === 'string' ? input.kind : null,
        model: `${String(input.provider || '')}/${String(input.model || '')}`,
        character: c ?? null,
        aesthetic_lora: aesthetic ?? null,
        loras,
        references: refs,
        pending_upload: refs,
        params: loras.length ? { loras: loras.map((l) => ({ path: l.path, scale: l.scale })) } : {},
        diagnostics: refs.length ? [{ severity: 'warning', message: `${refs.length} local reference(s) need uploading before the provider can read them; call lock with upload:true.` }] : []
      };
      return result;
    }
    case 'trainers':
      return TRAINERS;
    case 'train_preflight': {
      const c = chars.get(String(input.character_id || ''));
      if (!c) throw err('not_found', `unknown character ${String(input.character_id)}`);
      if (c.training_photos === 0) throw err('dataset_size', 'Character LoRA training requires at least one training photo; found 0.');
      const steps = Number(input.steps) || TRAINERS.default_steps;
      const trainer = TRAINERS.trainers.find((t) => t.id === (input.trainer || TRAINERS.default_trainer)) ?? TRAINERS.trainers[0];
      const cost = steps * trainer.usd_per_step;
      const plan: TrainPlan = {
        character_id: c.id,
        trainer,
        base_model: trainer.base_model,
        steps,
        trigger_word: `chr_${c.character_id}`,
        predicted_cost_usd: cost,
        cap_usd: Number(input.cap_usd) || TRAINERS.default_cap_usd,
        exceeds_cap: cost > (Number(input.cap_usd) || TRAINERS.default_cap_usd),
        dataset: { path: `/qa/characters/${c.id}/training-set`, photo_count: c.training_photos, caption_count: 0, hash: 'qa-hash', extreme: c.training_photos < TRAINERS.photo_range[0] },
        daily_cap_ok: true,
        blockers: ['No fal API key is configured.'],
        plan_id: `ap1_qa${String(++seq).padStart(4, '0')}`,
        approval_challenge: null
      };
      return plan;
    }
    // The engine command refuses a caller without an app-minted token; the browser-mode approve_draft stands in for the app.
    case 'train':
      throw err('approval_requires_app', 'Approval is a user action in the App app. Approve this training in the app; an agent, the plain engine CLI or `operator_approved: true` cannot approve it.');
    case 'train_approved': {
      const t: TrainStatus = { training_id: `train_qa${++seq}`, status: 'submitted', character_id: String(input.character_id || ''), predicted_cost_usd: 2 };
      trainings.set(t.training_id, t);
      return t;
    }
    case 'train_status': {
      if (input.resolve && !input.training_id) throw err('invalid_input', 'resolve needs a training_id');
      const t = input.training_id
        ? trainings.get(String(input.training_id))
        : [...trainings.values()].filter((r) => r.character_id === String(input.character_id || '')).pop();
      if (!t) throw err('training_not_found', 'unknown training');
      if (t.status === 'failed') return t;
      if (t.status === 'outcome_unknown') {
        // Mirrors the engine: a held record is returned as is until the user resolves it, then it ends as failed.
        if (!input.resolve) return { ...t };
        if (input.resolve !== 'release' && input.resolve !== 'abandon') throw err('invalid_input', 'resolve must be "release" or "abandon"');
        const amount = t.reserved_usd ?? 0;
        const resolved: TrainStatus = input.resolve === 'release'
          ? { ...t, status: 'failed', error: { code: 'outcome_unknown_released', message: `released by the user; nothing was started at fal, so the $${amount.toFixed(2)} reservation was released` } }
          : { ...t, status: 'failed', error: { code: 'outcome_unknown_abandoned', message: `abandoned by the user; fal may have started it, so $${amount.toFixed(2)} stays counted as spent` } };
        trainings.set(resolved.training_id, resolved);
        return resolved;
      }
      return { ...t, status: 'running' };
    }
    default:
      throw new Error(`mockCharacters: unknown verb ${verb}`);
  }
}
