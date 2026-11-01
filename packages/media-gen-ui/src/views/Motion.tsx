import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { assetUrl, engineCommand, errorCode, errorMessage, isTauri, studio } from '../lib/api';
import { Badge, Button, Checkbox, EmptyState, Input, Notice, ProgressBar, Select } from '@rightkit/react-shell/react';
import { Kv } from '../components/ui';
import { isH264Encoder } from '../lib/motion-encoder';
import { MediaPlayer } from '@rightkit/media-ui';
import './motion.css';

interface Template { id: string; name: string; description: string; width: number; height: number; duration_s: number; fps: number }
interface Plan {
  project_slug: string; project_dir: string; entry_file: string; composition_id: string;
  width: number; height: number; duration_s: number; fps: number; quality: string; frame_count: number;
  clips: number; audio: unknown[]; warnings: string[];
}
interface MotionStatus {
  capture_supported: boolean; ffmpeg_ready: boolean; error: string | null; running: boolean; out_dir: string;
  encoder: { name: string; codec: string; h264: boolean } | null;
}
interface RenderResult {
  status: string; output_path: string; poster_path: string; manifest_path: string; frames: number;
  width: number; height: number; fps: number; duration_s: number; encoder: string; elapsed_ms: number;
}
interface Progress { phase: string; frame: number; total: number }

const QUALITIES = ['draft', 'standard', 'high'] as const;
const NEEDS_DESKTOP = 'Rendering needs the desktop app; this browser preview can only plan.';

const MOCK_TEMPLATES: Template[] = [
  { id: 'title-card', name: 'Title card', description: 'Kicker, headline and subtitle over a dark gradient. 16:9, 6 seconds.', width: 1920, height: 1080, duration_s: 6, fps: 30 },
  { id: 'vertical-hook', name: 'Vertical hook', description: 'Three punchy lines and a call to action. 9:16, 6 seconds.', width: 1080, height: 1920, duration_s: 6, fps: 30 },
  { id: 'stat-reveal', name: 'Stat reveal', description: 'Three counting statistics on light cards. 16:9, 8 seconds.', width: 1920, height: 1080, duration_s: 8, fps: 30 },
  { id: 'blank', name: 'Blank project', description: 'The empty HyperFrames starter (16:9, 10 seconds).', width: 1920, height: 1080, duration_s: 10, fps: 30 }
];

function call<T>(verb: string, input: Record<string, unknown> = {}): Promise<T> {
  if (!isTauri()) {
    if (verb === 'templates') return Promise.resolve({ templates: MOCK_TEMPLATES } as T);
    if (verb === 'status') return Promise.resolve({ projects_root: '', defaults: { fps: 30, quality: 'draft', entry_file: 'index.html' } } as T);
    return Promise.reject(Object.assign(new Error(NEEDS_DESKTOP), { code: 'desktop_only' }));
  }
  return studio<T>(`studio.motion.${verb}`, input);
}

function problemText(e: unknown): string {
  const code = errorCode(e);
  const msg = errorMessage(e);
  return code && !msg.startsWith(code) ? `${code}: ${msg}` : msg;
}

function phaseLabel(p: Progress | null): string {
  if (!p) return 'Starting';
  if (p.phase === 'capturing') return `Capturing frame ${p.frame} of ${p.total}`;
  if (p.phase === 'encoding') return 'Finishing the MP4';
  if (p.phase === 'loading') return 'Loading the composition';
  return p.phase;
}

export function Motion() {
  const [templates, setTemplates] = useState<Template[]>([]);
  const [projectsRoot, setProjectsRoot] = useState('');
  const [status, setStatus] = useState<MotionStatus | null>(null);
  const [templateId, setTemplateId] = useState('title-card');
  const [projectDir, setProjectDir] = useState('');
  const [entryFile, setEntryFile] = useState('index.html');
  const [fps, setFps] = useState(30);
  const [quality, setQuality] = useState<(typeof QUALITIES)[number]>('draft');
  const [plan, setPlan] = useState<Plan | null>(null);
  const [approved, setApproved] = useState(false);
  const [busy, setBusy] = useState<'scaffold' | 'plan' | 'render' | null>(null);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [result, setResult] = useState<RenderResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    void call<{ templates: Template[] }>('templates').then((r) => alive.current && setTemplates(r.templates)).catch(() => undefined);
    void call<{ projects_root: string }>('status').then((r) => alive.current && setProjectsRoot(r.projects_root)).catch(() => undefined);
    if (isTauri()) void engineCommand<MotionStatus>('motion.status').then((s) => alive.current && setStatus(s)).catch(() => undefined);
    let unlisten: (() => void) | undefined;
    if (isTauri()) void listen<Progress>('motion://progress', (e) => setProgress(e.payload)).then((u) => { unlisten = u; });
    return () => { alive.current = false; unlisten?.(); };
  }, []);

  const resetOutcome = () => { setPlan(null); setApproved(false); setResult(null); setError(null); setNotice(null); };

  const createProject = async () => {
    setBusy('scaffold'); resetOutcome();
    try {
      const made = await call<{ project_dir: string; entry_file: string; template_id: string }>('scaffold', {
        template_id: templateId, ...(projectDir.trim() ? { project_dir: projectDir.trim() } : {})
      });
      setProjectDir(made.project_dir); setEntryFile(made.entry_file);
      const tpl = templates.find((t) => t.id === made.template_id);
      if (tpl) setFps(tpl.fps);
      setNotice(`Project created at ${made.project_dir}. Edit index.html, then check the project.`);
    } catch (e) { setError(problemText(e)); } finally { setBusy(null); }
  };

  const checkProject = async () => {
    setBusy('plan'); resetOutcome();
    try {
      setPlan(await call<Plan>('plan', { project_dir: projectDir.trim(), entry_file: entryFile.trim() || 'index.html', fps, quality }));
    } catch (e) { setError(problemText(e)); } finally { setBusy(null); }
  };

  const render = async () => {
    if (!plan) return;
    setBusy('render'); setError(null); setResult(null); setProgress(null);
    try {
      if (!isTauri()) throw new Error(NEEDS_DESKTOP);
      const out = await engineCommand<RenderResult>('motion.render', {
        project_dir: plan.project_dir, entry_file: plan.entry_file, fps: plan.fps, quality: plan.quality,
        project_slug: plan.project_slug, width: plan.width, height: plan.height, operator_approved: approved
      });
      setResult(out);
    } catch (e) {
      const msg = typeof e === 'string' ? e : problemText(e);
      setError(msg === 'cancelled' ? 'Render cancelled.' : msg);
    } finally { setBusy(null); }
  };

  const cancel = () => { if (isTauri()) void engineCommand('motion.cancel'); };
  const selected = templates.find((t) => t.id === templateId);
  const rendering = busy === 'render';
  const pct = progress && progress.total ? Math.round((progress.frame / progress.total) * 100) : 0;
  const unsupported = status && !status.capture_supported;
  const blocked = status && !status.ffmpeg_ready;

  return (
    <div className="content view" style={{ gridTemplateColumns: '1fr' }}>
      <section className="work motion">
        <div className="img-head">
          <div><div className="eyebrow">Create</div><h1>Motion</h1></div>
          {status?.encoder ? <Badge tone={status.encoder.h264 ? 'ok' : 'warn'}>{status.encoder.codec}</Badge> : null}
        </div>
        <p className="lede mo-lede">Render a HyperFrames HTML composition to MP4. The frames are drawn by this app&apos;s own web view and encoded locally, so no browser or Node install is needed.</p>

        {unsupported ? <Notice tone="bad">Motion rendering is not available on this platform; macOS (WKWebView) and Windows (WebView2) builds support it.</Notice> : null}
        {blocked ? <Notice tone="bad">The bundled encoder is not available{status?.error ? `: ${status.error}` : ''}.</Notice> : null}

        <div className="mo-grid">
          <div className="mo-card">
            <h3>1. Project</h3>
            <div className="mo-f">
              <span>Start from</span>
              <Select label="Start from" value={templateId} onChange={setTemplateId} disabled={busy !== null} options={templates.map((t) => ({ value: t.id, label: t.name }))} />
            </div>
            {selected ? <p className="mo-hint">{selected.description}</p> : null}
            <label className="mo-f">Project folder
              <Input value={projectDir} onChange={(e) => { setProjectDir(e.target.value); resetOutcome(); }} placeholder={projectsRoot ? `${projectsRoot}/${templateId}` : '/path/to/hyperframes-project'} spellCheck={false} />
            </label>
            <div className="mo-row">
              <Button type="button" variant="secondary" disabled={busy !== null || templates.length === 0} onClick={() => void createProject()}>{busy === 'scaffold' ? 'Creating…' : 'Create from template'}</Button>
            </div>
            <div className="mo-row mo-3">
              <label className="mo-f">Entry file
                <Input value={entryFile} onChange={(e) => { setEntryFile(e.target.value); resetOutcome(); }} spellCheck={false} />
              </label>
              <label className="mo-f">FPS
                <Input type="number" min={1} max={120} value={fps} onChange={(e) => { setFps(Number(e.target.value) || 30); resetOutcome(); }} />
              </label>
              <div className="mo-f">
                <span>Quality</span>
                <Select label="Quality" value={quality} onChange={(v) => { setQuality(v); resetOutcome(); }} options={QUALITIES.map((q) => ({ value: q, label: q }))} />
              </div>
            </div>
            <div className="mo-row">
              <Button type="button" variant="primary" disabled={busy !== null || !projectDir.trim()} onClick={() => void checkProject()}>{busy === 'plan' ? 'Checking…' : 'Check project'}</Button>
            </div>
          </div>

          <div className="mo-card">
            <h3>2. Render</h3>
            {plan ? (
              <>
                <Kv rows={[
                  ['Composition', plan.composition_id],
                  ['Size', `${plan.width} × ${plan.height}`],
                  ['Length', `${plan.duration_s} s at ${plan.fps} fps (${plan.frame_count} frames)`],
                  ['Clips', String(plan.clips)],
                  ['Audio tracks', String(plan.audio.length)]
                ]} />
                {plan.warnings.map((w) => <Notice key={w} tone="warn">{w}</Notice>)}
                <Checkbox checked={approved} disabled={rendering} onChange={setApproved} label="Approve rendering this project on this computer" />
                <div className="mo-row">
                  <Button type="button" variant="primary" disabled={!approved || busy !== null || Boolean(unsupported) || Boolean(blocked)} onClick={() => void render()}>{rendering ? 'Rendering…' : 'Render MP4'}</Button>
                  {rendering ? <Button type="button" variant="danger" onClick={cancel}>Cancel</Button> : null}
                </div>
                {rendering ? (
                  <div className="mo-progress">
                    <ProgressBar value={pct} max={100} tone="accent" label="Render progress" />
                    <small>{phaseLabel(progress)}</small>
                  </div>
                ) : null}
              </>
            ) : <EmptyState title="Check the project first">Create one from a template or enter an existing HyperFrames folder, then check it.</EmptyState>}
          </div>
        </div>

        {notice ? <Notice tone="info">{notice}</Notice> : null}
        {error ? <Notice tone="bad">{error}</Notice> : null}

        {result ? (
          <div className="mo-card mo-result">
            <h3>Rendered</h3>
            <div className="mo-media">
              <img src={assetUrl(result.poster_path)} alt="A frame from the rendered video" />
              {isH264Encoder(result.encoder) ? (
                <MediaPlayer src={assetUrl(result.output_path)} kind="video" poster={assetUrl(result.poster_path)} title="Rendered video" />
              ) : null}
            </div>
            <Kv rows={[
              ['File', <span key="f" className="selectable mo-path">{result.output_path}</span>],
              ['Encoded', `${result.width} × ${result.height}, ${result.frames} frames, ${result.encoder}`],
              ['Time', `${(result.elapsed_ms / 1000).toFixed(1)} s`]
            ]} />
            {isH264Encoder(result.encoder) ? null : <p className="mo-hint">This build encodes MPEG-4 Part 2. It plays in QuickTime and editors; the in-app player needs H.264.</p>}
          </div>
        ) : null}
      </section>
    </div>
  );
}
