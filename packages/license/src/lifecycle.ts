/**
 * The licence lifecycle as the UI follows it: a display snapshot of the native view, the notices a state asks
 * for, and a monitor that re-reads the licence at launch, on focus, at the next boundary and on every change.
 * Display only: nothing here authorises anything; native code does, at each paid command.
 */
// `index.ts` re-exports this module, so these bindings are read at call time only, never while the module loads.
import { isPro, licenseMessage, type LicenseMessageOverrides, type LicenseView } from './index.js';

export type LicenseLifecycleKind =
  | 'checking'
  | 'free'
  | 'active_trial'
  | 'expired_trial'
  | 'active_lifetime_provisional'
  | 'verification_required'
  | 'active_lifetime'
  | 'active_legacy'
  | 'migration_required'
  | 'clock_repair_required'
  | 'invalid'
  | 'storage_error';

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

const CHECK_FALLBACK_MS = 60_000;

/**
 * Project the native licence view onto the UI lifecycle snapshot the dialogs, chips and boundary timers use.
 * `isPro` here never authorises anything. The view's times are Unix milliseconds; the snapshot keeps Unix
 * seconds. A busy view still shows its last committed plan (the native lock is only held briefly); an
 * unresolved one is a storage error, never Pro. `messages` is the host's wording for the view's message code.
 */
export function snapshotFromLicenseView(view: LicenseView, messages?: LicenseMessageOverrides): LicenseLifecycleSnapshot {
  const seconds = (ms: number | null) => (ms == null ? null : Math.floor(ms / 1000));
  const message = view.message === 'none' ? null : licenseMessage(view.message, messages);
  if (view.unresolved) {
    return { kind: 'storage_error', isPro: false, message };
  }
  const base = {
    isPro: isPro({ ...view, busy: false }),
    entitlementId: view.licenseIdSuffix,
    expiresAtUnix: seconds(view.validUntil),
    confirmationDueAtUnix: seconds(view.confirmationDueAt),
    message,
  };
  const kinds: Record<LicenseView['lifecycle'], LicenseLifecycleKind> = {
    unlicensed: 'free',
    migration_required: 'migration_required',
    trial: 'active_trial',
    expired_trial: 'expired_trial',
    active: 'active_legacy',
    grace: 'active_legacy',
    expired: 'free',
    lifetime_provisional: 'active_lifetime_provisional',
    verification_required: 'verification_required',
    lifetime: 'active_lifetime',
    clock_repair_required: 'clock_repair_required',
    revoked: 'free',
    invalid: 'invalid',
  };
  return { kind: kinds[view.lifecycle] ?? 'invalid', ...base };
}

export interface LicenseLifecycleDialogOptions {
  /** The app's name as the copy says it. */
  appName: string;
  /** The paid plan's name as the copy says it, for example the one a trial gives. */
  paidTier: string;
  /** `en-US` writes "license"; the default `en-GB` writes "licence", as `LICENSE_MESSAGES` does. */
  spelling?: 'en-GB' | 'en-US';
}

/** The notice a lifecycle state asks the app to show, or null when it asks for none. */
export function dialogForLicenseLifecycle(
  state: LicenseLifecycleSnapshot,
  { appName, paidTier, spelling = 'en-GB' }: LicenseLifecycleDialogOptions,
): LicenseLifecycleDialog | null {
  const licence = spelling === 'en-US' ? 'license' : 'licence';
  if (state.kind === 'expired_trial') {
    return {
      title: `Your ${paidTier} trial has expired`,
      body: `You can purchase ${paidTier} or continue using ${appName} for free.`,
      primaryAction: `Purchase ${paidTier}`,
      secondaryAction: 'Continue for free',
    };
  }
  if (state.kind === 'verification_required') {
    return {
      title: 'Connect once to verify your purchase',
      body: `${appName} needs one internet connection to confirm the purchase was not refunded. After confirmation, your lifetime ${licence} works offline.`,
      primaryAction: 'Verify purchase',
      secondaryAction: 'Continue with Free',
    };
  }
  if (state.kind === 'clock_repair_required') {
    return {
      title: 'Check your device clock',
      body: `Your device clock moved backwards. Correct the date and time, then connect once so ${appName} can verify the ${licence} safely.`,
      primaryAction: `Retry ${licence} check`,
      secondaryAction: 'Continue with Free',
    };
  }
  if (state.kind === 'migration_required') {
    return {
      title: `Connect once to update your ${licence}`,
      body: `${appName} needs one internet connection to move this older ${licence} into secure offline lifecycle storage.`,
      primaryAction: `Update ${licence}`,
      secondaryAction: 'Continue with Free',
    };
  }
  return null;
}

/** A server refresh is asked for only by an explicit check, and only in a state one online answer can settle or renew. */
export function shouldRefreshLicenseFromServer(
  state: LicenseLifecycleSnapshot,
  explicitlyRequested: boolean,
): boolean {
  return explicitlyRequested && (
    state.kind === 'verification_required' ||
    state.kind === 'clock_repair_required' ||
    state.kind === 'migration_required' ||
    state.kind === 'active_legacy'
  );
}

/** Milliseconds until the state's next boundary (a trial's end, a provisional purchase's confirmation date); `fallbackMs` when it has none. */
export function millisecondsUntilLicenseCheck(
  state: LicenseLifecycleSnapshot,
  now = Date.now(),
  fallbackMs = CHECK_FALLBACK_MS,
): number {
  const boundary =
    state.kind === 'active_trial'
      ? state.expiresAtUnix
      : state.kind === 'active_lifetime_provisional'
        ? state.confirmationDueAtUnix
        : null;
  if (boundary == null) return fallbackMs;
  if (!Number.isFinite(boundary)) return 0;
  return Math.max(0, boundary * 1000 - now);
}

export function expiredTrialAcknowledgementKey(
  state: LicenseLifecycleSnapshot,
): string | null {
  if (state.kind !== 'expired_trial' || state.expiresAtUnix == null) {
    return null;
  }
  // The view carries only the last characters of the licence id, and none for some records. "none"
  // keeps the notice acknowledgeable per expiry instead of returning on every launch.
  return `expired_trial:${state.entitlementId || 'none'}:${state.expiresAtUnix}`;
}

/**
 * Earlier builds stored the full licence id in the key. A key written for this licence and this
 * expiry still counts, so a user who already chose to continue for free is not asked again.
 */
function isLegacyExpiredTrialKeyForState(
  stored: string,
  state: LicenseLifecycleSnapshot,
): boolean {
  const prefix = 'expired_trial:';
  const suffix = state.entitlementId;
  if (!suffix || state.expiresAtUnix == null || !stored.startsWith(prefix)) return false;
  const cut = stored.lastIndexOf(':');
  if (cut <= prefix.length - 1) return false;
  const id = stored.slice(prefix.length, cut);
  return id.endsWith(suffix) && stored.slice(cut + 1) === String(state.expiresAtUnix);
}

/** Where the expired-trial notice's acknowledgement is kept. */
export interface ExpiredTrialNoticeStore {
  /** The app's own key. An app that already ships one keeps it, so earlier acknowledgements still count. */
  storageKey: string;
  /** Defaults to `localStorage`. */
  storage?: Pick<Storage, 'getItem' | 'setItem'>;
}

export function isExpiredTrialNoticeAcknowledged(
  state: LicenseLifecycleSnapshot,
  { storageKey, storage }: ExpiredTrialNoticeStore,
): boolean {
  const key = expiredTrialAcknowledgementKey(state);
  if (!key) return false;
  try {
    const store = storage ?? (typeof localStorage === 'undefined' ? undefined : localStorage);
    if (!store) return false;
    const stored = store.getItem(storageKey);
    return stored === key || (stored !== null && isLegacyExpiredTrialKeyForState(stored, state));
  } catch {
    return false;
  }
}

export function acknowledgeExpiredTrialNotice(
  state: LicenseLifecycleSnapshot,
  { storageKey, storage }: ExpiredTrialNoticeStore,
): void {
  const key = expiredTrialAcknowledgementKey(state);
  if (!key) return;
  try {
    // This is only a non-secret UI acknowledgement. Credentials remain native.
    (storage ?? (typeof localStorage === 'undefined' ? undefined : localStorage))?.setItem(storageKey, key);
  } catch {
    /* An unavailable browser store only means the notice may appear again. */
  }
}

/** What the monitor needs of the licence client; `createLicenseClient`'s result is one. */
export interface LicenseLifecycleClient {
  status(): Promise<LicenseView>;
  refresh(): Promise<unknown>;
  retryPendingReleases(): Promise<unknown>;
  onChange?(callback: (view: LicenseView) => void): Promise<() => void>;
}

export interface LicenseLifecycleMonitorOptions {
  client: LicenseLifecycleClient;
  /**
   * Runs after each read and before subscribers hear of it, so the app applies a new state (for example
   * re-pushes entitlements to its engine) and a change of plan is one transition. A rejection publishes
   * `storage_error`.
   */
  afterChange?: (next: LicenseLifecycleSnapshot) => unknown;
  /** Window events the app dispatches after its own licence operations; each one re-checks. */
  changeEventNames?: readonly string[];
  /** The re-check interval when the state has no boundary, and the longest wait before any re-check. */
  fallbackMs?: number;
  /** The host's wording for the view's message code. */
  messages?: LicenseMessageOverrides;
}

/**
 * One monitor per app. `refresh()` runs one check: it retries the queued releases (a failure is ignored),
 * reads the status, asks the server only when `online` is set and the state needs it
 * (`shouldRefreshLicenseFromServer`), runs `afterChange`, then publishes; a check already running is shared.
 * Every published state arms a timer for its boundary (at most `fallbackMs` away), which re-checks online.
 * `start()` checks online now and again on window focus, when the page becomes visible, on each of
 * `changeEventNames`, and re-reads on the client's change event; it returns the function that stops all of it.
 */
export function createLicenseLifecycleMonitor({
  client,
  afterChange,
  changeEventNames = [],
  fallbackMs = CHECK_FALLBACK_MS,
  messages,
}: LicenseLifecycleMonitorOptions) {
  let snapshot: LicenseLifecycleSnapshot = { kind: 'checking', isPro: false };
  let checkInFlight: Promise<LicenseLifecycleSnapshot> | null = null;
  let boundaryTimer: number | null = null;
  const listeners = new Set<(next: LicenseLifecycleSnapshot) => void>();

  const read = async (): Promise<LicenseLifecycleSnapshot> => snapshotFromLicenseView(await client.status(), messages);
  const quietly = async (work: () => unknown): Promise<void> => {
    try { await work(); } catch { /* Kept natively; never blocks the check. */ }
  };

  function scheduleBoundaryCheck(next: LicenseLifecycleSnapshot): void {
    if (typeof window === 'undefined') return;
    if (boundaryTimer !== null) window.clearTimeout(boundaryTimer);
    const delay = millisecondsUntilLicenseCheck(next, Date.now(), fallbackMs);
    boundaryTimer = window.setTimeout(
      () => void refresh({ online: true }),
      Math.min(Math.max(delay, 1), fallbackMs),
    );
  }

  function publish(next: LicenseLifecycleSnapshot): LicenseLifecycleSnapshot {
    snapshot = next;
    for (const listener of listeners) listener(next);
    scheduleBoundaryCheck(next);
    return next;
  }

  function refresh(options: { online?: boolean } = {}): Promise<LicenseLifecycleSnapshot> {
    if (checkInFlight) return checkInFlight;

    checkInFlight = (async () => {
      try {
        // Every pass first retries the server-side releases queued before a local removal. A failure
        // stays queued natively and never blocks local use.
        await quietly(() => client.retryPendingReleases());
        let next = await read();
        if (shouldRefreshLicenseFromServer(next, options.online === true)) {
          await quietly(() => client.refresh());
          next = await read();
        }

        // Native code already evaluates the new state. The app applies it before the UI is told.
        await afterChange?.(next);
        return publish(next);
      } catch (error) {
        return publish({
          kind: 'storage_error',
          isPro: false,
          message: error instanceof Error ? error.message : String(error),
        });
      } finally {
        checkInFlight = null;
      }
    })();
    return checkInFlight;
  }

  return {
    /** The last published state; `checking` until the first check ends. */
    get: (): LicenseLifecycleSnapshot => snapshot,
    /** Calls `listener` with the current state now and with every published one. Returns the unsubscribe. */
    subscribe(listener: (next: LicenseLifecycleSnapshot) => void): () => void {
      listeners.add(listener);
      listener(snapshot);
      return () => { listeners.delete(listener); };
    },
    refresh,
    start(): () => void {
      if (typeof window === 'undefined' || typeof document === 'undefined') return () => undefined;
      const recheck = () => void refresh({ online: true });
      const onVisibility = () => {
        if (document.visibilityState === 'visible') recheck();
      };

      window.addEventListener('focus', recheck);
      for (const name of changeEventNames) window.addEventListener(name, recheck);
      document.addEventListener('visibilitychange', onVisibility);
      recheck();

      // Native licence changes (another window, a background refresh, a removal) re-read at once.
      let stopped = false;
      let release: (() => void) | undefined;
      void quietly(async () => {
        const unlisten = await client.onChange?.(() => void refresh());
        if (stopped) unlisten?.();
        else release = unlisten;
      });

      return () => {
        stopped = true;
        release?.();
        release = undefined;
        window.removeEventListener('focus', recheck);
        for (const name of changeEventNames) window.removeEventListener(name, recheck);
        document.removeEventListener('visibilitychange', onVisibility);
        if (boundaryTimer !== null) window.clearTimeout(boundaryTimer);
        boundaryTimer = null;
      };
    },
  };
}
export type LicenseLifecycleMonitor = ReturnType<typeof createLicenseLifecycleMonitor>;
