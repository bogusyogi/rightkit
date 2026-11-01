import { useEffect, useMemo, useRef, useState } from 'react';
import { errorMessage } from '../lib/api.js';
import { copyText } from '../lib/shell.js';
import { setHandoff, type HandoffView } from '../lib/handoff.js';
import type { ViewId } from '../lib/nav.js';
import {
  productionApi,
  type BrandKit,
  type HandoffEntry,
  type HandoffStatus,
  type PackDraft,
  type PreflightReport,
  type ProductionBuild,
  type ProductionLock,
  type ProductionTemplates,
  type ProductionValues,
  type SavedPack
} from '../lib/production-api.js';
import { Badge, Button, Card, Checkbox, Input, Notice, Pill, SegmentedControl, Select, Toggle } from '@rightkit/react-shell/react';
import { relativeTime } from '../components/ui.js';
import { IconCheck, IconFilm, IconImage, IconMic } from '../components/icons.js';
import { PipelineWorkflowsPanel } from './PipelineWorkflowsPanel.js';
import { BlenderFinishPanel } from './BlenderFinishPanel.js';
import { hostAppName } from "../appName.js";

type Mode = 'campaign' | 'canvas' | 'brand' | 'director' | 'post' | 'pipelines';
const MODES: Array<{ id: Mode; label: string }> = [
  { id: 'campaign', label: 'Campaign' },
  { id: 'canvas', label: 'Canvas' },
  { id: 'brand', label: 'Brand kit' },
  { id: 'director', label: 'Director' },
  { id: 'post', label: 'Post' },
  { id: 'pipelines', label: 'Pipelines' }
];

interface Form {
  brand: string;
  project_slug: string;
  product_name: string;
  offer: string;
  audience: string;
  promise: string;
  channels: string[];
  product_reference_path: string;
  logo_path: string;
  character_id: string;
  voice_id: string;
  style_notes: string;
  camera_profile: string;
  lens: string;
  movement: string;
  duration: number;
  aspect: string;
  audio_mode: string;
  captions: boolean;
  music: boolean;
  watermark: boolean;
}

function initialForm(t: ProductionTemplates): Form {
  const d = t.defaults;
  return {
    brand: t.default_brand,
    project_slug: d.project_slug,
    product_name: d.product_name,
    offer: d.offer,
    audience: d.audience,
    promise: d.promise,
    channels: ['instagram-reels', 'meta-feed'],
    product_reference_path: '',
    logo_path: '',
    character_id: '',
    voice_id: '',
    style_notes: d.style_notes,
    camera_profile: d.director.camera_profile,
    lens: d.director.lens,
    movement: d.director.movement,
    duration: d.director.duration,
    aspect: d.director.aspect,
    audio_mode: d.director.audio_mode,
    captions: d.post.captions,
    music: d.post.music,
    watermark: d.post.watermark
  };
}

function toValues(f: Form): ProductionValues {
  return {
    brand: f.brand,
    project_slug: f.project_slug,
    product_name: f.product_name,
    offer: f.offer,
    audience: f.audience,
    promise: f.promise,
    channels: f.channels,
    product_reference_path: f.product_reference_path,
    logo_path: f.logo_path,
    character_id: f.character_id,
    voice_id: f.voice_id,
    style_notes: f.style_notes,
    director: { camera_profile: f.camera_profile, lens: f.lens, movement: f.movement, duration: f.duration, aspect: f.aspect, audio_mode: f.audio_mode },
    post: { captions: f.captions, music: f.music, watermark: f.watermark }
  };
}

const HANDOFF_ICON = { image: <IconImage />, video: <IconFilm />, voice: <IconMic /> };
const HANDOFF_LABEL = { image: 'Image', video: 'Video', voice: 'Voice' } as const;

function navigateTo(view: ViewId, onNavigate?: (v: ViewId) => void) {
  if (onNavigate) onNavigate(view);
  else window.dispatchEvent(new CustomEvent('genright:navigate', { detail: view }));
}

export function Production({ onNavigate }: { onNavigate?: (view: ViewId) => void }) {
  const [templates, setTemplates] = useState<ProductionTemplates | null>(null);
  const [form, setForm] = useState<Form | null>(null);
  const [mode, setMode] = useState<Mode>('campaign');
  const [built, setBuilt] = useState<ProductionBuild | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [kit, setKit] = useState<BrandKit | null>(null);
  const [saved, setSaved] = useState<PackDraft | null>(null);
  const [report, setReport] = useState<PreflightReport | null>(null);
  const [packs, setPacks] = useState<SavedPack[] | null>(null);
  const [busy, setBusy] = useState<'save' | 'check' | null>(null);
  const [copied, setCopied] = useState('');
  const seq = useRef(0);

  useEffect(() => {
    let alive = true;
    productionApi
      .templates()
      .then((t) => {
        if (!alive) return;
        setTemplates(t);
        setForm(initialForm(t));
      })
      .catch((e) => alive && setError(errorMessage(e)));
    return () => {
      alive = false;
    };
  }, []);

  const values = useMemo(() => (form ? toValues(form) : null), [form]);

  // Rebuild every plan from the engine whenever the brief changes.
  useEffect(() => {
    if (!values) return;
    const mine = ++seq.current;
    const timer = window.setTimeout(() => {
      productionApi
        .build(values)
        .then((b) => {
          if (mine !== seq.current) return;
          setBuilt(b);
          setError(null);
          setSaved(null);
          setReport(null);
        })
        .catch((e) => mine === seq.current && setError(errorMessage(e)));
    }, 120);
    return () => window.clearTimeout(timer);
  }, [values]);

  // Persisted packs list; refreshed after each save.
  useEffect(() => {
    let alive = true;
    productionApi.packs().then((r) => alive && setPacks(r.packs)).catch(() => alive && setPacks([]));
    return () => {
      alive = false;
    };
  }, [saved]);

  // The file-exists check runs on the Brand kit tab.
  useEffect(() => {
    if (mode !== 'brand' || !values) return;
    let alive = true;
    productionApi.brandCheck(values).then((k) => alive && setKit(k)).catch((e) => alive && setError(errorMessage(e)));
    return () => {
      alive = false;
    };
  }, [mode, values]);

  if (!form || !templates) {
    return (
      <div className="content view">
        <section className="work">
          <div className="eyebrow">Workflows</div>
          <h1>Production</h1>
          {error ? <Notice tone="bad">{error}</Notice> : <p className="lede">Loading the production stack…</p>}
        </section>
      </div>
    );
  }

  const set = <K extends keyof Form>(key: K, value: Form[K]) => setForm((f) => (f ? { ...f, [key]: value } : f));
  const toggleChannel = (id: string) =>
    set('channels', form.channels.includes(id) ? (form.channels.length === 1 ? form.channels : form.channels.filter((c) => c !== id)) : [...form.channels, id]);

  const copy = async (key: string, text: string) => {
    try {
      await copyText(text);
      setCopied(key);
      window.setTimeout(() => setCopied((c) => (c === key ? '' : c)), 1400);
    } catch {
      setCopied('');
    }
  };

  const runPreflight = async () => {
    if (!built) return;
    setBusy('check');
    try {
      setReport(await productionApi.preflight(built.pack));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const savePack = async () => {
    if (!built) return;
    setBusy('save');
    try {
      setSaved(await productionApi.savePack(built.pack));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const send = (key: HandoffView, entry: HandoffEntry) => {
    const r = entry.request;
    setHandoff({
      view: key,
      kind: r.kind,
      prompt: r.prompt,
      params: r.params,
      provider: r.provider,
      model: r.model,
      label: r.label,
      source: 'Production stack',
      meta: { shot_contract: r.shot_contract, workflow: r.workflow, notes: r.notes }
    });
    navigateTo(key, onNavigate);
  };

  const readiness = built?.brand_kit;
  const missing = readiness?.missing.length ?? 0;

  return (
    <div className="content view production">
      <section className="work">
        <div className="eyebrow">Workflows</div>
        <h1>Production</h1>
        <p className="lede">Plan a campaign from one brief: hooks and scenes, a connected canvas, a director shot contract and a post plan. Nothing here spends money; hand-offs open Image, Video or Voice with the request filled in.</p>

        <div className="toolbar pr-modes">
          <SegmentedControl label="Production section" value={mode} onChange={setMode} options={MODES.map((m) => ({ value: m.id, label: m.label }))} />
          <span style={{ marginLeft: 'auto' }}><Pill tone={missing ? 'warn' : undefined}>{missing ? `${missing} missing` : 'Locks ready'}</Pill></span>
        </div>
        {error ? <Notice tone="bad">{error}</Notice> : null}

        {mode === 'campaign' ? (
          <>
            <Card className="form pr-brief">
              <div className="fgrid pr-row3">
                <Field label="Project"><Input value={form.project_slug} onChange={(e) => set('project_slug', e.target.value)} /></Field>
                <Field label="Brand">
                  <Select label="Brand" value={form.brand} onChange={(v) => set('brand', v)} options={templates.brands.map((b) => ({ value: b, label: b }))} />
                </Field>
                <Field label="Product"><Input value={form.product_name} onChange={(e) => set('product_name', e.target.value)} /></Field>
              </div>
              <div className="fgrid pr-row2">
                <Field label="Offer"><Input value={form.offer} onChange={(e) => set('offer', e.target.value)} /></Field>
                <Field label="Audience"><Input value={form.audience} onChange={(e) => set('audience', e.target.value)} /></Field>
              </div>
              <Field label="Promise"><textarea className="rk-input" value={form.promise} onChange={(e) => set('promise', e.target.value)} /></Field>
              <div>
                <label>Channels</label>
                <div role="group" aria-label="Channels" style={{ display: 'flex', flexWrap: 'wrap', gap: '6px 16px' }}>
                  {templates.channels.map((c) => (
                    <Checkbox key={c.id} checked={form.channels.includes(c.id)} onChange={() => toggleChannel(c.id)} label={c.short} />
                  ))}
                </div>
              </div>
              <div className="fgrid pr-row2">
                <Field label="Product reference (file or URL)"><Input value={form.product_reference_path} placeholder="/path/to/product.png" onChange={(e) => set('product_reference_path', e.target.value)} /></Field>
                <Field label="Logo (file or URL)"><Input value={form.logo_path} placeholder="/path/to/logo.png" onChange={(e) => set('logo_path', e.target.value)} /></Field>
              </div>
              <div className="fgrid pr-row3">
                <Field label="Character"><Input value={form.character_id} onChange={(e) => set('character_id', e.target.value)} /></Field>
                <Field label="Voice id"><Input value={form.voice_id} onChange={(e) => set('voice_id', e.target.value)} /></Field>
                <Field label="Continuity locks"><Input value={form.style_notes} onChange={(e) => set('style_notes', e.target.value)} /></Field>
              </div>
            </Card>

            <h3 className="pr-h">Hooks</h3>
            <div className="pr-cards">
              {built?.campaign.hooks.map((h) => (
                <Card className="pr-card" key={h.id}>
                  <div className="pr-k">{h.label}</div>
                  <div className="pr-line">{h.line}</div>
                  <div className="pr-sub">{h.angle}</div>
                </Card>
              ))}
            </div>

            <h3 className="pr-h">Scene stack</h3>
            <div className="pr-scenes">
              {built?.campaign.scenes.map((s, i) => (
                <div className="pr-scene" key={s.id}>
                  <span className="pr-num">{i + 1}</span>
                  <div className="pr-grow">
                    <b>{s.label}</b>
                    <div>{s.prompt}</div>
                    <div className="pr-sub">{s.voiceover}</div>
                  </div>
                  <div className="pr-meta">
                    <span>{s.duration_s}s · {s.visual_role}</span>
                    {s.required_refs.length ? <Badge tone="warn">needs {s.required_refs.join(', ')}</Badge> : <Badge tone="ok">refs clear</Badge>}
                  </div>
                </div>
              ))}
            </div>

            <h3 className="pr-h">Variants</h3>
            <div className="pr-cards">
              {built?.campaign.variants.map((v) => (
                <Card className="pr-card" key={v.id}>
                  <div className="pr-top"><b>{v.label}</b><Badge tone={v.priority === 'primary' ? 'ok' : undefined}>{v.priority}</Badge></div>
                  <div className="pr-sub">{v.caption_style}</div>
                  <div className="pr-chips"><Badge>{v.aspect}</Badge><Badge>{v.duration_s}s</Badge><Badge>{v.cta}</Badge></div>
                </Card>
              ))}
            </div>
          </>
        ) : null}

        {mode === 'canvas' && built ? <CanvasView canvas={built.canvas} /> : null}

        {mode === 'brand' ? (
          <div className="pr-brand">
            <Card className="form pr-brief">
              <div className="fgrid pr-row2">
                <Field label="Product reference (file or URL)"><Input value={form.product_reference_path} onChange={(e) => set('product_reference_path', e.target.value)} /></Field>
                <Field label="Logo (file or URL)"><Input value={form.logo_path} onChange={(e) => set('logo_path', e.target.value)} /></Field>
              </div>
              <div className="fgrid pr-row2">
                <Field label="Character"><Input value={form.character_id} onChange={(e) => set('character_id', e.target.value)} /></Field>
                <Field label="Voice id"><Input value={form.voice_id} onChange={(e) => set('voice_id', e.target.value)} /></Field>
              </div>
            </Card>
            <h3 className="pr-h">Lock ledger</h3>
            <Card flush className="group-box pr-ledger">
              {kit ? (Object.values(kit.locks) as ProductionLock[]).map((l) => <LockRow key={l.id} lock={l} />) : <div className="setting">Checking…</div>}
            </Card>
            {kit?.warnings?.length ? <Notice tone="warn">{kit.warnings.map((w) => <div key={w}>{w}</div>)}</Notice> : null}
            <h3 className="pr-h">Missing</h3>
            {kit && kit.missing.length ? (
              <div className="pr-cards">{kit.missing.map((m) => <Notice tone="warn" key={m.id} title={m.label}>{m.note}</Notice>)}</div>
            ) : kit ? <Notice tone="ok">All required locks are ready.</Notice> : null}
          </div>
        ) : null}

        {mode === 'director' && built ? (
          <div className="pr-director">
            <Card className="form pr-brief">
              <div className="fgrid pr-row3">
                <Field label="Profile">
                  <Input list="pr-profiles" value={form.camera_profile} onChange={(e) => set('camera_profile', e.target.value)} />
                  <datalist id="pr-profiles">{templates.camera_profiles.map((p) => <option key={p} value={p} />)}</datalist>
                </Field>
                <Field label="Lens">
                  <Input list="pr-lenses" value={form.lens} onChange={(e) => set('lens', e.target.value)} />
                  <datalist id="pr-lenses">{templates.lenses.map((p) => <option key={p} value={p} />)}</datalist>
                </Field>
                <Field label="Movement">
                  <Input list="pr-moves" value={form.movement} onChange={(e) => set('movement', e.target.value)} />
                  <datalist id="pr-moves">{templates.movements.map((p) => <option key={p} value={p} />)}</datalist>
                </Field>
              </div>
              <div className="fgrid pr-row3">
                <Field label="Duration (1-12 s)"><Input type="number" min={1} max={12} value={form.duration} onChange={(e) => set('duration', Number(e.target.value))} /></Field>
                <Field label="Aspect"><Select label="Aspect" value={form.aspect} onChange={(v) => set('aspect', v)} options={templates.aspects.map((a) => ({ value: a, label: a }))} /></Field>
                <Field label="Audio"><Select label="Audio" value={form.audio_mode} onChange={(v) => set('audio_mode', v)} options={templates.audio_modes.map((a) => ({ value: a, label: a }))} /></Field>
              </div>
            </Card>
            <div style={{ marginTop: 14 }}><Card className="pr-card">
              <div className="pr-top"><div className="pr-k">Shot contract</div>
                <Button type="button" variant="ghost" onClick={() => copy('contract', JSON.stringify(built.director.shot_contract_inline, null, 2))}>{copied === 'contract' ? 'Copied' : 'Copy'}</Button></div>
              <div className="pr-line">{built.director.prompt_addendum}</div>
              <pre className="pr-pre selectable">{JSON.stringify(built.director.shot_contract_inline, null, 2)}</pre>
            </Card></div>
            <div className="pr-two">
              <Card className="pr-card"><div className="pr-k">Controls</div>{built.director.control_stack.map((c) => <div className="pr-li" key={c.label}>{c.label}: {c.value}</div>)}</Card>
              <Card className="pr-card"><div className="pr-k">Checks</div>{built.director.checklist.map((c) => <div className="pr-li" key={c}>{c}</div>)}</Card>
            </div>
          </div>
        ) : null}

        {mode === 'post' && built ? (
          <div>
            <Card flush className="group-box pr-toggles">
              <div className="setting"><span>Captions</span><Toggle label="Captions" checked={form.captions} onChange={(v) => set('captions', v)} /></div>
              <div className="setting"><span>Music bed</span><Toggle label="Music bed" checked={form.music} onChange={(v) => set('music', v)} /></div>
              <div className="setting"><span>Brand bug</span><Toggle label="Brand bug" checked={form.watermark} onChange={(v) => set('watermark', v)} /></div>
            </Card>
            <Card flush className="group-box pr-table">
              {built.post.tasks.map((t) => (
                <div className="pr-post" key={t.id}>
                  <div><b>{t.label}</b><div className="pr-sub">{t.aspect} · {t.duration_s}s</div></div>
                  <div className="pr-chips">{t.steps.map((s) => <Badge key={s}>{s}</Badge>)}</div>
                  <code>{t.output_name}</code>
                </div>
              ))}
            </Card>
          </div>
        ) : null}

        {mode === 'pipelines' ? <PipelineWorkflowsPanel /> : null}
        {mode === 'pipelines' ? <BlenderFinishPanel /> : null}
      </section>

      <aside className="run">
        <div className="run-head"><div className="eyebrow">Hand-off</div><h3 style={{ margin: '2px 0 0', fontSize: 15 }}>{built?.campaign.project_slug ?? 'Production pack'}</h3></div>
        <div className="run-body">
          <Card flush className="group-box pr-handoffs gb-mt4">
            {(['image', 'video', 'voice'] as const).map((k) => {
              const e = built?.handoff[k];
              return (
                <div className="pr-ho" key={k}>
                  <div className="pr-top"><span className="pr-hname">{HANDOFF_ICON[k]} {HANDOFF_LABEL[k]}</span>{e ? <StatusChip status={e.status} /> : null}</div>
                  {e ? <div className="pr-sub">{e.request.provider} · {e.request.model}</div> : null}
                  {e?.reason ? <div className="pr-sub pr-reason">{e.reason}</div> : null}
                  <Button type="button" variant="ghost" className="pr-send" disabled={!e || e.status === 'blocked'} title={e?.reason ?? `Open ${HANDOFF_LABEL[k]} with this request`} onClick={() => e && send(k, e)}>
                    Open in {HANDOFF_LABEL[k]}
                  </Button>
                </div>
              );
            })}
          </Card>

          {built ? (
            <div className="pr-prompts">
              <Prompt label="Video prompt" text={built.campaign.handoff.video_prompt} copied={copied === 'v'} onCopy={() => copy('v', built.campaign.handoff.video_prompt)} />
              <Prompt label="Image prompt" text={built.campaign.handoff.image_prompt} copied={copied === 'i'} onCopy={() => copy('i', built.campaign.handoff.image_prompt)} />
              <Prompt label="Voice script" text={built.campaign.handoff.tts_script} copied={copied === 't'} onCopy={() => copy('t', built.campaign.handoff.tts_script)} />
            </div>
          ) : null}

          {report ? (
            <div style={{ marginTop: 12 }}>
              <Notice tone="info" title="Preflight">
                <div className="pr-li">{report.exports} exports planned{report.blocked.length ? ` · blocked: ${report.blocked.join(', ')}` : ' · nothing blocked'}</div>
                {report.warnings.map((w) => <div className="pr-sub pr-reason" key={w}>{w}</div>)}
              </Notice>
            </div>
          ) : null}
          {saved ? (
            <div style={{ marginTop: 12 }}>
              <Notice tone="ok" title={<><IconCheck /> Pack saved</>}>
                <div className="pr-path selectable">{saved.pack_dir}</div>
                <div className="pr-sub">{saved.files.length} files</div>
              </Notice>
            </div>
          ) : null}
          {packs && packs.length ? (
            <div className="pr-packs" style={{ marginTop: 12 }}>
              <div className="pr-k" style={{ marginBottom: 6 }}>Saved packs</div>
              {packs.map((p) => (
                <div className="pr-pack" key={p.draft_id}>
                  <b>{p.project_slug}</b>
                  <span className="pr-sub">{relativeTime(p.created_at)}</span>
                  <div className="pr-path selectable" title={p.pack_dir}>{p.pack_dir}</div>
                </div>
              ))}
            </div>
          ) : null}
        </div>
        <div className="run-foot">
          <div className="pr-foot-row">
            <Button type="button" variant="ghost" disabled={!built || busy !== null} onClick={runPreflight}>{busy === 'check' ? 'Checking…' : 'Preflight'}</Button>
            <Button type="button" variant="primary" disabled={!built || busy !== null} onClick={savePack}>{busy === 'save' ? 'Saving…' : 'Save production pack'}</Button>
          </div>
          <div className="foot-note">Writes a pack folder to your {hostAppName()} data folder. Calls no provider.</div>
        </div>
      </aside>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div><label>{label}</label>{children}</div>;
}

function Prompt({ label, text, copied, onCopy }: { label: string; text: string; copied: boolean; onCopy: () => void }) {
  return (
    <Card className="pr-card">
      <div className="pr-top"><div className="pr-k">{label}</div><Button type="button" variant="ghost" size="sm" className="link-btn" onClick={onCopy}>{copied ? 'Copied' : 'Copy'}</Button></div>
      <div className="pr-sub pr-clamp selectable">{text}</div>
    </Card>
  );
}

function StatusChip({ status }: { status: HandoffStatus }) {
  return <Badge tone={status === 'ready' ? 'ok' : status === 'blocked' ? 'bad' : 'warn'}>{status === 'ready_with_warnings' ? 'warning' : status}</Badge>;
}

function LockRow({ lock }: { lock: ProductionLock }) {
  const tone = lock.status === 'ready' ? 'ok' : lock.status === 'missing' ? 'warn' : undefined;
  return (
    <div className="setting pr-lock">
      <span className="pr-lname"><b>{lock.label}</b><small>{lock.value ?? lock.note}</small></span>
      <span className="pr-lstate">
        {lock.exists === false ? <Badge tone="warn">file not found</Badge> : null}
        <Badge tone={tone}>{lock.status}</Badge>
      </span>
    </div>
  );
}

const NODE_W = 176;
const NODE_H = 88;

function CanvasView({ canvas }: { canvas: ProductionBuild['canvas'] }) {
  const byId = new Map(canvas.nodes.map((n) => [n.id, n]));
  const width = Math.max(...canvas.nodes.map((n) => n.x)) + NODE_W + 40;
  const height = Math.max(...canvas.nodes.map((n) => n.y)) + NODE_H + 40;
  return (
    <div className="pr-canvas-wrap">
      <div className="pr-canvas" style={{ width, height }}>
        <svg width={width} height={height} aria-hidden="true">
          {canvas.edges.map((e) => {
            const a = byId.get(e.from);
            const b = byId.get(e.to);
            if (!a || !b) return null;
            const x1 = a.x + NODE_W;
            const y1 = a.y + NODE_H / 2;
            const x2 = b.x;
            const y2 = b.y + NODE_H / 2;
            const mid = Math.max(x1 + 36, (x1 + x2) / 2);
            return <path key={`${e.from}-${e.to}-${e.label}`} d={`M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`} />;
          })}
        </svg>
        {canvas.nodes.map((n) => (
          <div key={n.id} className={`pr-node ${n.status}`} style={{ left: n.x, top: n.y, width: NODE_W, height: NODE_H }} title={n.summary}>
            <div className="pr-nhead"><b>{n.label}</b><i /></div>
            <div className="pr-ntype">{n.type.replace('_', ' ')}</div>
            <div className="pr-nsum">{n.summary}</div>
          </div>
        ))}
      </div>
      <div className="pr-edges">{canvas.edges.length} connections · {canvas.nodes.length} nodes</div>
    </div>
  );
}
