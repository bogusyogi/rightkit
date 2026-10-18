/**
 * Renderer bridge for the `rightkit-tauri-shell` Tauri crate.
 *
 * Dependency-free: by default it talks to Tauri through `window.__TAURI_INTERNALS__` (what
 * `@tauri-apps/api` itself uses), so the same file runs under a bundler or as a plain ES module.
 * Pass your own transport to test or to use `@tauri-apps/api` directly.
 *
 * Event names and command names are the crate's stable contract (`rightkit-shell://*`, `rk_shell_*`).
 */

export type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
export type Unlisten = () => void;
export type Listen = <T>(event: string, handler: (payload: T) => void) => Promise<Unlisten>;
export interface ShellTransport {
  invoke: Invoke;
  listen: Listen;
}

export const EVENTS = {
  menu: 'rightkit-shell://menu',
  tray: 'rightkit-shell://tray',
  deepLink: 'rightkit-shell://deep-link',
  secondInstance: 'rightkit-shell://second-instance',
  stall: 'rightkit-shell://stall',
  engine: 'rightkit-shell://engine',
  window: 'rightkit-shell://window',
  idle: 'rightkit-shell://idle',
  presence: 'rightkit-shell://presence',
  /** Host asks the renderer to drain before exit (needs the `quitDrain` capability). */
  quitRequested: 'rightkit-shell://quit-requested',
} as const;

/** Drain budget for quit handlers when neither `createShell` nor `quit()` sets one. */
export const DEFAULT_QUIT_TIMEOUT_MS = 10_000;

/** Host features a renderer can rely on. Read with `shell.capabilities()`; missing means `false`. */
export interface ShellCapabilities {
  /** `rightkit-shell://quit-requested` plus `rk_shell_finish_quit`: quit handlers can drain. */
  quitDrain: boolean;
  /** `rk_shell_notify` honours `bypassDoNotDisturb`. */
  notifyBypassDnd: boolean;
}

export type QuitHandler = () => void | Promise<void>;
export interface QuitOptions {
  /** `true` quits at once through `rk_shell_quit` and runs no drain handlers. Default `false`. */
  force?: boolean;
  /** Maximum time to wait for drain handlers, in ms. Default: the `createShell` option, else 10 000. */
  timeoutMs?: number;
}
export interface QuitReport {
  /**
   * `drained`: every handler settled. `timed-out`: the budget elapsed first (`pending` handlers
   * were still running). `forced`: `quit({force: true})`. `immediate`: the host lacks the drain.
   */
  outcome: 'drained' | 'timed-out' | 'forced' | 'immediate';
  pending: number;
  /** Rejections from drain handlers, normalised to `ShellError`. */
  errors: ShellError[];
}
export interface ShellOptions {
  /** Default drain budget in ms (finite, >= 0). */
  quitTimeoutMs?: number;
  /** Receives every quit report that timed out or had handler errors. Default: `console.error`. */
  onQuitReport?: (report: QuitReport) => void;
}

export type MenuEntry =
  | { kind: 'item'; id: string; label: string; accelerator?: string; enabled?: boolean }
  | { kind: 'separator' };

export interface ShellStatus {
  appId: string;
  instance: string;
  tray: boolean;
  menu: { path: string; label: string }[];
  windows: { label: string; visible: boolean; title: string }[];
  trayRect: { x: number; y: number; w: number; h: number } | null;
  menuEnabled: Record<string, boolean>;
  stateDir: string;
  devtoolsOpen: boolean;
  streamScheme?: string | null;
  cacheRepair?: { dir: string; outcome: 'existing' | 'created' | 'repaired' } | null;
}

export interface DeepLink {
  scheme: string;
  host: string;
  segments: string[];
  query: Record<string, string>;
  raw: string;
}

export interface WindowSpec {
  label: string;
  title?: string;
  /** App-relative path, never a URL. */
  path?: string;
  width?: number;
  height?: number;
  focused?: boolean;
  alwaysOnTop?: boolean;
}

export type WindowAction = 'minimize' | 'maximize' | 'unmaximize' | 'toggle-maximize' | 'fullscreen' | 'hide' | 'show' | 'close' | 'start-drag';

/** macOS `SMAppService` state (`autostart` with `macosBackend: ServiceManagement`). `unknown` values arrive as `{ unknown: n }`. */
export type LoginItemStatus = 'notRegistered' | 'enabled' | 'requiresApproval' | 'notFound' | { unknown: number };

export interface AutostartStatus {
  supported: boolean;
  enabled: boolean;
  entry: string | null;
  /** Present only on the macOS SMAppService backend. `requiresApproval`: send the user to System Settings > Login Items (`rk_shell_open_login_items_settings`). */
  loginItem?: LoginItemStatus;
}

export interface TrayUpdate {
  tooltip?: string;
  /** macOS menu-bar title text. */
  title?: string;
  iconPng?: number[];
  items?: MenuEntry[];
}

export interface ClipboardContent {
  text?: string;
  html?: string;
  png?: Uint8Array;
  files?: string[];
  custom?: Record<string, Uint8Array>;
  /** Private pasteboard name (macOS). Omit for the general clipboard. */
  board?: string;
}

interface WirePayload {
  text?: string | null;
  html?: string | null;
  png?: string | null;
  files?: string[];
  custom?: Record<string, string>;
  board?: string | null;
}

/** Typed rejection from the notification / presence / path / recovery commands. */
export type ShellErrorCode =
  | 'broker_denied' | 'file_not_found' | 'permission_denied' | 'unsupported_format' | 'document_too_large'
  | 'io' | 'encoding' | 'write_conflict' | 'invalid' | 'unsupported' | 'timeout' | 'other';
export interface ShellError {
  code: ShellErrorCode;
  message: string;
}
/** Normalise any rejection (typed envelope or a legacy plain string) to `{code, message}`. */
export function asShellError(e: unknown): ShellError {
  if (e && typeof e === 'object' && 'code' in e && 'message' in e) return e as ShellError;
  return { code: 'other', message: String(e) };
}

export interface NotificationRequest {
  title: string;
  body?: string;
  /** Where a click navigates; also the per-target sound-throttle key (falls back to the title). */
  navigatePath?: string;
  soundEnabled?: boolean;
  soundName?: string;
  /**
   * Post even while Do-Not-Disturb (manual or automatic) is on. Default `false`. Needs the
   * `notifyBypassDnd` capability; otherwise `notifications.post` rejects with `unsupported`.
   * The OS Focus mode is not bypassed.
   */
  bypassDoNotDisturb?: boolean;
}
export interface NotifyOutcome {
  delivered: boolean;
  /** `dnd` while Do-Not-Disturb is in effect, `empty-title` for a blank title. */
  suppressed?: 'dnd' | 'empty-title';
  id: string | null;
  sound: boolean;
}
export interface DeliveredNotification {
  id: string;
  title: string;
  body: string;
  thread: string;
}
export interface NotifyStatus {
  authorization: string;
  supported: boolean;
  delivered: DeliveredNotification[];
  badge: number;
}

export interface PresenceState {
  manualDnd: boolean;
  autoHidden: boolean;
  autoDnd: boolean;
  effectiveDnd: boolean;
  controlsSupported: boolean;
}
export interface AutoPrefs {
  autoHideFullscreen?: boolean;
  autoDndMeetings?: boolean;
}
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}
export interface WindowObservation {
  owner: string;
  title?: string;
  layer?: number;
  bounds: Rect | null;
}
export interface Environment {
  fullscreenOnMainMonitor: boolean;
  meetingOrShareActive: boolean;
}

export interface IdleStatus {
  enabled: boolean;
  hidden: boolean;
  suspended: boolean;
  suspendAfterMs: number;
  suspends: number;
  resumes: number;
  nativeSuspend: boolean;
}

export interface EngineEvent {
  call: string;
  /** Engine request id this event belongs to (present on host builds with tagged calls). */
  id?: number | null;
  /** Caller tag passed to `engine.callTagged`, echoed on every event of that call. */
  tag?: string | null;
  event: unknown;
}

/** Typed rejection from `engine.callTagged` (`rk_shell_engine_call_tagged`). */
export interface EngineCallError {
  code: string;
  message: string;
  /** `supervisor`: refused before the engine ran. `engine`: the engine's own reply. */
  origin: 'engine' | 'supervisor';
  /** Structured error details when the host protocol carries them. */
  details?: unknown;
}

/** Engine lifecycle snapshot from `engine.status()` (`rk_shell_engine_status`). */
export interface EngineStatus {
  state: string;
  hello?: unknown;
  [key: string]: unknown;
}

/** Typed "open from clipboard" / paste result from `clipboard.item()`. */
export type ClipboardItem =
  | { kind: 'files'; paths: string[] }
  | { kind: 'image'; path: string }
  | { kind: 'text'; text: string }
  | { kind: 'none' };

/** Site-root grant for an opened document, from `site.grant()`. */
export interface SiteGrant {
  token: string;
  root: string;
  baseUrl: string;
}

interface TauriInternals {
  invoke: Invoke;
  transformCallback: (cb: (e: { payload: unknown }) => void) => number;
}

/** Transport over `window.__TAURI_INTERNALS__`. */
export function tauriTransport(): ShellTransport {
  const internals = (globalThis as unknown as { __TAURI_INTERNALS__?: TauriInternals }).__TAURI_INTERNALS__;
  if (!internals) throw new Error('rightkit-tauri-shell: not running inside a Tauri webview');
  return {
    invoke: (command, args) => internals.invoke(command, args),
    listen: async (event, handler) => {
      const id = internals.transformCallback((e) => handler(e.payload as never));
      const eventId = await internals.invoke<number>('plugin:event|listen', { event, target: { kind: 'Any' }, handler: id });
      return () => {
        void internals.invoke('plugin:event|unlisten', { event, eventId });
      };
    },
  };
}

const toBase64 = (b: Uint8Array): string => {
  let s = '';
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
};
const fromBase64 = (s: string): Uint8Array => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

/**
 * Structural twin of `@rightkit/react-shell`'s `AppShellBridge` (and `@rightkit/platform-ui`'s
 * `WindowControlAdapter`). Declared here so this package stays dependency-free; the example app
 * type-checks and runs the real `ShellProvider` against `shell.bridge()`.
 */
export interface ShellBridge {
  window: { minimize(): Promise<void>; toggleMaximize(): Promise<void>; close(): Promise<void> };
  openExternal: (url: string) => Promise<void>;
  onMenuAction: (handler: (actionId: string) => void) => Promise<() => void>;
  setNativeTheme: (theme: 'light' | 'dark' | 'system') => Promise<void>;
}

const messageOf = (e: unknown): string =>
  typeof e === 'string' ? e : e && typeof e === 'object' && 'message' in e ? String((e as { message: unknown }).message) : String(e);

const typed = (code: ShellErrorCode, message: string): ShellError => ({ code, message });

const checkTimeout = (ms: number, name: string): void => {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) throw typed('invalid', `${name} must be a finite number of milliseconds >= 0`);
};

const NO_CAPABILITIES: ShellCapabilities = { quitDrain: false, notifyBypassDnd: false };

export function createShell(transport: ShellTransport = tauriTransport(), options: ShellOptions = {}) {
  const { invoke, listen } = transport;
  const defaultTimeout = options.quitTimeoutMs ?? DEFAULT_QUIT_TIMEOUT_MS;
  checkTimeout(defaultTimeout, 'quitTimeoutMs');
  const reportQuit = options.onQuitReport ?? ((r: QuitReport) => console.error('rightkit-tauri-shell: quit drain', r));

  // Probed once per shell. A host that predates `rk_shell_capabilities` reports no capabilities.
  let capsCache: Promise<ShellCapabilities> | null = null;
  const capabilities = (): Promise<ShellCapabilities> => {
    capsCache ??= invoke<Partial<ShellCapabilities>>('rk_shell_capabilities').then(
      (c) => ({ quitDrain: c?.quitDrain === true, notifyBypassDnd: c?.notifyBypassDnd === true }),
      (e: unknown) => {
        const msg = messageOf(e);
        if (msg.includes('rk_shell_capabilities') && /not found/i.test(msg)) return NO_CAPABILITIES;
        capsCache = null;
        throw asShellError(e);
      },
    );
    return capsCache;
  };
  const requireCapability = async (key: keyof ShellCapabilities, what: string): Promise<void> => {
    if (!(await capabilities())[key]) throw typed('unsupported', `rightkit-tauri-shell host lacks ${what}; see the rightkit-tauri-shell crate README`);
  };

  // Before-quit drain. Subscribers run concurrently; one quit runs at a time.
  const quitHandlers = new Set<QuitHandler>();
  let quitListen: Promise<Unlisten> | null = null;
  let quitting: Promise<QuitReport> | null = null;

  const drain = async (timeoutMs: number): Promise<QuitReport> => {
    let pending = 0;
    const errors: ShellError[] = [];
    const runs = [...quitHandlers].map((h) => {
      pending += 1;
      return Promise.resolve()
        .then(() => h())
        .then(
          () => {
            pending -= 1;
          },
          (e: unknown) => {
            pending -= 1;
            errors.push(asShellError(e));
          },
        );
    });
    let timer: ReturnType<typeof setTimeout> | undefined;
    const budget = new Promise<'timed-out'>((resolve) => {
      timer = setTimeout(() => resolve('timed-out'), timeoutMs);
    });
    const all = Promise.all(runs).then(() => 'drained' as const);
    const outcome = await Promise.race([all, budget]);
    clearTimeout(timer);
    return { outcome, pending, errors: [...errors] };
  };

  const drainAndFinish = (timeoutMs: number): Promise<QuitReport> => {
    const run = quitting ?? (quitting = (async (): Promise<QuitReport> => {
      const report = await drain(timeoutMs);
      if (report.outcome === 'timed-out' || report.errors.length > 0) reportQuit(report);
      try {
        await invoke<void>('rk_shell_finish_quit');
      } catch (e) {
        quitting = null;
        throw asShellError(e);
      }
      return report;
    })());
    return run;
  };

  const api = {
    /** Tell the host the UI is listening: queued deep links are delivered now. Call once after subscribing. */
    ready: () => invoke<{ background: boolean; appId: string; appName: string; engine: { state: 'starting' | 'ready' | 'error'; hello?: unknown; code?: string; message?: string } | null }>('rk_shell_ready'),
    status: () => invoke<ShellStatus>('rk_shell_status'),
    /** Host feature probe. Missing fields and a host without `rk_shell_capabilities` read as `false`. */
    capabilities,

    /**
     * Quit the app. Default: run the drain handlers (see `onQuitRequested`), wait for them or the
     * timeout, then call `rk_shell_finish_quit`. Without the `quitDrain` capability it calls
     * `rk_shell_quit` at once. `force: true` always calls `rk_shell_quit` at once.
     */
    quit: async (quitOptions: QuitOptions = {}): Promise<QuitReport> => {
      if (quitOptions.force === true) {
        await invoke<void>('rk_shell_quit');
        return { outcome: 'forced', pending: 0, errors: [] };
      }
      const timeoutMs = quitOptions.timeoutMs ?? defaultTimeout;
      checkTimeout(timeoutMs, 'timeoutMs');
      if (!(await capabilities()).quitDrain) {
        await invoke<void>('rk_shell_quit');
        return { outcome: 'immediate', pending: 0, errors: [] };
      }
      return drainAndFinish(timeoutMs);
    },

    /**
     * Register a before-quit drain handler. It runs when the host asks the app to quit (tray Quit,
     * Dock, Cmd+Q) or when `quit()` is called. Returns an unsubscribe. Rejects `unsupported` when
     * the host lacks the `quitDrain` capability.
     */
    onQuitRequested: async (handler: QuitHandler): Promise<Unlisten> => {
      await requireCapability('quitDrain', 'quit drain (missing: rightkit-shell://quit-requested event and rk_shell_finish_quit command)');
      quitHandlers.add(handler);
      if (!quitListen) {
        quitListen = listen<unknown>(EVENTS.quitRequested, () => {
          drainAndFinish(defaultTimeout).catch((e: unknown) => reportQuit({ outcome: 'drained', pending: 0, errors: [asShellError(e)] }));
        }).catch((e: unknown) => {
          quitListen = null;
          throw asShellError(e);
        });
      }
      try {
        await quitListen;
      } catch (e) {
        quitHandlers.delete(handler);
        throw e;
      }
      return () => {
        quitHandlers.delete(handler);
      };
    },

    /** Open an http(s)/mailto URL in the system browser. Other schemes are refused by the host. */
    openExternal: (url: string) => invoke<void>('rk_shell_open_external', { url }),

    window: {
      do: (action: WindowAction) => invoke<void>('rk_shell_window', { action }),
      minimize: () => invoke<void>('rk_shell_window', { action: 'minimize' }),
      toggleMaximize: () => invoke<void>('rk_shell_window', { action: 'toggle-maximize' }),
      close: () => invoke<void>('rk_shell_window', { action: 'close' }),
      startDrag: () => invoke<void>('rk_shell_window', { action: 'start-drag' }),
      open: (spec: WindowSpec) => invoke<string>('rk_shell_open_window', { spec }),
      /** macOS: nudge the traffic lights from their default origin (dx right, dy down). */
      trafficLights: (dx: number, dy: number) => invoke<void>('rk_shell_traffic_lights', { dx, dy }),
    },

    menu: {
      setEnabled: (id: string, enabled: boolean) => invoke<void>('rk_shell_set_menu_item_enabled', { id, enabled }),
      onClick: (h: (id: string) => void) => listen<{ id: string }>(EVENTS.menu, (p) => h(p.id)),
    },

    tray: {
      update: (update: TrayUpdate) => invoke<void>('rk_shell_set_tray', { update }),
      /** Shell tray events: menu items (any id in the tray spec) and left clicks. */
      onEvent: (h: (e: { kind: 'menu'; id: string } | { kind: 'click' }) => void) => listen(EVENTS.tray, h),
      /**
       * An app-defined event, such as one the app's own Rust code emits (`app.emit("tray-compose", ())`).
       * The shell's tray does not emit custom names; use `onEvent` for menu item ids. Names under
       * `rightkit-shell://` are reserved and rejected as `invalid`.
       */
      onCustom: async (name: string, h: (payload: unknown) => void): Promise<Unlisten> => {
        if (!name || name.startsWith('rightkit-shell://')) throw typed('invalid', 'tray.onCustom needs a non-empty event name outside the rightkit-shell:// namespace');
        return listen<unknown>(name, h);
      },
    },

    deepLinks: {
      on: (h: (link: DeepLink) => void) => listen<DeepLink>(EVENTS.deepLink, h),
    },
    onSecondInstance: (h: (e: { argv: string[]; cwd: string }) => void) => listen(EVENTS.secondInstance, h),
    onStall: (h: (e: { stalledMs: number; thresholdMs: number }) => void) => listen(EVENTS.stall, h),

    autostart: {
      get: () => invoke<AutostartStatus>('rk_shell_autostart', { enabled: null }),
      set: (enabled: boolean) => invoke<AutostartStatus>('rk_shell_autostart', { enabled }),
    },

    clipboard: {
      read: async (board?: string): Promise<ClipboardContent> => {
        const w = await invoke<WirePayload>('rk_shell_clipboard_read', { board: board ?? null });
        return {
          text: w.text ?? undefined,
          html: w.html ?? undefined,
          png: w.png ? fromBase64(w.png) : undefined,
          files: w.files ?? [],
          custom: Object.fromEntries(Object.entries(w.custom ?? {}).map(([k, v]) => [k, fromBase64(v)])),
          board,
        };
      },
      write: (c: ClipboardContent) =>
        invoke<void>('rk_shell_clipboard_write', {
          payload: {
            text: c.text ?? null,
            html: c.html ?? null,
            png: c.png ? toBase64(c.png) : null,
            files: c.files ?? [],
            custom: Object.fromEntries(Object.entries(c.custom ?? {}).map(([k, v]) => [k, toBase64(v)])),
            board: c.board ?? null,
          },
        }),
      /**
       * The typed paste/open result: files first, then a bitmap (written to a granted temp
       * file), then text, else `{kind: 'none'}`. The `read`/`write` wrappers above are unchanged.
       */
      item: () => invoke<ClipboardItem>('rk_shell_clipboard_item'),
    },

    notifications: {
      /**
       * Post a native notification (suppressed while Do-Not-Disturb is on, unless
       * `bypassDoNotDisturb: true`, which needs the `notifyBypassDnd` capability). May wait on the
       * OS permission prompt. Without the option the request is sent exactly as before.
       */
      post: async (request: NotificationRequest): Promise<NotifyOutcome> => {
        const { bypassDoNotDisturb, ...rest } = request;
        if (bypassDoNotDisturb !== true) return invoke<NotifyOutcome>('rk_shell_notify', { request: rest });
        await requireCapability('notifyBypassDnd', 'notification bypass of Do-Not-Disturb (missing: bypassDoNotDisturb support in rk_shell_notify)');
        return invoke<NotifyOutcome>('rk_shell_notify', { request: { ...rest, bypassDoNotDisturb: true } });
      },
      /** Authorization plus what the OS reports as delivered to this app. */
      status: () => invoke<NotifyStatus>('rk_shell_notify_status'),
      clear: (ids?: string[]) => invoke<void>('rk_shell_notify_clear', { ids: ids ?? null }),
      setBadge: (count: number) => invoke<number>('rk_shell_badge', { count }),
    },

    presence: {
      state: () => invoke<PresenceState>('rk_shell_presence'),
      setDnd: (enabled: boolean) => invoke<PresenceState>('rk_shell_set_dnd', { enabled }),
      setAuto: (prefs: AutoPrefs) =>
        invoke<PresenceState>('rk_shell_presence_prefs', { prefs: { autoHideFullscreen: prefs.autoHideFullscreen ?? false, autoDndMeetings: prefs.autoDndMeetings ?? false } }),
      /** Sample the real OS: fullscreen / meeting detection over the visible windows. */
      probe: () => invoke<{ environment: Environment; windows: number; supported: boolean }>('rk_shell_presence_probe'),
      /** Run the detection policy over an injected observation; `apply` makes it the automatic state. */
      evaluate: (monitor: Rect, windows: WindowObservation[], apply = false) =>
        invoke<{ environment: Environment; state: PresenceState }>('rk_shell_presence_evaluate', { monitor, windows, apply }),
      onChange: (h: (s: PresenceState) => void) => listen<PresenceState>(EVENTS.presence, h),
    },

    idle: {
      status: () => invoke<IdleStatus>('rk_shell_idle_status'),
      /** Tell the idle machine the main window was hidden (`true`) or shown (`false`) by the app. */
      note: (hidden: boolean) => invoke<void>('rk_shell_idle_note', { hidden }),
      onChange: (h: (e: { state: 'hidden' | 'visible' | 'suspended' | 'resumed' }) => void) => listen(EVENTS.idle, h),
    },

    paths: {
      /** Canonical path if the broker granted it; rejects `broker_denied` otherwise. `write` checks a save target. */
      require: (path: string, write = false) => invoke<string>('rk_shell_path_require', { path, write }),
      revoke: (path: string) => invoke<void>('rk_shell_path_revoke', { path }),
      /** Trade a one-shot recent-file id (minted by the app in Rust) for a grant. */
      grantRecent: (id: string) => invoke<string>('rk_shell_path_grant_recent', { id }),
      regrantSaved: (path: string) => invoke<string>('rk_shell_path_regrant_saved', { path }),
      /** Stream URL for a granted file (`<scheme>://localhost/<id>`); supports Range requests. */
      streamUrl: (path: string) => invoke<{ id: string; url: string }>('rk_shell_stream_grant', { path }),
    },

    recovery: {
      save: (snapshot: unknown) => invoke<void>('rk_shell_recovery_save', { snapshot: typeof snapshot === 'string' ? snapshot : JSON.stringify(snapshot) }),
      load: async <T = unknown>(): Promise<T | null> => {
        const raw = await invoke<string | null>('rk_shell_recovery_load');
        return raw == null ? null : (JSON.parse(raw) as T);
      },
      clear: () => invoke<void>('rk_shell_recovery_clear'),
    },

    site: {
      /**
       * Grant the site root of a document the person already opened; returns `{token, root,
       * baseUrl}` for the document's `<base href>`.
       */
      grant: (document: string) => invoke<SiteGrant>('rk_shell_site_grant', { document }),
      /** Revoke a site-root token (its URLs stop serving at once and never revive). */
      revoke: (token: string) => invoke<void>('rk_shell_site_revoke', { token }),
    },

    engine: {
      /** One engine call over the stdio contract. Events stream through `onEvent` meanwhile. */
      call: <T = unknown>(method: string, params?: unknown) => invoke<T>('rk_shell_engine_call', { method, params: params ?? null }),
      /**
       * Tagged engine call: events carry the request `id` and this `tag`, and failures reject
       * with a typed `EngineCallError` instead of a plain string. `call` above is unchanged.
       */
      callTagged: <T = unknown>(method: string, params?: unknown, tag?: string) =>
        invoke<T>('rk_shell_engine_call_tagged', { method, params: params ?? null, tag: tag ?? null }),
      /** Engine lifecycle snapshot without marking the renderer ready. `null`: no engine. */
      status: () => invoke<EngineStatus | null>('rk_shell_engine_status'),
      /** `call === '$lifecycle'` carries `{state:'ready'|'error', ...}` for engine start; `ready()` replays the last one. */
      onEvent: (h: (e: EngineEvent) => void) => listen<EngineEvent>(EVENTS.engine, h),
    },
  };
  const bridge = (): ShellBridge => ({
    window: {
      minimize: () => api.window.minimize(),
      toggleMaximize: () => api.window.toggleMaximize(),
      close: () => api.window.close(),
    },
    openExternal: (url) => api.openExternal(url),
    // One id stream for native menu items, tray menu items and deep links (`deep-link:<raw url>`),
    // which is what app-shell routes to a command or shortcut with the same id.
    onMenuAction: async (handler) => {
      const offs = await Promise.all([
        api.menu.onClick(handler),
        api.tray.onEvent((e) => {
          if (e.kind === 'menu') handler(e.id);
        }),
        api.deepLinks.on((l) => handler(`deep-link:${l.raw}`)),
      ]);
      return () => offs.forEach((off) => off());
    },
    setNativeTheme: (theme) => invoke<void>('rk_shell_set_theme', { theme }),
  });
  return { ...api, bridge };
}

export type Shell = ReturnType<typeof createShell>;
