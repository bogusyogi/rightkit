// The Tauri implementation of `VideoHost` (with its `app` part): exactly the native calls @rightkit/video-ui
// 0.1.x made itself, now behind the host seam. A CutRight-shaped Tauri app installs it once at startup:
//
//   import { installVideoHost } from "@rightkit/video-ui/app";
//   import { tauriVideoHost } from "@rightkit/video-ui/tauri";
//   installVideoHost(tauriVideoHost());
//
// The app's src-tauri must serve the same commands 0.1.x called: the shell host (`rk_shell_*` through
// @rightkit/tauri-shell: engine call/events/status, window, quit), `playback_open`, `playback_refresh_finishing`,
// `playback_close`, `playback_status`, `playback_control`, `playback_frame`, `playback_set_gain`,
// `media_scope_allow`, `authorised_media_source_id`, `reveal_path`, and, for the app pages,
// `prepare_registered_media_playback`, `cancel_media_playback`, the `rightkit_license_*`/`rightkit_logs_*`
// commands and the `demo_*` commands and events.
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { createShell } from "@rightkit/tauri-shell";
import { pickFile, pickFiles, pickFolder } from "@rightkit/tauri/dialog";
import type { EngineFrame } from "../lib/progress.js";
import type { PlaybackMeta, VideoEngineHello, VideoEngineState, VideoEngineStatus, VideoHost } from "../host.js";

/** Created on first use (outside a Tauri webview createShell throws, so building the host must not call it). */
function lazy<T extends object>(make: () => T): T {
  let value: T | null = null;
  const get = (): T => (value ??= make());
  return new Proxy({} as T, {
    get: (_t, key) => {
      const v = Reflect.get(get(), key);
      return typeof v === "function" ? v.bind(get()) : v;
    },
    has: (_t, key) => key in get(),
  });
}

/** The app's @rightkit/tauri-shell instance (0.1.x `shell`), created on first use. */
export const shell = lazy(() => createShell());

/** The Tauri `VideoHost`, `app` part included. Building it touches no Tauri API; the first call does. */
export function tauriVideoHost(): Required<VideoHost> {
  return {
    engine: {
      // Tagged call from @rightkit/tauri-shell: events echo the tag, failures reject typed.
      call: <T>(method: string, params: Record<string, unknown>, tag?: string) => shell.engine.callTagged<T>(method, params, tag),
      // Frames are `{id: engine request id, tag: the caller's tag for that call, event}`; lifecycle frames are dropped.
      onEvent: (handler: (frame: EngineFrame) => void) =>
        shell.engine.onEvent((frame) => {
          if (frame.call === "$lifecycle" || frame.id === undefined) return;
          handler({ id: frame.id, tag: frame.tag ?? null, event: frame.event as EngineFrame["event"] });
        }),
      // Typed lifecycle snapshot (`rk_shell_engine_status`), mapped to the video engine's view model.
      status: async (): Promise<VideoEngineStatus> => {
        const status = (await shell.engine.status()) as {
          state: VideoEngineState | "error";
          hello?: VideoEngineHello | null;
          restarts?: number;
          last_error?: string | null;
          message?: string;
        } | null;
        if (!status) return { state: "failed", hello: null, restarts: 0, last_error: "no engine configured" };
        return {
          state: status.state === "error" ? "failed" : status.state,
          hello: (status.hello ?? null) as VideoEngineHello | null,
          restarts: status.restarts ?? 0,
          last_error: status.last_error ?? status.message ?? null,
        };
      },
    },
    playback: {
      open: (params) => invoke<PlaybackMeta>("playback_open", { params, toolsDir: null }),
      refreshFinishing: (dir) => invoke<PlaybackMeta | null>("playback_refresh_finishing", { dir, toolsDir: null }),
      close: () => invoke<void>("playback_close"),
      status: () => invoke<PlaybackMeta>("playback_status"),
      control: (cmd, value, value2) => invoke<PlaybackMeta>("playback_control", { cmd, value, value2 }),
      frame: () => invoke<ArrayBuffer | Uint8Array>("playback_frame"),
      setGain: (value) => invoke<number>("playback_set_gain", { value }),
    },
    assetUrl: (path) => convertFileSrc(path),
    allowMediaScope: (paths, recursive) => invoke<void>("media_scope_allow", { paths, recursive: recursive ?? null }),
    mediaSourceId: (path) => invoke<string>("authorised_media_source_id", { path }),
    revealPath: (path) => invoke<void>("reveal_path", { path }),
    app: {
      invoke: (command, args) => invoke(command, args),
      listen: (event, handler) => listen(event, handler),
      appVersion: async () => {
        const { getVersion } = await import("@tauri-apps/api/app");
        return getVersion();
      },
      relaunch: async () => {
        const { relaunch } = await import("@tauri-apps/plugin-process");
        await relaunch();
      },
      exit: async (code) => {
        const { exit } = await import("@tauri-apps/plugin-process");
        await exit(code);
      },
      quit: (options) => shell.quit(options).then(() => undefined),
      checkUpdate: async (options) => {
        const { check } = await import("@tauri-apps/plugin-updater");
        return check(options);
      },
      pickFolder: (options) => pickFolder(options),
      pickFile: (options) => pickFile(options),
      pickFiles: (options) => pickFiles(options),
      window: {
        minimize: () => shell.window.minimize(),
        toggleMaximize: () => shell.window.toggleMaximize(),
        close: () => shell.window.close(),
        setFullscreen: (on) => getCurrentWindow().setFullscreen(on),
      },
      onEngineRaw: (handler) => shell.engine.onEvent(handler),
      onDragDrop: (handler) =>
        getCurrentWebview().onDragDropEvent((e) => {
          const p = e.payload;
          if (p.type === "enter" || p.type === "drop") handler({ type: p.type, paths: p.paths });
          else handler({ type: p.type });
        }),
    },
  };
}
