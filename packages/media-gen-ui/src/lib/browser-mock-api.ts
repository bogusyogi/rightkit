import { getPlatform } from '@rightkit/platform-ui';
import type {
  GalleryArtifact,
  LocalImageArtifact,
  LocalImageConversation,
  LocalImageJob,
  LocalImageJobState,
  LocalImageJobEvent,
  LocalImageMessage,
  LocalImageProfile,
  LocalImageReference,
  LocalImageRuntimeStatus,
  LocalImageSubmitRequest
} from './types.js';
// The mock reports the host as Windows or macOS only, matching the two shipped targets.
const mockIsWindows = (): boolean => getPlatform() === 'windows';

// The engine's enforcement statement, verbatim from rental.rs (HONEST_LIMITS). A copy: lib does not import views,
// and src/tests/rentals-logic.test.mjs asserts it equals RENTAL_ENFORCEMENT_TEXT in src/views/rentals-logic.ts.
const RENTAL_ENFORCEMENT_TEXT =
  "enforcement runs only while App runs: at engine sidecar start, every 60 s while the app is open, at the start of each CLI command that touches workers or generation, and on studio.models.rentals_enforce. When neither the app nor a CLI command is running nothing enforces any limit, and the instance keeps billing until the next engine start or until you destroy it at the provider. Destroying needs the Vast API key saved in App (a rental is only created while one is saved) or VAST_API_KEY in the enforcing process. The real fix is an instance-side maximum lifetime or auto-stop in the create request; it needs the provider's documented field name from a fetched source, which has not been verified, so none is sent";

function unsupportedMock(target: string): never {
  throw new Error(`Unsupported browser QA mock: ${target}`);
}

function stableHash(value: string) {
  let hash = 0x811c9dc5;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
}

const LOCAL_IMAGE_MOCK_PROFILE: LocalImageProfile = {
  id: 'browser-qa-fake',
  revision: 1,
  label: 'Browser QA fake',
  role: 'text_to_image',
  description: 'Deterministic browser QA profile backed by no real model.',
  license: { name: 'QA fixture', url: 'https://example.invalid/qa-license', commercial: true },
  tasks: ['text_to_image', 'image_edit'],
  dimension_multiple: 32,
  defaults: {
    sample_steps: 20,
    sample_method: 'euler',
    scheduler: 'simple',
    txt_cfg: 3,
    negative_prompt: '',
    width: 1024,
    height: 1024
  },
  components: [],
  install_state: 'installed',
  qualification: { cpu: 'mock' },
  total_bytes: 0
};

/** The four packaged profiles (assets/local-image/profiles.json), sizes and defaults as shipped. */
function mockPackagedProfile(id: string, label: string, description: string, license: string, defaults: Partial<LocalImageProfile['defaults']>, files: Array<[string, string, number]>, installed: boolean): LocalImageProfile {
  return {
    ...LOCAL_IMAGE_MOCK_PROFILE,
    id,
    label,
    description,
    license: { name: license, url: 'https://example.invalid/license', commercial: true },
    defaults: { ...LOCAL_IMAGE_MOCK_PROFILE.defaults, sample_steps: 20, sample_method: 'euler', scheduler: null, txt_cfg: 6, width: 512, height: 512, ...defaults },
    components: files.map(([kind, file, bytes]) => ({ kind, file, bytes, sha256: null, state: installed ? 'installed' as const : 'missing' as const, required: true })),
    install_state: installed ? 'installed' : 'not_installed',
    qualification: {},
    total_bytes: files.reduce((n, f) => n + f[2], 0)
  };
}
const MOCK_ENCODER: [string, string, number] = ['encoder', 'Qwen3VL-8B-Instruct-Q4_K_M.gguf', 5_027_784_800];
const MOCK_MMPROJ: [string, string, number] = ['mmproj', 'mmproj-Qwen3VL-8B-Instruct-F16.gguf', 1_159_029_824];
const MOCK_VAE: [string, string, number] = ['vae', 'qwen_image_2.1_vae_bf16.safetensors', 675_509_688];
const LOCAL_IMAGE_MOCK_PACKAGED: LocalImageProfile[] = [
  mockPackagedProfile('qwen21-base-q6', 'Qwen-Image 2.1 base Q6_K', 'Compatibility and quality baseline. Text-to-image and image edit.', 'Apache-2.0', {}, [['transformer', 'qwen_image_2.1-Q6_K.gguf', 5_996_851_232], MOCK_ENCODER, MOCK_MMPROJ, MOCK_VAE], true),
  mockPackagedProfile('qwen21-uc-q6', 'abenzerps UC Q6_K', 'Optional GGUF comparison on the same encoder and VAE.', 'Apache-2.0', {}, [['transformer', 'qwen-image-2.1-UC-Q6_K.gguf', 5_876_556_576], MOCK_ENCODER, MOCK_MMPROJ, MOCK_VAE], false),
  mockPackagedProfile('noct-q-v4-int8', 'Noct Q V4 (INT8 ConvRot)', 'Preferred realism candidate. Not available on this Metal runtime.', 'Noct community licence', { sample_steps: 25, scheduler: 'simple', txt_cfg: 3, width: 1024, height: 1536, negative_prompt: 'artifacts, low quality, blurry, missing fingers' }, [['transformer', 'NoctQ_V4_int8_convrot.safetensors', 7_256_784_368], ['encoder', 'qwen3vl_8b_int8_convrot.safetensors', 9_350_798_360], MOCK_VAE, MOCK_MMPROJ], false),
  mockPackagedProfile('noct-q-v4-metal-int8', 'Noct Q V4 INT8 (Metal comparison)', 'Matched Metal comparison for the Noct transformer.', 'Noct community licence', {}, [['transformer', 'NoctQ_V4_int8_convrot.safetensors', 7_256_784_368], MOCK_ENCODER, MOCK_MMPROJ, MOCK_VAE], false)
];
const LOCAL_IMAGE_MOCK_PROFILES: LocalImageProfile[] = [LOCAL_IMAGE_MOCK_PROFILE, ...LOCAL_IMAGE_MOCK_PACKAGED];

function localImageQaMode() {
  return typeof window !== 'undefined' ? new URLSearchParams(window.location.search).get('qa_local_image') : null;
}

function localImageStepDelay() {
  return localImageQaMode() === 'slow' ? 2500 : 300;
}

interface LocalImageMockStore {
  conversations: LocalImageConversation[];
  messages: LocalImageMessage[];
  jobs: LocalImageJob[];
  events: Record<string, LocalImageJobEvent[]>;
  artifacts: Record<string, LocalImageArtifact[]>;
  references: LocalImageReference[];
  requests: Record<string, { jobId: string; payload: string; conversationId: string }>;
  sequence: number;
}

const localImageStore: LocalImageMockStore = {
  conversations: [],
  messages: [],
  jobs: [],
  events: {},
  artifacts: {},
  references: [],
  requests: {},
  sequence: 0
};

function localImageId(prefix: string) {
  localImageStore.sequence += 1;
  return `${prefix}-${localImageStore.sequence}`;
}

function localImageNow() {
  return new Date().toISOString();
}

function findLocalImageJob(jobId: string) {
  return localImageStore.jobs.find((job) => job.id === jobId);
}

function localImageError(code: string, message: string): never {
  throw new Error(`${code}: ${message}`);
}

function resolvedMockSeed(seed: number | null | undefined) {
  if (seed !== undefined && seed !== null && seed !== -1) {
    if (!Number.isInteger(seed) || seed < 0 || seed > 2_147_483_647) {
      localImageError('invalid_request', 'seed must be a nonnegative 32-bit integer or -1');
    }
    return seed;
  }
  const value = new Uint32Array(1);
  globalThis.crypto.getRandomValues(value);
  return value[0] & 0x7fffffff;
}

function recordLocalImageEvent(job: LocalImageJob, state: LocalImageJobState, stage: string, progress: number | null, errorCode: string | null = null) {
  const now = localImageNow();
  job.state = state;
  job.stage = stage;
  job.progress = progress;
  job.error_code = errorCode;
  job.updated_at = now;
  if (['succeeded', 'failed', 'cancelled', 'interrupted'].includes(state)) {
    job.finished_at = now;
  }
  const events = localImageStore.events[job.id] || [];
  events.push({
    job_id: job.id,
    revision: events.length + 1,
    state,
    stage,
    progress,
    runtime_session_id: job.runtime_session_id,
    error_code: errorCode,
    at: now
  });
  localImageStore.events[job.id] = events;
}

function progressMockLocalImage(jobId: string) {
  const job = findLocalImageJob(jobId);
  if (!job || !['queued', 'loading', 'submitting', 'running', 'finalizing'].includes(job.state)) {
    return;
  }
  const transitions: Partial<Record<LocalImageJobState, [LocalImageJobState, string, number]>> = {
    queued: ['loading', 'loading', 0.05],
    loading: ['submitting', 'submitting', 0.15],
    submitting: ['running', 'running', 0.25],
    running: ['finalizing', 'finalizing', 0.95]
  };
  const next = transitions[job.state];
  if (next) {
    recordLocalImageEvent(job, next[0], next[1], next[2]);
    globalThis.setTimeout(() => progressMockLocalImage(jobId), localImageStepDelay());
    return;
  }

  recordLocalImageEvent(job, 'succeeded', 'done', 1);
  localImageStore.artifacts[job.id] = [{
    id: localImageId('artifact'),
    job_id: job.id,
    path: `browser-qa-mock://local-image/${job.id}.png`,
    sidecar_path: null,
    manifest_path: null,
    sha256: null,
    bytes: 0,
    width: job.width,
    height: job.height,
    created_at: job.updated_at
  }];
}

function isPausedLifecycleQa() {
  return localImageQaMode() === 'paused';
}

function snapshotLocalImageConversation(conversationId: string) {
  const conversation = localImageStore.conversations.find((entry) => entry.id === conversationId);
  if (!conversation) {
    return null;
  }
  const jobs = localImageStore.jobs.filter((job) => job.conversation_id === conversationId);
  const artifacts: Record<string, LocalImageArtifact[]> = {};
  for (const job of jobs) {
    artifacts[job.id] = localImageStore.artifacts[job.id] || [];
  }
  return {
    conversation,
    messages: localImageStore.messages.filter((message) => message.conversation_id === conversationId),
    jobs,
    artifacts
  };
}

function validateMockOverrides(request: LocalImageSubmitRequest) {
  const overrides = (request.overrides ?? {}) as Record<string, unknown>;
  for (const [key, value] of Object.entries(overrides)) {
    if (!['sample_steps', 'sample_method', 'scheduler', 'txt_cfg', 'negative_prompt'].includes(key)) {
      localImageError('invalid_request', `unknown override ${key}`);
    }
    if (key === 'sample_steps' && !(Number.isInteger(value) && (value as number) >= 1 && (value as number) <= 100)) {
      localImageError('invalid_request', 'invalid sampling settings');
    }
    if (key === 'txt_cfg' && !(typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 50)) {
      localImageError('invalid_request', 'invalid sampling settings');
    }
    if (key === 'sample_method' && !(typeof value === 'string' && value)) {
      localImageError('invalid_request', 'invalid sampling settings');
    }
  }
}

function submitLocalImageMock(request: LocalImageSubmitRequest) {
  if (!request || typeof request !== 'object') {
    localImageError('invalid_request', 'request body is required');
  }
  if (!request.request_id?.trim()) {
    localImageError('invalid_request', 'request_id is required');
  }
  if (!request.prompt?.trim()) {
    localImageError('invalid_request', 'prompt is required');
  }
  const mockProfile = LOCAL_IMAGE_MOCK_PROFILES.find((entry) => entry.id === request.profile_id);
  if (!mockProfile) {
    localImageError('unknown_profile', request.profile_id);
  }
  if (mockProfile.install_state !== 'installed') {
    localImageError('profile_not_installed', request.profile_id);
  }
  validateMockOverrides(request);
  if (request.width < 64 || request.height < 64 || request.width > 4096 || request.height > 4096
      || request.width % 32 !== 0 || request.height % 32 !== 0) {
    localImageError('invalid_dimensions', `${request.width}x${request.height} must be within 64..4096 and multiples of 32`);
  }
  if (!Array.isArray(request.reference_ids)) {
    localImageError('invalid_request', 'reference_ids must be an array');
  }
  if (request.reference_ids.length > 1) {
    localImageError('invalid_request', 'at most one reference per edit');
  }
  const concreteSeed = resolvedMockSeed(request.seed);
  const existing = localImageStore.requests[request.request_id];
  const conversationId = request.conversation_id || existing?.conversationId || '';
  const negativePrompt = request.negative_prompt ?? request.overrides?.negative_prompt ?? '';
  const requestShape = {
    conversation_id: conversationId,
    parent_message_id: request.parent_message_id ?? null,
    profile_id: request.profile_id,
    profile_revision: 1,
    prompt: request.prompt,
    negative_prompt: negativePrompt,
    width: request.width,
    height: request.height,
    seed: request.seed === undefined || request.seed === null ? -1 : request.seed,
    reference_ids: request.reference_ids,
    overrides: request.overrides ?? {}
  };
  let payload = JSON.stringify(requestShape);
  if (existing) {
    if (existing.payload !== payload) {
      localImageError('request_id_conflict', request.request_id);
    }
    const job = findLocalImageJob(existing.jobId);
    if (!job) {
      throw new Error(`Local image job missing for request ${request.request_id}`);
    }
    return { job };
  }

  for (const referenceId of request.reference_ids) {
    const reference = localImageStore.references.find((entry) => entry.id === referenceId);
    if (!reference || !reference.stored_path.startsWith('browser-qa-mock://ref/')
        || !['image/png', 'image/jpeg', 'image/webp'].includes(reference.mime)
        || !reference.sha256 || reference.bytes <= 0 || reference.width <= 0 || reference.height <= 0) {
      localImageError('invalid_request', `missing or corrupt reference: ${referenceId}`);
    }
  }

  const now = localImageNow();
  let resolvedConversationId = request.conversation_id;
  if (resolvedConversationId && !localImageStore.conversations.some((entry) => entry.id === resolvedConversationId)) {
    localImageError('unknown_conversation', resolvedConversationId);
  }
  if (!resolvedConversationId) {
    resolvedConversationId = localImageId('conversation');
    localImageStore.conversations.push({
      id: resolvedConversationId,
      title: request.prompt.slice(0, 64) || 'Local image',
      created_at: now
    });
  }
  requestShape.conversation_id = resolvedConversationId;
  payload = JSON.stringify(requestShape);

  const userMessage: LocalImageMessage = {
    id: localImageId('message'),
    conversation_id: resolvedConversationId,
    parent_id: request.parent_message_id ?? null,
    role: 'user',
    kind: 'prompt',
    prompt: request.prompt,
    reference_ids: request.reference_ids,
    job_id: null,
    created_at: now
  };
  localImageStore.messages.push(userMessage);

  const assistantMessage: LocalImageMessage = {
    id: localImageId('message'),
    conversation_id: resolvedConversationId,
    parent_id: userMessage.id,
    role: 'assistant',
    kind: 'result',
    prompt: null,
    reference_ids: [],
    job_id: null,
    created_at: now
  };

  const job: LocalImageJob = {
    id: localImageId('job'),
    request_id: request.request_id,
    conversation_id: resolvedConversationId,
    message_id: assistantMessage.id,
    parent_message_id: request.parent_message_id ?? null,
    profile_id: request.profile_id,
    profile_revision: 1,
    prompt: request.prompt,
    negative_prompt: negativePrompt,
    width: request.width,
    height: request.height,
    seed: concreteSeed,
    reference_ids: request.reference_ids,
    overrides: request.overrides ?? {},
    state: isPausedLifecycleQa() ? 'paused' : 'queued',
    stage: isPausedLifecycleQa() ? 'paused' : 'queued',
    progress: null,
    queue_position: 0,
    provider_job_id: `browser-qa-provider-${localImageStore.sequence}`,
    runtime_session_id: 'mock',
    error_code: null,
    error_message: null,
    retry_of: null,
    created_at: now,
    updated_at: now,
    finished_at: null
  };
  assistantMessage.job_id = job.id;
  localImageStore.messages.push(assistantMessage);
  localImageStore.jobs.push(job);

  localImageStore.events[job.id] = [];
  recordLocalImageEvent(job, job.state, job.stage, job.progress);
  localImageStore.artifacts[job.id] = [];
  localImageStore.requests[request.request_id] = { jobId: job.id, payload, conversationId: resolvedConversationId };
  if (job.state === 'queued') {
    globalThis.setTimeout(() => progressMockLocalImage(job.id), localImageStepDelay());
  }
  return { job };
}

let localImageMockMemoryMode = 'performance';
let localImageMockIdleSeconds = 300;

function localImageMockRuntime(): LocalImageRuntimeStatus {
  // Mock machine: 24 GB, so `auto` resolves to balanced. Peaks are estimates (as in the engine).
  const peaks: Record<string, number> = { performance: 10.3, balanced: 8, low: 5.5 };
  const resolved = (localImageMockMemoryMode === 'auto' ? 'balanced' : localImageMockMemoryMode) as 'performance' | 'balanced' | 'low';
  return {
    state: 'ready',
    pid: 0,
    session_id: 'mock',
    profile_id: LOCAL_IMAGE_MOCK_PROFILE.id,
    port: 0,
    capabilities: { mock: true },
    last_error: null,
    backend_binary: 'browser-qa-mock',
    backend_present: true,
    loaded: true,
    memory_mb: peaks[resolved] * 1024,
    idle_unload_seconds: localImageMockIdleSeconds,
    memory: {
      requested: localImageMockMemoryMode,
      resolved,
      total_gb: 24,
      estimated_peak_gb: peaks[resolved],
      estimated_peaks_gb: peaks,
      running: resolved
    }
  };
}

export async function mockLocalImage<T>(command: string, args: Record<string, unknown>): Promise<T> {
  if (command === 'local_image.profiles') {
    return { profiles: LOCAL_IMAGE_MOCK_PROFILES.map((entry) => ({ ...entry, components: entry.components.map((component) => ({ ...component })) })) } as T;
  }
  if (command === 'local_image.runtime') {
    return localImageMockRuntime() as T;
  }
  if (command === 'local_image.set_idle_unload') {
    localImageMockIdleSeconds = Number(args.seconds ?? 300);
    return localImageMockRuntime() as T;
  }
  if (command === 'local_image.set_memory_mode') {
    localImageMockMemoryMode = String(args.mode ?? 'performance');
    return localImageMockRuntime() as T;
  }
  if (command === 'local_image.submit') {
    return submitLocalImageMock(args as unknown as LocalImageSubmitRequest) as T;
  }
  if (command === 'local_image.get') {
    const job = findLocalImageJob(String(args.job_id || ''));
    if (!job) {
      throw new Error(`Local image job not found: ${String(args.job_id)}`);
    }
    return {
      job,
      events: localImageStore.events[job.id] || [],
      artifacts: localImageStore.artifacts[job.id] || [],
      message: localImageStore.messages.find((message) => message.job_id === job.id) || null
    } as T;
  }
  if (command === 'local_image.list') {
    const conversationId = args.conversation_id ? String(args.conversation_id) : null;
    const limit = typeof args.limit === 'number' ? args.limit : null;
    const snapshots = localImageStore.conversations
      .filter((conversation) => !conversationId || conversation.id === conversationId)
      .map((conversation) => snapshotLocalImageConversation(conversation.id))
      .filter((snapshot): snapshot is NonNullable<typeof snapshot> => Boolean(snapshot));
    return { conversations: limit ? snapshots.slice(0, limit) : snapshots } as T;
  }
  if (command === 'local_image.cancel') {
    const job = findLocalImageJob(String(args.job_id || ''));
    if (!job) {
      throw new Error(`Local image job not found: ${String(args.job_id)}`);
    }
    if (job.state === 'queued') {
      recordLocalImageEvent(job, 'cancelled', 'cancelled', null, 'user_cancel');
    } else if (['loading', 'submitting', 'running'].includes(job.state)) {
      recordLocalImageEvent(job, 'cancelling', 'cancelling', job.progress, 'user_cancel');
      globalThis.setTimeout(() => {
        if (job.state === 'cancelling') {
          recordLocalImageEvent(job, 'cancelled', 'cancelled', null, 'user_cancel');
        }
      }, 150);
    } else if (job.state === 'paused') {
      recordLocalImageEvent(job, 'cancelled', 'cancelled', null, 'user_cancel');
    }
    return { job } as T;
  }
  if (command === 'local_image.resume') {
    const job = findLocalImageJob(String(args.job_id || ''));
    if (!job) {
      throw new Error(`Local image job not found: ${String(args.job_id)}`);
    }
    if (job.state === 'paused') {
      recordLocalImageEvent(job, 'queued', 'queued', null);
      globalThis.setTimeout(() => progressMockLocalImage(job.id), localImageStepDelay());
    }
    return { job } as T;
  }
  if (command === 'local_image.import_reference') {
    const sourcePath = String(args.path || '');
    const name = sourcePath.split(/[\\/]/).filter(Boolean).pop() || 'reference.png';
    const extension = name.includes('.') ? name.split('.').pop()?.toLowerCase() : '';
    if (!['png', 'jpg', 'jpeg', 'webp'].includes(extension || '')) {
      localImageError('invalid_reference', 'reference must be a PNG, JPEG, or WebP image');
    }
    const mime = extension === 'jpg' || extension === 'jpeg' ? 'image/jpeg' : extension === 'webp' ? 'image/webp' : 'image/png';
    const reference: LocalImageReference = {
      id: localImageId('ref'),
      stored_path: `browser-qa-mock://ref/${name}`,
      sha256: `browser-qa-mock-${localImageStore.sequence}`,
      bytes: 1,
      width: 1,
      height: 1,
      mime,
      created_at: localImageNow()
    };
    localImageStore.references.push(reference);
    return { reference } as T;
  }
  if (command === 'local_image.install') {
    const profile = LOCAL_IMAGE_MOCK_PROFILES.find((entry) => entry.id === String(args.profile_id));
    if (!profile) {
      localImageError('unknown_profile', String(args.profile_id));
    }
    if (profile.install_state === 'not_installed' || profile.install_state === 'failed') {
      profile.install_state = 'installing';
      let index = 0;
      const step = () => {
        if (profile.install_state !== 'installing') return; // cancelled
        const component = profile.components[index];
        if (!component) {
          profile.install_state = 'installed';
          return;
        }
        component.state = 'verifying';
        globalThis.setTimeout(() => {
          component.state = 'installed';
          index += 1;
          step();
        }, localImageQaMode() === 'slow' ? 2500 : 700);
      };
      step();
    }
    return { started: true } as T;
  }
  if (command === 'local_image.install.pause') {
    const profile = LOCAL_IMAGE_MOCK_PROFILES.find((entry) => entry.id === String(args.profile_id));
    const running = profile?.install_state === 'installing';
    if (profile && running) {
      // Pause keeps what is already installed; the rest resumes on the next install.
      profile.install_state = 'not_installed';
      for (const component of profile.components) {
        if (component.state !== 'installed') component.state = 'missing';
      }
    }
    return { paused: running } as T;
  }
  if (command === 'local_image.install.cancel') {
    const profile = LOCAL_IMAGE_MOCK_PROFILES.find((entry) => entry.id === String(args.profile_id));
    const running = profile?.install_state === 'installing';
    if (profile && running) {
      profile.install_state = 'not_installed';
      for (const component of profile.components) {
        if (component.state !== 'installed') component.state = 'missing';
      }
    }
    return { cancelled: running } as T;
  }
  throw unsupportedMock(`local image command ${command}`);
}

/* ---------- Engine mock used by browser mode (no Tauri) ---------- */

const SKIES = [['#0f172a', '#134e4a', '#22d3ee'], ['#1e1b4b', '#6d28d9', '#f0abfc'], ['#082f49', '#0e7490', '#a7f3d0'], ['#3b0764', '#be185d', '#fdba74']];

/** Deterministic scene art as a data URL, so browser QA shows real pixels. */
export function mockScene(seed: number): string {
  let r = seed * 7919 + 13;
  const rnd = () => ((r = (r * 9301 + 49297) % 233280) / 233280);
  const s = SKIES[Math.abs(seed) % SKIES.length];
  const h1 = 60 + rnd() * 10;
  const h2 = 70 + rnd() * 8;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" preserveAspectRatio="xMidYMid slice"><defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${s[0]}"/><stop offset=".6" stop-color="${s[1]}"/></linearGradient></defs><rect width="100" height="100" fill="url(#g)"/><path d="M0 ${30 + rnd() * 10} C30 ${15 + rnd() * 10},60 ${40 + rnd() * 10},100 ${20 + rnd() * 10}" stroke="${s[2]}" stroke-width="5" fill="none" opacity=".45"/><path d="M0 ${h1} L25 ${h1 - 12} L45 ${h1 - 4} L70 ${h1 - 18} L100 ${h1 - 6} V100 H0z" fill="#1e293b" opacity=".85"/><path d="M0 ${h2} Q50 ${h2 - 6} 100 ${h2 + 2} V100 H0z" fill="#e2e8f0"/><g transform="translate(${38 + rnd() * 14} ${h2 - 14})"><path d="M0 12 V4 L8 -2 L16 4 V12z" fill="#fde68a" stroke="#fff" stroke-width=".6" opacity=".95"/></g></svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

/** Mirrors `genright-engine models` (crates/genright-gen/src/providers/*.rs); labels and est_usd are the Rust values. */
const MOCK_MODELS: Array<Record<string, unknown> & { provider: string; model: string; kind: string; est_usd: number }> = [
  { provider: 'fal', model: 'fal-ai/flux-2/klein/9b', kind: 'image', label: 'FLUX.2 klein 9B (image, ~$0.006/MP)', est_usd: 0.0065 },
  { provider: 'fal', model: 'fal-ai/veo3.1/fast', kind: 'video', label: 'Veo 3.1 Fast text-to-video (4 s, no audio, $0.10/s)', est_usd: 0.4, durations_s: [4, 6, 8], default_duration_s: 4 },
  { provider: 'fal', model: 'fal-ai/veo3.1/fast/image-to-video', kind: 'video', label: 'Veo 3.1 Fast image-to-video (4 s, no audio, $0.10/s)', est_usd: 0.4, durations_s: [4, 6, 8], default_duration_s: 4 },
  { provider: 'nanobanana', model: 'nano-banana-2', kind: 'image', label: 'Nano Banana 2 (image + edit, 2K default, $0.04/$0.06/$0.09 per 1K/2K/4K)', est_usd: 0.06, references: { param: 'image_urls', local_upload: 'fal_storage' } },
  { provider: 'evolink', model: 'gpt-image-2', kind: 'image', label: 'GPT Image 2 (text rendering; medium ~$0.022, high ~$0.14 at 1K)', est_usd: 0.022, references: { param: 'image_urls', max: 16, local_upload: 'none' } },
  { provider: 'wavespeed', model: 'kling-3-pro', kind: 'video', label: 'Kling 3.0 Pro (3-15 s, first+last frame, $0.112/s)', est_usd: 0.56, min_duration_s: 3, max_duration_s: 15, default_duration_s: 5 },
  { provider: 'wavespeed', model: 'seedance-2-fast', kind: 'video', label: 'Seedance 2.0 Fast (4-15 s, first+last frame, $0.10-0.50/s by resolution)', est_usd: 0.9, min_duration_s: 4, max_duration_s: 15, default_duration_s: 5 },
  { provider: 'wavespeed', model: 'wan-ultra', kind: 'video', label: 'Wan 2.2 480p ultra-fast (5 or 8 s, $0.01/s)', est_usd: 0.05, durations_s: [5, 8], default_duration_s: 5 },
  { provider: 'wavespeed', model: 'wan-hd', kind: 'video', label: 'Wan 2.2 720p ultra-fast (5 or 8 s, $0.02/s)', est_usd: 0.1, durations_s: [5, 8], default_duration_s: 5 },
  { provider: 'wavespeed', model: 'qwen-image', kind: 'image', label: 'Qwen Image (painterly, $0.02/image)', est_usd: 0.02 },
  { provider: 'wavespeed', model: 'seedream-v4', kind: 'image', label: 'Seedream v4 ($0.027/image)', est_usd: 0.027 },
  { provider: 'wavespeed', model: 'recraft-v4', kind: 'image', label: 'Recraft v4 ($0.04/image)', est_usd: 0.04 },
  { provider: 'wavespeed', model: 'elevenlabs-turbo-v2.5', kind: 'voice', label: 'ElevenLabs Turbo v2.5 TTS ($0.10/1000 chars)', est_usd: 0.01 },
  { provider: 'wavespeed', model: 'minimax-speech-02-hd', kind: 'voice', label: 'MiniMax Speech-02 HD TTS ($0.10/1000 chars)', est_usd: 0.01 },
  { provider: 'wavespeed', model: 'infinitetalk', kind: 'avatar', label: 'InfiniteTalk talking avatar (image + audio, $0.03/s 480p, $0.06/s 720p, 3 s min)', est_usd: 0.3, min_duration_s: 3, max_duration_s: 600 },
  { provider: 'elevenlabs', model: 'eleven_v4_turbo', kind: 'voice', label: 'Eleven v4 Turbo (draft default) - $0.04/1K chars', est_usd: 0.02 },
  { provider: 'elevenlabs', model: 'eleven_v4', kind: 'voice', label: 'Eleven v4 (final) - $0.08/1K chars', est_usd: 0.04 },
  { provider: 'heygen', model: 'avatar-standard', kind: 'avatar', label: 'HeyGen standard avatar (Avatar III, ~$1/min)', est_usd: 0.5 },
  { provider: 'heygen', model: 'avatar-iv', kind: 'avatar', label: 'HeyGen Avatar IV (~$3-4/min)', est_usd: 2 },
  { provider: 'wavespeed', model: 'lipsync-3', kind: 'lipsync', label: 'Sync Lipsync-3 (video + audio, params.sync_mode)', est_usd: 0.05 },
  { provider: 'elevenlabs', model: 'music_v1', kind: 'music', label: 'ElevenLabs Music text-to-music (params.music_length_ms or composition_plan)', est_usd: 0.15 },
  { provider: 'fal', model: 'fal-ai/stable-audio', kind: 'music', label: 'Stable Audio text-to-music', est_usd: 0.05 },
  { provider: 'elevenlabs', model: 'eleven_text_to_sound_v2', kind: 'sfx', label: 'ElevenLabs text-to-sound (params.duration_seconds, loop, prompt_influence)', est_usd: 0.12 },
  { provider: 'fal', model: 'fal-ai/mmaudio/v2', kind: 'sfx', label: 'MMAudio video-to-audio (synchronized sound from params.video_url)', est_usd: 0.05 },
  { provider: 'realesrgan', model: 'realesrgan-x4plus', kind: 'upscale', label: 'Real-ESRGAN x4 general (local ncnn Vulkan, free, needs the official release binary)', est_usd: 0 },
  { provider: 'fal', model: 'fal-ai/creative-upscaler', kind: 'upscale', label: 'Creative Upscaler (image, needs params.image_url)', est_usd: 0.03 },
  { provider: 'fal', model: 'fal-ai/topaz/upscale/video', kind: 'upscale', label: 'Topaz video upscale (needs params.video_url)', est_usd: 0.5 },
  { provider: 'local-qts', model: 'qwen3-tts-0.6b-q8_0', kind: 'voice', label: 'Qwen3-TTS 0.6B (local, free) - x-vector cloning; Apple Silicon (Metal) or Vulkan GPU, 8 GB+ RAM; 1.8 GB download', est_usd: 0 }
];

/** Keyless providers: generation needs no stored key, status source is 'local'. */
const KEYLESS_PROVIDERS = new Set(['local-qts', 'realesrgan']);

/**
 * Mirrors the engine's per-model facts (crates/genright-gen/src/capabilities.rs and the adapters' `capabilities`)
 * for the models listed above. Keyed `provider/model`; a model absent here has the generic image/video/voice shape.
 */
const VEO_FACTS = { aspect: { param: 'aspect', values: ['16:9', '9:16'] }, resolution: { param: 'resolution', values: ['720p', '1080p'], required: false, default: '720p' }, duration: { param: 'duration' } };
const url = (param: string, required = true) => ({ param, accepts: 'url', required });
const httpsUrl = (param: string, required = true) => ({ param, accepts: 'https_url', required });
const MOCK_FACTS: Record<string, { inputs: Record<string, unknown>; settings: Record<string, unknown>; references?: unknown }> = {
  'fal/fal-ai/flux-2/klein/9b': { inputs: { mode: 'prompt' }, settings: { aspect: { param: 'aspect', values: ['1:1', '16:9', '9:16', '4:3', '3:4'] }, seed: { param: 'seed' } } },
  'fal/fal-ai/veo3.1/fast': { inputs: { mode: 'prompt', start_image: 'optional', start_image_param: 'image_url', end_image: 'none' }, settings: VEO_FACTS },
  'fal/fal-ai/veo3.1/fast/image-to-video': { inputs: { mode: 'image', start_image: 'required', start_image_param: 'image_url', end_image: 'none' }, settings: VEO_FACTS },
  'nanobanana/nano-banana-2': {
    inputs: { mode: 'prompt' },
    settings: {
      aspect: { param: 'aspect', values: ['1:1', '16:9', '9:16', '4:3', '3:4', '3:2', '2:3', '21:9', '4:5', '5:4', '1:4', '4:1', '1:8', '8:1', 'auto'] },
      resolution: { param: 'resolution', values: ['1K', '2K', '4K'], required: false, default: '2K' }
    }
  },
  'evolink/gpt-image-2': {
    inputs: { mode: 'prompt' },
    settings: { aspect: { param: 'aspect', values: ['1:1', '16:9', '9:16', '4:3'], open: true }, resolution: { param: 'resolution', values: ['1K', '2K', '4K'], required: false, default: '1K' } }
  },
  'wavespeed/kling-3-pro': { inputs: { mode: 'prompt', start_image: 'optional', start_image_param: 'image_url', end_image: 'optional', end_image_param: 'end_image_url' }, settings: { duration: { param: 'duration' }, aspect: { param: 'aspect', values: ['16:9', '9:16'], open: true } } },
  'wavespeed/seedance-2-fast': {
    inputs: { mode: 'prompt', start_image: 'optional', start_image_param: 'image_url', end_image: 'optional', end_image_param: 'end_image_url' },
    settings: { duration: { param: 'duration' }, aspect: { param: 'aspect', values: ['16:9', '9:16'], open: true }, resolution: { param: 'resolution', values: ['480p', '720p', '1080p'], required: true, default: null }, seed: { param: 'seed' } }
  },
  'wavespeed/wan-ultra': { inputs: { mode: 'prompt', start_image: 'optional', start_image_param: 'image_url', end_image: 'none' }, settings: { duration: { param: 'duration' }, aspect: { param: 'aspect', values: ['16:9', '9:16'], open: true }, seed: { param: 'seed' } } },
  'wavespeed/wan-hd': { inputs: { mode: 'prompt', start_image: 'optional', start_image_param: 'image_url', end_image: 'none' }, settings: { duration: { param: 'duration' }, aspect: { param: 'aspect', values: ['16:9', '9:16'], open: true }, seed: { param: 'seed' } } },
  'wavespeed/qwen-image': { inputs: { mode: 'prompt' }, settings: { aspect: { param: 'aspect', values: ['1:1', '16:9', '9:16', '3:2', '2:3'] }, seed: { param: 'seed' } } },
  'wavespeed/seedream-v4': { inputs: { mode: 'prompt' }, settings: { aspect: { param: 'aspect', values: ['1:1', '16:9', '9:16', '3:2', '2:3'] }, seed: { param: 'seed' } } },
  'wavespeed/recraft-v4': { inputs: { mode: 'prompt' }, settings: { aspect: { param: 'aspect', values: ['1:1', '16:9', '9:16', '3:2', '2:3'] } } },
  'wavespeed/elevenlabs-turbo-v2.5': { inputs: { mode: 'script', voice_id: { param: 'voice_id' } }, settings: {} },
  'wavespeed/minimax-speech-02-hd': { inputs: { mode: 'script', voice_id: { param: 'voice_id' } }, settings: {} },
  'wavespeed/infinitetalk': {
    inputs: { mode: 'image_and_audio', image: httpsUrl('image_url'), audio: httpsUrl('audio_url') },
    settings: { resolution: { param: 'resolution', values: ['480p', '720p'], required: false, default: '480p' }, seed: { param: 'seed' } }
  },
  'elevenlabs/eleven_v4_turbo': { inputs: { mode: 'script', voice_id: { param: 'voice_id', pattern: '^[A-Za-z0-9_-]+$' } }, settings: {} },
  'elevenlabs/eleven_v4': { inputs: { mode: 'script', voice_id: { param: 'voice_id', pattern: '^[A-Za-z0-9_-]+$' } }, settings: {} },
  'heygen/avatar-standard': { inputs: { mode: 'script_and_avatar_id', avatar_id: { param: 'avatar_id', required: true }, audio: httpsUrl('audio_url', false), voice_id: { param: 'voice_id' } }, settings: {} },
  'heygen/avatar-iv': { inputs: { mode: 'script_and_avatar_id', avatar_id: { param: 'avatar_id', required: true }, audio: httpsUrl('audio_url', false), voice_id: { param: 'voice_id' } }, settings: {} },
  'wavespeed/lipsync-3': {
    inputs: { mode: 'video_and_audio', video: httpsUrl('video_url'), audio: httpsUrl('audio_url') },
    settings: { sync_mode: { param: 'sync_mode', values: ['bounce', 'loop', 'cut_off', 'silence', 'remap'] } }
  },
  'elevenlabs/music_v1': { inputs: { mode: 'prompt' }, settings: { length: { param: 'music_length_ms', unit: 'ms', min: 3000, max: 600000, integer: true } } },
  'fal/fal-ai/stable-audio': { inputs: { mode: 'prompt' }, settings: { length: { param: 'seconds_total', unit: 's', min: 1, max: 600, integer: true } } },
  'elevenlabs/eleven_text_to_sound_v2': { inputs: { mode: 'prompt' }, settings: { length: { param: 'duration_seconds', unit: 's', min: 0.5, max: 30, integer: false }, loop: { param: 'loop' } } },
  'fal/fal-ai/mmaudio/v2': { inputs: { mode: 'video', video: url('video_url') }, settings: {} },
  'realesrgan/realesrgan-x4plus': { inputs: { mode: 'image', image: { param: 'image_path', accepts: 'path', required: true } }, settings: {} },
  'fal/fal-ai/creative-upscaler': { inputs: { mode: 'image', image: url('image_url') }, settings: {} },
  'fal/fal-ai/topaz/upscale/video': { inputs: { mode: 'video', video: url('video_url') }, settings: {} },
  'local-qts/qwen3-tts-0.6b-q8_0': { inputs: { mode: 'script' }, settings: {} }
};
/** Engine default rows: voice has a fixed fallback; avatar is the first registered model when no key is stored. */
const MOCK_DEFAULTS = new Set(['elevenlabs/eleven_v4_turbo', 'wavespeed/infinitetalk']);
const MOCK_MODEL_ROWS = MOCK_MODELS.map((m) => {
  const key = `${m.provider}/${m.model}`;
  const facts = MOCK_FACTS[key];
  const row: Record<string, unknown> = { ...m, needs_key: !KEYLESS_PROVIDERS.has(m.provider), default: MOCK_DEFAULTS.has(key), inputs: facts?.inputs ?? { mode: 'prompt' }, settings: facts?.settings ?? {} };
  if ((m.kind === 'image' || m.kind === 'video') && !('references' in m)) row.references = null;
  return row;
});

const MOCK_PROVIDER_IDS = ['elevenlabs', 'fal', 'heygen', 'nanobanana', 'wavespeed', 'local-qts', 'realesrgan'];

interface MockJob {
  job_id: string;
  kind: string;
  provider: string;
  model: string;
  prompt: string;
  params: Record<string, unknown>;
  est_usd: number;
  created_ms: number;
  seed: number;
  cancelled: boolean;
  /** QA scenario: a job whose runner died — stays non-terminal until job.run. */
  stuck?: boolean;
  /** QA scenario: a paid submit that may have been delivered — held as outcome_unknown until job.resolve. */
  unknown?: boolean;
  abandoned?: boolean;
}

const mockState = {
  configured: new Map<string, 'keychain' | 'environment'>([['fal', 'keychain'], ['wavespeed', 'environment'], ['elevenlabs', 'keychain']]),
  capUsd: 25,
  monthlyCapUsd: null as number | null,
  jobs: [] as MockJob[],
  modelRoutes: {} as Record<string, { route: string; model?: string | null; set_at?: string }>,
  modelWorkers: [] as Array<Record<string, unknown>>,
  modelWorkerSeq: 0,
  modelInstalls: {} as Record<string, Record<string, unknown>>,
  modelInstallTicks: {} as Record<string, number>,
  seq: 0
};
mockState.jobs.push(
  { job_id: 'job-seed-1', kind: 'image', provider: 'fal', model: 'fal-ai/flux-2/klein/9b', prompt: 'A tiny glass greenhouse under the northern lights', params: { aspect: '1:1' }, est_usd: 0.0065, created_ms: Date.now() - 3_600_000, seed: 1, cancelled: false },
  { job_id: 'job-seed-2', kind: 'image', provider: 'nanobanana', model: 'nano-banana-2', prompt: 'Puck cover, forest variant', params: { aspect: '3:4', resolution: '2K' }, est_usd: 0.06, created_ms: Date.now() - 7_200_000, seed: 2, cancelled: false }
);

/** Engine job status values: submitted | running | outcome_unknown | complete | failed | cancelled. */
function mockPhase(job: MockJob) {
  const elapsed = Date.now() - job.created_ms;
  if (job.cancelled) return { status: 'cancelled', stage: 'failed', elapsed };
  if (job.abandoned) return { status: 'failed', stage: 'failed', elapsed };
  if (job.unknown) return { status: 'outcome_unknown', stage: 'outcome_unknown', elapsed };
  if (job.stuck) return { status: 'running', stage: 'polling', elapsed };
  if (/\bfail\b/i.test(job.prompt) && elapsed >= 3000) return { status: 'failed', stage: 'failed', elapsed };
  if (elapsed < 1000) return { status: 'submitted', stage: 'submitted', elapsed };
  if (elapsed < 4000) return { status: 'running', stage: 'polling', elapsed };
  if (elapsed < 5000) return { status: 'running', stage: 'downloading', elapsed };
  return { status: 'complete', stage: 'complete', elapsed };
}

function mockJobRecord(job: MockJob, withProgress: boolean) {
  const phase = mockPhase(job);
  const failed = phase.status === 'failed';
  return {
    job_id: job.job_id,
    request_id: null,
    kind: job.kind,
    provider: job.provider,
    model: job.model,
    request: { kind: job.kind, provider: job.provider, model: job.model, prompt: job.prompt, params: job.params },
    status: phase.status,
    provider_job_id: null,
    attempts: 1,
    est_usd: job.est_usd,
    actual_usd: phase.status === 'complete' ? job.est_usd : null,
    error: job.abandoned
      ? { code: 'outcome_unknown_abandoned', message: 'abandoned by the user; the reservation stays counted as spent' }
      : job.unknown
        ? { code: 'unavailable', message: 'mock provider 503 after the request was sent' }
        : failed ? { code: 'bad_response', message: 'mock provider failure' } : null,
    acked_by: null,
    acked_at: null,
    created_at: new Date(job.created_ms).toISOString(),
    updated_at: new Date().toISOString(),
    ...(job.unknown
      ? { outcome_unknown: { attempt: 1, reserved_usd: job.est_usd, idempotency_key: `${job.job_id}:1`, resolve: ['resubmit', 'abandon'] } }
      : {}),
    ...(withProgress
      ? { progress: { event: 'job.progress', job_id: job.job_id, status: phase.status, stage: phase.stage, attempt: 1, at: new Date().toISOString(), message: phase.stage === 'polling' ? 'provider is working' : null } }
      : {})
  };
}

/** One second of 440 Hz as a WAV data URL, so the mock voice result is playable. */
function mockWav(): string {
  const rate = 8000;
  const n = rate;
  const buf = new Uint8Array(44 + n);
  const dv = new DataView(buf.buffer);
  const w = (o: number, t: string) => [...t].forEach((c, i) => dv.setUint8(o + i, c.charCodeAt(0)));
  w(0, 'RIFF'); dv.setUint32(4, 36 + n, true); w(8, 'WAVE'); w(12, 'fmt ');
  dv.setUint32(16, 16, true); dv.setUint16(20, 1, true); dv.setUint16(22, 1, true);
  dv.setUint32(24, rate, true); dv.setUint32(28, rate, true); dv.setUint16(32, 1, true); dv.setUint16(34, 8, true);
  w(36, 'data'); dv.setUint32(40, n, true);
  for (let i = 0; i < n; i += 1) dv.setUint8(44 + i, 128 + Math.round(40 * Math.sin((2 * Math.PI * 440 * i) / rate)));
  let bin = '';
  buf.forEach((b) => { bin += String.fromCharCode(b); });
  return `data:audio/wav;base64,${btoa(bin)}`;
}

class MockEngineError extends Error {
  code: string;
  constructor(code: string, message: string) {
    super(`${code}: ${message}`);
    this.code = code;
  }
}

/** Same parameter rules as the Rust adapters, so the UI cannot get away with bad values. */
function validateMockParams(model: { provider: string; model: string; kind: string } & Record<string, unknown>, params: Record<string, unknown>, prompt = '') {
  if (model.kind === 'avatar' && model.provider === 'heygen') {
    if (!params.avatar_id) throw new MockEngineError('invalid_request', 'params.avatar_id is required');
    if (params.audio_url && prompt.trim()) throw new MockEngineError('invalid_request', 'use a script or params.audio_url, not both');
    if (!params.audio_url && !prompt.trim()) throw new MockEngineError('invalid_request', 'a script (prompt) or params.audio_url is required');
  }
  if (model.kind === 'avatar' && model.provider === 'wavespeed' && (!params.image_url || !params.audio_url)) {
    throw new MockEngineError('invalid_request', 'params.image_url and params.audio_url are required');
  }
  if (model.provider === 'elevenlabs' && /locked voice/i.test(prompt)) {
    throw new MockEngineError('voice_unavailable', 'This ElevenLabs voice is not available on your plan');
  }
  if (model.kind !== 'video') return;
  const dur = params.duration;
  if (dur != null) {
    const set = model.durations_s as number[] | undefined;
    const lo = model.min_duration_s as number | undefined;
    const hi = model.max_duration_s as number | undefined;
    const ok = typeof dur === 'number' && (set ? set.includes(dur) : lo != null && hi != null && dur >= lo && dur <= hi);
    if (!ok) throw new MockEngineError('invalid_request', `${model.model} does not accept duration ${String(dur)}`);
  }
  if (model.provider === 'fal' && params.resolution != null && !['720p', '1080p'].includes(String(params.resolution))) {
    throw new MockEngineError('invalid_request', `unsupported Veo resolution ${String(params.resolution)} (720p or 1080p)`);
  }
  if (model.model === 'fal-ai/veo3.1/fast/image-to-video' && !params.image_url) {
    throw new MockEngineError('invalid_request', 'image-to-video requires params.image_url');
  }
  if (model.model === 'seedance-2-fast' && !params.resolution) {
    throw new MockEngineError('invalid_request', 'seedance-2-fast requires params.resolution (no silent default)');
  }
  if (model.model.startsWith('wan-') && params.resolution != null) {
    throw new MockEngineError('invalid_request', `${model.model} fixes its resolution`);
  }
}

/** Minimal recipe-library fixture mirroring crates/genright-studio data shape. */
function mockRecipeLibraries() {
  const lib = (id: string, slot: string, subcategory: string, principle: string, items: Array<[string, string, string]>) => ({
    id,
    slot,
    subcategory,
    principle,
    brand_defaults: { DD: items[0]?.[0] ?? null },
    items: items.map(([key, name, fragment]) => ({ key, name, use: name, fragment, tags: [], negative_constraints: ['blurry', 'watermark'] }))
  });
  return {
    libraries: [
      lib('aesthetics', 'aesthetic', 'Aesthetics', 'Lead the prompt with the look.', [
        ['editorial-studio', 'Editorial studio', 'clean editorial studio photograph, soft shadow detail'],
        ['commercial-hero', 'Commercial hero', 'polished commercial product photograph'],
        ['film-still', 'Film still', 'cinematic film still, muted grade']
      ]),
      lib('composition', 'composition', 'Composition', 'Where the subject sits.', [
        ['rule-of-thirds', 'Rule of thirds', 'subject on the left third'],
        ['center-hero', 'Center hero', 'centered hero framing']
      ]),
      lib('lens', 'lens', 'Lens', 'Optics and perspective.', [
        ['50mm', '50mm prime', 'shot on 50mm'],
        ['85mm-portrait', '85mm portrait', 'shot on 85mm, shallow depth of field']
      ]),
      lib('lighting', 'lighting', 'Lighting', 'Light quality and direction.', [
        ['soft-window', 'Soft window', 'soft window light'],
        ['dramatic-rim', 'Dramatic rim', 'dramatic rim lighting']
      ]),
      lib('depth', 'depth', 'Depth', 'Foreground / background separation.', [
        ['shallow-bokeh', 'Shallow bokeh', 'shallow depth of field, bokeh background'],
        ['deep-focus', 'Deep focus', 'deep focus throughout']
      ]),
      lib('color', 'color', 'Color', 'Palette direction.', [
        ['warm-neutrals', 'Warm neutrals', 'warm neutral palette'],
        ['cool-minimal', 'Cool minimal', 'cool minimal palette']
      ]),
      lib('skin', 'skin', 'Skin texture', 'How people render.', [
        ['natural', 'Natural skin', 'natural skin texture'],
        ['retouched', 'Retouched', 'clean retouched skin']
      ]),
      lib('editing', 'edit_mode', 'Editing', 'Edit-mode libraries apply through compose_edit.', [
        ['remove-object', 'Remove object', 'remove the object cleanly'],
        ['relight', 'Relight', 'relight the scene']
      ])
    ],
    models: [
      { slug: 'flux-2-pro', backend: 'fal', negative_compile_strategy: 'positive_rewrite', reference_max: 4, max_prompt_length: 8000, has_contract: true },
      { slug: 'nano-banana-pro', backend: 'nanobanana', negative_compile_strategy: 'natural_language_negative_prompt', reference_max: 8, max_prompt_length: 4000, has_contract: true },
      { slug: 'qwen-image', backend: 'local', negative_compile_strategy: 'natural_language_inline', reference_max: 2, max_prompt_length: 2000, has_contract: false }
    ],
    model_brand_defaults: { DD: 'flux-2-pro', KDP: 'nano-banana-pro' }
  };
}

function mockRecipeCompose(input: Record<string, unknown>): unknown {
  const opts = (input.opts && typeof input.opts === 'object' ? input.opts : input) as Record<string, unknown>;
  const subject = String(opts.subject ?? '').trim();
  if (!subject) return { ok: false, message: 'compose() requires opts.subject', violations: [] };
  const model = String(opts.model || (opts.brand ? 'flux-2-pro' : ''));
  if (!model) return { ok: false, message: 'compose() requires either ctx.model or ctx.brand with a brand default', violations: [] };
  const libs = mockRecipeLibraries().libraries;
  const fragments: string[] = [];
  for (const l of libs) {
    if (l.slot === 'edit_mode') continue;
    const key = String(opts[l.slot] ?? '');
    const item = key ? l.items.find((i) => i.key === key) : l.items[0];
    if (item && (l.slot !== 'skin' || opts.has_humans !== false)) fragments.push(item.fragment);
  }
  const prompt = [fragments[0], subject, ...fragments.slice(1)].filter(Boolean).join(', ');
  return {
    ok: true,
    prompt,
    params: { aspect_ratio: String(opts.aspect ?? '1:1') },
    meta: { schema_version: '1.0.0', model, brand: opts.brand ?? null }
  };
}

const APPROVAL_REQUIRES_APP =
  'Approval is a user action in the App app. Approve this draft in the app; an agent, the plain engine CLI or `operator_approved: true` cannot approve it.';

/**
 * Browser-mode `approve_draft`. The panels keep their own draft stores (image-quality-mock,
 * video-quality-mock), so this validates what the Tauri command validates and then hands the
 * approval to that store, which answers success. Missing ids and hashes are refused like the
 * command refuses them.
 */
export async function mockApproveDraft<T>(kind: string, draftId: string, displayedHash: string, params?: Record<string, unknown>): Promise<T> {
  if (kind !== 'recipes' && kind !== 'video_templates' && kind !== 'recipe_templates' && kind !== 'character_training' && kind !== 'eyes' && kind !== 'blender_finish') throw new MockEngineError('invalid_input', `unknown approval kind "${kind}"`);
  if (!draftId.trim() || !displayedHash.trim()) throw new MockEngineError('invalid_input', 'approve_draft needs the draft id and the hash the user was shown');
  if ((kind === 'character_training' || kind === 'eyes' || kind === 'blender_finish') !== (params !== undefined && params !== null && typeof params === 'object')) {
    throw new MockEngineError('invalid_input', 'approve_draft takes params for character_training, eyes and blender_finish and for no other kind');
  }
  return unsupportedMock('approve_draft');
}

const mockSwapTemplates = new Map<string, { id: string; name: string; kind: string; base: { path?: string; url?: string }; prompt: string; default_model: null; slots: unknown[] }>();

// Dev-only mock store: kept in localStorage so a reload keeps first-run state, as the engine's
// settings file does in the app (hidden browser QA reloads and expects setup remembered).
const MOCK_SETTINGS_KEY = 'genright.mock.settings';
const mockUiSettings: Record<string, unknown> = (() => {
  try { return JSON.parse(localStorage.getItem(MOCK_SETTINGS_KEY) ?? '{}') as Record<string, unknown>; } catch { return {}; }
})();
let mockUiRevision = 0;
function persistMockUiSettings(): void {
  try { localStorage.setItem(MOCK_SETTINGS_KEY, JSON.stringify(mockUiSettings)); } catch { /* storage unavailable: memory only */ }
}

function mockCommand(name: string, input: Record<string, unknown>): unknown {
  switch (name) {
    case 'rightkit.settings.get':
      return { values: { ...mockUiSettings }, revision: mockUiRevision };
    case 'rightkit.settings.patch': {
      for (const [k, v] of Object.entries((input.patch ?? {}) as Record<string, unknown>)) {
        if (v === null) delete mockUiSettings[k];
        else mockUiSettings[k] = v;
      }
      mockUiRevision += 1;
      persistMockUiSettings();
      return { values: { ...mockUiSettings }, revision: mockUiRevision };
    }
    case 'templates.list':
      return { ok: true, templates: [...mockSwapTemplates.values()], unserved_routes: [] };
    case 'templates.import':
    case 'templates.save': {
      const src = String(input.path ?? input.url ?? (input.base as { path?: string; url?: string } | undefined)?.path ?? '');
      const ty = /\.(mp4|mov|webm|m4v)(\?|#|$)/i.test(src) ? 'video' : 'image';
      const nm = String(input.name || src.split(/[\/]/).pop()?.replace(/\.[^.]+$/, '') || 'Template');
      const id = nm.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'template';
      const doc = { id, name: nm, kind: ty, base: /^https?:/.test(src) ? { url: src } : { path: src }, prompt: String(input.prompt ?? ''), default_model: null, slots: [{ name: 'subject', label: 'Your image', type: 'image', required: true }] };
      mockSwapTemplates.set(id, doc);
      return { ok: true, template: doc };
    }
    case 'templates.delete': {
      const id = String(input.id ?? '');
      return { ok: true, id, deleted: mockSwapTemplates.delete(id) };
    }
    case 'templates.swap_preflight': {
      const t = mockSwapTemplates.get(String(input.template_id ?? ''));
      if (!t) throw new MockEngineError('template_not_found', `No template "${String(input.template_id)}"`);
      const slots = (input.slots ?? {}) as Record<string, string>;
      const local = Object.values(slots).filter((v) => !/^https?:/i.test(v));
      const route = { provider: String(input.provider ?? 'nanobanana'), model: String(input.model ?? 'nano-banana-2') };
      const request = { kind: 'image', ...route, prompt: t.prompt, params: { image_urls: [t.base.path ?? t.base.url, ...Object.values(slots)] } };
      const hash = stableHash(JSON.stringify(request));
      const draft_id = `tpl-swap-${t.id}-${hash}`;
      rtDrafts.set(draft_id, { draft_id, status: 'draft', stage: 'single', requires_approval_of: null, request, request_hash: hash });
      return {
        ok: true, template_id: t.id, route, est_usd: 0.04,
        considered: [{ route: 'nanobanana/nano-banana-2', est_usd: 0.04, usable: true }, { route: 'evolink/gpt-image-2', est_usd: 0.09, usable: true }, { route: 'fal/bytedance/seedream/v5/lite/edit', usable: false, reason: 'no fal key' }],
        unserved_routes: [], draft: { draft_id, stage: 'single', status: 'draft', est_usd: 0.04, request_hash: hash, request },
        upload_confirmation: { required: local.length > 0, paths: local, confirm_field: 'confirm_fal_upload' }
      };
    }
    case 'doctor':
      return {
        ok: true,
        version: '0.1.0',
        contract_version: 2,
        platform: mockIsWindows() ? 'windows' : 'macos',
        arch: mockIsWindows() ? 'x86_64' : 'aarch64',
        engine: 'browser-qa-mock',
        data_dir: '~/Library/Application Support/App',
        library_dir: '~/Library/Application Support/App/library',
        checks: [{ id: 'engine', ok: true }, { id: 'index', ok: true }]
      };
    case 'license.changed':
      return { ok: true };
    case 'apps':
      return { apps: [{ id: 'heardright', name: 'App', version: '1.4.2', path: '~/Applications/HeardRight.app' }, { id: 'scraperight', name: 'App', version: '0.1.10', path: '~/Applications/ScrapeRight.app' }] };
    case 'models':
      return MOCK_MODEL_ROWS;
    // ---- price facts (media modalities) : begin ----
    case 'media.modalities':
      return mockModalities();
    // ---- price facts (media modalities) : end ----
    case 'credentials.status':
      return {
        providers: MOCK_PROVIDER_IDS.map((id) => ({
          id,
          needs_key: !KEYLESS_PROVIDERS.has(id),
          configured: mockState.configured.has(id) || KEYLESS_PROVIDERS.has(id),
          source: mockState.configured.get(id) ?? (KEYLESS_PROVIDERS.has(id) ? 'local' : null),
          // QA scenario: qts model missing on disk.
          runtime: id === 'local-qts'
            ? { ready: false, external: false, code: 'not_installed', message: 'QTS model files not found (1.8 GB download)' }
            : null
        }))
      };
    case 'credentials.import_env': {
      const missing = MOCK_PROVIDER_IDS.filter((id) => !KEYLESS_PROVIDERS.has(id) && !mockState.configured.has(id));
      const imported = mockState.configured.get('wavespeed') === 'environment' ? ['wavespeed'] : [];
      if (imported.length) mockState.configured.set('wavespeed', 'keychain');
      return { imported, skipped_present: MOCK_PROVIDER_IDS.filter((id) => mockState.configured.has(id) && !imported.includes(id)), missing, source: 'environment' };
    }
    case 'credentials.set': {
      const provider = String(input.provider || '');
      if (!MOCK_PROVIDER_IDS.includes(provider)) throw new MockEngineError('unknown_provider', `unknown provider: ${provider}`);
      if (KEYLESS_PROVIDERS.has(provider)) throw new MockEngineError('no_key_needed', `${provider} runs locally and takes no API key`);
      if (String(input.api_key || '')) mockState.configured.set(provider, 'keychain');
      else mockState.configured.delete(provider);
      return { ok: true };
    }
    case 'credentials.test': {
      const provider = String(input.provider || '');
      if (!MOCK_PROVIDER_IDS.includes(provider)) throw new MockEngineError('unknown_provider', `unknown provider: ${provider}`);
      if (KEYLESS_PROVIDERS.has(provider)) return { ok: true, message: 'local engine installed' };
      return mockState.configured.has(provider)
        ? { ok: true, message: 'key accepted' }
        : { ok: false, message: 'no API key stored' };
    }
    case 'generate': {
      const model = MOCK_MODELS.find((m) => m.provider === input.provider && m.model === input.model);
      if (!model) throw new MockEngineError('invalid_request', `unknown model ${String(input.provider)}/${String(input.model)}`);
      if (model.kind !== input.kind) throw new MockEngineError('invalid_request', 'model does not match request kind');
      // Estimate only: the engine returns before reserving spend or storing a job. The mock has no
      // per-parameter price table, so it echoes the catalog figure.
      const params = (input.params || {}) as Record<string, unknown>;
      // Mirrors the engine: a local NanoBanana reference is uploaded to the person's own fal storage,
      // and only with an explicit boolean `true` on this call. Checked before any provider key or spend.
      const localRefs = model.provider === 'nanobanana' && Array.isArray(params.image_urls)
        ? (params.image_urls as unknown[]).filter((r): r is string => typeof r === 'string' && !/^https?:\/\//i.test(r) && /^(file:\/\/|\/|[A-Za-z]:[\\/])/.test(r))
        : [];
      // A dry run never uploads or needs consent; it reports the local paths the engine would upload (as the engine does).
      if (input.dry_run) {
        return {
          dry_run: true,
          est_usd: model.est_usd,
          would_submit: { kind: input.kind, provider: model.provider, model: model.model, prompt: input.prompt },
          ...(localRefs.length ? { requires_fal_upload_confirmation: true, local_references: localRefs } : {})
        };
      }
      if (localRefs.length) {
        if (input.confirm_fal_upload !== true) {
          throw new MockEngineError('upload_confirmation_required', `App uploads this image to your fal storage so NanoBanana can read it (${localRefs.length} local image${localRefs.length === 1 ? '' : 's'}: ${localRefs.map((r) => r.split(/[\\/]/).pop() || 'image').join(', ')}). Send confirm_fal_upload: true to allow this for this run only.`);
        }
        if (!mockState.configured.has('fal')) throw new MockEngineError('no_key', 'No fal API key is configured. Add one in Settings > Providers > API keys > fal; the image is uploaded to your own fal storage, so a fal key is required.');
      }
      if (!KEYLESS_PROVIDERS.has(model.provider) && !mockState.configured.has(model.provider)) throw new MockEngineError('no_credentials', `no ${model.provider} API key stored`);
      if (!String(input.prompt || '').trim() && !params.audio_url) throw new MockEngineError('invalid_request', 'prompt is empty');
      validateMockParams(model, params, String(input.prompt || ''));
      const committed = mockState.jobs.filter((j) => !mockPhase(j).status.match(/failed|cancelled/)).reduce((n, j) => n + j.est_usd, 0);
      if (committed + model.est_usd > mockState.capUsd) throw new MockEngineError('spend_cap', `daily cap $${mockState.capUsd.toFixed(2)} would be exceeded`);
      // The mock keeps no calendar: every mock job counts in the current day and month.
      if (mockState.monthlyCapUsd !== null && committed + model.est_usd > mockState.monthlyCapUsd) throw new MockEngineError('spend_cap', `monthly cap $${mockState.monthlyCapUsd.toFixed(2)} would be exceeded`);
      mockState.seq += 1;
      const job: MockJob = { job_id: `job-mock-${mockState.seq}`, kind: String(input.kind), provider: model.provider, model: model.model, prompt: String(input.prompt), params, est_usd: model.est_usd, created_ms: Date.now(), seed: mockState.seq + 2, cancelled: false, stuck: /\bstuck\b/i.test(String(input.prompt ?? '')), unknown: /\bunknown\b/i.test(String(input.prompt ?? '')) };
      mockState.jobs.unshift(job);
      return { job_id: job.job_id, status: 'submitted', est_usd: job.est_usd, request_id: input.request_id ?? null, deduplicated: false };
    }
    case 'job.status': {
      const job = mockState.jobs.find((j) => j.job_id === input.job_id);
      if (!job) throw new MockEngineError('job_not_found', `job ${String(input.job_id)}`);
      return mockJobRecord(job, true);
    }
    case 'job.result': {
      const job = mockState.jobs.find((j) => j.job_id === input.job_id);
      if (!job) throw new MockEngineError('job_not_found', `job ${String(input.job_id)}`);
      if (mockPhase(job).status !== 'complete') throw new MockEngineError('not_ready', 'job has not completed');
      const count = job.kind === 'image' ? 1 : 1;
      const dur = typeof job.params.duration === 'number' ? job.params.duration : null;
      return {
        job_id: job.job_id,
        request_id: null,
        provider: job.provider,
        model: job.model,
        kind: job.kind,
        created_at: new Date(job.created_ms).toISOString(),
        acked_by: null,
        acked_at: null,
        assets: Array.from({ length: count }, (_, i) => ({
          asset_id: `${job.job_id}-a${i}`,
          path: job.kind === 'voice' ? mockWav() : mockScene(job.seed * 3 + i),
          sha256: stableHash(`${job.job_id}-${i}`).repeat(8),
          bytes: job.kind === 'voice' ? 8044 : 48_000,
          mime: job.kind === 'image' || job.kind === 'upscale' ? 'image/svg+xml' : job.kind === 'video' || job.kind === 'avatar' || job.kind === 'lipsync' ? 'video/mp4' : 'audio/wav',
          media: job.kind === 'image' || job.kind === 'upscale' ? { kind: 'image', width: 1024, height: 1024, duration_s: null } : job.kind === 'video' || job.kind === 'avatar' || job.kind === 'lipsync' ? { kind: 'video', width: 1280, height: 720, duration_s: dur ?? 4 } : { kind: 'voice', width: null, height: null, duration_s: 1 },
          provider: job.provider,
          model: job.model
        }))
      };
    }
    case 'job.list': {
      const limit = typeof input.limit === 'number' ? input.limit : 50;
      const rows = mockState.jobs.slice(0, limit);
      return { jobs: rows.map((j) => mockJobRecord(j, false)), truncated: mockState.jobs.length > limit };
    }
    case 'asset.delete': {
      const id = typeof input.asset_id === 'string' ? input.asset_id : null;
      const path = typeof input.path === 'string' ? input.path : null;
      if (!id && !path) throw new MockEngineError('invalid_input', 'asset_id or path is required');
      const index = MOCK_GALLERY.findIndex((a) => (id !== null && a.artifact_id === id) || (path !== null && a.absolute_path === path));
      if (index < 0) return { removed: false };
      MOCK_GALLERY.splice(index, 1);
      return { removed: true };
    }
    case 'job.cancel': {
      const job = mockState.jobs.find((j) => j.job_id === input.job_id);
      if (!job) throw new MockEngineError('job_not_found', `job ${String(input.job_id)}`);
      if (job.unknown) throw new MockEngineError('outcome_unknown', 'resolve the job instead of cancelling it');
      if (mockPhase(job).status !== 'complete') job.cancelled = true;
      return mockJobRecord(job, false);
    }
    case 'job.run': {
      // Resuming a non-terminal job: the caller becomes its runner.
      const job = mockState.jobs.find((j) => j.job_id === input.job_id);
      if (!job) throw new MockEngineError('job_not_found', `job ${String(input.job_id)}`);
      if (job.unknown) throw new MockEngineError('outcome_unknown', 'the job may already be running at the provider; resolve it first');
      if (job.stuck) {
        job.stuck = false;
        job.created_ms = Date.now();
      }
      return mockJobRecord(job, true);
    }
    case 'job.wait': {
      const job = mockState.jobs.find((j) => j.job_id === input.job_id);
      if (!job) throw new MockEngineError('job_not_found', `job ${String(input.job_id)}`);
      return { ...mockJobRecord(job, true), timed_out: !['complete', 'failed', 'cancelled', 'outcome_unknown'].includes(mockPhase(job).status) };
    }
    case 'job.resolve': {
      const job = mockState.jobs.find((j) => j.job_id === input.job_id);
      if (!job) throw new MockEngineError('job_not_found', `job ${String(input.job_id)}`);
      if (!job.unknown) throw new MockEngineError('not_outcome_unknown', `job ${job.job_id} is ${mockPhase(job).status}; only an outcome_unknown job can be resolved`);
      if (input.action !== 'resubmit' && input.action !== 'abandon') throw new MockEngineError('invalid_input', 'action must be resubmit or abandon');
      job.unknown = false;
      if (input.action === 'resubmit') job.created_ms = Date.now();
      else job.abandoned = true;
      return mockJobRecord(job, false);
    }
    case 'job.ack': {
      const job = mockState.jobs.find((j) => j.job_id === input.job_id);
      if (!job) throw new MockEngineError('job_not_found', `job ${String(input.job_id)}`);
      return { ...mockJobRecord(job, false), acked_by: input.consumer ?? null, acked_at: new Date().toISOString() };
    }
    case 'studio.balances.list': {
      const labels: Record<string, string> = { nanobanana: 'NanoBanana', fal: 'fal', wavespeed: 'WaveSpeed', evolink: 'EvoLink' };
      const known: Record<string, { balance: number; unit: string }> = {
        nanobanana: { balance: 512, unit: 'credits' },
        fal: { balance: 12.4, unit: 'USD' },
        wavespeed: { balance: 8.02, unit: 'USD' },
        evolink: { balance: 640, unit: 'credits' }
      };
      const now = new Date().toISOString();
      const wanted = Array.isArray(input.providers) ? (input.providers as string[]) : Object.keys(labels);
      return wanted.map((id) => {
        if (!mockState.configured.has(id)) {
          return { provider: id, label: labels[id] ?? id, balance: null, unit: null, fetched_at: now, error: 'no API key stored' };
        }
        const b = known[id];
        return { provider: id, label: labels[id] ?? id, balance: b ? b.balance : null, unit: b?.unit ?? null, fetched_at: now, ...(b ? {} : { error: 'balance endpoint unavailable' }) };
      });
    }
    case 'studio.avatars.list':
      return {
        avatars: [
          { id: 'ag_demo_twin', name: 'Adrian (studio)', status: 'pending_consent', consent_status: null, looks_count: 1, avatar_type: 'digital_twin', consent_url: 'https://example.invalid/consent/ag_demo_twin' },
          { id: 'ag_demo_prompt', name: 'Presenter Ava', status: 'completed', consent_status: null, looks_count: 3, avatar_type: 'prompt' }
        ],
        has_more: false,
        next_token: null
      };
    case 'studio.avatars.status':
      return { id: String(input.avatar_group_id), name: 'Adrian (studio)', status: 'completed', consent_status: 'accepted', ready: true, consent_url: null, next_action: 'Ready' };
    case 'studio.avatars.create_twin':
    case 'studio.avatars.create_photo':
    case 'studio.avatars.create_prompt': {
      const avatarType = name.replace('studio.avatars.create_', '');
      const costNotice = 'Price not published: HeyGen does not publish an API price for avatar creation, so App cannot estimate or reserve it. It may use your HeyGen plan credits or bill your account, and it is not counted against App\u2019s daily cap.';
      if (input.dry_run) return { dry_run: true, avatar_type: avatarType, request_body: { type: avatarType, name: input.name }, est_usd: null, price_published: false, cost_notice: costNotice, note: 'Nothing was sent. HeyGen avatar creation cost is account/plan gated.' };
      if (input.confirm_unpublished_price !== true) throw new MockEngineError('price_confirmation_required', costNotice);
      return { avatar_type: avatarType, est_usd: null, price_published: false, cost_notice: costNotice, avatar_item_id: 'look_mock', avatar_group_id: 'ag_mock', status: 'pending_consent', consent_url: 'https://example.invalid/consent/ag_mock', next_action: 'Open the link, finish the provider step, then check status until it is completed.' };
    }
    case 'studio.avatars.routes': {
      const cap = String(input.capability || 'avatar');
      const lip = cap === 'lip_sync' || cap === 'lipsync';
      const kind = lip ? 'lipsync' : 'avatar';
      const api = MOCK_MODELS.filter((m) => m.kind === kind).map((m) => ({
        route: 'api', provider: m.provider, model: m.model, label: m.label, est_usd: m.est_usd,
        credential: 'unconfigured', ready: false, quality: 'unreviewed'
      }));
      return {
        capability: lip ? 'lip_sync' : 'avatar',
        selected: mockState.modelRoutes['avatar'] ?? null,
        valid_routes: ['auto', 'local', 'api', 'rented'],
        routes: {
          api,
          local: [],
          rented: [
            {
              id: 'soulx-flashhead', label: 'SoulX-FlashHead avatar (experiment)', engine: 'rented', engine_kind: 'experimental',
              capabilities: ['talking_head', 'lip_sync'], requirements: { vram_gb: 8, disk_gb: 20 },
              readiness: { state: 'rental' }, fit: { fit: 'rental_only' },
              measured: [{ profile: 'rtx4070-8gb', notes: 'prior experiment report: executed but output quality rejected (portrait crop, mouth/head motion); not qualified' }],
              quality: 'unqualified'
            }
          ]
        },
        rented_workers: mockState.modelWorkers,
        worker_contract: 'studio.models.worker.action {action: register|stage|reconnect|begin|retrieve|teardown}'
      };
    }
    case 'studio.avatars.animate':
    case 'studio.avatars.lip_sync': {
      const sync = name === 'studio.avatars.lip_sync';
      if (sync && input.video_url) {
        if (!input.audio_url) throw new MockEngineError('invalid_input', 'video lip-sync needs an audio_url (https)');
        return {
          mode: 'lip_sync', route: 'api',
          request: { kind: 'lipsync', provider: String(input.provider ?? 'wavespeed'), model: String(input.model ?? 'lipsync-3'), prompt: '', params: { video_url: input.video_url, audio_url: input.audio_url } },
          est_usd: MOCK_MODELS.find((m) => m.model === 'lipsync-3')?.est_usd ?? null, ready: true, quality: 'unreviewed', submit_via: 'media.generate', note: 'Prepared only — nothing was sent.'
        };
      }
      if (mockState.modelRoutes['avatar']?.route === 'rented' || input.model === 'soulx-flashhead') {
        return {
          mode: sync ? 'lip_sync' : 'animate', route: 'rented', model: 'soulx-flashhead', engine_kind: 'experimental',
          quality: 'unqualified', worker_required: true, worker: mockState.modelWorkers[0] ?? null, est_usd: null,
          submit_via: 'studio.models.worker.action',
          note: 'SoulX-FlashHead is a rented/remote-worker experiment whose prior output was rejected; it is not quality-accepted.'
        };
      }
      const wantModel = input.model != null ? String(input.model) : null;
      const found = wantModel ? MOCK_MODELS.find((m) => m.model === wantModel && (m.kind === 'avatar' || m.kind === 'lipsync')) : null;
      const provider = String(input.provider ?? found?.provider ?? 'heygen');
      const model = wantModel ?? 'avatar-standard';
      if (provider === 'heygen') {
        if (!input.avatar_id && !input.avatar_group_id) throw new MockEngineError('invalid_input', 'avatar_id (HeyGen look id) or avatar_group_id is required');
        if (input.script && input.audio_url) throw new MockEngineError('invalid_input', 'use a script or audio_url, not both');
        if (!input.script && !input.audio_url) throw new MockEngineError('invalid_input', 'a script or audio_url is required');
      } else {
        if (!input.image_url) throw new MockEngineError('invalid_input', `${model} needs a portrait image_url (https)`);
        if (!input.audio_url) throw new MockEngineError('invalid_input', `${model} needs an audio_url (https)`);
      }
      const params: Record<string, unknown> = {};
      for (const k of ['avatar_id', 'image_url', 'audio_url', 'video_url', 'voice_id', 'aspect_ratio', 'resolution', 'sync_mode']) {
        if (input[k] != null && input[k] !== '') params[k] = input[k];
      }
      return {
        mode: sync ? 'lip_sync' : 'animate', route: 'api',
        request: { kind: 'avatar', provider, model, prompt: String(input.script ?? ''), params },
        est_usd: MOCK_MODELS.find((m) => m.provider === provider && m.model === model)?.est_usd ?? null,
        ready: true, quality: 'unreviewed', warnings: [], submit_via: 'media.generate', note: 'Prepared only — nothing was sent. Submit `request` via media.generate to spend.'
      };
    }
    case 'studio.avatars.review': {
      const verdict = String(input.verdict || '');
      if (!['accept', 'accepted', 'reject', 'unqualified', 'unreviewed'].includes(verdict)) throw new MockEngineError('invalid_input', 'verdict must be accept, reject, unqualified, or unreviewed');
      const subject = ['avatar_group_id', 'avatar_item_id', 'asset_id', 'path'].map((k) => input[k]).find(Boolean);
      if (!subject) throw new MockEngineError('invalid_input', 'a subject (avatar_group_id, avatar_item_id, asset_id, or path) is required');
      const quality = verdict === 'accept' || verdict === 'accepted' ? 'accepted' : verdict === 'unreviewed' ? 'unreviewed' : 'unqualified';
      return { subject: String(subject), quality, reviews: 1 };
    }
    // Mirrors the engine: approval is a user action in the app. The engine commands refuse a caller
    // that brings no app-minted token, whatever else it sets (`operator_approved` included).
    case 'studio.recipes.approve':
    case 'studio.recipes.template_approve':
    case 'studio.video_templates.approve':
    case 'studio.characters.train':
    case 'studio.eyes.approve':
      if (typeof input.approval_token !== 'string' || !input.approval_token) throw new MockEngineError('approval_requires_app', APPROVAL_REQUIRES_APP);
      return unsupportedMock(name);
    // The eyes gate reads the file's bytes, which the browser preview does not have.
    case 'studio.eyes.status':
      return { path: String(input.path ?? ''), exists: true, verdict: null, gate: 'none', export_allowed: false, hash: 'sha256:0000000000000000', recorded_hash: null };
    // A batch approve decision needs the same token, per decision; a reject needs none.
    case 'studio.recipes.batch_review': {
      const decisions = Array.isArray(input.decisions) ? (input.decisions as Array<Record<string, unknown>>) : [];
      if (decisions.some((d) => d.decision === 'approve' && (typeof d.approval_token !== 'string' || !d.approval_token))) {
        throw new MockEngineError('approval_requires_app', APPROVAL_REQUIRES_APP);
      }
      return unsupportedMock(name);
    }
    case 'studio.recipes.libraries':
      return mockRecipeLibraries();
    case 'studio.recipes.compose':
      return mockRecipeCompose(input);
    case 'studio.models.governor.loaded':
    case 'studio.models.governor.status': {
      // Mirrors governor.rs loaded_json/status: nothing resident at QA boot, RAM in GB, empty grants/events.
      const base = {
        loaded: [] as Array<{ runtime: string; model_id: string; rss_gb: number | null; idle: boolean }>,
        ram: { free_gb: 11.4, resident_gb: 0, reserve_gb: 1.5 }
      };
      return name === 'studio.models.governor.status' ? { ...base, grants: [], events: [] } : base;
    }
    case 'studio.models.hardware':
      return {
        os: mockIsWindows() ? 'windows' : 'macos',
        arch: 'aarch64',
        memory_gb: 24,
        gpus: [{ name: 'Apple Silicon (QA)', unified: true, backends: ['metal'] }],
        profile: 'macos-24gb-unified',
        source: 'detected'
      };
    case 'studio.models.catalog':
      return {
        hardware: mockCommand('studio.models.hardware', {}),
        entries: [
          { id: 'noct-q-v4-int8', label: 'Noct Q V4 (INT8 ConvRot)', modality: 'image', engine: 'sdcpp', engine_kind: 'native-bundled', revision: 1, quantization: 'int8', installable: true, license: { name: 'Qwen Research License', commercial: false }, readiness: { state: 'missing', missing_files: ['transformer.gguf', 'encoder.gguf', 'vae.safetensors'] }, fit: { fit: 'estimated', required_gb: 12, available_gb: 15.8, reasons: ['requirement fits detected hardware; not yet measured here'] } },
          { id: 'qwen3-tts-0.6b-q8_0', label: 'Qwen3-TTS 0.6B (QTS)', modality: 'voice', engine: 'qts', engine_kind: 'native-bundled', revision: 'qts-1', quantization: 'q8_0', installable: true, requirements: { memory_gb: 8 }, readiness: { state: 'missing', missing_files: ['qwen3-tts-0.6b-q8_0.gguf', 'qwen3-tts-vocoder.onnx'] }, fit: { fit: 'estimated', required_gb: 8, available_gb: 15.8, reasons: [] } },
          { id: 'wan2.2-ti2v-5b', label: 'Wan2.2 TI2V-5B (rented GPU route)', modality: 'video', engine: 'rented', engine_kind: 'rented-remote', revision: 'wan2.2', requirements: { vram_gb: 24 }, license: { name: 'Apache-2.0', commercial: true }, readiness: { state: 'rental' }, fit: { fit: 'rental_only' } },
          { id: 'soulx-flashhead', label: 'SoulX-FlashHead avatar (experiment)', modality: 'avatar', engine: 'rented', engine_kind: 'experimental', revision: 'flashhead-1', requirements: { vram_gb: 8 }, readiness: { state: 'rental' }, fit: { fit: 'rental_only' } }
        ],
        routes: mockState.modelRoutes,
        capabilities: ['image', 'video', 'voice', 'avatar', 'music', 'sfx', 'upscale', 'lipsync'],
        installs: mockState.modelInstalls,
        progress: Object.fromEntries(Object.entries(mockState.modelInstalls).map(([id, rec]) => {
          if (rec.state === 'installing') {
            mockState.modelInstallTicks[id] = (mockState.modelInstallTicks[id] ?? 0) + 1;
            const done = mockState.modelInstallTicks[id] * 800000000;
            if (done >= 2400000000) rec.state = 'installed';
            return [id, { bytes_done: Math.min(done, 2400000000), bytes_total: 2400000000, file: 'model.bin' }];
          }
          return [id, null];
        }))
      };
    case 'studio.models.describe':
    case 'studio.models.probe':
    case 'studio.models.estimate': {
      const cat = mockCommand('studio.models.catalog', {}) as { entries: Array<Record<string, unknown>> };
      const entry = cat.entries.find((e) => e.id === input.id);
      if (!entry) throw new MockEngineError('model_not_found', `no manifest entry: ${String(input.id)}`);
      return name === 'studio.models.describe'
        ? { entry, readiness: entry.readiness, fit: entry.fit }
        : { id: entry.id, readiness: entry.readiness, fit: entry.fit };
    }
    case 'studio.models.install': {
      const cat = mockCommand('studio.models.catalog', {}) as { entries: Array<Record<string, unknown>> };
      const entry = cat.entries.find((e) => e.id === input.id);
      if (!entry) throw new MockEngineError('model_not_found', `no manifest entry: ${String(input.id)}`);
      if (entry.engine === 'rented') return { id: entry.id, state: 'rental', message: 'no local install; stage a rented worker' };
      if (entry.installable === true) {
        const now = new Date().toISOString();
        mockState.modelInstalls[String(entry.id)] = {
          id: entry.id, state: 'installing', revision: String(entry.revision ?? ''),
          files: [{ name: 'model.bin', bytes: 2400000000, state: 'pending' }],
          events: [{ at: now, action: 'install_start', detail: { force: input.force === true } }],
          updated_at: now
        };
        mockState.modelInstallTicks[String(entry.id)] = 0;
        return { id: entry.id, state: 'installing', total_bytes: 2400000000 };
      }
      return { id: entry.id, state: (entry.readiness as { state?: string })?.state ?? 'missing', message: 'model files fetched by first-run setup' };
    }
    case 'studio.models.install.status': {
      if (input.id) {
        const rec = mockState.modelInstalls[String(input.id)];
        if (!rec) throw new MockEngineError('install_not_found', `no install record for ${String(input.id)}`);
        return { install: rec, progress: null };
      }
      return { installs: mockState.modelInstalls, progress: {} };
    }
    case 'studio.models.install.pause': {
      const rec = mockState.modelInstalls[String(input.id)];
      if (!rec) throw new MockEngineError('install_not_found', `no install record for ${String(input.id)}`);
      rec.state = 'paused';
      rec.updated_at = new Date().toISOString();
      return { id: String(input.id), state: 'paused' };
    }
    case 'studio.models.install.cancel': {
      const rec = mockState.modelInstalls[String(input.id)];
      if (!rec) throw new MockEngineError('install_not_found', `no install record for ${String(input.id)}`);
      rec.state = 'cancelled';
      rec.updated_at = new Date().toISOString();
      return { id: String(input.id), state: 'cancelled' };
    }
    case 'studio.models.install.uninstall': {
      const rec = mockState.modelInstalls[String(input.id)];
      if (rec) { rec.state = 'uninstalled'; rec.updated_at = new Date().toISOString(); }
      return { id: String(input.id), state: 'uninstalled', removed: ['model.bin'] };
    }
    case 'studio.models.install.rollback': {
      const rec = mockState.modelInstalls[String(input.id)];
      if (!rec) throw new MockEngineError('install_not_found', `no install record for ${String(input.id)}`);
      rec.state = 'installed';
      rec.updated_at = new Date().toISOString();
      return { id: String(input.id), revision: rec.revision, state: 'installed' };
    }
    case 'studio.models.rental.search': {
      const key = (input.api_key as string) || '';
      if (!key) throw new MockEngineError('no_credentials', 'no Vast API key: pass api_key or set VAST_API_KEY (never persisted)');
      return { offers: [
        { offer_id: 1001, gpu: 'RTX 4090', num_gpus: 1, vram_gb: 24, dph_total: 0.34, disk_gb: 50, reliability: 0.98, verified: true, geolocation: 'US' },
        { offer_id: 1002, gpu: 'RTX 5090', num_gpus: 1, vram_gb: 32, dph_total: 0.62, disk_gb: 60, reliability: 0.99, verified: true, geolocation: 'US' },
        { offer_id: 1003, gpu: 'A100', num_gpus: 1, vram_gb: 80, dph_total: 1.10, disk_gb: 80, reliability: 0.97, verified: true, geolocation: 'DE' }
      ] };
    }
    case 'studio.models.route.get':
      return { routes: mockState.modelRoutes, capabilities: ['image', 'video', 'voice', 'avatar', 'music', 'sfx', 'upscale', 'lipsync'], valid_routes: ['auto', 'local', 'api', 'rented'] };
    case 'studio.models.route.set': {
      const cap = String(input.capability || '');
      const route = String(input.route || '');
      if (!['image', 'video', 'voice', 'avatar', 'music', 'sfx', 'upscale', 'lipsync'].includes(cap)) throw new MockEngineError('invalid_input', `unknown capability: ${cap}`);
      if (!['auto', 'local', 'api', 'rented'].includes(route)) throw new MockEngineError('invalid_input', `route must be one of auto/local/api/rented`);
      mockState.modelRoutes[cap] = { route, model: (input.model as string) ?? null, set_at: new Date().toISOString() };
      return { routes: mockState.modelRoutes };
    }
    case 'studio.models.unload':
      return { id: String(input.id), unloaded: true, note: 'worker asked to exit' };
    case 'studio.models.workers':
      return { workers: [...mockState.modelWorkers, ...mockRentalWorkers()], driver: 'none' };
    case 'studio.models.worker.action': {
      // RENTALS section: rental records answer teardown and resolve_rental themselves.
      const rentalReply = mockRentalWorkerAction(input);
      if (rentalReply) return rentalReply.value;
      const action = String(input.action || '');
      const now = new Date().toISOString();
      if (action === 'register') {
        mockState.modelWorkerSeq += 1;
        const w = {
          worker_id: `rw-mock-${mockState.modelWorkerSeq}`,
          label: 'rented worker',
          spec: input.spec ?? {},
          driver: (input.spec as Record<string, unknown> | undefined)?.driver ?? 'none',
          state: 'configured',
          budget_usd: Number(input.budget_usd) || 0,
          accrued_usd: 0,
          outputs: [],
          events: [{ at: now, action: 'register', from: null, to: 'configured', detail: input.spec ?? {} }],
          created_at: now,
          updated_at: now
        };
        mockState.modelWorkers.push(w);
        return w;
      }
      const w = mockState.modelWorkers.find((x) => x.worker_id === input.worker_id);
      if (!w) throw new MockEngineError('worker_not_found', `no such worker: ${String(input.worker_id)}`);
      const from = String(w.state);
      const event = (to: string, detail: unknown = null) => {
        w.state = to;
        w.updated_at = now;
        (w.events as unknown[]).push({ at: now, action, from, to, detail });
      };
      if (action === 'teardown') { event('terminated', { released: false, note: 'no remote resources existed' }); return w; }
      if (action === 'status') return w;
      if (action === 'provision') {
        if (input.authorized !== true) {
          event('error', { code: 'not_authorized', message: 'billed instance create requires authorized:true' });
          throw new MockEngineError('not_authorized', 'billed instance create requires authorized:true with an explicit target and budget');
        }
        if (!(Number(input.offer_id) > 0) || !(Number(input.max_usd_hr) > 0) || !(Number(input.budget_usd) > 0) || !(Number(input.max_duration_min) > 0)) {
          event('error', { code: 'invalid_input', message: 'offer_id, max_usd_hr, budget_usd and max_duration_min required' });
          throw new MockEngineError('invalid_input', 'offer_id, max_usd_hr, budget_usd and max_duration_min required');
        }
        const spec = w.spec as Record<string, unknown>;
        const vast = { ...(spec.vast as Record<string, unknown> | undefined) };
        vast.instance_id = 900000 + mockState.modelWorkerSeq;
        vast.ssh_host = '203.0.113.10';
        vast.ssh_port = 22;
        spec.vast = vast;
        event('staged', { instance_id: vast.instance_id, offer_id: input.offer_id, dph_total: 0.34 });
        return w;
      }
      // A registered driver simulates the protocol lifecycle for QA; a
      // driverless worker mirrors the real NullDriver error.
      const drv = String((w.spec as Record<string, unknown>)?.driver ?? 'none');
      if (drv !== 'none') {
        const next: Record<string, string> = { stage: 'staged', reconnect: String(w.state), begin: 'running', retrieve: 'retrieved', idle: 'idle', unload: 'staged' };
        if (!next[action]) throw new MockEngineError('invalid_input', `unknown worker action: ${action}`);
        event(next[action], action === 'retrieve' ? { files: 0 } : null);
        return w;
      }
      // Mirror the real NullDriver: nothing fabricates remote success.
      event('error', { code: 'driver_not_configured', message: 'no remote interface configured for this worker' });
      throw new MockEngineError('driver_not_configured', 'no remote interface configured for this worker (set a remote driver or provider)');
    }
    case 'spend.status': {
      const actual = mockState.jobs.filter((j) => mockPhase(j).status === 'complete').reduce((n, j) => n + j.est_usd, 0);
      const reserved = mockState.jobs.filter((j) => ['submitted', 'running'].includes(mockPhase(j).status)).reduce((n, j) => n + j.est_usd, 0);
      const mcap = mockState.monthlyCapUsd;
      return {
        day: new Date().toISOString().slice(0, 10), cap_usd: mockState.capUsd, reserved_usd: reserved, actual_usd: actual,
        month: new Date().toISOString().slice(0, 7), monthly_cap_usd: mcap,
        month_reserved_usd: reserved, month_actual_usd: actual, month_counted_usd: reserved + actual,
        monthly_remaining_usd: mcap === null ? null : Math.max(0, mcap - reserved - actual),
        // The four amounts stay separate; the mock has no provider-reported cost. Invoice amounts are the
        // rows spend.reconcile recorded (attempt 1 of each job).
        amounts: (() => {
          const rows = mockState.jobs.length;
          const estimated = mockState.jobs.reduce((n, j) => n + j.est_usd, 0);
          const invoiceRows = (mockState as { invoiceRows?: Map<string, { usd: number }> }).invoiceRows ?? new Map<string, { usd: number }>();
          const invoiced = mockState.jobs.filter((j) => invoiceRows.has(`${j.job_id}#1`));
          const invoiceSum = invoiced.reduce((n, j) => n + (invoiceRows.get(`${j.job_id}#1`)?.usd ?? 0), 0);
          const period = {
            rows, estimated_usd: rows > 0 ? estimated : null, reserved_usd: estimated,
            provider_reported_usd: null, provider_reported_rows: 0, provider_reported_available: false,
            invoice_reconciled_usd: invoiced.length > 0 ? invoiceSum : null, invoice_reconciled_rows: invoiced.length,
            settled_usd: actual, open_reserved_usd: reserved, cap_counted_usd: reserved + actual,
          };
          return { day: period, month: period };
        })(),
      };
    }
    case 'spend.set_cap': {
      const hasDaily = 'cap_usd' in input;
      const hasMonthly = 'monthly_cap_usd' in input;
      if (!hasDaily && !hasMonthly) throw new MockEngineError('invalid_input', 'missing field: cap_usd or monthly_cap_usd');
      const cap = hasDaily ? Number(input.cap_usd) : null;
      if (cap !== null && (!Number.isFinite(cap) || cap < 0)) throw new MockEngineError('invalid_input', 'cap_usd must be a non-negative number');
      let monthly: number | null | undefined;
      if (hasMonthly) {
        if (input.monthly_cap_usd === null) monthly = null;
        else {
          monthly = Number(input.monthly_cap_usd);
          if (!Number.isFinite(monthly) || monthly < 0) throw new MockEngineError('invalid_input', 'monthly_cap_usd must be a non-negative number or null');
        }
      }
      if (cap !== null) mockState.capUsd = cap;
      if (monthly !== undefined) mockState.monthlyCapUsd = monthly;
      return mockCommand('spend.status', {});
    }
    case 'spend.reconcile': {
      // Mirrors ledger::reconcile_invoice. Records the invoice and per-row invoice amounts only; no cap,
      // reservation or counted amount moves. Invoice records live on the mock state for this session.
      type MockInvoiceRecord = { provider: string; from: string; to: string; total: number };
      const store = mockState as typeof mockState & { invoices?: Map<string, MockInvoiceRecord>; invoiceRows?: Map<string, { usd: number; ref: string }> };
      const invoices = store.invoices ?? (store.invoices = new Map<string, MockInvoiceRecord>());
      const invoiceRows = store.invoiceRows ?? (store.invoiceRows = new Map<string, { usd: number; ref: string }>());
      const money = (v: unknown, what: string) => {
        if (typeof v !== 'number' || !Number.isFinite(v) || v < 0) throw new MockEngineError('invalid_input', `${what} must be a non-negative number`);
        return v;
      };
      const period = input.period;
      let from: string;
      let to: string;
      if (typeof period === 'string') {
        if (!/^\d{4}-(0[1-9]|1[0-2])$/.test(period)) throw new MockEngineError('invalid_input', 'period must be "YYYY-MM" or {from: "YYYY-MM-DD", to: "YYYY-MM-DD"}');
        from = `${period}-01`;
        to = `${period}-31`;
      } else if (period && typeof period === 'object' && 'from' in period && 'to' in period) {
        const f = String((period as { from: unknown }).from);
        const t = String((period as { to: unknown }).to);
        if (!/^\d{4}-\d{2}-\d{2}$/.test(f) || !/^\d{4}-\d{2}-\d{2}$/.test(t) || f > t) {
          throw new MockEngineError('invalid_input', 'period must be "YYYY-MM" or {from: "YYYY-MM-DD", to: "YYYY-MM-DD"}');
        }
        from = f;
        to = t;
      } else {
        throw new MockEngineError('invalid_input', 'period must be "YYYY-MM" or {from: "YYYY-MM-DD", to: "YYYY-MM-DD"}');
      }
      const provider = typeof input.provider === 'string' ? input.provider.trim() : '';
      if (!provider) throw new MockEngineError('invalid_input', 'missing field: provider');
      const total = money(input.invoice_total_usd, 'invoice_total_usd');
      let ref: string;
      let generated = false;
      if (input.invoice_ref === undefined || input.invoice_ref === null) {
        ref = `auto:${provider}:${from}:${to}`;
        generated = true;
      } else {
        const r = typeof input.invoice_ref === 'string' ? input.invoice_ref.trim() : '';
        if (!r) throw new MockEngineError('invalid_input', 'invoice_ref must be a non-empty string');
        ref = r;
      }
      type MockLine = { job_id?: string; provider_job_id?: string; attempt?: number; usd: number };
      const rawLines = input.lines === undefined || input.lines === null ? [] : input.lines;
      if (!Array.isArray(rawLines)) throw new MockEngineError('invalid_input', 'lines must be an array');
      const lines: MockLine[] = rawLines.map((l: Record<string, unknown>, i: number) => {
        const text = (k: string) => (typeof l[k] === 'string' ? (l[k] as string).trim() : '');
        const jobId = text('job_id');
        const providerJobId = text('provider_job_id');
        if (!jobId === !providerJobId) throw new MockEngineError('invalid_input', `lines[${i}] needs exactly one of job_id or provider_job_id`);
        let attempt: number | undefined;
        if (l.attempt !== undefined && l.attempt !== null) {
          if (typeof l.attempt !== 'number' || !Number.isInteger(l.attempt) || l.attempt < 1) throw new MockEngineError('invalid_input', `lines[${i}].attempt must be a positive integer`);
          attempt = l.attempt;
        }
        const usd = money(l.usd, `lines[${i}].usd`);
        return { ...(jobId ? { job_id: jobId } : { provider_job_id: providerJobId }), ...(attempt === undefined ? {} : { attempt }), usd };
      });
      const linesTotal = lines.reduce((n, l) => n + l.usd, 0);

      const existing = invoices.get(ref);
      const already = existing !== undefined;
      if (existing && (existing.provider !== provider || existing.from !== from || existing.to !== to || Math.abs(existing.total - total) > 1e-9)) {
        throw new MockEngineError('invoice_ref_conflict', `invoice_ref ${ref} is already recorded for ${existing.provider} ${existing.from}..${existing.to} with total $${existing.total.toFixed(6)}; it is not changed by ${provider} ${from}..${to} $${total.toFixed(6)}`);
      }
      if (!existing) invoices.set(ref, { provider, from, to, total });

      // Ledger rows the mock keeps: one per job (attempt 1). Reserved while open; settled at the estimate.
      const rows = mockState.jobs.map((j) => {
        const phase = mockPhase(j);
        const released = phase.status === 'cancelled' || phase.status === 'failed';
        const settled = phase.status === 'complete';
        return {
          job_id: j.job_id, attempt: 1, provider: j.provider, est: j.est_usd,
          day: new Date(j.created_ms).toISOString().slice(0, 10),
          reserved: released ? 0 : j.est_usd,
          actual: released ? (0 as number | null) : settled ? j.est_usd : null,
        };
      });
      const inPeriod = rows.filter((r) => r.provider === provider && r.day >= from && r.day <= to);
      const key = (jobId: string, attempt: number) => `${jobId}#${attempt}`;

      const matched: unknown[] = [];
      const unmatchedLines: unknown[] = [];
      const seen = new Set<string>();
      lines.forEach((line, index) => {
        const unmatched = (reason: string) => unmatchedLines.push({ index, job_id: line.job_id ?? null, provider_job_id: line.provider_job_id ?? null, attempt: line.attempt ?? null, usd: line.usd, reason });
        const candidates = inPeriod.filter((r) => (line.job_id ? r.job_id === line.job_id : false) && (line.attempt === undefined || r.attempt === line.attempt));
        if (candidates.length === 0) return unmatched('no_ledger_row_for_provider_and_period');
        if (candidates.length > 1) return unmatched('ambiguous_attempt: the job has several rows in the period; name the attempt');
        const row = candidates[0];
        if (seen.has(key(row.job_id, row.attempt))) return unmatched('duplicate_line_for_row');
        const prior = invoiceRows.get(key(row.job_id, row.attempt));
        if (prior && (prior.ref !== ref || Math.abs(prior.usd - line.usd) > 1e-9)) return unmatched('row_already_reconciled_by_another_invoice_or_amount');
        invoiceRows.set(key(row.job_id, row.attempt), { usd: line.usd, ref });
        seen.add(key(row.job_id, row.attempt));
        const settledUsd = row.actual ?? 0;
        matched.push({ job_id: row.job_id, attempt: row.attempt, invoice_usd: line.usd, settled_usd: settledUsd, difference_usd: line.usd - settledUsd });
      });

      const unmatchedRows = lines.length === 0 ? [] : inPeriod
        .filter((r) => !invoiceRows.has(key(r.job_id, r.attempt)) && (r.actual ?? r.reserved) > 0)
        .map((r) => ({ job_id: r.job_id, attempt: r.attempt, settled_usd: r.actual, reserved_usd: r.reserved }));

      const jobRows = inPeriod.length;
      const estimated = inPeriod.reduce((n, r) => n + r.est, 0);
      const jobRowsSettled = inPeriod.reduce((n, r) => n + (r.actual ?? 0), 0);
      const reserved = inPeriod.reduce((n, r) => n + r.reserved, 0);
      const openReserved = inPeriod.reduce((n, r) => n + (r.actual === null ? r.reserved : 0), 0);
      const settled = inPeriod.reduce((n, r) => n + (r.actual ?? 0), 0);
      const invoiced = inPeriod.filter((r) => invoiceRows.has(key(r.job_id, r.attempt)));
      const invRows = invoiced.length;
      const invSum = invoiced.reduce((n, r) => n + (invoiceRows.get(key(r.job_id, r.attempt))?.usd ?? 0), 0);
      const invSettled = invoiced.reduce((n, r) => n + (r.actual ?? 0), 0);
      const pair = (a: number, b: number) => a - b;
      return {
        rows: inPeriod.length,
        estimated_usd: jobRows > 0 ? estimated : null,
        reserved_usd: reserved,
        provider_reported_usd: null,
        provider_reported_rows: 0,
        provider_reported_available: false,
        invoice_reconciled_usd: invRows > 0 ? invSum : null,
        invoice_reconciled_rows: invRows,
        settled_usd: settled,
        open_reserved_usd: openReserved,
        cap_counted_usd: settled + openReserved,
        provider,
        period: { from, to },
        invoice_ref: ref,
        invoice_ref_generated: generated,
        already_recorded: already,
        invoice_total_usd: total,
        differences: {
          invoice_total_minus_settled_usd: pair(total, settled),
          settled_minus_estimated_usd: jobRows > 0 ? pair(jobRowsSettled, estimated) : null,
          provider_reported_minus_settled_usd: null,
          invoice_reconciled_minus_settled_usd: invRows > 0 ? pair(invSum, invSettled) : null,
          lines_total_minus_invoice_total_usd: lines.length > 0 ? pair(linesTotal, total) : null,
        },
        ...(lines.length > 0 ? { lines: { given: lines.length, matched, unmatched_lines: unmatchedLines, unmatched_rows: unmatchedRows, lines_total_usd: linesTotal } } : {}),
      };
    }
    default: {
      // The appended "dubbing" and "device allowance" sections below answer their own commands.
      const handled = mockDubbingCommand(name, input) ?? mockDeviceAllowanceCommand(name, input) ?? mockRentalsCommand(name, input);
      if (handled) return handled.value;
      throw unsupportedMock(`engine command ${name}`);
    }
  }
}

const MOCK_GALLERY: GalleryArtifact[] = [
  ['krishna-p04', 'book-1-krishna', 'KDP', 'approved'],
  ['krishna-p07', 'book-1-krishna', 'KDP', 'needs_review'],
  ['puck-p02', 'puck', 'KDP', 'approved'],
  ['hr-hero-dusk', 'hr-hero', 'HR', 'draft'],
  ['puck-p05', 'puck', 'DV', 'needs_review'],
  ['krishna-p11', 'book-1-krishna', 'KDP', 'approved'],
  ['greenhouse-aurora', 'studio', 'DD', 'draft'],
  ['krishna-p19', 'book-1-krishna', 'KDP', 'rejected'],
  ['linen-flatlay', 'rh-spring', 'RH', 'approved'],
  ['toxic-tee-hero', 'ts-drop-3', 'TS', 'rejected'],
  ['unsorted-render', 'misc', 'NA', 'draft']
].map(([name, project, brand, overall], i) => ({
  schema_version: '1.0.0' as const,
  artifact_id: `mock-${name}`,
  artifact_type: 'image' as const,
  absolute_path: mockScene(i + 3),
  project_slug: project,
  brand: brand as GalleryArtifact['brand'],
  visibility: 'primary' as const,
  metadata_status: i === 3 ? ('incomplete' as const) : ('complete' as const),
  approval: {
    eyes: overall === 'approved' ? ('approved' as const) : overall === 'rejected' ? ('rejected' as const) : i % 3 === 2 ? ('rework' as const) : overall === 'draft' ? ('unknown' as const) : ('pending' as const),
    auto_jury: overall === 'rejected' ? ('blocked' as const) : overall === 'draft' ? ('unknown' as const) : i % 4 === 1 ? ('warn_only' as const) : ('passed' as const),
    overall: overall as GalleryArtifact['approval']['overall']
  },
  generation: { provider: i % 2 ? 'fal' : 'openai', model: i % 2 ? 'flux-2-pro' : 'gpt-image-2', chat_tool_version: null, prompt: `Mock prompt for ${name}`, aspect: '1:1', cost_usd: 0.05 },
  sidecars: { eyes_path: null, sidecar_path: null, verdict_path: null, manifest_path: null },
  diagnostics: [],
  // Two mock files stand in for what an engine caller registered (crates/genright-core/src/artifacts.rs `caller_of`).
  caller: i === 2 ? 'postright' : i === 4 ? 'cutright' : null,
  caller_label: i === 2 ? 'App' : i === 4 ? 'App' : null,
  indexed_at: new Date(Date.now() - i * 86_400_000).toISOString()
}));

const MOCK_EYES_STATES = ['unknown', 'pending', 'approved', 'rejected', 'rework', 'stale'];
const MOCK_JURY_STATES = ['unknown', 'pending', 'passed', 'blocked', 'warn_only'];

/** The engine's index filters (crates/genright-core/src/artifacts.rs IndexFilter), with the same rejections. */
function mockIndexFilter(params: Record<string, unknown>): (a: GalleryArtifact) => boolean {
  const text = (key: string): string | undefined => {
    const value = params[key];
    if (value == null) return undefined;
    if (typeof value !== 'string' || !value.trim()) throw new MockEngineError('invalid_input', `${key} must not be empty when given`);
    return value;
  };
  const project = text('project');
  // Mirrors the engine: `no_project` is a boolean, exclusive with `project`, and matches an empty slug.
  const noProject = params.no_project === true;
  if (noProject && project !== undefined) {
    throw new MockEngineError('invalid_input', 'no_project and project cannot be combined; send one of them');
  }
  const brand = text('brand');
  const eyes = text('eyes');
  const jury = text('jury');
  const caller = text('caller');
  if (eyes !== undefined && !MOCK_EYES_STATES.includes(eyes)) {
    throw new MockEngineError('invalid_input', `eyes must be one of ${MOCK_EYES_STATES.join(', ')}; got "${eyes}"`);
  }
  if (jury !== undefined && !MOCK_JURY_STATES.includes(jury)) {
    throw new MockEngineError('invalid_input', `jury must be one of ${MOCK_JURY_STATES.join(', ')}; got "${jury}"`);
  }
  return (a) =>
    (project === undefined || a.project_slug === project) &&
    (!noProject || a.project_slug === '') &&
    (brand === undefined || a.brand.toLowerCase() === brand.toLowerCase()) &&
    (eyes === undefined || a.approval.eyes === eyes) &&
    (jury === undefined || a.approval.auto_jury === jury) &&
    (caller === undefined || a.caller === caller);
}

// ---- price facts and character lock route (browser QA mock): begin ----
// Mirrors what the engine states per model in `media modalities` (catalogue.rs `describe`): `reconciled` on
// the row, `pricing.rounding` on a per-megapixel price. The `models` rows do not carry these fields.
const MOCK_PRICE_FACTS: Record<string, { reconciled?: boolean; pricing?: { kind: string; rounding: 'stated' | 'assumed' } }> = {
  'fal/fal-ai/flux-2/klein/9b': { reconciled: true },
  'wavespeed/kling-3-pro': { reconciled: false },
  'evolink/gpt-image-2': { reconciled: false, pricing: { kind: 'per_megapixel', rounding: 'assumed' } },
  'nanobanana/nano-banana-2': { pricing: { kind: 'per_megapixel', rounding: 'stated' } }
};

function mockModalities(): Record<string, unknown> {
  const kinds = [...new Set(MOCK_MODELS.map((m) => m.kind))];
  return {
    modalities: kinds.map((kind) => {
      const providers = new Map<string, Array<Record<string, unknown>>>();
      for (const m of MOCK_MODELS.filter((x) => x.kind === kind)) {
        const row = { kind, provider: m.provider, model: m.model, label: m.label, est_usd: m.est_usd, ...(MOCK_PRICE_FACTS[`${m.provider}/${m.model}`] ?? {}) };
        providers.set(m.provider, [...(providers.get(m.provider) ?? []), row]);
      }
      return { kind, state: 'available', providers: [...providers].map(([id, models]) => ({ id, models })) };
    }),
    probed: false,
    catalogue: { entries: Object.keys(MOCK_PRICE_FACTS).length, user_file: null, error: null }
  };
}

// Character LoRA routes the mock resolves. The sibling pair mirrors the engine's `X` / `X/edit` rule.
const MOCK_LORA_ROUTE = 'fal/fal-ai/flux-2-lora';
const MOCK_LORA_ROUTE_EDIT = 'fal/fal-ai/flux-2-lora/edit';
const MOCK_LORA_LOCAL_FILE_ROUTE = 'fal/fal-ai/flux-2-local-lora';
const MOCK_FLUX_PRO_NO_REFERENCES = 'fal/fal-ai/flux-2-max';
const MOCK_REFERENCE_MAX: Record<string, number> = { 'wavespeed/qwen-image': 1 };
const MOCK_REFERENCE_ROUTES = ['evolink/gpt-image-2', 'nanobanana/nano-banana-2'];
const MOCK_NO_LORA_INPUT = 'wavespeed/recraft-v4';

/**
 * Browser-QA lock. The base result comes from the character mock; this adds the route fields the engine
 * reports (`requested_model`, `route`, `reconciled`, `ready`) and the refusals the engine gives for a
 * route. Refusals carry only a code and a message, as the engine's do.
 */
async function mockCharacterLockRoute(input: Record<string, unknown>): Promise<unknown> {
  const { mockCharacters } = await import('../views/panels/character-mock.js');
  const base = (await mockCharacters('lock', input)) as Record<string, unknown> & { character?: { id: string; has_lora: boolean; refs: string[] } | null; diagnostics: Array<{ severity: string; message: string }> };
  const requested = `${String(input.provider ?? '')}/${String(input.model ?? '')}`;
  const character = base.character ?? null;
  const hasLora = Boolean(character?.has_lora);
  const refs = character ? [...character.refs] : [];
  const who = character?.id ?? '';
  if (requested === MOCK_LORA_LOCAL_FILE_ROUTE && hasLora) {
    throw new MockEngineError('lora_not_hosted', `The LoRA for ${who} is "/Volumes/D/characters/${character?.id.split('/')[1] ?? 'character'}/lora.safetensors", not an https URL the provider can fetch, so it cannot be sent. App does not upload LoRA weights: the app's only upload (a lock with upload:true) sends reference images to your own fal storage, and character training already returns a hosted URL. Host the file yourself on your own fal storage or another https host, put that https URL in the character's lora manifest, and lock again.`);
  }
  if (MOCK_FLUX_PRO_NO_REFERENCES === requested && !hasLora && refs.length) {
    throw new MockEngineError('reference_route_unsupported', `The served FLUX.2 Pro route (fal-ai/flux-2-max) takes no reference images, and character "${who.split('/')[1] ?? who}" has no trained LoRA, so its reference images cannot lock identity there; use a route that accepts reference images; the engine lists ${MOCK_REFERENCE_ROUTES.join(', ')}.`);
  }
  if (requested in MOCK_REFERENCE_MAX && refs.length > MOCK_REFERENCE_MAX[requested]) {
    throw new MockEngineError('too_many_references', `${refs.length} reference images were supplied, but ${requested} takes at most ${MOCK_REFERENCE_MAX[requested]}.`);
  }
  if (requested === MOCK_NO_LORA_INPUT && hasLora) {
    throw new MockEngineError('lora_unsupported_model', `Model "${requested}" does not support LoRA weights`);
  }
  // A sibling switch: the LoRA route with reference images resolves to its /edit sibling.
  const switchedTo = requested === MOCK_LORA_ROUTE && refs.length > 0 ? MOCK_LORA_ROUTE_EDIT : requested === MOCK_LORA_ROUTE_EDIT && refs.length === 0 ? MOCK_LORA_ROUTE : null;
  const resolved = switchedTo ?? requested;
  const references = switchedTo === MOCK_LORA_ROUTE_EDIT ? refs : resolved === requested ? (base.references as string[]) : [];
  const reconciled = MOCK_PRICE_FACTS[resolved]?.reconciled ?? (resolved === MOCK_LORA_ROUTE_EDIT ? false : undefined);
  const inconsistencies = resolved === MOCK_LORA_ROUTE_EDIT
    ? [{ code: 'served_mismatch', route: MOCK_LORA_ROUTE_EDIT, message: `studio table says in_engine=false for ${MOCK_LORA_ROUTE_EDIT}, the engine serves it` }]
    : [];
  const diagnostics: Array<{ severity: string; message: string }> = [
    ...base.diagnostics.filter((d) => d.severity !== 'notice'),
    ...inconsistencies.map((i) => ({ severity: 'warning', message: i.message })),
    ...(switchedTo ? [{ severity: 'notice', message: `LoRA route ${requested} resolved to its sibling ${resolved} (${references.length ? 'reference images supplied' : 'no reference images'}).` }] : []),
    ...(reconciled === false ? [{ severity: 'notice', message: `${resolved}: price not yet confirmed by a live run.` }] : [])
  ];
  return {
    ...base,
    model: resolved,
    requested_model: requested,
    references,
    pending_upload: references,
    mode: hasLora ? 'lora' : references.length ? 'reference' : 'none',
    route: {
      id: resolved,
      served: true,
      reconciled: reconciled ?? null,
      reconciled_notice: reconciled === false ? 'price not yet confirmed by a live run' : null,
      inconsistencies
    },
    reconciled: reconciled ?? null,
    ready: true,
    diagnostics
  };
}
// ---- price facts and character lock route (browser QA mock): end ----

/**
 * `rightkit_license_*` for @rightkit/license 0.3 in the browser preview: a configured build with no licence
 * (Free). Every mutation is refused; the real client runs natively in the shell.
 */
export async function mockLicenseInvoke(command: string): Promise<unknown> {
  if (command === 'rightkit_license_status') {
    return {
      mode: 'configured', status: 'none', lifecycle: 'unlicensed', tier: null, features: [], kind: null, stage: null,
      validUntil: null, confirmationDueAt: null, staleSince: null, licenseIdSuffix: null, busy: false, unresolved: false,
      revisionState: 'none', message: 'none'
    };
  }
  throw { code: 'unsupported', message: 'unsupported' };
}

export async function mockEngine<T>(method: string, params: Record<string, unknown>): Promise<T> {
  // ---- character lock route (browser QA mock): begin ----
  if (method === 'command' && params.name === 'studio.characters.lock') {
    return (await mockCharacterLockRoute((params.input || {}) as Record<string, unknown>)) as T;
  }
  // ---- character lock route (browser QA mock): end ----
  if (method === 'command' && String(params.name || '').startsWith('local_image.')) {
    return mockLocalImage<T>(String(params.name), (params.input || {}) as Record<string, unknown>);
  }
  if (method === 'command') {
    return mockCommand(String(params.name || ''), (params.input || {}) as Record<string, unknown>) as T;
  }
  if (method === 'index_artifacts') {
    const matched = MOCK_GALLERY.filter(mockIndexFilter(params));
    const offset = Math.min(Number(params.offset) || 0, matched.length);
    const page = matched.slice(offset, params.limit == null ? undefined : offset + Number(params.limit));
    return {
      artifacts: page,
      report: {
        schema_version: '1.0.0',
        indexed_artifacts_total: page.length,
        total_available: matched.length,
        offset,
        has_more: offset + page.length < matched.length,
        metadata_incomplete_count: 1,
        malformed_file_count: 0,
        missing_media_count: 0,
        skipped_path_count: 0,
        fatal_error_count: 0,
        elapsed_ms: 184,
        scanned_roots: (params.roots as string[]) || []
      }
    } as T;
  }
  if (method === 'health') {
    return { ok: true, sidecar: 'browser-qa-mock' } as T;
  }
  if (method === 'get_settings') {
    return { gallery_roots: ['Content'] } as T;
  }
  throw unsupportedMock(`engine method ${method}`);
}

/* ---------- Library search and asset import (browser mock) ----------
 * Shapes and refusals follow docs/ENGINE-CLI.md ("asset import", "library search") and the e2e
 * expectations in crates/genright-e2e/tests/fast_asset_facts.rs. The matcher approximates the
 * engine's boolean query: OR joins clauses and groups read as their terms. Browser QA only.
 */

import type { AssetFacts, AssetScope } from './types/library.js';

const LIBRARY_ORIGIN_VALUES = ['unknown', 'generated', 'captured', 'acquired', 'supplied'];
const LIBRARY_EVIDENCE_VALUES = ['unknown', 'authentic', 'illustrative'];
const LIBRARY_RIGHTS_VALUES = ['unknown', 'unresolved', 'cleared'];
const LIBRARY_FILTER_NAMES = ['kind', 'provider', 'origin', 'rights_status', 'venture', 'channel', 'account'];
const LIBRARY_OPERATORS = ['kind', 'provider', 'model', 'venture', 'channel', 'account', 'origin', 'evidence', 'rights'];
const LIBRARY_SEARCH_KEYS = ['query', 'limit', 'cursor', 'filters'];
const LIBRARY_NOT_INDEXED = ['transcript', 'ocr', 'identity', 'embedding'];
const LIBRARY_IMPORT_ORIGINS = ['supplied', 'acquired', 'captured'];
const LIBRARY_IMPORT_KEYS = ['path', 'origin', 'evidence', 'rights', 'source_url', 'scope', 'project', 'brand', 'dry_run'];
const LIBRARY_RIGHTS_KEYS = ['status', 'licence_evidence', 'attribution', 'permitted_uses', 'consent_refs'];
const LIBRARY_SCOPE_KEYS = ['venture', 'channel', 'account'];
const LIBRARY_SCOPE_ID = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/;
const LIBRARY_MEDIA: Record<string, { mime: string; kind: 'image' | 'video' | 'voice' }> = {
  png: { mime: 'image/png', kind: 'image' },
  jpg: { mime: 'image/jpeg', kind: 'image' },
  jpeg: { mime: 'image/jpeg', kind: 'image' },
  gif: { mime: 'image/gif', kind: 'image' },
  webp: { mime: 'image/webp', kind: 'image' },
  mp4: { mime: 'video/mp4', kind: 'video' },
  mov: { mime: 'video/quicktime', kind: 'video' },
  wav: { mime: 'audio/wav', kind: 'voice' },
  flac: { mime: 'audio/flac', kind: 'voice' },
  mp3: { mime: 'audio/mpeg', kind: 'voice' },
  m4a: { mime: 'audio/mp4', kind: 'voice' }
};

interface MockLibraryRecord {
  asset_id: string;
  job_id: string;
  path: string;
  /** The file the record was imported from; null for generated assets. Used for already_in_library. */
  source_path: string | null;
  sha256: string;
  bytes: number;
  mime: string;
  kind: string;
  provider: string | null;
  model: string | null;
  prompt: string;
  facts: AssetFacts;
  scope: AssetScope | null;
  exists: boolean;
}

interface MockLibraryTerm {
  text: string;
  negated: boolean;
  phrase: boolean;
  op?: string;
  value?: string;
}

const MOCK_LIBRARY_PROMPTS = [
  'Harbour at dusk, long exposure, muted teal palette',
  'Chemist portrait, studio light, shallow depth of field',
  'Linen tote flat lay on oak, soft morning light',
  'Greenhouse interior with aurora reflected on the glass',
  'Ornate book cover with gold leaf border',
  'Bold sans headline over a how-to workbench',
  'Street scene in the rain, neon signs, cinematic grain',
  'Archive photograph of a river mill, restored colour'
];

function mockFacts(origin: Partial<AssetFacts['origin']> & { kind: AssetFacts['origin']['kind'] }, evidence: AssetFacts['evidence'], rights: Partial<AssetFacts['rights']> = {}): AssetFacts {
  return {
    origin: { provider: null, model: null, job_id: null, source: null, parents: [], ...origin },
    evidence,
    rights: { status: 'unknown', licence_evidence: [], source: null, attribution: null, permitted_uses: [], consent_refs: [], ...rights }
  };
}

const MOCK_KDP_SCOPE: AssetScope = { venture: 'orthic', channel: 'kdp-books', account: 'main' };
const MOCK_RIGHTKIT_SCOPE: AssetScope = { venture: 'orthic', channel: 'cutright-how-to', account: 'main' };
const MOCK_DD_SCOPE: AssetScope = { venture: 'orthic', channel: 'dd-site', account: 'main' };

/** Library assets, newest first. One is an old row (origin, evidence and rights not recorded) and one has its file gone. */
const MOCK_LIBRARY_SEED: MockLibraryRecord[] = Array.from({ length: 23 }, (_, i) => {
  const n = i + 1;
  const id = `lib-mock-${String(n).padStart(2, '0')}`;
  const kind = i % 3 === 2 ? 'upscale' : 'image';
  const mime = LIBRARY_MEDIA.png.mime;
  const base = {
    asset_id: id,
    job_id: `job-mock-${n}`,
    path: `browser-qa-mock://library/${id}.png`,
    sha256: stableHash(`library-${n}`).repeat(8),
    bytes: 40_000 + n * 1_731,
    mime,
    kind,
    prompt: `${MOCK_LIBRARY_PROMPTS[i % MOCK_LIBRARY_PROMPTS.length]}, take ${n}`
  };
  if (i === 20) {
    return { ...base, provider: null, model: null, source_path: null, facts: mockFacts({ kind: 'unknown' }, 'unknown'), scope: null, exists: false };
  }
  switch (i % 4) {
    case 0: {
      const [provider, model] = i % 8 === 0 ? ['openai', 'gpt-image-2'] : ['fal', 'flux-2-pro'];
      const parents = i >= 4 ? [`lib-mock-${String(n - 4).padStart(2, '0')}`] : [];
      return {
        ...base,
        provider,
        model,
        source_path: null,
        facts: mockFacts({ kind: 'generated', provider, model, job_id: base.job_id, parents }, 'illustrative', { status: 'unresolved' }),
        scope: MOCK_RIGHTKIT_SCOPE,
        exists: true
      };
    }
    case 1: {
      const sourcePath = `/Users/example/Pictures/harbour-${n}.png`;
      return {
        ...base,
        provider: null,
        model: null,
        source_path: sourcePath,
        facts: mockFacts({ kind: 'supplied', source: sourcePath }, 'authentic', { status: 'cleared', licence_evidence: ['receipt 4417'], attribution: 'Photo by A. Example' }),
        scope: null,
        exists: true
      };
    }
    case 2:
      return {
        ...base,
        provider: null,
        model: null,
        source_path: null,
        facts: mockFacts({ kind: 'captured', source: 'Studio camera roll' }, 'authentic', { status: 'unresolved' }),
        scope: MOCK_DD_SCOPE,
        exists: true
      };
    default:
      return {
        ...base,
        provider: null,
        model: null,
        source_path: null,
        facts: mockFacts({ kind: 'acquired', source: 'https://stock.example/linen' }, 'illustrative', { status: 'cleared', licence_evidence: ['licence LS-22'], attribution: 'Stock photo, licence LS-22' }),
        scope: null,
        exists: true
      };
  }
});

/** Imports and seed records; imports are added to the front, so the newest asset comes first. */
let mockLibraryRecords: MockLibraryRecord[] = MOCK_LIBRARY_SEED;

/** Attaches facts and scope to the browser-QA gallery rows (index_artifacts), so the inspector has real values to show. */
function mockGalleryFacts(row: GalleryArtifact): { asset_facts: AssetFacts; scope: AssetScope | null } {
  const scope = row.brand === 'KDP' ? MOCK_KDP_SCOPE : null;
  switch (row.artifact_id) {
    case 'mock-puck-p02':
      return { asset_facts: mockFacts({ kind: 'supplied', source: '/Users/example/Pictures/puck-p02.png' }, 'authentic', { status: 'cleared', licence_evidence: ['receipt 4417'], attribution: 'Photo by A. Example' }), scope: null };
    case 'mock-linen-flatlay':
      return { asset_facts: mockFacts({ kind: 'acquired', source: 'https://stock.example/linen' }, 'illustrative', { status: 'cleared', licence_evidence: ['licence LS-22'] }), scope: null };
    case 'mock-greenhouse-aurora':
      return { asset_facts: mockFacts({ kind: 'captured', source: 'Studio camera roll' }, 'authentic', { status: 'unresolved' }), scope: MOCK_DD_SCOPE };
    case 'mock-unsorted-render':
      return { asset_facts: mockFacts({ kind: 'unknown' }, 'unknown'), scope: null };
    case 'mock-hr-hero-dusk':
      return { asset_facts: mockFacts({ kind: 'generated', provider: row.generation.provider, model: row.generation.model, job_id: null, parents: ['lib-mock-03'] }, 'illustrative', { status: 'unresolved' }), scope: null };
    default:
      return {
        asset_facts: mockFacts({ kind: 'generated', provider: row.generation.provider, model: row.generation.model, job_id: null, parents: [] }, 'illustrative', { status: 'unresolved' }),
        scope
      };
  }
}

for (const row of MOCK_GALLERY) Object.assign(row, mockGalleryFacts(row));

const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();

function libraryRefusal(code: string, message: string): never {
  throw new MockEngineError(code, message);
}

function unquoted(s: string): string {
  return s.length >= 2 && s.startsWith('"') && s.endsWith('"') ? s.slice(1, -1) : s;
}

/** Scans the query the way the engine's syntax table describes, refusing what it refuses. */
function parseLibraryQuery(query: string): { clauses: MockLibraryTerm[][]; freeWords: string[] } {
  if (query.length > 256) libraryRefusal('invalid_input', 'the query is longer than 256 characters');
  if (/[\u0000-\u001f\u007f]/.test(query)) libraryRefusal('invalid_input', 'the query holds a control character');
  const tokens: string[] = [];
  let i = 0;
  while (i < query.length) {
    const c = query[i];
    if (/\s/.test(c)) {
      i += 1;
      continue;
    }
    if ('(){}'.includes(c)) {
      tokens.push(c);
      i += 1;
      continue;
    }
    const start = i;
    if (c === '-') i += 1;
    if (query[i] === '"') {
      const end = query.indexOf('"', i + 1);
      if (end < 0) libraryRefusal('invalid_input', 'a phrase needs a closing quote');
      i = end + 1;
    } else {
      while (i < query.length && !/[\s(){}"]/.test(query[i])) {
        if (query[i] === ':' && query[i + 1] === '"') {
          const end = query.indexOf('"', i + 2);
          if (end < 0) libraryRefusal('invalid_input', 'a quoted value needs a closing quote');
          i = end + 1;
        } else {
          i += 1;
        }
      }
    }
    tokens.push(query.slice(start, i));
  }

  const stack: string[] = [];
  const pairs: Record<string, string> = { ')': '(', '}': '{' };
  for (let t = 0; t < tokens.length; t += 1) {
    const tok = tokens[t];
    if (tok === '(' || tok === '{') stack.push(tok);
    else if (tok === ')' || tok === '}') {
      if (stack.pop() !== pairs[tok]) libraryRefusal('invalid_input', `unbalanced bracket "${tok}"`);
      if (tokens[t - 1] === (tok === ')' ? '(' : '{')) libraryRefusal('invalid_input', 'an empty group has no terms');
    }
  }
  if (stack.length) libraryRefusal('invalid_input', 'unbalanced bracket: a group is never closed');

  const clauses: MockLibraryTerm[][] = [[]];
  const freeWords: string[] = [];
  for (const raw of tokens) {
    if (raw === '(' || raw === ')' || raw === '{' || raw === '}') continue;
    if (raw === 'OR') {
      if (clauses[clauses.length - 1].length === 0) libraryRefusal('invalid_input', 'OR needs a term on each side');
      clauses.push([]);
      continue;
    }
    const negated = raw.startsWith('-') && raw.length > 1;
    const body = negated ? raw.slice(1) : raw;
    const op = /^([a-z_]+):(.*)$/.exec(body);
    if (op && !body.startsWith('"')) {
      const [, name, rawValue] = op;
      const value = unquoted(rawValue);
      if (!LIBRARY_OPERATORS.includes(name)) libraryRefusal('invalid_input', `unknown operator "${name}:"`);
      if (!value) libraryRefusal('invalid_input', `${name}: needs a value`);
      if (name === 'origin' && !LIBRARY_ORIGIN_VALUES.includes(value)) libraryRefusal('invalid_input', `origin must be one of ${LIBRARY_ORIGIN_VALUES.join(', ')}`);
      if (name === 'evidence' && !LIBRARY_EVIDENCE_VALUES.includes(value)) libraryRefusal('invalid_input', `evidence must be one of ${LIBRARY_EVIDENCE_VALUES.join(', ')}`);
      if (name === 'rights' && !LIBRARY_RIGHTS_VALUES.includes(value)) libraryRefusal('invalid_input', `rights must be one of ${LIBRARY_RIGHTS_VALUES.join(', ')}`);
      clauses[clauses.length - 1].push({ text: body, negated, phrase: false, op: name, value });
    } else if (body.startsWith('"')) {
      const phrase = unquoted(body).trim();
      if (!phrase) libraryRefusal('invalid_input', 'an empty phrase has nothing to search');
      clauses[clauses.length - 1].push({ text: phrase, negated, phrase: true });
      if (!negated) freeWords.push(...phrase.split(/\s+/));
    } else if (/[A-Za-z0-9]/.test(body)) {
      clauses[clauses.length - 1].push({ text: body, negated, phrase: false });
      if (!negated) freeWords.push(body);
    }
  }
  const terms = clauses.flat();
  if (terms.length === 0) {
    libraryRefusal('invalid_input', tokens.length ? 'a query of punctuation only has nothing to search' : 'the query has no word, phrase or operator');
  }
  if (clauses[clauses.length - 1].length === 0) libraryRefusal('invalid_input', 'OR needs a term on each side');
  return { clauses, freeWords };
}

function mockOperatorMatches(record: MockLibraryRecord, op: string, value: string): boolean {
  switch (op) {
    case 'kind':
      return record.kind === value;
    case 'provider':
      return record.provider === value;
    case 'model':
      return record.model === value;
    case 'venture':
    case 'channel':
    case 'account':
      return record.scope?.[op as 'venture' | 'channel' | 'account'] === value;
    case 'origin':
      return record.facts.origin.kind === value;
    case 'evidence':
      return record.facts.evidence === value;
    case 'rights':
      return record.facts.rights.status === value;
    default:
      return false;
  }
}

function mockTermMatches(record: MockLibraryRecord, term: MockLibraryTerm): boolean {
  if (term.op) return mockOperatorMatches(record, term.op, term.value ?? '');
  const haystack = fold([record.prompt, record.kind, record.provider ?? '', record.model ?? ''].join(' '));
  if (term.phrase) return haystack.includes(fold(term.text));
  const word = fold(term.text);
  return haystack.split(/[^a-z0-9]+/).some((token) => token.startsWith(word));
}

/** Wraps each free-text match in [brackets], as FTS5 does for the best-matching field. */
function mockSnippet(prompt: string, words: string[]): string {
  if (words.length === 0) return prompt.slice(0, 120);
  const wanted = words.map(fold);
  return prompt.replace(/[A-Za-z0-9À-ɏ]+/g, (token) => (wanted.some((w) => fold(token).startsWith(w)) ? `[${token}]` : token));
}

function parseLibraryFilters(raw: unknown): Record<string, string> {
  if (raw === undefined || raw === null) return {};
  if (typeof raw !== 'object' || Array.isArray(raw)) libraryRefusal('invalid_input', 'filters must be an object');
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (!LIBRARY_FILTER_NAMES.includes(key)) libraryRefusal('invalid_input', `unknown filter "${key}"`);
    if (typeof value !== 'string' || !value) libraryRefusal('invalid_input', `filter ${key} must be a non-empty string`);
    if (key === 'origin' && !LIBRARY_ORIGIN_VALUES.includes(value)) libraryRefusal('invalid_input', `origin must be one of ${LIBRARY_ORIGIN_VALUES.join(', ')}`);
    if (key === 'rights_status' && !LIBRARY_RIGHTS_VALUES.includes(value)) libraryRefusal('invalid_input', `rights_status must be one of ${LIBRARY_RIGHTS_VALUES.join(', ')}`);
    out[key] = value;
  }
  return out;
}

function mockFilterMatches(record: MockLibraryRecord, filters: Record<string, string>): boolean {
  return Object.entries(filters).every(([key, value]) => {
    switch (key) {
      case 'kind':
        return record.kind === value;
      case 'provider':
        return record.provider === value;
      case 'origin':
        return record.facts.origin.kind === value;
      case 'rights_status':
        return record.facts.rights.status === value;
      default:
        return record.scope?.[key as 'venture' | 'channel' | 'account'] === value;
    }
  });
}

function mockLibrarySearch(input: Record<string, unknown>): unknown {
  for (const key of Object.keys(input)) {
    if (!LIBRARY_SEARCH_KEYS.includes(key)) libraryRefusal('invalid_input', `unknown key "${key}"`);
  }
  if (typeof input.query !== 'string') libraryRefusal('invalid_input', 'query must be text');
  const { clauses, freeWords } = parseLibraryQuery(input.query);
  const limit = input.limit === undefined || input.limit === null ? 20 : Number(input.limit);
  if (!Number.isInteger(limit) || limit < 1 || limit > 200) libraryRefusal('invalid_input', 'limit must be a whole number from 1 to 200');
  let offset = 0;
  if (input.cursor !== undefined && input.cursor !== null) {
    const found = /^o(\d+)$/.exec(String(input.cursor));
    if (!found) libraryRefusal('invalid_input', 'the cursor is not valid for this search; search again without one');
    offset = Number(found[1]);
  }
  const filters = parseLibraryFilters(input.filters);
  const matched = mockLibraryRecords.filter(
    (record) => mockFilterMatches(record, filters) && clauses.some((clause) => clause.every((term) => (term.negated ? !mockTermMatches(record, term) : mockTermMatches(record, term))))
  );
  const page = matched.slice(offset, offset + limit);
  const hits = page.map((record) => ({
    asset_id: record.asset_id,
    job_id: record.job_id,
    path: record.path,
    exists: record.exists,
    kind: record.kind,
    provider: record.provider,
    model: record.model,
    prompt_snippet: mockSnippet(record.prompt, freeWords),
    facts: record.facts,
    scope: record.scope
  }));
  return {
    query: input.query,
    total: matched.length,
    limit,
    filters,
    hits,
    next_cursor: offset + page.length < matched.length ? `o${offset + limit}` : null,
    index: { assets: mockLibraryRecords.length, refreshed: true, rebuilt: false },
    not_indexed: LIBRARY_NOT_INDEXED
  };
}

function mockAssetImport(input: Record<string, unknown>): unknown {
  for (const key of Object.keys(input)) {
    if (!LIBRARY_IMPORT_KEYS.includes(key)) libraryRefusal('invalid_request', `unknown key "${key}"`);
  }
  const origin = input.origin;
  if (origin === 'generated') libraryRefusal('invalid_origin', 'origin generated is for jobs App ran; import supplied, acquired or captured files');
  if (typeof origin !== 'string' || !LIBRARY_IMPORT_ORIGINS.includes(origin)) libraryRefusal('invalid_origin', 'origin must be supplied, acquired or captured');

  const evidence = input.evidence ?? 'unknown';
  if (typeof evidence !== 'string' || !LIBRARY_EVIDENCE_VALUES.includes(evidence)) libraryRefusal('invalid_evidence', 'evidence must be authentic, illustrative or unknown');

  const rights = (input.rights ?? {}) as unknown;
  if (typeof rights !== 'object' || rights === null || Array.isArray(rights)) libraryRefusal('invalid_rights', 'rights must be an object');
  const rightsIn = rights as Record<string, unknown>;
  for (const key of Object.keys(rightsIn)) {
    if (!LIBRARY_RIGHTS_KEYS.includes(key)) libraryRefusal('invalid_rights', `unknown rights field "${key}"`);
  }
  const status = rightsIn.status ?? 'unknown';
  if (typeof status !== 'string' || !LIBRARY_RIGHTS_VALUES.includes(status)) libraryRefusal('invalid_rights', 'rights status must be unknown, unresolved or cleared');
  const licence = rightsIn.licence_evidence ?? [];
  if (!Array.isArray(licence) || licence.some((entry) => typeof entry !== 'string')) libraryRefusal('invalid_rights', 'licence_evidence must be a list of text references');
  const licenceList = (licence as string[]).map((entry) => entry.trim()).filter(Boolean);
  if (status === 'cleared' && licenceList.length === 0) libraryRefusal('invalid_rights', 'cleared rights need licence_evidence: a receipt, licence id or URL');
  if (rightsIn.attribution !== undefined && typeof rightsIn.attribution !== 'string') libraryRefusal('invalid_rights', 'attribution must be text');

  let scope: AssetScope | null = null;
  if (input.scope !== undefined && input.scope !== null) {
    if (typeof input.scope !== 'object' || Array.isArray(input.scope)) libraryRefusal('invalid_scope', 'scope must be an object');
    const scopeIn = input.scope as Record<string, unknown>;
    for (const key of Object.keys(scopeIn)) {
      if (!LIBRARY_SCOPE_KEYS.includes(key)) libraryRefusal('invalid_scope', `unknown scope field "${key}"`);
    }
    for (const key of LIBRARY_SCOPE_KEYS) {
      const value = scopeIn[key];
      if (typeof value !== 'string' || !LIBRARY_SCOPE_ID.test(value)) libraryRefusal('invalid_scope', `scope needs venture, channel and account as ids; ${key} is missing or malformed`);
    }
    scope = { venture: String(scopeIn.venture), channel: String(scopeIn.channel), account: String(scopeIn.account) };
  }

  const path = input.path;
  if (typeof path !== 'string' || !/^(\/|[A-Za-z]:[\\/])/.test(path)) libraryRefusal('invalid_request', 'path must be the absolute path of the file');
  const dot = path.lastIndexOf('.');
  const extension = dot < 0 ? '' : path.slice(dot + 1).toLowerCase();
  const media = LIBRARY_MEDIA[extension];
  if (!media) libraryRefusal('unsupported_media', 'the media probe accepts PNG, JPEG, GIF, WebP, WAV, FLAC, MP3, MP4, MOV and M4A');
  if (/missing/i.test(path)) libraryRefusal('source_not_found', `no file at ${path}`);
  if (mockLibraryRecords.some((record) => record.source_path === path)) libraryRefusal('already_in_library', 'this file is already in the library');

  const facts = mockFacts(
    { kind: origin as AssetFacts['origin']['kind'], source: typeof input.source_url === 'string' ? input.source_url : null },
    evidence as AssetFacts['evidence'],
    { status: status as AssetFacts['rights']['status'], licence_evidence: licenceList, attribution: typeof rightsIn.attribution === 'string' ? rightsIn.attribution : null }
  );
  const sha256 = stableHash(`import:${path}`).repeat(8);
  const bytes = 48_213;
  if (input.dry_run === true) {
    return { dry_run: true, would_import: { path, sha256, bytes, mime: media.mime, facts, scope } };
  }
  const assetId = `lib-mock-import-${mockLibraryRecords.length + 1}`;
  const record: MockLibraryRecord = {
    asset_id: assetId,
    job_id: `job-import-${mockLibraryRecords.length + 1}`,
    path: `browser-qa-mock://library/${assetId}.${extension}`,
    source_path: path,
    sha256,
    bytes,
    mime: media.mime,
    kind: media.kind,
    provider: null,
    model: null,
    prompt: '',
    facts,
    scope,
    exists: true
  };
  mockLibraryRecords = [record, ...mockLibraryRecords];
  return {
    asset_id: record.asset_id,
    job_id: record.job_id,
    path: record.path,
    sha256,
    bytes,
    mime: media.mime,
    exists: true,
    facts,
    scope,
    media: { kind: media.kind, width: null, height: null, duration_s: null },
    storage: 'copied',
    source_preserved: true
  };
}

/** Browser-QA handler for `library.search` and `asset.import`, the commands api.ts routes outside Tauri. */
export async function mockLibraryCommand<T>(name: string, input: Record<string, unknown>): Promise<T> {
  if (name === 'library.search') return mockLibrarySearch(input) as T;
  if (name === 'asset.import') return mockAssetImport(input) as T;
  throw unsupportedMock(`engine command ${name}`);
}

/* ---------- routing: media.route and media.route_prefs (browser mock) ----------
 * Mirrors crates/genright-gen/src/routing.rs: the same outcomes, the same four states, the same refusals
 * and the same preference handling. The catalogue and key store are this mock's own; estimates are the
 * catalogue figures. Nothing here contacts a provider or spends anything. */

const MOCK_ROUTE_KINDS = ['image', 'video', 'voice', 'avatar', 'music', 'sfx', 'upscale', 'lipsync'];
const MOCK_ROUTE_VALUES: Record<string, string[]> = {
  privacy: ['any', 'local_only'],
  spend: ['ask', 'free_only', 'cheapest'],
  quality: ['best', 'balanced', 'economy'],
  time: ['fastest', 'balanced', 'relaxed']
};
const MOCK_ROUTE_NOT_APPLIED = ['quality', 'time'];
const MOCK_ROUTE_NOT_APPLIED_REASON = 'the catalogue records no quality or latency facts';
const mockRoutePrefs: Record<string, string> = { privacy: 'any', spend: 'ask', quality: 'balanced', time: 'balanced' };

interface MockRouteRow {
  provider: string;
  model: string;
  cloud: boolean;
  paid: boolean;
  est_usd: number;
  configured: boolean;
  installed: boolean | null;
  blockers: string[];
  narrowed: boolean;
  excluded: string | null;
}

const mockUsdText = (value: number) => `$${value.toFixed(4)}`;
const mockRouteUsable = (r: MockRouteRow) => r.excluded === null && r.blockers.length === 0;

function mockRouteRows(kind: string, provider: string | null, model: string | null, localOnly: boolean): MockRouteRow[] {
  const named = Boolean(provider && model);
  const freeOnly = mockRoutePrefs.spend === 'free_only';
  return MOCK_MODELS.filter((m) => m.kind === kind).map((m) => {
    const cloud = !KEYLESS_PROVIDERS.has(m.provider);
    const paid = cloud || m.est_usd > 0;
    const configured = !cloud || mockState.configured.has(m.provider);
    // QA scenario, as credentials.status reports it: the qts model files are not on disk.
    const installed = m.provider === 'local-qts' ? false : null;
    const blockers: string[] = [];
    if (!configured) blockers.push(`no key for ${m.provider}`);
    if (installed === false) blockers.push('model not installed');
    const narrowed = !named && ((provider !== null && m.provider !== provider) || (model !== null && m.model !== model));
    let excluded: string | null = null;
    if (!named) {
      if (narrowed) excluded = provider !== null && m.provider !== provider ? `not the requested provider ${provider}` : `not the requested model ${model}`;
      else if (localOnly && cloud) excluded = 'privacy preference excludes cloud routes';
      else if (freeOnly && paid) excluded = 'spend preference is free_only; this route spends money';
    }
    return { provider: m.provider, model: m.model, cloud, paid, est_usd: m.est_usd, configured, installed, blockers, narrowed, excluded };
  }).sort((a, b) => a.est_usd - b.est_usd || a.provider.localeCompare(b.provider) || a.model.localeCompare(b.model));
}

function mockRouteReason(r: MockRouteRow, chosen: MockRouteRow | null, outcome: string): string {
  const parts = [...(r.excluded ? [r.excluded] : []), ...r.blockers];
  if (parts.length) return parts.join('; ');
  if (chosen) {
    const id = `${chosen.provider}/${chosen.model}`;
    if (r.paid && !chosen.paid) return `paid route (${mockUsdText(r.est_usd)}); a usable free route was chosen`;
    if (r.est_usd > chosen.est_usd + 1e-9) return `more expensive than ${id} by ${mockUsdText(r.est_usd - chosen.est_usd)}`;
    if (r.est_usd < chosen.est_usd - 1e-9) return `${mockUsdText(chosen.est_usd - r.est_usd)} cheaper than ${id}; not chosen: lower priority`;
    return `same price as ${id}; ordered after it by provider and model`;
  }
  if (outcome === 'route_choice_required') return `usable paid route (${mockUsdText(r.est_usd)}); spend needs your choice before any is used`;
  return 'usable';
}

function mockRouteEntry(r: MockRouteRow, reason: string) {
  return {
    provider: r.provider,
    model: r.model,
    cloud: r.cloud,
    states: { configured: r.configured, installed: r.installed, qualified: r.cloud ? true : null, usable: r.blockers.length === 0 },
    est_usd: r.est_usd,
    est_source: 'default_output',
    eligible: r.excluded === null,
    reason
  };
}

function mockRouteDecide(input: Record<string, unknown>): unknown {
  const kind = typeof input.kind === 'string' ? input.kind : '';
  if (!MOCK_ROUTE_KINDS.includes(kind)) throw new MockEngineError('unknown_kind', `unknown kind ${JSON.stringify(input.kind ?? '')}`);
  const provider = typeof input.provider === 'string' && input.provider ? input.provider : null;
  const model = typeof input.model === 'string' && input.model ? input.model : null;
  const localOnly = input.privacy === 'local_only' || mockRoutePrefs.privacy === 'local_only';
  const named = Boolean(provider && model);
  const rows = mockRouteRows(kind, provider, model, localOnly);
  const sel = mockState.modelRoutes[kind];
  const selLabel = sel ? (sel.model ? `${sel.route} ${sel.model}` : sel.route) : '';

  // Decide: the outcome, its basis, the chosen row (if any), the note and the selection note.
  let outcome = 'no_usable_route';
  let basis = 'none';
  let chosen: MockRouteRow | null = null;
  let note = '';
  let selectionNote: string | null = null;

  if (named) {
    const hit = rows.find((r) => r.provider === provider && r.model === model) ?? null;
    basis = 'request';
    if (!hit) {
      note = `${provider}/${model} is not offered for ${kind}`;
    } else {
      outcome = 'chosen';
      chosen = hit;
      note = 'named in the request; routing never overrides it';
      if (localOnly && hit.cloud) note += '; privacy preference is local_only but the request named this cloud route';
      if (mockRoutePrefs.spend === 'free_only' && hit.paid) note += '; spend preference is free_only but the request named this paid route';
    }
  } else if (provider || model) {
    const pool = rows.filter((r) => !r.narrowed);
    basis = 'request';
    if (pool.length === 0) {
      note = `no ${kind} route matches the request`;
    } else if (pool.length === 1) {
      if (pool[0].excluded) {
        note = pool[0].excluded;
      } else {
        outcome = 'chosen';
        chosen = pool[0];
        note = 'the only route matching the request';
      }
    } else if (model) {
      outcome = 'ambiguous_route';
      note = `model ${model} is offered by ${pool.map((r) => r.provider).join(', ')}; pass provider explicitly`;
    } else {
      outcome = 'route_choice_required';
      note = `${provider} offers several ${kind} models; name one`;
    }
  } else {
    if (sel && (sel.route !== 'auto' || sel.model)) {
      if (sel.route === 'rented') {
        selectionNote = 'saved selection is rented: rented workers are not provider routes (use studio.models.worker.action)';
      } else {
        const hit = rows.find((r) => mockRouteUsable(r) && (sel.route === 'local' ? !r.cloud : sel.route === 'api' ? r.cloud : true) && (!sel.model || sel.model === r.model));
        if (hit) {
          outcome = 'chosen';
          basis = 'saved_selection';
          chosen = hit;
          note = `saved selection for ${kind}: ${selLabel}`;
        } else {
          selectionNote = `saved selection (${selLabel}) is unusable; fell back`;
        }
      }
    }
    if (!chosen && outcome !== 'chosen') {
      const free = rows.filter((r) => mockRouteUsable(r) && !r.paid);
      const paid = rows.filter((r) => mockRouteUsable(r) && r.paid);
      if (free.length) {
        outcome = 'chosen';
        basis = 'free_route';
        chosen = free[0];
        note = free.length === 1 ? 'the only usable free route; no spend' : `lowest-estimate of ${free.length} usable free routes (ties by provider, then model); no spend`;
      } else if (paid.length && mockRoutePrefs.spend === 'cheapest') {
        outcome = 'chosen';
        basis = 'spend_preference';
        chosen = paid[0];
        note = `spend preference cheapest: lowest-estimate usable paid route (${mockUsdText(paid[0].est_usd)})`;
      } else if (paid.length) {
        outcome = 'route_choice_required';
        note = `no usable free route and ${paid.length} usable paid route(s); nothing names paid spending (saved selection or spend preference cheapest), so the choice is yours`;
      } else {
        note = `no usable ${kind} route; see each alternative's reason`;
      }
    }
  }

  const others = rows.filter((r) => r !== chosen);
  // Usable and eligible first; the stable sort keeps the estimate order inside each group.
  const ordered = [...others.filter((r) => mockRouteUsable(r)), ...others.filter((r) => !mockRouteUsable(r))];
  const chosenReason = chosen ? (chosen.blockers.length ? `${note}; will be refused: ${chosen.blockers.join('; ')}` : note) : null;
  return {
    kind,
    outcome,
    route_choice_required: outcome === 'route_choice_required',
    basis,
    chosen: chosen && chosenReason ? mockRouteEntry(chosen, chosenReason) : null,
    alternatives: ordered.map((r) => mockRouteEntry(r, mockRouteReason(r, chosen, outcome))),
    preferences: { ...mockRoutePrefs, privacy: localOnly ? 'local_only' : mockRoutePrefs.privacy, ordering_note: 'quality and time are recorded but do not order routes: the catalogue holds no quality or latency facts' },
    selection: sel ? { route: sel.route, model: sel.model ?? null } : null,
    selection_note: selectionNote,
    note,
    preferences_not_applied: MOCK_ROUTE_NOT_APPLIED,
    preferences_not_applied_reason: MOCK_ROUTE_NOT_APPLIED_REASON
  };
}

function mockRoutePrefsCommand(input: Record<string, unknown>): unknown {
  const keys = Object.keys(input);
  if (keys.length > 0) {
    const next = { ...mockRoutePrefs };
    for (const key of keys) {
      if (!Object.hasOwn(MOCK_ROUTE_VALUES, key)) {
        throw new MockEngineError('invalid_input', `unknown routing preference ${JSON.stringify(key)}; accepted: privacy, spend, quality, time`);
      }
      const allowed = MOCK_ROUTE_VALUES[key];
      const value = input[key];
      if (typeof value !== 'string' || !allowed.includes(value)) {
        throw new MockEngineError('invalid_input', `${key} must be one of [${allowed.map((v) => `"${v}"`).join(', ')}]`);
      }
      next[key] = value;
    }
    Object.assign(mockRoutePrefs, next);
  }
  return {
    preferences: { ...mockRoutePrefs, ordering_note: 'quality and time are recorded but do not order routes: the catalogue holds no quality or latency facts' },
    saved_under: 'routing.preferences'
  };
}

/** Routing commands for the browser mock. Called by lib/api.ts only when the app runs without Tauri. */
export async function mockRouting<T>(method: string, input: Record<string, unknown>): Promise<T> {
  if (method === 'media.route') return mockRouteDecide(input) as T;
  if (method === 'media.route_prefs') return mockRoutePrefsCommand(input) as T;
  return unsupportedMock(method);
}

// ===========================================================================
// RECIPE TEMPLATES (begin): browser preview of studio.recipes.templates, template_compose,
// template_preflight, template_approve, character_batch, batch_review, and the request hash
// of a draft from studio.approvals.challenge. The engine owns every rule and code; this mirrors
// its shapes for the browser only. The served routes are a preview fixture, not the catalogue,
// and the estimates are fixtures, not dry runs. Template text and slots come from the real
// crates/genright-studio/data/recipes/templates.json.
// ===========================================================================

import recipeTemplateData from '../studio-data/recipes/templates.json' with { type: 'json' };

type RtRecord = Record<string, unknown>;

interface RtServedRoute {
  provider: string;
  model: string;
  kind: string;
  refParam: string;
  videoParam: string;
  maxReference: number;
  resolution?: string[];
  estUsd: number;
}

interface RtSlot {
  position: number;
  name: string;
  type: string;
  engine_role: string;
  required: boolean;
  max: number;
  role: string;
}

const RT_PREVIEW_NOTE = 'Browser preview: served routes and estimates are fixtures. The engine dry run decides in the app.';

/** Preview fixture of served routes. Object swap is absent, as the engine documents for object_replacement. */
const RT_SERVED_ROUTES: RtServedRoute[] = [
  { provider: 'fal', model: 'fal-ai/kling-video/v3/pro/motion-control', kind: 'motion_control', refParam: 'image_url', videoParam: 'video_url', maxReference: 1, resolution: ['720p', '1080p'], estUsd: 0.84 },
  { provider: 'fal', model: 'fal-ai/kling-video/v3/standard/motion-control', kind: 'motion_control', refParam: 'image_url', videoParam: 'video_url', maxReference: 1, estUsd: 0.56 },
  { provider: 'wavespeed', model: 'kwaivgi/kling-v3.0-pro/motion-control', kind: 'motion_control', refParam: 'image', videoParam: 'video', maxReference: 1, estUsd: 0.8 },
  { provider: 'wavespeed', model: 'kwaivgi/kling-v3.0-std/motion-control', kind: 'motion_control', refParam: 'image', videoParam: 'video', maxReference: 1, estUsd: 0.5 },
  { provider: 'wavespeed', model: 'wavespeed-ai/wan-2.2/animate-2', kind: 'motion_control', refParam: 'image', videoParam: 'video', maxReference: 1, estUsd: 0.4 }
];

/** Preview fixture: only this provider takes character references in the batch. */
const RT_BATCH_PROVIDER = 'fal';

const RT_DATA = recipeTemplateData as unknown as {
  _meta: RtRecord;
  generic_moderation: RtRecord;
  templates: Record<string, RtRecord>;
  batches: Record<string, RtRecord>;
};

const rtDrafts = new Map<string, RtRecord>();
const rtBatches = new Map<string, RtRecord>();
let rtSeq = 0;

function rtText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function rtHash(text: string): string {
  let out = '';
  for (let seed = 0; seed < 4; seed += 1) {
    let h = 2166136261 ^ seed;
    const src = `${seed}:${text}`;
    for (let i = 0; i < src.length; i += 1) {
      h ^= src.charCodeAt(i);
      h = Math.imul(h, 16777619) >>> 0;
    }
    out += h.toString(16).padStart(8, '0');
  }
  return out;
}

function rtMediaType(value: string): string | null {
  const path = value.split(/[?#]/)[0] ?? '';
  const dot = path.lastIndexOf('.');
  if (dot < 0) return null;
  const ext = path.slice(dot + 1).toLowerCase();
  if (['png', 'jpg', 'jpeg', 'webp', 'gif', 'avif', 'heic', 'bmp', 'tif', 'tiff'].includes(ext)) return 'image';
  if (['mp4', 'mov', 'webm', 'm4v', 'mkv', 'avi'].includes(ext)) return 'video';
  return null;
}

function rtTemplate(name: string): RtRecord {
  if (!Object.hasOwn(RT_DATA.templates, name)) throw new MockEngineError('template_not_found', `No recipe template named "${name}"`);
  return RT_DATA.templates[name];
}

function rtSlots(t: RtRecord): RtSlot[] {
  return (t.slots as RtSlot[]) ?? [];
}

function rtRoutes(t: RtRecord): Array<{ provider: string; model: string }> {
  return (t.routes as Array<{ provider: string; model: string }>) ?? [];
}

function rtServed(provider: string, model: string, kind: string): RtServedRoute | undefined {
  return RT_SERVED_ROUTES.find((r) => r.provider === provider && r.model === model && r.kind === kind);
}

function rtModeration(t: RtRecord): RtRecord {
  return {
    notes: t.moderation,
    generic: [RT_DATA.generic_moderation],
    note: 'Shown before submit. Notes with generic:false come from text already in the repository (source named); generic:true notes are general warnings, not provider facts.'
  };
}

function rtFail(code: string, message: string, violations: RtRecord[] = []): RtRecord {
  return { ok: false, code, message, violations };
}

function rtViolation(position: number, slot: string, code: string, problem: string): RtRecord {
  return { position, slot, code, problem };
}

function rtTemplatesList(): RtRecord {
  const templates = Object.entries(RT_DATA.templates).map(([name, t]) => {
    const kind = String(t.kind);
    const routes = rtRoutes(t).map((r) => ({ provider: r.provider, model: r.model, served: rtServed(r.provider, r.model, kind) !== undefined }));
    return {
      template: name,
      title: t.title,
      kind,
      served: routes.some((r) => r.served),
      routes,
      slots: t.slots,
      prompt: t.prompt,
      staged_resolution: t.staged_resolution ?? null,
      moderation: rtModeration(t)
    };
  });
  return { ok: true, templates, batches: RT_DATA.batches, slot_rules: RT_DATA._meta.slot_rules };
}

function rtCheckSlots(declared: RtSlot[], given: RtRecord[]): RtRecord[] {
  const violations: RtRecord[] = [];
  const order = declared.map((d) => `${d.position} ${d.name}`).join(', ');
  if (given.length > declared.length) {
    violations.push(rtViolation(declared.length + 1, '', 'slot_overflow', `${given.length} slots given but the template declares ${declared.length}: ${order}`));
  }
  declared.forEach((d, i) => {
    const g = given[i];
    if (!g) {
      if (d.required) violations.push(rtViolation(d.position, d.name, 'slot_missing', `${d.name} is required and may not be omitted`));
      return;
    }
    if (g.slot !== undefined && g.slot !== null && String(g.slot) !== String(d.position) && g.slot !== d.name) {
      violations.push(rtViolation(d.position, d.name, 'slot_order', `this entry names slot ${String(g.slot)} but sits at position ${d.position}`));
    }
    const value = rtText(g.value);
    if (g.type !== d.type) {
      violations.push(rtViolation(d.position, d.name, 'slot_type', `${d.name} takes ${d.type}, got ${String(g.type)}`));
    } else if (!value) {
      violations.push(rtViolation(d.position, d.name, 'slot_value', `${d.name} has no path or URL`));
    } else {
      const media = rtMediaType(value);
      if (media && media !== d.type) violations.push(rtViolation(d.position, d.name, 'slot_value', `${value} is a ${media} file, but ${d.name} takes ${d.type}`));
    }
  });
  return violations;
}

function rtStagePlan(t: RtRecord, chosen: RtServedRoute, callerParams: RtRecord): RtRecord {
  const staged = t.staged_resolution as RtRecord | null;
  const param = String(staged?.param ?? 'resolution');
  const unavailable = (reason: string): RtRecord => ({ available: false, param, reason });
  if (!chosen.resolution) {
    return unavailable(`${chosen.provider}/${chosen.model} serves no ${param} parameter, so there is no lower resolution to draft at`);
  }
  const requested = callerParams[param];
  if (typeof requested !== 'string') return unavailable(`params.${param} (the final resolution) is required for staged resolution`);
  const options = [...chosen.resolution];
  const lowest = options[0];
  return {
    available: true,
    param,
    options,
    lowest,
    requested,
    single_stage: lowest === requested,
    identical_prompt: true,
    second_requires_first_approval: true
  };
}

function rtCompose(input: RtRecord): RtRecord {
  const name = rtText(input.template);
  const t = rtTemplate(name);
  const declared = rtSlots(t);
  const given = Array.isArray(input.slots) ? (input.slots as RtRecord[]) : [];
  if (given.length === 0) {
    return rtFail('slots_required', `slots must list the template's reference slots in order: ${declared.map((d) => `${d.position} ${d.name}`).join(', ')}`);
  }
  const violations = rtCheckSlots(declared, given);
  if (violations.length) {
    const names = violations.map((v) => `${String(v.slot) || 'overflow'} (${String(v.code)})`).join(', ');
    return rtFail(String(violations[0].code), `slot violations: ${names}`, violations);
  }

  const kind = String(t.kind);
  const routes = rtRoutes(t);
  const provider = rtText(input.provider);
  const model = rtText(input.model);
  if ((provider && !model) || (!provider && model)) return rtFail('invalid_input', 'provider and model go together');
  if (provider && model && !routes.some((r) => r.provider === provider && r.model === model)) {
    const list = routes.map((r) => `${r.provider}/${r.model}`).join(', ');
    return rtFail('route_not_in_template', `${provider}/${model} is not a route of template "${name}". Its routes: ${list}`);
  }
  const candidates = provider && model ? [{ provider, model }] : routes;
  const chosen = candidates
    .map((c) => rtServed(c.provider, c.model, kind))
    .find((s): s is RtServedRoute => s !== undefined);
  if (!chosen) {
    return {
      ok: false,
      status: 'unsupported',
      code: 'route_not_served',
      template: name,
      kind,
      missing_routes: candidates,
      message: `No ${kind} route is served for template "${name}": the catalogue has no entry for ${candidates.map((c) => `${c.provider}/${c.model}`).join(', ')}. Nothing was routed or guessed.`,
      violations: []
    };
  }

  const callerParams: RtRecord = input.params && typeof input.params === 'object' ? (input.params as RtRecord) : {};
  const mapped: RtRecord = {};
  const rows: RtRecord[] = [];
  const unservable: RtRecord[] = [];
  let referenceCount = 0;
  declared.forEach((d, i) => {
    const g = given[i];
    if (!g) return;
    const value = rtText(g.value);
    let param: string;
    if (d.engine_role === 'reference') {
      referenceCount += 1;
      if (referenceCount > chosen.maxReference) {
        unservable.push(rtViolation(d.position, d.name, 'slot_unservable', `${chosen.provider}/${chosen.model} takes at most ${chosen.maxReference} reference input(s)`));
        return;
      }
      param = chosen.refParam;
    } else {
      param = chosen.videoParam;
    }
    mapped[param] = value;
    rows.push({ position: d.position, name: d.name, param, value });
  });
  if (unservable.length) {
    const names = unservable.map((v) => String(v.slot)).join(', ');
    return rtFail('slot_unservable', `${chosen.provider}/${chosen.model} cannot take slot(s): ${names}`, unservable);
  }
  const conflict = Object.keys(callerParams).find((k) => Object.hasOwn(mapped, k));
  if (conflict) return rtFail('param_conflict', `params.${conflict} is filled by a slot; remove it from params`);

  const params: RtRecord = { ...callerParams, ...mapped };
  const prompt = [String((t.prompt as RtRecord).lead), String((t.prompt as RtRecord).lock), rtText(input.prompt)]
    .filter((p) => p.length > 0)
    .join(' ')
    .replace(/\s+/g, ' ')
    .trim();
  return {
    ok: true,
    template: name,
    kind,
    route: { provider: chosen.provider, model: chosen.model, label: chosen.model, endpoint: 'preview fixture' },
    slots: rows,
    request: { kind, provider: chosen.provider, model: chosen.model, prompt, params },
    est_usd: chosen.estUsd,
    would_submit: false,
    moderation: rtModeration(t),
    staged: rtStagePlan(t, chosen, callerParams),
    warnings: [RT_PREVIEW_NOTE]
  };
}

function rtChallenge(doc: RtRecord): RtRecord {
  rtSeq += 1;
  return {
    kind: 'recipe_templates',
    draft_id: doc.draft_id,
    nonce: rtHash(`nonce:${rtSeq}:${String(doc.draft_id)}`).slice(0, 32),
    input_hash: rtHash(`input:${String(doc.request_hash)}`),
    legacy_hash: doc.request_hash,
    expires_unix: Math.floor(Date.now() / 1000) + 30 * 60
  };
}

function rtStoreDraft(doc: RtRecord): void {
  rtDrafts.set(String(doc.draft_id), doc);
}

function rtPreflight(input: RtRecord): RtRecord {
  const composed = rtCompose(input);
  if (composed.ok !== true) return composed;
  const staged = input.staged === true;
  const plan = composed.staged as RtRecord;
  const request = composed.request as RtRecord;
  const stages: Array<{ stage: string; request: RtRecord }> = [];
  if (staged) {
    if (plan.available !== true) return rtFail('staged_unavailable', String(plan.reason), []);
    if (plan.single_stage === true) {
      stages.push({ stage: 'single', request });
    } else {
      const draftParams = { ...(request.params as RtRecord), [String(plan.param)]: plan.lowest };
      stages.push({ stage: 'draft', request: { ...request, params: draftParams } }, { stage: 'final', request });
    }
  } else {
    stages.push({ stage: 'single', request });
  }

  let previous: string | null = null;
  const drafts: RtRecord[] = [];
  for (const s of stages) {
    const hash = rtHash(JSON.stringify(s.request));
    rtSeq += 1;
    const id = `tpl-${rtText(input.template)}-${rtSeq}-${hash.slice(0, 6)}`;
    const doc: RtRecord = {
      draft_id: id,
      status: 'draft',
      stage: s.stage,
      template: rtText(input.template),
      request: s.request,
      request_hash: hash,
      requires_approval_of: previous,
      est_usd: composed.est_usd,
      route: composed.route,
      slots: composed.slots,
      moderation: composed.moderation,
      warnings: composed.warnings
    };
    rtStoreDraft(doc);
    drafts.push({
      draft_id: id,
      stage: s.stage,
      status: 'draft',
      requires_approval_of: previous,
      est_usd: composed.est_usd,
      request_hash: hash,
      request: s.request,
      approval_challenge: rtChallenge(doc)
    });
    previous = id;
  }
  return {
    ok: true,
    template: composed.template,
    kind: composed.kind,
    route: composed.route,
    staged,
    stage_plan: plan,
    note: staged && drafts.length === 1 ? 'The requested resolution is already the lowest the route serves; one request, no draft stage.' : null,
    drafts,
    slots: composed.slots,
    moderation: composed.moderation,
    warnings: composed.warnings
  };
}

/** Approve a stored template draft the way the engine does; the app's approval token is the caller's job. */
function rtApproveDraft(draftId: string, displayedHash: string): RtRecord {
  const doc = rtDrafts.get(draftId);
  if (!doc) throw new MockEngineError('draft_not_found', `Template draft not found: ${draftId}`);
  if (doc.status === 'rejected') throw new MockEngineError('draft_rejected', `Draft ${draftId} was rejected; preflight again`);
  if (displayedHash !== doc.request_hash) throw new MockEngineError('draft_changed', 'The draft is not the one that was shown; preflight again.');
  if (doc.identity_attached === false) {
    throw new MockEngineError(
      'identity_not_attached',
      `Draft ${draftId} has no character identity attached (local references are not uploaded); rebuild the batch with https references or upload:true`
    );
  }
  if (typeof doc.requires_approval_of === 'string') {
    const dep = rtDrafts.get(doc.requires_approval_of);
    if (!dep || dep.status !== 'approved') {
      throw new MockEngineError('dependency_unapproved', `Draft ${draftId} is the ${String(doc.stage)} stage; approve its draft stage ${doc.requires_approval_of} first`);
    }
  }
  doc.status = 'approved';
  doc.approved_at = new Date().toISOString();
  return { ok: true, draft: { ...doc }, generate: doc.request };
}

function rtBatchView(batch: RtRecord): RtRecord {
  const approvalsByDraft = (batch.challenges ?? {}) as Record<string, RtRecord>;
  const rows = (batch.items as RtRecord[]).map((item) => {
    const index = item.index;
    const scenario = item.scenario;
    if (!item.draft_id) return { index, scenario, state: 'refused', draft_id: null, refusal: item.refusal, asset: null };
    const draftId = String(item.draft_id);
    const doc: RtRecord = rtDrafts.get(draftId) ?? {};
    const status = String(doc.status ?? 'draft');
    const state = status === 'approved' ? 'approved' : status === 'rejected' ? 'rejected' : 'pending';
    const row: RtRecord = {
      index,
      scenario,
      state,
      draft_id: draftId,
      prompt: (doc.request as RtRecord | undefined)?.prompt ?? '',
      est_usd: doc.est_usd ?? null,
      identity_attached: doc.identity_attached ?? null,
      asset: doc.asset ?? null,
      refusal: null
    };
    if (approvalsByDraft[draftId]) row.approval_challenge = approvalsByDraft[draftId];
    return row;
  });
  const count = (state: string) => rows.filter((r) => r.state === state).length;
  return {
    ok: true,
    batch_id: batch.batch_id,
    template: 'character_reference_batch',
    character_id: batch.character_id,
    lock: batch.lock,
    review: batch.review,
    counts: { items: rows.length, pending: count('pending'), approved: count('approved'), rejected: count('rejected'), refused: count('refused') },
    items: rows
  };
}

function rtCharacterBatch(input: RtRecord): RtRecord {
  const batchSpec = RT_DATA.batches.character_reference_batch;
  const characterId = rtText(input.character_id);
  const provider = rtText(input.provider);
  const model = rtText(input.model);
  if (!characterId) throw new MockEngineError('invalid_input', 'character_id is required');
  if (!provider) throw new MockEngineError('invalid_input', 'provider is required');
  if (!model) throw new MockEngineError('invalid_input', 'model is required');
  if (!Array.isArray(input.scenarios)) throw new MockEngineError('invalid_input', 'scenarios must be an array of scenario lines');
  const lines = (input.scenarios as unknown[]).map((s) => rtText(s)).filter((s) => s.length > 0);
  const max = Number(batchSpec.max_items);
  if (lines.length === 0) throw new MockEngineError('invalid_input', 'at least one scenario line is required');
  if (lines.length > max) throw new MockEngineError('invalid_input', `a batch takes at most ${max} scenario lines, got ${lines.length}`);

  const references = Array.isArray(input.references) ? (input.references as unknown[]).map((r) => rtText(r)).filter((r) => r.length > 0) : [];
  const identityAttached = input.upload === true || (references.length > 0 && references.every((r) => /^https:\/\//i.test(r)));
  const lockRefused = provider !== RT_BATCH_PROVIDER;
  const clause = String((batchSpec.prompt as RtRecord).lock_clauses && ((batchSpec.prompt as RtRecord).lock_clauses as RtRecord).reference);
  const batchId = `chrbatch-${characterId}-${rtHash(lines.join('\n')).slice(0, 6)}-${++rtSeq}`;
  const challenges: Record<string, RtRecord> = {};
  const items = lines.map((scenario, i) => {
    const index = i + 1;
    if (lockRefused) {
      return {
        index,
        scenario,
        draft_id: null,
        refusal: { code: 'reference_route_unsupported', message: `${provider}/${model} does not take character references` }
      };
    }
    const prompt = tidyPrompt(`${scenario} ${clause}`);
    const request = { kind: 'image', provider, model, prompt, params: { references } };
    const hash = rtHash(JSON.stringify(request));
    const draftId = `tpl-character-batch-${index}-${batchId.slice(-8)}-${hash.slice(0, 6)}`;
    const doc: RtRecord = {
      draft_id: draftId,
      status: 'draft',
      stage: 'single',
      template: 'character_reference_batch',
      batch_id: batchId,
      request,
      request_hash: hash,
      requires_approval_of: null,
      est_usd: null,
      identity_attached: identityAttached
    };
    rtStoreDraft(doc);
    challenges[draftId] = rtChallenge(doc);
    return { index, scenario, draft_id: draftId, refusal: null };
  });
  const manifest: RtRecord = {
    batch_id: batchId,
    character_id: characterId,
    lock: { state: lockRefused ? 'refused' : 'locked', provider, model, references_count: references.length },
    review: { mode: 'data_only', compositing: 'Not implemented in this slice: the contact sheet is a list of the batch items with a review state each, not a composited image.' },
    items
  };
  rtBatches.set(batchId, manifest);
  return rtBatchView({ ...manifest, challenges });
}

function tidyPrompt(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

function rtBatchReview(input: RtRecord): RtRecord {
  const id = rtText(input.batch_id);
  const batch = rtBatches.get(id);
  if (!batch) throw new MockEngineError('batch_not_found', `Character batch not found: ${id}`);
  const decisions = Array.isArray(input.decisions) ? (input.decisions as RtRecord[]) : [];
  const items = batch.items as RtRecord[];
  const plan: Array<{ draftId: string; decision: string }> = [];
  for (const d of decisions) {
    const index = Number(d.item);
    const item = items.find((i) => Number(i.index) === index);
    if (!item) throw new MockEngineError('item_not_found', `Batch ${id} has no item ${index}`);
    if (!item.draft_id) throw new MockEngineError('item_refused', `Item ${index} was refused by the character lock and cannot be reviewed`);
    const decision = rtText(d.decision);
    if (decision !== 'approve' && decision !== 'reject') throw new MockEngineError('invalid_input', 'decision must be approve or reject');
    const draftId = String(item.draft_id);
    const status = String(rtDrafts.get(draftId)?.status ?? '');
    if ((decision === 'approve' && status === 'rejected') || (decision === 'reject' && status === 'approved')) {
      throw new MockEngineError('decision_final', `Item ${index} is already ${status}; preflight a new batch to change it`);
    }
    if (decision === 'approve' && (typeof d.approval_token !== 'string' || !d.approval_token)) {
      throw new MockEngineError('approval_requires_app', APPROVAL_REQUIRES_APP);
    }
    plan.push({ draftId, decision });
  }
  for (const { draftId, decision } of plan) {
    if (decision === 'approve') {
      rtApproveDraft(draftId, String(rtDrafts.get(draftId)?.request_hash ?? ''));
    } else {
      const doc = rtDrafts.get(draftId);
      if (doc) {
        doc.status = 'rejected';
        doc.rejected_at = new Date().toISOString();
      }
    }
  }
  return rtBatchView(batch);
}

/** Browser-preview dispatch for the recipe-template commands. Called by lib/api.ts only outside Tauri. */
export async function mockRecipeTemplates<T>(name: string, input: Record<string, unknown>): Promise<T> {
  switch (name) {
    case 'studio.recipes.templates':
      return rtTemplatesList() as T;
    case 'studio.recipes.template_compose':
      return rtCompose(input) as T;
    case 'studio.recipes.template_preflight':
      return rtPreflight(input) as T;
    case 'studio.recipes.template_approve':
      // The engine accepts an approval only with a token the app minted; the preview never mints one.
      throw new MockEngineError('approval_requires_app', APPROVAL_REQUIRES_APP);
    case 'studio.recipes.character_batch':
      return rtCharacterBatch(input) as T;
    case 'studio.recipes.batch_review':
      return rtBatchReview(input) as T;
    case 'studio.approvals.challenge': {
      const draftId = rtText(input.draft_id);
      const doc = rtDrafts.get(draftId);
      if (rtText(input.kind) !== 'recipe_templates') throw new MockEngineError('invalid_input', 'the preview answers recipe_templates challenges only');
      if (!doc) throw new MockEngineError('draft_not_found', `Template draft not found: ${draftId}`);
      return rtChallenge(doc) as T;
    }
    default:
      return unsupportedMock(name);
  }
}

/** Browser-preview approval of one recipe-template draft; the app path is approveDraft in lib/api.ts. */
export async function mockRecipeTemplateApprove<T>(draftId: string, displayedHash: string): Promise<T> {
  if (!draftId.trim() || !displayedHash.trim()) throw new MockEngineError('invalid_input', 'approve_draft needs the draft id and the hash the user was shown');
  return rtApproveDraft(draftId.trim(), displayedHash.trim()) as T;
}
// RECIPE TEMPLATES (end)

// DUBBING (begin): `studio.pipelines.dubbing_plan`, the arithmetic of crates/genright-studio dubbing.rs
// (plan_value and place). The mirror takes `synth_ms` per segment; `audio_path` needs ffprobe, so it is
// refused rather than guessed. It writes no files (`out_dir` is refused) and renders no track.
type MockDubSeg = { id: string; start: number; end: number; src: string; text: string; synth: number };
type MockDubPlaced = { action: string; tempo: number | null; start: number; ms: number; slot: number; shift: number; excess: number; overlap: number };
const MOCK_DUB_FIT_TOLERANCE_MS = 60;
const mockDubRound4 = (v: number) => Math.round(v * 10000) / 10000;
const mockDubTruthy = (v: unknown) => v === true || (typeof v === 'string' && ['true', '1', 'yes'].includes(v.trim().toLowerCase()));
const mockDubStr = (v: unknown) => (typeof v === 'string' ? v.trim() : '');
const mockDubLang = (v: unknown) => mockDubStr(v).toLowerCase().replace(/_/g, '-');
function mockDubNumber(v: unknown): number | null {
  if (typeof v === 'number') return Number.isFinite(v) ? v : null;
  if (typeof v === 'string' && v.trim() !== '') {
    const n = Number(v.trim());
    return Number.isFinite(n) ? n : null;
  }
  return null;
}
/** Engine `ms_of`: the millisecond key wins, else the seconds key; numbers only. */
function mockDubMs(seg: Record<string, unknown>, msKey: string, sKey: string): number | null {
  if (typeof seg[msKey] === 'number' && Number.isFinite(seg[msKey] as number)) return Math.round(seg[msKey] as number);
  if (typeof seg[sKey] === 'number' && Number.isFinite(seg[sKey] as number)) return Math.round((seg[sKey] as number) * 1000);
  return null;
}
function mockDubPlace(segs: MockDubSeg[], maxTempo: number, allowShift: boolean, gap: number): MockDubPlaced[] {
  const out: MockDubPlaced[] = [];
  let prevEnd: number | null = null;
  segs.forEach((g, i) => {
    const next = segs.slice(i + 1).find((n) => n.synth > 0);
    const nextStart = next ? next.start : null;
    const bound = nextStart === null ? g.end : Math.min(nextStart, g.end);
    const slot = bound - g.start;
    if (g.synth === 0) {
      out.push({ action: 'empty', tempo: null, start: g.start, ms: 0, slot, shift: 0, excess: 0, overlap: 0 });
      return;
    }
    const start = allowShift && prevEnd !== null ? Math.max(g.start, prevEnd + gap) : g.start;
    const avail = bound - start;
    const synth = g.synth;
    let action: string;
    let tempo: number | null;
    let ms: number;
    if (avail > 0 && synth <= avail) {
      action = avail - synth <= MOCK_DUB_FIT_TOLERANCE_MS ? 'fits' : 'short';
      tempo = null;
      ms = synth;
    } else if (avail > 0 && synth / avail <= maxTempo + 1e-9) {
      action = 'tempo';
      tempo = mockDubRound4(synth / avail);
      ms = avail;
    } else if (allowShift) {
      action = 'extend';
      if (maxTempo > 1) {
        tempo = mockDubRound4(maxTempo);
        ms = Math.ceil(synth / maxTempo);
      } else {
        tempo = null;
        ms = synth;
      }
    } else {
      const overlap = nextStart === null ? 0 : Math.max(0, start + synth - nextStart);
      out.push({ action: 'overflow', tempo: null, start, ms: synth, slot, shift: 0, excess: synth - slot, overlap });
      prevEnd = start + synth;
      return;
    }
    out.push({ action, tempo, start, ms, slot, shift: start - g.start, excess: 0, overlap: 0 });
    prevEnd = start + ms;
  });
  return out;
}
function mockDubSegments(input: Record<string, unknown>): MockDubSeg[] {
  const arr = Array.isArray(input.segments) ? (input.segments as Array<Record<string, unknown>>) : [];
  if (arr.length === 0) throw new MockEngineError('invalid_input', 'a dubbing plan needs a non-empty segments array');
  const seen = new Set<string>();
  const out: MockDubSeg[] = [];
  arr.forEach((v, pos) => {
    const idRaw = v.id ?? v.index;
    const id = typeof idRaw === 'string' && idRaw.trim() ? idRaw.trim() : typeof idRaw === 'number' ? String(idRaw) : String(pos + 1);
    if (seen.has(id)) throw new MockEngineError('invalid_input', `duplicate segment id ${id}`);
    seen.add(id);
    const start = mockDubMs(v, 'start_ms', 'start_s');
    if (start === null) throw new MockEngineError('invalid_input', `segment ${id}: missing start_ms/start_s`);
    const end = mockDubMs(v, 'end_ms', 'end_s');
    if (end === null) throw new MockEngineError('invalid_input', `segment ${id}: missing end_ms/end_s`);
    if (start < 0 || end <= start) throw new MockEngineError('invalid_input', `segment ${id}: needs 0 <= start < end (got ${start}..${end} ms)`);
    const prev = out[out.length - 1];
    if (prev && start <= prev.start) throw new MockEngineError('invalid_input', `segment ${id}: segments must be in order with increasing starts`);
    if (mockDubStr(v.audio_path) !== '') throw new MockEngineError('tool_missing', `segment ${id}: audio_path needs ffprobe, which the browser mirror does not have; give synth_ms`);
    const text = mockDubStr(v.text);
    const src = mockDubStr(v.source_text) || mockDubStr(v.text_source);
    const given = mockDubMs(v, 'synth_ms', 'synth_s');
    let synth: number;
    if (text === '') {
      if ((given ?? 0) !== 0) throw new MockEngineError('invalid_input', `segment ${id}: has audio but no text`);
      synth = 0;
    } else {
      if (given === null || given <= 0) throw new MockEngineError('invalid_input', `segment ${id}: has text but no synthesised duration (synth_ms/synth_s or audio_path)`);
      synth = given;
    }
    out.push({ id, start, end, src, text, synth });
  });
  if (out.every((g) => g.synth === 0)) throw new MockEngineError('invalid_input', 'every segment is blank: nothing to dub');
  return out;
}
function mockDubSrt(cues: Array<[number, number, string]>): string {
  const stamp = (sec: number) => {
    const ms = Math.round(sec * 1000);
    const p = (n: number, l = 2) => String(n).padStart(l, '0');
    return `${p(Math.floor(ms / 3600000))}:${p(Math.floor(ms / 60000) % 60)}:${p(Math.floor(ms / 1000) % 60)},${p(ms % 1000, 3)}`;
  };
  return cues.map(([s, e, t], i) => `${i + 1}\n${stamp(s)} --> ${stamp(e)}\n${t}\n`).join('\n');
}
function mockDubbingPlan(input: Record<string, unknown>): Record<string, unknown> {
  if (mockDubStr(input.out_dir) !== '') throw new MockEngineError('tool_missing', 'out_dir writes dub_plan.json and dub_track.wav; the browser mirror writes no files');
  const sourceLang = mockDubLang(input.source_lang);
  const targetLang = mockDubLang(input.target_lang);
  if (!sourceLang || !targetLang) throw new MockEngineError('invalid_input', 'dubbing needs explicit source_lang and target_lang (no language is assumed)');
  if (sourceLang === targetLang && !mockDubTruthy(input.passthrough)) {
    throw new MockEngineError('dubbing_same_language', `source_lang and target_lang are both '${sourceLang}': nothing to dub (set passthrough: true only for a media-path test)`);
  }
  const maxTempo = mockDubNumber(input.max_tempo) ?? 1.15;
  if (!(maxTempo >= 1 && maxTempo <= 2)) throw new MockEngineError('invalid_input', `max_tempo must be between 1.0 and 2 (got ${maxTempo})`);
  const allowShift = mockDubTruthy(input.allow_shift);
  const gap = Math.round(mockDubNumber(input.min_gap_ms) ?? 0);
  if (gap < 0) throw new MockEngineError('invalid_input', 'min_gap_ms must be >= 0');
  const mediaRaw = mockDubNumber(input.media_duration_ms);
  const mediaMs = mediaRaw !== null && Math.round(mediaRaw) > 0 ? Math.round(mediaRaw) : null;
  const segs = mockDubSegments(input);
  const placed = mockDubPlace(segs, maxTempo, allowShift, gap);

  const report: Array<Record<string, unknown>> = [];
  const cues: Array<[number, number, string]> = [];
  const overflowIds: string[] = [];
  const reasons: string[] = [];
  const counts: Record<string, number> = {};
  let maxShift = 0;
  let lastEnd = 0;
  let totalExcess = 0;
  let maxRatio = 0;
  let finalShift = 0;
  segs.forEach((g, i) => {
    const p = placed[i];
    counts[p.action] = (counts[p.action] ?? 0) + 1;
    if (g.synth === 0) {
      report.push({ id: g.id, source_text: g.src, target_text: g.text, source_start_ms: g.start, source_end_ms: g.end, slot_ms: p.slot, synth_ms: 0, action: 'empty', tempo: null, shift_ms: 0, placed_start_ms: null, placed_end_ms: null, placed_ms: 0, excess_ms: 0, overlaps_next_ms: 0 });
      return;
    }
    const end = p.start + p.ms;
    report.push({ id: g.id, source_text: g.src, target_text: g.text, source_start_ms: g.start, source_end_ms: g.end, slot_ms: p.slot, synth_ms: g.synth, action: p.action, tempo: p.tempo, shift_ms: p.shift, placed_start_ms: p.start, placed_end_ms: end, placed_ms: p.ms, excess_ms: p.excess, overlaps_next_ms: p.overlap });
    if (p.action === 'overflow') {
      overflowIds.push(g.id);
      totalExcess += p.excess;
      const ratio = g.synth / Math.max(p.slot, 1);
      maxRatio = Math.max(maxRatio, ratio);
      reasons.push(`segment ${g.id}: synthesised ${g.synth} ms is ${p.excess} ms over its ${p.slot} ms slot and needs x${ratio.toFixed(3)} (max_tempo ${maxTempo.toFixed(2)})`);
    }
    maxShift = Math.max(maxShift, p.shift);
    finalShift = p.shift;
    lastEnd = Math.max(lastEnd, end);
    cues.push([p.start / 1000, end / 1000, g.text]);
  });
  if (mediaMs !== null && lastEnd > mediaMs) {
    reasons.push(`the dub track ends at ${lastEnd} ms, ${lastEnd - mediaMs} ms past the media (${mediaMs} ms); composing would cut it`);
  }

  const status = reasons.length === 0 ? 'ok' : 'needs_review';
  const options: string[] = [];
  if (status === 'needs_review') {
    if (overflowIds.length > 0) {
      options.push(`shorten the translation of segment(s) ${overflowIds.join(', ')} and re-run the dub`);
      if (maxRatio <= 2) {
        options.push(`allow a larger stretch: max_tempo of at least ${(Math.ceil(maxRatio * 100 - 1e-9) / 100).toFixed(2)} (speech that much faster than natural)`);
      } else {
        options.push(`a larger stretch is not enough: x${maxRatio.toFixed(2)} exceeds the 2 single-stage atempo ceiling`);
      }
      const preview = mockDubPlace(segs, maxTempo, true, gap);
      const drift = Math.max(0, ...preview.map((p) => p.shift));
      options.push(`allow_shift: true moves later segments after longer ones (preview: up to ${drift} ms later)`);
    }
    if (mediaMs !== null && lastEnd > mediaMs) options.push('lengthen the video or shorten the translation so the dub ends inside the media');
  }

  return {
    status,
    alignment: 'segment',
    word_alignment: 'unavailable: needs a RightKit alignment capability (rightkit-align is not vendored); cue times are segment-level',
    source_lang: sourceLang,
    target_lang: targetLang,
    passthrough: mockDubTruthy(input.passthrough),
    policy: {
      max_tempo: maxTempo, allow_shift: allowShift, min_gap_ms: gap, fit_tolerance_ms: MOCK_DUB_FIT_TOLERANCE_MS,
      tempo_filter: 'atempo (pitch preserving, never slows speech)',
      slot: "start to min(own end, next audible segment's source start); with allow_shift the start is max(source start, previous placed end + min_gap_ms)"
    },
    segments: report,
    summary: {
      counts, overflow_count: overflowIds.length, overflow_ids: overflowIds, total_excess_ms: totalExcess,
      max_shift_ms: maxShift, final_shift_ms: finalShift, track_end_ms: lastEnd, media_duration_ms: mediaMs
    },
    reasons,
    options,
    // Formatted here, not by rightkit_transcript: the text is close to the app's SRT but not byte-identical.
    srt: status === 'ok' ? mockDubSrt(cues) : null,
    browser_mirror: 'Placement is the engine arithmetic. The SRT is formatted by the mirror, and no dub track is rendered.'
  };
}
function mockDubbingCommand(name: string, input: Record<string, unknown>): { value: unknown } | null {
  if (name !== 'studio.pipelines.dubbing_plan') return null;
  return { value: mockDubbingPlan(input) };
}
// DUBBING (end)

// DEVICE ALLOWANCE (begin): `spend.allowance` and `spend.set_allowance`, mirroring crates/genright-gen
// ledger.rs (allowance_json, allowance_status, set_allowance) and api.rs. The allowance counts only mock jobs
// created after it was last set, and it is this mirror's own state: no other device exists here.
type MockAllowanceAudit = { id: number; at: string; who: string; device_id: string; old_usd: number | null; new_usd: number | null; note: string | null };
const mockAllowance = {
  deviceId: '',
  amount: null as number | null,
  setAt: null as string | null,
  setIndex: 0,
  audit: [] as MockAllowanceAudit[],
  seq: 0
};
function mockDeviceId(): string {
  if (!mockAllowance.deviceId) {
    mockAllowance.deviceId = typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
      ? crypto.randomUUID()
      : `mock-${Math.random().toString(36).slice(2, 14)}`;
  }
  return mockAllowance.deviceId;
}
/** Reservations made since the allowance was set: open ones and completed ones; failed and cancelled release theirs. */
function mockAllowanceCounted(): number {
  return mockState.jobs
    .slice(mockAllowance.setIndex)
    .filter((j) => ['complete', 'submitted', 'running', 'outcome_unknown'].includes(mockPhase(j).status))
    .reduce((n, j) => n + j.est_usd, 0);
}
function mockAllowanceView() {
  const amount = mockAllowance.amount;
  const counted = mockAllowanceCounted();
  return {
    device_id: mockDeviceId(),
    device_allowance_usd: amount,
    counted_usd: amount === null ? null : counted,
    remaining_usd: amount === null ? null : Math.max(0, amount - counted),
    set_at: amount === null ? null : mockAllowance.setAt,
    shared_allowance: { available: false, reason: 'no sync service: each device enforces its own offline allowance' },
    audit: mockAllowance.audit.slice(-20).reverse()
  };
}
function mockSetAllowance(input: Record<string, unknown>) {
  if (!('device_allowance_usd' in input)) throw new MockEngineError('invalid_input', 'missing field: device_allowance_usd (a number, or null to clear)');
  const raw = input.device_allowance_usd;
  let next: number | null = null;
  if (raw !== null) {
    if (typeof raw !== 'number' || !Number.isFinite(raw) || raw < 0) {
      throw new MockEngineError('invalid_input', 'device_allowance_usd must be a non-negative number or null');
    }
    next = raw;
  }
  if (input.note !== undefined && input.note !== null && typeof input.note !== 'string') throw new MockEngineError('invalid_input', 'note must be a string');
  const note = typeof input.note === 'string' ? input.note.trim() : '';
  if (Array.from(note).length > 500) throw new MockEngineError('invalid_input', 'note must be at most 500 characters');
  const at = new Date().toISOString();
  mockAllowance.seq += 1;
  mockAllowance.audit.push({ id: mockAllowance.seq, at, who: 'local user', device_id: mockDeviceId(), old_usd: mockAllowance.amount, new_usd: next, note: note || null });
  mockAllowance.amount = next;
  mockAllowance.setAt = next === null ? null : at;
  mockAllowance.setIndex = mockState.jobs.length;
  return mockAllowanceView();
}
function mockDeviceAllowanceCommand(name: string, input: Record<string, unknown>): { value: unknown } | null {
  if (name === 'spend.allowance') return { value: mockAllowanceView() };
  if (name === 'spend.set_allowance') return { value: mockSetAllowance(input) };
  return null;
}
// DEVICE ALLOWANCE (end)

// ===========================================================================
// RECIPE CATALOGUE (begin): browser preview of studio.recipes.catalogue and
// studio.recipes.catalogue_validate. The graphs are the seeded JSON under
// crates/genright-studio/data/recipes/catalogue/, the same files the engine embeds. Served status
// reads the preview route table (rtServed, above) plus the Vox fixture below; the engine's provider
// registry decides in the app. Validation mirrors a subset of the engine's checks (references,
// dependencies, kinds, stored status, cycles) and does not check param types. Read-only: no graph
// runs and nothing is spent.
// ===========================================================================

import catalogueMotionTransfer from '../studio-data/recipes/catalogue/motion_transfer.json' with { type: 'json' };
import catalogueObjectReplacement from '../studio-data/recipes/catalogue/object_replacement.json' with { type: 'json' };
import catalogueVoxDirector from '../studio-data/recipes/catalogue/vox_director.json' with { type: 'json' };
import catalogueIndex from '../studio-data/recipes/catalogue/_index.json' with { type: 'json' };

interface CatIssue {
  code: string;
  node: string | null;
  message: string;
}

const CAT_ENGINE_KINDS = ['image', 'video', 'voice', 'avatar', 'music', 'sfx', 'upscale', 'lipsync', 'text', 'extend', 'motion_control', 'replace_object', 'restyle'];
const CAT_INPUT_TYPES = ['image', 'video', 'audio', 'text', 'number', 'boolean'];
const CAT_OUTPUT_TYPES = ['image', 'video', 'audio', 'text', 'data'];

/** Preview fixture: the Vox routes are not in the template preview table, so the preview marks them served. */
const CAT_PREVIEW_EXTRA_SERVED = [
  { provider: 'fal', model: 'fal-ai/flux-2/klein/9b', kind: 'image' },
  { provider: 'local-qts', model: 'qwen3-tts-0.6b-q8_0', kind: 'voice' },
  { provider: 'fal', model: 'fal-ai/veo3.1/fast/image-to-video', kind: 'video' }
];

const CAT_SEEDED: Array<{ id: string; graph: RtRecord }> = [
  { id: 'motion_transfer', graph: catalogueMotionTransfer as unknown as RtRecord },
  { id: 'object_replacement', graph: catalogueObjectReplacement as unknown as RtRecord },
  { id: 'vox_director', graph: catalogueVoxDirector as unknown as RtRecord }
];

function catOutputOf(kind: string): string | null {
  switch (kind) {
    case 'image':
    case 'upscale':
      return 'image';
    case 'video':
    case 'avatar':
    case 'extend':
    case 'lipsync':
    case 'motion_control':
    case 'replace_object':
    case 'restyle':
      return 'video';
    case 'voice':
    case 'music':
    case 'sfx':
      return 'audio';
    case 'text':
      return 'text';
    default:
      return null;
  }
}

function catServed(provider: string, model: string, kind: string): boolean {
  if (rtServed(provider, model, kind) !== undefined) return true;
  return CAT_PREVIEW_EXTRA_SERVED.some((r) => r.provider === provider && r.model === model && r.kind === kind);
}

/** One node's served status, shaped as the engine's node_status (params_checked is false: no declared entries here). */
function catNodeStatus(n: RtRecord): RtRecord {
  const id = rtText(n.id);
  const kind = rtText(n.kind);
  if (kind === 'native') {
    return { id, kind, output: rtText(n.output) || null, status: 'native', route: null, missing_routes: [] };
  }
  const output = catOutputOf(kind);
  if (n.route === 'ask') return { id, kind, output, status: 'ask', route: 'ask', missing_routes: [] };
  const route = n.route as RtRecord | undefined;
  const provider = rtText(route?.provider);
  const model = rtText(route?.model);
  if (!provider || !model) return { id, kind, output, status: 'invalid_route', route: null, missing_routes: [] };
  const primary = { provider, model };
  if (catServed(provider, model, kind)) {
    return { id, kind, output, status: 'served', route: primary, params_checked: false, missing_routes: [] };
  }
  const alternatives = ((n.route_alternatives as RtRecord[] | undefined) ?? []).map((a) => ({ provider: rtText(a.provider), model: rtText(a.model) }));
  return { id, kind, output, status: 'unserved', route: primary, missing_routes: [primary, ...alternatives] };
}

function catReport(id: string, errors: CatIssue[], nodes: RtRecord[]): RtRecord {
  return { ok: errors.length === 0, id, errors, nodes };
}

/** Browser mirror of the engine's validator (subset). Every error names the node it concerns, or null. */
function catValidate(graph: unknown): RtRecord {
  if (typeof graph !== 'object' || graph === null || Array.isArray(graph)) {
    return catReport('', [{ code: 'invalid_graph', node: null, message: 'a graph is a JSON object' }], []);
  }
  const g = graph as RtRecord;
  const id = rtText(g.id);
  const errors: CatIssue[] = [];

  const inputTypes = new Map<string, string>();
  for (const inp of (g.inputs as RtRecord[] | undefined) ?? []) {
    const name = rtText(inp.name);
    const ty = rtText(inp.type);
    if (!name || !CAT_INPUT_TYPES.includes(ty)) {
      errors.push({ code: 'unknown_input_type', node: null, message: `input "${name}" has type "${ty}"; allowed: ${CAT_INPUT_TYPES.join(', ')}` });
    }
    inputTypes.set(name, ty);
  }

  const byId = new Map<string, RtRecord>();
  const order: string[] = [];
  const outputOf = new Map<string, string | null>();
  for (const n of (g.nodes as RtRecord[] | undefined) ?? []) {
    const nid = rtText(n.id);
    if (!nid) {
      errors.push({ code: 'invalid_graph', node: null, message: 'a node has no id' });
      continue;
    }
    if (byId.has(nid)) {
      errors.push({ code: 'duplicate_node', node: nid, message: 'node id is declared twice' });
      continue;
    }
    byId.set(nid, n);
    order.push(nid);
    for (const key of ['status', 'served']) {
      if (n[key] !== undefined && n[key] !== null) errors.push({ code: 'stored_status', node: nid, message: `"${key}" is computed on load and must not be stored` });
    }
    const kind = rtText(n.kind);
    let out: string | null = null;
    if (kind === 'native') {
      const o = rtText(n.output);
      if (CAT_OUTPUT_TYPES.includes(o)) out = o;
      else errors.push({ code: 'unknown_output_type', node: nid, message: `native output "${o}" is not one of ${CAT_OUTPUT_TYPES.join(', ')}` });
    } else if (CAT_ENGINE_KINDS.includes(kind)) {
      out = catOutputOf(kind);
    } else {
      errors.push({ code: 'unknown_kind', node: nid, message: `kind "${kind}" is neither an engine kind nor native` });
    }
    outputOf.set(nid, out);
  }

  const edges = new Map<string, string[]>();
  for (const nid of order) {
    const n = byId.get(nid) as RtRecord;
    const declared = ((n.depends_on as unknown[] | undefined) ?? []).map((d) => rtText(d));
    const deps: string[] = [];
    for (const d of declared) {
      if (outputOf.has(d)) deps.push(d);
      else errors.push({ code: 'dangling_reference', node: nid, message: `depends_on names "${d}", which is not a node` });
    }
    const params = (n.params as RtRecord | undefined) ?? {};
    for (const [key, val] of Object.entries(params)) {
      if (!val || typeof val !== 'object') continue; // a literal
      const ref = val as RtRecord;
      if (typeof ref.node === 'string') {
        if (!outputOf.has(ref.node)) errors.push({ code: 'dangling_reference', node: nid, message: `param "${key}" reads node "${ref.node}", which does not exist` });
        else if (!declared.includes(ref.node)) errors.push({ code: 'undeclared_dependency', node: nid, message: `param "${key}" reads node "${ref.node}" but depends_on does not list it` });
      } else if (typeof ref.input === 'string') {
        if (!inputTypes.has(ref.input)) errors.push({ code: 'dangling_reference', node: nid, message: `param "${key}" reads input "${ref.input}", which is not declared` });
      }
    }
    edges.set(nid, deps);
  }

  for (const o of (g.outputs as RtRecord[] | undefined) ?? []) {
    const from = rtText(o.from);
    if (!outputOf.has(from)) errors.push({ code: 'dangling_reference', node: null, message: `output "${rtText(o.name)}" reads node "${from}", which does not exist` });
  }

  const settled = new Set<string>();
  let grew = true;
  while (grew) {
    grew = false;
    for (const nid of order) {
      if (!settled.has(nid) && (edges.get(nid) ?? []).every((d) => settled.has(d))) {
        settled.add(nid);
        grew = true;
      }
    }
  }
  for (const nid of order) {
    if (!settled.has(nid)) errors.push({ code: 'cycle', node: nid, message: `node "${nid}" cannot be ordered: it sits on a dependency cycle or downstream of one` });
  }

  return catReport(id, errors, order.map((nid) => catNodeStatus(byId.get(nid) as RtRecord)));
}

function catListing(): RtRecord {
  const graphs = CAT_SEEDED.map(({ id, graph }) => {
    const v = catValidate(graph);
    const nodes = v.nodes as RtRecord[];
    const missing = nodes.flatMap((n) => ((n.missing_routes as RtRecord[] | undefined) ?? []).map((m) => ({ ...m, node: n.id })));
    return {
      id,
      title: graph.title,
      source: graph.source,
      inputs: graph.inputs,
      outputs: graph.outputs,
      review: graph.review,
      valid: v.ok,
      errors: v.errors,
      nodes,
      missing_routes: missing
    };
  });
  const idx = catalogueIndex as unknown as RtRecord;
  return {
    ok: true,
    graphs,
    plan: idx.plan,
    plan_named: idx.plan_named,
    pipeline_templates_not_named_by_plan: idx.pipeline_templates_not_named_by_plan
  };
}

function catValidateCommand(input: RtRecord): RtRecord {
  const id = rtText(input.id);
  if (id) {
    const seeded = CAT_SEEDED.find((s) => s.id === id);
    if (!seeded) throw new MockEngineError('not_found', `no seeded recipe graph "${id}"`);
    return catValidate(seeded.graph);
  }
  if (input.graph && typeof input.graph === 'object') return catValidate(input.graph);
  throw new MockEngineError('invalid_input', 'pass {id} for a seeded graph or {graph} for an inline graph');
}

/** Browser-preview dispatch for the recipe catalogue commands. Called by lib/api.ts only outside Tauri. */
export async function mockRecipeCatalogue<T>(name: string, input: Record<string, unknown>): Promise<T> {
  switch (name) {
    case 'studio.recipes.catalogue':
      return catListing() as T;
    case 'studio.recipes.catalogue_validate':
      return catValidateCommand(input as RtRecord) as T;
    default:
      return unsupportedMock(name);
  }
}
// RECIPE CATALOGUE (end)

// RENTALS (begin): browser preview of studio.models.rentals, studio.models.rentals_enforce and the rental actions of
// studio.models.worker.action (teardown, resolve_rental). Mirrors crates/genright-studio model_management/rental.rs
// (list, limits, settle, enforce_rentals) and mod.rs (worker_action). Mock state only: no provider is contacted.
// The preview has no VAST_API_KEY, so a destroy fails as the engine does without one (no_credentials, the instance
// recorded as possibly billing) and a due rental is reported as blocked. resolve_rental finds no instance (none
// exists in the preview), so confirm_none releases and nothing else is adopted.
const MOCK_RENTAL_BILLING =
  'the Vast instance may still be billing: destroy was not confirmed by the provider; the spend reservation stays open';
const MOCK_RENTAL_DELIVERED =
  'the Vast create may have been delivered: an instance labelled for this worker may exist and bill; the spend reservation stays open until resolve_rental';
const MOCK_RENTAL_NO_KEY = 'VAST_API_KEY is not set in this process, so the instance cannot be destroyed from here';

type MockRentalWorker = {
  worker_id: string;
  label: string;
  state: string;
  budget_usd: number;
  accrued_usd: number;
  spec: Record<string, unknown>;
  outputs: unknown[];
  events: unknown[];
  created_at: string;
  updated_at: string;
  idle_limit_min?: number;
  idle_since_epoch?: number;
  rental: Record<string, unknown>;
};

let mockRentalState: MockRentalWorker[] | null = null;

function mockRentalNum(o: Record<string, unknown>, k: string): number | null {
  const v = o[k];
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

function mockRentalIso(epoch: number): string {
  return new Date(epoch * 1000).toISOString();
}

/** One rental per notable status, timestamps relative to the moment the preview first reads them. */
function mockRentalSeed(nowSec: number): MockRentalWorker[] {
  const make = (id: string, label: string, state: string, rental: Record<string, unknown>, extra: Partial<MockRentalWorker> = {}): MockRentalWorker => {
    const instance = rental.instance_id;
    const reserved = typeof rental.reserved_usd === 'number' ? rental.reserved_usd : 0;
    return {
      worker_id: id,
      label,
      state,
      budget_usd: reserved,
      accrued_usd: 0,
      spec: { driver: 'vast', gpu: 'RTX 4090', vram_gb: 24, vast: instance != null ? { instance_id: instance, ssh_host: '203.0.113.10', ssh_port: 22 } : {} },
      outputs: [],
      events: [],
      created_at: mockRentalIso(nowSec - 7200),
      updated_at: mockRentalIso(nowSec),
      rental: { ledger_id: `rental:${id}`, ...rental },
      ...extra
    };
  };
  return [
    make('rw-rental-creating', 'batch render', 'configured', {
      attempt: 1, status: 'renting', reserved_usd: 3, max_duration_min: 60, offer_id: 1002,
      reserved_at_epoch: nowSec - 90, reserved_at: mockRentalIso(nowSec - 90)
    }),
    make('rw-rental-running', 'upscale pass', 'running', {
      attempt: 2, status: 'active', reserved_usd: 2, max_duration_min: 60, offer_id: 1001, dph_total: 0.34, instance_id: 900101,
      reserved_at_epoch: nowSec - 2402, rented_at_epoch: nowSec - 2400, rented_at: mockRentalIso(nowSec - 2400)
    }),
    make('rw-rental-idle', 'voice set', 'idle', {
      attempt: 1, status: 'active', reserved_usd: 5, max_duration_min: 240, offer_id: 1002, dph_total: 0.62, instance_id: 900102,
      reserved_at_epoch: nowSec - 5402, rented_at_epoch: nowSec - 5400, rented_at: mockRentalIso(nowSec - 5400)
    }, { idle_limit_min: 10, idle_since_epoch: nowSec - 720 }),
    make('rw-rental-unknown', 'avatar test', 'configured', {
      attempt: 1, status: 'outcome_unknown', reserved_usd: 3, max_duration_min: 60, offer_id: 1003,
      reserved_at_epoch: nowSec - 900, held_at: mockRentalIso(nowSec - 900),
      held_because: { code: 'vast', message: 'create HTTP 503: service unavailable' },
      billing_warning: MOCK_RENTAL_DELIVERED
    }),
    make('rw-rental-destroying', 'frame export', 'idle', {
      attempt: 1, status: 'destroying', reserved_usd: 2, max_duration_min: 60, offer_id: 1001, dph_total: 0.34, instance_id: 900104,
      reserved_at_epoch: nowSec - 3902, rented_at_epoch: nowSec - 3900, destroying_at_epoch: nowSec - 30,
      destroying_reason: 'duration', destroy_claim: 'mock-claim', billing_warning: MOCK_RENTAL_BILLING
    }),
    make('rw-rental-unconfirmed', 'color pass', 'error', {
      attempt: 1, status: 'destroy_unconfirmed', reserved_usd: 4, max_duration_min: 240, offer_id: 1002, dph_total: 0.62, instance_id: 900105,
      reserved_at_epoch: nowSec - 1802, rented_at_epoch: nowSec - 1800, billing_warning: MOCK_RENTAL_BILLING,
      destroy_error: { code: 'vast', message: 'destroy HTTP 500', at: mockRentalIso(nowSec - 400), at_epoch: nowSec - 400 }
    }),
    make('rw-rental-unsettled', 'mix down', 'terminated', {
      attempt: 1, status: 'destroyed_unsettled', reserved_usd: 1, max_duration_min: 180, offer_id: 1001, dph_total: 0.5, instance_id: 900106,
      reserved_at_epoch: nowSec - 7202, rented_at_epoch: nowSec - 7200, destroyed_at_epoch: nowSec - 120, destroyed_at: mockRentalIso(nowSec - 120)
    }),
    make('rw-rental-settled', 'batch render B', 'terminated', {
      attempt: 1, status: 'settled', reserved_usd: 1, max_duration_min: 240, offer_id: 1001, dph_total: 0.5, instance_id: 900107,
      reserved_at_epoch: nowSec - 9146, rented_at_epoch: nowSec - 9144, destroyed_at_epoch: nowSec - 1800,
      elapsed_sec: 7344, rate_usd_hr: 0.5, actual_usd: 1.02, overrun_usd: 0.02, settled_on_declared_budget: false,
      auto_destroyed: {
        reason: 'budget',
        detail: 'accrued $1.00 (elapsed 7200.00 s x $0.50/hr) >= budget $1.00',
        at: mockRentalIso(nowSec - 1800), at_epoch: nowSec - 1800
      }
    }, { accrued_usd: 1.02 }),
    make('rw-rental-released', 'old test', 'configured', {
      attempt: 1, status: 'released', reserved_usd: 2, max_duration_min: 60, offer_id: 1003,
      reserved_at_epoch: nowSec - 86400, released_at: mockRentalIso(nowSec - 86000),
      released_because: 'no instance carries the label; confirmed by the user'
    })
  ];
}

function mockRentals(): MockRentalWorker[] {
  if (!mockRentalState) mockRentalState = mockRentalSeed(Date.now() / 1000);
  return mockRentalState;
}

function mockRentalWorkers(): unknown[] {
  return mockRentals();
}

/** The live figures `studio.models.rentals` reports (rental.rs `list` and `limits`), at `nowSec`. */
function mockRentalEntry(w: MockRentalWorker, nowSec: number) {
  const r = w.rental;
  const status = String(r.status ?? '');
  const starts = [mockRentalNum(r, 'provider_start_epoch'), mockRentalNum(r, 'rented_at_epoch'), mockRentalNum(r, 'reserved_at_epoch')]
    .filter((t): t is number => t != null && t > 0 && t <= nowSec);
  const elapsed = starts.length ? Math.max(0, nowSec - Math.min(...starts)) : null;
  const rates = [mockRentalNum(r, 'dph_total'), mockRentalNum(r, 'provider_dph_total')].filter((x): x is number => x != null);
  const rate = Math.max(0, ...rates);
  const rateUsd = rate > 0 ? rate : null;
  const reserved = mockRentalNum(r, 'reserved_usd');
  const budget = reserved != null && reserved > 0 ? reserved : null;
  const accrued = elapsed != null && rateUsd != null ? Math.round((elapsed / 3600) * rateUsd * 1e6) / 1e6 : null;
  const maxMin = (mockRentalNum(r, 'max_duration_min') ?? 0) > 0 ? mockRentalNum(r, 'max_duration_min') : null;
  const idleLimit = w.state === 'idle' && (w.idle_limit_min ?? 0) > 0 ? (w.idle_limit_min as number) : null;
  const idleFor = w.state === 'idle' && w.idle_since_epoch != null && w.idle_since_epoch <= nowSec ? nowSec - w.idle_since_epoch : null;
  const dueDuration = maxMin != null && elapsed != null ? maxMin * 60 - elapsed : null;
  const dueBudget = budget != null && rateUsd != null && accrued != null ? ((budget - accrued) / rateUsd) * 3600 : null;
  const dueIdle = idleLimit != null && idleFor != null ? idleLimit * 60 - idleFor : null;
  const due = status === 'active'
    ? (dueDuration != null && dueDuration <= 0 ? 'duration' : dueBudget != null && dueBudget <= 0 ? 'budget' : dueIdle != null && dueIdle <= 0 ? 'idle' : null)
    : null;
  const warnings: string[] = [];
  if (elapsed == null) warnings.push('no start time is recorded: the duration and budget limits cannot be enforced');
  if (maxMin == null) warnings.push('no max_duration_min is recorded: the duration limit cannot be enforced');
  if (rateUsd == null) warnings.push('no hourly rate is known: the budget limit cannot be enforced');
  if (status === 'destroying' || status === 'destroy_unconfirmed') warnings.push(`rental status is ${status}`);
  if (typeof r.billing_warning === 'string') warnings.push(r.billing_warning);
  return {
    worker_id: w.worker_id,
    label: w.label,
    worker_state: w.state,
    rental_status: status,
    instance_id: r.instance_id ?? null,
    elapsed_sec: elapsed,
    rate_usd_hr: rateUsd,
    accrued_usd: accrued,
    budget_usd: budget,
    max_duration_min: maxMin,
    idle_limit_min: idleLimit,
    idle_for_sec: idleFor,
    due_in_sec: { duration: dueDuration, budget: dueBudget, idle: dueIdle },
    due,
    auto_destroyed: r.auto_destroyed ?? null,
    warnings
  };
}

/** rental.rs `settle` for a destroyed rental: elapsed x the higher known rate, overrun above the reservation. */
function mockRentalSettle(w: MockRentalWorker): Record<string, unknown> {
  const r = w.rental;
  const end = mockRentalNum(r, 'destroyed_at_epoch') ?? Date.now() / 1000;
  const starts = [mockRentalNum(r, 'provider_start_epoch'), mockRentalNum(r, 'rented_at_epoch')]
    .filter((t): t is number => t != null && t > 0 && t <= end);
  const start = starts.length ? Math.min(...starts) : Number.POSITIVE_INFINITY;
  const elapsed = Number.isFinite(start) ? Math.max(0, end - start) : 0;
  const rate = Math.max(0, mockRentalNum(r, 'dph_total') ?? 0, mockRentalNum(r, 'provider_dph_total') ?? 0);
  const unknownBasis = !Number.isFinite(start) || rate <= 0;
  const reserved = mockRentalNum(r, 'reserved_usd') ?? 0;
  const actual = unknownBasis ? reserved : Math.round((elapsed / 3600) * rate * 1e6) / 1e6;
  const overrun = Math.round(Math.max(actual - reserved, 0) * 1e6) / 1e6;
  Object.assign(r, {
    status: 'settled', settled_at: mockRentalIso(Date.now() / 1000), elapsed_sec: elapsed, rate_usd_hr: rate,
    actual_usd: actual, overrun_usd: overrun, settled_on_declared_budget: unknownBasis
  });
  delete r.billing_warning;
  delete r.destroy_error;
  w.state = 'terminated';
  return { instance_id: r.instance_id ?? null, elapsed_sec: elapsed, rate_usd_hr: rate, actual_usd: actual, reserved_usd: reserved, overrun_usd: overrun, settled_on_declared_budget: unknownBasis };
}

function mockRentalsCommand(name: string, _input: Record<string, unknown>): { value: unknown } | null {
  const nowSec = Date.now() / 1000;
  switch (name) {
    case 'studio.models.rentals':
      return {
        value: {
          now_epoch: nowSec,
          clock_overridden: false,
          rentals: mockRentals().map((w) => mockRentalEntry(w, nowSec)),
          enforcement: RENTAL_ENFORCEMENT_TEXT
        }
      };
    case 'studio.models.rentals_enforce': {
      const report = {
        at_epoch: nowSec,
        destroyed: [] as unknown[], failed: [] as unknown[], blocked: [] as unknown[], unresolved: [] as unknown[],
        in_progress: [] as unknown[], settled_only: [] as unknown[], persist_errors: [] as unknown[],
        busy: false, limits: RENTAL_ENFORCEMENT_TEXT
      };
      for (const w of mockRentals()) {
        const r = w.rental;
        const status = String(r.status ?? '');
        if (status === 'renting' || status === 'outcome_unknown') {
          report.unresolved.push({ worker_id: w.worker_id, status, billing_warning: r.billing_warning ?? null, action: 'run resolve_rental; enforcement never destroys a rental whose create outcome is unknown' });
        } else if (status === 'destroyed_unsettled') {
          report.settled_only.push({ worker_id: w.worker_id, settlement: mockRentalSettle(w) });
        } else if (status === 'destroying') {
          const claimed = mockRentalNum(r, 'destroying_at_epoch') ?? 0;
          if (nowSec - claimed < 180) {
            report.in_progress.push({ worker_id: w.worker_id });
          } else {
            report.blocked.push({ worker_id: w.worker_id, reason: 'retry', detail: 'a previous destroy claim never finished', error: MOCK_RENTAL_NO_KEY, billing_warning: 'the instance is past a limit and may still be billing' });
          }
        } else if (status === 'active') {
          const entry = mockRentalEntry(w, nowSec);
          if (entry.due) {
            report.blocked.push({ worker_id: w.worker_id, reason: entry.due, detail: `${entry.due} limit reached`, error: MOCK_RENTAL_NO_KEY, billing_warning: 'the instance is past a limit and may still be billing' });
          }
        } else if (status === 'destroy_unconfirmed') {
          const at = mockRentalNum(r.destroy_error as Record<string, unknown>, 'at_epoch') ?? 0;
          if (nowSec - at >= 300) {
            report.blocked.push({ worker_id: w.worker_id, reason: 'retry', detail: 'the previous destroy was not confirmed by the provider', error: MOCK_RENTAL_NO_KEY, billing_warning: 'the instance is past a limit and may still be billing' });
          }
        }
      }
      return { value: report };
    }
    default:
      return null;
  }
}

/** Rental records answer `status`, `teardown` and `resolve_rental`; any other action on one is refused. Null for a non-rental worker. */
function mockRentalWorkerAction(input: Record<string, unknown>): { value: unknown } | null {
  const id = String(input.worker_id ?? '');
  const w = mockRentals().find((x) => x.worker_id === id);
  if (!w) return null;
  const action = String(input.action ?? '');
  const r = w.rental;
  const status = String(r.status ?? '');
  const nowSec = Date.now() / 1000;
  const iso = mockRentalIso(nowSec);
  const log = (to: string, detail: unknown) => {
    w.events.push({ at: iso, action, from: w.state, to, detail });
    w.state = to;
    w.updated_at = iso;
  };
  if (action === 'status') return { value: w };
  if (action === 'resolve_rental') {
    if (status !== 'renting' && status !== 'outcome_unknown') throw new MockEngineError('bad_state', 'no held rental to resolve');
    if (input.confirm_none !== true) {
      throw new MockEngineError('rental_unresolved', "no instance carries this worker's label; pass confirm_none:true only when sure the create never reached Vast (the reservation stays open until then)");
    }
    r.status = 'released';
    r.released_at = iso;
    r.released_because = 'no instance carries the label; confirmed by the user';
    delete r.billing_warning;
    log('configured', { released: true });
    return { value: w };
  }
  if (action === 'teardown') {
    const force = input.force === true;
    const live = ['active', 'destroy_unconfirmed', 'destroyed_unsettled', 'destroying'].includes(status);
    const forced = w.state === 'running' && live && force;
    if (['running', 'staging', 'retrieving'].includes(w.state) && !forced) {
      throw new MockEngineError('bad_state', w.state === 'running' && live
        ? 'the rental is running work and is billing; teardown would interrupt it. Pass "force": true to destroy it anyway (the destroy is confirmed by the provider and the spend is settled as usual)'
        : `finish or cancel in-flight work before teardown (state ${w.state})`);
    }
    if (status === 'renting' || status === 'outcome_unknown') {
      throw new MockEngineError('rental_unresolved', 'a Vast create may have been delivered and may be billing; run resolve_rental first');
    }
    if (status === 'destroying' && nowSec - (mockRentalNum(r, 'destroying_at_epoch') ?? 0) < 180) {
      throw new MockEngineError('destroy_in_progress', 'an automatic destroy of this rental is in flight; read the worker again in a moment');
    }
    if (status === 'destroyed_unsettled') {
      mockRentalSettle(w);
      log('terminated', { released: true, driver: 'vast', settlement: 'settled without a second destroy' });
      return { value: w };
    }
    if (live) {
      // No VAST_API_KEY in the preview: the destroy fails before the DELETE, so the instance may still bill.
      r.status = 'destroy_unconfirmed';
      r.billing_warning = MOCK_RENTAL_BILLING;
      r.destroy_error = { code: 'no_credentials', message: 'VAST_API_KEY not set — cannot destroy instance', at: iso, at_epoch: nowSec };
      log('error', { code: 'no_credentials', message: 'VAST_API_KEY not set — cannot destroy instance' });
      throw new MockEngineError('no_credentials', 'VAST_API_KEY not set — cannot destroy instance');
    }
    throw new MockEngineError('no_credentials', 'VAST_API_KEY not set — cannot destroy instance');
  }
  throw new MockEngineError('bad_state', `cannot ${action} a rental record in the browser preview`);
}
// RENTALS (end)
