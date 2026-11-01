// macOS Pro continuity (iCloud Keychain bridge token).
// Typed wrappers over the macOS-only Tauri commands in src-tauri/src/pro_bridge.rs.
// This is Apple-device Pro continuity ONLY — it never carries a transcript or
// a BYOK provider API key; those Mac sync paths are separate work outside
// this tranche. No "sync now": publish/consume are launch-driven only.

import { invoke } from "../tauri.js";

/** Status of this Mac's Apple-device Pro continuity: the local sync
 * preference, whether iCloud Keychain is reachable, whether this device
 * currently suppresses consuming the shared bridge, and the split
 * keychain-credential / backend Pro-resolution channels (HR-SYNC-002).
 * Rejects (rather than resolving `null`) outside a native Tauri runtime or on
 * a non-macOS host, since the underlying command only exists there. */
export type ProBridgeStatus = {
  /** Legacy combined flag, retained by pro_bridge.rs for back-compat. Mirrors
   *  keychainSyncEnabled; prefer the split fields below for anything that must
   *  show one channel succeeding while the other fails (HR-SYNC-002). */
  syncEnabled: boolean;
  keychainReady: boolean;
  keychainDetail?: string | null;
  consumptionSuppressed: boolean;
  keychainSyncEnabled: boolean;
  proResolutionEnabled: boolean;
  /** "off" | "blocked_no_keychain" | "ok" — "blocked_no_keychain" is its own
   *  distinct failure mode and must never be shown as plain "Off". */
  proResolutionStatus: "off" | "blocked_no_keychain" | "ok";
};

export async function getProBridgeStatus(): Promise<ProBridgeStatus> {
  return invoke<ProBridgeStatus>("pro_bridge_status");
}

/** Turn Apple-device Pro continuity on or off for this Mac. Turning it on is
 * an explicit reconnect: it clears any standing consumption suppression left
 * from a prior "Remove license" on this device. Turning it off never deletes
 * the shared iCloud item — only an explicit key removal does that. */
export async function setProBridgeSyncEnabled(enabled: boolean): Promise<void> {
  await invoke("pro_bridge_set_sync_enabled", { enabled });
}

/** Suppress (or clear) this device's ability to consume the shared Pro
 * bridge. `removeLicenseWithBridgeSuppression()` (in core.ts) already calls
 * this with `true` before deactivating — call it directly only for a bespoke
 * removal flow. Pass `false` for an explicit reconnect outside the
 * sync-preference toggle. */
export async function setProBridgeConsumptionSuppressed(suppressed: boolean): Promise<void> {
  await invoke("pro_bridge_suppress_consumption", { suppressed });
}
