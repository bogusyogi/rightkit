import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { assetUrl, errorCode, errorMessage, studio, type MediaKind } from '../lib/api';
import { useApp } from '../lib/app-state';
import { Badge, Card, Checkbox, Button, EmptyState, Input, Notice, SegmentedControl, Select } from '@rightkit/react-shell/react';
import { Kv } from '../components/ui';
import { MediaPlayer } from '@rightkit/media-ui';
import './gallery.css';
import { hostAppName } from "../appName";

type AvatarType = 'twin' | 'photo' | 'prompt';
type SourceKind = 'path' | 'url';
type Flow = 'animate' | 'lip_sync';

interface CreateResult {
  dry_run?: boolean;
  avatar_type: string;
  avatar_item_id?: string | null;
  avatar_group_id?: string | null;
  status?: string;
  consent_status?: string | null;
  consent_url?: string | null;
  consent_error?: string | null;
  next_action?: string;
  request_body?: unknown;
  note?: string;
  cost_notice?: string;
}

interface AvatarGroup {
  id: string;
  name: string;
  status?: string | null;
  consent_status?: string | null;
  looks_count?: number;
  consent_url?: string | null;
  avatar_type?: string | null;
  preview_image_url?: string | null;
  ready?: boolean;
  next_action?: string;
  quality?: 'accepted' | 'unqualified' | 'unreviewed';
}

interface RouteEntry {
  route: string;
  provider?: string;
  model?: string;
  id?: string;
  label: string;
  est_usd?: number | null;
  credential?: string;
  ready?: boolean;
  engine?: string;
  engine_kind?: string;
  readiness?: { state?: string };
  quality?: string;
}

interface RoutesResponse {
  capability: string;
  selected: { route?: string; model?: string } | null;
  valid_routes: string[];
  routes: { api: RouteEntry[]; local: RouteEntry[]; rented: RouteEntry[] };
  rented_workers: Array<{ worker_id?: string; state?: string; label?: string }>;
  worker_contract: string;
}

interface Prepared {
  mode: string;
  route: 'api' | 'rented' | 'local';
  request?: { kind: string; provider: string; model: string; prompt: string; params: Record<string, unknown> };
  est_usd?: number | null;
  ready?: boolean;
  quality: string;
  warnings?: string[];
  note?: string;
  submit_via: string;
  worker_required?: boolean;
  worker?: { worker_id?: string; state?: string } | null;
}

/** Prior SoulX-FlashHead spike renders, kept under docs/spikes as reference
 *  evidence. They ran and finished but quality was rejected — these are NOT
 *  accepted-quality samples. */
const SPIKES = [
  { path: 'docs/spikes/flashhead_3s_minimax.mp4', label: '3 s minimax audio', quality: 'unqualified' },
  { path: 'docs/spikes/flashhead_12s_podcast.mp4', label: '12 s podcast audio', quality: 'unqualified' },
  { path: 'docs/spikes/soulx-v2/flashhead_512.mp4', label: '512 px render', quality: 'unqualified' },
  { path: 'docs/spikes/soulx-v2/flashhead_1080.mp4', label: '1080 px render', quality: 'unqualified' }
];

const TYPES: Array<{ id: AvatarType; label: string; blurb: string }> = [
  { id: 'twin', label: 'Digital twin', blurb: 'Train a reusable avatar of a real person from a video of them talking.' },
  { id: 'photo', label: 'Photo avatar', blurb: 'Make a talking avatar from one still portrait.' },
  { id: 'prompt', label: 'Prompt avatar', blurb: 'Generate a new avatar from a text description. Nobody real is depicted.' }
];

/** Mirrors the engine's `cost_notice` (genright-studio avatars.rs PRICE_NOTICE); the engine is the authority and repeats it in every create result and refusal. */
const PRICE_NOTICE =
  `Price not published: HeyGen does not publish an API price for avatar creation, so ${hostAppName()} cannot estimate or reserve it. It may use your HeyGen plan credits or bill your account, and it is not counted against ${hostAppName()}\u2019s daily cap.`;

const tone = (s?: string | null) => (s === 'completed' ? 'ok' : s === 'failed' ? 'bad' : s ? 'warn' : undefined) as 'ok' | 'bad' | 'warn' | undefined;
const qualityTone = (q?: string | null) => (q === 'accepted' ? 'ok' : q === 'unqualified' ? 'bad' : 'warn') as 'ok' | 'bad' | 'warn';
const qualityLabel = (q?: string | null) => (q === 'accepted' ? 'accepted' : q === 'unqualified' ? 'unqualified' : 'unreviewed');

export function Avatars() {
  const app = useApp();
  const [type, setType] = useState<AvatarType>('twin');
  const [name, setName] = useState('');
  const [sourceKind, setSourceKind] = useState<SourceKind>('path');
  const [source, setSource] = useState('');
  const [prompt, setPrompt] = useState('');
  const [aspect, setAspect] = useState('auto');
  const [refs, setRefs] = useState('');
  const [consent, setConsent] = useState(false);
  const [consentLink, setConsentLink] = useState(true);
  const [confirmPrice, setConfirmPrice] = useState(false);
  const [busy, setBusy] = useState<null | 'check' | 'create'>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CreateResult | null>(null);
  const [groups, setGroups] = useState<AvatarGroup[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  const [loadingList, setLoadingList] = useState(false);

  const [routes, setRoutes] = useState<RoutesResponse | null>(null);
  const [routesError, setRoutesError] = useState<string | null>(null);
  const [flow, setFlow] = useState<Flow>('animate');
  const [modelPick, setModelPick] = useState('');
  const [avatarId, setAvatarId] = useState('');
  const [imageUrl, setImageUrl] = useState('');
  const [videoUrl, setVideoUrl] = useState('');
  const [audioUrl, setAudioUrl] = useState('');
  const [script, setScript] = useState('');
  const [voiceId, setVoiceId] = useState('');
  const [resolution, setResolution] = useState('480p');
  const [prep, setPrep] = useState<Prepared | null>(null);
  const [prepBusy, setPrepBusy] = useState<null | 'prepare' | 'submit'>(null);
  const [prepError, setPrepError] = useState<string | null>(null);
  const [jobKey, setJobKey] = useState<string | null>(null);

  const needsConsent = type !== 'prompt';
  const canSubmit = name.trim() !== '' && (type === 'prompt' ? prompt.trim() !== '' : source.trim() !== '') && (!needsConsent || consent);

  const input = useCallback(
    (dry_run: boolean): Record<string, unknown> => {
      const base: Record<string, unknown> = { name: name.trim(), dry_run };
      if (!dry_run) base.confirm_unpublished_price = confirmPrice;
      if (type === 'prompt') {
        base.prompt = prompt.trim();
        if (aspect !== 'auto') base.aspect_ratio = aspect;
        const urls = refs.split('\n').map((s) => s.trim()).filter(Boolean);
        if (urls.length) base.reference_images = urls.map((u) => (u.startsWith('https://') ? { url: u } : { path: u }));
        return base;
      }
      base.consent = consent;
      base.consent_link = consentLink;
      const key = type === 'twin' ? 'video' : 'image';
      base[`${key}_${sourceKind}`] = source.trim();
      return base;
    },
    [name, type, prompt, aspect, refs, consent, consentLink, confirmPrice, sourceKind, source]
  );

  const loadList = useCallback(async () => {
    setLoadingList(true);
    try {
      const r = await studio<{ avatars: AvatarGroup[] }>('avatars.list', { limit: 20 });
      setGroups(r.avatars ?? []);
      setListError(null);
    } catch (e) {
      setGroups([]);
      setListError(errorMessage(e));
    } finally {
      setLoadingList(false);
    }
  }, []);

  const loadRoutes = useCallback(async () => {
    try {
      setRoutes(await studio<RoutesResponse>('avatars.routes', { capability: flow === 'lip_sync' ? 'lip_sync' : 'avatar' }));
      setRoutesError(null);
    } catch (e) {
      setRoutes(null);
      setRoutesError(errorMessage(e));
    }
  }, [flow]);

  useEffect(() => {
    void loadList();
  }, [loadList]);

  useEffect(() => {
    void loadRoutes();
  }, [loadRoutes]);

  const run = async (dry: boolean) => {
    setBusy(dry ? 'check' : 'create');
    setError(null);
    setResult(null);
    try {
      const r = await studio<CreateResult>(`avatars.create_${type}`, input(dry));
      setResult(r);
      if (!dry) void loadList();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const refresh = async (id: string) => {
    try {
      const g = await studio<AvatarGroup>('avatars.status', { avatar_group_id: id });
      setGroups((all) => all.map((x) => (x.id === id ? { ...x, ...g } : x)));
    } catch (e) {
      setListError(errorMessage(e));
    }
  };

  const setReview = async (id: string, verdict: 'accept' | 'reject') => {
    try {
      await studio('avatars.review', { avatar_group_id: id, verdict });
      setGroups((all) => all.map((x) => (x.id === id ? { ...x, quality: verdict === 'accept' ? 'accepted' : 'unqualified' } : x)));
    } catch (e) {
      setListError(errorMessage(e));
    }
  };

  const setRoute = async (route: string, model?: string) => {
    try {
      await studio('models.route.set', { capability: 'avatar', route, model: model ?? null });
      void loadRoutes();
    } catch (e) {
      setRoutesError(errorMessage(e));
    }
  };

  const prepare = async () => {
    setPrepBusy('prepare');
    setPrepError(null);
    setPrep(null);
    setJobKey(null);
    try {
      const body: Record<string, unknown> = {};
      if (modelPick) {
        const [provider, model] = modelPick.split('::');
        body.provider = provider;
        body.model = model;
      }
      if (flow === 'lip_sync' && videoUrl.trim()) body.video_url = videoUrl.trim();
      if (avatarId.trim()) body.avatar_id = avatarId.trim();
      if (imageUrl.trim()) body.image_url = imageUrl.trim();
      if (audioUrl.trim()) body.audio_url = audioUrl.trim();
      if (script.trim()) body.script = script.trim();
      if (voiceId.trim()) body.voice_id = voiceId.trim();
      if (resolution && flow === 'animate') body.resolution = resolution;
      setPrep(await studio<Prepared>(`avatars.${flow}`, body));
    } catch (e) {
      setPrepError(errorCode(e) ? `${errorCode(e)}: ${errorMessage(e)}` : errorMessage(e));
    } finally {
      setPrepBusy(null);
    }
  };

  const submit = async () => {
    if (!prep?.request || prep.route !== 'api') return;
    setPrepBusy('submit');
    setPrepError(null);
    try {
      const req = prep.request;
      const job = await app.submitCloud(
        { kind: req.kind as MediaKind, provider: req.provider, model: req.model, prompt: req.prompt, params: req.params },
        `${req.provider}/${req.model}`
      );
      setJobKey(job.key);
    } catch (e) {
      setPrepError(errorMessage(e));
    } finally {
      setPrepBusy(null);
    }
  };

  const picked = TYPES.find((t) => t.id === type)!;
  const apiModels = routes?.routes.api ?? [];
  const job = jobKey ? app.jobs.find((j) => j.key === jobKey) : undefined;
  const jobAsset = job?.assets.find((a) => a.media === 'video' || (a.mime ?? '').startsWith('video/'));
  const avatarModels = apiModels.filter((m) => flow === 'lip_sync' || m.model !== 'lipsync-3' || Boolean(videoUrl.trim()));

  return (
    <div className="content view" style={{ gridTemplateColumns: '1fr' }}>
      <section className="work">
        <div className="eyebrow">Library</div>
        <h1>Avatars</h1>
        <p className="lede">Create HeyGen avatars, animate them, or re-time a video&apos;s lips. API routes bill the provider; the rented SoulX route is an unqualified experiment.</p>

        <div className="av-types">
          <SegmentedControl label="Avatar type" value={type} onChange={(t) => { setType(t); setResult(null); setError(null); }} options={TYPES.map((t) => ({ value: t.id, label: t.label }))} />
        </div>
        <p className="lede" style={{ marginTop: 4 }}>{picked.blurb}</p>

        <form className="av-form" onSubmit={(e) => { e.preventDefault(); if (canSubmit && confirmPrice && !busy) void run(false); }}>
          <label className="fld">Name
            <Input type="text" aria-label="Avatar name" value={name} maxLength={120} onChange={(e) => setName(e.target.value)} placeholder="e.g. Adrian, studio presenter" />
          </label>

          {type === 'prompt' ? (
            <>
              <label className="fld">Description ({prompt.length}/1000)
                <textarea className="rk-input" aria-label="Avatar description" value={prompt} maxLength={1000} onChange={(e) => setPrompt(e.target.value)} placeholder="A friendly presenter in a navy blazer, warm studio light…" />
              </label>
              <label className="fld">Reference images (optional, up to 3, one https URL or file path per line)
                <textarea className="rk-input" aria-label="Reference images" style={{ minHeight: 56 }} value={refs} onChange={(e) => setRefs(e.target.value)} />
              </label>
              <label className="fld">Aspect ratio
                <Select label="Aspect ratio" value={aspect} onChange={setAspect} options={['auto', '16:9', '9:16', '1:1', '4:5', '5:4'].map((a) => ({ value: a, label: a }))} />
              </label>
            </>
          ) : (
            <div className="av-src">
              <div className="fld" style={{ display: 'grid', gap: 4, fontSize: 12, color: 'var(--ink-3)' }}>{type === 'twin' ? 'Training video (mp4, mov, webm; up to 200 MiB)' : 'Portrait image (png, jpg, webp; up to 200 MiB)'}</div>
              <div className="opt">
                <SegmentedControl label="Source" value={sourceKind} onChange={setSourceKind} options={[{ value: 'path', label: 'File on this Mac' }, { value: 'url', label: 'https URL' }]} />
              </div>
              <Input type="text" aria-label="Source" value={source} onChange={(e) => setSource(e.target.value)} placeholder={sourceKind === 'path' ? '/Users/you/Movies/me-talking.mp4' : 'https://…'} />
            </div>
          )}

          {needsConsent ? (
            <>
              <Checkbox className="av-consent" checked={consent} onChange={setConsent} label="The person shown has agreed to having their likeness copied as an avatar. HeyGen will ask them to confirm separately." />
              <Checkbox checked={consentLink} onChange={setConsentLink} label="Create HeyGen's consent link after the avatar is made" />
            </>
          ) : null}

          <Checkbox className="av-consent" checked={confirmPrice} onChange={setConfirmPrice} label={`${PRICE_NOTICE} Tick to allow this one creation.`} />

          <div className="av-actions">
            <Button type="button" disabled={!canSubmit || busy !== null} onClick={() => void run(true)}>{busy === 'check' ? 'Checking…' : 'Check request'}</Button>
            <Button type="submit" variant="primary" disabled={!canSubmit || !confirmPrice || busy !== null}>{busy === 'create' ? 'Creating…' : 'Create avatar'}</Button>
            {needsConsent && !consent ? <span className="brand-note">Confirm consent to continue.</span> : null}
          </div>
        </form>

        {error ? <div style={{ marginTop: 14 }}><Notice tone="bad">{error}</Notice></div> : null}
        {result ? (
          <Notice tone={result.dry_run ? 'info' : 'ok'} className="av-result">
            {result.dry_run ? (
              <>
                <b>Request checked, nothing sent</b>
                <div className="brand-note">{result.note}</div>
                {result.cost_notice ? <div className="brand-note">{result.cost_notice}</div> : null}
                <pre className="selectable" style={{ margin: 0, fontSize: 11, whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{JSON.stringify(result.request_body, null, 2)}</pre>
              </>
            ) : (
              <>
                <b>Avatar submitted</b>
                <Kv rows={[
                  ['Group', result.avatar_group_id ?? 'unknown'],
                  ['Look id', result.avatar_item_id ?? 'pending'],
                  ['Status', (result.status ?? 'submitted').replace('_', ' ')]
                ]} />
                {result.consent_url ? <div>Consent link: <a href={result.consent_url} target="_blank" rel="noreferrer">{result.consent_url}</a></div> : null}
                {result.consent_error ? <Notice tone="bad">Avatar created, but the consent link failed: {result.consent_error}</Notice> : null}
                {result.cost_notice ? <div className="brand-note">{result.cost_notice}</div> : null}
                {result.next_action ? <div className="brand-note">{result.next_action}</div> : null}
              </>
            )}
          </Notice>
        ) : null}

        <div className="recent" style={{ marginTop: 28 }}>
          <div className="toolbar" style={{ marginBottom: 4 }}>
            <h6 style={{ margin: 0, fontSize: 13, color: 'var(--ink)' }}>Your avatars</h6>
            <Button type="button" variant="ghost" size="sm" className="gal-clear" onClick={() => void loadList()} disabled={loadingList}>{loadingList ? 'Loading…' : 'Refresh'}</Button>
          </div>
          {listError ? <Notice tone="bad">{listError}</Notice> : null}
          {!listError && !loadingList && groups.length === 0 ? <EmptyState title="No avatars yet">Avatars you create appear here.</EmptyState> : null}
          <div className="av-list">
            {groups.map((g) => (
              <div className="av-row" key={g.id}>
                <span><b>{g.name}</b> {g.avatar_type ? <Badge>{g.avatar_type.replace('_', ' ')}</Badge> : null} <Badge tone={tone(g.status)}>{(g.status ?? 'ready').replace('_', ' ')}</Badge> <Badge tone={qualityTone(g.quality)}>{qualityLabel(g.quality)}</Badge></span>
                <span className="sub">{g.id}{g.looks_count != null ? ` · ${g.looks_count} look${g.looks_count === 1 ? '' : 's'}` : ''}{g.consent_status ? ` · consent ${g.consent_status}` : ''}</span>
                <span className="acts">
                  {g.consent_url && g.status === 'pending_consent' ? <a href={g.consent_url} target="_blank" rel="noreferrer" style={{ display: 'inline-grid', placeItems: 'center' }}>Consent</a> : null}
                  <Button type="button" onClick={() => void refresh(g.id)}>Check status</Button>
                  {g.status === 'completed' ? <Button type="button" onClick={() => void setReview(g.id, 'accept')}>Accept</Button> : null}
                  {g.status === 'completed' ? <Button type="button" onClick={() => void setReview(g.id, 'reject')}>Reject</Button> : null}
                </span>
              </div>
            ))}
          </div>
        </div>

        <div className="recent" style={{ marginTop: 28 }}>
          <div className="toolbar" style={{ marginBottom: 4 }}>
            <h6 style={{ margin: 0, fontSize: 13, color: 'var(--ink)' }}>Animate &amp; lip-sync</h6>
          </div>
          <p className="lede" style={{ marginTop: 0 }}>Prepare a request first — nothing is sent until you press Submit. Animate makes a talking video from a script or audio; lip-sync re-times an existing video.</p>
          <SegmentedControl label="Flow" value={flow} onChange={(f) => { setFlow(f); setPrep(null); setPrepError(null); }} options={[{ value: 'animate', label: 'Talking avatar' }, { value: 'lip_sync', label: 'Lip-sync' }]} />
          <div className="av-form" style={{ marginTop: 10 }}>
            <label className="fld">Model
              <Select label="Avatar model" value={modelPick} onChange={setModelPick} options={[{ value: '', label: 'Auto (selected route)' }, ...avatarModels.map((m) => ({ value: `${m.provider}::${m.model}`, label: `${m.label}${m.est_usd != null ? ` (~$${m.est_usd})` : ''}${m.ready === false ? ' — needs key' : ''}` }))]} />
            </label>
            {flow === 'lip_sync' ? (
              <label className="fld">Video to re-time (https URL; leave empty for still-image talking head)
                <Input type="text" aria-label="Video URL" value={videoUrl} onChange={(e) => setVideoUrl(e.target.value)} placeholder="https://…/talking.mp4" />
              </label>
            ) : null}
            <label className="fld">Avatar look id (HeyGen; from the list above or Create result)
              <Input type="text" aria-label="Avatar look id" value={avatarId} onChange={(e) => setAvatarId(e.target.value)} placeholder="look_…" />
            </label>
            <label className="fld">Portrait image URL (still-image models such as InfiniteTalk)
              <Input type="text" aria-label="Portrait image URL" value={imageUrl} onChange={(e) => setImageUrl(e.target.value)} placeholder="https://…/portrait.png" />
            </label>
            <label className="fld">Audio URL (https)
              <Input type="text" aria-label="Audio URL" value={audioUrl} onChange={(e) => setAudioUrl(e.target.value)} placeholder="https://…/speech.mp3" />
            </label>
            {flow === 'animate' ? (
              <>
                <label className="fld">Script (HeyGen; use script or audio, not both)
                  <textarea className="rk-input" aria-label="Script" value={script} maxLength={5000} onChange={(e) => setScript(e.target.value)} placeholder="What the avatar should say…" />
                </label>
                <label className="fld">Voice id (optional, HeyGen)
                  <Input type="text" aria-label="Voice id" value={voiceId} onChange={(e) => setVoiceId(e.target.value)} />
                </label>
                <label className="fld">Resolution
                  <Select label="Resolution" value={resolution} onChange={setResolution} options={['480p', '720p', '1080p'].map((r) => ({ value: r, label: r }))} />
                </label>
              </>
            ) : null}
            <div className="av-actions">
              <Button type="button" disabled={prepBusy !== null} onClick={() => void prepare()}>{prepBusy === 'prepare' ? 'Preparing…' : 'Prepare request'}</Button>
            </div>
          </div>

          {prepError ? <div style={{ marginTop: 10 }}><Notice tone="bad">{prepError}</Notice></div> : null}
          {prep ? (
            <div style={{ marginTop: 10 }}><Notice tone="info" className="av-result">
              <b>{prep.route === 'api' ? 'Prepared request' : prep.route === 'rented' ? 'Rented route' : 'Local route'}</b>
              <Kv rows={[
                ['Route', prep.route] as [string, ReactNode],
                ['Model', prep.request ? `${prep.request.provider} / ${prep.request.model}` : (prep as { model?: string }).model ?? ''] as [string, ReactNode],
                ['Quality', qualityLabel(prep.quality)] as [string, ReactNode],
                ...(prep.est_usd != null ? ([['Estimate', `$${prep.est_usd}`] as [string, ReactNode]]) : []),
                ...(prep.ready === false ? ([['Readiness', 'needs an API key']] as [string, ReactNode][]) : [])
              ]} />
              {prep.warnings?.map((w) => <div key={w} className="brand-note">{w}</div>)}
              {prep.note ? <div className="brand-note">{prep.note}</div> : null}
              {prep.route === 'rented' ? (
                <div className="brand-note">
                  Stage or reconnect a worker under Models → Workers ({String(prep.submit_via)}).{prep.worker ? ` Staged: ${prep.worker.worker_id} (${prep.worker.state}).` : ''}
                </div>
              ) : null}
              {prep.request ? (
                <>
                  <pre className="selectable" style={{ margin: '8px 0', fontSize: 11, whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{JSON.stringify(prep.request, null, 2)}</pre>
                  <Button type="button" variant="primary" disabled={prepBusy !== null || prep.ready === false} onClick={() => void submit()}>{prepBusy === 'submit' ? 'Submitting…' : `Submit (${String(prep.submit_via)})`}</Button>
                </>
              ) : null}
            </Notice></div>
          ) : null}

          {job ? (
            <div style={{ marginTop: 10 }}><Card className="av-result">
              <b>Avatar job {job.jobId}</b>
              <Kv rows={[
                ['Status', job.status],
                ...(job.stageMessage ? [['Step', job.stageMessage] as [string, string]] : []),
                ...(job.errorMessage ? [['Error', job.errorMessage] as [string, string]] : [])
              ]} />
              {jobAsset ? <MediaPlayer src={assetUrl(jobAsset.path)} kind="video" title="Avatar output" /> : null}
              {job.status === 'failed' ? <div className="brand-note">Adjust the request and prepare again — nothing is retried automatically.</div> : null}
            </Card></div>
          ) : null}
        </div>

        <div className="recent" style={{ marginTop: 28 }}>
          <div className="toolbar" style={{ marginBottom: 4 }}>
            <h6 style={{ margin: 0, fontSize: 13, color: 'var(--ink)' }}>Routes</h6>
            <Button type="button" variant="ghost" size="sm" className="gal-clear" onClick={() => void loadRoutes()}>Refresh</Button>
          </div>
          {routesError ? <Notice tone="bad">{routesError}</Notice> : null}
          {routes ? (
            <>
              <p className="lede" style={{ marginTop: 0 }}>Selected route for avatar work: <b>{routes.selected?.route ?? 'auto'}</b>{routes.selected?.model ? ` · ${routes.selected.model}` : ''}. Picking a route here is what &quot;Auto&quot; submits through.</p>
              <div className="av-list">
                {(['api', 'local', 'rented'] as const).map((group) => (
                  <div key={group}>
                    {routes.routes[group].map((r) => (
                      <div className="av-row" key={`${group}-${r.provider ?? ''}-${r.model ?? r.id}`}>
                        <span>
                          <b>{r.label}</b>{' '}
                          <Badge>{r.route ?? group}</Badge>
                          <Badge tone={qualityTone(r.quality)}>{qualityLabel(r.quality)}</Badge>
                          {r.engine_kind ? <Badge tone={r.engine_kind === 'experimental' ? 'bad' : 'warn'}>{r.engine_kind}</Badge> : null}
                        </span>
                        <span className="sub">
                          {[r.provider, r.model ?? r.id, r.credential, r.readiness?.state, r.est_usd != null ? `~$${r.est_usd}` : null].filter(Boolean).join(' · ')}
                        </span>
                        <span className="acts">
                          <Button type="button" onClick={() => void setRoute(group, r.model ?? r.id)}>Use this route</Button>
                        </span>
                      </div>
                    ))}
                    {routes.routes[group].length === 0 ? <div className="sub" style={{ padding: '4px 0' }}>No {group} routes for {routes.capability === 'lip_sync' ? 'lip-sync' : 'avatars'}.</div> : null}
                  </div>
                ))}
              </div>
              {(routes.rented_workers ?? []).length > 0 ? (
                <p className="lede">Rented workers: {routes.rented_workers.map((w) => `${w.worker_id} (${w.state})`).join(', ')} — managed under Models → Workers.</p>
              ) : null}
              <p className="lede" style={{ fontSize: 12 }}>SoulX-FlashHead stays a rented/remote experiment: prior spike output was rejected for portrait crop and mouth/head motion, so its route is labelled unqualified. The local Wav2Lip route is free and runs on-device, but its model is research/non-commercial licensed — do not ship its output commercially. Accepted quality requires an operator review of real output.</p>
            </>
          ) : !routesError ? <EmptyState title="Loading routes…" /> : null}
        </div>

        <div className="recent" style={{ marginTop: 28 }}>
          <h6 style={{ margin: '0 0 4px', fontSize: 13, color: 'var(--ink)' }}>Reference evidence</h6>
          <p className="lede" style={{ marginTop: 0 }}>SoulX-FlashHead spike renders kept under <code>docs/spikes/</code>. They ran to completion but were rejected on quality — reference only, not accepted output.</p>
          <div className="av-list">
            {SPIKES.map((s) => (
              <div className="av-row" key={s.path}>
                <span><b>{s.label}</b> <Badge tone="bad">{s.quality}</Badge></span>
                <span className="sub selectable">{s.path}</span>
              </div>
            ))}
          </div>
        </div>
      </section>
    </div>
  );
}
