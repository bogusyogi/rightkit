// HR-ACCOUNT-P1..P8 account session + device management client.
//
// Typed wrappers over the account-session Tauri commands (`src-tauri/src/account_session.rs`),
// which run the shared native `AccountSessionClient` (`rightkit-license`). The session token, the
// magic-link code, the transport and the storage never reach this module: native code answers the
// owner's token-free views (`AccountStatusView`, `AccountDeviceView`) and a closed error object
// that `errors.ts` words for the UI.
//
// HR-ACCOUNT-P1/P5: an account is required ONLY for cross-platform activation, lost-device removal,
// web device management, and recovery — never for Free use or local transcription. Nothing in the
// dictation, recording, or local-model code paths calls anything in this section; only
// AccountSettings.tsx / DeviceManagementSettings.tsx / LicenseSettings.tsx's optional Windows-link
// panel do. That is the structural half of the HR-ACCOUNT-P4 guarantee — Pro eligibility never
// reads account-session state, so an account being signed out, expired, or never linked cannot
// suppress an already-verified StoreKit or licence-key Pro entitlement.
//
// Passwordless only (HR-ACCOUNT-P2): the only entry points are a magic-link email round trip and
// (iOS-native) Sign in with Apple. There is intentionally no password field, no username field, and
// no client-side password hashing anywhere in this module.

import { invoke } from "../tauri.js";
import { accountCommandError } from "./errors.js";

/** `{ signedIn, email, expiresAt }`. `expiresAt` is a Unix-SECONDS display estimate (the native
 * view carries milliseconds; this wrapper keeps the unit the settings UI has always used). */
export type AccountSessionStatus = {
  signedIn: boolean;
  email: string | null;
  expiresAt: number | null;
};

/** The native account status view (milliseconds, plus display-only flags). */
type NativeAccountStatus = {
  signedIn: boolean;
  email: string | null;
  expiresAt: number | null;
  busy?: boolean;
  unresolved?: boolean;
};

/** One row of `account_devices_list()`, the owner's `AccountDeviceView`. `id` (the licence
 * activation row's own server-generated id) is what rename/remove key off; the licence key is
 * reduced to its last four characters natively. `platform`/`pool`/`label` are optional (the server
 * does not guarantee them) and times are Unix milliseconds. `state` stays a plain string rather
 * than a closed union: HR-PRO-011/012 requires the UI to distinguish active / renewing /
 * offline-grace / removed / revoked, so DeviceManagementSettings.tsx's `deviceStateLabel()` maps
 * known values and renders anything else literally instead of assuming a closed set. */
export type AccountDeviceRow = {
  id: string;
  licenseSuffix: string | null;
  appKey: string;
  platform: string | null;
  pool: string | null;
  label: string | null;
  state: string;
  lastSeenAt: number | null;
  activatedAt: number | null;
  generation: number;
};

function status(native: NativeAccountStatus): AccountSessionStatus {
  return {
    signedIn: native.signedIn,
    email: native.email,
    expiresAt: native.expiresAt == null ? null : Math.floor(native.expiresAt / 1000),
  };
}

async function call<T>(command: string, args: Record<string, unknown> | undefined, invalidInput: string): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (cause) {
    throw accountCommandError(cause, invalidInput);
  }
}

export async function getAccountSessionStatus(): Promise<AccountSessionStatus> {
  return status(await call<NativeAccountStatus>("account_session_status", undefined, ""));
}

export async function refreshAccountSession(): Promise<AccountSessionStatus> {
  return status(await call<NativeAccountStatus>("account_session_refresh", undefined, ""));
}

/** Start a passwordless sign-in: the server emails a one-time link/token for this `email`. No
 * password is ever collected or sent. */
export async function beginAccountMagicLink(email: string): Promise<void> {
  await call("account_session_begin_magic_link", { email }, "Enter your email address first.");
}

/** Complete sign-in with the token from the emailed magic link (deep-linked back into the app, or
 * pasted). */
export async function completeAccountSession(token: string): Promise<AccountSessionStatus> {
  return status(
    await call<NativeAccountStatus>("account_session_complete", { token }, "That sign-in link looks incomplete."),
  );
}

export async function signOutAccount(): Promise<void> {
  await call("account_session_sign_out", undefined, "");
}

export async function listAccountDevices(): Promise<AccountDeviceRow[]> {
  return call<AccountDeviceRow[]>("account_devices_list", undefined, "");
}

/** `deviceIdHash` here is the device row's `id` field (see `AccountDeviceRow`) — the command's
 * parameter name, kept literal to the original contract even though the value is not a hash. */
export async function renameAccountDevice(deviceIdHash: string, label: string): Promise<void> {
  await call("account_device_rename", { deviceIdHash, label }, "Enter a device name.");
}

/** Destructive: revokes this device's lease/authorization remotely and frees its seat. Callers
 * (DeviceManagementSettings.tsx) MUST confirm before calling this and MUST treat a rejected
 * promise as a failure, never as success — same standard HR-HISTORY-011A sets for destructive
 * History actions. `deviceIdHash` is the row's `id`.
 *
 * This is the ONE action for HR-ACCOUNT-P6's remove, reassign, AND lost-device recovery:
 * `DELETE /v1/shop/account/devices/{id}` tombstones the row, bumps `generation`, and frees the pool
 * slot transactionally (HR-PRO-009) — freeing the slot IS lost-device recovery, and it is the
 * precondition for reassignment (a replacement device then activates and takes the freed slot).
 * There is deliberately NO separate `account_device_reassign` command. */
export async function removeAccountDevice(deviceIdHash: string): Promise<void> {
  await call("account_device_remove", { deviceIdHash }, "That device is no longer on your account.");
}

type AccountDeletionAnswer =
  | { outcome: "deleted"; localSessionCleared: boolean }
  | { outcome: "refused"; reason: "activeSubscription" | "rateLimited" | "other" }
  | { outcome: "indeterminate" };

/** HR-ACCOUNT-P8: permanently deletes the signed-in the host app account — the real
 * `DELETE /v1/shop/account` (GDPR erasure) endpoint, which hard-deletes the customer/product-profile
 * row and every PII-bearing table that references it, while ANONYMIZING (not deleting) order rows
 * so purchase/accounting records survive. Resolves only when the service said the account is
 * deleted. A refusal (an active subscription must be cancelled first: the server will not cancel
 * billing inside a delete call) and an answer that cannot be told apart from "may have happened"
 * both REJECT, with the session kept. Callers MUST treat a rejected promise as a failure, never as
 * success. */
export async function deleteAccount(): Promise<void> {
  const answer = await call<AccountDeletionAnswer>("account_delete", undefined, "");
  if (answer.outcome === "deleted") return;
  if (answer.outcome === "refused") {
    throw new Error(
      answer.reason === "activeSubscription"
        ? "You have an active subscription. Cancel it before deleting your account, then try again."
        : answer.reason === "rateLimited"
          ? "Too many attempts. Try again shortly."
          : "The account service declined the deletion. Try again later.",
    );
  }
  throw new Error("We couldn't confirm your account was deleted. Check your connection and try again.");
}
