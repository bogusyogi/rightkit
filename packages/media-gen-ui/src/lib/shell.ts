import { createShell } from '@rightkit/tauri-shell';

/**
 * The native shell bridge (`@rightkit/tauri-shell`), created on first use: `createShell()` throws outside a
 * Tauri webview, so the browser QA build never constructs it. The host commands (`rk_shell_*`) are registered
 * by `rightkit_tauri_shell::handler!` in `src-tauri/src/lib.rs`.
 */
let shell: ReturnType<typeof createShell> | undefined;

function bridge(): ReturnType<typeof createShell> {
  shell ??= createShell();
  return shell;
}

/** One custom-title-bar window action through the shell's `rk_shell_window` command. Rejects outside Tauri. */
export async function windowControl(action: 'minimize' | 'toggleMaximize' | 'close'): Promise<void> {
  const win = bridge().window;
  await (action === 'minimize' ? win.minimize() : action === 'toggleMaximize' ? win.toggleMaximize() : win.close());
}

/** Writes plain text to the system clipboard through the shell's clipboard command. Rejects outside Tauri. */
export async function copyText(text: string): Promise<void> {
  await bridge().clipboard.write({ text });
}
