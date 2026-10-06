/**
 * Renderer bridge for the `rightkit-shell` Tauri crate.
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
} as const;

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

export interface AutostartStatus {
  supported: boolean;
  enabled: boolean;
  entry: string | null;
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
  event: unknown;
}

interface TauriInternals {
  invoke: Invoke;
  transformCallback: (cb: (e: { payload: unknown }) => void) => number;
}

/** Transport over `window.__TAURI_INTERNALS__`. */
export function tauriTransport(): ShellTransport {
  const internals = (globalThis as unknown as { __TAURI_INTERNALS__?: TauriInternals }).__TAURI_INTERNALS__;
  if (!internals) throw new Error('rightkit-shell: not running inside a Tauri webview');
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
 * Structural twin of `@rightkit/app-shell`'s `AppShellBridge` (and `@rightkit/platform-ui`'s
 * `WindowControlAdapter`). Declared here so this package stays dependency-free; the example app
 * type-checks and runs the real `ShellProvider` against `shell.bridge()`.
 */
export interface ShellBridge {
  window: { minimize(): Promise<void>; toggleMaximize(): Promise<void>; close(): Promise<void> };
  openExternal: (url: string) => Promise<void>;
  onMenuAction: (handler: (actionId: string) => void) => Promise<() => void>;
  setNativeTheme: (theme: 'light' | 'dark' | 'system') => Promise<void>;
}

export function createShell(transport: ShellTransport = tauriTransport()) {
  const { invoke, listen } = transport;
  const api = {
    /** Tell the host the UI is listening: queued deep links are delivered now. Call once after subscribing. */
    ready: () => invoke<{ background: boolean; appId: string; appName: string; engine: { state: 'starting' | 'ready' | 'error'; hello?: unknown; code?: string; message?: string } | null }>('rk_shell_ready'),
    status: () => invoke<ShellStatus>('rk_shell_status'),
    quit: () => invoke<void>('rk_shell_quit'),

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
      onEvent: (h: (e: { kind: 'menu'; id: string } | { kind: 'click' }) => void) => listen(EVENTS.tray, h),
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
    },

    notifications: {
      /** Post a native notification (suppressed while Do-Not-Disturb is on). May wait on the OS permission prompt. */
      post: (request: NotificationRequest) => invoke<NotifyOutcome>('rk_shell_notify', { request }),
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

    engine: {
      /** One engine call over the stdio contract. Events stream through `onEvent` meanwhile. */
      call: <T = unknown>(method: string, params?: unknown) => invoke<T>('rk_shell_engine_call', { method, params: params ?? null }),
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
