// P2 video quality: shot-contract templates, camera-move and ad-pattern pickers,
// recipe libraries, and the guarded preflight → approved run (rule: no I2V run
// without a validated shot contract and storyboard start/end frames).
import { useEffect, useMemo, useRef, useState } from 'react';
import './video.css';
import type { StudioPanelProps } from './types';
import { errorText } from '../../lib/engine-error';
import {
  ASPECTS,
  RECIPE_PICKERS,
  VENTURES,
  appendPromptFragment,
  approvedParams,
  beatLine,
  buildPreflightInput,
  groupByCategory,
  guardReason,
  parseOverrides,
  usd,
  videoQuality,
  type Draft,
  type Recipe,
  type RecipeField,
  type RecipeLibrary,
  type TemplateInfo,
  type Venture
} from './video-quality';
import { Badge, Button, Checkbox, Input, Notice, Pill, Select as RkSelect, Toggle } from '@rightkit/react-shell/react';
import { readUiSetting, writeUiSetting } from '../../lib/settings-backend';

export const VideoShotPanelKinds: string[] = ['video'];

const GUARD_KEY = 'genright.video.guard';

function Select({ label, value, onChange, children, options, hint }: { label: string; value: string; onChange: (v: string) => void; children?: React.ReactNode; options?: { value: string; label: string }[]; hint?: React.ReactNode }) {
  return (
    <label className="vq-field">
      <span>{label}</span>
      {options
        ? <RkSelect label={label} value={value} onChange={onChange} options={options} />
        : <select className="rk-select" value={value} onChange={(e) => onChange(e.target.value)}>{children}</select>}
      {hint ? <small>{hint}</small> : null}
    </label>
  );
}

export function RecipeOptions({ items }: { items: Recipe[] }) {
  const groups = groupByCategory(items);
  if (groups.length === 1 && !groups[0].key) return <>{items.map((r) => <option key={r.slug} value={r.slug}>{r.name}</option>)}</>;
  return (
    <>
      {groups.map((g) => (
        <optgroup key={g.key} label={g.label}>
          {g.items.map((r) => <option key={r.slug} value={r.slug}>{r.name}</option>)}
        </optgroup>
      ))}
    </>
  );
}

export function VideoShotPanel({ provider, model, prompt, setPrompt, setParams, setBlock, setAction }: StudioPanelProps) {
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
  const [moves, setMoves] = useState<Recipe[]>([]);
  const [patterns, setPatterns] = useState<Recipe[]>([]);
  const [libs, setLibs] = useState<Partial<Record<string, RecipeLibrary>>>({});
  const [loadError, setLoadError] = useState<string | null>(null);

  const [venture, setVenture] = useState<Venture>('HR');
  const [templates, setTemplates] = useState<TemplateInfo[]>([]);
  const [template, setTemplate] = useState('');
  const [overridesText, setOverridesText] = useState('');
  const [projectSlug, setProjectSlug] = useState('');
  const [cameraMove, setCameraMove] = useState('');
  const [adPattern, setAdPattern] = useState('');
  const [recipes, setRecipes] = useState<Partial<Record<RecipeField, string>>>({});
  const [locks, setLocks] = useState<string[]>([]);
  const [startFrame, setStartFrame] = useState('');
  const [endFrame, setEndFrame] = useState('');
  const [aspect, setAspect] = useState<string>('9:16');
  const [duration, setDuration] = useState('4');
  const [resolution, setResolution] = useState('');
  const [cinema, setCinema] = useState(false);

  const [draft, setDraft] = useState<Draft | null>(null);
  const [busy, setBusy] = useState<'preflight' | 'approve' | null>(null);
  const [error, setError] = useState<string | null>(null);

  // The prompt the operator typed, kept apart from the contract prompt Generate shows after approval.
  const basePrompt = useRef(prompt);
  if (!draft || prompt !== draft.engine_request.prompt) basePrompt.current = prompt;

  const blockRef = useRef(setBlock);
  const paramsRef = useRef(setParams);
  const actionRef = useRef(setAction);
  blockRef.current = setBlock;
  paramsRef.current = setParams;
  actionRef.current = setAction;

  useEffect(() => {
    let live = true;
    Promise.all([videoQuality.cameraMoves(), videoQuality.adPatterns()])
      .then(([cm, ap]) => {
        if (!live) return;
        setMoves(cm.moves);
        setPatterns(ap.patterns);
      })
      .catch((e) => live && setLoadError(errorText(e)));
    for (const picker of [...RECIPE_PICKERS.map((p) => p.library), 'continuity']) {
      videoQuality.recipes(picker).then((lib) => live && setLibs((all) => ({ ...all, [picker]: lib }))).catch(() => undefined);
    }
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    let live = true;
    setTemplate('');
    videoQuality
      .templates(venture)
      .then((r) => live && setTemplates(r.templates))
      .catch((e) => {
        if (live) {
          setTemplates([]);
          setLoadError(errorText(e));
        }
      });
    return () => {
      live = false;
    };
  }, [venture]);

  // Generate stays blocked until the draft in view is approved and still matches what would be sent.
  const reason = useMemo(() => guardReason({ enabled, draft, provider, model, prompt }), [enabled, draft, provider, model, prompt]);
  useEffect(() => {
    blockRef.current(reason);
  }, [reason]);
  const approved = enabled && draft && !reason ? draft : null;
  useEffect(() => {
    paramsRef.current({ __panel: 'video', ...(approved ? approvedParams(approved) : {}) });
  }, [approved]);
  // Surface the same preflight handler inline in the composer while the gate is blocking.
  // Depends only on the visible label state; the handler is read through a ref so the effect
  // does not re-fire (and re-set parent state) on every render.
  const preflightRef = useRef<() => void>(() => undefined);
  preflightRef.current = () => void runPreflight();
  const gateActive = Boolean(enabled && reason);
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
      paramsRef.current({ __panel: 'video' });
      actionRef.current?.(null);
    },
    []
  );

  const overrides = parseOverrides(overridesText);
  const current = (field: RecipeField) => recipes[field] ?? '';
  const move = moves.find((m) => m.slug === cameraMove);
  const pattern = patterns.find((p) => p.slug === adPattern);
  const tpl = templates.find((t) => t.slug === template);

  function toggleGuard(on: boolean) {
    setEnabled(on);
    writeUiSetting(GUARD_KEY, on).catch((e) => setError(`Could not save setting: ${errorText(e)}`));
  }

  async function runPreflight() {
    setError(null);
    if (!overrides.ok) {
      setError(overrides.error);
      return;
    }
    setBusy('preflight');
    try {
      const input = buildPreflightInput({
        venture,
        projectSlug,
        provider,
        model,
        prompt: basePrompt.current,
        template,
        overrides: overrides.value,
        cameraMove,
        adPattern,
        recipes,
        continuityLocks: locks,
        startFrame,
        endFrame,
        aspect,
        duration,
        resolution,
        cinemaStudio: cinema
      });
      setDraft(await videoQuality.preflight(input));
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
      if (!draft.fingerprint) throw new Error('This draft carries no fingerprint to approve; run the preflight again.');
      const next = await videoQuality.approve(draft.draft_id, draft.fingerprint);
      setDraft(next);
      setPrompt(next.engine_request.prompt);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const defaults = draft?.request.composer_meta?.defaults_used ?? {};

  return (
    <section className="vq" aria-label="Video shot quality">
      <div className="vq-head">
        <div>
          <div className="eyebrow">Shot quality</div>
          <small className="vq-count">{moves.length} camera moves · {patterns.length} ad patterns · {templates.length} templates</small>
        </div>
        <span className="vq-switch" title="Rule 1 and the shot_contract gate: no image-to-video run without a validated contract and storyboard frames">
          <Toggle checked={enabled} onChange={toggleGuard} label="Shot guard" />
        </span>
      </div>
      {loadError ? <Notice tone="bad">{loadError}</Notice> : null}

      <div className="vq-block">
        <Select
          label="Camera move"
          value={cameraMove}
          onChange={setCameraMove}
          hint={move ? <>{move.description}{move.good_for_anchored_flf ? ' · safe with first/last frames' : ''}{move.warning ? ` · ${move.warning}` : ''}</> : 'Pick one move per shot.'}
        >
          <option value="">{moves.length ? 'Choose camera move' : 'Loading camera moves'}</option>
          <RecipeOptions items={moves} />
        </Select>
        {move ? <Button type="button" variant="ghost" onClick={() => setPrompt(appendPromptFragment(prompt, move.prompt_fragment))}>Add move to prompt</Button> : null}

        <Select
          label="Ad pattern"
          value={adPattern}
          onChange={setAdPattern}
          hint={pattern ? <>{pattern.description}<br />{beatLine(pattern)}</> : 'Pick the pattern before storyboarding.'}
        >
          <option value="">{patterns.length ? 'Choose ad pattern' : 'Loading ad patterns'}</option>
          <RecipeOptions items={patterns} />
        </Select>
        {pattern ? <Button type="button" variant="ghost" onClick={() => setPrompt(appendPromptFragment(prompt, pattern.prompt_fragment))}>Add pattern to prompt</Button> : null}
      </div>

      <details className="vq-details">
        <summary>Recipes <small>aesthetic, motion, transition, text, audio, QA, continuity</small></summary>
        <div className="vq-block">
          {RECIPE_PICKERS.map(({ field, library, label }) => {
            const lib = libs[library];
            const picked = lib?.items.find((r) => r.slug === current(field));
            return (
              <Select
                key={field}
                label={label}
                value={current(field)}
                onChange={(v) => setRecipes((all) => ({ ...all, [field]: v }))}
                hint={picked ? picked.description : lib ? `${lib.count} available` : 'Loading'}
              >
                <option value="">{lib ? 'None' : 'Loading'}</option>
                {lib ? <RecipeOptions items={lib.items} /> : null}
              </Select>
            );
          })}
          <fieldset className="vq-locks">
            <legend>Continuity locks</legend>
            {(libs.continuity?.items ?? []).map((l) => (
              <span key={l.slug} title={l.description}>
                <Checkbox checked={locks.includes(l.slug)} onChange={(on) => setLocks((all) => (on ? [...all, l.slug] : all.filter((x) => x !== l.slug)))} label={l.name} />
              </span>
            ))}
          </fieldset>
        </div>
      </details>

      {enabled ? (
        <div className="vq-guard">
          <div className="vq-row">
            <Select label="Venture" value={venture} onChange={(v) => setVenture(v as Venture)} options={VENTURES.map((v) => ({ value: v, label: v }))} />
            <label className="vq-field">
              <span>Project slug</span>
              <Input value={projectSlug} placeholder="spring-launch" onChange={(e) => setProjectSlug(e.target.value)} />
            </label>
          </div>
          <Select label="Shot contract template" value={template} onChange={setTemplate} hint={tpl ? tpl.summary : 'A contract is required: no run without one.'} options={[{ value: '', label: 'Choose template' }, ...templates.map((t) => ({ value: t.slug, label: `${t.name}${t.source === 'content' ? ' (Content)' : ''}` }))]} />
          <details className="vq-details">
            <summary>Contract overrides <small>JSON, merged onto the template</small></summary>
            <textarea className="rk-input vq-json" spellCheck={false} value={overridesText} placeholder={'{"role_blocking": {"gaze": "fixed on the product"}}'} onChange={(e) => setOverridesText(e.target.value)} />
            {!overrides.ok ? <Notice tone="bad">{overrides.error}</Notice> : null}
          </details>
          <label className="vq-field">
            <span>Start frame URL</span>
            <Input value={startFrame} placeholder="https://… storyboard start" onChange={(e) => setStartFrame(e.target.value)} />
          </label>
          <label className="vq-field">
            <span>End frame URL</span>
            <Input value={endFrame} placeholder="https://… storyboard end" onChange={(e) => setEndFrame(e.target.value)} />
            <small>Rule 1: anchor both frames. Models that take no end frame say so below.</small>
          </label>
          <div className="vq-row three">
            <Select label="Aspect" value={aspect} onChange={setAspect} options={ASPECTS.map((a) => ({ value: a, label: a }))} />
            <label className="vq-field">
              <span>Seconds</span>
              <Input value={duration} inputMode="decimal" onChange={(e) => setDuration(e.target.value)} />
            </label>
            <label className="vq-field">
              <span>Resolution</span>
              <Input value={resolution} placeholder="model" onChange={(e) => setResolution(e.target.value)} />
            </label>
          </div>
          <Checkbox className="vq-check" checked={cinema} onChange={setCinema} label="Cinema Studio layout for Kling (generate twice, pick the better take)" />
          <Button type="button" variant="ghost" className="vq-run" disabled={!model || busy !== null} onClick={() => void runPreflight()}>
            {busy === 'preflight' ? 'Checking…' : draft ? 'Run preflight again' : 'Run preflight'}
          </Button>
          {error ? <Notice tone="bad">{error}</Notice> : null}

          {draft ? (
            <div className="vq-draft" data-state={reason ? 'blocked' : 'approved'}>
              <div className="vq-draft-head">
                <b>{draft.status === 'approved' ? 'Approved' : 'Draft ready'}</b>
                {reason ? <Pill tone="warn">not approved</Pill> : <Badge tone="ok">approved</Badge>}
              </div>
              <dl className="vq-kv">
                <dt>Estimate</dt><dd>{usd(draft.estimated_cost_usd)}</dd>
                <dt>Renderer</dt><dd>{draft.route.family} · {draft.route.ref_kind}</dd>
                <dt>Draft</dt><dd className="mono">{draft.draft_id}</dd>
              </dl>
              {Object.keys(defaults).length ? (
                <p className="vq-note">
                  Defaults applied: {Object.entries(defaults).map(([k, v]) => `${k.replace('_', ' ')} ${v.key} (${v.source.replace('_', ' ')})`).join(', ')}.
                </p>
              ) : null}
              {draft.warnings.map((w) => <Notice key={w} tone="warn">{w}</Notice>)}
              {draft.browser_mock ? <Notice tone="warn">Browser preview: the engine is not running, so this draft is a stand-in.</Notice> : null}
              <details className="vq-details" open>
                <summary>Prompt sent to the model <small>contract, {draft.route.family} layout</small></summary>
                <pre className="vq-pre">{draft.contract_prompt}</pre>
              </details>
              <details className="vq-details">
                <summary>Composed storyboard prompt <small>camera, pattern, aesthetic</small></summary>
                <pre className="vq-pre">{draft.composed_prompt}</pre>
              </details>
              {draft.approval.approved ? null : (
                <Button type="button" variant="primary" className="vq-approve" disabled={busy !== null} onClick={() => void approve()}>
                  {busy === 'approve' ? 'Approving…' : 'Approve this draft'}
                </Button>
              )}
              {reason ? <p className="vq-note" role="status">{reason}</p> : <p className="vq-note" role="status">Prompt and parameters are locked into Generate.</p>}
            </div>
          ) : (
            <p className="vq-note" role="status">{reason}</p>
          )}
        </div>
      ) : (
        <p className="vq-note">The guard is off: Generate sends the prompt as typed, with no contract or frame check.</p>
      )}
    </section>
  );
}
