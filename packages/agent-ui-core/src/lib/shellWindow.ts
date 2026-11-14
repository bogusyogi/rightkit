import { createShell, type WindowAction } from "@rightkit/tauri-shell";
import { invoke } from "../host/tauriHost.js";
import { listen } from "../host/tauriHost.js";

/**
 * Window commands through RightKit's typed shell client (`createShell().window`) over the
 * app's own Tauri `invoke`/`listen`, so every caller shares one transport. Rejects when there is
 * no native window (browser preview); callers decide whether that matters.
 */
export function shellWindowAction(action: WindowAction): Promise<void> {
  const shell = createShell({
    invoke,
    listen: (event, handler) => listen(event, (message) => handler(message.payload as never)),
  });
  return shell.window.do(action);
}
