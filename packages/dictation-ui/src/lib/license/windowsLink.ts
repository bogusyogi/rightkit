// HR-PRO-015 Windows link codes: pair a Windows device to an already-leased Apple device without
// typing a licence key. These wrap the native `license_windows_link_request` /
// `license_windows_link_redeem` commands (`src-tauri/src/license/link.rs`), which run the owner's
// `LicenseClient::link_request` / `link_redeem`: the proof of ownership is this device's own
// lease, and the lease a redeem returns is verified and stored natively. Nothing here sees a
// token, a key or a lease.

import type { LicenseView } from "@rightkit/license";

import { invoke } from "../tauri";
import { licenseCommandError } from "./errors";
import { notifyLicenseUpdated, refreshEntitlements } from "./core";

type LinkRequestAnswer =
  | { outcome: "issued"; code: string; expiresAt: number }
  | { outcome: "notLeased" }
  | { outcome: "notActiveLeaseHolder" };

/** HR-PRO-015: generate a short-lived, single-use Windows link code from THIS already-leased Apple
 * device, to be read aloud/typed into the Windows app. `expiresAt` is Unix milliseconds. */
export async function requestWindowsLinkCode(): Promise<{ code: string; expiresAt: number }> {
  let answer: LinkRequestAnswer;
  try {
    answer = await invoke<LinkRequestAnswer>("license_windows_link_request");
  } catch (cause) {
    throw licenseCommandError(cause, "Activate Pro on this device before generating a Windows link code.");
  }
  if (answer.outcome === "issued") return { code: answer.code, expiresAt: answer.expiresAt };
  if (answer.outcome === "notActiveLeaseHolder") {
    throw new Error("Pro isn't active on this device.");
  }
  throw new Error("Reconnect Pro on this device first.");
}

/** HR-PRO-015: redeem a Windows link code on this (Windows) device. Issues a computer-pool lease
 * exactly like a typed key, just via the code. Rejects (the input stays for a retry) when the code
 * is invalid, expired, already used, or the device pool is full. */
export async function redeemWindowsLinkCode(code: string, deviceLabel?: string): Promise<void> {
  try {
    await invoke<LicenseView>("license_windows_link_redeem", {
      code,
      deviceLabel: deviceLabel ?? null,
    });
  } catch (cause) {
    throw licenseCommandError(cause, "This link code is invalid, expired, or already used.");
  }
  await refreshEntitlements();
  notifyLicenseUpdated();
}
