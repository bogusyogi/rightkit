import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { Badge, Button, Input, SegmentedControl, Toggle, createThemeStore, useMediaQuery, useShellOptional, type ThemeChoice, type ThemeStore } from "@rightkit/app-shell/react";
import { isLicensed, tier as licenseTier, type StoredLicense, type createLicenseClient } from "@rightkit/license";
import type { LogsClient } from "@rightkit/logs";
import { applyBrand, brandFromAccent } from "@rightkit/theme";
import type { AutoUpdateClient, AutoUpdateStatus } from "@rightkit/updates";
import { sectionIcons } from "./icons.js";
import { ProgressBar, SettingRow, SettingsGroup, formatBytes } from "./primitives.js";
import type { ModelEntry, ModelsBackend, SecretPresence, SecretRef, SecretsBackend, SettingsContext, SettingsSection } from "./types.js";

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Two-step button: first click arms, second confirms; avoids a modal for destructive row actions. */
function ConfirmButton({ label, confirmLabel = "Confirm", onConfirm, disabled }: { label: string; confirmLabel?: string; onConfirm: () => void; disabled?: boolean }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 4000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <Button
      size="sm"
      variant={armed ? "danger" : "ghost"}
      disabled={disabled}
      onClick={() => {
        if (armed) {
          setArmed(false);
          onConfirm();
        } else setArmed(true);
      }}
    >
      {armed ? confirmLabel : label}
    </Button>
  );
}

/* ---- Appearance ----------------------------------------------------------------------------- */

export interface AppearanceOptions {
  /** Theme store; defaults to the surrounding `ShellProvider`'s, else a private `rightkit.theme` store. */
  themeStore?: ThemeStore;
  /** Optional accent presets (user-chosen colour on top of the brand). The first is treated as the brand default. */
  accents?: ReadonlyArray<{ id: string; label: string; color: string }>;
  /** Extra rows (density, font size) the app adds under Appearance. */
  extra?: (ctx: SettingsContext) => ReactNode;
}

const THEME_OPTIONS = [
  { value: "system" as const, label: "System" },
  { value: "light" as const, label: "Light" },
  { value: "dark" as const, label: "Dark" },
];

function AppearanceBody({ ctx, options }: { ctx: SettingsContext; options: AppearanceOptions }) {
  const shell = useShellOptional();
  const fallback = useMemo(() => options.themeStore ?? createThemeStore({ storageKey: "rightkit.theme" }), [options.themeStore]);
  const store = options.themeStore ?? shell?.themeStore ?? fallback;
  const choice = useSyncExternalStore(store.subscribe, store.read, store.read);
  const systemDark = useMediaQuery("(prefers-color-scheme: dark)");
  const resolved = choice === "system" ? (systemDark ? "dark" : "light") : choice;
  const saved = ctx.values["appearance.theme"] as ThemeChoice | undefined;
  const accentId = ctx.values["appearance.accent"] as string | undefined;
  const accents = options.accents ?? [];

  // Settings are the source of truth across windows; the theme store mirrors them.
  useEffect(() => {
    if ((saved === "light" || saved === "dark" || saved === "system") && saved !== store.read()) store.save(saved);
  }, [saved, store]);

  const accent = accents.find((a) => a.id === accentId);
  useEffect(() => {
    if (!accent || accent.id === accents[0]?.id) return;
    return applyBrand(brandFromAccent(accent.label, accent.color), resolved);
  }, [accent, accents, resolved]);

  return (
    <>
      <SettingsGroup>
        <SettingRow id="appearance.theme" label="Theme" description="System follows your operating system, and switches when it does.">
          <SegmentedControl<ThemeChoice>
            label="Theme"
            value={choice}
            options={THEME_OPTIONS}
            onChange={(next) => {
              store.save(next);
              void ctx.set("appearance.theme", next);
            }}
          />
        </SettingRow>
        {accents.length ? (
          <SettingRow id="appearance.accent" label="Accent colour" description="Used for buttons, switches and selection.">
            <div className="rk-swatches" role="radiogroup" aria-label="Accent colour">
              {accents.map((a, i) => {
                const on = (accentId ?? accents[0].id) === a.id;
                return (
                  <button
                    key={a.id}
                    type="button"
                    role="radio"
                    aria-checked={on}
                    aria-label={a.label}
                    title={a.label}
                    className={on ? "rk-swatch is-on" : "rk-swatch"}
                    style={{ background: a.color }}
                    onClick={() => void ctx.set("appearance.accent", i === 0 ? null : a.id)}
                  />
                );
              })}
            </div>
          </SettingRow>
        ) : null}
      </SettingsGroup>
      {options.extra?.(ctx)}
    </>
  );
}

export function appearanceSection(options: AppearanceOptions = {}): SettingsSection {
  return {
    id: "appearance",
    icon: sectionIcons.appearance,
    label: "Appearance",
    group: "General",
    order: 100,
    keywords: ["theme", "dark", "light", "colour", "color", "accent"],
    entries: [
      { id: "appearance.theme", label: "Theme", keywords: ["dark", "light", "system", "mode"] },
      ...(options.accents?.length ? [{ id: "appearance.accent", label: "Accent colour", keywords: ["color", "brand"] }] : []),
    ],
    render: (ctx) => <AppearanceBody ctx={ctx} options={options} />,
  };
}

/* ---- Updates -------------------------------------------------------------------------------- */

export interface UpdatesOptions {
  client: AutoUpdateClient;
  currentVersion: string;
  /** Called when an update is downloaded and the user chose to restart. */
  onRestart?: () => void | Promise<void>;
  /** Shown for a Pro-only feature update on a Free licence. */
  upgradeHint?: ReactNode;
  /**
   * Whether the update client can install updates by itself (`createAutoUpdateClient({ shouldAutoInstall })` returning true
   * for some tier). The "Install updates automatically" toggle is shown only when this is true. Default false.
   */
  supportsAutoInstall?: boolean;
  /** Override any user-visible string. */
  labels?: Partial<UpdatesLabels>;
}

export interface UpdatesLabels {
  version: (currentVersion: string) => string;
  checkNow: string;
  installUpdate: string;
  restartNow: string;
  proUpdate: string;
  autoInstall: string;
  autoInstallDescription: string;
  /** Replace the status sentence; receives the default text. */
  status: (status: AutoUpdateStatus, fallback: string) => string;
}

const defaultUpdatesLabels: UpdatesLabels = {
  version: (v) => `Version ${v}`,
  checkNow: "Check now",
  installUpdate: "Install update",
  restartNow: "Restart now",
  proUpdate: "Pro update",
  autoInstall: "Install updates automatically",
  autoInstallDescription: "Patches install when the app is idle. Feature updates always ask first.",
  status: (_s, fallback) => fallback,
};

const updatesLabels = (o: UpdatesOptions): UpdatesLabels => ({ ...defaultUpdatesLabels, ...o.labels });

function statusLine(s: AutoUpdateStatus): { tone: "ok" | "warn" | "bad" | "neutral" | "accent"; text: string } {
  switch (s.kind) {
    case "idle": return { tone: "neutral", text: "Not checked yet." };
    case "checking": return { tone: "neutral", text: "Checking for updates…" };
    case "up_to_date": return { tone: "ok", text: "You are up to date." };
    case "available": return { tone: s.eligible ? "accent" : "warn", text: `Version ${s.version} is available${s.tier ? ` (${s.tier})` : ""}.` };
    case "deferred": return { tone: "warn", text: `Version ${s.version} will install later: ${s.reason.replace(/_/g, " ")}.` };
    case "downloading": return { tone: "accent", text: `Downloading ${s.version}… ${Math.round(s.pct)}%` };
    case "ready": return { tone: "ok", text: `Version ${s.version} is ready to install.` };
    case "error": return { tone: "bad", text: s.message };
  }
}

function UpdatesBody({ ctx, o }: { ctx: SettingsContext; o: UpdatesOptions }) {
  const [status, setStatus] = useState<AutoUpdateStatus>({ kind: "idle" });
  const busy = status.kind === "checking" || status.kind === "downloading";
  const run = useCallback(async (fn: () => Promise<AutoUpdateStatus>) => {
    setStatus({ kind: "checking" });
    try {
      setStatus(await fn());
    } catch (e) {
      setStatus({ kind: "error", message: errText(e) });
    }
  }, []);
  const L = updatesLabels(o);
  const line = statusLine(status);
  const auto = ctx.values["updates.autoInstall"] === true;
  return (
    <SettingsGroup>
      <SettingRow id="updates.status" label={L.version(o.currentVersion)} description={<span role="status" data-update-status={status.kind}>{L.status(status, line.text)}</span>}>
        <div className="rk-sr__buttons">
          <Badge tone={line.tone}>{status.kind.replace(/_/g, " ")}</Badge>
          {status.kind === "available" && status.eligible ? (
            <Button size="sm" variant="primary" disabled={busy} onClick={() => void run(() => o.client.installAvailable())}>{L.installUpdate}</Button>
          ) : null}
          {status.kind === "ready" && o.onRestart ? (
            <Button size="sm" variant="primary" onClick={() => void o.onRestart?.()}>{L.restartNow}</Button>
          ) : null}
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => void run(() => o.client.checkOnce())}>{L.checkNow}</Button>
        </div>
      </SettingRow>
      {status.kind === "available" && !status.eligible && status.reason === "pro_required" && o.upgradeHint ? (
        <SettingRow label={L.proUpdate} description={o.upgradeHint} />
      ) : null}
      {o.supportsAutoInstall ? (
        <SettingRow id="updates.auto" label={L.autoInstall} description={L.autoInstallDescription}>
          <Toggle label={L.autoInstall} checked={auto} onChange={(v) => void ctx.set("updates.autoInstall", v)} />
        </SettingRow>
      ) : null}
    </SettingsGroup>
  );
}

export function updatesSection(options: UpdatesOptions): SettingsSection {
  return {
    id: "updates",
    icon: sectionIcons.updates,
    label: "Updates",
    group: "General",
    order: 200,
    keywords: ["version", "upgrade", "release"],
    entries: [
      { id: "updates.status", label: "Check for updates", keywords: ["version", "upgrade"] },
      ...(options.supportsAutoInstall ? [{ id: "updates.auto", label: updatesLabels(options).autoInstall, keywords: ["auto", "patch"] }] : []),
    ],
    render: (ctx) => <UpdatesBody ctx={ctx} o={options} />,
  };
}

/* ---- Licence -------------------------------------------------------------------------------- */

export interface LicenseOptions {
  client: ReturnType<typeof createLicenseClient>;
  graceDays?: number;
  purchaseUrl?: string;
  openExternal?: (url: string) => void | Promise<void>;
  /** Display names for tiers, e.g. `{ pro: "Pro" }`. Falls back to the tier string. */
  tierLabels?: Record<string, string>;
  /** Plan / upsell panel rendered above the licence rows. */
  planSlot?: ReactNode;
  /** Fired after activate/deactivate so the app can refresh its entitlements. */
  onChange?: (lic: StoredLicense | null) => void;
}

function describeLicense(lic: StoredLicense | null, grace: number): { tone: "ok" | "warn" | "bad" | "neutral"; label: string; detail: string } {
  if (!lic || lic.status === "none") return { tone: "neutral", label: "Free", detail: "No licence key is active on this device." };
  if (lic.status === "invalid" || lic.status === "failed") return { tone: "bad", label: lic.status === "failed" ? "Not activated" : "Invalid", detail: lic.message ?? "The licence key was not accepted." };
  const valid = isLicensed(lic, grace);
  const until = lic.validUntil ? new Date(lic.validUntil).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }) : null;
  const kind = lic.licenseKind === "trial" ? "Trial" : lic.licenseKind === "term" ? "Subscription" : "Lifetime";
  if (!valid) return { tone: "warn", label: "Expired", detail: until ? `This ${kind.toLowerCase()} ended on ${until}.` : "This licence is no longer valid." };
  return { tone: "ok", label: kind, detail: until ? `${kind} valid until ${until}.` : `${kind} licence is active.` };
}

function LicenseBody({ ctx, o }: { ctx: SettingsContext; o: LicenseOptions }) {
  void ctx;
  const [lic, setLic] = useState<StoredLicense | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const grace = o.graceDays ?? 7;
  useEffect(() => {
    let live = true;
    void o.client.load().then((l) => live && setLic(l));
    return () => {
      live = false;
    };
  }, [o.client]);
  const apply = (l: StoredLicense | null) => {
    setLic(l);
    o.onChange?.(l);
  };
  const info = describeLicense(lic, grace);
  const t = licenseTier(lic);
  const active = lic?.status === "active";
  return (
    <SettingsGroup>
      <SettingRow id="license.status" label="Licence" description={<span role="status">{info.detail}</span>}>
        <div className="rk-sr__buttons">
          {t ? <Badge tone="accent">{o.tierLabels?.[t] ?? t}</Badge> : null}
          <Badge tone={info.tone}>{info.label}</Badge>
        </div>
      </SettingRow>
      {active ? (
        <SettingRow id="license.deactivate" label="Deactivate this device" description="Frees the seat so you can activate another device.">
          <ConfirmButton
            label="Deactivate"
            confirmLabel="Confirm deactivate"
            disabled={busy}
            onConfirm={() => {
              setBusy(true);
              void o.client
                .deactivate()
                .then(() => apply(null))
                .catch((e) => setNote(errText(e)))
                .finally(() => setBusy(false));
            }}
          />
        </SettingRow>
      ) : (
        <SettingRow id="license.activate" label="Licence key" description={note ?? "Paste the key from your purchase email."} stacked>
          <form
            className="rk-sr__inline"
            onSubmit={(e) => {
              e.preventDefault();
              if (!key.trim()) return;
              setBusy(true);
              setNote(null);
              void o.client
                .activate(key)
                .then((l) => {
                  apply(l);
                  setKey("");
                  if (l.status !== "active") setNote(l.message ?? "The key was not accepted.");
                })
                .catch((err) => setNote(errText(err)))
                .finally(() => setBusy(false));
            }}
          >
            <Input
              aria-label="Licence key"
              placeholder="XXXX-XXXX-XXXX-XXXX"
              value={key}
              autoComplete="off"
              spellCheck={false}
              onChange={(e) => setKey(e.target.value)}
              aria-invalid={lic?.status === "invalid" || lic?.status === "failed" ? true : undefined}
            />
            <Button type="submit" variant="primary" size="sm" disabled={busy || !key.trim()}>Activate</Button>
          </form>
        </SettingRow>
      )}
      {o.purchaseUrl && !active ? (
        <SettingRow label="Need a licence?">
          <Button size="sm" variant="ghost" onClick={() => void o.openExternal?.(o.purchaseUrl!)}>Get a licence</Button>
        </SettingRow>
      ) : null}
    </SettingsGroup>
  );
}

export function licenseSection(options: LicenseOptions): SettingsSection {
  return {
    id: "license",
    icon: sectionIcons.license,
    label: "Licence",
    group: "Account",
    order: 100,
    keywords: ["pro", "trial", "activate", "key", "purchase"],
    entries: [
      { id: "license.status", label: "Licence status", keywords: ["pro", "trial", "tier"] },
      { id: "license.activate", label: "Activate licence key", keywords: ["key"] },
    ],
    render: (ctx) => (
      <>
        {options.planSlot}
        <LicenseBody ctx={ctx} o={options} />
      </>
    ),
  };
}

/* ---- Legal ---------------------------------------------------------------------------------- */

export interface LegalOptions {
  documents: ReadonlyArray<{ title: string; url: string }>;
  /** ISO timestamp of acceptance (from the legal gate), when known. */
  acceptedAt?: string | null;
  acceptedVersion?: string | null;
  openExternal?: (url: string) => void | Promise<void>;
  /** Re-open the `@rightkit/legal-ui` gate in `re-acceptance` mode. */
  onReview?: () => void;
}

export function legalSection(o: LegalOptions): SettingsSection {
  return {
    id: "legal",
    icon: sectionIcons.legal,
    label: "Legal",
    group: "Account",
    order: 200,
    keywords: ["terms", "privacy", "agreement", "eula"],
    entries: o.documents.map((d, i) => ({ id: `legal.doc.${i}`, label: d.title, keywords: ["terms", "privacy"] })),
    render: () => (
      <SettingsGroup description={o.acceptedAt ? `You accepted these on ${new Date(o.acceptedAt).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" })}${o.acceptedVersion ? ` (version ${o.acceptedVersion})` : ""}.` : undefined}>
        {o.documents.map((d, i) => (
          <SettingRow key={d.url} id={`legal.doc.${i}`} label={d.title}>
            <Button size="sm" variant="ghost" onClick={() => void o.openExternal?.(d.url)}>Read</Button>
          </SettingRow>
        ))}
        {o.onReview ? (
          <SettingRow label="Review agreement" description="Open the agreement screen again.">
            <Button size="sm" variant="secondary" onClick={o.onReview}>Review</Button>
          </SettingRow>
        ) : null}
      </SettingsGroup>
    ),
  };
}

/* ---- Diagnostics & logs --------------------------------------------------------------------- */

export interface DiagnosticsOptions {
  logs: LogsClient;
  /** Copy text to the clipboard; defaults to `navigator.clipboard`. */
  copy?: (text: string) => void | Promise<void>;
}

function DiagnosticsBody({ ctx, o }: { ctx: SettingsContext; o: DiagnosticsOptions }) {
  const [report, setReport] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const copy = o.copy ?? ((t: string) => navigator.clipboard.writeText(t));
  return (
    <>
      <SettingsGroup description="Logs are content-free: field names are allow-listed by the app and never include what you type, say or open.">
        <SettingRow id="diagnostics.verbose" label="Verbose logging" description="Record more detail while you reproduce a problem.">
          <Toggle label="Verbose logging" checked={ctx.values["diagnostics.verbose"] === true} onChange={(v) => void ctx.set("diagnostics.verbose", v)} />
        </SettingRow>
        <SettingRow id="diagnostics.collect" label="Diagnostics report" description={note ?? "Collect a report you can attach to a support request."}>
          <div className="rk-sr__buttons">
            <Button
              size="sm"
              variant="secondary"
              disabled={busy}
              onClick={() => {
                setBusy(true);
                setNote(null);
                void o.logs
                  .flush()
                  .then(() => o.logs.collectDiagnostics())
                  .then((r) => setReport(JSON.stringify(r, null, 2)))
                  .catch((e) => setNote(errText(e)))
                  .finally(() => setBusy(false));
              }}
            >
              Collect
            </Button>
            {report ? (
              <Button size="sm" variant="ghost" onClick={() => void Promise.resolve(copy(report)).then(() => setNote("Copied to the clipboard."), (e) => setNote(errText(e)))}>Copy</Button>
            ) : null}
          </div>
        </SettingRow>
        <SettingRow id="diagnostics.clear" label="Clear logs" description="Deletes the log files on this device.">
          <ConfirmButton
            label="Clear"
            disabled={busy}
            onConfirm={() => void o.logs.clear().then(() => (setReport(null), setNote("Logs cleared.")), (e) => setNote(errText(e)))}
          />
        </SettingRow>
      </SettingsGroup>
      {report ? <pre className="rk-diag" aria-label="Diagnostics report" tabIndex={0}>{report}</pre> : null}
    </>
  );
}

export function diagnosticsSection(options: DiagnosticsOptions): SettingsSection {
  return {
    id: "diagnostics",
    icon: sectionIcons.diagnostics,
    label: "Diagnostics",
    group: "Support",
    order: 100,
    keywords: ["logs", "debug", "support", "report"],
    entries: [
      { id: "diagnostics.verbose", label: "Verbose logging", keywords: ["debug"] },
      { id: "diagnostics.collect", label: "Collect diagnostics report", keywords: ["support", "logs"] },
      { id: "diagnostics.clear", label: "Clear logs" },
    ],
    render: (ctx) => <DiagnosticsBody ctx={ctx} o={options} />,
  };
}

/* ---- Models --------------------------------------------------------------------------------- */

export interface ModelsOptions {
  models: ModelsBackend;
  title?: string;
}

const FIT_TEXT: Record<NonNullable<ModelEntry["fit"]>["kind"], { tone: "ok" | "warn" | "bad" | "neutral"; text: string }> = {
  measured: { tone: "ok", text: "Tested on this hardware" },
  estimated: { tone: "neutral", text: "Should fit" },
  unqualified: { tone: "warn", text: "Not tested here" },
  unsupported: { tone: "bad", text: "Too large for this device" },
};

function ModelsBody({ o }: { o: ModelsOptions }) {
  const [entries, setEntries] = useState<ModelEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    let off: (() => void) | undefined;
    o.models.list().then((l) => live && setEntries(l), (e) => live && setError(errText(e)));
    void o.models.subscribe?.((entry) => live && setEntries((cur) => (cur ? cur.map((x) => (x.id === entry.id ? { ...x, ...entry } : x)) : cur))).then((u) => (live ? (off = u) : u()));
    return () => {
      live = false;
      off?.();
    };
  }, [o.models]);
  const act = (fn: () => Promise<void>) => fn().then(() => o.models.list()).then(setEntries, (e) => setError(errText(e)));
  if (error && !entries) return <p className="rk-settings__error" role="alert">{error}</p>;
  if (!entries) return <p className="rk-settings__loading">Loading models…</p>;
  return (
    <SettingsGroup description={error ?? undefined}>
      {entries.map((m) => {
        const fit = m.fit ? FIT_TEXT[m.fit.kind] : null;
        const working = m.state === "downloading" || m.state === "verifying";
        return (
          <SettingRow
            key={m.id}
            id={`models.${m.id}`}
            label={<>{m.label}{m.active ? <> <Badge tone="accent">In use</Badge></> : null}</>}
            description={
              <>
                {[m.description, m.sizeBytes ? formatBytes(m.sizeBytes) : null].filter(Boolean).join(" · ")}
                {fit ? <> {" "}<Badge tone={fit.tone}>{fit.text}</Badge></> : null}
                {m.state === "error" && m.error ? <span className="rk-sr__err" role="alert"> {m.error.message}</span> : null}
                {working ? <ProgressBar value={m.progress ?? 0} label={`${m.label} ${m.state}`} /> : null}
              </>
            }
          >
            {m.state === "installed" ? (
              <ConfirmButton label="Remove" confirmLabel="Confirm remove" onConfirm={() => void act(() => o.models.remove(m.id))} />
            ) : working ? (
              <Button size="sm" variant="ghost" onClick={() => void act(() => o.models.cancel(m.id))}>Cancel</Button>
            ) : (
              <Button size="sm" variant="secondary" disabled={m.fit?.kind === "unsupported"} onClick={() => void act(() => o.models.install(m.id))}>
                {m.state === "error" ? "Retry" : "Download"}
              </Button>
            )}
          </SettingRow>
        );
      })}
    </SettingsGroup>
  );
}

export function modelsSection(options: ModelsOptions): SettingsSection {
  return {
    id: "models",
    icon: sectionIcons.models,
    label: options.title ?? "Models",
    group: "General",
    order: 300,
    keywords: ["download", "local", "ai", "speech", "storage"],
    entries: [{ id: "models.list", label: "Downloaded models", keywords: ["download", "remove"] }],
    render: () => <ModelsBody o={options} />,
  };
}

/* ---- API keys ------------------------------------------------------------------------------- */

export interface ApiKeySlot {
  id: string;
  label: string;
  description?: ReactNode;
  ref: SecretRef;
  placeholder?: string;
}

export interface ApiKeysOptions {
  slots: readonly ApiKeySlot[];
  secrets: SecretsBackend;
}

function ApiKeyRow({ slot, secrets, ctx, presence, onChanged }: { slot: ApiKeySlot; secrets: SecretsBackend; ctx: SettingsContext; presence: SecretPresence; onChanged: (p: SecretPresence) => void }) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const save = async () => {
    const v = value;
    if (!v.trim()) return;
    setBusy(true);
    setErr(null);
    setValue("");
    try {
      await secrets.set(slot.ref, v.trim());
      // Settings keep only the pointer; the value lives in the credential store.
      await ctx.set(`secrets.${slot.id}`, slot.ref);
      onChanged("present");
    } catch (e) {
      setErr(errText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <SettingRow
      id={`apikeys.${slot.id}`}
      label={slot.label}
      description={err ?? slot.description ?? (presence === "present" ? "Stored in the system credential store." : presence === "unknown" ? "The credential store could not be read." : "Not set.")}
      stacked={presence !== "present"}
    >
      {presence === "present" ? (
        <div className="rk-sr__buttons">
          <Badge tone="ok">Saved</Badge>
          <ConfirmButton
            label="Remove"
            disabled={busy}
            onConfirm={() => {
              setBusy(true);
              void secrets
                .clear(slot.ref)
                .then(() => ctx.set(`secrets.${slot.id}`, null))
                .then(() => onChanged("absent"))
                .catch((e) => setErr(errText(e)))
                .finally(() => setBusy(false));
            }}
          />
        </div>
      ) : (
        <form
          className="rk-sr__inline"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <Input type="password" aria-label={`${slot.label} value`} autoComplete="off" spellCheck={false} placeholder={slot.placeholder ?? "Paste key"} value={value} onChange={(e) => setValue(e.target.value)} />
          <Button type="submit" size="sm" variant="primary" disabled={busy || !value.trim()}>Save</Button>
        </form>
      )}
    </SettingRow>
  );
}

function ApiKeysBody({ o, ctx }: { o: ApiKeysOptions; ctx: SettingsContext }) {
  // Render rows immediately while host checks credential presence.
  const [presence, setPresence] = useState<SecretPresence[]>(() => o.slots.map(() => "unknown"));
  const presenceGeneration = useRef(0);
  useEffect(() => {
    let live = true;
    const generation = ++presenceGeneration.current;
    o.secrets.presence(o.slots.map((s) => s.ref)).then(
      (p) => live && generation === presenceGeneration.current && setPresence(p),
      () => live && generation === presenceGeneration.current && setPresence(o.slots.map(() => "unknown")),
    );
    return () => {
      live = false;
    };
  }, [o.secrets, o.slots]);
  return (
    <SettingsGroup description="Keys are stored in your system credential store. They are never written to settings, logs or diagnostics, and cannot be shown again.">
      {o.slots.map((slot, i) => (
        <ApiKeyRow key={slot.id} slot={slot} secrets={o.secrets} ctx={ctx} presence={presence[i] ?? "unknown"} onChanged={(p) => { presenceGeneration.current += 1; setPresence((cur) => cur.map((x, j) => (j === i ? p : x))); }} />
      ))}
    </SettingsGroup>
  );
}

export function apiKeysSection(options: ApiKeysOptions): SettingsSection {
  return {
    id: "api-keys",
    icon: sectionIcons.apiKeys,
    label: "API keys",
    group: "Account",
    order: 300,
    keywords: ["secret", "token", "credential", "keychain"],
    entries: options.slots.map((s) => ({ id: `apikeys.${s.id}`, label: s.label, keywords: ["key", "secret"] })),
    render: (ctx) => <ApiKeysBody o={options} ctx={ctx} />,
  };
}

/* ---- About ---------------------------------------------------------------------------------- */

export interface AboutOptions {
  appName: string;
  version: string;
  build?: string;
  platform?: string;
  copyright?: string;
  links?: ReadonlyArray<{ label: string; url: string }>;
  openExternal?: (url: string) => void | Promise<void>;
  copy?: (text: string) => void | Promise<void>;
  /** Rows rendered inside the About card. */
  extra?: ReactNode;
  /** Full panels rendered below the About card, outside it. */
  panels?: ReactNode;
}

export function aboutSection(o: AboutOptions): SettingsSection {
  const info = [`${o.appName} ${o.version}`, o.build ? `build ${o.build}` : null, o.platform ?? null].filter(Boolean).join(" · ");
  return {
    id: "about",
    icon: sectionIcons.about,
    label: "About",
    group: "Support",
    order: 900,
    keywords: ["version", "build", "licence", "credits"],
    entries: [{ id: "about.version", label: "Version", keywords: ["build"] }],
    render: () => (
      <>
      <SettingsGroup>
        <SettingRow id="about.version" label={o.appName} description={info}>
          <Button size="sm" variant="ghost" onClick={() => void Promise.resolve((o.copy ?? ((t: string) => navigator.clipboard.writeText(t)))(info))}>Copy version info</Button>
        </SettingRow>
        {(o.links ?? []).map((l) => (
          <SettingRow key={l.url} label={l.label}>
            <Button size="sm" variant="ghost" onClick={() => void o.openExternal?.(l.url)}>Open</Button>
          </SettingRow>
        ))}
        {o.extra}
        {o.copyright ? <p className="rk-sg__desc">{o.copyright}</p> : null}
      </SettingsGroup>
      {o.panels}
      </>
    ),
  };
}
