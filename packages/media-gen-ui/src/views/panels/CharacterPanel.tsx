// P3 characters: pick/create a character, attach its LoRA (or fall back to its
// reference images), stack a vetted aesthetic LoRA, and push the locked params
// into Generate. The training section is preflight-first: train_preflight shows
// dataset size, predicted cost and blockers; starting a training goes through the
// app's approve_draft (an app-minted token over the exact plan), so nothing spends without the
// user's click on a plan they were shown.
import { useEffect, useMemo, useRef, useState } from 'react';
import './video.css';
import type { StudioPanelProps } from './types';
import {
  characters,
  lockInconsistencies,
  lockPriceUnconfirmed,
  lockPrimaryDiagnostics,
  lockRefusal,
  lockSwitchSentence,
  trainErrorText,
  type AestheticLora,
  type CharacterSummary,
  type LockRefusal,
  type LockResult,
  type TrainPlan,
  type TrainStatus,
  type TrainersResult
} from './character-lock';
import { UNCONFIRMED_PRICE_LABEL, UNCONFIRMED_PRICE_SENTENCE } from '../../lib/gen-params';
import { Badge, Button, Checkbox, Input, Notice, Pill, Select } from '@rightkit/react-shell/react';
import { usd } from './video-quality';
import { OUTCOME_UNKNOWN, errorCode, errorDetails, errorText, isHeldJob } from '../../lib/api';
import {
  heldLabel,
  trainingAmount,
  trainingHeldActionLabel,
  trainingHeldActions,
  trainingHeldConfirmLabel,
  trainingHeldConsequence,
  trainingHeldExplanation,
  trainingHeldSince,
  trainingHeldTitle,
  type TrainingResolve
} from '../../lib/held-job';
import { useConfirm } from '../../lib/confirm';

export const CharacterPanelKinds: string[] = ['image', 'video'];

export function CharacterPanel({ kind, provider, model, setParams }: StudioPanelProps) {
  const [list, setList] = useState<CharacterSummary[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [characterId, setCharacterId] = useState('');
  const [loras, setLoras] = useState<AestheticLora[]>([]);
  const [aestheticId, setAestheticId] = useState('');
  const [fallbackRefs, setFallbackRefs] = useState(true);
  const [lock, setLock] = useState<LockResult | null>(null);
  const [lockModel, setLockModel] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A refused lock, in plain words (the code sits in a Details disclosure).
  const [refusal, setRefusal] = useState<LockRefusal | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  // Create form
  const [newId, setNewId] = useState('');
  const [newVenture, setNewVenture] = useState('');
  const [newDesc, setNewDesc] = useState('');
  const [newIdentifiable, setNewIdentifiable] = useState(false);
  const [newConsent, setNewConsent] = useState(false);

  // Reference import
  const [refPath, setRefPath] = useState('');
  const [refKind, setRefKind] = useState<'reference' | 'training'>('reference');

  // Training
  const [trainers, setTrainers] = useState<TrainersResult | null>(null);
  const [trainer, setTrainer] = useState('');
  const [steps, setSteps] = useState('');
  const [cap, setCap] = useState('');
  const [plan, setPlan] = useState<TrainPlan | null>(null);
  const [status, setStatus] = useState<TrainStatus | null>(null);

  const paramsRef = useRef(setParams);
  paramsRef.current = setParams;
  const confirm = useConfirm();

  const refresh = async () => {
    const r = await characters.list();
    setList(r.characters);
    return r.characters;
  };

  useEffect(() => {
    let live = true;
    characters
      .list()
      .then((r) => {
        if (!live) return;
        setList(r.characters);
        setCharacterId((cur) => cur || r.characters[0]?.id || '');
      })
      .catch((e) => live && setLoadError(errorText(e)));
    characters.trainers().then((t) => {
      if (!live) return;
      setTrainers(t);
      setTrainer(t.default_trainer);
      setSteps(String(t.default_steps));
      setCap(String(t.default_cap_usd));
    }).catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);

  const current = list.find((c) => c.id === characterId) ?? null;
  const brand = current?.brand;

  useEffect(() => {
    let live = true;
    characters
      .loras(brand)
      .then((r) => live && setLoras(r.aesthetic_loras))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [brand]);

  const modelKey = provider && model ? `${provider}/${model}` : null;
  const stale = lock !== null && lockModel !== modelKey;

  const ventures = useMemo(() => [...new Set(list.map((c) => c.venture))], [list]);

  async function applyLock() {
    setError(null);
    setRefusal(null);
    setBusy('lock');
    try {
      const res = await characters.lock({
        kind,
        provider: provider ?? '',
        model: model ?? '',
        ...(characterId ? { character_id: characterId } : {}),
        ...(aestheticId ? { aesthetic_lora_id: aestheticId } : {}),
        fallback_to_references: fallbackRefs,
        upload: false
      });
      setLock(res);
      setLockModel(modelKey);
      paramsRef.current({ __panel: 'character', ...res.params });
    } catch (e) {
      setRefusal(lockRefusal(e));
    } finally {
      setBusy(null);
    }
  }

  function clearLock() {
    setLock(null);
    setLockModel(null);
    setRefusal(null);
    paramsRef.current({ __panel: 'character' });
  }

  async function createCharacter() {
    setError(null);
    setBusy('create');
    try {
      const r = await characters.create({
        character_id: newId.trim(),
        venture: (newVenture.trim() || 'na').toLowerCase(),
        description: newDesc.trim(),
        identifiable: newIdentifiable,
        consent: newConsent
      });
      await refresh();
      setCharacterId(r.character.id);
      setNewId('');
      setNewDesc('');
      setNewConsent(false);
      setNewIdentifiable(false);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function addImage() {
    if (!refPath.trim()) return;
    setError(null);
    setBusy('image');
    try {
      await characters.addImage({
        path: refPath.trim(),
        kind: refKind,
        ...(refKind === 'training' || characterId ? { character_id: characterId } : {})
      });
      setRefPath('');
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function preflightTrain() {
    if (!current) return;
    setError(null);
    setPlan(null);
    setBusy('trainpreflight');
    try {
      setPlan(await characters.trainPreflight({
        character_id: current.id,
        trainer,
        steps: Number(steps) || undefined,
        cap_usd: Number(cap) || undefined
      }));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function startTraining() {
    if (!current || !plan) return;
    setError(null);
    setBusy('train');
    try {
      // Only the app can approve a paid training: this click mints the token over this exact plan.
      const s = await characters.train(plan, {
        character_id: current.id,
        trainer,
        steps: plan.steps,
        cap_usd: plan.cap_usd
      });
      setStatus(s);
    } catch (e) {
      // The engine holds a submit that may have reached fal and names the training in the error: load its record.
      const heldId = errorCode(e) === OUTCOME_UNKNOWN ? errorDetails(e)?.training_id : undefined;
      if (typeof heldId === 'string' && heldId) {
        setPlan(null);
        try {
          setStatus(await characters.trainStatus({ training_id: heldId }));
        } catch (inner) {
          setError(errorText(inner));
        }
      } else {
        setError(errorText(e));
      }
    } finally {
      setBusy(null);
    }
  }

  async function checkTraining() {
    if (!current) return;
    try {
      setStatus(await characters.trainStatus(status?.training_id ? { training_id: status.training_id } : { character_id: current.id }));
    } catch (e) {
      setError(errorText(e));
    }
  }

  /** Resolve a held training: `release` when nothing started at fal, `abandon` when it ran or may have. */
  async function resolveHeldTraining(action: TrainingResolve) {
    const held = status;
    if (!held || !isHeldJob(held.status)) return;
    const ok = await confirm({
      title: trainingHeldTitle(action),
      paragraphs: [trainingHeldConsequence(action, held.reserved_usd)],
      confirmLabel: trainingHeldConfirmLabel(action),
      danger: action === 'abandon'
    });
    if (!ok) return;
    setError(null);
    setBusy('resolve');
    try {
      setStatus(await characters.trainStatus({ training_id: held.training_id, resolve: action }));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const usableLoras = loras.filter((l) => l.usable !== false);
  const switchSentence = lock ? lockSwitchSentence(lock) : null;
  const priceUnconfirmed = lock ? lockPriceUnconfirmed(lock) : false;
  const inconsistencies = lock ? lockInconsistencies(lock) : [];

  return (
    <section className="vq" aria-label="Characters and LoRA">
      <div className="vq-head">
        <div>
          <div className="eyebrow">Characters</div>
          <small className="vq-count">{list.length} characters · {loras.length} aesthetic LoRAs</small>
        </div>
      </div>
      {loadError ? <Notice tone="bad">{loadError}</Notice> : null}
      {error ? <Notice tone="bad">{error}</Notice> : null}
      {refusal ? (
        <Notice tone="bad">
          <div>{refusal.sentence}</div>
          <details className="vq-details">
            <summary>Details</summary>
            <dl className="vq-kv">
              <dt>Code</dt><dd className="mono">{refusal.code ?? 'none'}</dd>
              <dt>Engine message</dt><dd>{refusal.technical}</dd>
            </dl>
          </details>
        </Notice>
      ) : null}

      <div className="vq-block">
        <label className="vq-field">
          <span>Character</span>
          <select className="rk-select" value={characterId} onChange={(e) => { setCharacterId(e.target.value); setPlan(null); }}>
            <option value="">None</option>
            {ventures.map((v) => (
              <optgroup key={v} label={v.toUpperCase()}>
                {list.filter((c) => c.venture === v).map((c) => (
                  <option key={c.id} value={c.id}>{c.character_id}{c.has_lora ? ' · LoRA' : ` · ${c.refs_count} refs`}</option>
                ))}
              </optgroup>
            ))}
          </select>
          {current ? (
            <small>
              {current.description || current.id} · {current.brand}
              {current.has_lora ? ` · LoRA on ${current.lora_base_model ?? 'unknown base'} @${current.lora_scale}` : ` · ${current.refs_count} reference image(s)`}
              {current.identifiable ? ` · ${current.has_consent ? 'consent on file' : 'identifiable — consent needed to train'}` : ''}
            </small>
          ) : null}
        </label>

        <label className="vq-field">
          <span>Aesthetic LoRA</span>
          <Select
            label="Aesthetic LoRA"
            value={aestheticId}
            onChange={setAestheticId}
            options={[
              { value: '', label: 'None' },
              ...loras.map((l) => ({
                value: l.id,
                label: `${l.name ?? l.id} · ${l.base_model}${l.usable === false ? ` — ${l.reason ?? l.vetting_status ?? 'unavailable'}` : ''}`,
                disabled: l.usable === false,
              })),
            ]}
          />
          {aestheticId ? <small>{usableLoras.find((l) => l.id === aestheticId)?.license ?? ''} license{brand ? ` · evaluated for ${brand}` : ''}</small> : null}
        </label>

        <Checkbox className="vq-check" checked={fallbackRefs} onChange={setFallbackRefs} label="Fall back to reference images when the LoRA base does not match the selected model" />

        <div className="vq-row">
          <Button type="button" variant="ghost" disabled={busy !== null || !modelKey} onClick={() => void applyLock()}>
            {busy === 'lock' ? 'Locking…' : lock ? 'Re-apply character lock' : 'Apply character lock'}
          </Button>
          {lock ? <Button type="button" variant="ghost" onClick={clearLock}>Clear lock</Button> : null}
        </div>
        {!modelKey ? <p className="vq-note">Pick a provider model above before locking — the lock is checked against its LoRA base and reference shape.</p> : null}

        {lock ? (
          <div className="vq-draft" data-state={stale ? 'blocked' : 'approved'}>
            <div className="vq-draft-head">
              <b>Lock: {lock.mode}</b>
              {stale ? <Pill tone="warn">{`stale — model changed to ${modelKey}`}</Pill> : <Badge tone="ok">active</Badge>}
            </div>
            <dl className="vq-kv">
              <dt>Model</dt><dd className="mono">{lock.model}</dd>
              <dt>LoRAs</dt><dd>{lock.loras.length ? lock.loras.map((l) => `${l.path} @${l.scale}`).join(', ') : 'none'}</dd>
              <dt>References</dt><dd>{lock.references.length} image(s)</dd>
            </dl>
            {lock.pending_upload.length ? <Notice tone="warn">{lock.pending_upload.length} local reference(s) need an upload before the provider can read them.</Notice> : null}
            {switchSentence ? <p className="vq-note">{switchSentence}</p> : null}
            {priceUnconfirmed ? <p className="vq-note"><Badge>{UNCONFIRMED_PRICE_LABEL}</Badge> {UNCONFIRMED_PRICE_SENTENCE}.</p> : null}
            {lockPrimaryDiagnostics(lock).map((d) => <Notice key={d.message} tone="warn">{d.message}</Notice>)}
            {inconsistencies.length ? (
              <details className="vq-details">
                <summary>Details</summary>
                <ul>
                  {inconsistencies.map((i, n) => (
                    <li key={`${n}-${i.message}`}>{i.message}{i.code ? <> (<span className="mono">{i.code}</span>)</> : null}</li>
                  ))}
                </ul>
              </details>
            ) : null}
          </div>
        ) : null}
      </div>

      <details className="vq-details">
        <summary>New character</summary>
        <div className="vq-block">
          <div className="vq-row">
            <label className="vq-field"><span>Id</span><Input value={newId} placeholder="iris" onChange={(e) => setNewId(e.target.value)} /></label>
            <label className="vq-field"><span>Venture</span><Input value={newVenture} placeholder="dd" onChange={(e) => setNewVenture(e.target.value)} /></label>
          </div>
          <label className="vq-field"><span>Description</span><Input value={newDesc} placeholder="Short studio description" onChange={(e) => setNewDesc(e.target.value)} /></label>
          <Checkbox className="vq-check" checked={newIdentifiable} onChange={setNewIdentifiable} label="Real, identifiable person" />
          {newIdentifiable ? <Checkbox className="vq-check" checked={newConsent} onChange={setNewConsent} label="Consent is on file" /> : null}
          <Button type="button" variant="ghost" disabled={busy !== null || !newId.trim()} onClick={() => void createCharacter()}>
            {busy === 'create' ? 'Creating…' : 'Create character'}
          </Button>
        </div>
      </details>

      <details className="vq-details">
        <summary>Add image <small>reference or training photo, by path</small></summary>
        <div className="vq-block">
          <label className="vq-field"><span>Image path</span><Input value={refPath} placeholder="/path/to/photo.png" onChange={(e) => setRefPath(e.target.value)} /></label>
          <div className="vq-row">
            <label className="vq-field">
              <span>Into</span>
              <Select
                label="Into"
                value={refKind}
                onChange={setRefKind}
                options={[
                  { value: 'reference', label: 'Reference set' },
                  { value: 'training', label: 'Training set', disabled: !current },
                ]}
              />
            </label>
            <Button type="button" variant="ghost" disabled={busy !== null || !refPath.trim()} onClick={() => void addImage()}>
              {busy === 'image' ? 'Adding…' : 'Add image'}
            </Button>
          </div>
        </div>
      </details>

      <details className="vq-details">
        <summary>LoRA training <small>fal trainer · preflight before spend</small></summary>
        <div className="vq-block">
          {!current ? <p className="vq-note">Pick a character first.</p> : (
            <>
              <div className="vq-row three">
                <label className="vq-field">
                  <span>Trainer</span>
                  <Select
                    label="Trainer"
                    value={trainer}
                    onChange={setTrainer}
                    options={(trainers?.trainers ?? []).map((t) => ({ value: t.id, label: t.label }))}
                  />
                  {trainers?.trainers.find((t) => t.id === trainer)?.price_note ? <small>{trainers.trainers.find((t) => t.id === trainer)?.price_note}</small> : null}
                </label>
                <label className="vq-field"><span>Steps</span><Input value={steps} inputMode="numeric" onChange={(e) => setSteps(e.target.value)} /></label>
                <label className="vq-field"><span>Cap USD</span><Input value={cap} inputMode="decimal" onChange={(e) => setCap(e.target.value)} /></label>
              </div>
              <div className="vq-row">
                <Button type="button" variant="ghost" disabled={busy !== null} onClick={() => void preflightTrain()}>
                  {busy === 'trainpreflight' ? 'Checking…' : 'Preflight training'}
                </Button>
                <Button type="button" variant="ghost" disabled={busy !== null} onClick={() => void checkTraining()}>Check status</Button>
              </div>
              <p className="vq-note">{current.training_photos} training photo(s) · needs {trainers ? `${trainers.photo_range[0]}–${trainers.photo_range[1]}` : '15–30'}</p>

              {plan ? (
                <div className="vq-draft" data-state="blocked">
                  <div className="vq-draft-head"><b>Training plan</b>{plan.blockers?.length || plan.exceeds_cap ? <Pill tone="warn">blocked</Pill> : <Badge tone="ok">ready</Badge>}</div>
                  <dl className="vq-kv">
                    <dt>Trainer</dt><dd>{plan.trainer.label} · {plan.base_model}</dd>
                    <dt>Steps</dt><dd>{plan.steps} · trigger {plan.trigger_word}</dd>
                    <dt>Dataset</dt><dd>{plan.dataset.photo_count} photos{plan.dataset.extreme ? ' (outside the usual range)' : ''}</dd>
                    <dt>Predicted cost</dt><dd>{usd(plan.predicted_cost_usd)} of {usd(plan.cap_usd)} cap</dd>
                  </dl>
                  {plan.blockers?.map((b) => <Notice key={b} tone="warn">{b}</Notice>)}
                  <Button type="button" variant="primary" className="vq-approve" disabled={busy !== null || Boolean(plan.blockers?.length) || plan.exceeds_cap} onClick={() => void startTraining()}>
                    {busy === 'train' ? 'Submitting…' : `Train LoRA — ${usd(plan.predicted_cost_usd)}`}
                  </Button>
                </div>
              ) : null}

              {status && isHeldJob(status.status) ? (
                <div className="vq-draft" data-state="blocked" role="note">
                  <div className="vq-draft-head"><b>{heldLabel(false)}</b><Pill tone="warn">held</Pill></div>
                  <p className="vq-note">{trainingHeldExplanation('fal', status.reserved_usd)}</p>
                  <dl className="vq-kv">
                    <dt>Training</dt><dd className="mono">{status.training_id}</dd>
                    <dt>Reserved</dt><dd>{trainingAmount(status.reserved_usd)} stays reserved</dd>
                    {trainingHeldSince(status.held_at) ? <><dt>When</dt><dd>{trainingHeldSince(status.held_at)}</dd></> : null}
                  </dl>
                  <div className="vq-row">
                    {trainingHeldActions().map((a) => (
                      <Button
                        key={a}
                        type="button"
                        variant={a === 'abandon' ? 'danger' : 'ghost'}
                        disabled={busy !== null}
                        onClick={() => void resolveHeldTraining(a)}
                      >
                        {busy === 'resolve' ? 'Working…' : trainingHeldActionLabel(a)}
                      </Button>
                    ))}
                  </div>
                </div>
              ) : status ? (
                <div className="vq-draft">
                  <dl className="vq-kv">
                    <dt>Training</dt><dd className="mono">{status.training_id}</dd>
                    <dt>Status</dt><dd>{status.status}</dd>
                    {status.lora_url ? <><dt>LoRA</dt><dd className="mono">{status.lora_url}</dd></> : null}
                    {trainErrorText(status.error) ? <><dt>Error</dt><dd>{trainErrorText(status.error)}</dd></> : null}
                  </dl>
                </div>
              ) : null}
            </>
          )}
        </div>
      </details>
    </section>
  );
}
