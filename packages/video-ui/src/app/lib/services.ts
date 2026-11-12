import { createAutoUpdateClient, type CheckForUpdate } from "@rightkit/app-update";
import { createLicenseClient } from "@rightkit/license";
import { createLogsClient } from "@rightkit/logs";
import { videoAppHost } from "../../host.js";
import { engineBusy } from "../../lib/engine.js";

// The native command bridge is the installed host's (`VideoAppHost.invoke`/`listen`), read at call time, so
// importing this module needs no host and the clients below run over whichever host is installed.
const invoke = <T>(command: string, args?: Record<string, unknown>) => videoAppHost().invoke<T>(command, args);
const listen = <T>(event: string, handler: (event: { payload: T }) => void) => videoAppHost().listen<T>(event, handler);

/**
 * The licence runs natively (the host's `rightkit_license_*` commands behind the shell). The renderer holds no
 * token, key or device id: it reads fixed-schema views, sends the licence key or trial e-mail once as a command
 * argument, and hears `rightkit://license-changed`. Native code authorizes every protected operation itself; a
 * view is a display hint.
 */
export const licenseClient = createLicenseClient({ invoke, listen });

export const logs = createLogsClient({ invoke, allowedFields: ["surface", "state", "code"] });

let updateBusy = false;
export function setUpdateBusy(busy: boolean) {
  updateBusy = busy;
}

export async function restartAfterUpdate() {
  await videoAppHost().relaunch();
}

/**
 * Patch releases only. An entitlement-gated "update" release needs the licence's update authorization,
 * which exists only natively (rightkit-tauri-shell `update-license`); the renderer no longer holds the
 * tokens that authorization used to send, so those releases are refused here until the native updater
 * replaces this client.
 */
export const updates = createAutoUpdateClient({
  check: (options) => (videoAppHost().checkUpdate as CheckForUpdate)(options),
  // Closing a project also stops preview; engine calls cover background requests from other views.
  canInstall: () => !updateBusy && !engineBusy(),
  deferReason: () => "Close your project & finish media work before installing an update.",
  isEligible: (tier) => tier === "patch",
});
