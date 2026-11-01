// the host app licensing on @rightkit/license 0.3 (the Right Suite licence client). Free plus Pro, and a
// 30-day Pro trial the licence service issues (a signed licence of kind `trial`, tier `pro`).
//
// The client runs natively: rightkit-tauri-shell's `license` host (src-tauri/src/license_host.rs) owns
// the key exchange, the record in the OS credential store, the clock and the refresh. This file only
// calls its commands and shows the views it returns; no token, key or device id reaches the renderer.
//
// Authority: the engine verifies the signed token itself (crates/genright-engine/src/entitlement.rs).
// Nothing here unlocks a feature: the UI shows state, and every Pro command is refused by the engine
// without a valid Pro licence.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createLicenseClient, type LicenseInvoke, type LicenseListen } from '@rightkit/license';
import { engineCommand, isTauri, loadBrowserMock } from './api.js';

// Browser preview (`vite`, dev only): the mock answers `rightkit_license_status` with a Free licence and
// refuses every mutation. Release builds outside Tauri have no licence host.
const browserInvoke: LicenseInvoke = async <T>(command: string): Promise<T> =>
  (await (await loadBrowserMock()).mockLicenseInvoke(command)) as T;
const browserListen: LicenseListen = async () => () => {};

export const licenseClient = isTauri()
  ? createLicenseClient({ invoke, listen })
  : createLicenseClient({ invoke: browserInvoke, listen: browserListen });

/**
 * Once per launch: tells the engine, after every licence change, to drop its cached verdict (a content-free
 * hint; the engine re-reads and re-verifies the stored record), then renews the stored licence: the signed
 * token is short-lived, so one that lapsed since the last launch is renewed too, and a lapsed trial or a
 * revoked licence is settled by the server's answer. Never throws.
 */
export async function startLicense(): Promise<void> {
  try {
    await licenseClient.onChange(() => {
      void engineCommand('license.changed').catch(() => undefined);
    });
  } catch {
    /* no change events here (browser preview) */
  }
  try {
    const view = await licenseClient.status();
    // `status` follows the lifecycle (a lapsed token reads 'expired'), so gate on the stored record, not on the status:
    // with no record `refresh` answers `noCredential` without touching the network.
    if (view.mode === 'configured' && view.revisionState === 'active') await licenseClient.refresh();
  } catch {
    /* offline or unreadable: the engine keeps enforcing whatever token it can verify */
  }
}
