import os from "node:os";
import path from "node:path";

export function notarytoolAuthArgs({ profile, env = process.env } = {}) {
  // A configured profile explicitly selects Keychain authentication. Inherited
  // API settings belong to automatic selection only.
  if (profile !== undefined) {
    if (typeof profile !== "string" || !profile.trim()) {
      throw new Error("Notarization keychain profile must be a non-empty string");
    }
    return ["--keychain-profile", profile.trim()];
  }
  const keyPath = env.APPLE_API_KEY_PATH?.trim();
  const keyId = env.APPLE_API_KEY?.trim();
  const issuer = env.APPLE_API_ISSUER?.trim();
  const apiValues = [keyPath, keyId, issuer];

  if (apiValues.every(Boolean)) {
    return ["--key", expandHome(keyPath), "--key-id", keyId, "--issuer", issuer];
  }
  if (apiValues.some(Boolean)) {
    throw new Error("Notarization API auth requires APPLE_API_KEY_PATH, APPLE_API_KEY, and APPLE_API_ISSUER together");
  }
  return ["--keychain-profile", "apple-dev-notary"];
}

function expandHome(value) {
  return value === "~" ? os.homedir() : value.startsWith("~/") ? path.join(os.homedir(), value.slice(2)) : value;
}
