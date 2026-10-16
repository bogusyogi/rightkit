import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Button, Input, Badge } from "@rightkit/react-shell/react";
import type { LicenseBackend, ModelEntry, ModelsAutoStart, ModelsStepEnterContext, LicenseState, ModelsBackend, OnboardingStep, PermissionStatus, PermissionsBackend } from "./types.js";

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

interface ModelsBodyProps extends ModelsAutoStart { backend: ModelsBackend; onReady?: (ready: boolean) => void; intro?: ReactNode; startGuard?: { current: boolean } }

function ModelsBody({ backend, onReady, autoStartDownload, shouldAutoStart, onModelsStepEnter, intro, startGuard }: ModelsBodyProps) {
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const ownGuard = useRef(false);
  const autoStarted = startGuard ?? ownGuard;
  const reload = useCallback(() => backend.list().then((list) => { setModels(list); setLoaded(true); }, (e) => setError(e instanceof Error ? e.message : String(e))), [backend]);
  useEffect(() => {
    void reload();
    if (!backend.subscribe) return;
    let off: (() => void) | undefined;
    let live = true;
    void backend.subscribe((entry) => setModels((current) => current.some((m) => m.id === entry.id) ? current.map((m) => m.id === entry.id ? entry : m) : [...current, entry])).then((unsubscribe) => { if (live) off = unsubscribe; else unsubscribe(); });
    return () => { live = false; off?.(); };
  }, [backend, reload]);
  const hasInstalled = models.some((m) => m.state === "installed");
  useEffect(() => { onReady?.(hasInstalled); }, [hasInstalled, onReady]);
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
  return (
    <div className="rk-onboarding__card" data-testid="onboarding-models">
      {intro ? <div className="rk-onboarding__intro" data-testid="onboarding-models-intro">{intro}</div> : null}
      {error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}
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
}

export function modelsStep(backend: ModelsBackend, options: ModelsStepOptions = {}): OnboardingStep {
  return {
    id: "models", label: options.label ?? "Models", title: options.title ?? "Choose local models", description: options.description ?? "Downloads are verified before a model is activated.", skippable: true,
    canContinue: options.required ? (ctx) => ctx.ready.models === true : undefined,
    blockedHint: options.required ? "Download a model to continue." : undefined,
    render: (ctx) => <ModelsBody backend={backend} onReady={ctx.setReady} autoStartDownload={options.autoStartDownload} shouldAutoStart={options.shouldAutoStart} onModelsStepEnter={options.onModelsStepEnter} intro={options.intro} startGuard={options.startGuard} />,
  };
}

function LicenseBody({ backend }: { backend: LicenseBackend }) {
  const [state, setState] = useState<LicenseState>({ status: "none" });
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => void backend.load().then(setState, (e) => setError(e instanceof Error ? e.message : String(e))), [backend]);
  const run = (fn: () => Promise<LicenseState>) => { setBusy(true); setError(null); void fn().then(setState, (e) => setError(e instanceof Error ? e.message : String(e))).finally(() => setBusy(false)); };
  return <div className="rk-onboarding__card" data-testid="onboarding-license">
    <div className="rk-onboarding__license-state"><Badge tone={state.status === "active" || state.status === "trial" ? "ok" : state.status === "expired" || state.status === "invalid" ? "bad" : "neutral"}>{state.label ?? state.status}</Badge><span>{state.detail ?? "You can continue without a licence."}</span></div>
    {error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}
    {backend.activate ? <form className="rk-onboarding__form" onSubmit={(e) => { e.preventDefault(); if (key.trim()) run(() => backend.activate!(key.trim())); }}><Input aria-label="Licence key" placeholder="Licence key" value={key} onChange={(e) => setKey(e.target.value)} disabled={busy} /><Button size="sm" variant="primary" type="submit" disabled={busy || !key.trim()}>Activate</Button></form> : null}
    <div className="rk-onboarding__row-actions">{backend.startTrial ? <Button size="sm" variant="secondary" disabled={busy} onClick={() => run(() => backend.startTrial!())}>Start trial</Button> : null}{backend.purchaseUrl ? <Button size="sm" variant="ghost" onClick={() => void backend.openExternal?.(backend.purchaseUrl!)}>View plans</Button> : null}</div>
  </div>;
}

export function licenseStep(backend: LicenseBackend, options: { label?: string; title?: string; description?: string } = {}): OnboardingStep {
  return { id: "license", label: options.label ?? "Licence", title: options.title ?? "Choose how you’ll use this app", description: options.description ?? "Activate a licence, start a trial, or continue with the free plan.", skippable: true, render: () => <LicenseBody backend={backend} /> };
}
