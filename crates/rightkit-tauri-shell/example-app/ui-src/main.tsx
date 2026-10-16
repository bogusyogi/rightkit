// Real @rightkit/react-shell running on the real @rightkit/tauri-shell bridge, inside the example webview.
import { createRoot } from "react-dom/client";
import { AppShell } from "@rightkit/react-shell/react";
import type { AppShellBridge } from "@rightkit/react-shell";
import { createShell } from "@rightkit/tauri-shell";

const shell = createShell();
const native = shell.bridge();
declare global {
  interface Window {
    __appshell: { ran: string[]; themes: string[]; errors: string[] };
    __bridge: AppShellBridge;
  }
}
window.__appshell = { ran: [], themes: [], errors: [] };

// Compile-time proof that the shell's bridge satisfies app-shell's structural contract; the
// wrapper only records what app-shell asked the native side to do.
const bridge: AppShellBridge = {
  ...native,
  setNativeTheme: async (theme) => {
    window.__appshell.themes.push(theme);
    await native.setNativeTheme(theme).catch((e) => void window.__appshell.errors.push(String(e)));
  },
};
window.__bridge = bridge;

createRoot(document.getElementById("appshell-root")!).render(
  <AppShell
    bridge={bridge}
    groups={[{ items: [{ id: "home", label: "Home" }] }]}
    activeId="home"
    onNavigate={() => {}}
    title="app-shell on rightkit-tauri-shell"
    commands={[
      { id: "demo.ping", label: "Ping", run: () => void window.__appshell.ran.push("demo.ping") },
      { id: "tray.status", label: "Status", run: () => void window.__appshell.ran.push("tray.status") },
      // Deep links reach app-shell as `deep-link:<raw url>` ids.
      { id: "deep-link:rkshell-example://open/doc/7", label: "Open doc 7", run: () => void window.__appshell.ran.push("deep-link:doc/7") },
    ]}
  >
    <div id="appshell-ready">app-shell mounted</div>
  </AppShell>,
);
