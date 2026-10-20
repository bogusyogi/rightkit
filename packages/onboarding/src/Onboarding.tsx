import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { LegalGate } from "@rightkit/legal-ui";
import { Button } from "@rightkit/react-shell/react";
import { cx } from "./ui.js";
import { licenseStep, modelsStep, permissionsStep, signInStep } from "./steps.js";
import type { EligibilityBasis, LegalAcceptanceRecord, LegalGateDocument, LegalStepConfig, OnboardingLabels, OnboardingProps, OnboardingResult, OnboardingStep, OnboardingStepContext, PersistedOnboardingState } from "./types.js";

const KEY = "onboarding.state";
const INITIAL: PersistedOnboardingState = { version: 1, currentStep: 0, completed: [], skipped: false, legalAccepted: false };
const DEFAULT_LABELS: OnboardingLabels = { skip: "Set up later", skipStep: "Skip", back: "Back", continue: "Continue", finish: "Finish setup", loading: "Loading setup…" };

/** Same canonical-UTC rule as `@rightkit/legal` receipts: `Date#toISOString()` form, round-tripping unchanged. */
function isCanonicalUtc(value: unknown): value is string {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(value)) return false;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) && new Date(parsed).toISOString() === value;
}

/** @internal Validates one stored or returned acceptance record; `undefined` when it is not a well-formed record. */
export function readAcceptance(value: unknown): LegalAcceptanceRecord | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const r = value as Partial<LegalAcceptanceRecord>;
  if (typeof r.version !== "string" || !r.version.trim()) return undefined;
  if (typeof r.documentsHash !== "string" || !r.documentsHash.trim()) return undefined;
  if (!isCanonicalUtc(r.acceptedAt)) return undefined;
  if (r.eligibilityBasis !== "individual" && r.eligibilityBasis !== "enterprise") return undefined;
  return { version: r.version, documentsHash: r.documentsHash, acceptedAt: r.acceptedAt, eligibilityBasis: r.eligibilityBasis };
}

/** @internal Normalises the persisted `onboarding.state` value. */
export function readState(value: unknown): PersistedOnboardingState {
  if (!value || typeof value !== "object") return INITIAL;
  const candidate = value as Partial<PersistedOnboardingState>;
  const legalAcceptance = readAcceptance(candidate.legalAcceptance);
  // A present receipt is authoritative, including when it is malformed. Keep the
  // legacy boolean only for state written before the receipt field existed.
  const hasStoredAcceptance = Object.prototype.hasOwnProperty.call(candidate, "legalAcceptance");
  return {
    version: 1,
    currentStep: typeof candidate.currentStep === "number" && candidate.currentStep >= 0 ? Math.floor(candidate.currentStep) : 0,
    completed: Array.isArray(candidate.completed) ? candidate.completed.filter((x): x is string => typeof x === "string") : [],
    skipped: candidate.skipped === true,
    legalAccepted: legalAcceptance !== undefined || (!hasStoredAcceptance && candidate.legalAccepted === true),
    eligibilityBasis: legalAcceptance?.eligibilityBasis ?? (candidate.eligibilityBasis === "individual" || candidate.eligibilityBasis === "enterprise" ? candidate.eligibilityBasis : undefined),
    ...(legalAcceptance ? { legalAcceptance } : {}),
    ...(candidate.legalReopened === true ? { legalReopened: true } : {}),
    ...(Array.isArray(candidate.skippedSteps) ? { skippedSteps: candidate.skippedSteps.filter((x): x is string => typeof x === "string") } : {}),
  };
}

/** Replace the acceptance in `state` with the one from the app's receipt (`undefined`: not accepted). */
function withReceipt(state: PersistedOnboardingState, record: LegalAcceptanceRecord | undefined): PersistedOnboardingState {
  const { legalAcceptance: _drop, eligibilityBasis: _basis, ...rest } = state;
  void _drop; void _basis;
  return record ? { ...rest, legalAccepted: true, eligibilityBasis: record.eligibilityBasis, legalAcceptance: record } : { ...rest, legalAccepted: false };
}

/** True when the stored acceptance satisfies the host's requirement. */
export function isLegalAccepted(
  state: PersistedOnboardingState,
  requiredVersion?: string,
  custom?: (state: PersistedOnboardingState) => boolean,
  requiredDocumentsHash?: string,
): boolean {
  // A reopened agreement (`reopenLegalAcceptance`) is not accepted until the user agrees again, whatever the host's check says.
  if (state.legalReopened === true) return false;
  if (custom) return custom(state);
  // A stored record whose timestamp is not canonical UTC is not an acceptance,
  // even when the host passes no version/hash requirement (same rule as legal receipts).
  if (state.legalAcceptance !== undefined && !isCanonicalUtc(state.legalAcceptance.acceptedAt)) return false;
  // A host that supplies the documents' hash is asking for the text itself to be what was accepted:
  // a changed text re-prompts even when the version string did not move.
  if (requiredDocumentsHash !== undefined && state.legalAcceptance?.documentsHash !== requiredDocumentsHash) return false;
  if (requiredVersion !== undefined) return state.legalAcceptance?.version === requiredVersion;
  // With no version or hash requirement a recorded acceptance is still required: a bare legacy
  // `legalAccepted` flag carries no version, time or basis, so it re-prompts once (as re-acceptance).
  return state.legalAcceptance !== undefined;
}

/** FNV-1a fallback when SubtleCrypto is unavailable. */
function fnv(text: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) { h ^= text.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
  return `fnv1a:${h.toString(16).padStart(8, "0")}`;
}

async function hashDocuments(documents: readonly { title: string; url?: string }[]): Promise<string> {
  const text = JSON.stringify(documents.map((d) => [d.title, d.url ?? ""]));
  try {
    const subtle = globalThis.crypto?.subtle;
    if (subtle) {
      const digest = await subtle.digest("SHA-256", new TextEncoder().encode(text));
      return `sha256:${[...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("")}`;
    }
  } catch { /* fall through */ }
  return fnv(text);
}

export function onboardingStateKey(): string { return KEY; }

function TitleStrip({ titlebar, dragRegion }: { titlebar?: ReactNode; dragRegion?: boolean }) {
  if (!titlebar && !dragRegion) return null;
  return <div className="rk-onboarding__titlebar" data-testid="onboarding-titlebar" {...(dragRegion ? { "data-tauri-drag-region": "" } : {})}>{titlebar}</div>;
}

function NoticeList({ documents }: { documents: readonly LegalGateDocument[] }) {
  if (!documents.length) return <p className="rk-onboarding__muted">No notices to show.</p>;
  return <ul className="rk-onboarding__notices">{documents.map((d) => <li key={d.title}>{d.onOpen ? <button type="button" onClick={d.onOpen}>{d.title}</button> : d.url ? <a href={d.url} target="_blank" rel="noreferrer">{d.title}</a> : d.title}</li>)}</ul>;
}

export function Onboarding({ settings, legal, legalMode = "gate", appName, steps = [], permissions, models, license, signIn, receiptDocuments, onComplete, onSkip, className, layout = "window", requiredVersion, requiredDocumentsHash, isAccepted, documentsHash, beforeLegalGate, requireModel, autoStartDownload, shouldAutoStart, onModelsStepEnter, modelsIntro, allowSkip = true, titlebar, dragRegion, legalDragRegion = false, legalReceipt, labels: labelOverrides }: OnboardingProps) {
  if (legalMode === "gate" && !legal) throw new Error("Onboarding: `legal` is required when legalMode is \"gate\"");
  const L: OnboardingLabels = { ...DEFAULT_LABELS, ...labelOverrides };
  const gated = legalMode === "gate";
  /** Leading steps the user cannot navigate to: the Agreement stand-in in gate mode. */
  const lead = gated ? 1 : 0;
  const [state, setState] = useState<PersistedOnboardingState | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [readyMap, setReadyMap] = useState<Record<string, boolean>>({});
  const [gateCleared, setGateCleared] = useState(false);
  /** One auto-start per mount, even when the models step unmounts (Back/Next) or the step list is rebuilt. */
  const modelsStarted = useRef(false);
  /** Values passed to `ctx.setResult`, keyed by step id; handed to `onComplete`. */
  const stepResults = useRef<Record<string, unknown>>({});
  const [continuing, setContinuing] = useState(false);
  /** A receipt the native service already created for this agreement when saving the settings failed; a retry reuses it instead of stamping a second one. */
  const createdReceipt = useRef<{ key: string; record: LegalAcceptanceRecord } | null>(null);
  /** Persists a patch; throws when the settings backend rejects it. */
  const persist = useCallback(async (patch: Partial<PersistedOnboardingState>) => {
    setSaving(true); setError(null);
    const next = { ...(state ?? INITIAL), ...patch };
    try {
      const snap = await settings.patch({ [KEY]: next });
      setState(readState(snap.values[KEY]));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      throw e;
    } finally { setSaving(false); }
  }, [settings, state]);
  /** Like `persist` but reports success as a boolean; the failure is already visible via `error`. */
  const save = useCallback(async (patch: Partial<PersistedOnboardingState>): Promise<boolean> => {
    try { await persist(patch); return true; } catch { return false; }
  }, [persist]);
  const saveStep = useCallback(async (patch: Partial<PersistedOnboardingState>): Promise<void> => { await save(patch); }, [save]);
  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const loaded = readState((await settings.load()).values[KEY]);
        if (!legalReceipt) { if (live) setState(loaded); return; }
        // The app's receipt is the source of truth for acceptance; an unreadable receipt counts as none.
        let record: LegalAcceptanceRecord | undefined;
        try { record = readAcceptance(await legalReceipt.read()); } catch { record = undefined; }
        if (live) setState(withReceipt(loaded, record));
      } catch (e) { if (live) setError(e instanceof Error ? e.message : String(e)); }
    })();
    return () => { live = false; };
  }, [settings, legalReceipt]);

  const builtins = useMemo<OnboardingStep[]>(() => [
    ...(gated ? [{ id: "legal", label: "Agreement", title: "Review agreement", skippable: false, render: () => <p className="rk-onboarding__muted">Agreement accepted.</p> }] : []),
    ...(legalMode === "notice" ? [{ id: "notice", label: "Notice", title: "Notices", skippable: false, render: () => <NoticeList documents={legal?.documents ?? []} /> }] : []),
    ...(permissions ? [permissionsStep(permissions)] : []),
    ...(models ? [modelsStep(models, { required: requireModel, autoStartDownload, shouldAutoStart, onModelsStepEnter, intro: modelsIntro, startGuard: modelsStarted })] : []),
    ...(license ? [licenseStep(license)] : []),
    ...(signIn ? [signInStep(signIn)] : []),
    ...steps,
  ], [gated, legalMode, legal?.documents, license, signIn, models, permissions, steps, requireModel, autoStartDownload, shouldAutoStart, onModelsStepEnter, modelsIntro]);
  const activeState = state ?? INITIAL;
  const accepted = !gated || isLegalAccepted(activeState, requiredVersion, isAccepted, requiredDocumentsHash);
  const needsGate = state !== null && !accepted;
  // The agreement gate does not depend on the saved step: until it is accepted, the active step is the first.
  // Once accepted, the Agreement step (index 0) is never shown again when other steps exist.
  const index = !accepted ? 0 : builtins.length > lead ? Math.max(lead, Math.min(activeState.currentStep, builtins.length - 1)) : 0;
  const active = builtins[index];
  const activeId = active?.id ?? "";
  /** The step on screen now; an `onContinue` that resolves after the step changed is stale. */
  const activeIdRef = useRef(activeId);
  activeIdRef.current = activeId;
  const setReady = useCallback((ready: boolean) => setReadyMap((m) => m[activeId] === ready ? m : { ...m, [activeId]: ready }), [activeId]);
  useEffect(() => {
    if (!needsGate) { setGateCleared(false); return; }
    if (gateCleared) return;
    let live = true;
    void (async () => { try { await beforeLegalGate?.(); } catch (e) { if (live) setError(e instanceof Error ? e.message : String(e)); } if (live) setGateCleared(true); })();
    return () => { live = false; };
  }, [needsGate, gateCleared, beforeLegalGate]);

  const setResult = useCallback((value: unknown) => { stepResults.current[activeId] = value; }, [activeId]);
  const context: OnboardingStepContext = { state: activeState, settings, setState: saveStep, permissions, models, license, signIn, ready: readyMap, setReady, setResult };
  const canContinue = active?.canContinue ? active.canContinue(context) : true;
  const runCallback = async (callback: (() => void | Promise<void>) | undefined) => {
    try { await callback?.(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  };
  const resultOf = (completed: string[], skipped: string[]): OnboardingResult => ({ completed, skipped, steps: { ...stepResults.current } });
  /** Marks `mark.completed` done or `mark.skipped` skipped (never both), then moves to `nextIndex`; completing the last step calls `onComplete`. */
  const markAndMove = async (nextIndex: number, mark: { completed?: string; skipped?: string } = {}) => {
    const previousSkipped = activeState.skippedSteps ?? [];
    // A step is completed or skipped, never both: a skip removes an earlier completion of the same step.
    const completedBase = activeState.completed.filter((id) => id !== mark.skipped);
    const completed = mark.completed && !completedBase.includes(mark.completed) ? [...completedBase, mark.completed] : completedBase;
    const skippedBase = previousSkipped.filter((id) => id !== mark.completed);
    const skippedSteps = mark.skipped && !skippedBase.includes(mark.skipped) ? [...skippedBase, mark.skipped] : skippedBase;
    if (nextIndex >= builtins.length) {
      if (await save({ completed, skippedSteps, currentStep: Math.max(0, builtins.length - 1), skipped: false })) await runCallback(() => onComplete(resultOf(completed, skippedSteps)));
      return;
    }
    await save({ completed, skippedSteps, currentStep: nextIndex, skipped: false });
  };
  /** Primary button: runs the step's `onContinue` veto first; `false` or a throw keeps the step. */
  const advance = async () => {
    if (active.onContinue) {
      const stepId = active.id;
      setContinuing(true); setError(null);
      let proceed = true;
      try { proceed = (await active.onContinue(context)) !== false; } catch (e) { if (activeIdRef.current === stepId) setError(e instanceof Error ? e.message : String(e)); return; } finally { setContinuing(false); }
      // The step changed while onContinue was awaited: discard this completion rather than move or save from stale state.
      if (!proceed || activeIdRef.current !== stepId) return;
    }
    await markAndMove(index + 1, { completed: active.id });
  };
  const rootClass = cx("rk-onboarding", layout === "contained" && "rk-onboarding--contained", className);
  if (error && !state) return <div className={rootClass}><p className="rk-onboarding__error" role="alert">{error}</p><Button onClick={() => window.location.reload()}>Retry</Button></div>;
  if (state && !active) return <main className={rootClass} data-testid="onboarding"><p className="rk-onboarding__muted">Nothing to set up.</p><Button variant="primary" onClick={() => void runCallback(() => onComplete(resultOf(activeState.completed, activeState.skippedSteps ?? [])))}>{L.finish}</Button></main>;
  if (!state || !active) return <div className={rootClass} aria-busy="true"><p className="rk-onboarding__muted">{L.loading}</p></div>;
  if (needsGate) {
    if (!gateCleared) return <div className={rootClass} aria-busy="true"><TitleStrip titlebar={titlebar} dragRegion={legalDragRegion} /><p className="rk-onboarding__muted">{L.loading}</p></div>;
    if (!legal) return null;
    const reacceptance = activeState.legalAccepted || activeState.legalAcceptance !== undefined;
    /** Records the acceptance and returns the record to mirror into the settings state. A throw keeps the gate closed. */
    const recordAcceptance = async (gate: LegalStepConfig, basis: EligibilityBasis): Promise<LegalAcceptanceRecord> => {
      const version = requiredVersion ?? "1";
      const hash = documentsHash ?? requiredDocumentsHash ?? await hashDocuments(gate.documents);
      if (legalReceipt?.create) {
        // Native path: the service stamps the time and stores the receipt; nothing is built in the renderer.
        const key = [version, hash, basis].join("\n");
        if (createdReceipt.current?.key === key) return createdReceipt.current.record;
        const declared = receiptDocuments ?? gate.documents.map((d) => ({ title: d.title, ...(d.url ? { url: d.url } : {}) }));
        const created = readAcceptance(await legalReceipt.create({ version, documentsHash: hash, documents: declared, eligibilityBasis: basis, surface: "first_run" }));
        if (!created) throw new Error("The legal receipt service returned an invalid record.");
        // A receipt for another agreement would leave the gate showing again after a "successful" save.
        if (created.eligibilityBasis !== basis || (requiredVersion !== undefined && created.version !== requiredVersion) || (requiredDocumentsHash !== undefined && created.documentsHash !== requiredDocumentsHash)) {
          throw new Error("The legal receipt does not match the agreement that was shown.");
        }
        createdReceipt.current = { key, record: created };
        return created;
      }
      // LEGACY: the renderer builds the record with the webview clock. Deprecated; implement `legalReceipt.create`.
      const record: LegalAcceptanceRecord = { version, documentsHash: hash, acceptedAt: new Date().toISOString(), eligibilityBasis: basis };
      if (legalReceipt) {
        if (!legalReceipt.write) throw new Error("The legal receipt can neither create nor write an acceptance.");
        await legalReceipt.write(record);
      }
      return record;
    };
    return <div className={cx("rk-onboarding rk-onboarding--legal", layout === "contained" && "rk-onboarding--contained", className)} data-testid="onboarding-legal" data-variant={reacceptance ? "re-acceptance" : "first-run"}><TitleStrip titlebar={titlebar} dragRegion={legalDragRegion} /><LegalGate {...legal} layout={layout} variant={reacceptance ? "re-acceptance" : "first-run"} onQuit={() => void legal.onQuit?.()} onAgree={async ({ eligibilityBasis }) => {
      let legalAcceptance: LegalAcceptanceRecord;
      try { legalAcceptance = await recordAcceptance(legal, eligibilityBasis); } catch (e) { setError(e instanceof Error ? e.message : String(e)); throw e; }
      const progress = reacceptance ? {} : { completed: ["legal"], currentStep: builtins.length > 1 ? 1 : 0 };
      await persist({ legalAccepted: true, legalReopened: false, eligibilityBasis, legalAcceptance, ...progress });
      createdReceipt.current = null;
      if (!reacceptance && builtins.length === 1) await onComplete(resultOf(["legal"], []));
    }} /></div>;
  }
  const body: ReactNode = active.render(context);
  return <main className={rootClass} data-testid="onboarding" data-step-id={active.id}>
    <TitleStrip titlebar={titlebar} dragRegion={dragRegion} />
    <aside className="rk-onboarding__rail"><div className="rk-onboarding__brand">{appName ?? legal?.appName}</div><nav aria-label="Setup progress">{builtins.map((step, i) => <button key={step.id} type="button" className={cx("rk-onboarding__step", i === index && "is-active", activeState.completed.includes(step.id) && "is-done")} aria-current={i === index ? "step" : undefined} disabled={(gated && i === 0 && builtins.length > 1) || continuing} onClick={() => { if (!continuing && i >= lead && (i <= index || canContinue)) void save({ currentStep: i }); }}><span>{activeState.completed.includes(step.id) ? "✓" : i + 1}</span>{step.label}</button>)}</nav></aside>
    <section className="rk-onboarding__main"><header><p className="rk-onboarding__eyebrow">Step {index + 1} of {builtins.length}</p>{active.title ? <h1>{active.title}</h1> : null}{active.description ? <p className="rk-onboarding__description">{active.description}</p> : null}</header><div className="rk-onboarding__content">{body}</div>{error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}<footer>{allowSkip ? <Button variant="ghost" disabled={saving || continuing} onClick={() => { void save({ currentStep: index, skipped: true }).then((ok) => ok ? runCallback(onSkip) : undefined); }}>{L.skip}</Button> : null}{active.skippable ? <Button variant="ghost" disabled={saving || continuing} onClick={() => void markAndMove(index + 1, { skipped: active.id })}>{L.skipStep}</Button> : null}<span className="rk-onboarding__hint">{!canContinue && active.blockedHint ? active.blockedHint : null}</span><Button variant="secondary" disabled={index <= lead || saving || continuing} onClick={() => void save({ currentStep: index - 1 })}>{L.back}</Button><Button variant="primary" disabled={saving || continuing || !canContinue} onClick={() => void advance()}>{active.continueLabel ?? (index === builtins.length - 1 ? L.finish : L.continue)}</Button></footer></section>
  </main>;
}
