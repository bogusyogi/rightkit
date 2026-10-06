import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { LegalGate } from "@rightkit/legal-ui";
import { Button } from "@rightkit/app-shell/react";
import { cx } from "./ui.js";
import { licenseStep, modelsStep, permissionsStep } from "./steps.js";
import type { OnboardingProps, OnboardingStep, OnboardingStepContext, PersistedOnboardingState } from "./types.js";

const KEY = "onboarding.state";
const INITIAL: PersistedOnboardingState = { version: 1, currentStep: 0, completed: [], skipped: false, legalAccepted: false };

function readState(value: unknown): PersistedOnboardingState {
  if (!value || typeof value !== "object") return INITIAL;
  const candidate = value as Partial<PersistedOnboardingState>;
  return {
    version: 1,
    currentStep: typeof candidate.currentStep === "number" && candidate.currentStep >= 0 ? Math.floor(candidate.currentStep) : 0,
    completed: Array.isArray(candidate.completed) ? candidate.completed.filter((x): x is string => typeof x === "string") : [],
    skipped: candidate.skipped === true,
    legalAccepted: candidate.legalAccepted === true,
    eligibilityBasis: candidate.eligibilityBasis === "individual" || candidate.eligibilityBasis === "enterprise" ? candidate.eligibilityBasis : undefined,
  };
}

export function onboardingStateKey(): string { return KEY; }

export function Onboarding({ settings, legal, steps = [], permissions, models, license, onComplete, onSkip, className }: OnboardingProps) {
  const [state, setState] = useState<PersistedOnboardingState | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const save = useCallback(async (patch: Partial<PersistedOnboardingState>) => {
    setSaving(true); setError(null);
    const next = { ...(state ?? INITIAL), ...patch };
    try {
      const snap = await settings.patch({ [KEY]: next });
      setState(readState(snap.values[KEY]));
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setSaving(false); }
  }, [settings, state]);
  useEffect(() => { let live = true; void settings.load().then((snapshot) => { if (live) setState(readState(snapshot.values[KEY])); }, (e) => { if (live) setError(e instanceof Error ? e.message : String(e)); }); return () => { live = false; }; }, [settings]);

  const builtins = useMemo<OnboardingStep[]>(() => [
    { id: "legal", label: "Agreement", title: "Review agreement", skippable: false, render: () => null },
    ...(permissions ? [permissionsStep(permissions)] : []),
    ...(models ? [modelsStep(models)] : []),
    ...(license ? [licenseStep(license)] : []),
    ...steps,
  ], [license, models, permissions, steps]);
  const activeState = state ?? INITIAL;
  const index = Math.min(activeState.currentStep, builtins.length - 1);
  const active = builtins[index];

  const context: OnboardingStepContext = { state: activeState, settings, setState: save, permissions, models, license };
  const markAndMove = async (nextIndex: number, completedId?: string) => {
    const completed = completedId && !activeState.completed.includes(completedId) ? [...activeState.completed, completedId] : activeState.completed;
    if (nextIndex >= builtins.length) { await save({ completed, currentStep: Math.max(0, builtins.length - 1), skipped: false }); await onComplete(); return; }
    await save({ completed, currentStep: nextIndex, skipped: false });
  };
  if (error && !state) return <div className={cx("rk-onboarding", className)}><p className="rk-onboarding__error" role="alert">{error}</p><Button onClick={() => window.location.reload()}>Retry</Button></div>;
  if (!state || !active) return <div className={cx("rk-onboarding", className)} aria-busy="true"><p className="rk-onboarding__muted">Loading setup…</p></div>;
  if (index === 0 && !activeState.legalAccepted) {
    return <div className={cx("rk-onboarding rk-onboarding--legal", className)} data-testid="onboarding-legal"><LegalGate {...legal} variant="first-run" onQuit={() => void legal.onQuit?.()} onAgree={async ({ eligibilityBasis }) => { await save({ legalAccepted: true, eligibilityBasis, completed: ["legal"], currentStep: builtins.length > 1 ? 1 : 0 }); if (builtins.length === 1) await onComplete(); }} /></div>;
  }
  const body: ReactNode = active.render(context);
  return <main className={cx("rk-onboarding", className)} data-testid="onboarding" data-step-id={active.id}>
    <aside className="rk-onboarding__rail"><div className="rk-onboarding__brand">{legal.appName}</div><nav aria-label="Setup progress">{builtins.map((step, i) => <button key={step.id} type="button" className={cx("rk-onboarding__step", i === index && "is-active", activeState.completed.includes(step.id) && "is-done")} aria-current={i === index ? "step" : undefined} onClick={() => { if (i > 0 || activeState.legalAccepted) void save({ currentStep: i }); }}><span>{activeState.completed.includes(step.id) ? "✓" : i + 1}</span>{step.label}</button>)}</nav></aside>
    <section className="rk-onboarding__main"><header><p className="rk-onboarding__eyebrow">Step {index + 1} of {builtins.length}</p><h1>{active.title}</h1>{active.description ? <p className="rk-onboarding__description">{active.description}</p> : null}</header><div className="rk-onboarding__content">{body}</div>{error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}<footer><Button variant="ghost" disabled={saving} onClick={() => { void save({ currentStep: index, skipped: true }).then(() => onSkip?.()); }}>Set up later</Button><span /><Button variant="secondary" disabled={index === 0 || saving} onClick={() => void save({ currentStep: index - 1 })}>Back</Button><Button variant="primary" disabled={saving} onClick={() => void markAndMove(index + 1, active.id)}>{index === builtins.length - 1 ? "Finish setup" : "Continue"}</Button></footer></section>
  </main>;
}
