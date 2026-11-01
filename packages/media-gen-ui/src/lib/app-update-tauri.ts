import { check } from '@tauri-apps/plugin-updater';
import type { CheckForUpdate } from '@rightkit/app-update';

// Adapts Tauri's updater plugin check to RightKit's CheckForUpdate port. Only this module imports the
// plugin, so the update policy in app-update.ts stays testable without a Tauri runtime.
export const tauriCheckForUpdate: CheckForUpdate = async (options) => {
  const update = await check(options?.headers ? { headers: options.headers } : undefined);
  return update;
};
