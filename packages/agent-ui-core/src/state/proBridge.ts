// Apple device Pro continuity and Windows link codes.
//
// Typed wrappers over the Tauri commands in src-tauri/src/pro_bridge.rs
// (macOS iCloud Keychain bridge; inert stubs elsewhere) and
// src-tauri/src/license_host.rs (Windows link-code redemption). Ported from
// the host app `src/lib/license/proBridge.ts` and `windowsLink.ts`.
//
// The bridge token never reaches this layer: Rust and the Swift helper own it, and the licence
// itself is the native owner's (`state/license`). There is no "sync now": publish and consume are
// driven by launch, activation and the opt-in toggle only. A licence stored by a redemption or an
// exchange reaches the app through the owner's change event (`licenseClient.onChange`).

import { invoke } from "@tauri-apps/api/core";
import { createProBridgeClient, type ProBridgeStatus as BridgeStatus } from "@rightkit/license";
import type { LicenseView } from "./license.js";

export type { ProResolutionStatus } from "@rightkit/license";

/** Mirrors `ProBridgeStatus` in rightkit-tauri-shell `host::pro_bridge` (camelCase over the wire). */
export type ProBridgeStatus = Omit<BridgeStatus, "supported"> & {
  /** False on platforms without iCloud Keychain (Windows). */
  supported: boolean;
};

// The one client (`@rightkit/license`); its suppress and clear rule is the one every app shares.
const bridge = createProBridgeClient({
  invoke: <T>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
});

export async function getProBridgeStatus(): Promise<ProBridgeStatus> {
  return (await bridge.status()) as ProBridgeStatus;
}

/** Turn Apple device Pro continuity on or off for this Mac. Turning it on is an
 *  explicit reconnect (clears suppression after a removal). Turning it off
 *  never deletes the shared iCloud item. */
export const setProBridgeSyncEnabled = (enabled: boolean): Promise<void> =>
  bridge.setSyncEnabled(enabled);

/** Suppress (or clear) this device's ability to consume the shared bridge.
 *  Best effort: a failure here must never block a licence removal, and the
 *  command is an inert stub outside macOS. */
export const suppressProBridgeConsumption = (suppressed: boolean): Promise<void> =>
  bridge.suppressConsumption(suppressed);

/** Call after an explicit activation succeeds: clears suppression and, when
 *  the user opted in, publishes this device's activation as a bridge. Best
 *  effort; offline or unsupported never affects the local licence. */
export const notifyProBridgeAfterActivation = (): Promise<void> => bridge.notifyAfterActivation();

/** Redeem the one-time code shown on an entitled Apple device. The licence owner verifies the
 *  returned lease before storing it and answers the new view (no token). Rejects with the owner's
 *  command error (`{ code, reason?, message }`): render it with `licenseErrorMessage`. */
export async function redeemWindowsLinkCode(
  code: string,
  deviceLabel?: string,
): Promise<LicenseView> {
  return invoke<LicenseView>("license_windows_link_redeem", {
    code,
    deviceLabel: deviceLabel ?? null,
  });
}

/** Tauri commands reject with plain strings (the Rust `Err(String)`), which the
 *  shared `messageOf` would replace with its fallback. */
export function reasonOf(error: unknown, fallback: string): string {
  if (typeof error === "string" && error.trim()) return error.trim();
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  return fallback;
}
