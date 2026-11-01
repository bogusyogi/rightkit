/**
 * Pure helpers that turn the engine's `models` catalog plus form values into
 * `generate` params. The engine's per-model facts (`needs_key`, `default`, `inputs`, `settings`, see
 * crates/genright-gen/src/capabilities.rs) decide; the rule each helper falls back to for a row that
 * lacks them mirrors the Rust adapters in crates/genright-gen/src/providers/*.rs.
 * No imports on purpose: node --test loads this file directly.
 */

/** An input the model reads: the param that carries it and how the adapter takes the value. */
export interface IoFact {
  param: string;
  /** `url`: any remote URL; `https_url`: https only; `path`: a local file path. */
  accepts?: 'url' | 'https_url' | 'path';
  required?: boolean;
  /** Voice ids only: a regular expression the id must match. */
  pattern?: string;
}

export interface LengthFact {
  param: string;
  unit: 's' | 'ms';
  min: number;
  max: number;
  integer: boolean;
}

/**
 * Facts the engine's `models` rows state per model (crates/genright-gen/src/capabilities.rs, capability
 * `models.facts`). Every field is optional: a row from an older engine lacks it and the helpers below fall
 * back to the rule the UI used before, field by field.
 */
export interface ModelFacts {
  needs_key?: boolean;
  /** The engine's own default row for the model's kind. */
  default?: boolean;
  inputs?: {
    mode?: string;
    start_image?: 'required' | 'optional' | 'none';
    start_image_param?: string;
    end_image?: 'required' | 'optional' | 'none';
    end_image_param?: string;
    image?: IoFact;
    video?: IoFact;
    audio?: IoFact;
    model?: IoFact;
    avatar_id?: IoFact;
    voice_id?: IoFact;
    /** Declared role inputs from the catalogue row (see mask-export.ts). */
    mask?: { param: string; required?: boolean } | null;
    start?: { param: string; required?: boolean } | null;
  };
  settings?: {
    aspect?: { param: string; values: string[]; open?: boolean };
    resolution?: { param: string; values: string[]; required: boolean; default: string | null };
    seed?: { param: string };
    duration?: { param: string };
    length?: LengthFact;
    loop?: { param: string };
    sync_mode?: { param: string; values: string[] };
  };
}

export interface ModelCaps extends ModelFacts {
  provider: string;
  model: string;
  kind: string;
  durations_s?: number[] | null;
  min_duration_s?: number | null;
  max_duration_s?: number | null;
  default_duration_s?: number | null;
}

export type DurationSpec =
  | { type: 'set'; values: number[]; default: number }
  | { type: 'range'; min: number; max: number; default: number };

/** Duration rule for a model, or null when the model takes no duration. */
export function durationSpec(m: ModelCaps): DurationSpec | null {
  if (Array.isArray(m.durations_s) && m.durations_s.length > 0) {
    const values = [...m.durations_s].sort((a, b) => a - b);
    const def = m.default_duration_s != null && values.includes(m.default_duration_s) ? m.default_duration_s : values[0];
    return { type: 'set', values, default: def };
  }
  if (m.min_duration_s != null && m.max_duration_s != null && m.max_duration_s >= m.min_duration_s) {
    const def = m.default_duration_s != null && m.default_duration_s >= m.min_duration_s && m.default_duration_s <= m.max_duration_s ? m.default_duration_s : m.min_duration_s;
    return { type: 'range', min: m.min_duration_s, max: m.max_duration_s, default: def };
  }
  return null;
}

/** Every value the control may offer. Ranges are whole seconds. */
export function durationChoices(spec: DurationSpec): number[] {
  if (spec.type === 'set') return spec.values;
  const out: number[] = [];
  for (let s = spec.min; s <= spec.max; s += 1) out.push(s);
  return out;
}

export function isValidDuration(spec: DurationSpec | null, value: number | null | undefined): boolean {
  if (!spec) return value == null;
  if (value == null || !Number.isInteger(value)) return false;
  return spec.type === 'set' ? spec.values.includes(value) : value >= spec.min && value <= spec.max;
}

/** Keep the current value when it is still valid for the model, else the model default. */
export function reconcileDuration(spec: DurationSpec | null, current: number | null): number | null {
  if (!spec) return null;
  return isValidDuration(spec, current) ? current : spec.default;
}

/** Param names the form values travel under. Older rows state none, and the names below are what every built-in adapter reads. */
export interface ParamNames {
  aspect: string;
  resolution: string;
  duration: string;
  seed: string;
  startImage: string;
  endImage: string;
}

const DEFAULT_PARAM_NAMES: ParamNames = { aspect: 'aspect', resolution: 'resolution', duration: 'duration', seed: 'seed', startImage: 'image_url', endImage: 'end_image_url' };

function paramNames(m: ModelCaps): ParamNames {
  return {
    aspect: m.settings?.aspect?.param ?? DEFAULT_PARAM_NAMES.aspect,
    resolution: m.settings?.resolution?.param ?? DEFAULT_PARAM_NAMES.resolution,
    duration: m.settings?.duration?.param ?? DEFAULT_PARAM_NAMES.duration,
    seed: m.settings?.seed?.param ?? DEFAULT_PARAM_NAMES.seed,
    startImage: m.inputs?.start_image_param ?? DEFAULT_PARAM_NAMES.startImage,
    endImage: m.inputs?.end_image_param ?? DEFAULT_PARAM_NAMES.endImage
  };
}

/** The resolution to preselect: the adapter's own default, else the first value when one is required, else none. */
function startingResolution(r: { values: string[]; required: boolean; default: string | null }): string | null {
  return r.default ?? (r.required ? r.values[0] ?? null : null);
}

export interface VideoCaps {
  /** Resolutions the adapter accepts; empty when the model fixes it. */
  resolutions: string[];
  resolutionRequired: boolean;
  defaultResolution: string | null;
  aspects: string[];
  imageUrl: 'required' | 'optional' | 'none';
  /** Last frame: `none` for models that take none. */
  endImage: 'required' | 'optional' | 'none';
  params: ParamNames;
}

/**
 * What a video model takes. The engine's `inputs` and `settings` decide; a row without them (an older
 * engine) gets the rule the UI used before: fal 720p/1080p with i2v by model name, Seedance by name.
 */
export function videoCaps(m: ModelCaps): VideoCaps {
  const params = paramNames(m);
  if (m.settings && m.inputs) {
    const res = m.settings.resolution;
    return {
      resolutions: res?.values ?? [],
      resolutionRequired: res?.required ?? false,
      defaultResolution: res ? startingResolution(res) : null,
      aspects: m.settings.aspect?.values ?? [],
      imageUrl: m.inputs.start_image ?? 'optional',
      endImage: m.inputs.end_image ?? 'none',
      params
    };
  }
  if (m.provider === 'fal') {
    return {
      resolutions: ['720p', '1080p'],
      resolutionRequired: false,
      defaultResolution: '720p',
      aspects: ['16:9', '9:16'],
      imageUrl: m.model.includes('image-to-video') ? 'required' : 'optional',
      endImage: 'none',
      params
    };
  }
  if (m.provider === 'wavespeed' && m.model.startsWith('seedance')) {
    return { resolutions: ['480p', '720p', '1080p'], resolutionRequired: true, defaultResolution: '720p', aspects: ['16:9', '9:16'], imageUrl: 'optional', endImage: 'none', params };
  }
  // Kling takes no resolution; Wan bakes it into the endpoint.
  return { resolutions: [], resolutionRequired: false, defaultResolution: null, aspects: ['16:9', '9:16'], imageUrl: 'optional', endImage: 'none', params };
}

export interface ImageCaps {
  resolutions: string[];
  defaultResolution: string | null;
  aspects: string[];
  seed: boolean;
  params: ParamNames;
}

export function imageCaps(m: ModelCaps): ImageCaps {
  const params = paramNames(m);
  if (m.settings) {
    const res = m.settings.resolution;
    return {
      resolutions: res?.values ?? [],
      defaultResolution: res ? startingResolution(res) : null,
      aspects: m.settings.aspect?.values ?? [],
      seed: m.settings.seed !== undefined,
      params
    };
  }
  if (m.provider === 'nanobanana') {
    return {
      resolutions: ['1K', '2K', '4K'],
      defaultResolution: '2K',
      aspects: ['1:1', '16:9', '9:16', '4:3', '3:4', '3:2', '2:3', '21:9'],
      seed: false,
      params
    };
  }
  if (m.provider === 'wavespeed') return { resolutions: [], defaultResolution: null, aspects: ['1:1', '16:9', '9:16', '3:2', '2:3'], seed: true, params };
  if (m.provider === 'fal') return { resolutions: [], defaultResolution: null, aspects: ['1:1', '16:9', '9:16', '4:3', '3:4'], seed: true, params };
  return { resolutions: [], defaultResolution: null, aspects: ['1:1', '16:9', '9:16', '4:3'], seed: false, params };
}

export interface FormValues {
  aspect?: string;
  resolution?: string;
  duration?: number | null;
  seed?: string;
  imageUrl?: string;
  /** Last frame of a video that takes one. */
  endImageUrl?: string;
  voiceId?: string;
  avatarId?: string;
  audioUrl?: string;
  /** The prompt text; HeyGen refuses a script together with an audio URL. */
  prompt?: string;
}

export type ParamResult = { ok: true; params: Record<string, unknown> } | { ok: false; error: string };

const URL_RE = /^https?:\/\/\S+$/i;

/** Whether an input value must be an http(s) URL (every input but a local file path). */
function needsUrl(accepts: IoFact['accepts']): boolean {
  return accepts !== 'path';
}

export function buildParams(m: ModelCaps, form: FormValues): ParamResult {
  const params: Record<string, unknown> = {};
  if (m.kind === 'image') {
    const caps = imageCaps(m);
    if (form.aspect && caps.aspects.includes(form.aspect)) params[caps.params.aspect] = form.aspect;
    if (caps.resolutions.length) {
      const res = form.resolution && caps.resolutions.includes(form.resolution) ? form.resolution : caps.defaultResolution;
      if (res) params[caps.params.resolution] = res;
    }
    const seed = (form.seed ?? '').trim();
    if (seed) {
      if (!caps.seed) return { ok: false, error: 'This model does not take a seed.' };
      if (!/^-?\d+$/.test(seed)) return { ok: false, error: 'Seed must be a whole number.' };
      params[caps.params.seed] = Number.parseInt(seed, 10);
    }
    return { ok: true, params };
  }
  if (m.kind === 'video') {
    const caps = videoCaps(m);
    const spec = durationSpec(m);
    if (spec) {
      if (!isValidDuration(spec, form.duration)) {
        return { ok: false, error: spec.type === 'set' ? `Duration must be one of ${spec.values.join(', ')} seconds.` : `Duration must be ${spec.min}-${spec.max} seconds.` };
      }
      params[caps.params.duration] = form.duration;
    }
    if (form.aspect && caps.aspects.includes(form.aspect)) params[caps.params.aspect] = form.aspect;
    if (caps.resolutions.length) {
      const res = form.resolution && caps.resolutions.includes(form.resolution) ? form.resolution : caps.defaultResolution;
      if (!res && caps.resolutionRequired) return { ok: false, error: 'Pick a resolution.' };
      if (res) params[caps.params.resolution] = res;
    }
    const url = (form.imageUrl ?? '').trim();
    if (caps.imageUrl === 'none') {
      // The model reads no start frame; a typed one from an earlier model is not sent.
    } else if (url) {
      if (!URL_RE.test(url)) return { ok: false, error: 'Start image must be an http(s) URL.' };
      params[caps.params.startImage] = url;
    } else if (caps.imageUrl === 'required') {
      return { ok: false, error: 'Image-to-video needs a start image URL.' };
    }
    if (caps.endImage !== 'none') {
      const end = (form.endImageUrl ?? '').trim();
      if (end) {
        if (!URL_RE.test(end)) return { ok: false, error: 'End image must be an http(s) URL.' };
        params[caps.params.endImage] = end;
      } else if (caps.endImage === 'required') {
        return { ok: false, error: 'This model needs an end image URL.' };
      }
    }
    return { ok: true, params };
  }
  if (m.kind === 'avatar') {
    const caps = avatarCaps(m);
    const audio = (form.audioUrl ?? '').trim();
    const image = (form.imageUrl ?? '').trim();
    const voice = (form.voiceId ?? '').trim();
    if (audio && needsUrl(caps.audioAccepts) && !URL_RE.test(audio)) return { ok: false, error: 'Audio must be an http(s) URL.' };
    if (caps.scripted) {
      const avatar = (form.avatarId ?? '').trim();
      if (!avatar) return { ok: false, error: 'Enter a HeyGen avatar id.' };
      params[caps.avatarIdParam] = avatar;
      if (audio && (form.prompt ?? '').trim()) return { ok: false, error: 'Use a script or an audio URL, not both.' };
      if (!audio && !(form.prompt ?? '').trim()) return { ok: false, error: 'Enter a script or an audio URL.' };
      if (audio) params[caps.audioParam] = audio;
      else if (voice) params[caps.voiceIdParam] = voice;
      return { ok: true, params };
    }
    if (!image || (needsUrl(caps.imageAccepts) && !URL_RE.test(image))) return { ok: false, error: caps.imageAccepts === 'path' ? 'This avatar model needs the full path to a portrait image.' : 'This avatar model needs a portrait image URL.' };
    if (!audio) return { ok: false, error: caps.audioAccepts === 'path' ? 'This avatar model needs the full path to an audio file.' : 'This avatar model needs an audio URL.' };
    params[caps.imageParam] = image;
    params[caps.audioParam] = audio;
    if (form.resolution && caps.resolutions.includes(form.resolution)) params.resolution = form.resolution;
    return { ok: true, params };
  }
  if (m.kind === 'voice') {
    const voice = (form.voiceId ?? '').trim();
    if (voice) {
      const spec = voiceIdSpec(m);
      if (spec === null) return { ok: true, params };
      if (spec.pattern && !new RegExp(spec.pattern).test(voice)) return { ok: false, error: 'Voice id may only contain letters, digits, - and _.' };
      params[spec.param] = voice;
    }
    return { ok: true, params };
  }
  return { ok: true, params };
}

/**
 * The voice id a voice model reads, or null when the engine says the model reads none (its `inputs` lack
 * `voice_id`). A row without `inputs` gets the old rule: every model takes `voice_id`, and ElevenLabs
 * ids are letters, digits, `-` and `_`.
 */
export function voiceIdSpec(m: ModelCaps): { param: string; pattern?: string } | null {
  if (m.inputs) return m.inputs.voice_id ? { param: m.inputs.voice_id.param, pattern: m.inputs.voice_id.pattern } : null;
  return { param: 'voice_id', pattern: m.provider === 'elevenlabs' ? '^[A-Za-z0-9_-]+$' : undefined };
}

/** What the person should do next for an engine error code. */
export function nextStep(code: string | null | undefined): { text: string; action?: 'keys' | 'settings' } | null {
  switch (code) {
    case 'no_credentials':
    case 'no_key':
    case 'auth':
      return { text: 'Add or re-test this provider key in Models & Keys.', action: 'keys' };
    case 'spend_cap':
      return { text: 'The daily spend cap would be passed. Raise the cap in Models & Keys, or try again tomorrow.', action: 'settings' };
    case 'voice_unavailable':
      return { text: 'That ElevenLabs voice is not available on your plan.' };
    case 'billing':
      return { text: 'The provider rejected the job for billing reasons. Check credit on the provider account.' };
    case 'invalid_request':
    case 'invalid_input':
      return { text: 'Change the prompt or options and try again.' };
    case 'rate_limited':
      return { text: 'The provider is rate limiting. Wait a minute and retry.' };
    case 'transport':
    case 'unavailable':
      return { text: 'The provider could not be reached. Retry shortly; the job resumes on its own if it was started.' };
    default:
      return null;
  }
}

/** Split a flattened "code: message" string back apart. */
export function splitError(text: string | null | undefined): { code: string | null; message: string } {
  if (!text) return { code: null, message: '' };
  const m = /^([a-z][a-z0-9_]*):\s*([\s\S]*)$/.exec(text);
  return m ? { code: m[1], message: m[2] } : { code: null, message: text };
}

/** Human line for the latest progress event. */
export function stageLabel(stage: string | null | undefined, message?: string | null): string {
  const base =
    stage === 'submitted' ? 'Submitted to provider'
    : stage === 'polling' ? 'Provider is generating'
    : stage === 'downloading' ? 'Downloading result'
    : stage === 'complete' ? 'Complete'
    : stage === 'failed' ? 'Failed'
    : 'Starting';
  return message ? `${base} · ${message}` : base;
}

export function formatDuration(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds)) return '';
  if (seconds < 60) return `${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} s`;
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

export interface AvatarCaps {
  /** `script_and_avatar_id` (HeyGen): an avatar id plus a script or an audio URL. Otherwise a portrait image plus audio. */
  scripted: boolean;
  needsImage: boolean;
  resolutions: string[];
  defaultResolution: string | null;
  imageParam: string;
  imageAccepts: IoFact['accepts'];
  audioParam: string;
  audioAccepts: IoFact['accepts'];
  avatarIdParam: string;
  voiceIdParam: string;
}

/**
 * Avatar models. The engine's `inputs.mode` decides: `script_and_avatar_id` takes avatar_id + script or
 * audio_url, `image_and_audio` a portrait image + audio, each under the param the row names. A row
 * without `inputs` gets the old rule: HeyGen by provider id, every other provider as image + audio.
 */
export function avatarCaps(m: ModelCaps): AvatarCaps {
  const res = m.settings?.resolution;
  if (m.inputs?.mode) {
    const scripted = m.inputs.mode === 'script_and_avatar_id';
    return {
      scripted,
      needsImage: !scripted,
      resolutions: res?.values ?? [],
      defaultResolution: res ? startingResolution(res) : null,
      imageParam: m.inputs.image?.param ?? 'image_url',
      imageAccepts: m.inputs.image?.accepts ?? 'url',
      audioParam: m.inputs.audio?.param ?? 'audio_url',
      audioAccepts: m.inputs.audio?.accepts ?? 'url',
      avatarIdParam: m.inputs.avatar_id?.param ?? 'avatar_id',
      voiceIdParam: m.inputs.voice_id?.param ?? 'voice_id'
    };
  }
  const heygen = m.provider === 'heygen';
  return {
    scripted: heygen,
    needsImage: !heygen,
    resolutions: heygen ? [] : ['480p', '720p'],
    defaultResolution: heygen ? null : '480p',
    imageParam: 'image_url',
    imageAccepts: 'url',
    audioParam: 'audio_url',
    audioAccepts: 'url',
    avatarIdParam: 'avatar_id',
    voiceIdParam: 'voice_id'
  };
}

/** Older engines mark no default row; these provider groups were listed first for the kind. */
const LEGACY_PREFERRED_PROVIDER: Record<string, string | undefined> = { voice: 'elevenlabs', avatar: 'heygen' };

/**
 * Provider whose group is listed first for a kind: the provider of the engine's default row. Only when no
 * row of the kind carries the `default` field (an older engine) the old per-kind preference applies.
 */
export function preferredProvider(kind: string, rows: ModelCaps[]): string | undefined {
  const ofKind = rows.filter((r) => r.kind === kind);
  if (ofKind.some((r) => r.default !== undefined)) return ofKind.find((r) => r.default === true)?.provider;
  return LEGACY_PREFERRED_PROVIDER[kind];
}

/** Keyless on-device providers an older engine does not mark with `needs_key: false`. */
const LEGACY_KEYLESS_PROVIDERS = new Set(['local-qts', 'realesrgan']);

/** Whether a model runs without an API key (no spend): the row's `needs_key`, else the old provider list. */
export function isKeyless(m: Pick<ModelCaps, 'provider' | 'needs_key'>): boolean {
  return m.needs_key !== undefined ? m.needs_key === false : LEGACY_KEYLESS_PROVIDERS.has(m.provider);
}

// ---- Music, sound effects, upscale and lipsync ----

export const UPSCALE_SCALES = [2, 3, 4];

const URL_FIELD: IoFact['accepts'] = 'url';

export interface MusicCaps {
  /** Length the person may type, in seconds; null when the model takes none. */
  length: LengthFact | null;
}

/** Older rows: ElevenLabs music takes `music_length_ms` 3-600 s; every other music model `seconds_total`, whole seconds up to 600. */
function legacyMusicLength(m: ModelCaps): LengthFact {
  return m.provider === 'elevenlabs'
    ? { param: 'music_length_ms', unit: 'ms', min: 3000, max: 600000, integer: true }
    : { param: 'seconds_total', unit: 's', min: 1, max: 600, integer: true };
}

export function musicCaps(m: ModelCaps): MusicCaps {
  return { length: m.settings ? m.settings.length ?? null : legacyMusicLength(m) };
}

export interface SfxCaps {
  /** `video`: the sound is made from a video URL (video-to-audio); `prompt`: from a text description. */
  mode: 'video' | 'prompt';
  videoParam: string;
  length: LengthFact | null;
  /** Param of the loop flag; null when the model has no loop option. */
  loopParam: string | null;
}

/** Older rows: fal sfx is video-to-audio with no length or loop; others are text with `duration_seconds` 0.5-30 and `loop`. */
export function sfxCaps(m: ModelCaps): SfxCaps {
  if (m.inputs && m.settings) {
    return {
      mode: m.inputs.mode === 'video' ? 'video' : 'prompt',
      videoParam: m.inputs.video?.param ?? 'video_url',
      length: m.settings.length ?? null,
      loopParam: m.settings.loop?.param ?? null
    };
  }
  const video = m.provider === 'fal';
  return {
    mode: video ? 'video' : 'prompt',
    videoParam: 'video_url',
    length: video ? null : { param: 'duration_seconds', unit: 's', min: 0.5, max: 30, integer: false },
    loopParam: video ? null : 'loop'
  };
}

export interface UpscaleCaps {
  input: 'image' | 'video';
  param: string;
  accepts: IoFact['accepts'];
}

/** Older rows: Real-ESRGAN takes a local `image_path`; a model named for video or Topaz takes `video_url`; the rest `image_url`. */
export function upscaleCaps(m: ModelCaps): UpscaleCaps {
  const mode = m.inputs?.mode;
  if (m.inputs && (mode === 'video' || mode === 'image')) {
    const io = mode === 'video' ? m.inputs.video : m.inputs.image;
    return { input: mode, param: io?.param ?? (mode === 'video' ? 'video_url' : 'image_url'), accepts: io?.accepts ?? URL_FIELD };
  }
  if (m.provider === 'realesrgan') return { input: 'image', param: 'image_path', accepts: 'path' };
  if (m.model.includes('video') || m.model.includes('topaz')) return { input: 'video', param: 'video_url', accepts: URL_FIELD };
  return { input: 'image', param: 'image_url', accepts: URL_FIELD };
}

export interface LipsyncCaps {
  videoParam: string;
  audioParam: string;
  /** Param and values of the sync-mode option; null when the model has none. */
  syncMode: { param: string; values: string[] } | null;
}

/** Older rows: `video_url` + `audio_url`, and a sync mode only on WaveSpeed. */
export function lipsyncCaps(m: ModelCaps): LipsyncCaps {
  if (m.inputs && m.settings) {
    return {
      videoParam: m.inputs.video?.param ?? 'video_url',
      audioParam: m.inputs.audio?.param ?? 'audio_url',
      syncMode: m.settings.sync_mode ?? null
    };
  }
  return {
    videoParam: 'video_url',
    audioParam: 'audio_url',
    syncMode: m.provider === 'wavespeed' ? { param: 'sync_mode', values: ['bounce', 'loop', 'cut_off', 'silence', 'remap'] } : null
  };
}

export interface MediaForm {
  prompt: string;
  secs: string;
  imageUrl: string;
  videoUrl: string;
  audioUrl: string;
  scale: string;
  syncMode: string;
  loop: boolean;
}

/** Params for the music / sfx / upscale / lipsync modalities, under the param names the model's row states. */
export function buildMediaParams(m: ModelCaps, form: MediaForm): ParamResult {
  const p: Record<string, unknown> = {};
  const secs = form.secs.trim();
  const secsN = secs ? Number(secs) : null;
  if (secs && (!Number.isFinite(secsN) || (secsN ?? 0) <= 0)) return { ok: false, error: 'Length must be a positive number of seconds.' };
  const img = form.imageUrl.trim();
  const vid = form.videoUrl.trim();
  const aud = form.audioUrl.trim();
  /** The typed length in the model's unit, checked against its bounds. */
  const lengthParam = (len: LengthFact | null, what: string): string | null => {
    if (secsN == null || !len) return null;
    const value = len.unit === 'ms' ? Math.round(secsN * 1000) : len.integer ? Math.round(secsN) : secsN;
    const lo = len.unit === 'ms' ? len.min / 1000 : len.min;
    const hi = len.unit === 'ms' ? len.max / 1000 : len.max;
    if (value < len.min || value > len.max) return `${what} length must be ${lo}-${hi} seconds.`;
    p[len.param] = value;
    return null;
  };
  switch (m.kind) {
    case 'music': {
      if (!form.prompt.trim()) return { ok: false, error: 'Describe the track first.' };
      const problem = lengthParam(musicCaps(m).length, 'Music');
      return problem ? { ok: false, error: problem } : { ok: true, params: p };
    }
    case 'sfx': {
      const caps = sfxCaps(m);
      if (caps.mode === 'video') {
        if (!vid || !URL_RE.test(vid)) return { ok: false, error: 'Video-to-sound needs an http(s) video URL.' };
        p[caps.videoParam] = vid;
        return { ok: true, params: p };
      }
      if (!form.prompt.trim()) return { ok: false, error: 'Describe the sound first.' };
      const problem = lengthParam(caps.length, 'Sound');
      if (problem) return { ok: false, error: problem };
      if (form.loop && caps.loopParam) p[caps.loopParam] = true;
      return { ok: true, params: p };
    }
    case 'upscale': {
      const caps = upscaleCaps(m);
      const scale = Number(form.scale || '4');
      if (!UPSCALE_SCALES.includes(scale)) return { ok: false, error: 'Scale must be 2, 3 or 4.' };
      const value = caps.input === 'video' ? vid : img;
      if (caps.accepts === 'path') {
        if (!value) return { ok: false, error: 'Local upscale needs a file path to the image.' };
        p[caps.param] = value.replace(/^file:\/\//, '');
      } else {
        if (!value || !URL_RE.test(value)) return { ok: false, error: caps.input === 'video' ? 'Video upscale needs an http(s) video URL.' : 'Image upscale needs an http(s) image URL.' };
        p[caps.param] = value;
      }
      p.scale = scale;
      return { ok: true, params: p };
    }
    case 'lipsync': {
      const caps = lipsyncCaps(m);
      if (!vid || !URL_RE.test(vid)) return { ok: false, error: 'Lipsync needs an http(s) video URL.' };
      if (!aud || !URL_RE.test(aud)) return { ok: false, error: 'Lipsync needs an http(s) audio URL.' };
      p[caps.videoParam] = vid;
      p[caps.audioParam] = aud;
      if (form.syncMode && caps.syncMode?.values.includes(form.syncMode)) p[caps.syncMode.param] = form.syncMode;
      return { ok: true, params: p };
    }
    default:
      return { ok: true, params: p };
  }
}

/** Most reference images the composer attaches. The engine enforces each provider's real limit. */
export const REFERENCE_MAX = 8;

/**
 * Providers whose image models read `params.image_urls` as reference images (mirrors
 * crates/genright-gen/src/providers/nanobanana.rs and evolink.rs). fal and WaveSpeed
 * reference support varies by model, so they are not listed; the engine can declare it per model.
 */
const REFERENCE_PROVIDERS = new Set(['nanobanana', 'evolink']);

export interface ReferenceCaps {
  /** Engine param that carries the reference list. */
  param: string;
  /** Most references the engine accepts for the model (the composer's cap when the engine names none). */
  max: number;
  /** `fal_storage`: local files are uploaded with the run's confirmation. `none`: https URLs only. */
  local_upload?: 'fal_storage' | 'none';
}

/** The engine's `references` field on an image model row (crates/genright-gen/src/providers/mod.rs `ReferenceInput`). */
export interface EngineReferences {
  param: string;
  max?: number;
  local_upload?: 'fal_storage' | 'none';
}

/**
 * Whether a model takes reference images, and under which param. An engine-declared
 * `references` field wins when present (null means declared none); otherwise the provider rule applies.
 * Only image models take references.
 */
export function referenceCaps(m: ModelCaps & { references?: EngineReferences | null }): ReferenceCaps | null {
  if (m.kind !== 'image') return null;
  const declared = m.references;
  if (declared !== undefined) {
    if (declared === null) return null;
    return {
      param: declared.param,
      max: declared.max ?? REFERENCE_MAX,
      ...(declared.local_upload ? { local_upload: declared.local_upload } : {})
    };
  }
  return REFERENCE_PROVIDERS.has(m.provider) ? { param: 'image_urls', max: REFERENCE_MAX } : null;
}

/** Why a typed reference cannot be used, or null. Refuses bare file names: the engine needs a URL or an absolute path. */
export function referencePathError(value: string): string | null {
  const v = value.trim();
  if (!v) return 'Enter a path or URL.';
  if (URL_RE.test(v) || /^file:\/\//i.test(v) || /^(\/|[A-Za-z]:[\\/])/.test(v)) return null;
  return 'Use an http(s) URL or the full path to the image.';
}

/** Add one reference: trimmed, `file://` stripped, no duplicates, capped. On refusal the list is returned unchanged. */
export function addReference(list: string[], value: string, max: number): { list: string[]; error: string | null } {
  const problem = referencePathError(value);
  if (problem) return { list, error: problem };
  const v = value.trim().replace(/^file:\/\//i, '');
  if (list.includes(v)) return { list, error: 'That image is already attached.' };
  if (list.length >= max) return { list, error: `Attach at most ${max} reference images.` };
  return { list: [...list, v], error: null };
}

/** Engine params with the reference list merged in. Models without reference support never receive it. */
export function withReferenceParams(params: Record<string, unknown>, refs: string[], caps: ReferenceCaps | null): Record<string, unknown> {
  if (!caps || refs.length === 0) return params;
  return { ...params, [caps.param]: [...refs] };
}

// ---------------------------------------------------------------- price facts (media modalities)

/**
 * Price facts the engine states per model in `media modalities` (crates/genright-gen/src/catalogue.rs,
 * `describe`: `reconciled` at the top of a model row, `pricing.rounding` on per-megapixel prices). The
 * `models` rows do not carry them. Absent fields mean the engine states nothing about that fact.
 */
export interface ModelPriceFacts {
  /** `false`: the price was read from the provider's page and no real paid run has confirmed it yet. */
  reconciled?: boolean;
  /** `assumed`: the provider's page does not state how it rounds billed megapixels. */
  rounding?: 'stated' | 'assumed';
}

/** Marker label and sentence for a price no real run has confirmed. Neutral, never blocking. */
export const UNCONFIRMED_PRICE_LABEL = 'Unconfirmed price';
export const UNCONFIRMED_PRICE_SENTENCE = "Price from the provider's page, not yet confirmed by a real run";
export const ASSUMED_ROUNDING_SENTENCE = 'rounded up as an assumption';

export function isUnconfirmedPrice(facts: ModelPriceFacts | null | undefined): boolean {
  return facts?.reconciled === false;
}

export function isAssumedRounding(facts: ModelPriceFacts | null | undefined): boolean {
  return facts?.rounding === 'assumed';
}

/** Plain sentences for a model's price caveats, in display order. Empty when the price needs none. */
export function priceFactNotes(facts: ModelPriceFacts | null | undefined): string[] {
  const notes: string[] = [];
  if (isUnconfirmedPrice(facts)) notes.push(UNCONFIRMED_PRICE_SENTENCE);
  if (isAssumedRounding(facts)) notes.push(ASSUMED_ROUNDING_SENTENCE);
  return notes;
}

/**
 * Reads a `media modalities` answer into facts keyed `provider/model`. A model appears only when the engine
 * states at least one fact about it. Anything that is not the expected shape is skipped, never guessed.
 */
export function priceFactsFromModalities(raw: unknown): Record<string, ModelPriceFacts> {
  const out: Record<string, ModelPriceFacts> = {};
  const record = (v: unknown): Record<string, unknown> | null => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null);
  const list = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
  for (const modality of list(record(raw)?.modalities)) {
    for (const provider of list(record(modality)?.providers)) {
      const providerRec = record(provider);
      const providerId = typeof providerRec?.id === 'string' ? providerRec.id : null;
      for (const row of list(providerRec?.models)) {
        const r = record(row);
        const model = typeof r?.model === 'string' ? r.model : null;
        const owner = typeof r?.provider === 'string' ? r.provider : providerId;
        if (!r || !model || !owner) continue;
        const facts: ModelPriceFacts = {};
        if (typeof r.reconciled === 'boolean') facts.reconciled = r.reconciled;
        const rounding = record(r.pricing)?.rounding;
        if (rounding === 'stated' || rounding === 'assumed') facts.rounding = rounding;
        if (Object.keys(facts).length) out[`${owner}/${model}`] = facts;
      }
    }
  }
  return out;
}
