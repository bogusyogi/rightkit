/** Native licence views only. Native commands own persistence & authorization. */
export type LicenseTier = 'free' | 'plus' | 'pro' | (string & {});
export type LicenseKind = 'trial' | 'lifetime' | 'term';
export type EntitlementStage = 'time_bound' | 'provisional' | 'final';
export type Lifecycle = 'unlicensed' | 'migration_required' | 'trial' | 'expired_trial' | 'active' | 'grace' | 'expired' | 'lifetime_provisional' | 'verification_required' | 'lifetime' | 'clock_repair_required' | 'revoked' | 'invalid';
export type LicenseStatus = 'none' | 'active' | 'grace' | 'expired' | 'revoked' | 'invalid' | 'unverifiable';
export interface LicenseView {
  mode: 'disabled' | 'configured';
  status: LicenseStatus;
  lifecycle: Lifecycle;
  tier: string | null;
  features: string[];
  kind: LicenseKind | null;
  stage: EntitlementStage | null;
  /** Unix milliseconds; display only. */
  validUntil: number | null;
  confirmationDueAt: number | null;
  staleSince: number | null;
  licenseIdSuffix: string | null;
  busy: boolean;
  unresolved: boolean;
  revisionState: 'active' | 'tombstone' | 'none';
  message: MessageCode;
}
export type RefreshOutcome = 'noCredential' | 'refreshed' | 'stale' | 'revoked' | 'reanchored' | 'clockRefused';
export interface RefreshView { view: LicenseView; outcome: RefreshOutcome }
export interface TrialView { accepted: boolean; message: MessageCode }
export type RejectReason = 'keyNotAccepted' | 'revoked' | 'expired' | 'deviceLimit' | 'rateLimited' | 'other';
export type CommandErrorCode = 'unconfigured' | 'busy' | 'unresolved' | 'network' | 'rejected' | 'store' | 'clock' | 'verify' | 'forbidden' | 'unsupported';
export const LICENSE_MESSAGES = Object.freeze({
  none: '',
  unconfigured: 'Licensing is not available in this build.',
  busy: 'Another licence operation is in progress. Try again in a moment.',
  unresolved: 'The last licence change did not finish. Try again to complete it.',
  networkUnavailable: 'Could not reach the licence server. Check your connection and try again.',
  keyNotAccepted: 'That licence key was not accepted.',
  revoked: 'This licence has been revoked.',
  expired: 'This licence has expired.',
  deviceLimit: 'This licence is already active on the maximum number of devices.',
  rateLimited: 'Too many attempts. Wait a few minutes and try again.',
  rejectedOther: 'The licence server declined the request.',
  storeFailed: 'The licence could not be saved on this device.',
  clockRollback: 'Your system clock appears to be set earlier than before. Correct it and try again.',
  clockUnavailable: 'The system clock could not be read.',
  verifyFailed: 'The licence could not be verified.',
  invalidToken: 'The stored licence could not be verified.',
  issuedInTheFuture: 'The licence was issued later than your system clock allows. Check the date and time.',
  invalidLifecycle: 'The licence data is inconsistent.',
  migrationRequired: 'Your licence needs a one-time online refresh.',
  clockRepairRequired: 'Your system clock needs correcting before the licence can be used.',
  forbidden: 'This window cannot manage the licence.',
  unsupported: 'This feature is not available in this app.',
  trialRequested: 'Check your email to start the trial.',
  trialRefused: 'A trial could not be started for this email.',
  updateRefused: 'This update cannot be installed with the current licence.',
  updateFailed: 'The update could not be downloaded or installed.',
  unknown: 'Something went wrong.',
});
export type MessageCode = keyof typeof LICENSE_MESSAGES;
/** The host's own wording, by message code. A code not listed keeps the default text from `LICENSE_MESSAGES`. Text only; no code is added or removed. */
export type LicenseMessageOverrides = Partial<Record<MessageCode, string>>;
/** `LICENSE_MESSAGES` in US spelling ("license" for "licence"); pass it as `messages`. */
export const LICENSE_MESSAGES_EN_US: Readonly<Record<MessageCode, string>> = Object.freeze(Object.fromEntries(
  Object.entries(LICENSE_MESSAGES).map(([code, words]) => [code, words.replace(/\b([Ll])icence(s?)\b/g, '$1icense$2')]),
) as Record<MessageCode, string>);
export function licenseMessage(code: unknown, messages?: LicenseMessageOverrides): string {
  const known = messageCode(code);
  const own = messages?.[known];
  return typeof own === 'string' ? own : LICENSE_MESSAGES[known];
}
export class LicenseClientError extends Error {
  readonly code: CommandErrorCode | 'unknown';
  readonly reason?: RejectReason;
  constructor(code: CommandErrorCode | 'unknown' = 'unknown', message: MessageCode = 'unknown', reason?: RejectReason, messages?: LicenseMessageOverrides) {
    super(licenseMessage(message, messages));
    this.name = 'LicenseClientError';
    this.code = code;
    if (code === 'rejected' && reason !== undefined) this.reason = reason;
  }
}
export type LicenseInvoke = <T = unknown>(command: string, args?: Record<string, unknown>) => Promise<T>;
export type LicenseUnlisten = () => void;
export type LicenseListen = <T = unknown>(event: string, callback: (event: { payload: T }) => void) => Promise<LicenseUnlisten>;
export interface LicenseConfig {
  invoke: LicenseInvoke;
  listen: LicenseListen;
  /** The host's own wording for the client's errors; see `LicenseMessageOverrides`. */
  messages?: LicenseMessageOverrides;
}
export interface LicenseRequirement { tier?: LicenseTier; feature?: string; allFeatures?: readonly string[]; anyFeature?: readonly string[] }
const lifecycles: readonly Lifecycle[] = ['unlicensed', 'migration_required', 'trial', 'expired_trial', 'active', 'grace', 'expired', 'lifetime_provisional', 'verification_required', 'lifetime', 'clock_repair_required', 'revoked', 'invalid'];
const reasons: readonly RejectReason[] = ['keyNotAccepted', 'revoked', 'expired', 'deviceLimit', 'rateLimited', 'other'];
const codes: readonly CommandErrorCode[] = ['unconfigured', 'busy', 'unresolved', 'network', 'rejected', 'store', 'clock', 'verify', 'forbidden', 'unsupported'];
function fail(): never { throw new LicenseClientError(); }
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return fail();
  return value as Record<string, unknown>;
}
function oneOf<T extends string>(value: unknown, choices: readonly T[]): T {
  if (typeof value !== 'string' || !choices.includes(value as T)) return fail();
  return value as T;
}
function boolean(value: unknown): boolean { return typeof value === 'boolean' ? value : fail(); }
function text(value: unknown, max: number): string {
  if (typeof value !== 'string' || value.length === 0 || value.length > max || /[\u0000-\u001f\u007f]/u.test(value)) return fail();
  return value;
}
function nullableText(value: unknown, max: number): string | null { return value === null ? null : text(value, max); }
function time(value: unknown): number | null {
  return value === null ? null : typeof value === 'number' && Number.isSafeInteger(value) && Math.abs(value) <= 8_640_000_000_000_000 ? value : fail();
}
function messageCode(value: unknown): MessageCode {
  return typeof value === 'string' && Object.hasOwn(LICENSE_MESSAGES, value) ? value as MessageCode : 'unknown';
}
/** Projects every field; unknown properties never enter renderer state. */
function projectView(value: unknown): LicenseView {
  const v = record(value);
  if (!Array.isArray(v.features) || v.features.length > 256) return fail();
  return {
    mode: oneOf(v.mode, ['disabled', 'configured']),
    status: oneOf(v.status, ['none', 'active', 'grace', 'expired', 'revoked', 'invalid', 'unverifiable']),
    lifecycle: oneOf(v.lifecycle, lifecycles),
    tier: nullableText(v.tier, 128),
    features: v.features.map((f: unknown) => text(f, 128)),
    kind: v.kind === null ? null : oneOf(v.kind, ['trial', 'lifetime', 'term'] as const),
    stage: v.stage === null ? null : oneOf(v.stage, ['time_bound', 'provisional', 'final'] as const),
    validUntil: time(v.validUntil), confirmationDueAt: time(v.confirmationDueAt), staleSince: time(v.staleSince),
    licenseIdSuffix: nullableText(v.licenseIdSuffix, 4),
    busy: boolean(v.busy), unresolved: boolean(v.unresolved),
    revisionState: oneOf(v.revisionState, ['active', 'tombstone', 'none']), message: messageCode(v.message),
  };
}
function commandError(value: unknown, messages?: LicenseMessageOverrides): LicenseClientError {
  try {
    const e = record(value);
    const code = oneOf(e.code, codes);
    const reason = code === 'rejected' && e.reason !== undefined ? oneOf(e.reason, reasons) : undefined;
    const defaults: Record<CommandErrorCode, MessageCode> = {
      unconfigured: 'unconfigured', busy: 'busy', unresolved: 'unresolved', network: 'networkUnavailable',
      rejected: reason === 'other' || reason === undefined ? 'rejectedOther' : reason,
      store: 'storeFailed', clock: 'clockUnavailable', verify: 'verifyFailed', forbidden: 'forbidden', unsupported: 'unsupported',
    };
    // Only a closed native MessageCode selects text; no raw message or cause survives.
    return new LicenseClientError(code, e.message === undefined ? defaults[code] : messageCode(e.message), reason, messages);
  } catch { return new LicenseClientError('unknown', 'unknown', undefined, messages); }
}
export function createLicenseClient({ invoke, listen, messages }: LicenseConfig) {
  const unknown = (): LicenseClientError => new LicenseClientError('unknown', 'unknown', undefined, messages);
  async function call<T>(command: string, project: (value: unknown) => T, args?: Record<string, unknown>): Promise<T> {
    let value: unknown;
    try { value = await invoke(command, args); } catch (error) { throw commandError(error, messages); }
    try { return project(value); } catch { throw unknown(); }
  }
  /** A transient input (key, code, email), refused in the client's own words before any native call. */
  const input = (value: unknown, max: number): string => { try { return text(value, max); } catch { throw unknown(); } };
  return {
    status: (): Promise<LicenseView> => call('rightkit_license_status', projectView),
    activate: async (key: string): Promise<LicenseView> => call('rightkit_license_activate', projectView, { key: input(key, 4096) }),
    refresh: (): Promise<RefreshView> => call('rightkit_license_refresh', value => {
      const v = record(value);
      return { view: projectView(v.view), outcome: oneOf(v.outcome, ['noCredential', 'refreshed', 'stale', 'revoked', 'reanchored', 'clockRefused']) };
    }),
    deactivate: (): Promise<LicenseView> => call('rightkit_license_deactivate', projectView),
    /**
     * Ask for a device-link code (#1130). `issued` carries the one-time code for display; `notLeased` means this
     * device holds no active lease to prove ownership with; `notActiveLeaseHolder` is the service's refusal.
     */
    requestLink: (): Promise<LinkRequestView> => call('rightkit_license_link_request', linkRequestView),
    /** Redeem a link code another device issued (#1130). Answers the new licence view. */
    redeemLink: async (code: string, deviceLabel?: string): Promise<LicenseView> => call('rightkit_license_link_redeem', projectView, {
      code: input(code, 4096), deviceLabel: deviceLabel === undefined ? null : input(deviceLabel, 128),
    }),
    /**
     * Settle the app's queued licence releases now (#1036): the native host drains the queue the app configured
     * and answers counts only. Rejects as `unsupported` when the host has no queue.
     */
    retryPendingReleases: (): Promise<PendingReleaseDrain> => call('rightkit_license_retry_pending_releases', drainReport),
    requestTrial: async (email: string): Promise<TrialView> => call('rightkit_license_request_trial', value => {
      const v = record(value);
      return { accepted: boolean(v.accepted), message: messageCode(v.message) };
    }, { email: input(email, 320) }),
    /** Invalid events stop this subscription & report one fixed error. Call cleanup on unmount. */
    async onChange(callback: (view: LicenseView) => void, onError?: (error: LicenseClientError) => void): Promise<LicenseUnlisten> {
      let active = true;
      let cleanup: LicenseUnlisten | undefined;
      const stop = () => {
        active = false;
        const release = cleanup;
        cleanup = undefined;
        if (release) { try { release(); } catch { /* Never expose host errors. */ } }
      };
      const report = () => {
        stop();
        try { onError?.(unknown()); } catch { /* Caller errors cannot prevent cleanup. */ }
      };
      let release: LicenseUnlisten;
      try {
        release = await listen<unknown>('rightkit://license-changed', event => {
          if (!active) return;
          let view: LicenseView;
          try { view = projectView(record(event.payload).view); }
          catch { report(); return; }
          try { callback(view); } catch { report(); }
        });
      } catch { stop(); throw unknown(); }
      if (typeof release !== 'function') { stop(); throw unknown(); }
      cleanup = release;
      if (!active) stop();
      return stop;
    },
  };
}
export type LicenseClient = ReturnType<typeof createLicenseClient>;

/** What a link request came to (#1130). */
export type LinkRequestView =
  | { outcome: 'issued'; code: string; expiresAt: number }
  | { outcome: 'notLeased' }
  | { outcome: 'notActiveLeaseHolder' };
function linkRequestView(value: unknown): LinkRequestView {
  const v = record(value);
  const outcome = oneOf(v.outcome, ['issued', 'notLeased', 'notActiveLeaseHolder'] as const);
  if (outcome !== 'issued') return { outcome };
  const expiresAt = time(v.expiresAt);
  if (expiresAt === null) return fail();
  return { outcome, code: text(v.code, 256), expiresAt };
}

/** Counts of one pending-release drain (#1036). */
export interface PendingReleaseDrain { attempted: number; settled: number; kept: number; acknowledgeFailed: number }
function count(value: unknown): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : fail();
}
function drainReport(value: unknown): PendingReleaseDrain {
  const v = record(value);
  return { attempted: count(v.attempted), settled: count(v.settled), kept: count(v.kept), acknowledgeFailed: count(v.acknowledgeFailed) };
}

// ---- account session (#1032) -------------------------------------------------------------

export type AccountErrorCode = 'unconfigured' | 'busy' | 'unresolved' | 'network' | 'notSignedIn' | 'sessionExpired' | 'superseded' | 'rejected' | 'store' | 'badResponse' | 'forbidden' | 'unsupported';
export type AccountRejectReason = 'rateLimited' | 'linkInvalid' | 'deviceNotFound' | 'invalidInput' | 'other';
const accountCodes: readonly AccountErrorCode[] = ['unconfigured', 'busy', 'unresolved', 'network', 'notSignedIn', 'sessionExpired', 'superseded', 'rejected', 'store', 'badResponse', 'forbidden', 'unsupported'];
const accountReasons: readonly AccountRejectReason[] = ['rateLimited', 'linkInvalid', 'deviceNotFound', 'invalidInput', 'other'];
/** Fixed text for each native account error; the same words as `AccountError`'s `Display` in `rightkit-license`. */
export const ACCOUNT_MESSAGES = Object.freeze({
  unconfigured: 'Accounts are not available in this build.',
  busy: 'Another account operation is in progress. Try again in a moment.',
  unresolved: 'The last account change did not finish. Try again.',
  network: 'Could not reach the account service. Check your connection and try again.',
  notSignedIn: 'Sign in to manage devices on your account.',
  sessionExpired: 'Your session expired. Sign in again.',
  superseded: 'The account changed while this request was running. Try again.',
  rejected: 'The account service declined the request.',
  rateLimited: 'Too many attempts. Try again shortly.',
  linkInvalid: 'This sign-in link is invalid, expired, or already used.',
  deviceNotFound: 'That device is no longer on your account.',
  invalidInput: 'That input is not valid.',
  store: 'The account session could not be saved on this device.',
  badResponse: 'The account service sent an unexpected answer.',
  forbidden: 'This window cannot manage the account.',
  unsupported: 'This feature is not available in this app.',
  unknown: 'Something went wrong.',
});
export type AccountMessageCode = keyof typeof ACCOUNT_MESSAGES;
/** The host's own account wording, by message code. A code not listed keeps the default text from `ACCOUNT_MESSAGES`. */
export type AccountMessageOverrides = Partial<Record<AccountMessageCode, string>>;
export class AccountClientError extends Error {
  readonly code: AccountErrorCode | 'unknown';
  readonly reason?: AccountRejectReason;
  constructor(code: AccountErrorCode | 'unknown' = 'unknown', reason?: AccountRejectReason, messages?: AccountMessageOverrides) {
    const words: AccountMessageCode = code === 'rejected' && reason !== undefined && reason !== 'other' ? reason : code;
    const own = messages?.[words];
    super(typeof own === 'string' ? own : ACCOUNT_MESSAGES[words]);
    this.name = 'AccountClientError';
    this.code = code;
    if (code === 'rejected' && reason !== undefined) this.reason = reason;
  }
}
export interface AccountStatusView { signedIn: boolean; email: string | null; expiresAt: number | null; busy: boolean; unresolved: boolean }
export interface AccountDeviceView {
  id: string; licenseSuffix: string | null; appKey: string; platform: string | null; pool: string | null;
  label: string | null; state: string; lastSeenAt: number | null; activatedAt: number | null; generation: number;
}
/** How an account deletion ended. `indeterminate`: the account may or may not be deleted; refresh to find out. */
export type AccountDeletionView =
  | { outcome: 'deleted'; localSessionCleared: boolean }
  | { outcome: 'refused'; reason: 'activeSubscription' | 'rateLimited' | 'other' }
  | { outcome: 'indeterminate' };
function accountError(value: unknown, messages?: AccountMessageOverrides): AccountClientError {
  try {
    const e = record(value);
    const code = oneOf(e.code, accountCodes);
    return new AccountClientError(code, code === 'rejected' && e.reason !== undefined ? oneOf(e.reason, accountReasons) : undefined, messages);
  } catch { return new AccountClientError('unknown', undefined, messages); }
}
function optionalText(value: unknown, max: number): string | null { return value === null || value === undefined ? null : text(value, max); }
function accountStatus(value: unknown): AccountStatusView {
  const v = record(value);
  return { signedIn: boolean(v.signedIn), email: optionalText(v.email, 320), expiresAt: time(v.expiresAt ?? null), busy: boolean(v.busy), unresolved: boolean(v.unresolved) };
}
function accountDevice(value: unknown): AccountDeviceView {
  const v = record(value);
  return {
    id: text(v.id, 128), licenseSuffix: optionalText(v.licenseSuffix, 4), appKey: typeof v.appKey === 'string' && v.appKey.length <= 32 ? v.appKey : fail(),
    platform: optionalText(v.platform, 32), pool: optionalText(v.pool, 32), label: optionalText(v.label, 128), state: text(v.state, 32),
    lastSeenAt: time(v.lastSeenAt ?? null), activatedAt: time(v.activatedAt ?? null), generation: count(v.generation),
  };
}
function accountDeletion(value: unknown): AccountDeletionView {
  const v = record(value);
  const outcome = oneOf(v.outcome, ['deleted', 'refused', 'indeterminate'] as const);
  if (outcome === 'deleted') return { outcome, localSessionCleared: boolean(v.localSessionCleared) };
  if (outcome === 'refused') return { outcome, reason: oneOf(v.reason, ['activeSubscription', 'rateLimited', 'other'] as const) };
  return { outcome };
}
/** Fixed text for each way an account deletion can end; `deleted` has none. */
export const ACCOUNT_DELETION_MESSAGES = Object.freeze({
  deleted: '',
  activeSubscription: 'You have an active subscription. Cancel it before deleting your account, then try again.',
  rateLimited: 'Too many attempts. Try again shortly.',
  other: 'The account service declined the deletion. Try again later.',
  indeterminate: "We couldn't confirm your account was deleted. Check your connection and try again.",
});
export type AccountDeletionMessageCode = keyof typeof ACCOUNT_DELETION_MESSAGES;
/**
 * The words for how an account deletion ended: empty for `deleted`, the refusal's reason, or that the answer
 * could not be confirmed (`indeterminate`: the account may or may not be deleted). `messages` replaces any of them.
 */
export function accountDeletionMessage(view: AccountDeletionView, messages?: Partial<Record<AccountDeletionMessageCode, string>>): string {
  const code: AccountDeletionMessageCode = view.outcome === 'refused' ? view.reason : view.outcome;
  const own = messages?.[code];
  return typeof own === 'string' ? own : ACCOUNT_DELETION_MESSAGES[code];
}
const deviceId = (id: string): string => /^[A-Za-z0-9_-]{1,128}$/u.test(id) ? id : fail();
/**
 * The native passwordless account session (#1032): sign-in by e-mailed code, status, the account's devices and
 * deletion, over the `rightkit_account_*` commands of the native host (`AccountSessionClient` in
 * `rightkit-license`). Native keeps the session token; the renderer sees only the views and fixed error text.
 * The link calls live on `createLicenseClient` (`requestLink`, `redeemLink`).
 */
export function createAccountClient({ invoke, messages }: { invoke: LicenseInvoke; messages?: AccountMessageOverrides }) {
  async function call<T>(command: string, project: (value: unknown) => T, args?: Record<string, unknown>): Promise<T> {
    let value: unknown;
    try { value = await invoke(command, args); } catch (error) { throw accountError(error, messages); }
    try { return project(value); } catch { throw new AccountClientError('unknown', undefined, messages); }
  }
  const nothing = (): void => undefined;
  return {
    status: (): Promise<AccountStatusView> => call('rightkit_account_status', accountStatus),
    refresh: (): Promise<AccountStatusView> => call('rightkit_account_refresh', accountStatus),
    /** Send the sign-in e-mail. Answers nothing: the service never says whether the address has an account. */
    requestSignIn: (email: string): Promise<void> => call('rightkit_account_request_sign_in', nothing, { email: text(email, 320) }),
    /** Complete sign-in with the code (or link token) from the e-mail. */
    completeSignIn: (code: string): Promise<AccountStatusView> => call('rightkit_account_complete_sign_in', accountStatus, { code: text(code, 4096) }),
    signOut: (): Promise<AccountStatusView> => call('rightkit_account_sign_out', accountStatus),
    listDevices: (): Promise<AccountDeviceView[]> => call('rightkit_account_list_devices', value => {
      if (!Array.isArray(value) || value.length > 256) return fail();
      return value.map(accountDevice);
    }),
    renameDevice: (device: string, label: string): Promise<void> => {
      if (typeof label !== 'string' || new TextEncoder().encode(label).length > 256 || /[\u0000-\u001f\u007f]/u.test(label)) throw new AccountClientError('rejected', 'invalidInput', messages);
      return call('rightkit_account_rename_device', nothing, { device: deviceId(device), label });
    },
    removeDevice: (device: string): Promise<void> => call('rightkit_account_remove_device', nothing, { device: deviceId(device) }),
    /**
     * Delete the account. Irreversible: call it only after the person confirmed. `confirmed` must be `true`; the
     * native command builds its confirmation proof from it and refuses anything else.
     */
    deleteAccount: (options: { confirmed: true }): Promise<AccountDeletionView> => {
      if (options?.confirmed !== true) throw new AccountClientError('rejected', 'invalidInput', messages);
      return call('rightkit_account_delete', accountDeletion, { confirmed: true });
    },
  };
}
export type AccountClient = ReturnType<typeof createAccountClient>;
/** Display hints only; native authorization must still run at each protected operation. */
export function lifecycleGrantsAccess(lifecycle: Lifecycle): boolean { return ['trial', 'active', 'grace', 'lifetime_provisional', 'lifetime'].includes(lifecycle); }
export function lifecycleNeedsOnlineRefresh(lifecycle: Lifecycle): boolean { return ['migration_required', 'verification_required', 'clock_repair_required'].includes(lifecycle); }
export function isLicensed(view: LicenseView | null): boolean {
  return !!view && view.mode === 'configured' && !view.busy && !view.unresolved && view.revisionState === 'active'
    && (view.status === 'active' || view.status === 'grace') && lifecycleGrantsAccess(view.lifecycle);
}
export function tier(view: LicenseView | null): string | null { return view?.tier ?? null; }
export function hasFeature(view: LicenseView | null, feature: string): boolean { return isLicensed(view) && !!view?.features.includes(feature); }
function tierRank(value: string | null): number { return value === 'pro' ? 2 : value === 'plus' ? 1 : 0; }
export function tierAtLeast(view: LicenseView | null, required: LicenseTier): boolean {
  const current = tier(view);
  return isLicensed(view) && (current === required || (['free', 'plus', 'pro'].includes(required) && tierRank(current) >= tierRank(required)));
}
export function hasAllFeatures(view: LicenseView | null, features: readonly string[]): boolean { return isLicensed(view) && features.every(f => hasFeature(view, f)); }
export function hasAnyFeature(view: LicenseView | null, features: readonly string[]): boolean { return isLicensed(view) && features.some(f => hasFeature(view, f)); }
export function isPro(view: LicenseView | null): boolean { return tierAtLeast(view, 'pro'); }
export function satisfiesRequirement(view: LicenseView | null, requirement: LicenseRequirement): boolean {
  return isLicensed(view) && (!requirement.tier || tierAtLeast(view, requirement.tier))
    && (!requirement.feature || hasFeature(view, requirement.feature))
    && (!requirement.allFeatures || hasAllFeatures(view, requirement.allFeatures))
    && (!requirement.anyFeature || hasAnyFeature(view, requirement.anyFeature));
}
export * from './lifecycle.js';
export * from './proBridge.js';
