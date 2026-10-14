/**
 * The TS bridge contract between `@rightkit/app-shell` (React frame) and the native shell
 * (`@rightkit/tauri-shell` + Tauri). app-shell never imports Tauri: apps pass whatever implements these
 * structural interfaces. Every member is optional so a browser preview or a test needs none of it.
 *
 * Coordination rules with `@rightkit/tauri-shell`:
 *  - window controls: `WindowControlAdapter` is owned by `@rightkit/platform-ui`; `@rightkit/tauri-shell`
 *    returns an object that satisfies it. app-shell only calls minimize / toggleMaximize / close.
 *  - links: `openExternal` is how the shell broker routes http(s) to the system browser.
 *  - menus: native menu items arrive as `onMenuAction(id)`; app-shell routes an id that matches a
 *    registered command id or shortcut id to it (`ShellProvider` does this when `bridge` is passed).
 *  - nothing in app-shell calls `invoke`; every native effect goes through a bridge member.
 */
import type { WindowControlAdapter } from "@rightkit/platform-ui";

export interface AppShellBridge {
  /** Caption buttons for Windows/Linux; macOS draws native traffic lights. */
  window?: WindowControlAdapter;
  /** Route a URL to the system browser (webview navigation policy lives in the shell). */
  openExternal?: (url: string) => void | Promise<void>;
  /**
   * Subscribe to native-menu / tray / deep-link actions by id. Returns an unsubscribe function
   * (a promise of one is accepted, as Tauri `listen` is async).
   */
  onMenuAction?: (handler: (actionId: string) => void) => (() => void) | Promise<() => void>;
  /** Mirror theme into native chrome (window theme, tray icon). */
  setNativeTheme?: (theme: "light" | "dark" | "system") => void | Promise<void>;
}
