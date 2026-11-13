// `@rightkit/tauri/window`: the caption-button adapter for a Tauri window. The app hands in how to reach its
// window (normally `getCurrentWindow` of its own `@tauri-apps/api`), so this package neither imports nor depends
// on `@tauri-apps/api`. The window is looked up on each call; errors from the window call reach the caller.

/** Structurally `WindowControlAdapter` of `@rightkit/platform-ui`, the type `@rightkit/react-shell`'s caption buttons take. */
export interface TauriWindowControls {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
}

/** The part of a Tauri 2 window the adapter calls. `Window` of `@tauri-apps/api/window` satisfies it. */
export interface TauriWindowHandle {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
}

/**
 * Minimise, toggle maximise and close a window. `current` returns the window, now or later:
 * `tauriWindowControls(async () => (await import("@tauri-apps/api/window")).getCurrentWindow())`.
 * Pass the result as `adapter` of `CaptionControls` or `windowControls` of the shell.
 */
export function tauriWindowControls(current: () => TauriWindowHandle | Promise<TauriWindowHandle>): TauriWindowControls {
  return {
    minimize: async () => { await (await current()).minimize(); },
    toggleMaximize: async () => { await (await current()).toggleMaximize(); },
    close: async () => { await (await current()).close(); },
  };
}
