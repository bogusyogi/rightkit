// Pure helpers for the Voice library view (no imports at runtime, so node tests load it directly).
// Command contracts: crates/rightkit-media-gen/src/voices.rs.

export interface VoiceEntry {
  id: string;
  name: string;
  provider: 'elevenlabs' | 'wavespeed' | 'local';
  /** WaveSpeed voices belong to one model. */
  model?: string;
  source: 'api' | 'builtin' | 'local';
  category: string;
  kind?: string;
  preset?: string;
  description?: string | null;
  preview_url?: string | null;
  default: boolean;
  deletable: boolean;
  verified?: boolean;
  prompt_present?: boolean;
  free_plan_usable?: boolean;
  provenance?: { origin?: string; imported_at?: string; source_path?: string; instruct?: string } | null;
}

export interface VoiceWarning {
  code: string;
  message: string;
  provider?: string;
}

export interface VoiceList {
  voices: VoiceEntry[];
  defaults: Record<string, string>;
  warnings: VoiceWarning[];
}

export interface ImportResult {
  source_found: boolean;
  source_dir?: string;
  searched: string[];
  found: number;
  imported: Array<{ id: string; name: string; verified: boolean }>;
  skipped: Array<{ source_id: string; reason: string }>;
  errors: Array<{ source_id: string; message: string }>;
  verification: string;
}

export interface BatchLine {
  n: number;
  job_id?: string | null;
  text: string;
  status: 'queued' | 'submitted' | 'polling' | 'downloading' | 'complete' | 'failed' | 'cancelled' | 'rejected' | string;
  file: string | null;
  error: { code?: string; message?: string } | null;
}

export interface BatchStatus {
  batch_id: string;
  provider: string;
  model: string;
  output_dir: string;
  total: number;
  completed: number;
  failed: number;
  running: number;
  /** Lines the engine holds for the user (`outcome_unknown`); not running, not finished. */
  held?: number;
  est_usd: number;
  finished: boolean;
  lines: BatchLine[];
}

export interface Tier {
  tier: string | null;
  can_use_instant_voice_cloning: boolean;
  character_count: number | null;
  character_limit: number | null;
}

export interface VoiceModelFile {
  name: string;
  expected_bytes: number;
  bytes: number;
  present: boolean;
}

export interface VoiceInstallState {
  state: 'idle' | 'running' | 'done' | 'error' | string;
  file?: string;
  downloaded_bytes?: number;
  file_bytes?: number;
  total_bytes?: number;
  running?: boolean;
  ready?: boolean;
  error?: { code?: string; message?: string } | null;
}

export interface VoiceModeStatus {
  installed: boolean;
  ready: boolean;
  model_dir: string;
  local_only: boolean;
  files?: string[];
}

export interface VoicePresets { speakers: string[]; installed: boolean }

/** studio.voices.models_status: what the on-device engine still needs. */
export interface VoiceModelsStatus {
  engine: string;
  model_dir: string;
  files: VoiceModelFile[];
  models_installed: boolean;
  modes?: Record<'base' | 'custom_voice' | 'voice_design', VoiceModeStatus>;
  onnxruntime: { present: boolean; path?: string | null; name: string; supported_platform: boolean };
  worker: { present: boolean; path?: string | null };
  ready: boolean;
  download_bytes: number;
  install?: VoiceInstallState;
}

export function fmtBytes(n: number | undefined): string {
  const b = n ?? 0;
  if (b >= 1e9) return `${(b / 1e9).toFixed(2)} GB`;
  if (b >= 1e6) return `${(b / 1e6).toFixed(0)} MB`;
  return `${(b / 1e3).toFixed(0)} KB`;
}

/** One-line install summary for the engine card. */
export function installSummaryText(s: VoiceModelsStatus): string {
  const missing = s.files.filter((f) => !f.present).map((f) => f.name);
  const bits: string[] = [];
  if (missing.length) bits.push(`${missing.length} model ${missing.length === 1 ? 'file' : 'files'} missing (${fmtBytes(s.download_bytes)})`);
  if (!s.onnxruntime.present) bits.push(`${s.onnxruntime.name} missing`);
  if (!s.worker.present) bits.push('voice worker missing (shipped with the app)');
  return bits.join('; ');
}

export const MAX_BATCH_LINES = 500;

/** One generated file per non-empty line, as the engine counts them. */
export function splitLines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
}

export function providerTitle(v: Pick<VoiceEntry, 'provider' | 'model'>): string {
  if (v.provider === 'elevenlabs') return 'ElevenLabs';
  if (v.provider === 'wavespeed') return `WaveSpeed › ${v.model ?? ''}`.trim();
  return 'On this computer';
}

export function groupVoices(voices: VoiceEntry[]): Array<{ key: string; title: string; items: VoiceEntry[] }> {
  const out: Array<{ key: string; title: string; items: VoiceEntry[] }> = [];
  for (const v of voices) {
    const key = v.provider === 'wavespeed' ? `wavespeed/${v.model ?? ''}` : v.provider;
    let g = out.find((x) => x.key === key);
    if (!g) {
      g = { key, title: providerTitle(v), items: [] };
      out.push(g);
    }
    g.items.push(v);
  }
  const order = (k: string) => (k === 'elevenlabs' ? 0 : k.startsWith('wavespeed') ? 1 : 2);
  return out.sort((a, b) => order(a.key) - order(b.key));
}

export function voiceSub(v: VoiceEntry): string {
  const bits: string[] = [];
  if (v.provider === 'local') bits.push(v.kind === 'imported' ? 'Imported from VoiceRight' : v.kind === 'base' ? 'Base voice' : 'Cloned');
  else bits.push(v.category);
  if (v.provider === 'elevenlabs' && v.free_plan_usable === false) bits.push('needs a paid plan');
  return bits.join(' · ');
}

/** Model a batch should use for a voice: WaveSpeed voices are bound to theirs;
 * local voices synthesize through the local-qts provider's single model. */
export function modelFor(v: VoiceEntry, voiceModels: Array<{ provider: string; model: string; default?: boolean }>): string | null {
  if (v.provider === 'wavespeed') return v.model ?? null;
  const engine = v.provider === 'local' ? 'local-qts' : v.provider;
  const own = voiceModels.filter((m) => m.provider === engine);
  // The engine marks its own default voice row; an older engine marks none, and the draft model was the default then.
  const engineMarks = voiceModels.some((m) => m.default !== undefined);
  const preferred = engineMarks ? own.find((m) => m.default === true) : own.find((m) => m.model === 'eleven_v4_turbo');
  return preferred?.model ?? own[0]?.model ?? null;
}

/** Every listed voice can synthesize; local voices go through local-qts. */
export function canBatch(v: VoiceEntry | undefined): boolean {
  return Boolean(v);
}

export function batchBlocker(args: { lines: number; voice: VoiceEntry | undefined; model: string | null; keyReady: boolean }): string | null {
  if (!args.voice) return 'Pick a voice.';
  if (!canBatch(args.voice)) return 'Local voices synthesize once the on-device voice engine is installed.';
  if (args.voice.provider === 'local' && args.voice.prompt_present === false) return 'This voice has no voice-prompt file; it cannot synthesize.';
  if (!args.model) return 'No speech model is available for this voice.';
  if (args.lines === 0) return 'Type at least one line.';
  if (args.lines > MAX_BATCH_LINES) return `A batch is limited to ${MAX_BATCH_LINES} lines.`;
  if (!args.keyReady) return 'Add the provider key in Settings first.';
  return null;
}

/** Mirrors the engine's `cost_notice` for an ElevenLabs instant clone (rightkit-media-gen voices.rs CLONE_COST_NOTICE). */
export const CLONE_COST_NOTICE = 'No per-call charge; uses a plan voice slot.';

export function cloneBlocker(args: { name: string; path: string; consent: boolean; canClone: boolean | null }): string | null {
  if (args.canClone === false) return 'This ElevenLabs plan does not include instant voice cloning.';
  if (!args.name.trim()) return 'Name the voice.';
  if (!args.path.trim()) return 'Give the path of a clean reference recording (WAV, MP3, M4A, FLAC or OGG, under 10 MB).';
  if (!args.consent) return 'Confirm the speaker has consented.';
  return null;
}

const ERRORS: Record<string, { text: string; next?: string }> = {
  consent_required: { text: 'Cloning needs the speaker’s consent.', next: 'Tick the consent box.' },
  clone_not_allowed: { text: 'ElevenLabs does not allow instant cloning on this account.', next: 'Upgrade the plan in ElevenLabs, or clone locally later.' },
  no_credentials: { text: 'No ElevenLabs key is stored.', next: 'Add it in Settings.' },
  not_a_clone: { text: 'Only cloned voices can be deleted.' },
  too_many_lines: { text: 'That batch is too long.', next: 'Split it into batches of 500 lines or fewer.' },
  provider_unavailable: { text: 'That provider is not available for this voice.', next: 'Pick another voice.' },
  voice_unavailable: { text: 'That voice is not available on this plan.', next: 'Pick a premade voice.' },
  voice_prompt_missing: { text: 'This voice has no voice-prompt file.', next: 'Re-import or re-clone it, or delete the broken record.' },
  not_installed: { text: 'The on-device engine or model files are not installed yet.', next: 'Install the local voice model, then try again.' },
  integrity: { text: 'A downloaded file failed its integrity check and was removed.', next: 'Run Install again — completed files are kept.' },
  transport: { text: 'The download could not reach the server.', next: 'Check the connection and run Install again; it resumes where it stopped.' },
  verify_pending: { text: 'The on-device voice model is not installed yet, so this voice cannot be checked.', next: 'It stays unverified until the model files are installed.' },
  daily_cap_exceeded: { text: 'The daily spend cap would be exceeded.', next: 'Raise the cap in Settings.' },
  ffmpeg_missing: { text: 'No ffmpeg is available for conversion.', next: 'Export as WAV, or install ffmpeg.' },
  nothing_to_export: { text: 'Nothing has finished in this batch yet.', next: 'Wait for lines to complete, then export.' },
  export_failed: { text: 'The audio conversion failed.', next: 'Try WAV output, or check that the source files still exist.' },
  file_not_found: { text: 'The source audio file no longer exists.', next: 'Regenerate or re-export from the batch.' }
};

export function explainError(code: string | null, message: string): { text: string; next?: string } {
  const known = code ? ERRORS[code] : undefined;
  return known ?? { text: message.replace(/^[a-z][a-z0-9_]*:\s*/, '') };
}

export function lineTone(status: string): 'ok' | 'bad' | 'run' | 'warn' | undefined {
  if (status === 'complete') return 'ok';
  if (status === 'outcome_unknown') return 'warn';
  if (status === 'failed' || status === 'rejected') return 'bad';
  if (status === 'cancelled') return undefined;
  return 'run';
}

export function lineLabel(status: string): string {
  switch (status) {
    case 'complete': return 'Done';
    case 'failed': return 'Failed';
    case 'rejected': return 'Rejected';
    case 'cancelled': return 'Cancelled';
    case 'queued': return 'Queued';
    // Never show the raw status; Voices.tsx refines this with the held-job wording.
    case 'outcome_unknown': return 'Outcome unknown';
    default: return 'Speaking…';
  }
}

export function importSummary(r: ImportResult): string {
  if (!r.source_found) return 'No VoiceRight voices were found on this computer. Choose its voices folder to import from elsewhere.';
  const parts = [`${r.imported.length} imported`];
  const already = r.skipped.filter((s) => s.reason === 'already_imported').length;
  if (already) parts.push(`${already} already here`);
  const other = r.skipped.length - already;
  if (other) parts.push(`${other} skipped`);
  if (r.errors.length) parts.push(`${r.errors.length} failed`);
  return `${parts.join(', ')}. Imported voices stay unverified until they are test-synthesized.`;
}
