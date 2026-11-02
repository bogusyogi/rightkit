import { useCallback, useEffect, useState } from 'react';
import { approveRecipeTemplateDraft, assetUrl, engineCommand, errorMessage, submitGeneration, type GenerateRequest, type RecipeApproveResult } from '../lib/api.js';
import { Card, Badge, Button, Checkbox, EmptyState, Input, Notice, Select } from '@rightkit/react-shell/react';
import { Kv, usd } from '../components/ui.js';
import { useFalUploadConfirm } from '../components/FalUploadConfirm.js';
import type { ViewId } from '../lib/nav.js';
import {
  alternatives,
  baseMedia,
  canApprove,
  canImport,
  canPreflight,
  importInput,
  isUrl,
  mediaTypeOf,
  preflightInput,
  routeLabel,
  runRequest,
  slotProblems,
  uploadPaths,
  usableRoutes,
  type SwapPreflight,
  type SwapTemplate
} from './template-swap-logic.js';
import { hostAppName } from "../appName.js";

/** Template gallery and swap runner (#269). Estimate, route and consent are shown before anything is approved or sent. */
export function TemplateSwap({ onNavigate }: { onNavigate: (view: ViewId) => void }) {
  const [templates, setTemplates] = useState<SwapTemplate[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [source, setSource] = useState('');
  const [name, setName] = useState('');
  const [prompt, setPrompt] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setTemplates((await engineCommand<{ templates: SwapTemplate[] }>('templates.list', {})).templates);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      await engineCommand('templates.import', importInput(source, name, prompt));
      setSource('');
      setName('');
      setPrompt('');
      await load();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: string) => {
    setError(null);
    try {
      await engineCommand('templates.delete', { id });
      if (selected === id) setSelected(null);
      await load();
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  const current = templates?.find((t) => t.id === selected) ?? null;
  if (current) return <SwapScreen template={current} onBack={() => setSelected(null)} onNavigate={onNavigate} />;

  return (
    <div>
      <div className="eyebrow">Create</div>
      <h2>Template swap</h2>
      <p className="lede">Save a scene once, then swap your own images into it. Nothing is sent until you approve the estimate.</p>
      {error ? <Notice tone="bad">{error}</Notice> : null}

      <Card flush className="group-box gb-mb16">
        <div className="setting" style={{ borderTop: 0 }}><b style={{ fontSize: 12.5 }}>New template</b></div>
        <div style={{ padding: '0 12px 12px', display: 'grid', gap: 8 }}>
          <Input aria-label="Base media file path or URL" placeholder="A local image or video file, or an https:// link" value={source} onChange={(e) => setSource(e.target.value)} spellCheck={false} disabled={busy} />
          {isUrl(source) ? <p style={{ fontSize: 11.5, margin: 0 }}>Links need {hostAppName()}; if it is not available {hostAppName()} says so and you can import a local file instead.</p> : null}
          <Input aria-label="Template name" placeholder="Name (optional)" value={name} onChange={(e) => setName(e.target.value)} disabled={busy} />
          <textarea className="rk-input" aria-label="Swap prompt" placeholder="What to swap, e.g. Replace the person with the new subject." value={prompt} onChange={(e) => setPrompt(e.target.value)} rows={2} disabled={busy} />
          <div><Button type="button" variant="primary" disabled={busy || !canImport(source, prompt)} onClick={create}>Create template</Button></div>
        </div>
      </Card>

      {!templates && !error ? <EmptyState title="Loading templates…" /> : null}
      {templates && templates.length === 0 ? <EmptyState title="No templates yet">Create one above from a file.</EmptyState> : null}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))', gap: 12 }}>
        {(templates ?? []).map((t) => (
          <div key={t.id} className="group-box" style={{ padding: 8 }}>
            <Button type="button" variant="ghost" onClick={() => setSelected(t.id)} aria-label={`Open ${t.name}`} style={{ display: 'block', width: '100%', height: 'auto', padding: 0, textAlign: 'left' }}>
              {t.kind === 'video'
                ? <video src={assetUrl(baseMedia(t))} muted preload="metadata" style={{ width: '100%', aspectRatio: '4 / 3', objectFit: 'cover', borderRadius: 6 }} />
                : <img src={assetUrl(baseMedia(t))} alt="" style={{ width: '100%', aspectRatio: '4 / 3', objectFit: 'cover', borderRadius: 6 }} />}
              <div style={{ marginTop: 6, fontSize: 12.5 }}><b>{t.name}</b></div>
            </Button>
            <div style={{ display: 'flex', gap: 6, alignItems: 'center', marginTop: 4 }}>
              <Badge>{t.kind}</Badge>
              <span style={{ marginLeft: 'auto' }}><Button type="button" variant="secondary" onClick={() => remove(t.id)} aria-label={`Delete ${t.name}`}>Delete</Button></span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function SwapScreen({ template, onBack, onNavigate }: { template: SwapTemplate; onBack: () => void; onNavigate: (view: ViewId) => void }) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [route, setRoute] = useState('');
  const [pre, setPre] = useState<SwapPreflight | null>(null);
  const [consent, setConsent] = useState(false);
  const [approved, setApproved] = useState<RecipeApproveResult | null>(null);
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [attempted, setAttempted] = useState(false);
  const fal = useFalUploadConfirm();

  const reset = () => {
    setPre(null);
    setConsent(false);
    setApproved(null);
    setSubmitted(false);
    setError(null);
  };
  const setValue = (n: string, v: string) => {
    setValues((cur) => ({ ...cur, [n]: v }));
    reset();
  };

  const guard = async (fn: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const preflight = () => {
    setAttempted(true);
    if (!canPreflight(template.slots, values)) return;
    void guard(async () => {
      setApproved(null);
      setConsent(false);
      setPre(await engineCommand<SwapPreflight>('templates.swap_preflight', preflightInput(template.id, template.slots, values, route)));
    });
  };

  const approve = () => {
    const draft = pre?.draft;
    if (!draft) return;
    void guard(async () => setApproved(await approveRecipeTemplateDraft(draft.draft_id, draft.request_hash)));
  };

  const run = () => {
    if (!approved) return;
    const request = runRequest(approved.generate as unknown as Record<string, unknown>, pre, consent) as unknown as GenerateRequest;
    void guard(async () => {
      await submitGeneration({ ...request, request_id: crypto.randomUUID() });
      setSubmitted(true);
    });
  };

  const problems = slotProblems(template.slots, values);
  const uploads = uploadPaths(pre);
  const alts = alternatives(pre);

  return (
    <div>
      <Button type="button" variant="secondary" onClick={onBack}>All templates</Button>
      <h2 style={{ marginTop: 8 }}>{template.name}</h2>
      <p className="lede">{template.prompt}</p>

      {template.slots.map((s) => {
        const p = problems.find((x) => x.name === s.name);
        const v = values[s.name] ?? '';
        return (
          <div key={s.name} style={{ display: 'grid', gap: 4, marginBottom: 10 }}>
            <div style={{ display: 'flex', gap: 6, alignItems: 'baseline' }}>
              <b style={{ fontSize: 12.5 }}>{s.label}</b><Badge>{s.type}</Badge><Badge>{s.required ? 'required' : 'optional'}</Badge>
            </div>
            <Input aria-label={`${s.label} file path or URL`} placeholder={s.type === 'video' ? 'A video file, or an https:// link' : 'An image file, or an https:// link'} value={v} onChange={(e) => setValue(s.name, e.target.value)} disabled={busy} spellCheck={false} />
            {v && mediaTypeOf(v) === 'image' ? <img src={assetUrl(v)} alt="" style={{ maxHeight: 90, maxWidth: 160, objectFit: 'cover', borderRadius: 6 }} /> : null}
            {p && (attempted || v) ? <Notice tone="bad">{p.text}</Notice> : null}
          </div>
        );
      })}

      {pre?.ok ? (
        <div style={{ marginBottom: 10 }}>
          <label style={{ fontSize: 11.5 }}>Route</label>
          <Select label="Route" value={route} disabled={busy} onChange={(v) => { setRoute(v); reset(); }} options={[{ value: '', label: 'Cheapest usable route (default)' }, ...usableRoutes(pre).map((r) => ({ value: r, label: r }))]} />
        </div>
      ) : null}

      <div style={{ marginBottom: 12 }}>
        <Button type="button" variant="secondary" disabled={busy} onClick={preflight}>Estimate cost</Button>
      </div>

      {error ? <Notice tone="bad">{error}</Notice> : null}
      {pre && pre.ok === false ? (
        <Notice tone="bad">
          <p style={{ margin: 0 }}>{pre.message ?? 'No route can serve this template with these slots.'}</p>
          {pre.considered.map((c) => <div key={c.route} style={{ fontSize: 12 }}>{c.route}: {c.reason ?? 'unusable'}</div>)}
        </Notice>
      ) : null}

      {pre?.ok ? (
        <Card flush className="group-box gb-p12">
          <Kv rows={[['Route', routeLabel(pre)], ['Estimated cost', usd(pre.est_usd)]]} />
          {alts.length ? (
            <div style={{ marginTop: 8, fontSize: 12 }}>
              <div style={{ color: 'var(--ink-3)', marginBottom: 2 }}>Other routes considered</div>
              {alts.map((c) => (
                <div key={c.route}>{c.route}: {c.usable ? usd(c.est_usd) : `not usable (${c.reason ?? 'unavailable'})`}</div>
              ))}
            </div>
          ) : null}
          {uploads.length ? (
            <div style={{ marginTop: 10, fontSize: 12 }}>
            <Notice tone="info">
              <div>These local files are uploaded to your own fal storage for this run only:</div>
              <ul style={{ margin: '4px 0 6px 16px' }}>{uploads.map((p) => <li key={p} className="mono">{p}</li>)}</ul>
              <Checkbox checked={consent} disabled={busy} onChange={setConsent} label="I agree to upload these files to my fal storage for this run." />
            </Notice>
            </div>
          ) : null}
          <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
            {!approved ? (
              <Button type="button" variant="primary" disabled={busy || !canApprove(pre, consent)} onClick={approve}>Approve {usd(pre.est_usd)}</Button>
            ) : (
              <Button type="button" variant="primary" disabled={busy || submitted} onClick={run}>{submitted ? 'Submitted' : 'Run swap'}</Button>
            )}
          </div>
          {submitted ? <p style={{ marginTop: 8 }}>Submitted. <Button type="button" variant="secondary" onClick={() => onNavigate('gallery')}>Open Gallery</Button></p> : null}
        </Card>
      ) : null}
    </div>
  );
}
