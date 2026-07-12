import os from "node:os";
import path from "node:path";

export function notarytoolAuthArgs({ profile = "apple-dev-notary" } = {}) {
  const keyPath = process.env.APPLE_API_KEY_PATH?.trim();
  const keyId = process.env.APPLE_API_KEY?.trim();
  const issuer = process.env.APPLE_API_ISSUER?.trim();
  const apiValues = [keyPath, keyId, issuer];

  if (apiValues.every(Boolean)) {
    return ["--key", expandHome(keyPath), "--key-id", keyId, "--issuer", issuer];
  }
  if (apiValues.some(Boolean)) {
    throw new Error("Notarization API auth requires APPLE_API_KEY_PATH, APPLE_API_KEY, and APPLE_API_ISSUER together");
  }
  return ["--keychain-profile", profile];
}

function expandHome(value) {
  return value === "~" ? os.homedir() : value.startsWith("~/") ? path.join(os.homedir(), value.slice(2)) : value;
}
