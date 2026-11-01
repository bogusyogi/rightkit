import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createAutoUpdateClient } from "@rightkit/app-update";
import { createLicenseClient } from "@rightkit/license";
import { createLogsClient } from "@rightkit/logs";
import { engineBusy } from "./engine.js";

/**
 * The licence runs natively (src-tauri/src/licence.rs behind the shell's `rightkit_license_*` commands).
 * The renderer holds no token, key or device id: it reads fixed-schema views, sends the licence key or
 * trial e-mail once as a command argument, and hears `rightkit://license-changed`. Native code authorizes
 * every protected operation itself; a view is a display hint.
 */
export const licenseClient = createLicenseClient({ invoke, listen });

export const logs = createLogsClient({ invoke, allowedFields: ["surface", "state", "code"] });

let updateBusy = false;
export function setUpdateBusy(busy: boolean) {
  updateBusy = busy;
}

export async function restartAfterUpdate() {
  const { relaunch } = await import("@tauri-apps/plugin-process");
  await relaunch();
}

/**
 * Patch releases only. An entitlement-gated "update" release needs the licence's update authorization,
 * which exists only natively (rightkit-tauri-shell `update-license`); the renderer no longer holds the
 * tokens that authorization used to send, so those releases are refused here until the native updater
 * replaces this client.
 */
export const updates = createAutoUpdateClient({
  check: async (options) => {
    const { check } = await import("@tauri-apps/plugin-updater");
    return check(options);
  },
  // Closing a project also stops preview; engine calls cover background requests from other views.
  canInstall: () => !updateBusy && !engineBusy(),
  deferReason: () => "Close your project & finish media work before installing an update.",
  isEligible: (tier) => tier === "patch",
});
