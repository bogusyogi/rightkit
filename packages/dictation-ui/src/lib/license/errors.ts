// Wording for the failures of the the host app-specific licence and account commands (the ones the
// shell does not provide). Native code answers a closed object, never text: the owner's
// `{ code, reason?, message }` for licence commands (`message` a MessageCode) and
// `{ code, reason? }` for account commands. This module turns those into the copy the UI has always
// shown; nothing a server said ever reaches the screen.

import { licenseText, usLicenseWords } from "../licenseLifecycle";

// The US-English wrapper over the package's licence messages is defined in `licenseLifecycle.ts`.
export { licenseText, usLicenseWords };

type Rejection = { code?: unknown; reason?: unknown; message?: unknown };

function rejection(cause: unknown): Rejection {
  return cause && typeof cause === "object" ? (cause as Rejection) : {};
}

/** Words for a rejected licence command (link-code request and redeem). `invalidReason` is the
 * sentence for a code the service does not accept. */
export function licenseCommandError(cause: unknown, invalidReason: string): Error {
  const { code, reason, message } = rejection(cause);
  if (code === "rejected" && (reason === "keyNotAccepted" || reason === "revoked")) {
    return new Error(invalidReason);
  }
  if (code === "rejected" && reason === "deviceLimit") {
    return new Error("Device limit reached. Remove a device, then try again.");
  }
  return new Error(licenseText(message ?? codeMessage(code)));
}

function codeMessage(code: unknown): string {
  switch (code) {
    case "network":
      return "networkUnavailable";
    case "busy":
    case "unresolved":
    case "forbidden":
    case "unsupported":
    case "unconfigured":
      return code;
    case "store":
      return "storeFailed";
    default:
      return "unknown";
  }
}

/** Words for a rejected account command. `invalidInput` is the sentence for input native code
 * refused before sending anything (it differs per command). */
export function accountCommandError(cause: unknown, invalidInput: string): Error {
  const { code, reason } = rejection(cause);
  switch (code) {
    case "forbidden":
      return new Error("This window cannot manage the account.");
    case "unconfigured":
      return new Error("Accounts are not available in this build.");
    case "busy":
      return new Error("Another account operation is in progress. Try again in a moment.");
    case "unresolved":
      return new Error("The last account change did not finish. Try again.");
    case "network":
      return new Error("Could not reach the account server. Try again.");
    case "notSignedIn":
      return new Error("Sign in to manage devices on your account.");
    case "sessionExpired":
      return new Error("Your session expired. Sign in again.");
    case "superseded":
      return new Error("Your session changed while this was running. Try again.");
    case "store":
      return new Error("The account session could not be saved on this device.");
    case "badResponse":
      return new Error("The account service sent an unexpected answer. Try again.");
    case "rejected":
      switch (reason) {
        case "rateLimited":
          return new Error("Too many attempts. Try again shortly.");
        case "linkInvalid":
          return new Error("That link has expired or was already used.");
        case "deviceNotFound":
          return new Error("That device is no longer on your account.");
        default:
          return new Error(invalidInput);
      }
    default:
      return new Error("Something went wrong. Try again.");
  }
}
