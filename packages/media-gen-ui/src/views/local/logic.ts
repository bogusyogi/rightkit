/**
 * Pure helpers for the local image (sd.cpp) chat. Payload shapes mirror
 * src-tauri/src/local_image/commands.rs (SubmitRequest, resolved_settings).
 * No runtime imports on purpose: node --test loads this file directly.
 */
import type {
  LocalImageConversationSnapshot,
  LocalImageInstallProgress,
  LocalImageJob,
  LocalImageJobState,
  LocalImageProfile,
  LocalImageRuntimeStatus,
  LocalImageSubmitRequest
} from '../../lib/types.js';

export const ACTIVE_JOB_STATES: LocalImageJobState[] = ['queued', 'loading', 'submitting', 'running', 'finalizing', 'cancelling'];
export const RETRYABLE_JOB_STATES: LocalImageJobState[] = ['failed', 'interrupted'];
const RETRY_OVERRIDE_KEYS = ['sample_steps', 'sample_method', 'scheduler', 'txt_cfg', 'negative_prompt'];

export const RESOLUTIONS = [
  { label: '256×256 · quick CPU preview', width: 256, height: 256 },
  { label: '512×512 · preview', width: 512, height: 512 },
  { label: '1024×1024', width: 1024, height: 1024 },
  { label: '1024×1536', width: 1024, height: 1536 },
  { label: '1536×1024', width: 1536, height: 1024 },
  { label: '768×1280', width: 768, height: 1280 },
  { label: '1280×768', width: 1280, height: 768 }
];

/** stable-diffusion.cpp names; the backend accepts any non-empty sampler string. */
export const SAMPLERS = ['euler', 'euler_a', 'heun', 'dpm2', 'dpm++2s_a', 'dpm++2m', 'dpm++2mv2', 'ipndm', 'ipndm_v', 'lcm', 'ddim_trailing', 'tcd'];
export const SCHEDULERS = ['discrete', 'karras', 'exponential', 'ays', 'gits', 'smoothstep', 'sgm_uniform', 'simple', 'lcm'];
export const AUTO_SCHEDULER = 'auto';

export function isActiveJob(job: Pick<LocalImageJob, 'state'>): boolean {
  return ACTIVE_JOB_STATES.includes(job.state);
}

export function isRetryable(job: Pick<LocalImageJob, 'state'>): boolean {
  return RETRYABLE_JOB_STATES.includes(job.state);
}

export interface SettingsForm {
  width: number;
  height: number;
  seed: string;
  steps: string;
  cfg: string;
  sampler: string;
  scheduler: string;
  negative: string;
}

export function defaultSettings(profile: Pick<LocalImageProfile, 'defaults'>): SettingsForm {
  return {
    width: profile.defaults.width,
    height: profile.defaults.height,
    seed: '',
    steps: '',
    cfg: '',
    sampler: '',
    scheduler: '',
    negative: profile.defaults.negative_prompt
  };
}

export type BuildResult = { ok: true; request: LocalImageSubmitRequest } | { ok: false; error: string };

export function buildSubmitRequest(input: {
  profile: LocalImageProfile;
  prompt: string;
  settings: SettingsForm;
  referenceIds: string[];
  conversationId?: string | null;
  parentMessageId?: string | null;
  requestId: string;
}): BuildResult {
  const { profile, settings: s } = input;
  const prompt = input.prompt.trim();
  if (!prompt) return { ok: false, error: 'Describe what to generate first.' };
  const multiple = profile.dimension_multiple || 32;
  if (!Number.isInteger(s.width) || !Number.isInteger(s.height) || s.width < 64 || s.height < 64 || s.width > 4096 || s.height > 4096 || s.width % multiple !== 0 || s.height % multiple !== 0) {
    return { ok: false, error: `Size ${s.width}x${s.height} must be 64-4096 and a multiple of ${multiple}.` };
  }
  if (input.referenceIds.length > 1) return { ok: false, error: 'Local editing takes at most one reference image.' };
  const overrides: Record<string, unknown> = {};
  if (s.steps.trim()) {
    const steps = Number(s.steps);
    if (!Number.isInteger(steps) || steps < 1 || steps > 100) return { ok: false, error: 'Steps must be a whole number from 1 to 100.' };
    overrides.sample_steps = steps;
  }
  if (s.cfg.trim()) {
    const cfg = Number(s.cfg);
    if (!Number.isFinite(cfg) || cfg < 0 || cfg > 50) return { ok: false, error: 'CFG must be a number from 0 to 50.' };
    overrides.txt_cfg = cfg;
  }
  if (s.sampler.trim() && s.sampler.trim() !== profile.defaults.sample_method) overrides.sample_method = s.sampler.trim();
  if (s.scheduler.trim()) {
    if (s.scheduler === AUTO_SCHEDULER) {
      if (profile.defaults.scheduler) overrides.scheduler = null;
    } else if (s.scheduler !== profile.defaults.scheduler) {
      overrides.scheduler = s.scheduler;
    }
  }
  let seed: number | undefined;
  if (s.seed.trim()) {
    seed = Number(s.seed);
    if (!Number.isInteger(seed) || seed < -1 || seed > 2147483647) return { ok: false, error: 'Seed must be -1 (random) or a whole number up to 2147483647.' };
  }
  const request: LocalImageSubmitRequest = {
    request_id: input.requestId,
    conversation_id: input.conversationId ?? undefined,
    parent_message_id: input.parentMessageId ?? undefined,
    profile_id: profile.id,
    prompt,
    negative_prompt: s.negative.trim(),
    width: s.width,
    height: s.height,
    reference_ids: input.referenceIds
  };
  if (seed !== undefined) request.seed = seed;
  if (Object.keys(overrides).length) request.overrides = overrides as LocalImageSubmitRequest['overrides'];
  return { ok: true, request };
}

/** Re-run a failed or interrupted job with its exact settings. */
export function buildRetryRequest(job: LocalImageJob, requestId: string): LocalImageSubmitRequest {
  const overrides = Object.fromEntries(Object.entries(job.overrides ?? {}).filter(([k]) => RETRY_OVERRIDE_KEYS.includes(k)));
  return {
    request_id: requestId,
    conversation_id: job.conversation_id,
    parent_message_id: job.parent_message_id ?? undefined,
    profile_id: job.profile_id,
    prompt: job.prompt,
    negative_prompt: job.negative_prompt,
    width: job.width,
    height: job.height,
    seed: job.seed,
    reference_ids: job.reference_ids,
    overrides: Object.keys(overrides).length ? (overrides as LocalImageSubmitRequest['overrides']) : undefined,
    retry_of: job.id
  };
}

export function conversationLabel(snapshot: LocalImageConversationSnapshot): string {
  const prompt = snapshot.messages.find((m) => m.role === 'user' && m.prompt)?.prompt;
  const label = prompt || snapshot.conversation.title || 'Untitled image';
  return label.length > 48 ? `${label.slice(0, 48)}…` : label;
}

export function conversationActivity(snapshot: LocalImageConversationSnapshot): string {
  const stamps = [snapshot.conversation.created_at, ...snapshot.messages.map((m) => m.created_at), ...snapshot.jobs.map((j) => j.updated_at || j.created_at)];
  return stamps.sort().pop() || '';
}

export function orderConversations(list: LocalImageConversationSnapshot[]): LocalImageConversationSnapshot[] {
  return [...list].sort((a, b) => conversationActivity(b).localeCompare(conversationActivity(a)));
}

export interface QueueEntry {
  job: LocalImageJob;
  conversationId: string;
  label: string;
  /** 1-based position among jobs still waiting to run; 0 when running or paused. */
  waiting: number;
}

/** Active and paused jobs across all conversations, oldest first. */
export function queueEntries(list: LocalImageConversationSnapshot[]): QueueEntry[] {
  const entries: QueueEntry[] = [];
  for (const snap of list) {
    for (const job of snap.jobs) {
      if (isActiveJob(job) || job.state === 'paused') entries.push({ job, conversationId: snap.conversation.id, label: conversationLabel(snap), waiting: 0 });
    }
  }
  entries.sort((a, b) => a.job.created_at.localeCompare(b.job.created_at));
  let n = 0;
  for (const e of entries) {
    if (e.job.state === 'queued') {
      n += 1;
      e.waiting = e.job.queue_position > 0 ? e.job.queue_position : n;
    }
  }
  return entries;
}

export function jobStatusLabel(job: LocalImageJob): string {
  if (job.state === 'queued') return job.queue_position > 0 ? `Queued · #${job.queue_position}` : 'Queued';
  if (job.state === 'paused') return 'Paused';
  if (job.state === 'cancelling') return 'Cancelling';
  if (isActiveJob(job)) return job.progress ? `${job.stage} · ${Math.round(job.progress * 100)}%` : job.stage;
  if (job.state === 'succeeded') return 'Done';
  if (job.state === 'cancelled') return 'Cancelled';
  return job.state === 'interrupted' ? 'Interrupted' : 'Failed';
}

export interface InstallView {
  fraction: number | null;
  text: string;
  failed: boolean;
  error: string | null;
}

/**
 * Install progress for the panel. Prefer the live `local-image://install` event;
 * fall back to component states so browser mode and a missed event still move.
 */
export function installView(profile: LocalImageProfile, progress: LocalImageInstallProgress | null | undefined): InstallView {
  const failedComponent = profile.components.some((c) => c.state === 'failed');
  const failed = progress?.state === 'failed' || profile.install_state === 'failed' || failedComponent;
  const error = progress?.error ?? (failed ? 'The last install attempt failed. Install again to retry.' : null);
  const required = profile.components.filter((c) => c.required);
  const totalBytes = required.reduce((n, c) => n + (c.bytes ?? 0), 0) || profile.total_bytes;
  const doneBytes = required.filter((c) => c.state === 'installed').reduce((n, c) => n + (c.bytes ?? 0), 0);
  const live = progress && progress.bytes_total > 0 && progress.state === 'downloading' ? progress.bytes_done : 0;
  const fraction = totalBytes > 0 ? Math.min(1, (doneBytes + live) / totalBytes) : null;
  let text = '';
  if (progress && progress.state !== 'installed' && progress.state !== 'failed') text = `${progress.state} ${progress.file}`.trim();
  else if (profile.install_state === 'installing') {
    const current = required.find((c) => c.state !== 'installed');
    text = current ? `${current.state === 'verifying' ? 'verifying' : 'downloading'} ${current.file}` : 'finishing';
  }
  return { fraction, text, failed, error };
}

export function runtimeNeedsWarning(status: LocalImageRuntimeStatus): boolean {
  return status.state !== 'ready' && !(status.state === 'stopped' && !status.last_error) && !(status.state === 'starting' && !status.last_error);
}

export function runtimeLabel(status: LocalImageRuntimeStatus | null): string {
  if (!status) return 'Loading';
  if (status.state === 'stopped' && !status.last_error) return 'Idle';
  return status.state.replace(/_/g, ' ');
}

export function formatMemoryMb(mb: number): string {
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`;
}

export function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}
