import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Badge, Button, Input, Toggle } from "@rightkit/react-shell/react";
import { LICENSE_MESSAGES, isLicensed, licenseMessage, type LicenseView, type Lifecycle, type MessageCode } from "@rightkit/license";
import type { DiagnosticEntry, LogsClient } from "@rightkit/logs";
import type { AutoUpdateClient, AutoUpdateStatus } from "@rightkit/app-update";
import { errText } from "./errors.js";
import { useDoctor, useLicenseSection, type DoctorCheck, type DoctorResult, type DoctorStatus, type LicenseClientLike } from "./hooks.js";
import { sectionIcons } from "./icons.js";
import { ProgressBar, SettingRow, SettingsGroup, formatBytes } from "./primitives.js";
import type { ModelEntry, ModelsBackend, SecretPresence, SecretRef, SecretsBackend, SettingBinding, SettingsContext, SettingsSection } from "./types.js";


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
  /** Where the automatic-install choice is stored, when the host keeps it elsewhere. Default: the `updates.autoInstall` key. */
  autoInstallBinding?: SettingBinding<boolean>;
  /** Override any user-visible string. */
  labels?: Partial<UpdatesLabels>;
  /**
   * Replace the whole body. Receives the controller (status and actions) and the default card, so an app can keep any piece
   * and draw its own markup around the same state.
   */
  render?: (parts: { controller: UpdatesController; card: ReactNode }) => ReactNode;
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
  const autoBinding = o.autoInstallBinding;
  const auto = (autoBinding ? autoBinding.read(ctx) : ctx.values["updates.autoInstall"]) === true;
  const card = (
    <SettingsGroup>
      <SettingRow id="updates.status" label={L.version(o.currentVersion)} description={<span role="status" data-update-status={status.kind}>{L.status(status, line.text)}</span>}>
        <UpdatesControls controller={controller} onRestart={o.onRestart} labels={o.labels} showStatus={false} showBadge />
      </SettingRow>
      {status.kind === "available" && !status.eligible && status.reason === "pro_required" && o.upgradeHint ? (
        <SettingRow label={L.proUpdate} description={o.upgradeHint} />
      ) : null}
      {o.supportsAutoInstall ? (
        <SettingRow id="updates.auto" label={L.autoInstall} description={L.autoInstallDescription}>
          <Toggle label={L.autoInstall} checked={auto} onChange={(v) => void (autoBinding ? autoBinding.write(ctx, v) : ctx.set("updates.autoInstall", v))} />
        </SettingRow>
      ) : null}
    </SettingsGroup>
  );
  return o.render ? <>{o.render({ controller, card })}</> : card;
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
  /** Status badge texts. Defaults "Free", "Invalid", "Revoked", "Expired", "Action needed". */
  free?: string;
  invalid?: string;
  revoked?: string;
  expired?: string;
  /** Badge for a licence that needs an online refresh or a clock fix. Default "Action needed". */
  attention?: string;
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
  /** Shown when the client rejects the key (its `keyNotAccepted` code, or no code). Default: the package's keyNotAccepted text. */
  notAccepted?: string;
  /** Purchase row. Defaults "Need a licence?", "Get a licence". */
  purchaseTitle?: string;
  /** Verify-purchase row (shown only when the client has `refresh()` and the view needs verification). Defaults "Licence check", "Check this purchase with the licence server.", "Verify purchase". */
  verifyTitle?: string;
  /** Provisional and staleness lines (shown when the view sets `confirmationDueAt` / `staleSince`). Each is the prefix followed by the date. Defaults "Confirm by", "Could not verify online since". `staleSince` is when online verification first failed, not the last success. */
  confirmBy?: string;
  unverifiedSince?: string;
  verifyHint?: string;
  verify?: string;
  purchase?: string;
  /** Shown while the client resolves and until the first read settles. Default "Checking licence…". */
  loading?: string;
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
  /** The "Verify purchase" button. */
  verify?: string;
}

/** What a `plan` function receives. `licensed` is any active licence; `pro` is the Pro tier or above. */
export interface LicensePlanState {
  view: LicenseView | null;
  tier: string | null;
  pro: boolean;
  licensed: boolean;
}

/** The client as the licence section takes it: the object, a promise of it, or a factory that resolves it. */
export type LicenseClientSource = LicenseClientLike | Promise<LicenseClientLike> | (() => Promise<LicenseClientLike>);

export interface LicenseOptions {
  client: LicenseClientSource;
  /** Awaited before `client.deactivate()` (e.g. suppress a bridge that would react to the licence going away). A throw aborts the deactivation and is shown. */
  beforeDeactivate?: () => Promise<void> | void;
  /** Ask for a second click before deactivating. Default true; false deactivates on the first click. */
  confirmDeactivate?: boolean;
  purchaseUrl?: string;
  openExternal?: (url: string) => void | Promise<void>;
  /** Display names for tiers, e.g. `{ pro: "Pro" }`. Falls back to the tier string. */
  tierLabels?: Record<string, string>;
  /** Plan / upsell panel rendered above the licence rows. Shown only once the first read has settled. */
  planSlot?: ReactNode;
  /**
   * Plan wording, rendered above the rows in place of `planSlot`. A function receives the licence state, so the app can
   * tell Pro from any other licensed plan (`pro`) and name the tier (`tier`). Not shown until the first read settles.
   */
  plan?: ReactNode | ((state: LicensePlanState) => ReactNode);
  /** Fired after activate/deactivate so the app can refresh its licence state. */
  onChange?: (view: LicenseView | null) => void;
  /** Reword every visible string. */
  labels?: LicenseLabels;
  /** Stable element ids. */
  ids?: LicenseIds;
}

type Tone = "ok" | "warn" | "bad" | "neutral";

/** Lifecycles that need an online refresh or a clock fix; their detail comes from the package's fixed message codes. */
const ATTENTION: Partial<Record<Lifecycle, MessageCode>> = {
  migration_required: "migrationRequired",
  verification_required: "verifyFailed",
  clock_repair_required: "clockRepairRequired",
};

/** Badge and detail for a view. Every string is a label option or a fixed package message; server text is never shown. */
/** Short date for a licence time (Unix milliseconds from the view). */
function formatLicenseDate(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

/** Provisional and staleness lines: each shows only when its view field is set. */
function licenseDetailLines(view: LicenseView | null, L: LicenseLabels): string[] {
  const lines: string[] = [];
  if (view?.confirmationDueAt != null) lines.push(`${L.confirmBy ?? "Confirm by"} ${formatLicenseDate(view.confirmationDueAt)}`);
  if (view?.staleSince != null) lines.push(`${L.unverifiedSince ?? "Could not verify online since"} ${formatLicenseDate(view.staleSince)}`);
  return lines;
}

function describeLicense(view: LicenseView | null, L: LicenseLabels): { tone: Tone; label: string; detail: string } {
  if (!view || view.status === "none" || view.lifecycle === "unlicensed") return { tone: "neutral", label: L.free ?? "Free", detail: L.freeDetail ?? "No licence key is active on this device." };
  const message = view.message === "none" ? null : licenseMessage(view.message);
  if (view.lifecycle === "revoked") return { tone: "bad", label: L.revoked ?? "Revoked", detail: message ?? LICENSE_MESSAGES.revoked };
  if (view.lifecycle === "invalid") return { tone: "bad", label: L.invalid ?? "Invalid", detail: message ?? L.rejectedDetail ?? LICENSE_MESSAGES.invalidToken };
  const until = view.validUntil === null ? null : formatLicenseDate(view.validUntil);
  const kind = view.kind === "trial" ? (L.trial ?? "Trial") : view.kind === "term" ? (L.subscription ?? "Subscription") : (L.lifetime ?? "Lifetime");
  if (view.lifecycle === "expired" || view.lifecycle === "expired_trial") {
    return { tone: "warn", label: L.expired ?? "Expired", detail: L.expiredDetail ? L.expiredDetail(kind, until) : until ? `This ${kind.toLowerCase()} ended on ${until}.` : (message ?? LICENSE_MESSAGES.expired) };
  }
  const attention = ATTENTION[view.lifecycle];
  if (attention) return { tone: "warn", label: L.attention ?? "Action needed", detail: message ?? licenseMessage(attention) };
  if (isLicensed(view)) return { tone: "ok", label: kind, detail: L.activeDetail ? L.activeDetail(kind, until) : until ? `${kind} valid until ${until}.` : `${kind} licence is active.` };
  return { tone: "warn", label: L.attention ?? "Action needed", detail: message ?? licenseMessage(view.unresolved ? "unresolved" : view.busy ? "busy" : "unknown") };
}

function isLicenseClient(source: LicenseClientSource): source is LicenseClientLike {
  return typeof source === "object" && source !== null && typeof (source as LicenseClientLike).activate === "function";
}

/** Resolves the section's client source. `client` stays null until it resolves; `failed` is set when the promise or factory rejects. */
function useLicenseClient(source: LicenseClientSource): { client: LicenseClientLike | null; failed: boolean } {
  const [state, setState] = useState<{ client: LicenseClientLike | null; failed: boolean }>(() => (isLicenseClient(source) ? { client: source, failed: false } : { client: null, failed: false }));
  useEffect(() => {
    if (isLicenseClient(source)) {
      setState({ client: source, failed: false });
      return;
    }
    let live = true;
    let pending: Promise<LicenseClientLike>;
    try {
      pending = typeof source === "function" ? source() : source;
    } catch (e) {
      pending = Promise.reject(e);
    }
    void pending.then(
      (client) => {
        if (live) setState({ client, failed: false });
      },
      () => {
        if (live) setState({ client: null, failed: true });
      },
    );
    return () => {
      live = false;
    };
  }, [source]);
  return state;
}

function LicenseStatusRow({ o, text }: { o: LicenseOptions; text: string }) {
  const L = o.labels ?? {};
  return (
    <SettingsGroup>
      <SettingRow id="license.status" label={L.statusLabel ?? "Licence"} description={<span role="status" id={o.ids?.status}>{text}</span>} />
    </SettingsGroup>
  );
}

function LicenseBody({ o }: { o: LicenseOptions }) {
  const { client, failed } = useLicenseClient(o.client);
  if (!client) return <LicenseStatusRow o={o} text={failed ? LICENSE_MESSAGES.unknown : (o.labels?.loading ?? "Checking licence…")} />;
  return <LicenseLoaded client={client} o={o} />;
}

function LicenseLoaded({ client, o }: { client: LicenseClientLike; o: LicenseOptions }) {
  const L = o.labels ?? {};
  const ids = o.ids ?? {};
  const [key, setKey] = useState("");
  const lic = useLicenseSection({ client, beforeDeactivate: o.beforeDeactivate, confirmDeactivate: o.confirmDeactivate, notAcceptedMessage: L.notAccepted, onChange: o.onChange });
  // Until the first read settles a null view means "not read yet", never "Free".
  if (!lic.loaded) return <LicenseStatusRow o={o} text={L.loading ?? "Checking licence…"} />;
  const info = describeLicense(lic.license, L);
  const detailLines = licenseDetailLines(lic.license, L);
  const t = lic.tier;
  const active = isLicensed(lic.license);
  const holds = lic.license?.revisionState === "active";
  const note = lic.error;
  const busy = lic.busy;
  const planState: LicensePlanState = { view: lic.license, tier: t, pro: lic.pro, licensed: active };
  const plan = o.plan === undefined ? o.planSlot : typeof o.plan === "function" ? o.plan(planState) : o.plan;
  return (
    <>
      {plan}
      <SettingsGroup>
        <SettingRow id="license.status" label={active ? (L.activeStatusLabel ?? L.statusLabel ?? "Licence") : (L.statusLabel ?? "Licence")} description={<><span role="status" id={ids.status}>{info.detail}</span>{detailLines.map((line) => <div key={line} className="rk-sr__extra">{line}</div>)}</>}>
          <div className="rk-sr__buttons">
            {t ? <Badge tone="accent">{o.tierLabels?.[t] ?? t}</Badge> : null}
            <Badge tone={info.tone}>{info.label}</Badge>
          </div>
        </SettingRow>
        {lic.canVerify ? (
          <SettingRow id="license.verify" label={L.verifyTitle ?? "Licence check"} description={lic.verifyError ? <span role="alert">{lic.verifyError}</span> : (L.verifyHint ?? "Check this purchase with the licence server.")}>
            <Button id={ids.verify} size="sm" variant="ghost" disabled={lic.verifying} onClick={() => void lic.verify()}>{L.verify ?? "Verify purchase"}</Button>
          </SettingRow>
        ) : null}
        {holds ? (
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
                aria-invalid={lic.license?.status === "invalid" ? true : undefined}
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
    </>
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
    render: () => (options.ids?.section ? <div id={options.ids.section}><LicenseBody o={options} /></div> : <LicenseBody o={options} />),
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
  /** Replace the whole body. Receives the default card and the acceptance sentence (null when no acceptance is known). */
  render?: (parts: { card: ReactNode; acceptedNote: string | null }) => ReactNode;
}

export function legalSection(o: LegalOptions): SettingsSection {
  const acceptedNote = o.acceptedAt ? `You accepted these on ${new Date(o.acceptedAt).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" })}${o.acceptedVersion ? ` (version ${o.acceptedVersion})` : ""}.` : null;
  return {
    id: "legal",
    icon: sectionIcons.legal,
    label: "Legal",
    group: "Account",
    order: 200,
    keywords: ["terms", "privacy", "agreement", "eula"],
    entries: o.documents.map((d, i) => ({ id: `legal.doc.${i}`, label: d.title, keywords: ["terms", "privacy"] })),
    render: () => {
      const card = (
        <SettingsGroup description={acceptedNote ?? undefined}>
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
      );
      return o.render ? <>{o.render({ card, acceptedNote })}</> : card;
    },
  };
}

/* ---- Diagnostics & logs --------------------------------------------------------------------- */

/** Email-support action: the report is copied, then a prefilled `mailto:` opens. */
export interface DiagnosticsEmailSupport {
  /** Recipient address. */
  address: string;
  subject: string;
  /** Body text. Say that the report is on the clipboard; the user pastes it in. */
  body: string;
}

/** The end of one log file, for the preview. Log text is content-free by the app's own field policy. */
export interface LogFileTail {
  name: string;
  path: string;
  tail: string;
}

export interface DiagnosticsLogFiles {
  /** Tail text of each log file. Called on mount and on Refresh. */
  list(): Promise<LogFileTail[]>;
  /** Reveals a log file in the system file manager. Omit to hide the Reveal buttons. */
  reveal?(path: string): void | Promise<void>;
}

export interface DiagnosticsOptions {
  logs: LogsClient;
  /** Copy text to the clipboard; defaults to `navigator.clipboard`. */
  copy?: (text: string) => void | Promise<void>;
  /** Email-support action (copy the report, then open the mail link). Omit to hide the action. */
  emailSupport?: DiagnosticsEmailSupport;
  /** Opens the mail link; defaults to `window.location.assign`. Hosts that block `mailto:` in the webview pass their opener. */
  openExternal?: (url: string) => void | Promise<void>;
  /** The renderer ring log, shown inline by "View". Called on open, on Refresh and after Clear. */
  entries?: () => DiagnosticEntry[];
  /** Multi-file log tail preview with reveal. */
  logFiles?: DiagnosticsLogFiles;
}

function DiagnosticsBody({ ctx, o }: { ctx: SettingsContext; o: DiagnosticsOptions }) {
  const [report, setReport] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [showLog, setShowLog] = useState(false);
  const [ring, setRing] = useState<DiagnosticEntry[]>([]);
  const [tails, setTails] = useState<LogFileTail[] | null>(null);
  const copy = o.copy ?? ((t: string) => navigator.clipboard.writeText(t));
  const { logFiles, entries, emailSupport } = o;
  useEffect(() => {
    if (logFiles) void logFiles.list().then(setTails, (e) => setNote(errText(e)));
  }, [logFiles]);
  const readTails = () => {
    if (logFiles) void logFiles.list().then(setTails, (e) => setNote(errText(e)));
  };
  const readRing = () => {
    if (entries) setRing(entries());
  };
  // Flush, then collect; the report is kept so Copy and the email action reuse it.
  const collect = async (): Promise<string> => {
    await o.logs.flush();
    const text = JSON.stringify(await o.logs.collectDiagnostics(), null, 2);
    setReport(text);
    return text;
  };
  const sendEmail = async (email: DiagnosticsEmailSupport) => {
    setBusy(true);
    setNote(null);
    try {
      const text = report ?? (await collect());
      await copy(text);
      setNote("Report copied. Paste it into the email.");
      const url = `mailto:${email.address}?subject=${encodeURIComponent(email.subject)}&body=${encodeURIComponent(email.body)}`;
      await (o.openExternal ? o.openExternal(url) : window.location.assign(url));
    } catch (e) {
      setNote(errText(e));
    } finally {
      setBusy(false);
    }
  };
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
                void collect()
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
        {emailSupport ? (
          <SettingRow id="diagnostics.email" label="Email to support" description="Copies the report to the clipboard, then opens an email to support. Paste the report into it.">
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => void sendEmail(emailSupport)}>Email to support</Button>
          </SettingRow>
        ) : null}
        {entries ? (
          <SettingRow id="diagnostics.log" label="Recent log" description={showLog ? `${ring.length} recent renderer events.` : "Show the renderer's recent events here."}>
            <div className="rk-sr__buttons">
              {showLog ? <Button size="sm" variant="ghost" onClick={readRing}>Refresh</Button> : null}
              <Button
                size="sm"
                variant="secondary"
                aria-expanded={showLog}
                onClick={() => {
                  if (!showLog) readRing();
                  setShowLog((v) => !v);
                }}
              >
                {showLog ? "Hide" : "View"}
              </Button>
            </div>
          </SettingRow>
        ) : null}
        <SettingRow id="diagnostics.clear" label="Clear logs" description="Deletes the log files on this device.">
          <ConfirmButton
            label="Clear"
            disabled={busy}
            onConfirm={() =>
              void o.logs.clear().then(
                () => {
                  setReport(null);
                  setNote("Logs cleared.");
                  readRing();
                  readTails();
                },
                (e) => setNote(errText(e)),
              )
            }
          />
        </SettingRow>
      </SettingsGroup>
      {report ? <pre className="rk-diag" aria-label="Diagnostics report" tabIndex={0}>{report}</pre> : null}
      {entries && showLog ? (
        ring.length === 0 ? (
          <p className="rk-sg__desc">No renderer events recorded yet.</p>
        ) : (
          <ol className="rk-diag rk-diag__entries" aria-label="Recent log" tabIndex={0}>
            {ring.map((e, i) => (
              <li key={`${e.ts_ms}-${i}`}>
                <time dateTime={new Date(e.ts_ms).toISOString()}>{new Date(e.ts_ms).toLocaleTimeString()}</time> <span className="rk-diag__kind">{e.kind}</span> <span>{e.event}</span>
                {e.duration_ms !== undefined ? ` · ${e.duration_ms} ms` : null}
              </li>
            ))}
          </ol>
        )
      ) : null}
      {logFiles && tails ? (
        <SettingsGroup title="Log files" description="The end of each log file on this device.">
          {tails.map((f, i) => (
            <SettingRow
              key={f.path}
              id={`diagnostics.files.${i}`}
              label={f.name}
              stacked={!logFiles.reveal}
              description={<pre className="rk-diag rk-diag__tail" aria-label={`${f.name} tail`} tabIndex={0}>{f.tail || "(empty)"}</pre>}
            >
              {logFiles.reveal ? (
                <Button size="sm" variant="ghost" aria-label={`Reveal ${f.name}`} onClick={() => void logFiles.reveal?.(f.path)}>Reveal</Button>
              ) : null}
            </SettingRow>
          ))}
        </SettingsGroup>
      ) : null}
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
      ...(options.emailSupport ? [{ id: "diagnostics.email", label: "Email to support", keywords: ["support", "mail", "report"] }] : []),
      ...(options.entries ? [{ id: "diagnostics.log", label: "Recent log", keywords: ["ring", "events", "renderer"] }] : []),
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

export interface DoctorOptions<P = undefined> {
  checks: ReadonlyArray<DoctorCheck<P>>;
  /** Run every check once when the section first renders. Default false. */
  autoRun?: boolean;
  /** Called after each check finishes. */
  onResult?: (check: DoctorCheck<P>, result: DoctorResult) => void;
  /**
   * One expensive probe shared by every check (for example a single engine `doctor` call). Concurrent checks share one call;
   * each check's `run` receives its result. See `useDoctor`.
   */
  probe?: () => Promise<P>;
  /** Content under the summary row, rendered from the latest probe report once one has settled (for example engine notes). */
  notes?: (report: P) => ReactNode;
  labels?: DoctorLabels;
  ids?: DoctorIds;
}

function DoctorBody<P>({ o }: { o: DoctorOptions<P> }) {
  const L = o.labels ?? {};
  const ids = o.ids ?? {};
  const doc = useDoctor<P>(o.checks, { autoRun: o.autoRun, onResult: o.onResult, probe: o.probe });
  const runOne = (c: DoctorCheck<P>) => void doc.runOne(c.id);
  const notes = o.notes && doc.report !== null ? o.notes(doc.report) : null;
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
      {notes ? <div className="rk-doctor__notes">{notes}</div> : null}
    </SettingsGroup>
  );
  return ids.section ? <div id={ids.section}>{body}</div> : body;
}

/**
 * Engine doctor: app-supplied checks with run-all, per-check re-run, status badges, labels and stable ids. The package
 * runs nothing itself; `run()` is yours (an IPC call, a sidecar probe). Add to `standardSections` as `doctor`.
 */
export function doctorSection<P = undefined>(options: DoctorOptions<P>): SettingsSection {
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
  /**
   * Per-model disable. Return a reason to disable Download for that model; the reason shows in its row. Return false or
   * nothing to keep it enabled. An installed model keeps its status (shown while disabled) and Remove.
   */
  isDisabled?: (model: ModelEntry) => string | false | null | undefined;
  /** Extra controls rendered after a row's main action (for example "Make active"). */
  actions?: (model: ModelEntry) => ReactNode;
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
        const installed = m.state === "installed";
        const reason = o.isDisabled?.(m) || null;
        return (
          <SettingRow
            key={m.id}
            id={`models.${m.id}`}
            label={<>{m.label}{m.active ? <> <Badge tone="accent">In use</Badge></> : null}{m.shared ? <> <Badge>Shared</Badge></> : null}</>}
            description={
              <>
                {[m.description, m.sizeBytes ? formatBytes(m.sizeBytes) : null].filter(Boolean).join(" · ")}
                {fit ? <> {" "}<Badge tone={fit.tone}>{fit.text}</Badge></> : null}
                {reason ? <> {" "}<span className="rk-sr__disabled-reason">{reason}</span></> : null}
                {reason && installed ? <> {" "}<Badge tone="ok">Installed</Badge></> : null}
                {m.state === "error" && m.error ? <span className="rk-sr__err" role="alert"> {m.error.message}</span> : null}
                {working ? <ProgressBar value={m.progress ?? 0} label={`${m.label} ${m.state}`} /> : null}
              </>
            }
          >
            {installed ? (
              m.shared || m.removable === false ? null : <ConfirmButton label="Remove" confirmLabel="Confirm remove" onConfirm={() => void act(() => o.models.remove(m.id))} />
            ) : working ? (
              <Button size="sm" variant="ghost" onClick={() => void act(() => o.models.cancel(m.id))}>Cancel</Button>
            ) : (
              <Button size="sm" variant="secondary" disabled={!!reason || NOT_LOCAL.has(m.fit?.kind ?? "")} onClick={() => void act(() => o.models.install(m.id))}>
                {m.state === "error" ? "Retry" : "Download"}
              </Button>
            )}
            {o.actions ? o.actions(m) : null}
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
