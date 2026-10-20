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
export function licenseMessage(code: unknown): string { return LICENSE_MESSAGES[messageCode(code)]; }
export class LicenseClientError extends Error {
  readonly code: CommandErrorCode | 'unknown';
  readonly reason?: RejectReason;
  constructor(code: CommandErrorCode | 'unknown' = 'unknown', message: MessageCode = 'unknown', reason?: RejectReason) {
    super(licenseMessage(message));
    this.name = 'LicenseClientError';
    this.code = code;
    if (code === 'rejected' && reason !== undefined) this.reason = reason;
  }
}
export type LicenseInvoke = <T = unknown>(command: string, args?: Record<string, unknown>) => Promise<T>;
export type LicenseUnlisten = () => void;
export type LicenseListen = <T = unknown>(event: string, callback: (event: { payload: T }) => void) => Promise<LicenseUnlisten>;
export interface LicenseConfig { invoke: LicenseInvoke; listen: LicenseListen }
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
function commandError(value: unknown): LicenseClientError {
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
    return new LicenseClientError(code, e.message === undefined ? defaults[code] : messageCode(e.message), reason);
  } catch { return new LicenseClientError(); }
}
export function createLicenseClient({ invoke, listen }: LicenseConfig) {
  async function call<T>(command: string, project: (value: unknown) => T, args?: Record<string, unknown>): Promise<T> {
    let value: unknown;
    try { value = await invoke(command, args); } catch (error) { throw commandError(error); }
    try { return project(value); } catch { throw new LicenseClientError(); }
  }
  return {
    status: (): Promise<LicenseView> => call('rightkit_license_status', projectView),
    activate: async (key: string): Promise<LicenseView> => call('rightkit_license_activate', projectView, { key: text(key, 4096) }),
    refresh: (): Promise<RefreshView> => call('rightkit_license_refresh', value => {
      const v = record(value);
      return { view: projectView(v.view), outcome: oneOf(v.outcome, ['noCredential', 'refreshed', 'stale', 'revoked', 'reanchored', 'clockRefused']) };
    }),
    deactivate: (): Promise<LicenseView> => call('rightkit_license_deactivate', projectView),
    requestTrial: async (email: string): Promise<TrialView> => call('rightkit_license_request_trial', value => {
      const v = record(value);
      return { accepted: boolean(v.accepted), message: messageCode(v.message) };
    }, { email: text(email, 320) }),
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
        try { onError?.(new LicenseClientError()); } catch { /* Caller errors cannot prevent cleanup. */ }
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
      } catch { stop(); throw new LicenseClientError(); }
      if (typeof release !== 'function') { stop(); throw new LicenseClientError(); }
      cleanup = release;
      if (!active) stop();
      return stop;
    },
  };
}
export type LicenseClient = ReturnType<typeof createLicenseClient>;
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
