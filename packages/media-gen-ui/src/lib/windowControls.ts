import { isTauri } from '@tauri-apps/api/core';
import { createWindowControls } from '@rightkit/platform-ui';
import { windowControl } from './shell.js';

// Window actions for the custom title bar run through `@rightkit/tauri-shell` (`window.*`, the shell's
// `rk_shell_window` command), so the webview needs no core:window permissions. Outside Tauri
// (browser QA) they are no-ops.
const run = async (action: 'minimize' | 'toggleMaximize' | 'close') => {
  if (!isTauri()) return;
  await windowControl(action).catch(() => undefined);
};

/** Window-control adapter for the RightKit shell and onboarding title bars. */
export const WINDOW_CONTROLS = createWindowControls({
  minimize: () => run('minimize'),
  toggleMaximize: () => run('toggleMaximize'),
  close: () => run('close'),
});

export const closeWindow = () => void WINDOW_CONTROLS.close();
