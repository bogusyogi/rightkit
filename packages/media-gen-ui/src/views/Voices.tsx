import { useCallback, useEffect, useMemo, useState } from 'react';
import { assetUrl, errorCode, errorMessage, isHeldJob, isTauri, resolveJob, studio } from '../lib/api';
import { heldActionLabel, heldActions, heldConsequence, heldExplanation, heldLabel, isRecoveredCode, type HeldAction } from '../lib/held-job';
import { useApp } from '../lib/app-state';
import { useConfirm } from '../lib/confirm';
import { Card, Badge, Button, Checkbox, EmptyState, Input, Notice, ProgressBar, SegmentedControl, Select } from '@rightkit/react-shell/react';
import { providerName, usd } from '../components/ui';
import { MediaPlayer } from '@rightkit/media-ui';
import {
  MAX_BATCH_LINES,
  batchBlocker,
  canBatch,
  cloneBlocker,
  CLONE_COST_NOTICE,
  explainError,
  fmtBytes,
  groupVoices,
  importSummary,
  installSummaryText,
  lineLabel,
  lineTone,
  modelFor,
  splitLines,
  voiceSub,
  type BatchLine,
  type BatchStatus,
  type ImportResult,
  type Tier,
  type VoiceEntry,
  type VoiceList,
  type VoicePresets,
  type VoiceModelsStatus
} from './voices-logic';
import './voices.css';
import { hostAppName } from "../appName";

/** The in-memory studio.voices.* stand-in: a DEV build's browser mode only; a release build never contains it. */
type VoicesMock = ReturnType<typeof import('./voices-mock').createMockVoicesEngine>;
let mock: Promise<VoicesMock> | undefined;
function loadMock(): Promise<VoicesMock> {
  if (!import.meta.env.DEV) return Promise.reject(new Error(`${hostAppName()} needs the desktop app.`));
  mock ??= import('./voices-mock').then((m) => m.createMockVoicesEngine());
  return mock;
}

/** studio.voices.* through the engine; an in-memory stand-in in a plain browser (DEV only). */
async function call<T>(verb: string, input: Record<string, unknown> = {}): Promise<T> {
  const name = `studio.voices.${verb}`;
  if (!isTauri()) return (await loadMock())(name, input) as T;
  return studio<T>(name, input);
}

type Tab = 'library' | 'batch' | 'clone';
function badgeTone(t: 'ok' | 'bad' | 'run' | 'warn' | undefined): 'ok' | 'bad' | 'accent' | 'warn' | undefined {
  return t === 'run' ? 'accent' : t;
}

interface Problem { code: string | null; message: string }

function ProblemNote({ problem }: { problem: Problem | null }) {
  if (!problem) return null;
  const x = explainError(problem.code, problem.message);
  return (
    <Notice tone="bad">
      {x.text}
      {x.next ? <span className="v-next"> {x.next}</span> : null}
    </Notice>
  );
}

function toProblem(e: unknown): Problem {
  return { code: errorCode(e), message: errorMessage(e) };
}

export function Voices() {
  const app = useApp();
  const [tab, setTab] = useState<Tab>('library');
  const [list, setList] = useState<VoiceList | null>(null);
  const [loading, setLoading] = useState(true);
  const [problem, setProblem] = useState<Problem | null>(null);

  const refresh = useCallback(async () => {
    try {
      setList(await call<VoiceList>('list'));
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => { void refresh(); }, [refresh]);

  const voices = list?.voices ?? [];
  const [engine, setEngine] = useState<VoiceModelsStatus | null>(null);
  const refreshEngine = useCallback(async () => {
    try {
      setEngine(await call<VoiceModelsStatus>('models_status'));
    } catch {
      setEngine(null);
    }
  }, []);
  useEffect(() => { void refreshEngine(); }, [refreshEngine]);
  const [presets, setPresets] = useState<VoicePresets | null>(null);
  useEffect(() => { void call<VoicePresets>('presets').then(setPresets).catch((e) => setProblem(toProblem(e))); }, []);
  const installing = engine?.install?.state === 'running' || engine?.install?.running === true;
  useEffect(() => {
    if (!installing) return;
    const t = window.setInterval(() => void refreshEngine(), 2000);
    return () => window.clearInterval(t);
  }, [installing, refreshEngine]);

  return (
    <div className="content view" style={{ gridTemplateColumns: '1fr' }}>
      <section className="work voices">
        <div className="img-head">
          <div><div className="eyebrow">Create</div><h1>Voice library</h1></div>
          <SegmentedControl<Tab> label="Voice library section" value={tab} onChange={(t) => { setTab(t); setProblem(null); }} options={[{ value: 'library', label: 'Voices' }, { value: 'batch', label: 'Batch' }, { value: 'clone', label: 'Clone' }]} />
        </div>
        <ProblemNote problem={problem} />
        <EngineCard engine={engine} refresh={refreshEngine} setProblem={setProblem} />
        {tab === 'library' && <VoiceModes engine={engine} presets={presets} refresh={refresh} refreshEngine={refreshEngine} setProblem={setProblem} />}
        {tab === 'library' && <Library list={list} loading={loading} refresh={refresh} engine={engine} setProblem={setProblem} />}
        {tab === 'batch' && <Batch voices={voices} presets={presets} models={app.models} configured={app.configured} engine={engine} setProblem={setProblem} />}
        {tab === 'clone' && <Clone configured={app.configured} refresh={refresh} engine={engine} setProblem={setProblem} />}
      </section>
    </div>
  );
}

// ---------------------------------------------------------------- on-device engine card

function EngineCard({ engine, refresh, setProblem }: { engine: VoiceModelsStatus | null; refresh: () => Promise<void>; setProblem: (p: Problem | null) => void }) {
  const [busy, setBusy] = useState(false);
  if (!engine) return null;
  const inst = engine.install;
  const running = inst?.state === 'running' || inst?.running === true;
  const pct = running && inst?.total_bytes ? Math.min(100, Math.round(((inst.downloaded_bytes ?? 0) / inst.total_bytes) * 100)) : 0;
  const start = async () => {
    setBusy(true);
    setProblem(null);
    try {
      await call('install');
      await refresh();
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="v-group">
      <div className="model-group-title">On-device voice engine (Qwen3-TTS 0.6B)</div>
      {engine.ready ? (
        <Notice tone="info"><Badge tone="ok">Installed</Badge> <small className="v-count"> Local voices, cloning and batch speech run privately on this computer.</small></Notice>
      ) : running ? (
        <>
          <ProgressBar value={pct} max={100} label="Engine download progress" />
          <small className="v-count">Downloading {inst?.file ?? 'files'} — {fmtBytes(inst?.downloaded_bytes)} of {fmtBytes(inst?.total_bytes)} ({pct}%). It resumes if interrupted.</small>
        </>
      ) : (
        <div className="v-inline">
          <div style={{ flex: 1 }}><Notice tone="warn">
            Not installed — {installSummaryText(engine) || 'model files needed'}.
            {inst?.state === 'error' && inst.error ? <span> Last attempt failed: {inst.error.message ?? 'unknown error'}.</span> : null}
            {!engine.onnxruntime.supported_platform ? <span> This platform has no ONNX Runtime build; local voices are unavailable.</span> : null}
          </Notice></div>
          <Button type="button" variant="primary" disabled={busy || !engine.onnxruntime.supported_platform} onClick={() => void start()}>{busy ? 'Starting…' : `Install (${fmtBytes(engine.download_bytes)})`}</Button>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- design & presets

function VoiceModes({ engine, presets, refresh, refreshEngine, setProblem }: {
  engine: VoiceModelsStatus | null; presets: VoicePresets | null;
  refresh: () => Promise<void>; refreshEngine: () => Promise<void>; setProblem: (p: Problem | null) => void;
}) {
  const [instruct, setInstruct] = useState('');
  const [text, setText] = useState('Welcome. Here is a short sample of this voice, speaking clearly and naturally.');
  const [name, setName] = useState('');
  const [speaker, setSpeaker] = useState('vivian');
  const [presetInstruct, setPresetInstruct] = useState('');
  const [busy, setBusy] = useState<string | null>(null);
  const [designSrc, setDesignSrc] = useState<string | null>(null);
  const [presetSrc, setPresetSrc] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const designReady = engine?.modes?.voice_design.ready === true;
  const presetReady = engine?.modes?.custom_voice.ready === true;
  const saveReady = designReady && engine?.ready === true;
  async function design(save: boolean) {
    setBusy(save ? 'save' : 'design'); setProblem(null); setSaved(null); setDesignSrc(null);
    try {
      const result = await call<{ path: string; voice_id: string | null }>('design', { instruct: instruct.trim(), sample_text: text.trim(), name: name.trim() || undefined, save });
      setDesignSrc(assetUrl(result.path));
      if (result.voice_id) { setSaved(result.voice_id); await refresh(); }
    } catch (e) { setProblem(toProblem(e)); }
    finally { setBusy(null); }
  }
  async function previewPreset() {
    setBusy('preset'); setProblem(null); setPresetSrc(null);
    try {
      const result = await call<{ path: string }>('preview', { preset: speaker, instruct: presetInstruct.trim() || undefined, text: text.trim() });
      setPresetSrc(assetUrl(result.path));
    } catch (e) { setProblem(toProblem(e)); }
    finally { setBusy(null); }
  }
  const hint = (mode: 'voice_design' | 'custom_voice', label: string) => {
    const status = engine?.modes?.[mode];
    return <Notice tone="warn" action={<Button type="button" size="sm" disabled={Boolean(busy)} onClick={() => void refreshEngine()}>Refresh status</Button>}>{status?.installed ? `${label} files installed; voice runtime unavailable.` : `${label} not installed. Install compatible local 1.7B model files in your QTS model folder.`}</Notice>;
  };
  return (
    <div className="v-modes">
      <div className="v-group">
        <div className="model-group-title">Design a voice</div>
        {!designReady ? hint('voice_design', 'VoiceDesign') : null}
        <Card className="form v-form">
          <div><label htmlFor="v-design-instruct">Describe your voice</label><textarea className="rk-input" id="v-design-instruct" rows={3} disabled={!designReady || Boolean(busy)} placeholder="A warm, low narrator with a calm pace. A bright, expressive voice with crisp diction." value={instruct} onChange={(e) => { setInstruct(e.target.value); setDesignSrc(null); setSaved(null); }} /></div>
          <div><label htmlFor="v-design-text">Sample text</label><textarea className="rk-input" id="v-design-text" rows={2} disabled={Boolean(busy)} value={text} onChange={(e) => { setText(e.target.value); setDesignSrc(null); setPresetSrc(null); setSaved(null); }} /></div>
          <div><label htmlFor="v-design-name">Voice name</label><Input id="v-design-name" disabled={!designReady || Boolean(busy)} maxLength={100} placeholder="Designed voice" value={name} onChange={(e) => setName(e.target.value)} /></div>
          <div className="v-inline">
            <Button type="button" disabled={!designReady || Boolean(busy) || !instruct.trim() || !text.trim()} onClick={() => void design(false)}>{busy === 'design' ? 'Speaking…' : 'Preview'}</Button>
            <Button type="button" variant="primary" disabled={!saveReady || Boolean(busy) || !instruct.trim() || !text.trim()} onClick={() => void design(true)}>{busy === 'save' ? 'Saving…' : 'Save as voice'}</Button>
            {designReady && !saveReady ? <small className="v-count">Install Base voice engine above to save.</small> : null}
          </div>
          {designSrc ? <MediaPlayer kind="audio" src={designSrc} title="Designed voice preview" autoPlay={false} compact /> : null}
          {saved ? <Notice tone="info">Saved as {saved}.</Notice> : null}
        </Card>
      </div>
      <div className="v-group">
        <div className="model-group-title">Preset voices</div>
        {!presetReady ? hint('custom_voice', 'CustomVoice') : null}
        <Card className="form v-form">
          <div className="v-two">
            <div><label>Speaker</label><Select label="Speaker" disabled={!presetReady || Boolean(busy)} value={speaker} onChange={(v) => { setSpeaker(v); setPresetSrc(null); }} options={(presets?.speakers ?? []).map((s) => ({ value: s, label: s.replaceAll('_', ' ') }))} /></div>
            <div><label htmlFor="v-preset-instruct">Delivery (optional)</label><Input id="v-preset-instruct" disabled={!presetReady || Boolean(busy)} placeholder="Speak warmly & slowly" value={presetInstruct} onChange={(e) => { setPresetInstruct(e.target.value); setPresetSrc(null); }} /></div>
          </div>
          <div className="v-inline"><Button type="button" disabled={!presetReady || Boolean(busy) || !text.trim()} onClick={() => void previewPreset()}>{busy === 'preset' ? 'Speaking…' : 'Preview preset'}</Button><small className="v-count">Uses sample text above.</small></div>
          {presetSrc ? <MediaPlayer kind="audio" src={presetSrc} title={`${speaker} preview`} autoPlay={false} compact /> : null}
        </Card>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- library

function Library({ list, loading, refresh, engine, setProblem }: { list: VoiceList | null; loading: boolean; refresh: () => Promise<void>; engine: VoiceModelsStatus | null; setProblem: (p: Problem | null) => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [importPath, setImportPath] = useState('');
  const [imported, setImported] = useState<ImportResult | null>(null);
  const [previewKey, setPreviewKey] = useState<string | null>(null);
  const [previewSrc, setPreviewSrc] = useState<Record<string, string>>({});
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameName, setRenameName] = useState('');
  const engineReady = engine?.ready !== false;

  async function run(key: string, fn: () => Promise<unknown>) {
    setBusy(key);
    setProblem(null);
    try {
      await fn();
      await refresh();
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(null);
      setConfirm(null);
    }
  }

  async function togglePreview(v: VoiceEntry, key: string) {
    if (previewKey === key) {
      setPreviewKey(null);
      return;
    }
    setProblem(null);
    if (v.provider === 'local' && !previewSrc[key]) {
      setBusy(`preview:${key}`);
      try {
        const r = await call<{ path: string }>('preview', { id: v.id });
        setPreviewSrc((m) => ({ ...m, [key]: assetUrl(r.path) }));
      } catch (e) {
        setProblem(toProblem(e));
        setBusy(null);
        return;
      }
      setBusy(null);
    }
    setPreviewKey(key);
  }

  const groups = useMemo(() => groupVoices(list?.voices ?? []), [list]);
  const hasLocal = groups.some((g) => g.key === 'local');

  return (
    <>
      <p className="lede v-lede">Provider voices, your clones, and voices imported from VoiceRight. The default voice per provider is what new speech uses.</p>
      {(list?.warnings ?? []).map((w) => <Notice tone="warn" key={`${w.code}${w.provider}`}>{w.message}</Notice>)}
      {loading && !list ? <EmptyState title="Loading voices" /> : null}
      {groups.map((g) => (
        <div className="v-group" key={g.key}>
          <div className="model-group-title">{g.title}</div>
          <Card flush className="row-list">
            {g.items.map((v) => {
              const key = `${v.provider}:${v.id}`;
              const canPreview = Boolean(v.preview_url) || (v.provider === 'local' && engineReady);
              const src = v.provider === 'local' ? previewSrc[key] : v.preview_url ?? undefined;
              return (
                <div className="v-row" key={key}>
                  <div className="v-main">
                    {renameId === v.id ? (
                      <span className="v-inline">
                        <Input aria-label={`Rename ${v.name}`} value={renameName} onChange={(e) => setRenameName(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') void run(key, async () => { await call('rename', { id: v.id, name: renameName.trim() }); setRenameId(null); }); }} />
                        <Button type="button" size="sm" disabled={busy === key || !renameName.trim()} onClick={() => void run(key, async () => { await call('rename', { id: v.id, name: renameName.trim() }); setRenameId(null); })}>Save</Button>
                        <Button type="button" size="sm" onClick={() => setRenameId(null)}>Cancel</Button>
                      </span>
                    ) : (
                      <b>{v.name}</b>
                    )}
                    <small>{voiceSub(v)}{v.description ? ` · ${v.description}` : ''}</small>
                  </div>
                  <div className="v-chips">
                    {v.default ? <Badge tone="ok">Default</Badge> : null}
                    {v.provider === 'local' ? (v.verified ? <Badge tone="ok">Verified</Badge> : <Badge tone="warn">Unverified</Badge>) : null}
                    {v.provider === 'local' && v.prompt_present === false ? <Badge tone="bad">Prompt file missing</Badge> : null}
                  </div>
                  <div className="v-actions">
                    {canPreview ? <Button type="button" size="sm" disabled={busy === `preview:${key}`} onClick={() => void togglePreview(v, key)} aria-label={`Preview ${v.name}`}>{busy === `preview:${key}` ? 'Speaking…' : previewKey === key ? 'Stop' : 'Preview'}</Button> : null}
                    {v.provider === 'local' && !v.verified ? (
                      <Button type="button" size="sm" disabled={busy === key || !engineReady} title={engineReady ? 'Test-synthesize a line with this voice' : 'Install the on-device engine first'} onClick={() => void run(key, () => call('verify', { id: v.id }))}>Verify</Button>
                    ) : null}
                    {v.provider === 'local' ? (
                      <Button type="button" size="sm" aria-label={`Rename ${v.name}`} onClick={() => { setRenameId(v.id); setRenameName(v.name); }}>Rename</Button>
                    ) : null}
                    {!v.default ? (
                      <Button type="button" size="sm" disabled={busy === key} onClick={() => void run(key, () => call('set_default', { provider: v.provider, voice_id: v.id, model: v.model }))}>Set default</Button>
                    ) : null}
                    {v.deletable ? (
                      confirm === key ? (
                        <>
                          <Button type="button" variant="danger" size="sm" disabled={busy === key} onClick={() => void run(key, () => call('delete', { id: v.id, source: v.provider === 'local' ? 'local' : v.provider }))}>Delete {v.name}</Button>
                          <Button type="button" size="sm" onClick={() => setConfirm(null)}>Keep</Button>
                        </>
                      ) : (
                        <Button type="button" variant="danger" size="sm" aria-label={`Delete ${v.name}`} onClick={() => setConfirm(key)}>Delete</Button>
                      )
                    ) : null}
                  </div>
                  {previewKey === key && src ? (
                    <div className="v-preview"><MediaPlayer kind="audio" src={src} title={`Preview ${v.name}`} autoPlay compact /></div>
                  ) : null}
                </div>
              );
            })}
          </Card>
        </div>
      ))}

      <div className="v-group">
        <div className="model-group-title">Import from VoiceRight</div>
        <div className="v-import">
          <p className="lede">Copies your VoiceRight voices into {hostAppName()}. VoiceRight itself is left untouched.{!hasLocal ? ' None imported yet.' : ''}</p>
          <div className="v-inline">
            <Input aria-label="VoiceRight voices folder (optional)" placeholder="Folder (optional, found automatically)" value={importPath} onChange={(e) => setImportPath(e.target.value)} />
            <Button type="button" variant="primary" disabled={busy === 'import'} onClick={() => void run('import', async () => setImported(await call<ImportResult>('import_voiceright', importPath.trim() ? { source_dir: importPath.trim() } : {})))}>
              {busy === 'import' ? 'Importing…' : 'Import voices'}
            </Button>
          </div>
          {imported ? <Notice tone="warn">{importSummary(imported)}</Notice> : null}
        </div>
      </div>
    </>
  );
}

// ---------------------------------------------------------------- batch

function Batch({ voices, presets, models, configured, engine, setProblem }: { voices: VoiceEntry[]; presets: VoicePresets | null; models: Array<{ provider: string; model: string; kind: string; label: string }>; configured: Set<string>; engine: VoiceModelsStatus | null; setProblem: (p: Problem | null) => void }) {
  const confirm = useConfirm();
  const [text, setText] = useState('');
  const [voiceKey, setVoiceKey] = useState('');
  const [outDir, setOutDir] = useState('');
  const [estimate, setEstimate] = useState<number | null>(null);
  const [status, setStatus] = useState<BatchStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [playLine, setPlayLine] = useState<number | null>(null);
  const [language, setLanguage] = useState('en');
  const [instruct, setInstruct] = useState('');
  const [temperature, setTemperature] = useState('');
  const [topP, setTopP] = useState('');
  const [topK, setTopK] = useState('');
  const [exFormat, setExFormat] = useState<'wav' | 'flac' | 'm4a'>('wav');
  const [exNormalize, setExNormalize] = useState(false);
  const [exZip, setExZip] = useState(true);
  const [exported, setExported] = useState<string | null>(null);

  const voiceModels = useMemo(() => models.filter((m) => m.kind === 'voice'), [models]);
  const options = useMemo(() => [...voices.filter(canBatch), ...(presets?.speakers ?? []).map((speaker): VoiceEntry => ({
    id: `preset-${speaker}`, name: speaker.replaceAll('_', ' '), provider: 'local', source: 'builtin', category: 'preset', preset: speaker, default: false, deletable: false
  }))], [voices, presets]);
  const keyOf = (v: VoiceEntry) => `${v.provider}/${v.model ?? ''}/${v.id}`;
  const voice = options.find((v) => keyOf(v) === voiceKey) ?? options.find((v) => v.default) ?? options[0];
  const model = voice ? modelFor(voice, voiceModels) : null;
  const lines = splitLines(text);
  const engineReady = voice?.preset ? engine?.modes?.custom_voice.ready === true : engine?.ready !== false;
  const keyReady = voice ? (voice.provider === 'local' ? engineReady : configured.has(voice.provider) || !isTauri()) : false;
  let blocker = batchBlocker({ lines: lines.length, voice, model, keyReady });
  if (voice?.provider === 'local' && !engineReady) blocker = voice.preset ? 'CustomVoice not installed. Add local 1.7B model files & refresh status.' : 'Install the on-device voice engine first (see the card above).';
  const engineOf = (v: VoiceEntry | undefined) => (v?.provider === 'local' ? 'local-qts' : v?.provider);
  const modelLabel = voiceModels.find((m) => m.provider === engineOf(voice) && m.model === model)?.label;

  useEffect(() => { setEstimate(null); }, [text, voiceKey, model, instruct]);

  const batchId = status?.batch_id;
  const finished = status?.finished ?? true;
  useEffect(() => {
    if (!batchId || finished) return;
    const t = window.setInterval(() => {
      void call<BatchStatus>('batch_status', { batch_id: batchId }).then(setStatus).catch((e) => setProblem(toProblem(e)));
    }, 1500);
    return () => window.clearInterval(t);
  }, [batchId, finished, setProblem]);

  const input = () => {
    const base: Record<string, unknown> = { lines, provider: voice?.provider, model, voice_id: voice?.preset ? undefined : voice?.id, preset: voice?.preset, instruct: voice?.preset ? instruct.trim() || undefined : undefined, output_dir: outDir.trim() || undefined };
    // QTS voice controls: only the params the engine actually honours.
    if (voice?.provider === 'local') {
      const params: Record<string, unknown> = { language };
      if (temperature.trim()) params.temperature = Number(temperature);
      if (topP.trim()) params.top_p = Number(topP);
      if (topK.trim()) params.top_k = Number(topK);
      base.params = params;
    }
    return base;
  };

  async function doExport() {
    if (!status?.batch_id) return;
    setBusy(true);
    setProblem(null);
    setExported(null);
    try {
      const r = await call<{ zip?: string | null; dir?: string }>('export', { batch_id: status.batch_id, format: exFormat, normalize: exNormalize, zip: exZip });
      setExported(r.zip ?? r.dir ?? 'Exported');
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(false);
    }
  }

  /** Resolve a held line exactly as the Jobs view does, then let the batch continue. */
  async function resolveLine(l: BatchLine, action: HeldAction) {
    if (!l.job_id || !status?.batch_id) return;
    if (!(await confirm({ title: heldActionLabel(action), paragraphs: [heldConsequence(action)], confirmLabel: heldActionLabel(action) }))) return;
    setBusy(true);
    setProblem(null);
    try {
      await resolveJob(l.job_id, action);
      setStatus(await call<BatchStatus>('batch_status', { batch_id: status.batch_id, resume: true }));
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(false);
    }
  }

  async function doEstimate() {
    setBusy(true);
    setProblem(null);
    try {
      setEstimate((await call<{ est_usd: number }>('batch', { ...input(), dry_run: true })).est_usd);
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(false);
    }
  }

  async function start() {
    setBusy(true);
    setProblem(null);
    try {
      setStatus(await call<BatchStatus>('batch', input()));
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="v-batch">
      <p className="lede v-lede">One audio file per line. Blank lines are skipped. Each line is its own job, so a failed line never stops the rest.</p>
      <Card className="form v-form">
        <div>
          <label htmlFor="v-batch-text">Script, one line per file</label>
          <textarea className="rk-input" id="v-batch-text" rows={7} value={text} onChange={(e) => setText(e.target.value)} placeholder={'Welcome to the channel.\nHere is what we cover today.\nLet’s get started.'} />
          <small className="v-count">{lines.length} {lines.length === 1 ? 'file' : 'files'}{lines.length > MAX_BATCH_LINES ? ` (limit ${MAX_BATCH_LINES})` : ''}</small>
        </div>
        <div className="v-two">
          <div>
            <label>Voice</label>
            <Select label="Voice" value={voice ? keyOf(voice) : ''} onChange={setVoiceKey} options={options.map((v) => ({ value: keyOf(v), disabled: Boolean(v.preset) && engine?.modes?.custom_voice.ready !== true, label: `${v.name} (${providerName(v.provider)}${v.preset ? ' · preset' : ''}${v.model ? ` · ${v.model}` : ''})${v.default ? ' ★' : ''}` }))} />
          </div>
          <div>
            <label htmlFor="v-batch-out">Output folder</label>
            <Input id="v-batch-out" value={outDir} onChange={(e) => setOutDir(e.target.value)} placeholder={`Default: inside the ${hostAppName()} data folder`} />
          </div>
        </div>
        {voice?.provider === 'local' ? (
          <div className="v-two">
            <div>
              <label>Language</label>
              <Select label="Language" value={language} onChange={setLanguage} options={[['en', 'English'], ['zh', 'Chinese'], ['ja', 'Japanese'], ['ko', 'Korean'], ['de', 'German'], ['fr', 'French'], ['es', 'Spanish'], ['it', 'Italian'], ['pt', 'Portuguese'], ['ru', 'Russian']].map(([value, label]) => ({ value, label }))} />
            </div>
            <div>
              <label htmlFor="v-batch-temp">Sampling (temperature · top-p · top-k)</label>
              <div className="v-inline">
                <Input id="v-batch-temp" inputMode="decimal" value={temperature} onChange={(e) => setTemperature(e.target.value)} placeholder="0.9" aria-label="Temperature" />
                <Input inputMode="decimal" value={topP} onChange={(e) => setTopP(e.target.value)} placeholder="1.0" aria-label="Top-p" />
                <Input inputMode="decimal" value={topK} onChange={(e) => setTopK(e.target.value)} placeholder="50" aria-label="Top-k" />
              </div>
            </div>
          </div>
        ) : null}
        {voice?.preset ? <div><label htmlFor="v-batch-instruct">Delivery (optional)</label><Input id="v-batch-instruct" value={instruct} onChange={(e) => setInstruct(e.target.value)} placeholder="Speak warmly & slowly" /></div> : null}
        {voice?.provider === 'local' ? <small className="v-count">On-device engine supports language and sampling controls. Speed and pitch controls are not part of this checkpoint.</small> : null}
        {modelLabel ? <small className="v-count">Model: {modelLabel}</small> : null}
        <div className="v-inline">
          <Button type="button" disabled={busy || Boolean(blocker)} onClick={() => void doEstimate()}>Estimate cost</Button>
          <Button type="button" variant="primary" disabled={busy || Boolean(blocker)} onClick={() => void start()}>Generate {lines.length || ''} {lines.length === 1 ? 'file' : 'files'}</Button>
          {estimate !== null ? <span className="v-est">Estimated {usd(estimate)}</span> : null}
        </div>
        {blocker && text ? <small className="v-count">{blocker}</small> : null}
      </Card>

      {status ? (
        <div className="v-group" aria-live="polite">
          <div className="model-group-title">
            {status.finished ? (status.held ? 'Waiting for you' : 'Finished') : 'Running'}: {status.completed} of {status.total} done{status.failed ? `, ${status.failed} failed` : ''}{status.held ? `, ${status.held} on hold` : ''}
          </div>
          <ProgressBar value={status.completed + status.failed} max={Math.max(1, status.total)} label="Batch progress" />
          <Card flush className="row-list">
            {status.lines.map((l) => {
              const held = isHeldJob(l.status);
              const recovered = held && isRecoveredCode(l.error?.code);
              return (
              <div className="v-row" key={l.n}>
                <div className="v-main">
                  <b>{l.text}</b>
                  {held ? (
                    <small role="note">{heldExplanation(status.provider === 'local-qts' ? '' : providerName(status.provider), recovered)}</small>
                  ) : (
                    <small>{l.file ?? (l.error ? explainError(l.error.code ?? null, l.error.message ?? '').text : `line ${l.n}`)}</small>
                  )}
                </div>
                <div className="v-chips"><Badge tone={badgeTone(lineTone(l.status))}>{held ? heldLabel(recovered) : lineLabel(l.status)}</Badge></div>
                {held && l.job_id ? (
                  <div className="v-actions">
                    {heldActions(recovered).map((a) => (
                      <Button key={a} type="button" size="sm" variant={a === 'abandon' ? 'danger' : 'secondary'} disabled={busy} onClick={() => void resolveLine(l, a)}>{heldActionLabel(a)}</Button>
                    ))}
                  </div>
                ) : null}
                {l.file ? (
                  <div className="v-actions">
                    <Button type="button" size="sm" onClick={() => setPlayLine(playLine === l.n ? null : l.n)}>{playLine === l.n ? 'Stop' : 'Play'}</Button>
                  </div>
                ) : null}
                {playLine === l.n && l.file ? <div className="v-preview"><MediaPlayer kind="audio" src={assetUrl(l.file)} title={`Line ${l.n}`} autoPlay compact /></div> : null}
              </div>
              );
            })}
          </Card>
          <p className="lede v-lede">Saved to <span className="selectable">{status.output_dir}</span></p>
          {status.completed > 0 ? (
            <div className="v-inline">
              <label className="v-count">Export</label>
              <Select label="Export format" value={exFormat} onChange={(v) => setExFormat(v as 'wav' | 'flac' | 'm4a')} options={[{ value: 'wav', label: 'WAV' }, { value: 'flac', label: 'FLAC' }, { value: 'm4a', label: 'M4A (AAC)' }]} />
              <Checkbox checked={exNormalize} onChange={setExNormalize} label="Normalise loudness (EBU R128)" />
              <Checkbox checked={exZip} onChange={setExZip} label="Zip with manifest" />
              <Button type="button" disabled={busy} onClick={() => void doExport()}>Export</Button>
              {exported ? <small className="v-count selectable">{exported}</small> : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- clone

function Clone({ configured, refresh, engine, setProblem }: { configured: Set<string>; refresh: () => Promise<void>; engine: VoiceModelsStatus | null; setProblem: (p: Problem | null) => void }) {
  const [tier, setTier] = useState<Tier | null>(null);
  const [tierNote, setTierNote] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const keyReady = configured.has('elevenlabs') || !isTauri();

  useEffect(() => {
    if (!keyReady) return;
    let cancelled = false;
    call<Tier>('tier')
      .then((t) => { if (!cancelled) setTier(t); })
      .catch((e) => { if (!cancelled) setTierNote(explainError(errorCode(e), errorMessage(e)).text); });
    return () => { cancelled = true; };
  }, [keyReady]);

  const canClone = tier ? tier.can_use_instant_voice_cloning : null;
  const blocker = cloneBlocker({ name, path, consent, canClone });

  async function submit() {
    setBusy(true);
    setProblem(null);
    setDone(null);
    try {
      const r = await call<{ name: string }>('clone_elevenlabs', { name: name.trim(), audio_path: path.trim(), consent });
      setDone(`Cloned “${r.name}”. It is in your ElevenLabs voices now.`);
      setName('');
      setPath('');
      setConsent(false);
      await refresh();
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="v-clone">
      <div className="v-group">
        <div className="model-group-title">ElevenLabs instant clone</div>
        {!keyReady ? <Notice tone="warn">Add an ElevenLabs key in Settings to clone a voice.</Notice> : null}
        {tier ? (
          <Notice tone="warn">
            Plan: {tier.tier ?? 'unknown'}. {tier.can_use_instant_voice_cloning ? 'Instant cloning is included.' : 'Instant cloning is not included on this plan.'}
          </Notice>
        ) : tierNote ? <Notice tone="warn">{tierNote}</Notice> : null}
        <Card className="form v-form">
          <div className="v-two">
            <div>
              <label htmlFor="v-clone-name">Voice name</label>
              <Input id="v-clone-name" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Adrian, narration" />
            </div>
            <div>
              <label htmlFor="v-clone-path">Reference recording</label>
              <Input id="v-clone-path" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/path/to/clean-reference.wav" />
            </div>
          </div>
          <Checkbox checked={consent} onChange={setConsent} label={<>I own this voice or have the speaker&rsquo;s explicit consent to clone it.</>} />
          <small className="v-count">{CLONE_COST_NOTICE}</small>
          <div className="v-inline">
            <Button type="button" variant="primary" disabled={busy || !keyReady || Boolean(blocker)} onClick={() => void submit()}>{busy ? 'Cloning…' : 'Clone voice'}</Button>
            {blocker && (name || path || consent) ? <small className="v-count">{blocker}</small> : null}
          </div>
          {done ? <Notice tone="warn">{done}</Notice> : null}
        </Card>
      </div>
      <LocalClone engine={engine} refresh={refresh} setProblem={setProblem} />
    </div>
  );
}

function LocalClone({ engine, refresh, setProblem }: { engine: VoiceModelsStatus | null; refresh: () => Promise<void>; setProblem: (p: Problem | null) => void }) {
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const ready = engine?.ready === true;

  const submit = async () => {
    setBusy(true);
    setProblem(null);
    setDone(null);
    try {
      const r = await call<{ id: string; name: string }>('clone_local', { name: name.trim(), audio_path: path.trim(), consent });
      setDone(`Cloned “${r.name}” as a local voice (id ${r.id}). Use Verify on the Voices tab to check it.`);
      setName('');
      setPath('');
      setConsent(false);
      await refresh();
    } catch (e) {
      setProblem(toProblem(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="v-group">
      <div className="model-group-title">On this computer (free, private)</div>
      {!ready ? (
        <Notice tone="warn">Install the on-device voice engine (card on the Voices tab) to clone a voice locally.</Notice>
      ) : null}
      <Card className="form v-form">
        <div className="v-two">
          <div>
            <label htmlFor="v-lclone-name">Voice name</label>
            <Input id="v-lclone-name" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Narrator" />
          </div>
          <div>
            <label htmlFor="v-lclone-path">Reference recording</label>
            <Input id="v-lclone-path" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/path/to/clean-reference.wav (3–30 s)" />
          </div>
        </div>
        <Checkbox checked={consent} onChange={setConsent} label={<>I own this voice or have the speaker&rsquo;s explicit consent to clone it.</>} />
        <div className="v-inline">
          <Button type="button" variant="primary" disabled={busy || !ready || !name.trim() || !path.trim() || !consent} onClick={() => void submit()}>{busy ? 'Cloning…' : 'Clone locally'}</Button>
          <small className="v-count">Runs entirely on this Mac; nothing is uploaded.</small>
        </div>
        {done ? <Notice tone="warn">{done}</Notice> : null}
      </Card>
    </div>
  );
}
