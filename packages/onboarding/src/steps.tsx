import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Button, Input, Badge } from "@rightkit/react-shell/react";
import type { SignInBackend, SignInProvider, LicenseBackend, ModelEntry, ModelsAutoStart, ModelsStepEnterContext, LicenseState, ModelsBackend, OnboardingStep, OnboardingStepContext, PermissionStatus, PermissionsBackend } from "./types.js";
import { nextEntryProgress, trackModelInstall } from "./model-progress.js";

function ProgressBar({ value, label }: { value: number; label: string }) {
  const pct = Math.max(0, Math.min(1, value));
  return <div className="rk-onboarding__progress" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct * 100)}><i style={{ width: `${pct * 100}%` }} /></div>;
}

const formatBytes = (n?: number): string => {
  if (n === undefined) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value >= 100 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
};

const permissionTone = (state: PermissionStatus["state"]): "ok" | "warn" | "bad" | "neutral" =>
  state === "ready" ? "ok" : state === "denied" || state === "restricted" ? "bad" : state === "needed" ? "warn" : "neutral";

function PermissionsBody({ backend }: { backend: PermissionsBackend }) {
  const [items, setItems] = useState<PermissionStatus[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const reload = useCallback(() => backend.list().then(setItems, (e) => setError(e instanceof Error ? e.message : String(e))), [backend]);
  useEffect(() => void reload(), [reload]);
  return (
    <div className="rk-onboarding__card" data-testid="onboarding-permissions">
      {error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}
      {items.length === 0 && error ? <Button size="sm" variant="secondary" onClick={() => { setError(null); void reload(); }}>Try again</Button> : null}
      {items.length === 0 && !error ? <p className="rk-onboarding__muted">Checking permissions…</p> : null}
      {items.map((item) => (
        <div className="rk-onboarding__row" key={item.id} data-permission-id={item.id}>
          <div><strong>{item.label}</strong><p>{item.description ?? item.detail}</p></div>
          <div className="rk-onboarding__row-actions">
            <Badge tone={permissionTone(item.state)}>{item.state}</Badge>
            {item.state === "needed" || item.state === "denied" ? (
              <Button size="sm" variant="secondary" disabled={busy === item.id} onClick={() => { setBusy(item.id); setError(null); void backend.request(item.id).then((next) => setItems((current) => current.map((x) => x.id === next.id ? next : x)), (e) => setError(e instanceof Error ? e.message : String(e))).finally(() => setBusy(null)); }}>
                {item.state === "denied" ? "Try again" : "Allow"}
              </Button>
            ) : null}
          </div>
        </div>
      ))}
    </div>
  );
}

export function permissionsStep(backend: PermissionsBackend, options: { label?: string; title?: string; description?: string } = {}): OnboardingStep {
  return { id: "permissions", label: options.label ?? "Permissions", title: options.title ?? "Allow what this app needs", description: options.description ?? "You can change these permissions later in System Settings.", skippable: true, render: () => <PermissionsBody backend={backend} /> };
}

/** What the step knows about its models, for `ModelsStepOptions.extras`. */
export interface ModelsStepView {
  /** False until the first read of the model list has finished. */
  loaded: boolean;
  /** True when at least one model is installed. */
  installed: boolean;
  models: readonly ModelEntry[];
}

interface ModelsBodyProps extends ModelsAutoStart {
  backend: ModelsBackend; onReady?: (ready: boolean) => void; intro?: ReactNode; startGuard?: { current: boolean };
  context?: OnboardingStepContext; onInstalledInSession?: ModelsStepOptions["onInstalledInSession"]; installedDelayMs?: number; extras?: ModelsStepOptions["extras"];
}

function ModelsBody({ backend, onReady, autoStartDownload, shouldAutoStart, onModelsStepEnter, intro, startGuard, context, onInstalledInSession, installedDelayMs = 900, extras }: ModelsBodyProps) {
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const ownGuard = useRef(false);
  const autoStarted = startGuard ?? ownGuard;
  // The bar only moves forward: the last progress each model showed, reset by an error entry.
  const lastProgress = useRef(new Map<string, number>());
  const reload = useCallback(() => backend.list().then((list) => { setModels(list); setLoaded(true); }, (e) => setError(e instanceof Error ? e.message : String(e))), [backend]);
  useEffect(() => {
    void reload();
    if (!backend.subscribe) return;
    let off: (() => void) | undefined;
    let live = true;
    void backend.subscribe((incoming) => {
      const progress = nextEntryProgress(lastProgress.current.get(incoming.id) ?? 0, incoming);
      lastProgress.current.set(incoming.id, progress);
      const entry = { ...incoming, progress };
      setModels((current) => current.some((m) => m.id === entry.id) ? current.map((m) => m.id === entry.id ? entry : m) : [...current, entry]);
    }).then((unsubscribe) => { if (live) off = unsubscribe; else unsubscribe(); });
    return () => { live = false; off?.(); };
  }, [backend, reload]);
  const hasInstalled = models.some((m) => m.state === "installed");
  useEffect(() => { onReady?.(hasInstalled); }, [hasInstalled, onReady]);
  // Advance on an install that finished while this step was mounted (see `trackModelInstall`).
  const contextRef = useRef(context);
  contextRef.current = context;
  const onInstalledRef = useRef(onInstalledInSession);
  onInstalledRef.current = onInstalledInSession;
  const mountedRef = useRef(true);
  const sawNotInstalledRef = useRef(false);
  const [installedHere, setInstalledHere] = useState(false);
  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);
  const aggregateState = !loaded ? "unknown" : hasInstalled ? "installed" : "missing";
  useEffect(() => {
    const next = trackModelInstall(sawNotInstalledRef.current, aggregateState);
    sawNotInstalledRef.current = next.sawNotInstalled;
    setInstalledHere(next.advance);
  }, [aggregateState]);
  const watchInstalled = onInstalledInSession !== undefined;
  useEffect(() => {
    if (!installedHere || !watchInstalled) return;
    const timer = window.setTimeout(() => {
      const current = contextRef.current;
      if (!current) return;
      void Promise.resolve(onInstalledRef.current?.(current, () => mountedRef.current)).catch((e) => { if (mountedRef.current) setError(e instanceof Error ? e.message : String(e)); });
    }, installedDelayMs);
    return () => window.clearTimeout(timer);
  }, [installedHere, watchInstalled, installedDelayMs]);
  const run = (id: string, request: Promise<void>) => {
    setBusy(id); setError(null);
    void request.then(reload, (e) => setError(e instanceof Error ? e.message : String(e))).finally(() => setBusy(null));
  };
  useEffect(() => {
    if (!loaded || autoStarted.current) return;
    if (!onModelsStepEnter && !shouldAutoStart && !autoStartDownload) return;
    autoStarted.current = true;
    if (onModelsStepEnter) {
      // The app owns which model starts and when; the hook runs once per Onboarding mount.
      const ctx: ModelsStepEnterContext = { models, start: (id) => backend.install(id).then(reload), refresh: () => backend.list().then((list) => { setModels(list); return list; }) };
      void Promise.resolve().then(() => onModelsStepEnter(ctx)).catch((e) => setError(e instanceof Error ? e.message : String(e)));
      return;
    }
    if (shouldAutoStart) {
      const pick = models.find((m) => m.state === "missing" && shouldAutoStart(m, models));
      if (pick) run(pick.id, backend.install(pick.id));
      return;
    }
    if (models.some((m) => m.state === "installed" || m.state === "downloading" || m.state === "verifying")) return;
    const first = models.find((m) => m.state === "missing");
    if (first) run(first.id, backend.install(first.id));
    // run is intentionally omitted: it only closes over setters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoStartDownload, shouldAutoStart, onModelsStepEnter, loaded, models, backend]);
  const card = (
    <div className="rk-onboarding__card" data-testid="onboarding-models">
      {intro ? <div className="rk-onboarding__intro" data-testid="onboarding-models-intro">{intro}</div> : null}
      {error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}
      {error && !loaded ? <Button size="sm" variant="secondary" onClick={() => { setError(null); void reload(); }}>Try again</Button> : null}
      {models.map((model) => {
        const progress = model.progress ?? (model.state === "installed" ? 1 : 0);
        const active = model.state === "downloading" || model.state === "verifying";
        const cancel = backend.cancel;
        return <div className="rk-onboarding__row" key={model.id} data-model-id={model.id}>
          <div className="rk-onboarding__grow"><strong>{model.label}</strong><p>{model.description ?? ""}{model.sizeBytes ? ` · ${formatBytes(model.sizeBytes)}` : ""}</p>{active ? <ProgressBar value={progress} label={`${model.label} download`} /> : null}</div>
          <div className="rk-onboarding__row-actions"><Badge tone={model.state === "installed" ? "ok" : model.state === "error" ? "bad" : "neutral"}>{model.state}</Badge>
            {model.state === "installed" ? null
              : model.state === "error" ? <Button size="sm" variant="secondary" disabled={busy === model.id} onClick={() => run(model.id, backend.install(model.id))}>Retry</Button>
              : active ? (cancel ? <Button size="sm" variant="secondary" disabled={busy === model.id} onClick={() => run(model.id, cancel.call(backend, model.id))}>Pause</Button> : null)
              : <Button size="sm" variant="secondary" disabled={busy === model.id} onClick={() => run(model.id, backend.install(model.id))}>Download</Button>}
          </div>
        </div>;
      })}
      {models.length === 0 && !error ? <p className="rk-onboarding__muted">No models available.</p> : null}
    </div>
  );
  if (!extras) return card;
  // The extras sit under the card; both are laid out as one centred column.
  return <div style={{ display: "grid", width: "100%", justifyItems: "center", gap: 12 }}>{card}{extras({ loaded, installed: hasInstalled, models })}</div>;
}

export interface ModelsStepOptions extends ModelsAutoStart {
  label?: string; title?: string; description?: string;
  /** Continue/Finish wait until at least one model is installed. Default false. */
  required?: boolean;
  /** Start downloading the first missing model when the step opens. Default false. */
  autoStartDownload?: boolean;
  /** App copy rendered at the top of the step. */
  intro?: ReactNode;
  /** Shared "already auto-started" flag. `Onboarding` supplies one that survives Back/Next and re-renders; omit it and the guard lives for the step's mount only. */
  startGuard?: { current: boolean };
  /**
   * Called once when a model goes from not installed to installed while this step is mounted (a download that
   * finished here), after `installedDelayMs`. A model already installed when the step opens (relaunch, Back to
   * the step) never calls it. `isActive` is false once the step unmounted (Back, a rail click): check it before
   * moving on. Use `context.advance()` to complete the step, or your own activation first. A rejection shows as
   * the step's error.
   */
  onInstalledInSession?: (context: OnboardingStepContext, isActive: () => boolean) => void | Promise<void>;
  /** Wait before `onInstalledInSession`, so the finished bar is seen. Default 900. */
  installedDelayMs?: number;
  /** App content under the model card (a feature reel while nothing is installed, a licence credit). Receives the step's model state. */
  extras?: (view: ModelsStepView) => ReactNode;
}

export function modelsStep(backend: ModelsBackend, options: ModelsStepOptions = {}): OnboardingStep {
  return {
    id: "models", label: options.label ?? "Models", title: options.title ?? "Choose local models", description: options.description ?? "Downloads are verified before a model is activated.", skippable: true,
    canContinue: options.required ? (ctx) => ctx.ready.models === true : undefined,
    blockedHint: options.required ? "Download a model to continue." : undefined,
    render: (ctx) => <ModelsBody backend={backend} onReady={ctx.setReady} autoStartDownload={options.autoStartDownload} shouldAutoStart={options.shouldAutoStart} onModelsStepEnter={options.onModelsStepEnter} intro={options.intro} startGuard={options.startGuard} context={ctx} onInstalledInSession={options.onInstalledInSession} installedDelayMs={options.installedDelayMs} extras={options.extras} />,
  };
}

/** Copy overrides for the licence step. Each key defaults to the English text. */
export interface LicenseStepCopy {
  /** Key field label and placeholder. Default "Licence key". */
  keyLabel?: string;
  /** Submit button. Default "Activate". */
  activateLabel?: string;
  /** Trial button. Default "Start trial". */
  trialLabel?: string;
  /** Plans link. Default "View plans". */
  plansLabel?: string;
  /** Status text when no licence is active. Default "You can continue without a licence.". */
  noLicenceDetail?: string;
}

function LicenseBody({ backend, copy }: { backend: LicenseBackend; copy: Required<LicenseStepCopy> }) {
  const [state, setState] = useState<LicenseState>({ status: "none" });
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => void backend.load().then(setState, (e) => setError(e instanceof Error ? e.message : String(e))), [backend]);
  const run = (fn: () => Promise<LicenseState>) => { setBusy(true); setError(null); void fn().then(setState, (e) => setError(e instanceof Error ? e.message : String(e))).finally(() => setBusy(false)); };
  return <div className="rk-onboarding__card" data-testid="onboarding-license">
    <div className="rk-onboarding__license-state"><Badge tone={state.status === "active" || state.status === "trial" ? "ok" : state.status === "expired" || state.status === "invalid" ? "bad" : "neutral"}>{state.label ?? state.status}</Badge><span>{state.detail ?? copy.noLicenceDetail}</span></div>
    {error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}
    {backend.activate ? <form className="rk-onboarding__form" onSubmit={(e) => { e.preventDefault(); if (key.trim()) run(() => backend.activate!(key.trim())); }}><Input aria-label={copy.keyLabel} placeholder={copy.keyLabel} value={key} onChange={(e) => setKey(e.target.value)} disabled={busy} /><Button size="sm" variant="primary" type="submit" disabled={busy || !key.trim()}>{copy.activateLabel}</Button></form> : null}
    <div className="rk-onboarding__row-actions">{backend.startTrial ? <Button size="sm" variant="secondary" disabled={busy} onClick={() => run(() => backend.startTrial!())}>{copy.trialLabel}</Button> : null}{backend.purchaseUrl ? <Button size="sm" variant="ghost" onClick={() => void backend.openExternal?.(backend.purchaseUrl!)}>{copy.plansLabel}</Button> : null}</div>
  </div>;
}

export interface LicenseStepOptions { label?: string; title?: string; description?: string; copy?: LicenseStepCopy }

export function licenseStep(backend: LicenseBackend, options: LicenseStepOptions = {}): OnboardingStep {
  const copy: Required<LicenseStepCopy> = {
    keyLabel: "Licence key", activateLabel: "Activate", trialLabel: "Start trial", plansLabel: "View plans",
    noLicenceDetail: "You can continue without a licence.", ...options.copy,
  };
  return { id: "license", label: options.label ?? "Licence", title: options.title ?? "Choose how you’ll use this app", description: options.description ?? "Activate a licence, start a trial, or continue with the free plan.", skippable: true, render: () => <LicenseBody backend={backend} copy={copy} /> };
}

function SignInBody({ backend }: { backend: SignInBackend }) {
  const [account, setAccount] = useState<{ signedIn: boolean; account?: string }>({ signedIn: false });
  const [busy, setBusy] = useState<SignInProvider | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { let live = true; backend.load().then((a) => { if (live) setAccount(a); }, () => undefined); return () => { live = false; }; }, [backend]);
  const start = async (provider: SignInProvider) => {
    setBusy(provider);
    setError(null);
    try { setAccount(await backend.start(provider)); } catch (e) { setError(e instanceof Error ? e.message : "Sign-in did not finish."); } finally { setBusy(null); }
  };
  return <div className="rk-onboarding__signin">
    {account.signedIn ? <p>Signed in{account.account ? <> as <strong>{account.account}</strong></> : null}.</p> : null}
    <div className="rk-onboarding__row-actions">{backend.providers.map((p) => <Button key={p.id} size="sm" variant="secondary" disabled={busy !== null} onClick={() => void start(p.id)}>{busy === p.id ? "Waiting for your browser…" : p.label ?? `Sign in with ${p.id === "google" ? "Google" : p.id === "apple" ? "Apple" : "your account"}`}</Button>)}</div>
    <p className="rk-onboarding__hint"><small>Opens your browser. You stay signed in to the accounts you already use there.</small></p>
    {error ? <p role="alert">{error}</p> : null}
  </div>;
}

/** Sign-in step. Every button hands off to the shared owner and opens the user's default browser; no in-app webview. */
export function signInStep(backend: SignInBackend, options: { label?: string; title?: string; description?: string } = {}): OnboardingStep {
  return { id: "sign-in", label: options.label ?? "Sign in", title: options.title ?? "Sign in", description: options.description ?? "Sign-in opens your default browser, so you stay signed in to the accounts you already use there.", skippable: true, render: () => <SignInBody backend={backend} /> };
}
