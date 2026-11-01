import { createThemeStore, type AppShellBridge } from "@rightkit/react-shell";
import { createShell } from "@rightkit/tauri-shell";

/** Persisted light/dark/system choice (same key the pre-RightKit app used, so
 *  existing users keep their theme). */
// alwaysPaintDataTheme: false keeps the pre-0.5.0 behaviour (system leaves data-theme unset and
// the OS decides in CSS), which brand.css and shell.css were written against.
export const themeStore = createThemeStore({ storageKey: "cutright.theme", alwaysPaintDataTheme: false });

export const shell = createShell();

/** Native side of @rightkit/react-shell: caption buttons on Windows; macOS draws
 *  its traffic lights through the overlay title bar. The caption actions reach the
 *  host's `rk_shell_window` command (rightkit-tauri-shell) through @rightkit/tauri-shell. */
export const shellBridge: AppShellBridge = {
  window: {
    minimize: () => shell.window.minimize(),
    toggleMaximize: () => shell.window.toggleMaximize(),
    close: () => shell.window.close(),
  },
};

/** Quits the app at once through the shell host (`rk_shell_quit`): no drain handlers run. */
export const quitNow = () => shell.quit({ force: true });
