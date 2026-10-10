// Window controls for @rightkit/app-shell's caption buttons. app-shell never imports Tauri; the app
// passes this bridge. The actions are the Tauri window API, already allowed by the capability file
// (core:window:allow-minimize, allow-toggle-maximize, allow-close).
import type { AppShellBridge } from "@rightkit/app-shell";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "./host";

const act =
  (run: (w: ReturnType<typeof getCurrentWindow>) => Promise<void>) =>
  async (): Promise<void> => {
    if (isTauri()) await run(getCurrentWindow()).catch(() => undefined);
  };

export const shellBridge: AppShellBridge = {
  window: {
    minimize: act((w) => w.minimize()),
    toggleMaximize: act((w) => w.toggleMaximize()),
    close: act((w) => w.close()),
  },
};
