// GitHub Releases lane of `right-release publish-update` (Orthic-Labs/rightkit#237).
//
// The updater artifacts and their minisign signatures are the ones the r2-rightapps lane already uses: they are
// signed once by `sign-updater.mjs` with the suite updater key (loaded from the OS credential store, never a
// key file), sealed, and re-verified by `verifyPublicationRelease`. This module only turns the same
// `platforms` map the r2 lane builds ({ <platform>: { signature, url } }) into a Tauri v2 static `latest.json`
// and uploads it, with the bundles and their `.sig` files, as assets of the GitHub release `v<version>`, so
// `https://github.com/<owner>/<repo>/releases/latest/download/latest.json` resolves. It adds no key and no trust root:
// a Right Suite app's updater keys come from the Right Suite contract (`resolveUpdaterTrust`), never from the app's config.
import { createHash, createPublicKey, verify as cryptoVerify } from "node:crypto";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { PublicationError, digestBytes, readVerifiedArtifact, verifyArtifactFile } from "./publication-artifacts.mjs";
import { inspectRemoteAsset, runCommand } from "./github-release.mjs";
import { RIGHT_SUITE_APP_KEYS, RIGHT_SUITE_UPDATER_PUBLIC_KEYS, RIGHT_SUITE_UPDATER_SIGNING_KEY_ID } from "./suite-api.generated.mjs";

const REPO_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})\/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const SEMVER_RE = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/;
// GitHub rewrites other characters in an asset name, which would make the manifest url differ from the asset.
const ASSET_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,199}$/;
// Tauri v2 updater platform keys: <os>-<arch>, optionally <os>-<arch>-<installer>.
const PLATFORM_RE = /^(?:windows|darwin|linux)-(?:x86_64|aarch64|i686|armv7)(?:-(?:nsis|msi|app|appimage|deb|rpm))?$/;
export const UPDATER_KEY_ID_RE = /^[0-9A-F]{16}$/;

export const LATEST_JSON_NAME = "latest.json";

export function assertGitHubRepository(repo) {
  if (!REPO_RE.test(repo ?? "")) throw new PublicationError("PUBLICATION_GITHUB_REPOSITORY_INVALID", "release config distribution.repository must be owner/repo for github-releases");
  return repo;
}

export function assertUpdaterPlatform(platform) {
  if (!PLATFORM_RE.test(platform ?? "")) throw new PublicationError("PUBLICATION_UPDATER_PLATFORM_INVALID", `updater platform must be a Tauri target such as windows-x86_64 or darwin-aarch64, got: ${platform}`);
  return platform;
}

export function assertAssetName(name) {
  if (!ASSET_NAME_RE.test(name ?? "")) throw new PublicationError("PUBLICATION_ASSET_NAME_INVALID", `GitHub release asset names must match ${ASSET_NAME_RE}, got: ${name}`);
  return name;
}

export function assertUpdaterVersion(version) {
  if (!SEMVER_RE.test(version ?? "")) throw new PublicationError("PUBLICATION_VERSION_INVALID", `updater version must be SemVer, got: ${version}`);
  return version;
}

export const githubTag = (version) => `v${assertUpdaterVersion(version)}`;

export function githubAssetUrl(repo, tag, name) {
  return `https://github.com/${assertGitHubRepository(repo)}/releases/download/${tag}/${assertAssetName(name)}`;
}

/** The stable URL an app configures as its updater endpoint (resolves to the newest non-draft, non-prerelease release). */
export function githubLatestJsonUrl(repo) {
  return `https://github.com/${assertGitHubRepository(repo)}/releases/latest/download/${LATEST_JSON_NAME}`;
}

/**
 * Key id (16 upper-case hex digits, as minisign prints it) of the key that made a Tauri `.sig` file's content.
 * The `.sig` text is base64 of a minisign signature file; its second line is base64 of
 * [2-byte algorithm][8-byte key id, little endian][64-byte signature].
 */
export function updaterSignatureKeyId(signatureText) {
  return parseMinisignSignature(signatureText)?.keyId ?? null;
}

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

function parseMinisignSignature(signatureText) {
  try {
    const decode = (text) => {
      if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(text)) throw new Error("invalid base64");
      const bytes = Buffer.from(text, "base64");
      if (bytes.toString("base64") !== text) throw new Error("invalid base64");
      return bytes;
    };
    const envelope = decode(String(signatureText).replace(/^[ \t\n\f\r]+|[ \t\n\f\r]+$/g, ""));
    const text = new TextDecoder("utf-8", { fatal: true }).decode(envelope);
    const lines = text.split(/\r?\n/);
    if (lines.at(-1) === "") lines.pop();
    if (lines.length !== 4 || !lines[2].startsWith("trusted comment: ")) return null;
    const raw = decode(lines[1]);
    const globalSignature = decode(lines[3]);
    if (globalSignature.length !== 64) return null;
    if (raw.length !== 74) return null;
    const algorithm = raw.subarray(0, 2).toString("latin1");
    if (algorithm !== "Ed" && algorithm !== "ED") return null;
    return { algorithm, keyId: Buffer.from(raw.subarray(2, 10)).reverse().toString("hex").toUpperCase(), signature: raw.subarray(10), globalSignature, trustedComment: Buffer.from(lines[2].slice(17), "utf8") };
  } catch { return null; }
}

function parseMinisignPublicKey(publicKeyText) {
  const text = String(publicKeyText ?? "").trim();
  try {
    const lines = Buffer.from(text, "base64").toString("utf8").split(/\r?\n/);
    const raw = Buffer.from(lines[1] ?? "", "base64");
    if (raw.length === 42 && ["Ed", "ED"].includes(raw.subarray(0, 2).toString("latin1"))) {
      return { keyId: Buffer.from(raw.subarray(2, 10)).reverse().toString("hex").toUpperCase(), key: raw.subarray(10) };
    }
  } catch { /* fall through to raw forms */ }
  try {
    const raw = Buffer.from(text, "base64");
    if (raw.length === 42 && ["Ed", "ED"].includes(raw.subarray(0, 2).toString("latin1"))) return { keyId: Buffer.from(raw.subarray(2, 10)).reverse().toString("hex").toUpperCase(), key: raw.subarray(10) };
    if (raw.length === 32) return { keyId: null, key: raw };
  } catch { /* invalid below */ }
  return null;
}

/**
 * Cryptographic minisign (Ed25519) verification over the EXACT artifact bytes. `Ed` signatures
 * cover the bytes directly; `ED` signatures cover their BLAKE2b-512 prehash. Returns false (never
 * throws) for any malformed signature/key, key-id mismatch, or bad signature.
 */
export function verifyMinisignSignature(bytes, signatureText, publicKeyText) {
  const sig = parseMinisignSignature(signatureText);
  const pub = parseMinisignPublicKey(publicKeyText);
  if (!sig || !pub || (pub.keyId && pub.keyId !== sig.keyId)) return false;
  try {
    const data = sig.algorithm === "ED" ? createHash("blake2b512").update(bytes).digest() : Buffer.from(bytes);
    const key = createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, pub.key]), format: "der", type: "spki" });
    return cryptoVerify(null, data, key, sig.signature)
      && cryptoVerify(null, Buffer.concat([sig.signature, sig.trustedComment]), key, sig.globalSignature);
  } catch { return false; }
}

/**
 * Require a cryptographically valid minisign signature over `bytes` under `publicKeyText` (for a suite app, the
 * contract key `resolveUpdaterTrust` returns). The key-id check (`assertUpdaterKeyId`) names the wrong key; this is the seal/upload gate.
 */
export function assertUpdaterSignature(bytes, signatureText, publicKeyText, label = "updater signature") {
  if (!verifyMinisignSignature(bytes, signatureText, publicKeyText)) {
    throw new PublicationError("PUBLICATION_SIGNATURE_INVALID", `${label} does not cryptographically verify over the exact artifact bytes under the declared updater public key`);
  }
}

// Case-insensitively: release config accepts app names in any case, and a different spelling must not move the trust root out of the contract.
export const isSuiteUpdaterApp = (app) => typeof app === "string" && RIGHT_SUITE_APP_KEYS.includes(app.toLowerCase());

/**
 * The updater trust a sealed signature is checked against: `{ keyId, publicKey, suite }`. `now` is unix seconds and `keys` the
 * accepted-key list, both injectable for tests.
 *
 * A Right Suite app (RIGHT_SUITE_APP_KEYS) takes its key from the contract so a config cannot swap the trust root: the key id is
 * the configured `distribution.updaterKeyId` or else the contract's signing key, it must be a listed, unexpired key, and a declared
 * public key must be that key's exact contract text. Any other app (Ember has its own updater key) keeps the declared key as the
 * only trust root and the key id stays optional.
 */
export function resolveUpdaterTrust({ app, configuredPublicKey, configuredKeyId, now = Math.floor(Date.now() / 1000), keys = RIGHT_SUITE_UPDATER_PUBLIC_KEYS } = {}) {
  if (!isSuiteUpdaterApp(app)) {
    if (typeof configuredPublicKey !== "string" || !configuredPublicKey.trim()) {
      throw new PublicationError("PUBLICATION_SIGNATURE_UNVERIFIABLE", `updater public key is required to verify updater artifacts: ${app ?? "this app"} is not a Right Suite app and declares no updater public key (distribution.updaterPublicKey); refusing an unverified signature`);
    }
    return { keyId: configuredKeyId, publicKey: configuredPublicKey, suite: false };
  }
  const keyId = configuredKeyId ?? RIGHT_SUITE_UPDATER_SIGNING_KEY_ID;
  const accepted = UPDATER_KEY_ID_RE.test(keyId) ? keys.find((key) => key.kid === keyId) : undefined;
  if (!accepted) {
    throw new PublicationError("PUBLICATION_SIGNATURE_KEY_MISMATCH", `updater key ${JSON.stringify(keyId)} is not an updater key in the Right Suite contract (key ids are 16 upper-case hex digits; accepted: ${keys.map((key) => key.kid).join(", ")}); omit distribution.updaterKeyId to use the signing key`);
  }
  if (accepted.notAfterUnix != null && accepted.notAfterUnix <= now) {
    throw new PublicationError("PUBLICATION_SIGNATURE_KEY_MISMATCH", `updater key ${keyId} ended${accepted.notAfter ? ` (notAfter ${accepted.notAfter})` : ""} in the Right Suite contract and is no longer accepted`);
  }
  const declared = configuredPublicKey !== undefined && configuredPublicKey !== null && !(typeof configuredPublicKey === "string" && !configuredPublicKey.trim());
  if (declared && configuredPublicKey !== accepted.publicKey) {
    throw new PublicationError("PUBLICATION_SIGNATURE_KEY_MISMATCH", `the declared updater public key is not the Right Suite contract key for ${keyId}; suite apps do not declare their own`);
  }
  return { keyId, publicKey: accepted.publicKey, suite: true };
}

/**
 * Refuses a signature made by any key other than `expectedKeyId`. Pass `trust.keyId` from `resolveUpdaterTrust`. An undefined id
 * is a no-op only for an app outside the Right Suite; with `app` set to a suite app the id is resolved and enforced even when omitted.
 */
export function assertUpdaterKeyId(signatureText, expectedKeyId, label = "updater signature", { app, now, keys } = {}) {
  if (isSuiteUpdaterApp(app)) expectedKeyId = resolveUpdaterTrust({ app, configuredKeyId: expectedKeyId, now, keys }).keyId;
  if (expectedKeyId === undefined) return;
  if (!UPDATER_KEY_ID_RE.test(expectedKeyId)) throw new PublicationError("PUBLICATION_SIGNATURE_KEY_MISMATCH", "distribution.updaterKeyId must be 16 upper-case hex digits");
  const actual = updaterSignatureKeyId(signatureText);
  if (actual !== expectedKeyId) throw new PublicationError("PUBLICATION_SIGNATURE_KEY_MISMATCH", `${label} was not made by updater key ${expectedKeyId} (found ${actual ?? "unreadable"})`);
}

/** Tauri v2 static manifest. `platforms` is { <target>: { signature, url } }, exactly what the r2 lane builds. */
export function buildLatestJson({ version, notes, pubDate, platforms }) {
  assertUpdaterVersion(version);
  const entries = Object.entries(platforms ?? {});
  if (!entries.length) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "latest.json needs at least one platform");
  const sorted = {};
  for (const [name, entry] of entries.sort(([a], [b]) => a.localeCompare(b))) {
    assertUpdaterPlatform(name);
    if (!entry?.signature || !/^https:\/\//.test(entry.url ?? "")) throw new PublicationError("PUBLICATION_SIGNATURE_MISSING", `latest.json platform ${name} needs a signature and an https url`);
    sorted[name] = { signature: entry.signature, url: entry.url };
  }
  return { version, notes: notes ?? "", pub_date: pubDate, platforms: sorted };
}

/** The win and mac runs publish separately to one latest.json: keep the other platform's entries for the same version. */
export function mergeLatestJson(existing, next) {
  if (!existing) return next;
  if (existing.version !== next.version) throw new PublicationError("PUBLICATION_MANIFEST_CONFLICT", `remote latest.json is for ${existing.version}, not ${next.version}; refusing to overwrite it`);
  return buildLatestJson({ version: next.version, notes: next.notes, pubDate: next.pub_date, platforms: { ...(existing.platforms ?? {}), ...next.platforms } });
}

/**
 * Uploads the sealed updater bundles, their signatures and latest.json to release `tag` of `repo`, creating the
 * (public, non-draft) release when it does not exist. Every sealed asset is byte-verified after upload; latest.json
 * is read back and compared. `assets` is [{ file, evidence }] with the sealed artifact as evidence.
 */
export function publishGitHubUpdaterRelease({ repo, tag, title, notes, assets, latest, stateDir, dryRun = false, run = runCommand }) {
  assertGitHubRepository(repo);
  const byName = new Map();
  for (const asset of assets) {
    const name = assertAssetName(path.basename(asset.file));
    if (byName.has(name) && byName.get(name).file !== asset.file) throw new PublicationError("PUBLICATION_ASSET_NAME_INVALID", `duplicate updater asset name: ${name}`);
    byName.set(name, asset);
  }
  // A dry run makes no network call: it only reports the assets it would publish.
  if (dryRun) return { status: "would-publish", tag, assets: [...byName.keys(), LATEST_JSON_NAME] };
  const existing = run("gh", ["release", "view", tag, "--repo", repo, "--json", "tagName"], { allowFailure: true });

  mkdirSync(stateDir, { recursive: true });
  if (!existing.ok) {
    const notesFile = path.join(stateDir, "release-notes.md");
    writeFileSync(notesFile, `${notes}\n`);
    try { run("gh", ["release", "create", tag, "--repo", repo, "--title", title, "--notes-file", notesFile]); }
    finally { rmSync(notesFile, { force: true }); }
  }
  const missing = [...byName.values()].filter((asset) => inspectRemoteAsset(tag, repo, asset.file, run, asset.evidence) === "missing");
  if (missing.length) run("gh", ["release", "upload", tag, ...missing.map((asset) => asset.file), "--repo", repo]);
  for (const asset of byName.values()) {
    if (inspectRemoteAsset(tag, repo, asset.file, run, asset.evidence) !== "match") throw new PublicationError("PUBLICATION_REMOTE_VERIFY_FAILED", `GitHub release asset missing after upload: ${repo}/${tag}/${path.basename(asset.file)}`);
  }

  // Merge with the other platform's entries, then upload latest.json last so it never points at a missing asset.
  const downloadDir = path.join(stateDir, "remote-latest");
  rmSync(downloadDir, { recursive: true, force: true });
  mkdirSync(downloadDir, { recursive: true });
  let remote = null;
  const fetched = run("gh", ["release", "download", tag, "--repo", repo, "--pattern", LATEST_JSON_NAME, "--dir", downloadDir], { allowFailure: true });
  if (fetched.ok) remote = JSON.parse(readFileSync(path.join(downloadDir, LATEST_JSON_NAME), "utf8"));
  else if (!/no assets match|not found/i.test(`${fetched.output ?? ""}\n${fetched.error ?? ""}`)) throw new PublicationError("PUBLICATION_REMOTE_VERIFY_FAILED", `cannot read remote latest.json: ${fetched.error || fetched.output}`);
  rmSync(downloadDir, { recursive: true, force: true });
  const merged = mergeLatestJson(remote, latest);
  const bytes = Buffer.from(`${JSON.stringify(merged, null, 2)}\n`);
  const manifestFile = path.join(stateDir, LATEST_JSON_NAME);
  writeFileSync(manifestFile, bytes);
  run("gh", ["release", "upload", tag, manifestFile, "--repo", repo, "--clobber"]);
  const verifyDir = path.join(stateDir, "verify-latest");
  rmSync(verifyDir, { recursive: true, force: true });
  mkdirSync(verifyDir, { recursive: true });
  try {
    run("gh", ["release", "download", tag, "--repo", repo, "--pattern", LATEST_JSON_NAME, "--dir", verifyDir]);
    verifyArtifactFile(path.join(verifyDir, LATEST_JSON_NAME), readVerifiedArtifact(manifestFile, { sha256: digestBytes(bytes) }));
  } finally {
    rmSync(verifyDir, { recursive: true, force: true });
  }
  return { status: existing.ok ? "updated" : "created", tag, assets: [...byName.keys(), LATEST_JSON_NAME], latestJsonUrl: githubLatestJsonUrl(repo), manifest: merged };
}
