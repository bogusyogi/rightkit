import { createShell, type WindowAction } from "@rightkit/tauri-shell";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

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
