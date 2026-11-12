/**
 * What the host app supplies to the video editing components: the call to its video engine, the
 * native preview session, and the few shell services a webview cannot do itself. The package
 * imports no Tauri module; a Tauri host implements this interface over its own commands (see the
 * README), and a test or web host can implement it over anything else.
 *
 * Install it once at startup, before rendering any component: `installVideoHost(host)`.
 */
import type { EngineFrame } from "./lib/progress.js";

/** Lifecycle of the host's engine process, as the host's supervisor reports it. */
export type VideoEngineState = "starting" | "ready" | "restarting" | "failed" | "stopped";

/** The engine's `hello`. */
export type VideoEngineHello = { engine: string; version: string; contract_version: number };

/** A snapshot of the host's engine process. */
export type VideoEngineStatus = {
  state: VideoEngineState;
  hello: VideoEngineHello | null;
  restarts: number;
  last_error: string | null;
};

/** The video engine call. Failures reject with `{ code, message }`. */
export interface VideoEngineTransport {
  /** Call one engine method; events of that call echo `tag`. */
  call<T = unknown>(method: string, params: Record<string, unknown>, tag?: string): Promise<T>;
  /** Every engine event frame (`{id, tag, event}`), lifecycle frames excluded. Resolves to an unsubscribe. */
  onEvent(handler: (frame: EngineFrame) => void): Promise<() => void>;
  /** The engine process's current state. */
  status(): Promise<VideoEngineStatus>;
}

/** The native timeline/file preview session's state. */
export type PlaybackMeta = {
  kind: "timeline" | "file";
  playing: boolean;
  position_ms: number;
  frame: number;
  total_frames: number;
  duration_ms: number;
  fps: { num: number; den: number };
  width: number;
  height: number;
  audio: string;
  speed: number;
  loop?: { start_ms: number; end_ms: number } | null;
  revision: number;
};

/** The native preview session (decode, composite and audio in the host; frames to the webview). */
export interface VideoPlaybackHost {
  /** Open a timeline (`dir`) or a single file (`file`) for preview. */
  open(params: { dir?: string; file?: string; width?: number; height?: number }): Promise<PlaybackMeta>;
  /** Rebuild the open timeline preview with fresh colour chains, keeping the playhead; null when none is open. */
  refreshFinishing(dir: string): Promise<PlaybackMeta | null>;
  close(): Promise<void>;
  status(): Promise<PlaybackMeta>;
  /** Transport command (`play`, `pause`, `seek`, `step`, `speed`, `loop`, ...). */
  control(cmd: string, value: number, value2: number): Promise<PlaybackMeta>;
  /** One binary frame: `[u32 w][u32 h][u32 frame][u32 total][f64 pts]` + RGBA8. */
  frame(): Promise<ArrayBuffer | Uint8Array>;
  /** Preview-only output gain (0.0..=1.0, 0 mutes); renders never read it. */
  setGain(value: number): Promise<number>;
}

/** One filter of a file picker. */
export type VideoDialogFilter = { name: string; extensions: string[] };

/** File-picker options. */
export type VideoPickOptions = { title?: string; filters?: VideoDialogFilter[] };

/** A file drag over (or drop onto) the app window, as the host's webview reports it. */
export type VideoDragDropEvent =
  | { type: "enter"; paths: string[] }
  | { type: "over" }
  | { type: "drop"; paths: string[] }
  | { type: "leave" };

/** Every engine event payload, lifecycle frames and id-less frames included (Tauri shell `EngineEvent`). */
export type VideoEngineRawEvent = { call: string; id?: number | null; tag?: string | null; event: unknown };

/** The result of an update check the app pages hand to `@rightkit/app-update` (the host's updater handle). */
export type VideoUpdateCheck = (options?: { headers?: Record<string, string> }) => Promise<unknown>;

/**
 * What the app-level entry (`@rightkit/video-ui/app`: the desktop workspace, its pages, settings, legal gate,
 * demo mode) needs from the host beyond the editing components: the native command bridge the RightKit
 * clients (licence, logs) and the app's own commands (demo mode, media playback derivatives) run over, the
 * window, the pickers, the updater and the process. `tauriVideoHost()` (`@rightkit/video-ui/tauri`) is the
 * Tauri implementation; another host implements it over anything else.
 */
export interface VideoAppHost {
  /** Call a native command by name (Tauri `invoke` shape); rejects with the command's error. */
  invoke<T = unknown>(command: string, args?: Record<string, unknown>): Promise<T>;
  /** Listen to a native event by name (Tauri `listen` shape); resolves to an unsubscribe. */
  listen<T = unknown>(event: string, handler: (event: { payload: T }) => void): Promise<() => void>;
  /** The running app's version. */
  appVersion(): Promise<string>;
  /** Restart the app (after an update is installed). */
  relaunch(): Promise<void>;
  /** End the process with `code` at once (the legal gate's last resort when quitting fails). */
  exit(code: number): Promise<void>;
  /** Quit the app through the shell host; `force` skips drain handlers. */
  quit(options: { force: boolean }): Promise<void>;
  /** The host updater's check (Tauri: `@tauri-apps/plugin-updater` `check`). */
  checkUpdate: VideoUpdateCheck;
  /** Native pickers; each resolves null (or no paths) when the person cancels. */
  pickFolder(options: VideoPickOptions): Promise<string | null>;
  pickFile(options: VideoPickOptions): Promise<string | null>;
  pickFiles(options: VideoPickOptions): Promise<string[] | null>;
  /** The app window. */
  window: {
    minimize(): Promise<void>;
    toggleMaximize(): Promise<void>;
    close(): Promise<void>;
    setFullscreen(on: boolean): Promise<void>;
  };
  /** Every engine event payload, lifecycle and id-less frames included (the pages' progress readers);
   *  `VideoEngineTransport.onEvent` is the filtered stream. Resolves to an unsubscribe. */
  onEngineRaw(handler: (payload: VideoEngineRawEvent) => void): Promise<() => void>;
  /** File drags over the window; resolves to an unsubscribe. */
  onDragDrop(handler: (event: VideoDragDropEvent) => void): Promise<() => void>;
}

/** Everything the components need from the host app. */
export interface VideoHost {
  engine: VideoEngineTransport;
  playback: VideoPlaybackHost;
  /** A URL the webview can load for a local file (thumbnails, scopes, sources, renders). */
  assetUrl(path: string): string;
  /** Grant the webview read access to project folders and referenced media. */
  allowMediaScope(paths: string[], recursive?: boolean): Promise<void>;
  /** The host's registered id for a media file, for `@rightkit/media-ui` playback; rejects outside the media scope. */
  mediaSourceId(path: string): Promise<string>;
  /** Show a file or folder in the system file manager. */
  revealPath(path: string): Promise<void>;
  /** Required by the app-level entry (`@rightkit/video-ui/app`) only; the editing components never read it. */
  app?: VideoAppHost;
}

let current: VideoHost | null = null;

/** Install the host. Call once at startup, before rendering any component of this package. */
export function installVideoHost(host: VideoHost): void {
  current = host;
}

/** The installed host. Throws when `installVideoHost` has not been called. */
export function videoHost(): VideoHost {
  if (!current) throw new Error("@rightkit/video-ui: call installVideoHost(host) before rendering");
  return current;
}

/** The installed host's app part. Throws when the host has none (the app-level entry needs it). */
export function videoAppHost(): VideoAppHost {
  const app = videoHost().app;
  if (!app) {
    throw new Error(
      "@rightkit/video-ui/app: the installed VideoHost has no `app` part; install one that has it (Tauri: installVideoHost(tauriVideoHost()) from @rightkit/video-ui/tauri)",
    );
  }
  return app;
}
