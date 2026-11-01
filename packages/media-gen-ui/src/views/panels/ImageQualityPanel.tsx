// P2 image quality: intent/brand routing, recipe picks, seed/steps/cfg knobs and
// the guarded preflight → approve flow (studio.recipes.*). An approved draft
// hands Generate the rendered prompt + engine params, so an approved recipe can
// run unattended; anything else stays gated until review.
import { useEffect, useMemo, useRef, useState } from 'react';
import type { StudioPanelProps } from './types.js';
import { errorText } from '../../lib/engine-error.js';
import {
  RECIPE_SLOTS,
  buildPreflightInput,
  guardReason,
  imageQuality,
  type ApproveResult,
  type ImageDraft,
  type IntentInfo,
  type LibrariesResult,
  type RecipeSlot
} from './image-quality.js';
import { usd } from './video-quality.js';
import { Badge, Button, Input, Notice, Pill, Select, Toggle } from '@rightkit/react-shell/react';
import { readUiSetting, writeUiSetting } from '../../lib/settings-backend.js';

export const ImageQualityPanelKinds: string[] = ['image'];

const GUARD_KEY = 'genright.image.guard';

const ASPECTS = ['1:1', '16:9', '9:16', '4:3', '3:4', '3:2', '2:3'];

export function ImageQualityPanel({ provider, model, prompt, setPrompt, setParams, setBlock, setAction }: StudioPanelProps) {
  const [enabled, setEnabled] = useState(true);
  useEffect(() => {
    let live = true;
    readUiSetting<boolean>(GUARD_KEY, true)
      .then((v) => live && setEnabled(v))
      .catch((e) => live && setLoadError(`Could not read saved setting: ${errorText(e)}`));
    return () => {
      live = false;
    };
  }, []);
  const [intents, setIntents] = useState<Record<string, IntentInfo>>({});
  const [brands, setBrands] = useState<string[]>([]);
  const [libs, setLibs] = useState<LibrariesResult | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [brand, setBrand] = useState('NA');
  const [intent, setIntent] = useState('mid');
  const [contract, setContract] = useState('');
  const [aspect, setAspect] = useState('1:1');
  const [projectSlug, setProjectSlug] = useState('');
  const [recipes, setRecipes] = useState<Partial<Record<RecipeSlot, string>>>({});
  const [seed, setSeed] = useState('');
  const [steps, setSteps] = useState('');
  const [guidance, setGuidance] = useState('');
  const [negative, setNegative] = useState('');

  const [draft, setDraft] = useState<ImageDraft | null>(null);
  const [approved, setApproved] = useState<ApproveResult | null>(null);
  const [busy, setBusy] = useState<'preflight' | 'approve' | null>(null);
  const [error, setError] = useState<string | null>(null);

  // The prompt the operator typed, kept apart from the rendered prompt pushed on approve.
  const basePrompt = useRef(prompt);
  if (!approved || prompt !== approved.generate.prompt) basePrompt.current = prompt;

  const blockRef = useRef(setBlock);
  const paramsRef = useRef(setParams);
  const actionRef = useRef(setAction);
  blockRef.current = setBlock;
  paramsRef.current = setParams;
  actionRef.current = setAction;

  useEffect(() => {
    let live = true;
    imageQuality
      .intents()
      .then((r) => {
        if (!live) return;
        setIntents(r.intents ?? {});
        setBrands(r.brands ?? []);
      })
      .catch((e) => live && setLoadError(errorText(e)));
    imageQuality
      .libraries()
      .then((r) => live && setLibs(r))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);

  // The gate applies to contract-backed cloud models; local sd.cpp profiles have
  // no prompt contract, so they pass through.
  const applies = provider !== null && provider !== 'local';
  const reason = useMemo(
    () => (applies ? guardReason({ enabled, draft, approved, prompt }) : null),
    [applies, enabled, draft, approved, prompt]
  );
  useEffect(() => {
    blockRef.current(reason);
  }, [reason]);
  useEffect(() => {
    paramsRef.current({ __panel: 'image-quality', ...(approved ? approved.generate.params : {}) });
  }, [approved]);
  // Surface the same preflight handler inline in the composer while the gate is blocking.
  // Depends only on the visible label state; the handler is read through a ref so the effect
  // does not re-fire (and re-set parent state) on every render.
  const preflightRef = useRef<() => void>(() => undefined);
  preflightRef.current = () => void runPreflight();
  const gateActive = Boolean(applies && enabled && reason);
  const gateLabel = busy === 'preflight' ? 'Checking…' : draft ? 'Run preflight again' : 'Run preflight';
  const gateDisabled = busy !== null;
  useEffect(() => {
    const set = actionRef.current;
    if (!set) return;
    set(gateActive ? { label: gateLabel, run: () => preflightRef.current(), disabled: gateDisabled } : null);
  }, [gateActive, gateLabel, gateDisabled]);
  useEffect(
    () => () => {
      blockRef.current(null);
      paramsRef.current({ __panel: 'image-quality' });
      actionRef.current?.(null);
    },
    []
  );

  function toggleGuard(on: boolean) {
    setEnabled(on);
    writeUiSetting(GUARD_KEY, on).catch((e) => setError(`Could not save setting: ${errorText(e)}`));
  }

  async function runPreflight() {
    setError(null);
    setApproved(null);
    setBusy('preflight');
    try {
      const input = buildPreflightInput(
        { brand, intent, contract, aspect, projectSlug, recipes, seed, steps, guidance, negative },
        basePrompt.current
      );
      const d = await imageQuality.preflight(input);
      if (d.ok === false || d.message) {
        setDraft(null);
        setError(d.message ?? 'Preflight failed');
      } else {
        setDraft(d);
      }
    } catch (e) {
      setDraft(null);
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function approve() {
    if (!draft) return;
    setError(null);
    setBusy('approve');
    try {
      if (!draft.request_hash) throw new Error('This draft carries no request hash to approve; run the preflight again.');
      const next = await imageQuality.approve(draft.draft_id, draft.request_hash);
      setApproved(next);
      setDraft(next.draft);
      setPrompt(next.generate.prompt);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const intentInfo = intents[intent];
  const contractSlug = contract || intentInfo?.model || '';
  const slotLibs = useMemo(() => (libs?.libraries ?? []).filter((l) => (RECIPE_SLOTS as readonly string[]).includes(l.slot)), [libs]);
  const contractRow = libs?.models.find((m) => m.slug === contractSlug);

  return (
    <section className="vq" aria-label="Image quality">
      <div className="vq-head">
        <div>
          <div className="eyebrow">Image quality</div>
          <small className="vq-count">{Object.keys(intents).length} intents · {slotLibs.length} recipe libraries</small>
        </div>
        <span className="vq-switch" title="Preflight → approve gate: an approved draft runs unattended; unapproved prompts stay blocked">
          <Toggle checked={enabled} onChange={toggleGuard} label="Quality gate" />
        </span>
      </div>
      {loadError ? <Notice tone="bad">{loadError}</Notice> : null}

      {!applies ? (
        <p className="vq-note">Quality gate covers contract-backed provider models. The selected local profile has no prompt contract, so it runs as typed.</p>
      ) : enabled ? (
        <div className="vq-guard">
          <div className="vq-row">
            <label className="vq-field">
              <span>Intent</span>
              <Select
                label="Intent"
                value={intent}
                onChange={(v) => { setIntent(v); setContract(''); }}
                options={Object.keys(intents).length ? Object.entries(intents).map(([id, i]) => ({ value: id, label: i.label ?? id })) : [{ value: 'mid', label: 'Mid' }]}
              />
              {intentInfo ? <small>{intentInfo.use} · {intentInfo.estimated_cost}</small> : null}
            </label>
            <label className="vq-field">
              <span>Brand</span>
              <Select
                label="Brand"
                value={brand}
                onChange={setBrand}
                options={brands.length ? brands.map((b) => ({ value: b, label: b })) : [{ value: 'NA', label: 'NA' }]}
              />
            </label>
          </div>
          <div className="vq-row">
            <label className="vq-field">
              <span>Contract</span>
              <Select
                label="Contract"
                value={contractSlug}
                onChange={(v) => setContract(v === intentInfo?.model ? '' : v)}
                options={[
                  ...[...new Set(Object.values(intents).map((i) => i.model).concat((libs?.models ?? []).map((m) => m.slug)))].map((m) => ({ value: m, label: `${m}${m === intentInfo?.model ? ' (intent route)' : ''}` })),
                  ...(!contractSlug ? [{ value: '', label: '—' }] : []),
                ]}
              />
              {contractRow ? <small>{contractRow.backend ?? '?'} backend{contractRow.reference_max ? ` · up to ${contractRow.reference_max} refs` : ''}</small> : null}
            </label>
            <label className="vq-field">
              <span>Aspect</span>
              <Select label="Aspect" value={aspect} onChange={setAspect} options={ASPECTS.map((a) => ({ value: a, label: a }))} />
            </label>
            <label className="vq-field">
              <span>Project slug</span>
              <Input value={projectSlug} placeholder="studio-session" onChange={(e) => setProjectSlug(e.target.value)} />
            </label>
          </div>

          {slotLibs.length ? (
            <details className="vq-details">
              <summary>Recipe picks <small>aesthetic, composition, lens, lighting, depth, color, skin</small></summary>
              <div className="vq-block">
                {slotLibs.map((lib) => (
                  <label className="vq-field" key={lib.id}>
                    <span>{lib.subcategory ?? lib.id}</span>
                    <Select
                      label={lib.subcategory ?? lib.id}
                      value={recipes[lib.slot as RecipeSlot] ?? ''}
                      onChange={(v) => setRecipes((r) => ({ ...r, [lib.slot]: v }))}
                      options={[{ value: '', label: 'Auto' }, ...lib.items.map((it) => ({ value: it.key, label: it.name ?? it.key }))]}
                    />
                    {lib.principle ? <small>{lib.principle}</small> : null}
                  </label>
                ))}
              </div>
            </details>
          ) : null}

          <details className="vq-details">
            <summary>Generation params <small>validated against the contract before spend</small></summary>
            <div className="vq-row three">
              <label className="vq-field"><span>Seed</span><Input value={seed} inputMode="numeric" placeholder="auto" onChange={(e) => setSeed(e.target.value)} /></label>
              <label className="vq-field"><span>Steps</span><Input value={steps} inputMode="numeric" placeholder="model" onChange={(e) => setSteps(e.target.value)} /></label>
              <label className="vq-field"><span>Guidance</span><Input value={guidance} inputMode="decimal" placeholder="model" onChange={(e) => setGuidance(e.target.value)} /></label>
            </div>
            <label className="vq-field">
              <span>Negative prompt</span>
              <Input value={negative} placeholder="compiled per model when supported" onChange={(e) => setNegative(e.target.value)} />
            </label>
          </details>

          <Button type="button" variant="ghost" className="vq-run" disabled={busy !== null} onClick={() => void runPreflight()}>
            {busy === 'preflight' ? 'Checking…' : draft ? 'Run preflight again' : 'Run preflight'}
          </Button>
          {error ? <Notice tone="bad">{error}</Notice> : null}

          {draft ? (
            <div className="vq-draft" data-state={reason ? 'blocked' : 'approved'}>
              <div className="vq-draft-head">
                <b>{approved ? 'Approved' : 'Draft ready'}</b>
                {reason ? <Pill tone="warn">not approved</Pill> : <Badge tone="ok">approved</Badge>}
              </div>
              <dl className="vq-kv">
                <dt>Estimate</dt><dd>{usd(draft.cost?.est_usd)}</dd>
                <dt>Contract</dt><dd>{String(draft.request.contract ?? '')}{draft.rendered?.pipeline ? ` · ${draft.rendered.pipeline}` : ''}</dd>
                <dt>Draft</dt><dd className="mono">{draft.draft_id}</dd>
              </dl>
              {draft.warnings?.map((w) => <Notice key={w} tone="warn">{w}</Notice>)}
              {(draft as { browser_mock?: boolean }).browser_mock ? <Notice tone="warn">Browser preview: the engine is not running, so this draft is a stand-in.</Notice> : null}
              <details className="vq-details" open>
                <summary>Prompt sent to the model</summary>
                <pre className="vq-pre">{draft.rendered?.prompt ?? ''}</pre>
              </details>
              {Object.keys(draft.rendered?.engine_params ?? {}).length ? (
                <details className="vq-details">
                  <summary>Engine params</summary>
                  <pre className="vq-pre">{JSON.stringify(draft.rendered?.engine_params, null, 2)}</pre>
                </details>
              ) : null}
              {!approved ? (
                <Button type="button" variant="primary" className="vq-approve" disabled={busy !== null} onClick={() => void approve()}>
                  {busy === 'approve' ? 'Approving…' : 'Approve this draft'}
                </Button>
              ) : null}
              {reason ? <p className="vq-note" role="status">{reason}</p> : <p className="vq-note" role="status">Prompt and parameters are locked into Generate.</p>}
            </div>
          ) : (
            <p className="vq-note" role="status">{reason}</p>
          )}
        </div>
      ) : (
        <p className="vq-note">The gate is off: Generate sends the prompt as typed. Saved approved recipes still run unattended — the gate only blocks unreviewed prompts.</p>
      )}
    </section>
  );
}
