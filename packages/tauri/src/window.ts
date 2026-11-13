// `@rightkit/tauri/window`: the caption-button adapter for the current Tauri window. `@tauri-apps/api` is an
// optional peer of this package that apps using this entry install; it is imported on the first call, so
// building the adapter costs nothing until a button is pressed. Errors from the window call reach the caller.

/** Structurally `WindowControlAdapter` of `@rightkit/platform-ui`, the type `@rightkit/react-shell`'s caption buttons take. */
export interface TauriWindowControls {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
}

/** Minimise, toggle maximise and close the current window. Pass it as `adapter` of `CaptionControls` or `windowControls` of the shell. */
export function tauriWindowControls(): TauriWindowControls {
  const current = async () => (await import('@tauri-apps/api/window')).getCurrentWindow();
  return {
    minimize: async () => { await (await current()).minimize(); },
    toggleMaximize: async () => { await (await current()).toggleMaximize(); },
    close: async () => { await (await current()).close(); },
  };
}
