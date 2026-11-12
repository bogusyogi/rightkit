import type { AutoUpdateClient, AutoUpdateStatus } from './index.js';
import type { NativeUpdateTransport } from './native/index.js';

/** The host's `{ invoke, listen }` pair (Tauri's `invoke` and `listen`); the one `./native` takes. */
export type UpdateSessionTransport = NativeUpdateTransport;

/** What a native background update loop reports, by event and as its cached last answer. */
export type BackgroundUpdateStatus =
  | { kind: 'idle' | 'up_to_date' | 'available' }
  | { kind: 'error'; message: string };

/** The work an install must not interrupt, read from the app's own state command and change event. */
export interface UpdateSessionBusyConfig<State = unknown> {
  /** Command that answers the app's current state. A read that fails counts as not busy. */
  stateCommand: string;
  /** Event the app emits when that state changes; its payload has the shape the command answers. */
  changedEvent: string;
  isBusy(state: State | null | undefined): boolean;
  /** `reason` of the `deferred` status the in-process client emits while busy. Default `host_not_idle`. */
  deferReason?: string;
}

export interface UpdateSessionOptions<State = unknown> {
  transport: UpdateSessionTransport;
  /** Restarts the app once an install is `ready`, for example `relaunch` of a `./native` client. */
  relaunch: () => Promise<void> | void;
  /** Absent: an install is always safe, and `installAndRelaunch` returns a `deferred` result instead of waiting. */
  busy?: UpdateSessionBusyConfig<State>;
  /** Whether a background check may install what it finds (the app's setting). A throw reads as false. Default false. */
  autoInstall?: () => Promise<boolean> | boolean;
  /** The native background loop: the command that answers its cached status and the event that carries each new one. */
  background?: { statusCommand: string; statusEvent: string };
  /** Status before the first check. Default `{ kind: 'idle' }`. */
  initialStatus?: AutoUpdateStatus;
  /**
   * Called when the client's status kind changes, and for every `error`. Meant for the log: the client emits a
   * status per download progress tick, and logging each one can flood the log and the IPC bridge.
   */
  onTransition?: (status: AutoUpdateStatus) => void;
  /** The client's `onError`. */
  onError?: (error: unknown) => void;
}

/**
 * The callbacks a client is built with so that its statuses and install policy run through the session. They have
 * the names `createAutoUpdateClient` takes, so they spread into its config; `createNativeAutoUpdateClient` takes
 * `onStatus` and `onError` (the native host owns the install gate and the auto-install policy).
 */
export interface UpdateSessionClientHooks {
  canInstall(): Promise<boolean>;
  shouldAutoInstall(): Promise<boolean>;
  deferReason(): string;
  onStatus(status: AutoUpdateStatus): void;
  onError(error: unknown): void;
}

export interface UpdateSession {
  /** The latest status. */
  status(): AutoUpdateStatus;
  /** Calls `listener` with the latest status now and with every later one. Returns the unsubscribe. */
  subscribe(listener: (status: AutoUpdateStatus) => void): () => void;
  /** A check the user asked for. It never installs: auto-install is off while it runs. */
  check(): Promise<AutoUpdateStatus>;
  /** `installAvailable()` of the client. */
  install(): Promise<AutoUpdateStatus>;
  /**
   * Installs once the app is not busy and relaunches on `ready`. Single-flight: a call made while one runs gets
   * the same promise. With `busy` configured it waits and retries while the result is `deferred`.
   */
  installAndRelaunch(): Promise<AutoUpdateStatus>;
  /** Back to `idle`, for a surface the user dismissed. */
  dismiss(): void;
  /**
   * Follows the native background loop (`background`); runs once. The session owns no timer. A background
   * `available` runs a check that may auto-install; it never interrupts a check, a download, a staged update or
   * a finished install.
   */
  startBackground(): void;
  /** The client, built on first use; for the staged steps (`downloadAvailable`, `installStaged`). */
  client(): AutoUpdateClient;
}

/**
 * One update session per app window: the latest status with subscribers, a manual check, an install that waits for
 * the app to be idle and then relaunches, and the reconcile with a native background loop. `createClient` receives
 * the session's hooks and returns either client: `(hooks) => createAutoUpdateClient({ check, ...hooks })` or
 * `(hooks) => createNativeAutoUpdateClient(native, { onStatus: hooks.onStatus, onError: hooks.onError })`.
 */
export function createUpdateSession<State = unknown>(
  createClient: (hooks: UpdateSessionClientHooks) => AutoUpdateClient,
  options: UpdateSessionOptions<State>,
): UpdateSession {
  const { transport, busy, background } = options;
  let started = false;
  let manualChecks = 0;
  let interactiveInstall: Promise<AutoUpdateStatus> | null = null;
  let currentStatus: AutoUpdateStatus = options.initialStatus ?? { kind: 'idle' };
  let lastLoggedStatusKind: string | null = null;
  let updateClient: AutoUpdateClient | null = null;
  const listeners = new Set<(status: AutoUpdateStatus) => void>();

  function publish(status: AutoUpdateStatus): void {
    currentStatus = status;
    for (const listener of listeners) listener(status);
  }

  async function canInstall(): Promise<boolean> {
    if (!busy) return true;
    try {
      return !busy.isBusy(await transport.invoke<State>(busy.stateCommand));
    } catch {
      return true;
    }
  }

  async function autoInstall(): Promise<boolean> {
    try {
      return (await options.autoInstall?.()) === true;
    } catch {
      return false;
    }
  }

  const hooks: UpdateSessionClientHooks = {
    canInstall,
    async shouldAutoInstall() {
      return manualChecks === 0 && autoInstall();
    },
    deferReason: () => busy?.deferReason ?? 'host_not_idle',
    onStatus(status) {
      publish(status);
      // Transitions only; an error always counts, since two failures in a row are two events.
      if (status.kind === lastLoggedStatusKind && status.kind !== 'error') return;
      lastLoggedStatusKind = status.kind;
      options.onTransition?.(status);
    },
    onError(error) {
      options.onError?.(error);
    },
  };

  function client(): AutoUpdateClient {
    updateClient ??= createClient(hooks);
    return updateClient;
  }

  async function waitUntilInstallSafe(): Promise<void> {
    if (!busy || (await canInstall())) return;

    await new Promise<void>((resolve, reject) => {
      let settled = false;
      let unlisten: (() => void) | undefined;
      const finish = () => {
        if (settled) return;
        settled = true;
        unlisten?.();
        resolve();
      };

      void transport
        .listen<State>(busy.changedEvent, (event) => {
          if (!busy.isBusy(event.payload)) finish();
        })
        .then((stop) => {
          unlisten = stop;
          if (settled) {
            stop();
            return;
          }
          // Close the race between the first state check and listener setup.
          void canInstall().then((safe) => safe && finish());
        })
        .catch(reject);
    });
  }

  function installAndRelaunch(): Promise<AutoUpdateStatus> {
    if (interactiveInstall) return interactiveInstall;

    interactiveInstall = (async () => {
      let status: AutoUpdateStatus;
      do {
        await waitUntilInstallSafe();
        status = await client().installAvailable();
        // Without a busy signal there is nothing to wait for, so a deferred result is returned, not retried.
      } while (status.kind === 'deferred' && busy);

      if (status.kind === 'ready') await options.relaunch();
      return status;
    })().finally(() => {
      interactiveInstall = null;
    });

    return interactiveInstall;
  }

  async function check(): Promise<AutoUpdateStatus> {
    // Counted, so that the first of two overlapping manual checks to finish does not re-enable auto-install.
    manualChecks += 1;
    try {
      return await client().checkOnce();
    } finally {
      manualChecks -= 1;
    }
  }

  function startBackground(): void {
    if (started || !background) return;
    started = true;
    const backgroundClient = client();

    const reconcile = (status: BackgroundUpdateStatus | null | undefined) => {
      if (!status || status.kind === 'idle') return;
      const kind = currentStatus.kind;
      if (kind === 'checking' || kind === 'downloading' || kind === 'staged' || kind === 'ready') return;
      if (status.kind === 'available') {
        void backgroundClient.checkOnce().then(undefined, () => undefined);
      } else if (status.kind === 'up_to_date') {
        publish({ kind: 'up_to_date' });
      } else if (status.kind === 'error' && kind !== 'available' && kind !== 'deferred') {
        publish({ kind: 'error', message: status.message });
      }
    };

    // Subscribe first so completion cannot fall between cache read & listener setup.
    let receivedEvent = false;
    void transport
      .listen<BackgroundUpdateStatus>(background.statusEvent, (event) => {
        receivedEvent = true;
        reconcile(event.payload);
      })
      .then(() => transport.invoke<BackgroundUpdateStatus>(background.statusCommand))
      .then((status) => {
        if (!receivedEvent) reconcile(status);
      })
      .catch(() => undefined);
  }

  return {
    status: () => currentStatus,
    subscribe(listener) {
      listeners.add(listener);
      listener(currentStatus);
      return () => {
        listeners.delete(listener);
      };
    },
    check,
    install: () => client().installAvailable(),
    installAndRelaunch,
    dismiss: () => publish({ kind: 'idle' }),
    startBackground,
    client,
  };
}
