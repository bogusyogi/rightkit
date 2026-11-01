import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  createLicenseClient,
  licenseMessage,
  LicenseClientError,
  type LicenseView,
} from "@rightkit/license";

export { isLicensed, isPro, hasFeature, licenseMessage } from "@rightkit/license";
export type { LicenseView } from "@rightkit/license";

/**
 * The licence as the native owner (rightkit-license through the shell's `license` host service)
 * answers it. The renderer holds fixed-schema views only: no key, token or device id is ever
 * stored, cached or read here. Activation, refresh, deactivation and the change event are the
 * shell's `rightkit_license_*` commands.
 */
export const licenseClient = createLicenseClient({ invoke, listen });

/** The user-facing text of a licence failure. The owner's errors carry a message code, never text. */
export function licenseErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof LicenseClientError) return error.message;
  // A command rejection that did not pass through the client (the link-code command).
  if (typeof error === "object" && error !== null && "message" in error) {
    const code = (error as { message: unknown }).message;
    if (typeof code === "string" && code.length > 0) {
      const text = licenseMessage(code);
      if (text) return text;
    }
  }
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  return fallback;
}

/** The view's own message (a code the owner chose), as text; empty when there is none. */
export function viewMessage(view: LicenseView | null): string {
  return view ? licenseMessage(view.message) : "";
}

/** What a change in the licence means to the rest of the app: tier, features and standing. */
export function entitlementSignature(view: LicenseView | null): string {
  if (!view) return "none";
  return [view.status, view.lifecycle, view.tier ?? "", [...view.features].sort().join(",")].join("|");
}
