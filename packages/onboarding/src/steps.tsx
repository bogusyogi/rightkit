import { useCallback, useEffect, useState } from "react";
import { Button, Input, Badge } from "@rightkit/app-shell/react";
import type { LicenseBackend, LicenseState, ModelsBackend, OnboardingStep, PermissionStatus, PermissionsBackend } from "./types.js";

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

function ModelsBody({ backend }: { backend: ModelsBackend }) {
  const [models, setModels] = useState<import("./types.js").ModelEntry[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const reload = useCallback(() => backend.list().then(setModels, (e) => setError(e instanceof Error ? e.message : String(e))), [backend]);
  useEffect(() => {
    void reload();
    if (!backend.subscribe) return;
    let off: (() => void) | undefined;
    void backend.subscribe((entry) => setModels((current) => current.some((m) => m.id === entry.id) ? current.map((m) => m.id === entry.id ? entry : m) : [...current, entry])).then((unsubscribe) => { off = unsubscribe; });
    return () => off?.();
  }, [backend, reload]);
  const action = (model: import("./types.js").ModelEntry) => {
    setBusy(model.id); setError(null);
    const request = model.state === "downloading" || model.state === "verifying" ? backend.cancel(model.id) : backend.install(model.id);
    void request.then(reload, (e) => setError(e instanceof Error ? e.message : String(e))).finally(() => setBusy(null));
  };
  return (
    <div className="rk-onboarding__card" data-testid="onboarding-models">
      {error ? <p className="rk-onboarding__error" role="alert">{error}</p> : null}
      {models.map((model) => {
        const progress = model.progress ?? (model.state === "installed" ? 1 : 0);
        return <div className="rk-onboarding__row" key={model.id} data-model-id={model.id}>
          <div className="rk-onboarding__grow"><strong>{model.label}</strong><p>{model.description ?? ""}{model.sizeBytes ? ` · ${formatBytes(model.sizeBytes)}` : ""}</p>{model.state === "downloading" || model.state === "verifying" ? <ProgressBar value={progress} label={`${model.label} download`} /> : null}</div>
          <div className="rk-onboarding__row-actions"><Badge tone={model.state === "installed" ? "ok" : model.state === "error" ? "bad" : "neutral"}>{model.state}</Badge>{model.state !== "installed" && model.state !== "error" ? <Button size="sm" variant="secondary" disabled={busy === model.id} onClick={() => action(model)}>{model.state === "downloading" || model.state === "verifying" ? "Pause" : "Download"}</Button> : model.state === "error" ? <Button size="sm" variant="secondary" disabled={busy === model.id} onClick={() => action(model)}>Retry</Button> : null}</div>
        </div>;
      })}
      {models.length === 0 && !error ? <p className="rk-onboarding__muted">No models available.</p> : null}
    </div>
  );
}

export function modelsStep(backend: ModelsBackend, options: { label?: string; title?: string; description?: string } = {}): OnboardingStep {
  return { id: "models", label: options.label ?? "Models", title: options.title ?? "Choose local models", description: options.description ?? "Downloads are verified before a model is activated.", skippable: true, render: () => <ModelsBody backend={backend} /> };
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
