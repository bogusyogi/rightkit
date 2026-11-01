import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { assetUrl, errorCode, errorMessage, errorText, fetchJobResult, fetchJobStatus, isSuccessJob, isTauri, isTerminalJob, studio, submitGeneration, type GenerateRequest, type JobAsset } from '../lib/api.js';
import { useApp } from '../lib/app-state.js';
import { stageLabel } from '../lib/gen-params.js';
import { IconLock, IconPlay } from '../components/icons.js';
import { ErrorNote } from '../components/ErrorNote.js';
import { FAL_UPLOAD_CONFIRM_FIELD, falUploadCount, falUploadNames, needsFalUploadConfirmation, useFalUploadConfirm } from '../components/FalUploadConfirm.js';
import { Button, Card, EmptyState, Input, Notice, SegmentedControl } from '@rightkit/react-shell/react';
import { Kv, usd } from '../components/ui.js';
import {
  addRoot, canRun, compactSource, estimateText, removeRoot, sourceSummary,
  type KdpDraft, type KdpRoot, type KdpSettings, type KdpSource, type KdpSources, type KdpStyles
} from './kdp-model.js';
import { hostAppName } from "../appName.js";

/** Engine in the desktop app; a local stand-in in browser QA (the shared mock has no studio.* commands). */
function kdp<T>(name: string, input: Record<string, unknown> = {}): Promise<T> {
  if (isTauri()) return studio<T>(name, input);
  if (import.meta.env?.DEV) return import('./kdp-mock.js').then((m) => m.mockKdp<T>(name, input, (request) => submitGeneration(request as unknown as GenerateRequest)));
  return Promise.reject(new Error(`${hostAppName()} needs the desktop app.`));
}

interface RunState {
  jobId: string;
  status: string;
  stage: string | null;
  message: string | null;
  error: { code: string | null; message: string } | null;
  asset: JobAsset | null;
}

const thumbOf = (s: KdpSource) => s.thumb ?? assetUrl(s.path);

export function Kdp() {
  const app = useApp();
  const { refreshSpend } = app;
  const [meta, setMeta] = useState<KdpStyles | null>(null);
  const [sources, setSources] = useState<KdpSources | null>(null);
  const [settings, setSettings] = useState<KdpSettings | null>(null);
  const [style, setStyle] = useState('colored_pencil');
  const [selected, setSelected] = useState('');
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<{ code: string | null; message: string } | null>(null);
  const [run, setRun] = useState<RunState | null>(null);
  const [showRoots, setShowRoots] = useState(false);
  const [newRoot, setNewRoot] = useState('');
  const falUpload = useFalUploadConfirm();
  const runRef = useRef<RunState | null>(null);
  runRef.current = run;

  const loadSources = useCallback(async () => {
    const next = await kdp<KdpSources>('studio.kdp.sources');
    setSources(next);
    setSelected((cur) => (next.references.some((r) => r.path === cur) ? cur : next.references[0]?.path ?? ''));
  }, []);

  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const [m, s] = await Promise.all([kdp<KdpStyles>('studio.kdp.styles'), kdp<KdpSettings>('studio.kdp.settings')]);
        if (!live) return;
        setMeta(m);
        setSettings(s);
        setStyle(m.default_style);
        await loadSources();
      } catch (e) {
        if (live) setLoadError(errorMessage(e));
      } finally {
        if (live) setLoading(false);
      }
    })();
    return () => { live = false; };
  }, [loadSources]);

  const active = run !== null && !isTerminalJob(run.status);
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(async () => {
      const cur = runRef.current;
      if (!cur) return;
      try {
        const job = await fetchJobStatus(cur.jobId);
        let asset: JobAsset | null = null;
        if (isSuccessJob(job.status)) asset = (await fetchJobResult(cur.jobId)).assets[0] ?? null;
        setRun({
          ...cur,
          status: job.status,
          stage: job.progress?.stage ?? cur.stage,
          message: job.progress?.message ?? null,
          error: job.error_message || job.error ? { code: job.error_code ?? null, message: job.error_message ?? String(job.error) } : null,
          asset
        });
        if (isTerminalJob(job.status)) void refreshSpend();
      } catch (e) {
        setRun({ ...cur, status: 'failed', error: { code: errorCode(e), message: errorText(e) } });
      }
    }, 2000);
    return () => window.clearInterval(timer);
  }, [active, refreshSpend]);

  const source = useMemo(() => sources?.references.find((r) => r.path === selected) ?? null, [sources, selected]);
  const styleLabel = meta?.styles.find((s) => s.id === style)?.label ?? '';
  const est = meta?.estimate;

  const colorize = async () => {
    if (!source) return;
    setSubmitting(true);
    setError(null);
    setRun(null);
    try {
      // The page is a local file and NanoBanana reads only URLs, so the engine uploads it to the
      // person's own fal storage. It refuses until this run carries an explicit approval.
      const requestId = crypto.randomUUID();
      const send = (approved: boolean) =>
        kdp<KdpDraft>('studio.kdp.run', {
          source_path: source.path,
          style,
          project_slug: source.project_slug,
          request_id: requestId,
          ...(approved ? { [FAL_UPLOAD_CONFIRM_FIELD]: true } : {})
        });
      let draft: KdpDraft;
      try {
        draft = await send(false);
      } catch (e) {
        if (!needsFalUploadConfirmation(e)) throw e;
        if (!(await falUpload.confirm(falUploadNames(e, [compactSource(source.path)]), falUploadCount(e) ?? undefined))) return;
        draft = await send(true);
      }
      const job = draft.job;
      if (!job) throw new Error('The engine did not return a job.');
      setRun({ jobId: job.job_id, status: job.status || 'submitted', stage: 'submitted', message: null, error: null, asset: null });
      void refreshSpend();
    } catch (e) {
      setError({ code: errorCode(e), message: errorText(e) });
    } finally {
      setSubmitting(false);
    }
  };

  const saveRoots = async (roots: KdpRoot[] | null) => {
    try {
      setSettings(await kdp<KdpSettings>('studio.kdp.settings_set', { roots: roots?.map(({ path, kind }) => ({ path, kind })) ?? null }));
      await loadSources();
    } catch (e) {
      setError({ code: errorCode(e), message: errorText(e) });
    }
  };

  const failed = run?.status === 'failed' || run?.status === 'cancelled';
  const outputUrl = run?.asset ? assetUrl(run.asset.path) : null;
  const previewUrl = outputUrl ?? (source ? thumbOf(source) : null);
  const runTitle = run ? (active ? stageLabel(run.stage, run.message) : failed ? 'Failed' : 'Done') : submitting ? 'Submitting' : 'Ready';

  return (
    <div className="content view kdp">
      <section className="work">
        <div className="eyebrow">KDP workflow</div>
        <h1>{meta?.title ?? 'Color thumbnail'}</h1>
        <p className="lede">Turn one line-art page into a colored cover thumbnail. Everything except the coloring material is locked.</p>
        <div className="locks">
          {meta ? (
            <>
              <span className="lock"><IconLock />Model <b>{meta.locks.model_label}</b></span>
              <span className="lock"><IconLock />Aspect <b>{meta.locks.aspect}</b></span>
              <span className="lock"><IconLock />Resolution <b>{meta.locks.resolution}</b></span>
              <span className="lock"><IconLock />Prompt <b>{meta.prompt_label}</b></span>
            </>
          ) : null}
        </div>

        {loadError ? <ErrorNote message={loadError} /> : null}

        <div className="step done">
          <div className="num">1</div>
          <div className="step-body">
            <h3>Coloring material</h3>
            <p className="hint">The only creative choice in this workflow.</p>
            <SegmentedControl label="Coloring material" value={style} onChange={setStyle} options={(meta?.styles ?? []).map((s) => ({ value: s.id, label: s.label }))} />
          </div>
        </div>

        <div className={`step${source ? ' done' : ''}`}>
          <div className="num">2</div>
          <div className="step-body">
            <h3>Source page</h3>
            <p className="hint">
              {sources ? sourceSummary(sources.references.length) : 'Looking for KDP images'} · newest first ·{' '}
              <Button type="button" variant="ghost" size="sm" onClick={() => setShowRoots((v) => !v)}>{showRoots ? 'Hide folders' : 'Source folders'}</Button>
            </p>

            {showRoots && settings ? (
              <Card flush className="group-box kdp-roots">
                {settings.roots.map((r) => (
                  <div className="setting" key={r.path}>
                    <div className="kdp-root">
                      <span className="mono">{r.path}</span>
                      <span className="hint-inline">{r.kind === 'codex' ? 'Codex output (book-* only)' : 'KDP folder'}{r.exists === false ? ' · not found' : ''}</span>
                    </div>
                    <Button type="button" variant="ghost" size="sm" onClick={() => void saveRoots(removeRoot(settings.roots, r.path))}>Remove</Button>
                  </div>
                ))}
                <div className="setting">
                  <Input className="kdp-input" value={newRoot} placeholder="/path/to/KDP" aria-label="Add a source folder" onChange={(e) => setNewRoot(e.target.value)} />
                  <span className="kdp-actions">
                    <Button type="button" variant="secondary" disabled={!newRoot.trim()} onClick={() => { void saveRoots(addRoot(settings.roots, newRoot, 'kdp')); setNewRoot(''); }}>Add</Button>
                    {settings.customized ? <Button type="button" variant="secondary" onClick={() => void saveRoots(null)}>Reset</Button> : null}
                  </span>
                </div>
              </Card>
            ) : null}

            {loading ? (
              <EmptyState title="Loading KDP images" />
            ) : sources && sources.references.length === 0 ? (
              <Notice tone="info">No KDP line-art images found in {sources.roots.length ? sources.roots.join(', ') : 'any source folder'}. Add a folder under Source folders.</Notice>
            ) : (
              <div className="grid">
                {sources?.references.map((r) => (
                  <div key={r.path} className="tile" title={r.path}>
                    <Card flush selected={r.path === selected} onClick={() => setSelected(r.path)} label={r.basename}>
                      <span className="img"><img src={thumbOf(r)} alt="" loading="lazy" /></span>
                      <span className="meta"><b>{r.basename}</b><span>{r.project_slug}</span></span>
                    </Card>
                  </div>
                ))}
              </div>
            )}
            {sources?.diagnostics.map((d) => <Notice key={d.message} tone="info" className="kdp-diag">{d.message}</Notice>)}
          </div>
        </div>
      </section>

      <aside className="run">
        <div className="run-head"><div className="eyebrow">Run</div><h3 style={{ margin: '2px 0 0', fontSize: 15 }}>{runTitle}</h3></div>
        <div className="run-body">
          <div className="preview">
            {previewUrl ? <img className={outputUrl ? '' : 'kdp-src'} src={previewUrl} alt={outputUrl ? 'Colored thumbnail' : 'Selected source'} /> : <>Pick a source page<br />to preview it here</>}
          </div>
          {error ? <ErrorNote code={error.code} message={error.message} /> : null}
          {run?.error && failed ? <ErrorNote code={run.error.code} message={run.error.message} /> : null}
          <Kv rows={[
            ['Style', styleLabel || '—'],
            ['Source', source ? compactSource(source.path) : 'None selected'],
            ['Output', '2K · 1:1 · saved to Library']
          ]} />
          <Card className="cost">
            <div><small>Estimated cost</small><b>{est ? usd(est.usd) : '—'}</b></div>
            <small>{estimateText(est?.usd, est?.credits)}</small>
          </Card>
          {outputUrl ? <div className="kdp-open"><a href={outputUrl} target="_blank" rel="noreferrer">Open full size</a></div> : null}
        </div>
        <div className="run-foot">
          <Button type="button" variant="primary" disabled={!canRun(selected, submitting || active) || !meta} onClick={() => void colorize()}>
            <IconPlay />{submitting || active ? 'Colorizing' : 'Colorize thumbnail'}
          </Button>
          <div className="foot-note">NB2 · 2K · 1:1</div>
        </div>
      </aside>
    </div>
  );
}
