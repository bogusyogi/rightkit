import type { AutoUpdateClient, AutoUpdateStatus, StagedUpdateHandle } from './index.js';

/** Which per-action approval the consent card is asking for. Download and install are separate approvals. */
export type UpdateConsentAction = 'download' | 'install';

export type UpdateOverlayPhase =
  | 'hidden'
  | 'checking'
  | 'up_to_date'
  | 'available'
  | 'available_ineligible'
  | 'deferred'
  | 'downloading'
  | 'staged'
  | 'ready'
  | 'error';

/** Framework-free view model a React/Svelte/Swift surface renders; the package never touches the DOM. */
export interface UpdateOverlayModel {
  phase: UpdateOverlayPhase;
  version?: string;
  /** Bounded 0..100 progress, present only while downloading. */
  progress?: number;
  /** Non-eligible reason (`pro_required` | `auto_install_off`) or the deferral reason. */
  reason?: string;
  message?: string;
  /** Actions the surface may offer in this phase; an ineligible update offers none. */
  actions: { check: boolean; download: boolean; install: boolean; dismiss: boolean };
}

const clampPct = (pct: number): number => (Number.isFinite(pct) ? Math.max(0, Math.min(100, Math.round(pct))) : 0);

/** Pure mapping from `AutoUpdateStatus` to the overlay model (#625). */
export function deriveUpdateOverlay(status: AutoUpdateStatus): UpdateOverlayModel {
  const none = { check: false, download: false, install: false, dismiss: false };
  switch (status.kind) {
    case 'idle':
      return { phase: 'hidden', actions: { ...none, check: true } };
    case 'checking':
      return { phase: 'checking', actions: none };
    case 'up_to_date':
      return { phase: 'up_to_date', actions: { ...none, check: true, dismiss: true } };
    case 'available':
      return status.eligible
        ? { phase: 'available', version: status.version, actions: { ...none, download: true, dismiss: true } }
        : {
            phase: 'available_ineligible',
            version: status.version,
            ...(status.reason ? { reason: status.reason } : {}),
            actions: { ...none, dismiss: true },
          };
    case 'deferred':
      return { phase: 'deferred', version: status.version, reason: status.reason, actions: { ...none, install: true, dismiss: true } };
    case 'downloading':
      return { phase: 'downloading', version: status.version, progress: clampPct(status.pct), actions: none };
    case 'staged':
      return { phase: 'staged', version: status.version, actions: { ...none, install: true, dismiss: true } };
    case 'ready':
      return { phase: 'ready', version: status.version, actions: { ...none, dismiss: true } };
    case 'error':
      return { phase: 'error', message: status.message, actions: { ...none, check: true, dismiss: true } };
  }
}

export interface UpdateConsentState {
  /** The approval currently being asked for, or null when no card is shown. */
  pending: UpdateConsentAction | null;
  busy: boolean;
  error: string | null;
}

export interface UpdateConsentController {
  state(): UpdateConsentState;
  /** Show the card for one action. Ignored while a previous approval is busy. */
  request(action: UpdateConsentAction): void;
  /** "Allow Once": runs `run(action)` for exactly this approval; the approval is consumed either way. */
  allowOnce(run: (action: UpdateConsentAction) => Promise<unknown> | unknown): Promise<void>;
  /** "Don't Allow": clears the card and runs nothing. */
  deny(): void;
}

/**
 * Per-action consent: each `allowOnce` authorises only the action shown, so approving download never approves
 * install. A failed run keeps the error visible and requires a fresh `request` before anything runs again.
 */
export function createUpdateConsent(): UpdateConsentController {
  let current: UpdateConsentState = { pending: null, busy: false, error: null };
  return {
    state: () => ({ ...current }),
    request(action) {
      if (current.busy) return;
      current = { pending: action, busy: false, error: null };
    },
    async allowOnce(run) {
      const action = current.pending;
      if (!action || current.busy) return;
      current = { pending: action, busy: true, error: null };
      try {
        await run(action);
        current = { pending: null, busy: false, error: null };
      } catch (error) {
        current = { pending: null, busy: false, error: String(error) };
      }
    },
    deny() {
      if (current.busy) return;
      current = { pending: null, busy: false, error: null };
    },
  };
}

export interface UpdateConsentOperations {
  downloadAvailable: AutoUpdateClient['downloadAvailable'];
  installStaged: AutoUpdateClient['installStaged'];
  installAvailable: AutoUpdateClient['installAvailable'];
}

/**
 * Binds each consent action to its own operation, so an approved "download" never installs:
 * - `download` runs `downloadAvailable()` and keeps the returned handle for the install approval.
 * - `install` runs `installStaged(handle)`; with no handle (a deferred install retry) it runs `installAvailable()`.
 * A failed operation throws, so `createUpdateConsent` keeps the error visible and requires a fresh request.
 * Pass the returned function to `allowOnce`.
 */
export function createUpdateConsentRunner(
  client: UpdateConsentOperations,
): (action: UpdateConsentAction) => Promise<AutoUpdateStatus> {
  let handle: StagedUpdateHandle | null = null;
  return async (action) => {
    if (action === 'download') {
      const result = await client.downloadAvailable();
      if (!result.ok) {
        if (result.status.kind === 'error') throw new Error(result.status.message);
        throw new Error('no eligible update is available to download');
      }
      handle = result.handle;
      return result.status;
    }
    const status = handle ? await client.installStaged(handle) : await client.installAvailable();
    // A deferred install keeps its handle so the next install approval retries the same update.
    if (status.kind !== 'deferred') handle = null;
    if (status.kind === 'error') throw new Error(status.message);
    if (status.kind === 'up_to_date') throw new Error('no update is ready to install');
    return status;
  };
}
