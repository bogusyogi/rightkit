import { isPro as viewIsPro, licenseMessage, type LicenseView } from "@rightkit/license";
import { hostAppName } from "../appName";

/** US spelling for text that comes from `@rightkit/license`, whose messages say "licence". Retire
 * this wrapper once the package offers en-US wording. */
export function usLicenseWords(text: string): string {
  return text.replace(/\b([Ll])icence(s?)\b/g, "$1icense$2");
}

/** The words for a licence message code, in the UI's US English. The one place the app reads
 * `licenseMessage`; every other file calls this (`lib/license/errors.ts` re-exports it). It lives
 * in this module, which imports nothing relative, so the Node contract scripts can load it. */
export function licenseText(code: unknown): string {
  return usLicenseWords(licenseMessage(code));
}

export type LicenseLifecycleKind =
  | "checking"
  | "free"
  | "active_trial"
  | "expired_trial"
  | "active_lifetime_provisional"
  | "verification_required"
  | "active_lifetime"
  | "active_legacy"
  | "migration_required"
  | "clock_repair_required"
  | "invalid"
  | "storage_error";

export interface LicenseLifecycleSnapshot {
  kind: LicenseLifecycleKind;
  isPro: boolean;
  entitlementId?: string | null;
  expiresAtUnix?: number | null;
  confirmationDueAtUnix?: number | null;
  message?: string | null;
}

export interface LicenseLifecycleDialog {
  title: string;
  body: string;
  primaryAction: string;
  secondaryAction: string;
}

interface LicenseLifecycleDependencies {
  readSnapshot(): Promise<LicenseLifecycleSnapshot>;
  retryPendingRemovals(): Promise<unknown>;
  refreshServer(): Promise<unknown>;
  syncEntitlements(): Promise<unknown>;
}

const CHECK_FALLBACK_MS = 60_000;
const EXPIRED_TRIAL_ACK_KEY = "hr.expiredTrialNoticeAck.v1";
let dependencies: LicenseLifecycleDependencies | null = null;
let snapshot: LicenseLifecycleSnapshot = { kind: "checking", isPro: false };
let checkInFlight: Promise<LicenseLifecycleSnapshot> | null = null;
let boundaryTimer: number | null = null;
const listeners = new Set<(next: LicenseLifecycleSnapshot) => void>();

/** Project the native licence view onto the UI lifecycle snapshot the dialogs, chips and boundary
 * timers use. Display only: `isPro` here never authorises anything (native code does, at each paid
 * command). The view's times are Unix milliseconds; the snapshot keeps Unix seconds. A busy view
 * still shows its last committed plan (the native lock is only held briefly); an unresolved one
 * is a storage error, never Pro. */
export function snapshotFromLicenseView(view: LicenseView): LicenseLifecycleSnapshot {
  const seconds = (ms: number | null) => (ms == null ? null : Math.floor(ms / 1000));
  const message = view.message === "none" ? null : licenseText(view.message);
  if (view.unresolved) {
    return { kind: "storage_error", isPro: false, message };
  }
  const base = {
    isPro: viewIsPro({ ...view, busy: false }),
    entitlementId: view.licenseIdSuffix,
    expiresAtUnix: seconds(view.validUntil),
    confirmationDueAtUnix: seconds(view.confirmationDueAt),
    message,
  };
  const kinds: Record<LicenseView["lifecycle"], LicenseLifecycleKind> = {
    unlicensed: "free",
    migration_required: "migration_required",
    trial: "active_trial",
    expired_trial: "expired_trial",
    active: "active_legacy",
    grace: "active_legacy",
    expired: "free",
    lifetime_provisional: "active_lifetime_provisional",
    verification_required: "verification_required",
    lifetime: "active_lifetime",
    clock_repair_required: "clock_repair_required",
    revoked: "free",
    invalid: "invalid",
  };
  return { kind: kinds[view.lifecycle] ?? "invalid", ...base };
}

export function dialogForLicenseLifecycle(
  state: LicenseLifecycleSnapshot,
): LicenseLifecycleDialog | null {
  if (state.kind === "expired_trial") {
    return {
      title: "Your Pro trial has expired",
      body: `You can purchase Pro or continue using ${hostAppName()} for free.`,
      primaryAction: "Purchase Pro",
      secondaryAction: "Continue for free",
    };
  }
  if (state.kind === "verification_required") {
    return {
      title: "Connect once to verify your purchase",
      body: `${hostAppName()} needs one internet connection to confirm the purchase was not refunded. After confirmation, your lifetime license works offline.`,
      primaryAction: "Verify purchase",
      secondaryAction: "Continue with Free",
    };
  }
  if (state.kind === "clock_repair_required") {
    return {
      title: "Check your device clock",
      body: `Your device clock moved backwards. Correct the date and time, then connect once so ${hostAppName()} can verify the license safely.`,
      primaryAction: "Retry license check",
      secondaryAction: "Continue with Free",
    };
  }
  if (state.kind === "migration_required") {
    return {
      title: "Connect once to update your license",
      body: `${hostAppName()} needs one internet connection to move this older license into secure offline lifecycle storage.`,
      primaryAction: "Update license",
      secondaryAction: "Continue with Free",
    };
  }
  return null;
}

export function shouldRefreshLicenseFromServer(
  state: LicenseLifecycleSnapshot,
  explicitlyRequested: boolean,
): boolean {
  return explicitlyRequested && (
    state.kind === "verification_required" ||
    state.kind === "clock_repair_required" ||
    state.kind === "migration_required" ||
    state.kind === "active_legacy"
  );
}

export function millisecondsUntilLicenseCheck(
  state: LicenseLifecycleSnapshot,
  now = Date.now(),
): number {
  const boundary =
    state.kind === "active_trial"
      ? state.expiresAtUnix
      : state.kind === "active_lifetime_provisional"
        ? state.confirmationDueAtUnix
        : null;
  if (boundary == null) return CHECK_FALLBACK_MS;
  if (!Number.isFinite(boundary)) return 0;
  return Math.max(0, boundary * 1000 - now);
}

export function expiredTrialAcknowledgementKey(
  state: LicenseLifecycleSnapshot,
): string | null {
  if (state.kind !== "expired_trial" || state.expiresAtUnix == null) {
    return null;
  }
  // The view carries only the last characters of the licence id, and none for some records. "none"
  // keeps the notice acknowledgeable per expiry instead of returning on every launch.
  return `expired_trial:${state.entitlementId || "none"}:${state.expiresAtUnix}`;
}

/** Earlier builds stored the full licence id in the key. A key written for this licence and this
 * expiry still counts, so a user who already chose "Continue for free" is not asked again. */
function isLegacyExpiredTrialKeyForState(
  stored: string,
  state: LicenseLifecycleSnapshot,
): boolean {
  const prefix = "expired_trial:";
  const suffix = state.entitlementId;
  if (!suffix || state.expiresAtUnix == null || !stored.startsWith(prefix)) return false;
  const cut = stored.lastIndexOf(":");
  if (cut <= prefix.length - 1) return false;
  const id = stored.slice(prefix.length, cut);
  return id.endsWith(suffix) && stored.slice(cut + 1) === String(state.expiresAtUnix);
}

export function isExpiredTrialNoticeAcknowledged(state: LicenseLifecycleSnapshot): boolean {
  const key = expiredTrialAcknowledgementKey(state);
  if (!key || typeof localStorage === "undefined") return false;
  try {
    const stored = localStorage.getItem(EXPIRED_TRIAL_ACK_KEY);
    return stored === key || (stored !== null && isLegacyExpiredTrialKeyForState(stored, state));
  } catch {
    return false;
  }
}

export function acknowledgeExpiredTrialNotice(state: LicenseLifecycleSnapshot): void {
  const key = expiredTrialAcknowledgementKey(state);
  if (!key || typeof localStorage === "undefined") return;
  try {
    // This is only a non-secret UI acknowledgement. Credentials remain keyring-only.
    localStorage.setItem(EXPIRED_TRIAL_ACK_KEY, key);
  } catch {
    /* An unavailable browser store only means the notice may appear again. */
  }
}

export function configureLicenseLifecycle(next: LicenseLifecycleDependencies): void {
  dependencies = next;
}

export function getLicenseLifecycle(): LicenseLifecycleSnapshot {
  return snapshot;
}

export function subscribeLicenseLifecycle(
  listener: (next: LicenseLifecycleSnapshot) => void,
): () => void {
  listeners.add(listener);
  listener(snapshot);
  return () => listeners.delete(listener);
}

function publish(next: LicenseLifecycleSnapshot): LicenseLifecycleSnapshot {
  snapshot = next;
  for (const listener of listeners) listener(next);
  scheduleBoundaryCheck(next);
  return next;
}

function scheduleBoundaryCheck(next: LicenseLifecycleSnapshot): void {
  if (typeof window === "undefined") return;
  if (boundaryTimer !== null) window.clearTimeout(boundaryTimer);
  const delay = millisecondsUntilLicenseCheck(next);
  boundaryTimer = window.setTimeout(
    () => void refreshLicenseLifecycle({ verifyPurchase: true }),
    Math.min(Math.max(delay, 1), CHECK_FALLBACK_MS),
  );
}

export async function refreshLicenseLifecycle(
  options: { verifyPurchase?: boolean } = {},
): Promise<LicenseLifecycleSnapshot> {
  if (!dependencies) return snapshot;
  if (checkInFlight) return checkInFlight;

  checkInFlight = (async () => {
    try {
      // HR-PRO-010: every launch/focus/foreground pass retries server-side
      // removal work persisted before local Pro was cleared. Failure stays in
      // native secure storage & never blocks local Free use.
      await dependencies.retryPendingRemovals().catch(() => undefined);
      let next = await dependencies.readSnapshot();
      if (shouldRefreshLicenseFromServer(next, options.verifyPurchase === true)) {
        await dependencies.refreshServer().catch(() => undefined);
        next = await dependencies.readSnapshot();
      }

      // The trusted Rust gate already evaluates the new state. Reconfigure the
      // running sidecar before publishing UI so Pro -> Free is one transition.
      await dependencies.syncEntitlements();
      return publish(next);
    } catch (error) {
      return publish({
        kind: "storage_error",
        isPro: false,
        message: error instanceof Error ? error.message : String(error),
      });
    } finally {
      checkInFlight = null;
    }
  })();
  return checkInFlight;
}

export function startLicenseLifecycleMonitoring(): () => void {
  if (typeof window === "undefined" || typeof document === "undefined") return () => undefined;
  const recheck = () => void refreshLicenseLifecycle({ verifyPurchase: true });
  const onVisibility = () => {
    if (document.visibilityState === "visible") recheck();
  };

  window.addEventListener("focus", recheck);
  window.addEventListener("hr-license-updated", recheck);
  document.addEventListener("visibilitychange", onVisibility);
  recheck();

  return () => {
    window.removeEventListener("focus", recheck);
    window.removeEventListener("hr-license-updated", recheck);
    document.removeEventListener("visibilitychange", onVisibility);
    if (boundaryTimer !== null) window.clearTimeout(boundaryTimer);
    boundaryTimer = null;
  };
}
