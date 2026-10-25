// Right Suite API host helpers (contract right-suite-api.v1). Dependency-free: no Tauri, no Node built-ins.
// Constants come from the generated projection; edit contracts/right-suite-api.v1.json, never this list of hosts.
// The updater key helpers mirror rightkit-license `suite_api` (crates/rightkit-license/src/suite_api.rs: signature_key_id,
// select_updater_key), so a release the native host accepts is a release these helpers accept. They only name and pick a
// listed key; the updater plugin verifies the signature, so nothing here is a verifier.
import {
  APP_ARTIFACT_HOSTS,
  RIGHT_SUITE_API_ORIGIN,
  RIGHT_SUITE_ARTIFACT_PATH_PREFIX,
  RIGHT_SUITE_PUBLIC_INSTALLER_BASE,
  RIGHT_SUITE_RELEASE_STATUS_PATH,
  RIGHT_SUITE_UPDATER_MANIFEST_PATH,
  RIGHT_SUITE_UPDATER_PUBLIC_KEYS,
  RIGHT_SUITE_UPDATER_SIGNING_KEY_ID,
} from './suite-api.generated.js';

export * from './suite-api.generated.js';

const APP_KEY = /^[a-z][a-z0-9-]*$/u;
// One installer path segment: no separators, no dot-only names, so a caller cannot escape the app's installer tree.
const INSTALLER_SEGMENT = /^[A-Za-z0-9][A-Za-z0-9._-]*$/u;
// A minisign signature is 'E', the algorithm byte ('d' legacy, 'D' prehashed), an 8 byte key id and a 64 byte signature.
const MINISIGN_SIGNATURE_LENGTH = 74;

/** A suite updater key as the contract lists it. `notAfterUnix` is the first unix second the key is no longer accepted. */
export type UpdaterKey = { kid: string; publicKey: string; notAfter: string | null; notAfterUnix: number | null };

/** Why `selectUpdaterKey` chose no key: the signature is unreadable, names no listed key, or names a key past its end. */
export type UpdaterKeyRefusal = 'malformed' | 'unknown-key' | 'key-expired';

/** Validates a Right Suite app key and returns it; throws for anything outside `^[a-z][a-z0-9-]*$`. */
export function assertAppKey(app: string): string {
  if (typeof app !== 'string' || !APP_KEY.test(app)) throw new TypeError(`invalid Right Suite app key: ${String(app)}`);
  return app;
}

/** Absolute URL of the app's release status document on the Right Suite API. */
export function releaseStatusUrl(app: string): string {
  return RIGHT_SUITE_API_ORIGIN + RIGHT_SUITE_RELEASE_STATUS_PATH.split('{app}').join(assertAppKey(app));
}

/**
 * Tauri updater endpoint template for the app. `{{target}}` is kept verbatim: the updater fills it at runtime.
 */
export function updaterEndpoint(app: string): string {
  return RIGHT_SUITE_API_ORIGIN + RIGHT_SUITE_UPDATER_MANIFEST_PATH.split('{app}').join(assertAppKey(app));
}

/**
 * Hostnames the app may download artifacts from. Per app (contract `apps.<key>.artifactHosts`); an app that the
 * contract does not list gets none, and the result is always a fresh array the caller may not widen the contract with.
 */
export function artifactHosts(app: string): string[] {
  const key = assertAppKey(app);
  const sets = APP_ARTIFACT_HOSTS as Readonly<Record<string, readonly string[]>>;
  return Object.hasOwn(sets, key) ? [...sets[key]] : [];
}

/**
 * Public installer download URL. `path` is relative to the app's installer tree and has the form
 * `<platform>/current/<file>`, for example `mac/current/ViewRight_universal.dmg`. Each segment must match
 * `[A-Za-z0-9][A-Za-z0-9._-]*`, so `..`, separators and empty segments are refused.
 */
export function publicInstallerUrl(app: string, path: string): string {
  const key = assertAppKey(app);
  if (typeof path !== 'string') throw new TypeError('installer path must be a string');
  const segments = path.split('/');
  if (segments.length < 1 || segments.some((segment) => !INSTALLER_SEGMENT.test(segment))) {
    throw new TypeError(`invalid installer path: ${path}`);
  }
  const prefix = RIGHT_SUITE_ARTIFACT_PATH_PREFIX.split('{app}').join(key);
  return `${RIGHT_SUITE_PUBLIC_INSTALLER_BASE}${prefix}/installers/${segments.join('/')}`;
}

/** Every updater key the suite accepts, the signing key included. A fresh copy: the caller may not widen the contract with it. */
export function updaterKeys(): readonly UpdaterKey[] {
  return RIGHT_SUITE_UPDATER_PUBLIC_KEYS.map((key) => ({ ...key }));
}

/** Key id (16 upper-case hex digits) of the key suite releases are signed with today; always one of `updaterKeys()`. */
export function updaterSigningKeyId(): string {
  return RIGHT_SUITE_UPDATER_SIGNING_KEY_ID;
}

// Canonical standard base64 only, which is all the native host reads. atob alone also accepts missing padding, embedded
// whitespace and non-zero bits after the last byte, so the text must survive a round trip unchanged.
function decodeBase64(text: string): Uint8Array | null {
  let binary: string;
  try {
    binary = atob(text);
  } catch {
    return null;
  }
  if (btoa(binary) !== text) return null;
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
}

/**
 * Key id named by a release signature, written as `latest.json` carries it (base64 of the minisign `.sig` file), or
 * `null` when the text is not such a signature. Reads the id only: the signature is not verified here, the updater
 * plugin does that, so a forged id can only make a download fail. Never throws.
 */
export function updaterKeyIdOfSignature(signature: string): string | null {
  if (typeof signature !== 'string') return null;
  const envelope = decodeBase64(signature.trim());
  if (envelope === null) return null;
  let text: string;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(envelope);
  } catch {
    return null;
  }
  // Line 1 is the untrusted comment; line 2 carries the algorithm, key id and signature. A CR left by CRLF ends is trimmed.
  const line = text.split('\n')[1];
  if (line === undefined) return null;
  const raw = decodeBase64(line.trim());
  if (raw === null || raw.length !== MINISIGN_SIGNATURE_LENGTH) return null;
  if (raw[0] !== 0x45 || (raw[1] !== 0x64 && raw[1] !== 0x44)) return null;
  // The id is stored little endian; the contract writes it big endian.
  return Array.from(raw.subarray(2, 10))
    .reverse()
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('')
    .toUpperCase();
}

/**
 * The listed key a release signature names, unless it is unreadable, unlisted or past its end date. `nowUnix` is the
 * current time in unix seconds; a key with an end date is refused when it is missing, so an unreadable clock never
 * extends a retired key. Picks a key only: it does not verify the signature, the updater plugin does.
 */
export function selectUpdaterKey(
  signature: string,
  nowUnix?: number,
  keys: readonly UpdaterKey[] = updaterKeys(),
): { ok: true; key: UpdaterKey } | { ok: false; reason: UpdaterKeyRefusal } {
  const kid = updaterKeyIdOfSignature(signature);
  if (kid === null) return { ok: false, reason: 'malformed' };
  const key = keys.find((candidate) => candidate.kid === kid);
  if (key === undefined) return { ok: false, reason: 'unknown-key' };
  // The comparison is written so that a missing or NaN clock fails it: both read as an unreadable clock.
  if (key.notAfterUnix !== null && !(typeof nowUnix === 'number' && nowUnix < key.notAfterUnix)) {
    return { ok: false, reason: 'key-expired' };
  }
  return { ok: true, key };
}
