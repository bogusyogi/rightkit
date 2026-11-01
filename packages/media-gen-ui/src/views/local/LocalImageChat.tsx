import { useCallback, useEffect, useMemo, useRef, useState, type MutableRefObject } from 'react';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { assetUrl, errorMessage, importLocalImageReference, isTauri, setLocalImageIdleUnload, setLocalImageMemoryMode, submitLocalImage } from '../../lib/api';
import type { LocalImageArtifact, LocalImageJob, LocalImageMemoryStatus, LocalImageMessage, LocalImageProfile, LocalImageReference } from '../../lib/types';
import { ErrorNote } from '../../components/ErrorNote';
import { Card, Badge, Button, IconButton, Input, Notice, Pill, ProgressBar, Select } from '@rightkit/react-shell/react';
import type { LocalImageLive } from './useLocalImageLive';
import {
  AUTO_SCHEDULER, RESOLUTIONS, SAMPLERS, SCHEDULERS, buildSubmitRequest, conversationActivity, conversationLabel, defaultSettings,
  fileName, formatMemoryMb, isActiveJob, isRetryable, jobStatusLabel, orderConversations, runtimeLabel, runtimeNeedsWarning, type SettingsForm
} from './logic';
import './local.css';

type Prep = { state: 'pending' | 'ready' | 'failed'; reference?: LocalImageReference; error?: string };

export interface LocalChatHandle {
  submit: () => void;
}


function memoryModeLabel(mode?: string): string {
  return mode === 'performance' ? 'fastest' : mode === 'low' ? 'lowest memory' : 'balanced';
}

/** One sentence per mode. Peaks are estimates until measured on real runs. */
function memoryModeHelp(mode: string | undefined, memory: LocalImageMemoryStatus): string {
  const peak = (m: string) => {
    const gb = memory.estimated_peaks_gb?.[m];
    return gb == null ? '' : ` (about ${gb} GB, estimated)`;
  };
  if (mode === 'performance') return `Keeps every model in memory${peak('performance')}: fastest, but needs a computer with plenty of memory.`;
  if (mode === 'balanced') return `Reads the text model from disk for each image${peak('balanced')}: a little slower; decoding in tiles can leave faint seams.`;
  if (mode === 'low') return `Streams all models from disk${peak('low')}: slowest, for computers with 16 GB or less; decoding in tiles can leave faint seams.`;
  return '';
}

export function LocalImageChat({ profile, live, prompt, setPrompt, handleRef, onOpenSettings }: {
  profile: LocalImageProfile;
  live: LocalImageLive;
  prompt: string;
  setPrompt: (value: string) => void;
  handleRef: MutableRefObject<LocalChatHandle | null>;
  onOpenSettings: () => void;
}) {
  const { conversations, runtime, selectedId, setSelectedId, loading, error, setError } = live;
  const [settings, setSettings] = useState<SettingsForm>(() => defaultSettings(profile));
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [refPaths, setRefPaths] = useState<string[]>([]);
  const [preps, setPreps] = useState<Record<string, Prep>>({});
  const [pathInput, setPathInput] = useState('');
  const [replyTo, setReplyTo] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [idleInput, setIdleInput] = useState('');
  const [idleBusy, setIdleBusy] = useState(false);
  const [memoryBusy, setMemoryBusy] = useState(false);
  const refCache = useRef<Map<string, LocalImageReference>>(new Map());
  const msgsRef = useRef<HTMLDivElement | null>(null);
  const installed = profile.install_state === 'installed';

  useEffect(() => { setSettings(defaultSettings(profile)); }, [profile.id]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (!idleBusy) setIdleInput(String(runtime?.idle_unload_seconds ?? 300));
  }, [runtime?.idle_unload_seconds, idleBusy]);

  const saveIdleUnload = () => {
    const seconds = Math.max(0, Math.min(86400, Math.round(Number(idleInput) || 0)));
    setIdleInput(String(seconds));
    if (seconds === runtime?.idle_unload_seconds) return;
    setIdleBusy(true);
    setLocalImageIdleUnload(seconds)
      .catch((e) => setError(errorMessage(e)))
      .finally(() => { setIdleBusy(false); live.refreshRuntime(); });
  };

  const saveMemoryMode = (mode: string) => {
    setMemoryBusy(true);
    setLocalImageMemoryMode(mode)
      .catch((e) => setError(errorMessage(e)))
      .finally(() => { setMemoryBusy(false); live.refreshRuntime(); });
  };

  const patch = (p: Partial<SettingsForm>) => setSettings((s) => ({ ...s, ...p }));

  // A job only ever takes one reference; a new one replaces the old.
  const selectReference = useCallback((path: string) => {
    const p = path.trim();
    if (p) setRefPaths([p]);
  }, []);

  useEffect(() => {
    let cancelled = false;
    setPreps(Object.fromEntries(refPaths.map((p) => {
      const cached = refCache.current.get(p);
      return [p, cached ? { state: 'ready' as const, reference: cached } : { state: 'pending' as const }];
    })));
    for (const p of refPaths) {
      if (refCache.current.has(p)) continue;
      importLocalImageReference(p)
        .then(({ reference }) => {
          refCache.current.set(p, reference);
          if (!cancelled) setPreps((cur) => ({ ...cur, [p]: { state: 'ready', reference } }));
        })
        .catch((e) => { if (!cancelled) setPreps((cur) => ({ ...cur, [p]: { state: 'failed', error: errorMessage(e).replace(/^[a-z][a-z0-9_]*:\s*/, '') } })); });
    }
    return () => { cancelled = true; };
  }, [refPaths]);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let off: (() => void) | null = null;
    void getCurrentWebviewWindow()
      .onDragDropEvent((ev) => { if (ev.payload.type === 'drop' && ev.payload.paths.length > 0) selectReference(ev.payload.paths[0]); })
      .then((fn) => (cancelled ? fn() : (off = fn)))
      .catch(() => undefined);
    return () => { cancelled = true; off?.(); };
  }, [selectReference]);

  const selected = conversations.find((c) => c.conversation.id === selectedId) ?? null;
  const ordered = useMemo(() => orderConversations(conversations), [conversations]);
  const messages = useMemo(() => (selected ? [...selected.messages].sort((a, b) => a.created_at.localeCompare(b.created_at)) : []), [selected]);
  const readyRefs = refPaths.map((p) => preps[p]).filter((p): p is Prep & { reference: LocalImageReference } => p?.state === 'ready' && Boolean(p.reference));
  const refsReady = refPaths.every((p) => preps[p]?.state === 'ready');
  const aspectOptions = RESOLUTIONS.some((r) => r.width === profile.defaults.width && r.height === profile.defaults.height)
    ? RESOLUTIONS
    : [{ label: `${profile.defaults.width}×${profile.defaults.height}`, width: profile.defaults.width, height: profile.defaults.height }, ...RESOLUTIONS];

  useEffect(() => {
    const el = msgsRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages.length, selectedId, conversations]);

  const submit = useCallback(async () => {
    if (submitting) return;
    setError(null);
    if (!installed) return void setError(`${profile.label} is not installed yet. Install it from the panel on the right.`);
    if (!refsReady) return void setError('Wait for the reference image to finish preparing, or remove it.');
    const built = buildSubmitRequest({
      profile, prompt, settings, referenceIds: readyRefs.map((r) => r.reference.id),
      conversationId: selectedId, parentMessageId: replyTo, requestId: crypto.randomUUID()
    });
    if (!built.ok) return void setError(built.error);
    setSubmitting(true);
    try {
      const { job } = await submitLocalImage(built.request);
      setSelectedId(job.conversation_id);
      setPrompt('');
      setRefPaths([]);
      setReplyTo(null);
      void live.refreshRuntime();
      await live.refreshConversations();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSubmitting(false);
    }
  }, [submitting, installed, profile, prompt, settings, readyRefs, refsReady, selectedId, replyTo, setError, setSelectedId, setPrompt, live]);

  handleRef.current = { submit: () => void submit() };

  const newImage = () => {
    setSelectedId(null);
    setRefPaths([]);
    setReplyTo(null);
    setPrompt('');
    setError(null);
  };

  const useAsReference = (artifact: LocalImageArtifact, message: LocalImageMessage) => {
    selectReference(artifact.path);
    setReplyTo(message.id);
  };

  const reuseJob = (job: LocalImageJob) => {
    setPrompt(job.prompt);
    setSettings({
      width: job.width, height: job.height, seed: String(job.seed),
      steps: job.overrides.sample_steps != null ? String(job.overrides.sample_steps) : '',
      cfg: job.overrides.txt_cfg != null ? String(job.overrides.txt_cfg) : '',
      sampler: job.overrides.sample_method ?? '',
      scheduler: 'scheduler' in job.overrides ? (job.overrides.scheduler ?? AUTO_SCHEDULER) : '',
      negative: job.negative_prompt
    });
    setShowAdvanced(true);
  };

  const warn = runtime && runtimeNeedsWarning(runtime);

  return (
    <Card flush className="chat li-chat">
      <div className="convos">
        <h6>Conversations</h6>
        <Card flush className="convo li-new" onClick={newImage}><b>+ New image</b></Card>
        {ordered.length === 0 ? <small style={{ padding: '4px 8px', color: 'var(--ink-3)' }}>{loading ? 'Loading…' : 'No local image history yet'}</small> : null}
        {ordered.map((snap) => {
          const active = snap.jobs.filter((j) => isActiveJob(j) || j.state === 'paused').length;
          return (
            <Card flush key={snap.conversation.id} className="convo" selected={snap.conversation.id === selectedId} onClick={() => setSelectedId(snap.conversation.id)} label={conversationActivity(snap)}>
              <b>{conversationLabel(snap)}</b>
              <small>{snap.jobs.length} {snap.jobs.length === 1 ? 'job' : 'jobs'}{active ? ` · ${active} active` : ''}</small>
            </Card>
          );
        })}
      </div>
      <div className="thread">
        <div className="li-status">
          <span title={runtime ? `backend ${runtime.backend_binary}` : 'runtime status loading'}>
            {(() => {
              const text = (
                <>
                  Local engine · {runtimeLabel(runtime)}{runtime?.port ? ` · :${runtime.port}` : ''}
                  {runtime ? (runtime.loaded ? ` · loaded${runtime.memory_mb != null ? ` ${formatMemoryMb(runtime.memory_mb)}` : ''}` : ' · unloaded') : ''}
                </>
              );
              return runtime?.state === 'ready' ? <Badge tone="ok">{text}</Badge> : warn ? <Pill tone="warn">{text}</Pill> : <Pill>{text}</Pill>;
            })()}
          </span>
          {runtime ? (
            <label className="li-idle" title="Unload the local engine after this many idle seconds. 0 keeps it loaded.">
              unload
              <Input
                type="number" min={0} max={86400} step={30} value={idleInput}
                aria-label="Idle unload seconds"
                disabled={idleBusy}
                onChange={(e) => setIdleInput(e.target.value)}
                onBlur={saveIdleUnload}
                onKeyDown={(e) => { if (e.key === 'Enter') saveIdleUnload(); }}
              />
              s
            </label>
          ) : null}
          {runtime?.memory && !runtime.memory.error ? (
            <label className="li-idle" title={memoryModeHelp(runtime.memory.requested === 'auto' ? runtime.memory.resolved : runtime.memory.requested, runtime.memory)}>
              memory
              <Select
                value={runtime.memory.requested ?? 'performance'}
                label="Memory use"
                disabled={memoryBusy}
                onChange={saveMemoryMode}
                options={[
                  { value: 'auto', label: `Automatic (${memoryModeLabel(runtime.memory.resolved)} on this computer)` },
                  { value: 'performance', label: 'Fastest (most memory)' },
                  { value: 'balanced', label: 'Balanced' },
                  { value: 'low', label: 'Lowest memory (slowest)' },
                ]}
              />
            </label>
          ) : null}
          {runtime?.memory && !runtime.memory.error ? (
            <small>{memoryModeHelp(runtime.memory.requested === 'auto' ? runtime.memory.resolved : runtime.memory.requested, runtime.memory)}</small>
          ) : null}
          {runtime && runtime.state !== 'ready' ? (
            <small className={warn ? 'li-warn' : undefined}>
              {runtime.state === 'stopped' && !runtime.last_error ? 'Starts automatically on Generate.' : null}
              {runtime.state === 'starting' && !runtime.last_error ? 'Starting local engine…' : null}
              {warn ? `Runtime is ${runtime.state.replace(/_/g, ' ')}${runtime.last_error ? `: ${runtime.last_error}` : ''}` : null}
            </small>
          ) : null}
        </div>
        <div className="msgs" ref={msgsRef}>
          {loading && !selected ? <div className="hello">Loading local image history…</div> : null}
          {!loading && !selected ? <div className="hello">Describe an image and press Enter, or pick a conversation on the left.</div> : null}
          {messages.map((m) => (
            <div key={m.id} style={{ display: 'contents' }}>
              {m.role === 'user' && m.prompt ? (
                <div className="you">
                  {m.prompt}
                  {m.reference_ids.length ? <div className="li-refs-used">{m.reference_ids.map((id) => <span key={id} title={id}>reference</span>)}</div> : null}
                </div>
              ) : null}
              {m.job_id ? (
                <JobCard
                  job={selected?.jobs.find((j) => j.id === m.job_id) ?? null}
                  artifacts={(selected && m.job_id && selected.artifacts[m.job_id]) || []}
                  message={m}
                  onCancel={(j) => void live.cancel(j)}
                  onResume={(j) => void live.resume(j)}
                  onRetry={(j) => void live.retry(j)}
                  onUseAsReference={useAsReference}
                  onReuse={reuseJob}
                />
              ) : null}
            </div>
          ))}
        </div>
        <div className="composer">
          {error ? <ErrorNote message={error} onOpenKeys={onOpenSettings} /> : null}
          {!installed ? <Notice tone="info">{profile.label} is not installed. Use Install in the panel on the right; generation unlocks when the download finishes.</Notice> : null}
          {replyTo ? (
            <div className="li-chips"><span className="li-chip">Editing from a previous result <IconButton label="Clear edit source" onClick={() => setReplyTo(null)}>×</IconButton></span></div>
          ) : null}
          {refPaths.length ? (
            <div className="li-chips">
              {refPaths.map((p) => {
                const prep = preps[p];
                const ref = prep?.state === 'ready' ? prep.reference : undefined;
                return (
                  <span className="li-chip" key={p}>
                    {ref ? <img src={assetUrl(ref.stored_path)} alt="" /> : null}
                    <span>{fileName(p)}</span>
                    {prep?.state === 'pending' ? <em>Preparing…</em> : null}
                    {prep?.state === 'failed' ? <Notice tone="warn" className="li-warn">{prep.error || 'Could not add'}</Notice> : null}
                    <IconButton label="Remove reference" onClick={() => setRefPaths((cur) => cur.filter((x) => x !== p))}>×</IconButton>
                  </span>
                );
              })}
            </div>
          ) : null}
          <textarea className="rk-input"
            aria-label="Prompt"
            placeholder="Describe the image. Enter to generate, Shift+Enter for a new line."
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void submit(); } }}
          />
          <div className="comp-row">
            <Select
              className="li-select"
              label="Resolution"
              value={`${settings.width}x${settings.height}`}
              onChange={(v) => { const [width, height] = v.split('x').map(Number); patch({ width, height }); }}
              options={aspectOptions.map((o) => ({ value: `${o.width}x${o.height}`, label: o.label }))}
            />
            <Button type="button" variant="ghost" aria-expanded={showAdvanced} onClick={() => setShowAdvanced((v) => !v)}>{showAdvanced ? 'Hide settings' : 'Settings'}</Button>
            <Input
              className="li-path"
              aria-label="Reference image path"
              value={pathInput}
              onChange={(e) => setPathInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); selectReference(pathInput); setPathInput(''); } }}
              placeholder="Paste or drop an image path to edit from it"
            />
            <Button type="button" variant="ghost" title="Attach reference image" disabled={!pathInput.trim()} onClick={() => { selectReference(pathInput); setPathInput(''); }}>Attach</Button>
            <Button type="button" variant="primary" className="send" disabled={submitting || !installed || !prompt.trim() || !refsReady} onClick={() => void submit()}>{submitting ? 'Sending…' : 'Generate'}</Button>
          </div>
          {showAdvanced ? (
            <div className="li-adv" role="group" aria-label="Sampling settings">
              <label>Seed<Input inputMode="numeric" placeholder="random" value={settings.seed} onChange={(e) => patch({ seed: e.target.value })} /></label>
              <label>Steps<Input inputMode="numeric" placeholder={String(profile.defaults.sample_steps)} value={settings.steps} onChange={(e) => patch({ steps: e.target.value })} /></label>
              <label>CFG<Input inputMode="decimal" placeholder={String(profile.defaults.txt_cfg)} value={settings.cfg} onChange={(e) => patch({ cfg: e.target.value })} /></label>
              <label>Sampler
<Select
                  label="Sampler"
                  value={settings.sampler}
                  onChange={(v) => patch({ sampler: v })}
                  options={[{ value: '', label: `Profile default (${profile.defaults.sample_method})` }, ...SAMPLERS.filter((x) => x !== profile.defaults.sample_method).map((x) => ({ value: x, label: x }))]}
                />
              </label>
              <label>Scheduler
<Select
                  label="Scheduler"
                  value={settings.scheduler}
                  onChange={(v) => patch({ scheduler: v })}
                  options={[
                    { value: '', label: `Profile default (${profile.defaults.scheduler ?? 'auto'})` },
                    ...(profile.defaults.scheduler ? [{ value: AUTO_SCHEDULER, label: 'auto' }] : []),
                    ...SCHEDULERS.filter((x) => x !== profile.defaults.scheduler).map((x) => ({ value: x, label: x })),
                  ]}
                />
              </label>
              <label className="wide">Negative prompt<Input value={settings.negative} placeholder="Optional" onChange={(e) => patch({ negative: e.target.value })} /></label>
              <div className="wide"><Button type="button" variant="ghost" size="sm" className="link-btn" onClick={() => setSettings(defaultSettings(profile))}>Reset to profile defaults</Button></div>
            </div>
          ) : null}
        </div>
      </div>
    </Card>
  );
}

function JobCard({ job, artifacts, message, onCancel, onResume, onRetry, onUseAsReference, onReuse }: {
  job: LocalImageJob | null;
  artifacts: LocalImageArtifact[];
  message: LocalImageMessage;
  onCancel: (job: LocalImageJob) => void;
  onResume: (job: LocalImageJob) => void;
  onRetry: (job: LocalImageJob) => void;
  onUseAsReference: (artifact: LocalImageArtifact, message: LocalImageMessage) => void;
  onReuse: (job: LocalImageJob) => void;
}) {
  if (!job) return <Notice tone="info">Job {message.job_id} is not in this snapshot yet.</Notice>;
  const active = isActiveJob(job);
  const failed = job.state === 'failed' || job.state === 'interrupted';
  const paused = job.state === 'paused';
  const o = job.overrides ?? {};
  return (
    <div className={`job li-job${failed ? ' failed' : ''}`} data-state={job.state}>
      <div className="job-meta">
        <Badge tone={job.state === 'succeeded' ? 'ok' : failed ? 'bad' : active || paused ? 'accent' : 'neutral'}>{jobStatusLabel(job)}</Badge>
        <span>{job.width}×{job.height}</span>
        <span>seed {job.seed}</span>
        {o.sample_steps != null ? <span>{o.sample_steps} steps</span> : null}
        {o.txt_cfg != null ? <span>cfg {o.txt_cfg}</span> : null}
        {o.sample_method ? <span>{o.sample_method}</span> : null}
        {'scheduler' in o ? <span>{o.scheduler ?? 'auto'}</span> : null}
      </div>
      {active ? <ProgressBar value={Math.round((job.progress ?? 0.1) * 100)} max={100} label="Generation progress" /> : null}
      {failed ? <Notice tone="bad">{job.error_code ? <b className="mono">{job.error_code} </b> : null}{job.error_message || 'Generation stopped before a result was committed.'}</Notice> : null}
      {artifacts.length ? (
        <div className="outs">
          {artifacts.map((a) => (
            <div className="out" key={a.id}>
              <img src={assetUrl(a.path)} alt={job.prompt} />
              <div className="acts">
                <span className="media-info">{a.width} × {a.height}</span>
                <a href={assetUrl(a.path)} target="_blank" rel="noreferrer">Open</a>
                <Button type="button" variant="ghost" size="sm" onClick={() => onUseAsReference(a, message)}>Use as reference</Button>
              </div>
            </div>
          ))}
        </div>
      ) : null}
      <div className="li-actions">
        {paused ? <Button type="button" variant="primary" size="sm" onClick={() => onResume(job)}>Resume</Button> : null}
        {active || paused ? <Button type="button" variant="secondary" size="sm" disabled={job.state === 'cancelling'} onClick={() => onCancel(job)}>{job.state === 'cancelling' ? 'Cancelling…' : 'Cancel'}</Button> : null}
        {isRetryable(job) ? <Button type="button" variant="secondary" size="sm" onClick={() => onRetry(job)}>Retry</Button> : null}
        {!active && !paused ? <Button type="button" variant="ghost" size="sm" className="link-btn" onClick={() => onReuse(job)}>Reuse settings</Button> : null}
      </div>
    </div>
  );
}
