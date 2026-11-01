// Browser-mode stand-in for studio.voices.* (loaded only by a DEV build; a release build never contains it).
import { splitLines, CLONE_COST_NOTICE, type VoiceEntry, type VoiceList, type ImportResult, type BatchLine, type BatchStatus, type Tier, type VoicePresets, type VoiceModelsStatus } from './voices-logic.ts';

/** Generated 0.5 s sine WAV as a data URL for browser-mode previews (no server). */
export function mockWavDataUri(): string {
  const sr = 8000;
  const n = sr / 2;
  const bytes = new Uint8Array(44 + n * 2);
  const dv = new DataView(bytes.buffer);
  const w = (o: number, s: string) => { for (let i = 0; i < s.length; i++) bytes[o + i] = s.charCodeAt(i); };
  w(0, 'RIFF'); dv.setUint32(4, 36 + n * 2, true); w(8, 'WAVEfmt ');
  dv.setUint32(16, 16, true); dv.setUint16(20, 1, true); dv.setUint16(22, 1, true);
  dv.setUint32(24, sr, true); dv.setUint32(28, sr * 2, true); dv.setUint16(32, 2, true); dv.setUint16(34, 16, true);
  w(36, 'data'); dv.setUint32(40, n * 2, true);
  for (let i = 0; i < n; i++) {
    const env = Math.min(1, Math.min(i / 400, (n - i) / 400));
    dv.setInt16(44 + i * 2, Math.round(Math.sin((2 * Math.PI * 440 * i) / sr) * 12000 * env), true);
  }
  let bin = '';
  for (let i = 0; i < bytes.length; i += 8192) bin += String.fromCharCode(...bytes.subarray(i, i + 8192));
  return `data:audio/wav;base64,${btoa(bin)}`;
}

// ---------------------------------------------------------------- browser-mode mock

/** In-memory stand-in for studio.voices.* when the app runs in a plain browser. */
export function createMockVoicesEngine(now: () => number = Date.now) {
  const voices: VoiceEntry[] = [
    { id: 'JBFqnCBsd6RMkjVDRZzb', name: 'George', provider: 'elevenlabs', source: 'api', category: 'premade', description: 'Warm, captivating storyteller', default: true, deletable: false, free_plan_usable: true },
    { id: 'EXAVITQu4vr4xnSDxMaL', name: 'Sarah', provider: 'elevenlabs', source: 'api', category: 'premade', description: 'Mature, reassuring', default: false, deletable: false, free_plan_usable: true },
    { id: 'cloneMock1', name: 'My studio voice', provider: 'elevenlabs', source: 'api', category: 'cloned', default: false, deletable: true, free_plan_usable: true },
    { id: 'libMock1', name: 'Shared library narrator', provider: 'elevenlabs', source: 'api', category: 'professional', default: false, deletable: false, free_plan_usable: false },
    { id: 'Alicia', name: 'Alicia', provider: 'wavespeed', model: 'elevenlabs-turbo-v2.5', source: 'builtin', category: 'default', default: true, deletable: false },
    { id: 'Wise_Woman', name: 'Wise Woman', provider: 'wavespeed', model: 'minimax-speech-02-hd', source: 'builtin', category: 'default', default: true, deletable: false },
    { id: 'Deep_Voice_Man', name: 'Deep Voice Man', provider: 'wavespeed', model: 'minimax-speech-02-hd', source: 'builtin', category: 'default', default: false, deletable: false },
    { id: 'vr-voice-1', name: 'Adrian', provider: 'local', source: 'local', category: 'imported', kind: 'imported', default: false, deletable: true, verified: false, prompt_present: true, provenance: { origin: 'voiceright', imported_at: '2026-10-06T09:00:00Z' } }
  ];
  const batches = new Map<string, { id: string; at: number; lines: string[]; dir: string; model: string; provider: string }>();
  let n = 0;
  // The browser mock starts with the on-device engine "not installed" so the
  // install card is exercised; install() flips it through running → done.
  let installed = false;
  let installAt = 0;
  const modelsStatus = (): VoiceModelsStatus => {
    const elapsed = installed ? now() - installAt : -1;
    const running = installed && elapsed < 4000;
    return {
      engine: 'qts',
      model_dir: '~/Library/Application Support/App/models/qts',
      files: [
        { name: 'qwen3-tts-0.6b-q8_0.gguf', expected_bytes: 1_325_190_016, bytes: installed && !running ? 1_325_190_016 : running ? Math.min(1_325_190_016, Math.round((elapsed / 4000) * 1_325_190_016)) : 0, present: installed && !running },
        { name: 'qwen3-tts-vocoder.onnx', expected_bytes: 459_514_304, bytes: installed && !running ? 459_514_304 : 0, present: installed && !running }
      ],
      models_installed: installed && !running,
      modes: {
        base: { installed: installed && !running, ready: installed && !running, model_dir: '~/Library/Application Support/App/models/qts', local_only: false },
        custom_voice: { installed: false, ready: false, model_dir: '~/Library/Application Support/App/models/qts', local_only: true },
        voice_design: { installed: false, ready: false, model_dir: '~/Library/Application Support/App/models/qts', local_only: true }
      },
      onnxruntime: { present: installed && !running, path: installed && !running ? 'GenRight.app/Contents/Resources/runtime/libonnxruntime.dylib' : null, name: 'libonnxruntime.dylib', supported_platform: true },
      worker: { present: true, path: 'GenRight.app/Contents/MacOS/genright-qts-worker' },
      ready: installed && !running,
      download_bytes: 1_784_704_320,
      install: running ? { state: 'running', file: 'qwen3-tts-0.6b-q8_0.gguf', downloaded_bytes: Math.round((elapsed / 4000) * 1_784_704_320), total_bytes: 1_784_704_320, running: true } : installed ? { state: 'done' } : { state: 'idle' }
    };
  };
  let wavUri: string | null = null;
  const reject = (code: string, message: string): never => {
    throw Object.assign(new Error(`${code}: ${message}`), { code });
  };

  return function call(name: string, input: Record<string, unknown>): unknown {
    switch (name) {
      case 'studio.voices.list': {
        const f = typeof input.provider === 'string' ? input.provider : null;
        return { voices: voices.filter((v) => !f || v.provider === f), defaults: {}, warnings: [] } satisfies VoiceList;
      }
      case 'studio.voices.set_default': {
        const model = typeof input.model === 'string' ? input.model : undefined;
        for (const v of voices) {
          if (v.provider === input.provider && (v.provider !== 'wavespeed' || v.model === model)) v.default = v.id === input.voice_id;
        }
        return { ok: true };
      }
      case 'studio.voices.delete': {
        const i = voices.findIndex((v) => v.id === input.id && v.deletable);
        if (i < 0) reject('not_a_clone', 'only cloned voices can be deleted');
        voices.splice(i, 1);
        return { deleted: input.id };
      }
      case 'studio.voices.rename': {
        const v = voices.find((x) => x.id === input.id && x.provider === 'local');
        if (!v) return reject('voice_not_found', `no local voice ${String(input.id)}`);
        const nm = String(input.name ?? '').trim();
        if (!nm) reject('invalid_input', 'missing field: name');
        v.name = nm;
        return { id: v.id, name: nm };
      }
      case 'studio.voices.verify': {
        const v = voices.find((x) => x.id === input.id && x.provider === 'local');
        if (!v) return reject('voice_not_found', `no local voice ${String(input.id)}`);
        if (!modelsStatus().ready) return reject('verify_pending', 'the on-device voice model is not installed');
        if (v.prompt_present === false) reject('voice_prompt_missing', 'the voice record has no prompt file');
        v.verified = true;
        return { id: v.id, verified: true };
      }
      case 'studio.voices.presets':
        return { speakers: ['vivian', 'serena', 'uncle_fu', 'dylan', 'eric', 'ryan', 'aiden', 'ono_anna', 'sohee'], installed: false } satisfies VoicePresets;
      case 'studio.voices.design':
        return reject('not_installed', 'model_missing: voice_design');
      case 'studio.voices.preview': {
        if (input.preset) return reject('not_installed', 'model_missing: custom_voice');
        const v = voices.find((x) => x.id === input.id);
        if (!v) return reject('voice_not_found', `no local voice ${String(input.id)}`);
        if (v.provider === 'local' && !modelsStatus().ready) return reject('not_installed', 'the on-device voice model is not installed');
        if (!wavUri) wavUri = mockWavDataUri();
        return { id: v.id, path: wavUri, bytes: 8044 };
      }
      case 'studio.voices.clone_local': {
        if (input.consent !== true) reject('consent_required', 'consent is required');
        if (!modelsStatus().ready) return reject('not_installed', 'the on-device voice model is not installed');
        const nm = String(input.name ?? '').trim();
        if (!nm) reject('invalid_input', 'missing field: name');
        const id = nm.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || `voice-${++n}`;
        voices.push({ id, name: nm, provider: 'local', source: 'local', category: 'clone', kind: 'clone', default: false, deletable: true, verified: false, prompt_present: true, provenance: { origin: 'local_clone' } });
        return { id, name: nm, provider: 'local', verified: false };
      }
      case 'studio.voices.models_status':
        return modelsStatus();
      case 'studio.voices.install':
        if (!installed) {
          installed = true;
          installAt = now();
        }
        return modelsStatus().install;
      case 'studio.voices.install_status':
        return modelsStatus().install;
      case 'studio.voices.import_voiceright':
        return { source_found: true, source_dir: '~/Library/Application Support/com.rightsuite.voiceright/engine/voices', searched: [], found: 1, imported: [], skipped: [{ source_id: 'voice-1', reason: 'already_imported' }], errors: [], verification: 'pending' } satisfies ImportResult;
      case 'studio.voices.tier':
        return { tier: 'starter', can_use_instant_voice_cloning: true, character_count: 4210, character_limit: 30000 } satisfies Tier;
      case 'studio.voices.clone_elevenlabs': {
        if (input.consent !== true) reject('consent_required', 'consent is required');
        if (input.dry_run) return { dry_run: true, provider: 'elevenlabs', name: input.name, est_usd: null, cost_notice: CLONE_COST_NOTICE };
        const id = `cloneMock${++n + 1}`;
        voices.push({ id, name: String(input.name), provider: 'elevenlabs', source: 'api', category: 'cloned', default: false, deletable: true, free_plan_usable: true });
        return { voice_id: id, name: input.name, requires_verification: false, est_usd: null, cost_notice: CLONE_COST_NOTICE };
      }
      case 'studio.voices.batch': {
        if (input.preset) return reject('not_installed', 'model_missing: custom_voice');
        const lines = Array.isArray(input.lines) ? (input.lines as string[]) : splitLines(String(input.text ?? ''));
        if (lines.length === 0) reject('invalid_input', 'the batch has no non-empty lines');
        if (input.dry_run) return { dry_run: true, lines: lines.length, est_usd: lines.join('').length / 1000 * 0.04 };
        const id = `mock${++n}`;
        const dir = typeof input.output_dir === 'string' && input.output_dir ? input.output_dir : `~/Library/Application Support/App/voices/batches/${id}`;
        batches.set(id, { id, at: now(), lines, dir, model: String(input.model ?? ''), provider: String(input.provider ?? '') });
        return mockStatus(batches.get(id)!, now());
      }
      case 'studio.voices.batch_status': {
        const b = batches.get(String(input.batch_id));
        if (!b) reject('batch_not_found', 'no such batch');
        return mockStatus(b!, now());
      }
      case 'studio.voices.export': {
        const b = input.batch_id ? batches.get(String(input.batch_id)) : undefined;
        if (input.batch_id && !b) reject('batch_not_found', 'no such batch');
        const fmt = typeof input.format === 'string' ? input.format : 'wav';
        if (fmt === 'mp3') reject('invalid_input', 'mp3 export was replaced by m4a (AAC); use format wav, flac or m4a');
        if (!['wav', 'flac', 'm4a'].includes(fmt)) reject('invalid_input', 'format must be wav, flac or m4a');
        const base = b ? b.dir : '~/Library/Application Support/App/voices/exports';
        const names = b ? b.lines.map((_, i) => `line-${String(i + 1).padStart(3, '0')}.${fmt}`) : [`clip.${fmt}`];
        const dir = `${base}/export-${fmt}`;
        const files = names.map((n) => ({ name: n, path: `${dir}/${n}`, bytes: 88000, source: `${base}/${n.replace(fmt, 'wav')}` }));
        return {
          format: fmt, normalized: input.normalize === true, count: files.length, dir,
          files, zip: input.zip === true ? `${dir}/${b ? b.id : 'voice-export'}-${fmt}.zip` : null,
          note: input.normalize === true ? 'EBU R128 loudness normalised (I=-16, TP=-1.5, LRA=11)' : 'as-synthesised loudness'
        };
      }
      default:
        return reject('unknown_command', `unknown command: ${name}`);
    }
  };
}

function mockStatus(b: { id: string; at: number; lines: string[]; dir: string; model: string; provider: string }, t: number): BatchStatus {
  const done = Math.min(b.lines.length, Math.floor((t - b.at) / 900));
  const lines: BatchLine[] = b.lines.map((text, i) => ({
    n: i + 1,
    text,
    status: i < done ? 'complete' : i === done ? 'polling' : 'queued',
    file: i < done ? `${b.dir}/line-${String(i + 1).padStart(3, '0')}.mp3` : null,
    error: null
  }));
  return {
    batch_id: b.id, provider: b.provider, model: b.model, output_dir: b.dir, total: lines.length,
    completed: done, failed: 0, running: lines.length - done, est_usd: b.lines.join('').length / 1000 * 0.04,
    finished: done >= lines.length, lines
  };
}
