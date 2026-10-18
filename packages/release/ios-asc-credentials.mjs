// App Store Connect API credentials for the iOS lane (bogusyogi/rightkit#349, settled decision D4: no credential sits in a file).
//
// The .p8 key, the key id and the issuer id are read at run time from the macOS login Keychain:
//   security find-generic-password -s <service> -a <account> -w
// Service names are configurable (`distribution.credentials`), with documented defaults. They are never read from config
// values, environment variables or file paths:
//   - a config key that names a path, a key id, an issuer or a secret is refused (IOS_ASC_CREDENTIAL_SOURCE_REFUSED);
//   - key material in ANY environment variable is refused on every host; the usual ASC variable names are refused on managed
//     hosts, exactly as macOS notarization does (managed-host-secrets.mjs);
//   - key material never reaches a log line, an error message or a command argument.
// `right-release ios-credentials store` writes the three items once (ios-credentials-command.mjs); the key is stored as the
// single-line base64 of its PKCS#8 DER so no newline or quoting problem can occur, and it is sent to `security -i` on stdin so
// it never appears in any process argument list.
import { createPrivateKey } from "node:crypto";
import os from "node:os";
import { spawnSync } from "./background-process.mjs";
import { refuseManagedEnvSecrets } from "./managed-host-secrets.mjs";

export const DEFAULT_ASC_SERVICES = Object.freeze({
  keyService: "rightkit-asc-api-key",
  keyIdService: "rightkit-asc-key-id",
  issuerService: "rightkit-asc-issuer-id",
});

export class IosCredentialsError extends Error {
  constructor(code, message) { super(message); this.name = "IosCredentialsError"; this.code = code; }
}
const refuse = (code, message) => { throw new IosCredentialsError(code, message); };

const SERVICE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const ACCOUNT = /^[A-Za-z0-9][A-Za-z0-9._@-]{0,63}$/;
const KEY_ID = /^[A-Z0-9]{10}$/;
const ISSUER = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const PEM_MARKER = /-----BEGIN [A-Z ]*PRIVATE KEY-----/;
const SOURCE_KEYS = ["keyService", "keyIdService", "issuerService", "account"];
// Config keys that would carry a path, an identifier or a secret instead of a Keychain service name.
const FORBIDDEN_SOURCE_KEYS = /^(?:key|keyPath|keyFile|file|path|p8|pem|keyId|keyID|issuer|issuerId|issuerID|env|envKey|password|secret|token|value)$/i;

/** Environment variable names that carry ASC key material or paths; refused on managed hosts. */
export const ASC_ENV_NAMES = Object.freeze([
  "APPLE_API_KEY_PATH", "APPLE_API_KEY", "APPLE_API_ISSUER",
  "ASC_KEY_PATH", "ASC_KEY_ID", "ASC_ISSUER_ID", "ASC_API_KEY", "ASC_API_KEY_PATH",
  "APP_STORE_CONNECT_API_KEY", "APP_STORE_CONNECT_API_KEY_PATH", "APP_STORE_CONNECT_KEY_ID", "APP_STORE_CONNECT_ISSUER_ID",
  "HR_IOS_ASC_KEY_PATH", "HR_IOS_ASC_KEY_ID", "HR_IOS_ASC_ISSUER_ID",
]);

/** Validate an optional credentials block and return the service names and account (defaults applied). */
export function resolveAscCredentialSource(credentials, { env = process.env, label = "ASC credentials" } = {}) {
  if (credentials !== undefined && (credentials === null || typeof credentials !== "object" || Array.isArray(credentials))) {
    refuse("IOS_ASC_CONFIG_INVALID", `${label} must be an object of Keychain service names`);
  }
  const given = credentials ?? {};
  for (const key of Object.keys(given)) {
    if (FORBIDDEN_SOURCE_KEYS.test(key)) {
      refuse("IOS_ASC_CREDENTIAL_SOURCE_REFUSED", `${label}.${key} is refused: App Store Connect credentials are read only from the macOS login Keychain (service names ${SOURCE_KEYS.join(", ")}), never from a file path, an inline value or the environment`);
    }
    if (!SOURCE_KEYS.includes(key)) refuse("IOS_ASC_CONFIG_INVALID", `${label} has unknown key ${key} (allowed: ${SOURCE_KEYS.join(", ")})`);
  }
  const source = {};
  for (const key of ["keyService", "keyIdService", "issuerService"]) {
    const value = given[key] ?? DEFAULT_ASC_SERVICES[key];
    if (typeof value !== "string" || !SERVICE.test(value)) refuse("IOS_ASC_CONFIG_INVALID", `${label}.${key} must be a Keychain service name (letters, digits, dot, underscore, hyphen)`);
    source[key] = value;
  }
  const names = new Set([source.keyService, source.keyIdService, source.issuerService]);
  if (names.size !== 3) refuse("IOS_ASC_CONFIG_INVALID", `${label} must use three different Keychain service names`);
  const account = given.account ?? (env.USER || os.userInfo().username);
  if (typeof account !== "string" || !ACCOUNT.test(account)) refuse("IOS_ASC_CONFIG_INVALID", `${label}.account must be a Keychain account name`);
  return { ...source, account };
}

/** Key material in any variable is refused on every host (names only); the ASC variable names are refused on managed hosts. */
export function refuseIosAscEnv(env = process.env) {
  for (const [name, value] of Object.entries(env)) {
    if (PEM_MARKER.test(String(value ?? ""))) {
      refuse("IOS_ASC_ENV_SECRET_REFUSED", `${name} is refused: App Store Connect key material must never be passed in the environment; keep it in the macOS login Keychain (right-release ios-credentials store)`);
    }
  }
  refuseManagedEnvSecrets(env, ASC_ENV_NAMES);
}

/** Parse a .p8 (PEM text) or the stored single-line base64 PKCS#8 DER into a P-256 private KeyObject. */
export function parseAscPrivateKey(text) {
  const value = String(text ?? "").trim();
  let key;
  try {
    key = PEM_MARKER.test(value) ? createPrivateKey(value) : createPrivateKey({ key: Buffer.from(value, "base64"), format: "der", type: "pkcs8" });
  } catch { refuse("IOS_ASC_KEY_INVALID", "the App Store Connect API key is not a valid .p8 private key"); }
  if (key.asymmetricKeyType !== "ec" || key.asymmetricKeyDetails?.namedCurve !== "prime256v1") refuse("IOS_ASC_KEY_INVALID", "the App Store Connect API key must be an ES256 (P-256) key");
  return key;
}

export function validateAscKeyId(value) {
  if (!KEY_ID.test(String(value ?? ""))) refuse("IOS_ASC_KEY_ID_INVALID", "the App Store Connect key id must be 10 upper-case letters or digits");
  return String(value);
}
export function validateAscIssuer(value) {
  if (!ISSUER.test(String(value ?? ""))) refuse("IOS_ASC_ISSUER_INVALID", "the App Store Connect issuer id must be a UUID");
  return String(value);
}

export function iosCredentialsStoreHint() {
  return "pipe the .p8 into: right-release ios-credentials store --key-id <KEYID> --issuer <issuer-uuid> (then delete the .p8 file)";
}

function keychainRead({ service, account, run }) {
  const result = run("security", ["find-generic-password", "-s", service, "-a", account, "-w"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], shell: false, windowsHide: true, timeout: 30_000 });
  if (result.error || result.status !== 0) return undefined;
  const value = String(result.stdout ?? "").trim();
  return value || undefined;
}

/**
 * Read the key, key id and issuer from the login Keychain. macOS only. Returns the parsed KeyObject (never the text).
 * A missing item names the service, never a value.
 */
export function readAscCredentials({ credentials, env = process.env, platform = process.platform, run = spawnSync } = {}) {
  if (platform !== "darwin") refuse("IOS_ASC_HOST_UNSUPPORTED", "App Store Connect credentials are read from the macOS login Keychain; this host is not macOS");
  refuseIosAscEnv(env);
  const source = resolveAscCredentialSource(credentials, { env });
  const read = (service, what) => {
    const value = keychainRead({ service, account: source.account, run });
    if (!value) refuse("IOS_ASC_CREDENTIAL_MISSING", `the ${what} is not in the login Keychain (service ${service}, account ${source.account}); ${iosCredentialsStoreHint()}`);
    return value;
  };
  const keyText = read(source.keyService, "App Store Connect API key");
  const keyId = validateAscKeyId(read(source.keyIdService, "App Store Connect key id"));
  const issuer = validateAscIssuer(read(source.issuerService, "App Store Connect issuer id"));
  return { keyId, issuer, privateKey: parseAscPrivateKey(keyText), source };
}

/**
 * Write the three items into the login Keychain through `security -i` (commands on stdin, so the key is never an argument).
 * The items are read back and compared; a missing or different item fails the store. Never writes the key to disk.
 */
export function storeAscCredentials({ keyText, keyId, issuer, credentials, env = process.env, platform = process.platform, run = spawnSync } = {}) {
  if (platform !== "darwin") refuse("IOS_ASC_HOST_UNSUPPORTED", "App Store Connect credentials are stored in the macOS login Keychain; this host is not macOS");
  refuseIosAscEnv(env);
  const source = resolveAscCredentialSource(credentials, { env });
  const key = parseAscPrivateKey(keyText);
  const encodedKey = key.export({ type: "pkcs8", format: "der" }).toString("base64");
  const items = [
    { service: source.keyService, value: encodedKey },
    { service: source.keyIdService, value: validateAscKeyId(keyId) },
    { service: source.issuerService, value: validateAscIssuer(issuer) },
  ];
  // Base64, a key id and a UUID need no escaping inside double quotes; service and account were validated above.
  const script = `${items.map((item) => `add-generic-password -U -a "${source.account}" -s "${item.service}" -l "${item.service}" -w "${item.value}"`).join("\n")}\nexit\n`;
  const result = run("security", ["-i"], { input: script, encoding: "utf8", stdio: ["pipe", "pipe", "pipe"], shell: false, windowsHide: true, timeout: 60_000 });
  if (result.error) refuse("IOS_ASC_STORE_FAILED", "could not run security to store the App Store Connect credentials");
  for (const item of items) {
    if (keychainRead({ service: item.service, account: source.account, run }) !== item.value) {
      refuse("IOS_ASC_STORE_FAILED", `the Keychain item ${item.service} was not stored correctly (is the login keychain unlocked in this session?)`);
    }
  }
  return { services: [source.keyService, source.keyIdService, source.issuerService], account: source.account, keyId: items[1].value, issuer: items[2].value };
}

/** Report which items exist, without returning or printing any key material. */
export function checkAscCredentials(options) {
  const credentials = readAscCredentials(options);
  return { keyId: credentials.keyId, issuer: credentials.issuer, services: [credentials.source.keyService, credentials.source.keyIdService, credentials.source.issuerService], account: credentials.source.account };
}
