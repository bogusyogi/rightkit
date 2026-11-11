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
