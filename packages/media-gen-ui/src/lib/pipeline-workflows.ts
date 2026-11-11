import { errorMessage } from './engine-error.js';

/**
 * Pipeline workflows: named donor production pipelines (Vox Director,
 * ShotCraft, OpenCreator dubbing, Lanshu presenter, brag launch,
 * MoneyPrinterTurbo batch) as editable step DAGs persisted through the
 * `studio.pipelines.*` engine commands.
 *
 * The pure planning/state helpers below (`topoSchedule`, `planSegments`,
 * `validateWorkflow`, `readySteps`, `settleStep`) mirror the Rust
 * implementation in `crates/rightkit-media-pipeline/src/pipeline_workflows.rs` and are
 * unit-tested directly under `node --test`. The engine owns every stored workflow and run; the app keeps no
 * browser-side copy.
 */

// `./api` is imported lazily so this file also loads standalone under `node --test`
// (the test harness transpiles it alone).

export const PIPELINE_SCHEMA_VERSION = '1.0.0';
const SPEECH_CPS = 15;
const MEDIA_KINDS = ['image', 'video', 'voice', 'avatar'] as const;
const OPEN_STEPS = ['pending', 'ready', 'awaiting_submit', 'running'];

export type StepKind = 'plan' | 'voice' | 'image' | 'video' | 'avatar' | 'motion' | 'export' | 'check';
export type SupportStatus = 'native' | 'ready' | 'handoff' | 'route_unready' | 'unsupported';
export type StepStatus =
  | 'pending' | 'ready' | 'awaiting_submit' | 'running'
  | 'succeeded' | 'failed' | 'cancelled' | 'skipped' | 'unsupported' | 'route_unready';
export type RunStatus = 'queued' | 'running' | 'succeeded' | 'partial' | 'failed' | 'cancelled';

export interface StepNeed { from: string; kind: string; param: string }
export interface StepRequest {
  kind: 'image' | 'video' | 'voice' | 'avatar';
  provider: string;
  model: string;
  prompt: string;
  params: Record<string, unknown>;
}
export interface WorkflowStep {
  id: string;
  label: string;
  kind: StepKind;
  depends_on: string[];
  needs: StepNeed[];
  produces: string;
  request?: StepRequest;
  native?: string;
  optional?: boolean;
  support?: { status: SupportStatus | null; reason: string | null };
  schedule: { est_s: number; wave?: number };
  budget?: { est_usd?: number };
  retry: { max_attempts: number };
  params: Record<string, unknown>;
  segment?: { index: number; text: string; start_s: number; end_s: number } | null;
}
export interface PipelineSource { donor: string; url: string; notes: string }
export type PipelineRoutes = Record<string,
  | { provider: string; model: string; params?: Record<string, unknown> }
  | { native: 'still_clip'; params?: Record<string, unknown> }
>;
export interface PipelineTemplate {
  id: string;
  label: string;
  summary: string;
  local_routes?: PipelineRoutes;
  source: PipelineSource;
  inputs: string[];
  steps: WorkflowStep[];
}
export interface PipelineWorkflow {
  schema_version: string;
  kind: 'pipeline_workflow';
  id: string;
  name: string;
  template_id: string;
  template_label: string;
  source: PipelineSource;
  inputs: Record<string, string>;
  steps: WorkflowStep[];
  schedule: { total_est_s: number; waves: number };
  validation?: ValidationReport;
  created_at?: string;
  updated_at?: string;
}
export interface RunAsset {
  asset_id: string; path: string; sha256?: string; bytes?: number; mime?: string; kind?: string; duration_s?: number | null;
  /** Per-segment dub clips name the translated segment they voice (dub step only). */
  segment_id?: string | null;
  /** dub_plan assets only: `needs_review` holds the run; the plan and its per-segment report are kept. */
  review_status?: string;
  review_summary?: string;
  options?: string[];
  summary?: { overflow_ids?: string[]; total_excess_ms?: number; track_end_ms?: number; media_duration_ms?: number | null };
  report?: Array<Record<string, unknown>>;
}
/**
 * One voice request per translated segment, returned by `step_begin` for the dub step. The engine
 * generates `request_id` (`<run>:<step>:a<attempt>:<segment_id>`) and puts it in `request.request_id`
 * too: submit `request` as given, so a resubmission finds the same job instead of creating a second one.
 */
export interface SegmentRequest {
  segment_id: string;
  start_ms: number | null;
  end_ms: number | null;
  source_text: string;
  request_id: string;
  attempt?: number;
  request: StepRequest & { request_id?: string };
}
/** A segment job's state in a multi-job step. `not_attached`: no job named yet; `kept`: a clip carried over from an earlier attempt. */
export type SegmentJobState = 'attached' | 'running' | 'complete' | 'failed' | 'cancelled' | 'held' | 'missing' | 'not_attached' | 'kept';
/** The engine's per-segment row for a multi-job step (`step_status`, or `fanout.jobs` of a run step). */
export interface StepSegmentRow {
  segment_id: string;
  request_id: string | null;
  job_id: string | null;
  state: SegmentJobState;
  error: string | null;
  asset_id: string | null;
  attempt?: number;
}
/** What the engine keeps for a multi-job step in the run record (read it from `run_get`; `step_status` refreshes it). */
export interface StepFanout {
  policy: string;
  attempt: number;
  segment_ids: string[];
  expected: Array<{ segment_id: string; request_id: string }>;
  jobs: Array<{ segment_id: string; job_id: string; request_id: string; state: SegmentJobState; error: string | null }>;
  kept: Array<{ segment_id: string; job_id: string; attempt: number }>;
  cancelled: Array<{ segment_id: string; job_id: string; note: string }>;
  failed: Array<{ segment_id: string; job_id: string; error: string | null }>;
  held: Array<{ segment_id: string; job_id: string }>;
  /** Expected segments with no job attached yet. */
  missing: string[];
  /** A job is held as outcome_unknown: resolve it (job resolve) before the step can finish or retry. */
  awaits_resolution: boolean;
  synced_at?: string;
}
export interface StepStatusReport {
  step_id: string;
  fanout: boolean;
  status: StepStatus;
  attempt: number;
  error: string | null;
  job_id?: string | null;
  policy?: string;
  awaits_resolution?: boolean;
  segments?: StepSegmentRow[];
  missing?: string[];
  failed?: StepFanout['failed'];
  held?: StepFanout['held'];
  cancelled?: StepFanout['cancelled'];
  synced_at?: string;
}
export interface RunStep extends WorkflowStep {
  status: StepStatus;
  attempt: number;
  job_id: string | null;
  assets: RunAsset[];
  error: string | null;
  started_at: string | null;
  finished_at: string | null;
  actual_usd?: number;
  /** Present only on a multi-job step (the dub step): the engine's per-segment job record. */
  fanout?: StepFanout;
}
export interface PipelineRun {
  schema_version: string;
  kind: 'pipeline_run';
  run_id: string;
  workflow_id: string;
  workflow_name: string;
  template_id: string;
  status: RunStatus;
  budget_usd: number | null;
  spent_usd: number;
  inputs: Record<string, string>;
  steps: RunStep[];
  created_at: string;
  updated_at: string;
}
export interface ValidationReport { ok: boolean; errors: string[]; warnings: string[]; steps: number }

// ---------------------------------------------------------------------------
// dubbing: language contract and needs_review choices (pure; mirror dubbing.rs)

/** Engine's single-stage atempo ceiling (dubbing.rs MAX_TEMPO_LIMIT). */
export const DUB_TEMPO_CEILING = 2.0;
/** Engine's default speed-up limit (dubbing.rs DEFAULT_MAX_TEMPO). */
export const DUB_DEFAULT_MAX_TEMPO = 1.15;

const dubTruthy = (v: unknown) => v === true || (typeof v === 'string' && ['true', '1', 'yes'].includes(v.trim().toLowerCase()));
const dubLang = (v: unknown) => String(v ?? '').trim().toLowerCase().replace(/_/g, '-');

/**
 * The dubbing language contract, as dubbing.rs `check_languages`: both languages explicit and
 * different. `passthrough` allows the same language (a media-path test). `null` when it passes.
 */
export function checkDubbingLanguages(inputs: Record<string, unknown>): { code: string; message: string } | null {
  const source = dubLang(inputs.source_lang);
  const target = dubLang(inputs.target_lang);
  if (!source || !target) return { code: 'invalid_input', message: 'dubbing needs explicit source_lang and target_lang (no language is assumed)' };
  if (source === target && !dubTruthy(inputs.passthrough)) {
    return { code: 'dubbing_same_language', message: `source_lang and target_lang are both '${source}': nothing to dub (set passthrough: true only for a media-path test)` };
  }
  return null;
}

/** The run's speed-up limit as the engine reads it (`max_tempo`, default 1.15). */
export function dubMaxTempoOf(inputs: Record<string, unknown>): number {
  const n = typeof inputs.max_tempo === 'number' ? inputs.max_tempo : Number(String(inputs.max_tempo ?? '').trim());
  return Number.isFinite(n) && String(inputs.max_tempo ?? '').trim() !== '' ? n : DUB_DEFAULT_MAX_TEMPO;
}

export interface DubReviewRow {
  segment_id: string;
  source_text: string;
  target_text: string;
  slot_ms: number;
  synth_ms: number;
  action: string;
  excess_ms: number;
}
export type DubChoiceId = 'raise_max_tempo' | 'allow_shift' | 'shorten_translation';
export interface DubReviewChoice {
  id: DubChoiceId;
  label: string;
  enabled: boolean;
  /** Why the choice is off, or a note on it. */
  reason: string | null;
  /** The `inputs` a retry of dub_plan takes; `null` when the choice is not a retry. */
  inputs: Record<string, number | boolean> | null;
  /** allow_shift: the drift the engine reported for its preview; `null` when it reports none. */
  drift_ms?: number | null;
  /** shorten_translation: the segments whose translation must get shorter. */
  segment_ids?: string[];
}
export interface DubReview {
  rows: DubReviewRow[];
  choices: DubReviewChoice[];
  /** The engine's own option lines that no choice above covers (for example "lengthen the video"). */
  notes: string[];
  /** The speed-up limit the overflowing segments need, rounded up to 0.01 (as dubbing.rs). */
  suggested_max_tempo: number | null;
}

/**
 * Maps a `needs_review` dub_plan asset to the per-segment table and the three explicit choices. Nothing
 * is applied here: each enabled choice is a button the user presses. The drift comes from the engine's
 * option text (`preview: up to N ms later`), because the engine returns it in no field.
 */
export function dubReviewOf(asset: Pick<RunAsset, 'report' | 'options' | 'summary' | 'review_status'>, currentMaxTempo: number): DubReview {
  const rows: DubReviewRow[] = (asset.report ?? []).map((r) => ({
    segment_id: String(r.id ?? ''),
    source_text: String(r.source_text ?? ''),
    target_text: String(r.target_text ?? ''),
    slot_ms: Number(r.slot_ms ?? 0),
    synth_ms: Number(r.synth_ms ?? 0),
    action: String(r.action ?? ''),
    excess_ms: Number(r.excess_ms ?? 0)
  }));
  const overflow = rows.filter((r) => r.action === 'overflow');
  let maxRatio = 0;
  for (const r of overflow) maxRatio = Math.max(maxRatio, r.synth_ms / Math.max(r.slot_ms, 1));
  const suggested = overflow.length > 0 ? Math.ceil(maxRatio * 100 - 1e-9) / 100 : null;
  const options = asset.options ?? [];
  const driftMatch = options.map((o) => /preview: up to (\d+) ms later/.exec(o)).find(Boolean);
  const drift = driftMatch ? Number(driftMatch[1]) : null;
  const choices: DubReviewChoice[] = [];
  if (suggested !== null) {
    const ceilingOk = suggested <= DUB_TEMPO_CEILING;
    const raises = suggested > currentMaxTempo;
    choices.push({
      id: 'raise_max_tempo',
      label: `Raise the speed-up limit to ${suggested.toFixed(2)}`,
      enabled: ceilingOk && raises,
      reason: !ceilingOk
        ? `needs x${maxRatio.toFixed(2)}, above the ${DUB_TEMPO_CEILING.toFixed(1)} single-stage ceiling: a larger stretch is not enough`
        : !raises ? 'the current speed-up limit already allows this' : null,
      inputs: { max_tempo: suggested }
    });
    choices.push({
      id: 'allow_shift',
      label: 'Allow later segments to shift',
      enabled: true,
      reason: null,
      inputs: { allow_shift: true },
      drift_ms: drift
    });
    choices.push({
      id: 'shorten_translation',
      label: 'Go back and shorten the translation',
      enabled: true,
      reason: 'No retry here: a new translation re-runs translate and every paid voice job, so start a new run with the shorter text',
      inputs: null,
      segment_ids: [...(asset.summary?.overflow_ids ?? [])]
    });
  }
  const covered = /^(shorten the translation|allow a larger stretch|a larger stretch is not enough|allow_shift:)/;
  const notes = options.filter((o) => !covered.test(o));
  return {
    rows,
    choices,
    notes,
    suggested_max_tempo: suggested
  };
}

// ---------------------------------------------------------------------------
// pure planning helpers (kept in lockstep with the Rust module)

function estSpeechSeconds(text: string): number {
  return Math.max(1, [...text].length / SPEECH_CPS);
}

/** Lanshu: narration is the master clock; split at sentence boundaries <= cap. */
export function planSegments(script: string, capS: number): Array<{ index: number; text: string; start_s: number; end_s: number }> {
  const cap = Math.max(1, capS);
  const sentences: string[] = [];
  let cur = '';
  for (const c of script) {
    cur += c;
    if ('.!?;。！？\n'.includes(c)) {
      if (cur.trim()) sentences.push(cur.trim());
      cur = '';
    }
  }
  if (cur.trim()) sentences.push(cur.trim());
  const segments: Array<{ index: number; text: string; start_s: number; end_s: number }> = [];
  let buf = '';
  let start = 0;
  for (const s of sentences) {
    const d = estSpeechSeconds(s);
    if (buf && estSpeechSeconds(buf) + d > cap) {
      segments.push({ index: segments.length, text: buf.trim(), start_s: start, end_s: start + estSpeechSeconds(buf) });
      start += estSpeechSeconds(buf);
      buf = '';
    }
    if (d > cap && !buf) {
      segments.push({ index: segments.length, text: s, start_s: start, end_s: start + d });
      start += d;
      continue;
    }
    buf = buf ? `${buf} ${s}` : s;
  }
  if (buf.trim()) segments.push({ index: segments.length, text: buf.trim(), start_s: start, end_s: start + estSpeechSeconds(buf) });
  return segments;
}

/** Topological waves + critical-path seconds; throws on cycles/unknown deps. */
export function topoSchedule(steps: Array<Pick<WorkflowStep, 'id' | 'depends_on'> & { schedule?: { est_s?: number } }>): { waves: Record<string, number>; total_s: number } {
  const waves: Record<string, number> = {};
  const endAt: Record<string, number> = {};
  const remaining = steps.map((s) => s.id);
  let guard = 0;
  while (remaining.length) {
    if (++guard > steps.length + 2) throw new Error('step dependencies contain a cycle');
    let progressed = false;
    for (let i = 0; i < remaining.length; ) {
      const step = steps.find((s) => s.id === remaining[i])!;
      if (step.depends_on.every((d) => d in waves)) {
        waves[step.id] = step.depends_on.reduce((w, d) => Math.max(w, waves[d] + 1), 0);
        const start = step.depends_on.reduce((t, d) => Math.max(t, endAt[d]), 0);
        endAt[step.id] = start + (step.schedule?.est_s ?? 1);
        remaining.splice(i, 1);
        progressed = true;
      } else {
        i += 1;
      }
    }
    if (!progressed) throw new Error('step dependencies contain a cycle');
  }
  return { waves, total_s: Object.values(endAt).reduce((a, b) => Math.max(a, b), 0) };
}

export function validateWorkflow(wf: Pick<PipelineWorkflow, 'steps'>): ValidationReport {
  const errors: string[] = [];
  const warnings: string[] = [];
  const steps = wf.steps ?? [];
  if (!steps.length) errors.push('workflow has no steps');
  const ids = new Set(steps.map((s) => s.id));
  if (ids.size !== steps.length) errors.push('duplicate step ids');
  for (const s of steps) {
    for (const d of s.depends_on ?? []) {
      if (!ids.has(d)) errors.push(`step ${s.id} depends on unknown step ${d}`);
    }
    for (const n of s.needs ?? []) {
      if (!ids.has(n.from)) errors.push(`step ${s.id} needs artifact from unknown step ${n.from}`);
      if (!['image', 'video', 'audio', 'file', 'text', 'data'].includes(n.kind)) {
        errors.push(`step ${s.id} has unsupported needed asset kind ${n.kind}`);
      }
    }
    if (s.request) {
      if (!(MEDIA_KINDS as readonly string[]).includes(s.request.kind)) errors.push(`step ${s.id} request kind ${s.request.kind} is not a media kind`);
      if (!s.request.provider) warnings.push(`step ${s.id} request has no provider`);
    }
    if (!['plan', 'voice', 'image', 'video', 'avatar', 'motion', 'export', 'check'].includes(s.kind)) errors.push(`step ${s.id} has unknown kind ${s.kind}`);
    if (s.support?.status === 'unsupported' || s.support?.status === 'route_unready') warnings.push(`step ${s.id}: ${s.support.reason ?? 'unavailable'}`);
  }
  try {
    topoSchedule(steps);
  } catch (e) {
    errors.push(errorMessage(e));
  }
  return { ok: errors.length === 0, errors, warnings, steps: steps.length };
}

/** Recompute step readiness + run status (pure mirror of the Rust `refresh`). */
export function refreshRun<T extends { status: string; steps: RunStep[]; updated_at?: string }>(run: T): T {
  const done = new Set(run.steps.filter((s) => s.status === 'succeeded' || s.status === 'skipped').map((s) => s.id));
  const cancelled = run.status === 'cancelled';
  let anyOpen = false;
  let anyFailed = false;
  let anyRunning = false;
  for (const s of run.steps) {
    if (s.status === 'failed') anyFailed = true;
    if (s.status === 'running' || s.status === 'awaiting_submit') anyRunning = true;
    if (!OPEN_STEPS.includes(s.status)) continue;
    anyOpen = true;
    if (cancelled) {
      s.status = 'cancelled';
      continue;
    }
    const depsOk = s.depends_on.every((d) => done.has(d));
    const supported = s.support?.status !== 'unsupported' && s.support?.status !== 'route_unready';
    if (s.status === 'pending' && depsOk && supported) s.status = 'ready';
    else if (s.status === 'pending' && !supported) s.status = s.support?.status === 'route_unready' ? 'route_unready' : 'unsupported';
  }
  const doneCount = run.steps.filter((s) => ['succeeded', 'skipped', 'unsupported', 'route_unready'].includes(s.status)).length;
  if (cancelled) run.status = 'cancelled';
  else if (anyRunning) run.status = 'running';
  else if (doneCount === run.steps.length) run.status = anyFailed ? 'partial' : 'succeeded';
  else if (anyOpen || anyFailed) run.status = 'running';
  else run.status = 'queued';
  return run;
}

export function readySteps(run: PipelineRun): RunStep[] {
  return run.steps.filter((s) => s.status === 'ready');
}

/** Steps whose deps are all terminal-successful but which remain blocked. */
export function blockedReason(step: RunStep, run: PipelineRun): string | null {
  if (step.status === 'unsupported' || step.status === 'route_unready') return step.support?.reason ?? 'No supported engine route';
  if (step.status !== 'pending' && step.status !== 'ready') return null;
  const missing = step.depends_on.filter((d) => {
    const dep = run.steps.find((s) => s.id === d);
    return !dep || !['succeeded', 'skipped'].includes(dep.status);
  });
  if (missing.length) return `waiting on ${missing.join(', ')}`;
  if (step.support?.status === 'route_unready') return step.support.reason;
  return null;
}

/** Inject dependency artifacts into a request's params (`needs` wiring). */
export function injectNeeds(run: PipelineRun, step: RunStep, request: StepRequest): StepRequest {
  const out: StepRequest = { ...request, params: { ...request.params } };
  for (const n of step.needs ?? []) {
    const dep = run.steps.find((s) => s.id === n.from);
    if (!dep || dep.status !== 'succeeded') throw new Error(`dependency ${n.from} has not succeeded`);
    const asset = dep.assets.find((a) => a.kind === n.kind);
    if (!asset?.path) throw new Error(`dependency ${n.from} produced no ${n.kind} asset`);
    out.params[n.param] = asset.path;
  }
  return out;
}

function rid(prefix: string) {
  return `${prefix}-${Math.random().toString(16).slice(2, 10)}`;
}

/** Browser copies of the Rust templates (kept structurally identical). */
export const PIPELINE_TEMPLATES: PipelineTemplate[] = [
  {
    id: 'vox_director',
    label: 'Vox-style explainer',
    summary: 'Topic → beats/script → keyframes → motion scenes → narration → captions/composition → playable video.',
    source: { donor: 'Vox Director', url: 'https://github.com/Alisa0808/vox-director', notes: 'beats.json + poster-first flow; stage execution ported natively, models via existing adapters' },
    inputs: ['topic', 'script', 'aspect', 'hook_image_prompt', 'scene_video_prompt', 'asset_paths', 'music_path', 'subtitles_path', 'voice_id'],
    steps: [
      { id: 'script', label: 'Script', kind: 'plan', depends_on: [], needs: [], produces: 'text', native: 'script', schedule: { est_s: 1 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'beats', label: 'Beat map', kind: 'plan', depends_on: [], needs: [], produces: 'data', native: 'beats', schedule: { est_s: 1 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'keyframes', label: 'Beat keyframes', kind: 'image', depends_on: ['beats'], needs: [], produces: 'image', request: { kind: 'image', provider: 'fal', model: 'fal-ai/flux-2/klein/9b', prompt: '{hook_image_prompt}', params: { aspect: '{aspect}' } }, schedule: { est_s: 60 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'narration', label: 'Narration', kind: 'voice', depends_on: ['script'], needs: [], produces: 'audio', request: { kind: 'voice', provider: 'local-qts', model: 'qwen3-tts-0.6b-q8_0', prompt: '{script}', params: {} }, schedule: { est_s: 45 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'scenes', label: 'Motion scenes', kind: 'video', depends_on: ['keyframes', 'narration'], needs: [{ from: 'keyframes', kind: 'image', param: 'image_url' }], produces: 'video', request: { kind: 'video', provider: 'fal', model: 'fal-ai/veo3.1/fast/image-to-video', prompt: '{scene_video_prompt}', params: { aspect: '{aspect}', duration: 8 } }, schedule: { est_s: 300 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'captions', label: 'Compose + captions (native FFmpeg)', kind: 'video', depends_on: ['scenes', 'narration'], needs: [], produces: 'video', native: 'compose_video', schedule: { est_s: 30 }, retry: { max_attempts: 2 }, params: {} }
    ]
  },
  {
    id: 'shotcraft',
    label: 'ShotCraft product video',
    summary: 'Product assets → shot plan → camera/motion recipe clips → music/SFX timing → composition.',
    source: { donor: 'Video ShotCraft', url: 'https://github.com/Vincentwei1021/video-shotcraft', notes: 'references/shots recipe data + music-beat-sync/sound-design; pin revision before counting entries' },
    inputs: ['product_name', 'aspect', 'shot_a_prompt', 'shot_b_prompt'],
    steps: [
      { id: 'shot_plan', label: 'Shot plan', kind: 'plan', depends_on: [], needs: [], produces: 'data', native: 'shot_plan', schedule: { est_s: 1 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'shot_a', label: 'Hero shot', kind: 'video', depends_on: ['shot_plan'], needs: [], produces: 'video', request: { kind: 'video', provider: 'fal', model: 'fal-ai/veo3.1/fast', prompt: '{shot_a_prompt}', params: { aspect: '{aspect}', duration: 8 } }, schedule: { est_s: 120 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'shot_b', label: 'Detail shot', kind: 'video', depends_on: ['shot_plan'], needs: [], produces: 'video', request: { kind: 'video', provider: 'fal', model: 'fal-ai/veo3.1/fast', prompt: '{shot_b_prompt}', params: { aspect: '{aspect}', duration: 8 } }, schedule: { est_s: 120 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'timing', label: 'Music/SFX timing', kind: 'check', depends_on: ['shot_a', 'shot_b'], needs: [], produces: 'data', native: 'check_delivery_intent', schedule: { est_s: 2 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'compose', label: 'Compose final video (native FFmpeg)', kind: 'video', depends_on: ['timing'], needs: [], produces: 'video', native: 'compose_video', schedule: { est_s: 30 }, retry: { max_attempts: 2 }, params: {} }
    ]
  },
  {
    id: 'opencreator_dubbing',
    label: 'OpenCreator translation/dubbing',
    summary: 'Transcription → per-segment LLM translation (explicit source/target language) → one voice job per segment (cloned or provider voice) → explicit retiming (fits / short / tempo / overflow, needs_review on overflow) → segment-aligned SRT + dub track → subtitled MP4 (dub replaces or mixes over the original audio).',
    source: { donor: 'OpenCreator / KrillinAI', url: 'https://github.com/krillinai/OpenCreator', notes: 'subtitle-line handling, terminology translation, retiming, separate exports; Auto Clips & Digital Avatar are upstream in-development, not counted' },
    inputs: ['source_media', 'dubbed_script', 'voice_id', 'voice_prompt', 'target_lang', 'glossary', 'source_lang', 'audio_path',
      'translate_provider', 'translate_mode', 'max_tempo', 'allow_shift', 'min_gap_ms', 'original_audio', 'passthrough'],
    steps: [
      { id: 'transcribe', label: 'Transcription (whisper.cpp)', kind: 'check', depends_on: [], needs: [], produces: 'file', native: 'transcribe', support: { status: null, reason: 'Local whisper.cpp + ggml model; probed at plan time.' }, schedule: { est_s: 30 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'translate', label: 'Terminology-consistent translation', kind: 'plan', depends_on: ['transcribe'], needs: [], produces: 'text', native: 'translate', support: { status: 'native', reason: 'Sentence translation through a text-LLM route (translate_provider) with the glossary enforced; a dub refuses glossary-only translation and source_lang == target_lang unless passthrough is set. An unregistered route reports route_unready.' }, schedule: { est_s: 5 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'dub', label: 'Dub narration (one voice job per segment)', kind: 'voice', depends_on: ['translate'], needs: [], produces: 'audio', request: { kind: 'voice', provider: 'local-qts', model: 'qwen3-tts-0.6b-q8_0', prompt: '{dubbed_script}', params: {} }, schedule: { est_s: 60 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'dub_plan', label: 'Retiming plan + dub track', kind: 'plan', depends_on: ['dub'], needs: [], produces: 'audio', native: 'dub_plan', schedule: { est_s: 5 }, retry: { max_attempts: 6 }, params: {} },
      { id: 'srt_export', label: 'Aligned subtitle export (SRT, per segment)', kind: 'export', depends_on: ['dub_plan'], needs: [], produces: 'file', native: 'export_srt', schedule: { est_s: 2 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'compose', label: 'Dubbed MP4 (native FFmpeg)', kind: 'video', depends_on: ['srt_export', 'dub_plan'], needs: [], produces: 'video', native: 'compose_video', schedule: { est_s: 30 }, retry: { max_attempts: 2 }, params: {} }
    ]
  },
  {
    id: 'lanshu_presenter',
    label: 'Lanshu presenter (narration clock)',
    summary: 'Script → full narration as master clock → pause-aligned segments → presenter clips → joins, lip-sync & loudness checks.',
    source: { donor: 'Lanshu presenter skill', url: 'https://github.com/cclank/lanshu-create-ai-presenter-video', notes: 'plan_segments/check_state/finalize_delivery shape; cost estimate & retry ceiling surface before paid steps' },
    inputs: ['script', 'avatar_id', 'voice_id', 'segment_cap_s', 'segment_1_text'],
    steps: [
      { id: 'narration_clock', label: 'Narration master clock', kind: 'voice', depends_on: [], needs: [], produces: 'audio', request: { kind: 'voice', provider: 'local-qts', model: 'qwen3-tts-0.6b-q8_0', prompt: '{script}', params: {} }, schedule: { est_s: 60 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'segment_plan', label: 'Pause-aligned segment plan', kind: 'plan', depends_on: ['narration_clock'], needs: [], produces: 'data', native: 'segment_plan', schedule: { est_s: 1 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'segment_1', label: 'Presenter segment 1', kind: 'avatar', depends_on: ['segment_plan'], needs: [{ from: 'narration_clock', kind: 'audio', param: 'audio_url' }], produces: 'video', request: { kind: 'avatar', provider: 'heygen', model: 'avatar-iv', prompt: '{segment_1_text}', params: { avatar_id: '{avatar_id}' } }, schedule: { est_s: 240 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'joins_checks', label: 'Joins + lip-sync + loudness checks', kind: 'check', depends_on: ['segment_1'], needs: [], produces: 'data', native: 'check_presenter', schedule: { est_s: 3 }, retry: { max_attempts: 2 }, params: {} }
    ]
  },
  {
    id: 'brag_launch',
    label: 'brag launch video',
    summary: 'Inspect real product assets → concise entry→action→result story → motion composition → optional narration → video + share copy.',
    source: { donor: 'brag', url: 'https://github.com/latent-spaces/brag', notes: 'inspection rubric + step-1..4 delivery checklist; voiceover is optional upstream; git-log ingestion is our extension, not donor behaviour' },
    inputs: ['product_name', 'script', 'asset_paths', 'audio_path', 'music_path', 'subtitles_path', 'voice_id'],
    steps: [
      { id: 'inspect', label: 'Inspect product assets', kind: 'check', depends_on: [], needs: [], produces: 'data', native: 'check_inputs', schedule: { est_s: 2 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'story', label: 'Launch story', kind: 'plan', depends_on: ['inspect'], needs: [], produces: 'text', native: 'script', schedule: { est_s: 1 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'compose', label: 'Compose launch video (native FFmpeg)', kind: 'video', depends_on: ['story', 'inspect'], needs: [], produces: 'video', native: 'compose_video', schedule: { est_s: 60 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'voiceover', label: 'Optional narration', kind: 'voice', depends_on: ['story'], needs: [], produces: 'audio', optional: true, request: { kind: 'voice', provider: 'local-qts', model: 'qwen3-tts-0.6b-q8_0', prompt: '{script}', params: {} }, schedule: { est_s: 45 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'share_copy', label: 'Share copy', kind: 'export', depends_on: ['story'], needs: [], produces: 'file', native: 'export_text', schedule: { est_s: 1 }, retry: { max_attempts: 2 }, params: {} }
    ]
  },
  {
    id: 'mpt_batch',
    label: 'MoneyPrinterTurbo batch variations',
    summary: 'Topic → script → narration → N video variants. Stock-footage/source mapping remains a recommendation, not an executed source.',
    source: { donor: 'MoneyPrinterTurbo', url: 'https://github.com/harry0703/MoneyPrinterTurbo', notes: 'keeps the batch-variation contract; upstream general pipeline overlaps the Production stack and is not duplicated' },
    inputs: ['script', 'aspect', 'variant_prompt', 'variant_prompt_2', 'voice_id'],
    steps: [
      { id: 'mpt_script', label: 'Script', kind: 'plan', depends_on: [], needs: [], produces: 'text', native: 'script', schedule: { est_s: 1 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'mpt_voice', label: 'Narration', kind: 'voice', depends_on: ['mpt_script'], needs: [], produces: 'audio', request: { kind: 'voice', provider: 'local-qts', model: 'qwen3-tts-0.6b-q8_0', prompt: '{script}', params: {} }, schedule: { est_s: 45 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'variant_1', label: 'Variant 1 video', kind: 'video', depends_on: ['mpt_voice'], needs: [{ from: 'mpt_voice', kind: 'audio', param: 'audio_url' }], produces: 'video', request: { kind: 'video', provider: 'fal', model: 'fal-ai/veo3.1/fast', prompt: '{variant_prompt}', params: { aspect: '{aspect}', duration: 8 } }, schedule: { est_s: 240 }, retry: { max_attempts: 2 }, params: {} },
      { id: 'variant_2', label: 'Variant 2 video', kind: 'video', depends_on: ['mpt_voice'], needs: [{ from: 'mpt_voice', kind: 'audio', param: 'audio_url' }], produces: 'video', request: { kind: 'video', provider: 'fal', model: 'fal-ai/veo3.1/fast', prompt: '{variant_prompt_2}', params: { aspect: '{aspect}', duration: 8 } }, schedule: { est_s: 240 }, retry: { max_attempts: 2 }, params: {} }
    ]
  }
];

function fillText(template: string, vars: Record<string, string>): string {
  let out = template;
  for (const [k, v] of Object.entries(vars)) out = out.split(`{${k}}`).join(v);
  return out;
}
function fillDeep<T>(v: T, vars: Record<string, string>): T {
  if (typeof v === 'string') return fillText(v, vars) as T;
  if (Array.isArray(v)) return v.map((x) => fillDeep(x, vars)) as T;
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fillDeep(x, vars)])) as T;
  return v;
}

/** Pure plan builder shared by the browser path and tests. */
export function buildWorkflowFromTemplate(template: PipelineTemplate, inputs: Record<string, string>, meta: { id?: string; name?: string } = {}): PipelineWorkflow {
  const vars: Record<string, string> = { ...inputs };
  for (const k of template.inputs) vars[k] = vars[k] ?? '';
  let steps = template.steps.map((s) => fillDeep(structuredClone(s), vars));
  if (template.id === 'lanshu_presenter') {
    const cap = Math.min(120, Math.max(5, Number(inputs.segment_cap_s) || 25));
    const segments = planSegments(inputs.script ?? '', cap);
    const base = steps.findIndex((s) => s.id === 'segment_1');
    if (base >= 0 && segments.length > 1) {
      const tpl = steps[base];
      const expanded = segments.map((seg, i) => ({
        ...structuredClone(tpl),
        id: `segment_${i + 1}`,
        label: `Presenter segment ${i + 1} of ${segments.length}`,
        request: tpl.request ? { ...tpl.request, prompt: seg.text } : undefined,
        segment: seg
      }));
      steps.splice(base, 1, ...expanded);
      steps = steps.map((s) =>
        s.id === 'joins_checks' ? { ...s, depends_on: segments.map((_, i) => `segment_${i + 1}`) } : s
      );
    } else if (base >= 0) {
      steps[base] = { ...steps[base], segment: segments[0] ?? null, request: steps[base].request ? { ...steps[base].request!, prompt: segments[0]?.text ?? '' } : undefined };
    }
  }
  const { waves, total_s } = topoSchedule(steps);
  // Browser honesty: native media steps need engine subprocesses, so in this
  // mirror they are route_unready; glossary translation is real even here.
  const browserSupport: Record<string, { status: SupportStatus; reason: string }> = {
    compose_video: { status: 'route_unready', reason: 'composition needs the native engine (ffmpeg subprocess); run in the app' },
    dub_plan: { status: 'route_unready', reason: 'the retiming plan renders the dub track with ffmpeg in the native engine; run in the app' },
    transcribe: { status: 'route_unready', reason: 'ASR needs a whisper.cpp binary + ggml model in the native engine' },
    translate: { status: 'native', reason: 'glossary substitution; sentence MT needs a text-LLM route' }
  };
  steps = steps.map((s) => {
    let support = s.support;
    if (!support || support.status === null) {
      support = s.native && browserSupport[s.native]
        ? browserSupport[s.native]
        : { status: s.request ? 'ready' : 'native', reason: support?.reason ?? null };
    }
    return { ...s, schedule: { ...s.schedule, wave: waves[s.id] }, support };
  });
  const wf: PipelineWorkflow = {
    schema_version: PIPELINE_SCHEMA_VERSION,
    kind: 'pipeline_workflow',
    id: meta.id ?? rid('wf'),
    name: meta.name || template.label,
    template_id: template.id,
    template_label: template.label,
    source: template.source,
    inputs,
    steps,
    schedule: { total_est_s: total_s, waves: Math.max(...Object.values(waves), 0) + 1 }
  };
  wf.validation = validateWorkflow(wf);
  return wf;
}

/** Parse a glossary from a JSON-object string or `a=b; c=d` pairs. */
export function parseGlossary(raw: string | undefined): Array<[string, string]> {
  if (!raw?.trim()) return [];
  const s = raw.trim();
  try {
    const obj = JSON.parse(s) as Record<string, unknown>;
    if (obj && typeof obj === 'object' && !Array.isArray(obj)) {
      return Object.entries(obj).map(([k, v]) => [k, String(v)] as [string, string]);
    }
  } catch {
    /* not JSON — fall through to pair syntax */
  }
  return s
    .split(/[;\n]/)
    .map((p) => p.split(/=|:/).map((x) => x.trim()))
    .filter((p): p is [string, string] => p.length >= 2 && Boolean(p[0]) && Boolean(p[1]))
    .map(([a, b]) => [a, b]);
}

/** Terminology substitution (mirrors translate.rs); not sentence translation. */
export function applyGlossary(text: string, glossary: Array<[string, string]>): { text: string; applied: number } {
  let out = text;
  let applied = 0;
  for (const [from, to] of glossary) {
    if (!from) continue;
    const n = out.split(from).length - 1;
    if (n > 0) {
      applied += n;
      out = out.split(from).join(to);
    }
  }
  return { text: out, applied };
}

async function call<T>(verb: string, input: Record<string, unknown> = {}): Promise<T> {
  const { studio } = await import('./api.js');
  return studio<T>(`pipelines.${verb}`, input);
}

export const pipelinesApi = {
  templates: () => call<{ templates: PipelineTemplate[] }>('templates'),
  plan: (templateId: string, inputs: Record<string, string>, routes?: PipelineRoutes) => call<PipelineWorkflow>('plan', { template_id: templateId, inputs, ...(routes ? { routes } : {}) }),
  validate: (workflow: PipelineWorkflow) => call<ValidationReport>('validate', { workflow }),
  save: (workflow: PipelineWorkflow) => call<{ id: string; path?: string; workflow: PipelineWorkflow }>('save', { workflow }),
  list: () => call<{ workflows: Array<{ id: string; name: string; template_id: string; steps: number; updated_at?: string }> }>('list'),
  get: (id: string) => call<PipelineWorkflow>('get', { id }),
  delete: (id: string) => call<{ deleted: string }>('delete', { id }),
  runCreate: (workflow: PipelineWorkflow | string, budgetUsd?: number, routes?: PipelineRoutes) =>
    call<PipelineRun>('run_create', {
      ...(typeof workflow === 'string' ? { workflow_id: workflow } : { workflow }),
      budget_usd: budgetUsd ?? null,
      ...(routes ? { routes } : {})
    }),
  runList: () => call<{ runs: Array<{ run_id: string; workflow_name: string; status: RunStatus; spent_usd: number; budget_usd: number | null; updated_at: string }> }>('run_list'),
  runGet: (runId: string) => call<PipelineRun>('run_get', { run_id: runId }),
  runReady: (runId: string) => call<{ run_id: string; status: RunStatus; ready: RunStep[] }>('run_ready', { run_id: runId }),
  /**
   * `executed`: `native` ran now; `handoff` returns `request` (one job) or `requests` (one per segment);
   * `resume` (a multi-job step that is already begun) returns `requests` for only the segments with no
   * attached job, under the same attempt and request ids; `reused` means every segment already had its clip.
   */
  stepBegin: (runId: string, stepId: string) =>
    call<{ step_id: string; run_id: string; executed: 'native' | 'handoff' | 'resume' | 'reused'; request?: StepRequest; requests?: SegmentRequest[]; reused_segments?: string[]; asset?: RunAsset; run: PipelineRun }>('step_begin', { run_id: runId, step_id: stepId }),
  /** A multi-job step takes `segmentId` with each job (attach as many times as you like; the same pair again is a no-op). */
  stepAttach: (runId: string, stepId: string, jobId: string, segmentId?: string) =>
    call<PipelineRun & { attach?: { attached: string[]; already_attached: string[]; missing_segments: string[] } }>('step_attach', { run_id: runId, step_id: stepId, job_id: jobId, ...(segmentId ? { segment_id: segmentId } : {}) }),
  /** The engine reads the job store and settles a multi-job step itself; this returns per-segment state. */
  stepStatus: (runId: string, stepId?: string) =>
    call<{ run_id: string; status: RunStatus; steps: StepStatusReport[]; run: PipelineRun }>('step_status', { run_id: runId, ...(stepId ? { step_id: stepId } : {}) }),
  stepCancel: (runId: string, stepId: string) =>
    call<{ step_id: string; status: StepStatus; cancelled_job_ids: string[]; held_job_ids: string[]; still_live_job_ids: string[]; kept_segments: string[]; run: PipelineRun }>('step_cancel', { run_id: runId, step_id: stepId }),
  stepSettle: (runId: string, stepId: string, status: 'complete' | 'failed' | 'cancelled', extra: { assets?: RunAsset[]; error?: string; actual_usd?: number } = {}) =>
    call<PipelineRun>('step_settle', { run_id: runId, step_id: stepId, status, ...extra }),
  /** `inputs` merge into the run's inputs before the retry (the dub_plan choices use this). */
  stepRetry: (runId: string, stepId: string, inputs?: Record<string, unknown>) =>
    call<PipelineRun>('step_retry', { run_id: runId, step_id: stepId, ...(inputs ? { inputs } : {}) }),
  stepSkip: (runId: string, stepId: string) => call<PipelineRun>('step_skip', { run_id: runId, step_id: stepId }),
  /** `multi_job` (only when a multi-job step was live) lists what the engine itself cancelled, what is held, and which clips were kept. */
  runCancel: (runId: string) =>
    call<{ run: PipelineRun; cancel_job_ids: string[]; multi_job?: Array<{ step_id: string; cancelled_job_ids: string[]; held_job_ids: string[]; still_live_job_ids: string[]; kept_segments: string[] }> }>('run_cancel', { run_id: runId }),
  runRecover: (runId: string) => call<{ run: PipelineRun; recovered: string[]; orphaned: string[] }>('run_recover', { run_id: runId }),
  // Native media capability surface (honest errors in the browser mirror).
  mediaStatus: () => call<{ ffmpeg: { path: string | null; version: string | null }; ffprobe: { path: string | null; version: string | null }; whisper: { path: string | null; version: string | null }; capabilities: Record<string, boolean> }>('media_status'),
  probe: (path: string) => call<Record<string, unknown>>('probe', { path }),
  compose: (spec: Record<string, unknown>) => call<Record<string, unknown>>('compose', spec),
  transcribe: (input: { audio_path: string; model?: string; lang?: string }) => call<Record<string, unknown>>('transcribe', input as Record<string, unknown>),
  asrStatus: () => call<{ binary: string | null; model: string | null; ready: boolean }>('asr_status'),
  asrProvision: (model?: string) => call<Record<string, unknown>>('asr_provision', { model }),
  translate: (input: { text?: string; segments?: unknown[]; target_lang: string; glossary?: string | Record<string, string>; provider?: string }) =>
    call<Record<string, unknown>>('translate', input as Record<string, unknown>)
};
