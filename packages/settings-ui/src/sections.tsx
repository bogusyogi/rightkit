import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Badge, Button, Input, Toggle } from "@rightkit/react-shell/react";
import { isLicensed, type StoredLicense } from "@rightkit/license";
import type { LogsClient } from "@rightkit/logs";
import type { AutoUpdateClient, AutoUpdateStatus } from "@rightkit/app-update";
import { errText } from "./errors.js";
import { useDoctor, useLicenseSection, type DoctorCheck, type DoctorResult, type DoctorStatus, type LicenseClientLike } from "./hooks.js";
import { sectionIcons } from "./icons.js";
import { ProgressBar, SettingRow, SettingsGroup, formatBytes } from "./primitives.js";
import type { ModelEntry, ModelsBackend, SecretPresence, SecretRef, SecretsBackend, SettingsContext, SettingsSection } from "./types.js";


/** Two-step button: first click arms, second confirms; avoids a modal for destructive row actions. */
function ConfirmButton({ id, label, confirmLabel = "Confirm", onConfirm, disabled }: { id?: string; label: string; confirmLabel?: string; onConfirm: () => void; disabled?: boolean }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 4000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <Button
      id={id}
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

/* ---- Appearance: lives in ./appearance.tsx (no @rightkit/license import), re-exported here ---- */

export { appearanceSection } from "./appearance.js";
export type { AppearanceOptions } from "./appearance.js";

/* ---- Updates -------------------------------------------------------------------------------- */

/**
 * Function-style update client for apps that own their update plumbing (IPC commands, a private adapter) instead of
 * `createAutoUpdateClient`. `check` / `install` resolve to the resulting status; the optional `status` seeds the display
 * (e.g. the result of the startup check) when the controls mount.
 */
export interface UpdatesAdapter {
  check(): Promise<AutoUpdateStatus>;
  install(): Promise<AutoUpdateStatus>;
  status?: AutoUpdateStatus | (() => AutoUpdateStatus | null | Promise<AutoUpdateStatus | null>);
}

export type UpdatesClient = AutoUpdateClient | UpdatesAdapter;

interface NormalizedUpdates {
  check(): Promise<AutoUpdateStatus>;
  install(): Promise<AutoUpdateStatus>;
  initial?: UpdatesAdapter["status"];
}

function normalizeUpdates(client: UpdatesClient): NormalizedUpdates {
  if ("check" in client && typeof client.check === "function") return { check: () => client.check(), install: () => client.install(), initial: client.status };
  const c = client as AutoUpdateClient;
  return { check: () => c.checkOnce(), install: () => c.installAvailable() };
}

export interface UpdatesOptions {
  client: UpdatesClient;
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
  /** Nav label of the section. */
  sectionLabel: string;
  /** Search entry for the check row. */
  searchCheck: string;
  version: (currentVersion: string) => string;
  /** Check button ("Check now" by default; e.g. "Check for Updates"). */
  checkNow: string;
  /** Install button ("Install update" by default; e.g. "Install and restart"). */
  installUpdate: string;
  /** Shown on the check button while a check runs. Unset: the button keeps `checkNow`. */
  checking?: string;
  /** Shown on the install button while installing. Unset: the button keeps `installUpdate`. */
  installing?: string;
  restartNow: string;
  proUpdate: string;
  autoInstall: string;
  autoInstallDescription: string;
  /** Replace the status sentence; receives the default text. */
  status: (status: AutoUpdateStatus, fallback: string) => string;
  /** Replace the status badge text; receives the default (the status kind). */
  statusBadge: (status: AutoUpdateStatus, fallback: string) => string;
}

const defaultUpdatesLabels: UpdatesLabels = {
  sectionLabel: "Updates",
  searchCheck: "Check for updates",
  version: (v) => `Version ${v}`,
  checkNow: "Check now",
  installUpdate: "Install update",
  restartNow: "Restart now",
  proUpdate: "Pro update",
  autoInstall: "Install updates automatically",
  autoInstallDescription: "Patches install when the app is idle. Feature updates always ask first.",
  status: (_s, fallback) => fallback,
  statusBadge: (_s, fallback) => fallback,
};

const resolveUpdatesLabels = (labels?: Partial<UpdatesLabels>): UpdatesLabels => ({ ...defaultUpdatesLabels, ...labels });
const updatesLabels = (o: UpdatesOptions): UpdatesLabels => resolveUpdatesLabels(o.labels);

function statusLine(s: AutoUpdateStatus): { tone: "ok" | "warn" | "bad" | "neutral" | "accent"; text: string } {
  switch (s.kind) {
    case "idle": return { tone: "neutral", text: "Not checked yet." };
    case "checking": return { tone: "neutral", text: "Checking for updates…" };
    case "up_to_date": return { tone: "ok", text: "You are up to date." };
    case "available": return { tone: s.eligible ? "accent" : "warn", text: `Version ${s.version} is available${s.tier ? ` (${s.tier})` : ""}.` };
    case "deferred": return { tone: "warn", text: `Version ${s.version} will install later: ${s.reason.replace(/_/g, " ")}.` };
    case "downloading": return { tone: "accent", text: `Downloading ${s.version}… ${Math.round(s.pct)}%` };
    case "staged": return { tone: "ok", text: `Version ${s.version} is downloaded and waiting for your go-ahead.` };
    case "ready": return { tone: "ok", text: `Version ${s.version} is ready to install.` };
    case "error": return { tone: "bad", text: s.message };
  }
}

export interface UpdatesController {
  status: AutoUpdateStatus;
  /** True while a check or install is running (or the status is `checking` / `downloading`). */
  busy: boolean;
  /** Which action is in flight from this controller, if any. */
  pending: "check" | "install" | null;
  check(): Promise<void>;
  install(): Promise<void>;
}

/** State machine behind the Updates controls; use it to build custom update UI around the same client. */
export function useUpdates(client: UpdatesClient): UpdatesController {
  const [status, setStatus] = useState<AutoUpdateStatus>({ kind: "idle" });
  const [pending, setPending] = useState<"check" | "install" | null>(null);
  const clientRef = useRef(client);
  clientRef.current = client;
  useEffect(() => {
    // Seed once on mount; a client recreated every render must not reset the status.
    const initial = normalizeUpdates(clientRef.current).initial;
    if (initial === undefined) return;
    let live = true;
    void Promise.resolve(typeof initial === "function" ? initial() : initial).then((s) => { if (live && s) setStatus(s); }, () => {});
    return () => { live = false; };
  }, []);
  const run = useCallback(async (kind: "check" | "install") => {
    const n = normalizeUpdates(clientRef.current);
    setPending(kind);
    setStatus((prev) => (kind === "check" ? { kind: "checking" } : prev));
    try {
      setStatus(await (kind === "check" ? n.check() : n.install()));
    } catch (e) {
      setStatus({ kind: "error", message: errText(e) });
    } finally {
      setPending(null);
    }
  }, []);
  const check = useCallback(() => run("check"), [run]);
  const install = useCallback(() => run("install"), [run]);
  const busy = pending !== null || status.kind === "checking" || status.kind === "downloading";
  return { status, busy, pending, check, install };
}

export interface UpdatesControlsProps {
  /** Update client. Omit when passing a `controller` from `useUpdates`. */
  client?: UpdatesClient;
  /** Share state with surrounding UI (e.g. to show the status elsewhere). */
  controller?: UpdatesController;
  onRestart?: () => void | Promise<void>;
  upgradeHint?: ReactNode;
  labels?: Partial<UpdatesLabels>;
  /** Render the status sentence before the buttons. Default true. */
  showStatus?: boolean;
  /** Render the status badge. Default false. */
  showBadge?: boolean;
  className?: string;
}

function UpdatesControlsView({ c, o }: { c: UpdatesController; o: UpdatesControlsProps }) {
  const L = resolveUpdatesLabels(o.labels);
  const { status, busy, pending } = c;
  const line = statusLine(status);
  return (
    <div className={o.className ? `rk-sr__buttons ${o.className}` : "rk-sr__buttons"} data-update-status={status.kind}>
      {o.showStatus !== false ? <span className="rk-updates__status" role="status">{L.status(status, line.text)}</span> : null}
      {o.showBadge ? <Badge tone={line.tone}>{L.statusBadge(status, status.kind.replace(/_/g, " "))}</Badge> : null}
      {status.kind === "available" && !status.eligible && status.reason === "pro_required" && o.upgradeHint ? <span className="rk-updates__hint">{o.upgradeHint}</span> : null}
      {status.kind === "available" && status.eligible ? (
        <Button size="sm" variant="primary" disabled={busy} onClick={() => void c.install()}>{pending === "install" && L.installing ? L.installing : L.installUpdate}</Button>
      ) : null}
      {status.kind === "ready" && o.onRestart ? (
        <Button size="sm" variant="primary" onClick={() => void o.onRestart?.()}>{L.restartNow}</Button>
      ) : null}
      <Button size="sm" variant="secondary" disabled={busy} onClick={() => void c.check()}>{pending === "check" && L.checking ? L.checking : L.checkNow}</Button>
    </div>
  );
}

function OwnedUpdatesControls({ client, o }: { client: UpdatesClient; o: UpdatesControlsProps }) {
  return <UpdatesControlsView c={useUpdates(client)} o={o} />;
}

/**
 * The inner update controls (status sentence, Install / Restart, Check) without the Settings row, for embedding inside another
 * section's card. Pass a `client` (own state) or a shared `controller`.
 */
export function UpdatesControls(props: UpdatesControlsProps) {
  if (props.controller) return <UpdatesControlsView c={props.controller} o={props} />;
  if (!props.client) throw new Error("UpdatesControls needs a client or a controller");
  return <OwnedUpdatesControls client={props.client} o={props} />;
}

function UpdatesBody({ ctx, o }: { ctx: SettingsContext; o: UpdatesOptions }) {
  const controller = useUpdates(o.client);
  const { status } = controller;
  const L = updatesLabels(o);
  const line = statusLine(status);
  const auto = ctx.values["updates.autoInstall"] === true;
  return (
    <SettingsGroup>
      <SettingRow id="updates.status" label={L.version(o.currentVersion)} description={<span role="status" data-update-status={status.kind}>{L.status(status, line.text)}</span>}>
        <UpdatesControls controller={controller} onRestart={o.onRestart} labels={o.labels} showStatus={false} showBadge />
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
  const L = updatesLabels(options);
  return {
    id: "updates",
    icon: sectionIcons.updates,
    label: L.sectionLabel,
    group: "General",
    order: 200,
    keywords: ["version", "upgrade", "release"],
    entries: [
      { id: "updates.status", label: L.searchCheck, keywords: ["version", "upgrade"] },
      ...(options.supportsAutoInstall ? [{ id: "updates.auto", label: L.autoInstall, keywords: ["auto", "patch"] }] : []),
    ],
    render: (ctx) => <UpdatesBody ctx={ctx} o={options} />,
  };
}

/* ---- Licence -------------------------------------------------------------------------------- */

/** Every user-visible licence string. Unset keys keep the defaults shown; the `{...}` forms are functions so the app can reword or localise. */
export interface LicenseLabels {
  /** Nav label and page heading. Default "Licence". */
  sectionLabel?: string;
  /** Status row title. Default "Licence". */
  statusLabel?: string;
  /** Status row title while a licence is active (e.g. "Pro active"). Default: `statusLabel`. */
  activeStatusLabel?: string;
  /** Search entry labels. Defaults "Licence status" / "Activate licence key". */
  searchStatus?: string;
  searchActivate?: string;
  /** Status badge texts. Defaults "Free", "Not activated", "Invalid", "Expired". */
  free?: string;
  notActivated?: string;
  invalid?: string;
  expired?: string;
  /** Licence kind names. Defaults "Trial", "Subscription", "Lifetime". */
  trial?: string;
  subscription?: string;
  lifetime?: string;
  /** Status detail lines. `until` is a localised date or null. */
  freeDetail?: string;
  rejectedDetail?: string;
  activeDetail?: (kind: string, until: string | null) => string;
  expiredDetail?: (kind: string, until: string | null) => string;
  /** Deactivate row. Defaults "Deactivate this device", "Frees the seat so you can activate another device.", "Deactivate", "Confirm deactivate". */
  deactivateTitle?: string;
  deactivateHint?: string;
  deactivate?: string;
  confirmDeactivate?: string;
  /** Activate row. Defaults "Licence key", "Paste the key from your purchase email.", "Licence key" (aria), "XXXX-XXXX-XXXX-XXXX", "Activate". */
  keyTitle?: string;
  keyHint?: string;
  keyAria?: string;
  keyPlaceholder?: string;
  activate?: string;
  /** Button text while the activation call is in flight. Unset keeps `activate`. */
  activating?: string;
  /** Shown when the client returns a non-active licence without a message. Default "The key was not accepted." */
  notAccepted?: string;
  /** Purchase row. Defaults "Need a licence?", "Get a licence". */
  purchaseTitle?: string;
  purchase?: string;
}

/** Stable DOM ids the app can set (e.g. to keep QA selectors such as `#settings-pro`). Nothing is rendered unless set. */
export interface LicenseIds {
  /** Wraps the whole section body (plan slot included) in `<div id>`. */
  section?: string;
  /** The licence key input. */
  licenseKey?: string;
  /** The activate `<form>`. */
  form?: string;
  /** The Activate button. */
  activate?: string;
  /** The Deactivate button. */
  deactivate?: string;
  /** The status row's description (the `role="status"` span). */
  status?: string;
}

export interface LicenseOptions {
  client: LicenseClientLike;
  /** Awaited before `client.deactivate()` (e.g. suppress a bridge that would react to the licence going away). A throw aborts the deactivation and is shown. */
  beforeDeactivate?: () => Promise<void> | void;
  /** Ask for a second click before deactivating. Default true; false deactivates on the first click. */
  confirmDeactivate?: boolean;
  graceDays?: number;
  purchaseUrl?: string;
  openExternal?: (url: string) => void | Promise<void>;
  /** Display names for tiers, e.g. `{ pro: "Pro" }`. Falls back to the tier string. */
  tierLabels?: Record<string, string>;
  /** Plan / upsell panel rendered above the licence rows. */
  planSlot?: ReactNode;
  /** Fired after activate/deactivate so the app can refresh its entitlements. */
  onChange?: (lic: StoredLicense | null) => void;
  /** Reword every visible string. */
  labels?: LicenseLabels;
  /** Stable element ids. */
  ids?: LicenseIds;
}

function describeLicense(lic: StoredLicense | null, grace: number, L: LicenseLabels): { tone: "ok" | "warn" | "bad" | "neutral"; label: string; detail: string } {
  if (!lic || lic.status === "none") return { tone: "neutral", label: L.free ?? "Free", detail: L.freeDetail ?? "No licence key is active on this device." };
  if (lic.status === "invalid" || lic.status === "failed") return { tone: "bad", label: lic.status === "failed" ? (L.notActivated ?? "Not activated") : (L.invalid ?? "Invalid"), detail: lic.message ?? L.rejectedDetail ?? "The licence key was not accepted." };
  const valid = isLicensed(lic, grace);
  const until = lic.validUntil ? new Date(lic.validUntil).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }) : null;
  const kind = lic.licenseKind === "trial" ? (L.trial ?? "Trial") : lic.licenseKind === "term" ? (L.subscription ?? "Subscription") : (L.lifetime ?? "Lifetime");
  if (!valid) return { tone: "warn", label: L.expired ?? "Expired", detail: L.expiredDetail ? L.expiredDetail(kind, until) : until ? `This ${kind.toLowerCase()} ended on ${until}.` : "This licence is no longer valid." };
  return { tone: "ok", label: kind, detail: L.activeDetail ? L.activeDetail(kind, until) : until ? `${kind} valid until ${until}.` : `${kind} licence is active.` };
}

function LicenseBody({ ctx, o }: { ctx: SettingsContext; o: LicenseOptions }) {
  void ctx;
  const L = o.labels ?? {};
  const ids = o.ids ?? {};
  const [key, setKey] = useState("");
  const lic = useLicenseSection({ ...o, notAcceptedMessage: L.notAccepted });
  const grace = o.graceDays ?? 7;
  const info = describeLicense(lic.license, grace, L);
  const t = lic.tier;
  const active = lic.license?.status === "active";
  const note = lic.error;
  const busy = lic.busy;
  return (
    <SettingsGroup>
      <SettingRow id="license.status" label={active ? (L.activeStatusLabel ?? L.statusLabel ?? "Licence") : (L.statusLabel ?? "Licence")} description={<span role="status" id={ids.status}>{info.detail}</span>}>
        <div className="rk-sr__buttons">
          {t ? <Badge tone="accent">{o.tierLabels?.[t] ?? t}</Badge> : null}
          <Badge tone={info.tone}>{info.label}</Badge>
        </div>
      </SettingRow>
      {active ? (
        <SettingRow id="license.deactivate" label={L.deactivateTitle ?? "Deactivate this device"} description={note ? <span role="alert">{note}</span> : (L.deactivateHint ?? "Frees the seat so you can activate another device.")}>
          {o.confirmDeactivate === false ? (
            <Button id={ids.deactivate} size="sm" variant="ghost" disabled={busy} onClick={lic.requestDeactivate}>{L.deactivate ?? "Deactivate"}</Button>
          ) : (
            <Button id={ids.deactivate} size="sm" variant={lic.confirmPending ? "danger" : "ghost"} disabled={busy} onClick={lic.requestDeactivate}>
              {lic.confirmPending ? (L.confirmDeactivate ?? "Confirm deactivate") : (L.deactivate ?? "Deactivate")}
            </Button>
          )}
        </SettingRow>
      ) : (
        <SettingRow id="license.activate" label={L.keyTitle ?? "Licence key"} description={note ?? L.keyHint ?? "Paste the key from your purchase email."} stacked>
          <form
            id={ids.form}
            className="rk-sr__inline"
            onSubmit={(e) => {
              e.preventDefault();
              if (!key.trim()) return;
              void lic.activate(key).then((l) => {
                if (l) setKey("");
              });
            }}
          >
            <Input
              id={ids.licenseKey}
              aria-label={L.keyAria ?? "Licence key"}
              placeholder={L.keyPlaceholder ?? "XXXX-XXXX-XXXX-XXXX"}
              value={key}
              autoComplete="off"
              spellCheck={false}
              onChange={(e) => setKey(e.target.value)}
              aria-invalid={lic.license?.status === "invalid" || lic.license?.status === "failed" ? true : undefined}
            />
            <Button id={ids.activate} type="submit" variant="primary" size="sm" disabled={busy || !key.trim()}>{lic.activating && L.activating ? L.activating : (L.activate ?? "Activate")}</Button>
          </form>
        </SettingRow>
      )}
      {o.purchaseUrl && !active ? (
        <SettingRow label={L.purchaseTitle ?? "Need a licence?"}>
          <Button size="sm" variant="ghost" onClick={() => void o.openExternal?.(o.purchaseUrl!)}>{L.purchase ?? "Get a licence"}</Button>
        </SettingRow>
      ) : null}
    </SettingsGroup>
  );
}

export function licenseSection(options: LicenseOptions): SettingsSection {
  const L = options.labels ?? {};
  return {
    id: "license",
    icon: sectionIcons.license,
    label: L.sectionLabel ?? "Licence",
    group: "Account",
    order: 100,
    keywords: ["pro", "trial", "activate", "key", "purchase"],
    entries: [
      { id: "license.status", label: L.searchStatus ?? "Licence status", keywords: ["pro", "trial", "tier"] },
      { id: "license.activate", label: L.searchActivate ?? "Activate licence key", keywords: ["key"] },
    ],
    render: (ctx) => {
      const inner = (
        <>
          {options.planSlot}
          <LicenseBody ctx={ctx} o={options} />
        </>
      );
      return options.ids?.section ? <div id={options.ids.section}>{inner}</div> : inner;
    },
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

/* ---- Engine doctor (app-supplied checks) ---------------------------------------------------- */

export interface DoctorLabels {
  /** Nav label. Default "Engine check". */
  sectionLabel?: string;
  /** Page heading. Default: `sectionLabel`. */
  heading?: string;
  /** Group title above the rows. Unset renders none. */
  groupTitle?: string;
  /** Summary row title. Default "System check". */
  summaryTitle?: string;
  /** Summary before any run. Default "Run the checks to see the engine status." */
  idle?: string;
  /** Summary while any check runs. Default "Running diagnostics…" */
  running?: string;
  /** Summary after a run. Default `Last run: ${passed} of ${total} passed.` */
  summary?: (passed: number, total: number) => string;
  /** Run-all button text, idle and busy. Defaults "Run all", "Checking…". */
  runAll?: string;
  runAllBusy?: string;
  /** Per-check button text, idle and busy. Defaults "Re-run", "Checking…"; the button's aria-label is `${rerunAria ?? "Re-run"} ${label}`. */
  rerun?: string;
  rerunBusy?: string;
  rerunAria?: string;
  /** Status badge texts. Defaults "ok", "needs attention", "checking", "not run". */
  ok?: string;
  fail?: string;
  checking?: string;
  notRun?: string;
  /** Prefix for a failed check's `fix`. Default "Fix: ". */
  fixPrefix?: string;
  /** Wrap the check list in a `<details>` with this summary text. Unset lists the checks directly. */
  detailsLabel?: string;
  /** Search entry label. Default "Run engine checks". */
  searchLabel?: string;
}

/** Stable DOM ids the app can set. Nothing is rendered unless set. */
export interface DoctorIds {
  /** Wraps the whole section body. */
  section?: string;
  /** The summary text. */
  summary?: string;
  /** The run-all button. */
  runAll?: string;
  /** The check list (`<ul>`). */
  list?: string;
  /** Per-check `<li>` id. */
  check?: (checkId: string) => string;
  /** Per-check re-run button id. */
  rerun?: (checkId: string) => string;
}

export interface DoctorOptions {
  checks: ReadonlyArray<DoctorCheck>;
  /** Run every check once when the section first renders. Default false. */
  autoRun?: boolean;
  /** Called after each check finishes. */
  onResult?: (check: DoctorCheck, result: DoctorResult) => void;
  labels?: DoctorLabels;
  ids?: DoctorIds;
}

function DoctorBody({ o }: { o: DoctorOptions }) {
  const L = o.labels ?? {};
  const ids = o.ids ?? {};
  const doc = useDoctor(o.checks, { autoRun: o.autoRun, onResult: o.onResult });
  const runOne = (c: DoctorCheck) => void doc.runOne(c.id);
  const state = (id: string): DoctorStatus => doc.status[id] ?? { state: "idle" };
  const anyRunning = doc.running;
  const { passed } = doc.summary;
  const summary = anyRunning ? (L.running ?? "Running diagnostics…") : doc.summary.lastRun === null ? (L.idle ?? "Run the checks to see the engine status.") : L.summary ? L.summary(passed, o.checks.length) : `Last run: ${passed} of ${o.checks.length} passed.`;
  const list = (
    <ul className="rk-doctor__checks" id={ids.list}>
      {o.checks.map((c) => {
        const st = state(c.id);
        const result = st.state === "done" ? st.result : null;
        const tone = st.state === "running" ? "neutral" : result ? (result.ok ? "ok" : "bad") : "neutral";
        const text = st.state === "running" ? (L.checking ?? "checking") : result ? (result.ok ? (L.ok ?? "ok") : (L.fail ?? "needs attention")) : (L.notRun ?? "not run");
        const busy = st.state === "running";
        return (
          <li key={c.id} id={ids.check?.(c.id)} className="rk-doctor__check" data-check-id={c.id} data-status={busy ? "running" : result ? (result.ok ? "ok" : "fail") : "idle"}>
            <div className="rk-doctor__text">
              <span className="rk-doctor__label">{c.label}</span>
              {result?.detail || (!result && c.description) ? <span className="rk-doctor__detail">{result?.detail ?? c.description}</span> : null}
              {result && !result.ok && result.fix ? <span className="rk-doctor__fix">{(L.fixPrefix ?? "Fix: ") + result.fix}</span> : null}
            </div>
            <div className="rk-sr__buttons">
              <Badge tone={tone}>{text}</Badge>
              <Button id={ids.rerun?.(c.id)} size="sm" variant="ghost" aria-label={`${L.rerunAria ?? "Re-run"} ${c.label}`} disabled={busy} onClick={() => runOne(c)}>{busy ? (L.rerunBusy ?? "Checking…") : (L.rerun ?? "Re-run")}</Button>
            </div>
          </li>
        );
      })}
    </ul>
  );
  const body = (
    <SettingsGroup title={L.groupTitle}>
      <SettingRow id="doctor.run" label={L.summaryTitle ?? "System check"} description={<span role="status" id={ids.summary}>{summary}</span>}>
        <Button id={ids.runAll} size="sm" variant="secondary" disabled={anyRunning || o.checks.length === 0} onClick={() => void doc.runAll()}>{anyRunning ? (L.runAllBusy ?? "Checking…") : (L.runAll ?? "Run all")}</Button>
      </SettingRow>
      {L.detailsLabel ? <details className="rk-doctor__details"><summary>{L.detailsLabel}</summary>{list}</details> : list}
    </SettingsGroup>
  );
  return ids.section ? <div id={ids.section}>{body}</div> : body;
}

/**
 * Engine doctor: app-supplied checks with run-all, per-check re-run, status badges, labels and stable ids. The package
 * runs nothing itself; `run()` is yours (an IPC call, a sidecar probe). Add to `standardSections` as `doctor`.
 */
export function doctorSection(options: DoctorOptions): SettingsSection {
  const L = options.labels ?? {};
  const label = L.sectionLabel ?? "Engine check";
  return {
    id: "doctor",
    icon: sectionIcons.diagnostics,
    label,
    heading: L.heading ?? label,
    group: "Support",
    order: 150,
    keywords: ["doctor", "check", "health", "engine", "diagnose"],
    entries: [{ id: "doctor.run", label: L.searchLabel ?? "Run engine checks", keywords: ["doctor", "health"] }],
    render: () => <DoctorBody o={options} />,
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
  // rightkit-models 0.2.0 remote-only classes: no local backend, so nothing to download.
  rental_only: { tone: "neutral", text: "Runs on a rented GPU" },
  api: { tone: "neutral", text: "Runs through an API" },
};
const NOT_LOCAL = new Set(["unsupported", "rental_only", "api"]);

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
            label={<>{m.label}{m.active ? <> <Badge tone="accent">In use</Badge></> : null}{m.shared ? <> <Badge>Shared</Badge></> : null}</>}
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
              m.shared || m.removable === false ? null : <ConfirmButton label="Remove" confirmLabel="Confirm remove" onConfirm={() => void act(() => o.models.remove(m.id))} />
            ) : working ? (
              <Button size="sm" variant="ghost" onClick={() => void act(() => o.models.cancel(m.id))}>Cancel</Button>
            ) : (
              <Button size="sm" variant="secondary" disabled={NOT_LOCAL.has(m.fit?.kind ?? "")} onClick={() => void act(() => o.models.install(m.id))}>
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
  /** Replaces the About card (the default group, with its links, `extra` and copyright). `panels` still render below it. */
  card?: ReactNode;
  /**
   * Replaces the whole section body. Receives the defaults so the app can reuse any piece: `card` (the default card, or the
   * `card` option when set), `panels`, and the `info` line ("App 1.0.0 · build · platform").
   */
  render?: (defaults: AboutDefaults) => ReactNode;
}

export interface AboutDefaults {
  card: ReactNode;
  panels: ReactNode;
  info: string;
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
    render: () => {
      const defaultCard = (
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
      );
      const card = o.card ?? defaultCard;
      if (o.render) return <>{o.render({ card, panels: o.panels, info })}</>;
      return (
        <>
          {card}
          {o.panels}
        </>
      );
    },
  };
}
