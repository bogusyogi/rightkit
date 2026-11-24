// `@rightkit/license/react`: the state machines behind a licence section, an account section and a device list.
// Hooks over ports the host binds to its own licence and account functions (`createAppLicense`,
// `createAppAccountSession`); every sentence is a message input and the markup is the host's. Display only:
// native code authorises every paid operation again when it runs. Imports only `react`.
import { useCallback, useEffect, useState } from 'react';
import type { AppAccountDevice, AppAccountStatus } from './index.js';

// ---- a subscribed value --------------------------------------------------------------------------

/** A value with a synchronous read and a subscription (the licence lifecycle, the credential-store state). */
export interface SubscribedValue<T> {
  get(): T;
  subscribe(listener: (value: T) => void): () => void;
}

/** The latest value of a store: read once, then follow its changes. */
export function useSubscribedValue<T>(store: SubscribedValue<T>): T {
  const [value, setValue] = useState<T>(store.get);
  useEffect(() => store.subscribe(setValue), [store]);
  return value;
}

/**
 * The display Pro flag: the lifecycle's own answer once it has one, `fallback` while it is still `checking`.
 * Never an authority.
 */
export function useProFlag<L extends { kind: string; isPro: boolean }>(
  lifecycle: SubscribedValue<L>,
  fallback: () => boolean,
): boolean {
  const state = useSubscribedValue(lifecycle);
  return state.kind === 'checking' ? fallback() : state.isPro;
}

const messageOf = (e: unknown): string => (e instanceof Error ? e.message : String(e));

// ---- licence section -----------------------------------------------------------------------------

export interface LicenseSettingsView {
  status: string;
  message?: unknown;
}

export interface LicenseSettingsPort<V extends LicenseSettingsView, S extends { kind: string }> {
  load(): Promise<V>;
  activate(key: string): Promise<V>;
  retryStorage(): Promise<V>;
  remove(): Promise<unknown>;
  /** Resolves false when the key could not be sent. */
  requestTrial(email: string): Promise<boolean>;
  requestWindowsLinkCode(): Promise<{ code: string; expiresAt: number }>;
  redeemWindowsLinkCode(code: string): Promise<void>;
  /** The credential-store state (`kind: 'error'` while it is unavailable). */
  storage: SubscribedValue<S>;
  /** True when the view grants the paid tier (an active licence for a lower tier is active too). */
  viewShowsPro(view: V): boolean;
  /** The words for a message code of a view; empty when none. */
  licenseText(message: V['message']): string;
  /** Respells an error text already rendered by a client. Default: unchanged. */
  errorWords?(text: string): string;
}

export interface LicenseSettingsMessages {
  proActivated: string;
  licenseActivated: string;
  keyNotAccepted: string;
  activationFailed: string;
  storageRestored: string;
  removed: string;
  removeFailed(detail: string): string;
  trialSent: string;
  trialNotSent: string;
  windowsRedeemed: string;
}

export function useLicenseSettings<V extends LicenseSettingsView, S extends { kind: string }>(
  port: LicenseSettingsPort<V, S>,
  messages: LicenseSettingsMessages,
) {
  const words = port.errorWords ?? ((text: string) => text);
  const [license, setLicense] = useState<V | null>(null);
  const [keyInput, setKeyInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState('');
  const storageState = useSubscribedValue(port.storage);
  // Trial: an email, answered by an emailed key (no jump to a website).
  const [trialOpen, setTrialOpen] = useState(false);
  const [trialEmail, setTrialEmail] = useState('');
  const [trialBusy, setTrialBusy] = useState(false);
  const [trialNote, setTrialNote] = useState('');
  // Windows link: a short-lived single-use code requested on an already-leased Apple device, redeemed here.
  const [winLinkCode, setWinLinkCode] = useState<{ code: string; expiresAt: number } | null>(null);
  const [winLinkBusy, setWinLinkBusy] = useState(false);
  const [winLinkError, setWinLinkError] = useState('');
  const [winRedeemCodeInput, setWinRedeemCodeInput] = useState('');
  const [winRedeemBusy, setWinRedeemBusy] = useState(false);
  const [winRedeemError, setWinRedeemError] = useState('');
  const [winRedeemNote, setWinRedeemNote] = useState('');

  useEffect(() => {
    let alive = true;
    port
      .load()
      .then((l) => alive && setLicense(l))
      .catch(() => {});
    return () => {
      alive = false;
    };
    // The port is the host's module-level binding.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function requestTrial() {
    const email = trialEmail.trim();
    if (!email || trialBusy) return;
    setTrialBusy(true);
    setTrialNote('');
    try {
      const ok = await port.requestTrial(email);
      if (ok) {
        setTrialNote(messages.trialSent);
        setTrialEmail('');
      } else {
        setTrialNote(messages.trialNotSent);
      }
    } finally {
      setTrialBusy(false);
    }
  }

  async function activate() {
    const trimmed = keyInput.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setNote('');
    try {
      const result = await port.activate(trimmed);
      setLicense(result);
      if (result.status === 'active') {
        setKeyInput('');
        // `active` alone is not Pro: a licence for a lower tier is active too.
        setNote(port.viewShowsPro(result) ? messages.proActivated : messages.licenseActivated);
      } else {
        setNote(port.licenseText(result.message) || messages.keyNotAccepted);
      }
    } catch (error) {
      setLicense(null);
      // A credential-store failure has its own banner; do not double it with a note.
      if (port.storage.get().kind !== 'error') {
        setNote(error instanceof Error ? words(error.message) : messages.activationFailed);
      }
    } finally {
      setBusy(false);
    }
  }

  async function retryStorage() {
    if (busy) return;
    setBusy(true);
    setNote('');
    try {
      const restored = await port.retryStorage();
      setLicense(restored);
      setNote(messages.storageRestored);
    } catch {
      setLicense(null);
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (busy) return;
    setBusy(true);
    setNote('');
    try {
      await port.remove();
      setLicense(null);
      setKeyInput('');
      setNote(messages.removed);
    } catch (error) {
      setNote(messages.removeFailed(words(error instanceof Error ? error.message : String(error))));
    } finally {
      setBusy(false);
    }
  }

  async function requestWindowsLinkCode() {
    if (winLinkBusy) return;
    setWinLinkBusy(true);
    setWinLinkError('');
    try {
      setWinLinkCode(await port.requestWindowsLinkCode());
    } catch (e) {
      setWinLinkError(messageOf(e));
    } finally {
      setWinLinkBusy(false);
    }
  }

  async function redeemWindowsLinkCode() {
    const trimmed = winRedeemCodeInput.trim();
    if (!trimmed || winRedeemBusy) return;
    setWinRedeemBusy(true);
    setWinRedeemError('');
    setWinRedeemNote('');
    try {
      await port.redeemWindowsLinkCode(trimmed);
      setWinRedeemCodeInput('');
      setWinRedeemNote(messages.windowsRedeemed);
    } catch (e) {
      // A rejected redeem (expired, already used, or wrong code) must not read as activated: the input stays
      // so the user can retry with a fresh code from the Apple device.
      setWinRedeemError(messageOf(e));
    } finally {
      setWinRedeemBusy(false);
    }
  }

  return {
    license,
    keyInput,
    setKeyInput,
    busy,
    note,
    storageState,
    activate,
    retryStorage,
    remove,
    trialOpen,
    setTrialOpen,
    trialEmail,
    setTrialEmail,
    trialBusy,
    trialNote,
    requestTrial,
    winLinkCode,
    winLinkBusy,
    winLinkError,
    requestWindowsLinkCode,
    winRedeemCodeInput,
    setWinRedeemCodeInput,
    winRedeemBusy,
    winRedeemError,
    winRedeemNote,
    redeemWindowsLinkCode,
  };
}

// ---- account section -----------------------------------------------------------------------------

export interface AccountSessionPort {
  status(): Promise<AppAccountStatus>;
  refresh(): Promise<AppAccountStatus>;
  beginMagicLink(email: string): Promise<unknown>;
  complete(token: string): Promise<AppAccountStatus>;
  signOut(): Promise<unknown>;
  deleteAccount(): Promise<unknown>;
}

export interface AccountSessionMessages {
  /** After the sign-in link was requested. */
  linkSent(email: string): string;
  signedIn: string;
  signedOut: string;
  deleted: string;
}

/**
 * Passwordless account session: status read (an unreadable status is "unknown", never "signed out"), magic-link
 * sign-in, token completion, sign-out and account deletion. A failure is never shown as success: the form
 * stays up and the error is shown.
 */
export function useAccountSession(port: AccountSessionPort, messages: AccountSessionMessages) {
  const [status, setStatus] = useState<AppAccountStatus | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [email, setEmail] = useState('');
  const [linkSent, setLinkSent] = useState(false);
  const [token, setToken] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [note, setNote] = useState('');
  const [deleteConfirming, setDeleteConfirming] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const s = await port.status();
      setStatus(s);
      setLoadFailed(false);
      if (s.signedIn) {
        try {
          setStatus(await port.refresh());
        } catch {
          // Keep local state during transient failures. A rejected token is cleared natively, so re-read once
          // to reflect the signed-out state.
          setStatus(await port.status());
        }
      }
    } catch {
      // The command is not registered, or the call failed: this must read as "unknown", never as "signed out",
      // so the sign-in form and the signed-in panel are both suppressed in favour of an honest status line.
      setLoadFailed(true);
    }
    // The port is the host's module-level binding.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function sendLink() {
    const trimmed = email.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setError('');
    try {
      await port.beginMagicLink(trimmed);
      setLinkSent(true);
      setNote(messages.linkSent(trimmed));
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function completeToken() {
    const trimmed = token.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setError('');
    try {
      const s = await port.complete(trimmed);
      setStatus(s);
      setToken('');
      setLinkSent(false);
      setEmail('');
      setNote(messages.signedIn);
    } catch (e) {
      // A failed completion is never shown as signed in: the form stays up so the user can retry.
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function signOut() {
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      await port.signOut();
      await refresh();
      setNote(messages.signedOut);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function deleteAccount() {
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      await port.deleteAccount();
      // The account no longer exists: clear the local session at once rather than waiting on a refresh.
      setDeleteConfirming(false);
      setStatus({ signedIn: false, email: null, expiresAt: null });
      setNote(messages.deleted);
    } catch (e) {
      // Left in the confirming state so the user can read the error and retry or cancel.
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return {
    status,
    signedIn: status?.signedIn === true,
    loadFailed,
    email,
    setEmail,
    linkSent,
    token,
    setToken,
    busy,
    error,
    setError,
    note,
    deleteConfirming,
    setDeleteConfirming,
    sendLink,
    completeToken,
    signOut,
    deleteAccount,
    refresh,
  };
}

// ---- device list ---------------------------------------------------------------------------------

export interface AccountDevicesPort {
  status(): Promise<{ signedIn: boolean }>;
  list(): Promise<AppAccountDevice[]>;
  rename(deviceId: string, label: string): Promise<unknown>;
  remove(deviceId: string): Promise<unknown>;
}

/**
 * The devices leased to the signed-in account. A failed read is "unknown", never "no devices": an empty list
 * would be a false statement if the call merely failed to reach the server. `signedIn` is null until read.
 */
export function useAccountDevices(port: AccountDevicesPort) {
  const [signedIn, setSignedIn] = useState<boolean | null>(null);
  const [devices, setDevices] = useState<AppAccountDevice[] | null>(null);
  const [error, setError] = useState('');

  const refresh = useCallback(async () => {
    try {
      const status = await port.status();
      setSignedIn(status.signedIn);
      if (!status.signedIn) {
        setDevices(null);
        return;
      }
      setDevices(await port.list());
      setError('');
    } catch (e) {
      setError(messageOf(e));
      setDevices(null);
    }
    // The port is the host's module-level binding.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return { signedIn, devices, error, setError, refresh };
}

/**
 * One device row: rename (a failure keeps the edit field open with the attempted value) and remove (confirmed;
 * a rejected removal renders as a failure and the row is never optimistically marked removed).
 */
export function useAccountDeviceRow(
  port: Pick<AccountDevicesPort, 'rename' | 'remove'>,
  device: AppAccountDevice,
  onChanged: () => void,
) {
  const [renaming, setRenaming] = useState(false);
  const [label, setLabel] = useState(device.label ?? '');
  const [removeConfirming, setRemoveConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [rowError, setRowError] = useState('');

  async function rename() {
    const trimmed = label.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setRowError('');
    try {
      await port.rename(device.id, trimmed);
      setRenaming(false);
      onChanged();
    } catch (e) {
      setRowError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (busy) return;
    setBusy(true);
    setRowError('');
    try {
      await port.remove(device.id);
      setRemoveConfirming(false);
      onChanged();
    } catch (e) {
      setRowError(messageOf(e));
      setRemoveConfirming(false);
    } finally {
      setBusy(false);
    }
  }

  const cancelRename = () => {
    setRenaming(false);
    setLabel(device.label ?? '');
  };

  return {
    renaming,
    setRenaming,
    label,
    setLabel,
    removeConfirming,
    setRemoveConfirming,
    busy,
    rowError,
    rename,
    cancelRename,
    remove,
  };
}
