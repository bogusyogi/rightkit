/**
 * The licence and account layer of an app that answers its own licence commands next to the shell's
 * `rightkit_*` ones: the credential-store state, activation and removal with the Pro-bridge rule, the
 * trial request, link codes, the account session over the app's own commands, and the wording of their
 * closed errors. Display only: native code authorises every paid operation again when it runs. Command
 * names, event names and every sentence the app words differently are host options; the defaults are
 * the commands and wording the Right Suite apps ship.
 */
// `index.ts` re-exports this module, so these bindings are read at call time only, never while the module loads.
import {
  AccountClientError,
  LicenseClientError,
  accountDeletionMessage,
  createLicenseLifecycleMonitor,
  isPro as viewIsPro,
  licenseMessage,
  type AccountDeletionMessageCode,
  type AccountDeletionView,
  type AccountDeviceView,
  type AccountErrorCode,
  type AccountMessageOverrides,
  type AccountRejectReason,
  type LicenseClient,
  type LicenseInvoke,
  type LicenseMessageOverrides,
  type LicenseView,
  type LinkRequestView,
  type ProBridgeClient,
} from './index.js';

// ---- credential-store state ----------------------------------------------------------------

export type LicenseStorageOperation = 'read' | 'write' | 'delete' | 'device';
/** `error`: the OS credential store could not be read or written (the settings banner shows it, with no retry loop). */
export type LicenseStorageState =
  | { kind: 'ready' }
  | { kind: 'error'; operation: LicenseStorageOperation; message: string };

/** The default start of a storage error's text; the host words its own through `storageUnavailable`. */
export const SECURE_STORAGE_UNAVAILABLE = 'Secure licence storage is unavailable';

export class SecureLicenseStorageError extends Error {
  readonly operation: LicenseStorageOperation;
  constructor(operation: LicenseStorageOperation, cause: unknown, unavailable = SECURE_STORAGE_UNAVAILABLE) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    super(`${unavailable}: ${detail}`);
    this.name = 'SecureLicenseStorageError';
    this.operation = operation;
  }
}

// ---- wording of the closed errors ----------------------------------------------------------

type Rejection = { code?: unknown; reason?: unknown; message?: unknown };
function rejection(cause: unknown): Rejection { return cause && typeof cause === 'object' ? cause as Rejection : {}; }

function licenseCodeMessage(code: unknown): string {
  switch (code) {
    case 'network': return 'networkUnavailable';
    case 'busy': case 'unresolved': case 'forbidden': case 'unsupported': case 'unconfigured': return code;
    case 'store': return 'storeFailed';
    default: return 'unknown';
  }
}

export interface LicenseCommandErrorOptions {
  /** The host's wording by message code (for example `LICENSE_MESSAGES_EN_US`). */
  messages?: LicenseMessageOverrides;
  /** The sentence for a link code that reached the device limit. Default: the `deviceLimit` message. */
  deviceLimit?: string;
}

/**
 * Words for a rejected licence command that answers the owner's `{ code, reason?, message }` (link-code request
 * and redeem). `invalidReason` is the sentence for a key or code the service does not accept or has revoked.
 * Nothing a server said reaches the text.
 */
export function licenseCommandError(cause: unknown, invalidReason: string, options: LicenseCommandErrorOptions = {}): Error {
  const { code, reason, message } = rejection(cause);
  if (code === 'rejected' && (reason === 'keyNotAccepted' || reason === 'revoked')) return new Error(invalidReason);
  if (code === 'rejected' && reason === 'deviceLimit') {
    return new Error(options.deviceLimit ?? licenseMessage('deviceLimit', options.messages));
  }
  return new Error(licenseMessage(message ?? licenseCodeMessage(code), options.messages));
}

const accountErrorCodes: readonly string[] = ['unconfigured', 'busy', 'unresolved', 'network', 'notSignedIn', 'sessionExpired', 'superseded', 'rejected', 'store', 'badResponse', 'forbidden', 'unsupported'];

/**
 * Words for a rejected account command that answers `{ code, reason? }`. `invalidInput` is the sentence for input
 * native code refused before sending anything (it differs per command); an unknown code reads as `unknown`.
 */
export function accountCommandError(cause: unknown, invalidInput: string, messages?: AccountMessageOverrides): Error {
  const { code, reason } = rejection(cause);
  if (typeof code !== 'string' || !accountErrorCodes.includes(code)) return new Error(new AccountClientError('unknown', undefined, messages).message);
  if (code === 'rejected' && reason !== 'rateLimited' && reason !== 'linkInvalid' && reason !== 'deviceNotFound') return new Error(invalidInput);
  return new Error(new AccountClientError(code as AccountErrorCode, code === 'rejected' ? reason as AccountRejectReason : undefined, messages).message);
}

// ---- licence -------------------------------------------------------------------------------

export interface AppLicenseCommands {
  refreshEntitlements: string;
  openPricing: string;
  retryPendingRemovals: string;
  updateHeaders: string;
  updateInstallHeaders: string;
  windowsLinkRequest: string;
  windowsLinkRedeem: string;
}
export const APP_LICENSE_COMMANDS: Readonly<AppLicenseCommands> = Object.freeze({
  refreshEntitlements: 'refresh_entitlements',
  openPricing: 'open_pricing_page',
  retryPendingRemovals: 'license_retry_pending_removals',
  updateHeaders: 'license_update_headers',
  updateInstallHeaders: 'license_update_install_headers',
  windowsLinkRequest: 'license_windows_link_request',
  windowsLinkRedeem: 'license_windows_link_redeem',
});

/** The sentences of the Windows link code flow; they name the paid plan, so the host words them. */
export interface WindowsLinkWording {
  /** The device has no lease to prove ownership with (the request was rejected as not accepted). */
  notActivated: string;
  /** The service refused: this device does not hold the active lease. */
  notActiveLeaseHolder: string;
  /** The request answered `notLeased`. */
  notLeased: string;
  /** Redeem of a code that is invalid, expired or already used. */
  invalidCode: string;
  /** The device pool is full. Default: the `deviceLimit` message. */
  deviceLimit?: string;
}

export interface AppLicenseOptions {
  invoke: LicenseInvoke;
  /** `createLicenseClient`'s result: the shell's `rightkit_license_*` commands. */
  client: LicenseClient;
  /** Pass the Pro-bridge client only where the bridge exists (a native runtime); its rule runs around deactivation and activation. */
  proBridge?: Pick<ProBridgeClient, 'beforeDeactivate' | 'afterActivate'>;
  /** The window event dispatched after each licence mutation; the lifecycle monitor re-checks on it. */
  updatedEvent: string;
  commands?: Partial<AppLicenseCommands>;
  /** The host's wording by message code, for the lifecycle snapshot and the link-code errors. */
  messages?: LicenseMessageOverrides;
  windowsLinkWording: WindowsLinkWording;
  /** The start of a storage error's text, before the cause. Default `SECURE_STORAGE_UNAVAILABLE`. */
  storageUnavailable?: string;
}

/**
 * Normalises raw key input for the activation field. Issued keys are upper-case with real dashes, so the only
 * safe transforms are removing whitespace and upper-casing; never reshape or truncate.
 */
export const formatLicenseKey = (raw: string): string => raw.toUpperCase().replace(/\s+/g, '');

/** Display Pro flag from a view alone: fails closed while busy or unresolved. */
export const viewShowsPro = (view: LicenseView | null): boolean => viewIsPro(view);

/**
 * The one licence layer of an app. Licence changes reach the UI through explicit reads after each operation,
 * the lifecycle monitor (launch, focus, boundary timers), `updatedEvent` and the owner's change event.
 */
export function createAppLicense(options: AppLicenseOptions) {
  const { invoke, client, proBridge, updatedEvent, messages, windowsLinkWording, storageUnavailable } = options;
  const commands: AppLicenseCommands = { ...APP_LICENSE_COMMANDS, ...options.commands };

  let storageState: LicenseStorageState = { kind: 'ready' };
  const storageListeners = new Set<(state: LicenseStorageState) => void>();
  function publishStorage(state: LicenseStorageState): void {
    storageState = state;
    for (const listener of storageListeners) listener(state);
  }
  const storage = {
    get: (): LicenseStorageState => storageState,
    /** Calls `listener` with the current state at once; returns the unsubscribe. */
    subscribe(listener: (state: LicenseStorageState) => void): () => void {
      storageListeners.add(listener);
      listener(storageState);
      return () => storageListeners.delete(listener);
    },
  };

  /** Runs a licence call. A `store` failure is a storage error the settings banner shows; any other rejection passes through unchanged. */
  async function guarded<T>(operation: LicenseStorageOperation, work: () => Promise<T>): Promise<T> {
    try {
      const value = await work();
      if (storageState.kind !== 'ready') publishStorage({ kind: 'ready' });
      return value;
    } catch (cause) {
      if (cause instanceof LicenseClientError && cause.code === 'store') {
        const error = new SecureLicenseStorageError(operation, cause, storageUnavailable);
        publishStorage({ kind: 'error', operation, message: error.message });
        throw error;
      }
      throw cause;
    }
  }

  /** Tells listeners (the monitor and any UI observer) that the licence changed; one event for every mutation. */
  const notifyLicenseUpdated = (): void => { window.dispatchEvent(new Event(updatedEvent)); };

  /** Re-pushes entitlement-derived config to the engine without failing the caller's operation, but never silently. */
  async function refreshEntitlements(): Promise<void> {
    await invoke(commands.refreshEntitlements).catch((error: unknown) => { console.warn('refresh_entitlements failed', error); });
  }

  /** The licence as it stands (native, no network). */
  const loadLicense = (): Promise<LicenseView> => guarded('read', () => client.status());

  const monitor = createLicenseLifecycleMonitor({
    client: {
      status: loadLicense,
      refresh: async () => { await client.refresh(); },
      retryPendingReleases: () => invoke(commands.retryPendingRemovals),
      onChange: callback => client.onChange(callback),
    },
    afterChange: () => invoke(commands.refreshEntitlements),
    changeEventNames: [updatedEvent],
    messages,
  });

  const words = (code: unknown): string => licenseMessage(code, messages);
  const linkError = (cause: unknown, invalidReason: string): Error =>
    licenseCommandError(cause, invalidReason, { messages, deviceLimit: windowsLinkWording.deviceLimit });

  return {
    storage,
    monitor,
    loadLicense,
    /** Retries the native read after the user unlocks or repairs the credential store; a failed retry stays Free and republishes the error. */
    retryLicenseStorage: loadLicense,
    /** The renderer's display Pro flag (the monitor's last read). Never an authority. */
    isPro: (): boolean => monitor.get().isPro,
    viewShowsPro,
    formatLicenseKey,
    /** The words for a licence message code. */
    licenseText: words,
    notifyLicenseUpdated,
    refreshEntitlements,
    /** Activates a key: the key goes to native code once. `view.status === 'active'` is the only success. */
    async activateLicense(key: string): Promise<LicenseView> {
      const view = await guarded('write', () => client.activate(key));
      if (view.status === 'active') {
        await proBridge?.afterActivate(view);
        await refreshEntitlements();
        notifyLicenseUpdated();
      }
      return view;
    },
    /** Removes this device's licence: the bridge is suppressed first, so it cannot re-activate from the shared bridge. */
    async removeLicense(): Promise<void> {
      await proBridge?.beforeDeactivate();
      await guarded('delete', () => client.deactivate());
      await refreshEntitlements();
      notifyLicenseUpdated();
    },
    /** Requests a trial key by e-mail (the service e-mails it; it is not returned). Any rejection is `false`. */
    async requestTrialKey(email: string): Promise<boolean> {
      try { return (await client.requestTrial(email)).accepted; } catch { return false; }
    },
    openPricing: (): Promise<void> => invoke(commands.openPricing).then(() => undefined, () => undefined),
    /** Headers for the update check, or undefined when no active licence can authenticate it. */
    async updateHeaders(): Promise<Record<string, string> | undefined> {
      return (await invoke<Record<string, string> | null>(commands.updateHeaders)) ?? undefined;
    },
    /** Install-time artifact authorization; public patches carry no credential. */
    async updateInstallHeaders(tier: 'patch' | 'update'): Promise<Record<string, string> | undefined> {
      return (await invoke<Record<string, string> | null>(commands.updateInstallHeaders, { tier })) ?? undefined;
    },
    /** A short-lived single-use link code from this already-leased device; `expiresAt` is Unix milliseconds. */
    async requestWindowsLinkCode(): Promise<{ code: string; expiresAt: number }> {
      let answer: LinkRequestView;
      try { answer = await invoke<LinkRequestView>(commands.windowsLinkRequest); }
      catch (cause) { throw linkError(cause, windowsLinkWording.notActivated); }
      if (answer.outcome === 'issued') return { code: answer.code, expiresAt: answer.expiresAt };
      throw new Error(answer.outcome === 'notActiveLeaseHolder' ? windowsLinkWording.notActiveLeaseHolder : windowsLinkWording.notLeased);
    },
    /** Redeems a link code on this device; rejects (the input stays for a retry) when it is invalid, expired, used, or the pool is full. */
    async redeemWindowsLinkCode(code: string, deviceLabel?: string): Promise<void> {
      try { await invoke<LicenseView>(commands.windowsLinkRedeem, { code, deviceLabel: deviceLabel ?? null }); }
      catch (cause) { throw linkError(cause, windowsLinkWording.invalidCode); }
      await refreshEntitlements();
      notifyLicenseUpdated();
    },
  };
}
export type AppLicense = ReturnType<typeof createAppLicense>;

// ---- account session -----------------------------------------------------------------------

/** `expiresAt` is Unix SECONDS here (the native view carries milliseconds; this keeps the unit settings screens show). */
export type AppAccountStatus = { signedIn: boolean; email: string | null; expiresAt: number | null };
export type AppAccountDevice = AccountDeviceView;

export interface AppAccountCommands {
  status: string;
  refresh: string;
  beginMagicLink: string;
  complete: string;
  signOut: string;
  devices: string;
  renameDevice: string;
  removeDevice: string;
  deleteAccount: string;
}
export const APP_ACCOUNT_COMMANDS: Readonly<AppAccountCommands> = Object.freeze({
  status: 'account_session_status',
  refresh: 'account_session_refresh',
  beginMagicLink: 'account_session_begin_magic_link',
  complete: 'account_session_complete',
  signOut: 'account_session_sign_out',
  devices: 'account_devices_list',
  renameDevice: 'account_device_rename',
  removeDevice: 'account_device_remove',
  deleteAccount: 'account_delete',
});

/** The sentence for input native code refused before sending anything, per command. */
export interface AccountInvalidInput { beginMagicLink: string; complete: string; renameDevice: string; removeDevice: string }
export const APP_ACCOUNT_INVALID_INPUT: Readonly<AccountInvalidInput> = Object.freeze({
  beginMagicLink: 'Enter your email address first.',
  complete: 'That sign-in link looks incomplete.',
  renameDevice: 'Enter a device name.',
  removeDevice: 'That device is no longer on your account.',
});

export interface AppAccountOptions {
  invoke: LicenseInvoke;
  commands?: Partial<AppAccountCommands>;
  /** The host's wording by code; see `AccountMessageOverrides`. */
  messages?: AccountMessageOverrides;
  invalidInput?: Partial<AccountInvalidInput>;
  /** The host's wording of how a deletion ended; see `accountDeletionMessage`. */
  deletionMessages?: Partial<Record<AccountDeletionMessageCode, string>>;
}

/**
 * The account session over the app's own commands (magic-link sign-in, devices, deletion). Native code keeps
 * the session token; every rejection becomes an `Error` with the host's wording. An account is never needed
 * for Free use or local transcription, and no call here reads or changes the licence.
 */
export function createAppAccountSession({ invoke, commands: own, messages, invalidInput: ownInvalid, deletionMessages }: AppAccountOptions) {
  const commands: AppAccountCommands = { ...APP_ACCOUNT_COMMANDS, ...own };
  const invalid: AccountInvalidInput = { ...APP_ACCOUNT_INVALID_INPUT, ...ownInvalid };
  async function call<T>(command: string, args: Record<string, unknown> | undefined, invalidInput: string): Promise<T> {
    try { return await invoke<T>(command, args); } catch (cause) { throw accountCommandError(cause, invalidInput, messages); }
  }
  type Native = { signedIn: boolean; email: string | null; expiresAt: number | null };
  const status = (native: Native): AppAccountStatus => ({
    signedIn: native.signedIn,
    email: native.email,
    expiresAt: native.expiresAt == null ? null : Math.floor(native.expiresAt / 1000),
  });
  return {
    sessionStatus: async (): Promise<AppAccountStatus> => status(await call<Native>(commands.status, undefined, '')),
    refresh: async (): Promise<AppAccountStatus> => status(await call<Native>(commands.refresh, undefined, '')),
    /** Starts the passwordless sign-in: the server e-mails a one-time link or token. No password is ever collected. */
    beginMagicLink: async (email: string): Promise<void> => { await call(commands.beginMagicLink, { email }, invalid.beginMagicLink); },
    /** Completes sign-in with the token of the e-mailed link (deep-linked back into the app, or pasted). */
    complete: async (token: string): Promise<AppAccountStatus> => status(await call<Native>(commands.complete, { token }, invalid.complete)),
    signOut: async (): Promise<void> => { await call(commands.signOut, undefined, ''); },
    devices: (): Promise<AppAccountDevice[]> => call<AppAccountDevice[]>(commands.devices, undefined, ''),
    /** `id` is the device row's `id`; the command's parameter keeps its original name `deviceIdHash`. */
    renameDevice: async (id: string, label: string): Promise<void> => { await call(commands.renameDevice, { deviceIdHash: id, label }, invalid.renameDevice); },
    /** Destructive: frees the device's seat remotely. Callers confirm first and treat a rejection as a failure. */
    removeDevice: async (id: string): Promise<void> => { await call(commands.removeDevice, { deviceIdHash: id }, invalid.removeDevice); },
    /** Permanently deletes the account. Resolves only when the service said it is deleted; a refusal or an unconfirmed answer rejects, with the session kept. */
    async deleteAccount(): Promise<void> {
      const answer = await call<AccountDeletionView>(commands.deleteAccount, undefined, '');
      if (answer.outcome === 'deleted') return;
      // Any other refusal reads as `other`, and any other answer as unconfirmed.
      const reason = answer.outcome === 'refused' && (answer.reason === 'activeSubscription' || answer.reason === 'rateLimited') ? answer.reason : 'other';
      throw new Error(accountDeletionMessage(answer.outcome === 'refused' ? { outcome: 'refused', reason } : { outcome: 'indeterminate' }, deletionMessages));
    },
  };
}
export type AppAccountSession = ReturnType<typeof createAppAccountSession>;
