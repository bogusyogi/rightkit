// GitHub Releases lane of `right-release publish-update` (Orthic-Labs/rightkit#237).
//
// The updater artifacts and their minisign signatures are the ones the r2-rightapps lane already uses: they are
// signed once by `sign-updater.mjs` with the suite updater key (the same TAURI_SIGNING_PRIVATE_KEY / key file as
// every other app), sealed, and re-verified by `verifyPublicationRelease`. This module only turns the same
// `platforms` map the r2 lane builds ({ <platform>: { signature, url } }) into a Tauri v2 static `latest.json`
// and uploads it, with the bundles and their `.sig` files, as assets of the GitHub release `v<version>`, so
// `https://github.com/<owner>/<repo>/releases/latest/download/latest.json` resolves. It adds no key and no trust root.
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { PublicationError, digestBytes, readVerifiedArtifact, verifyArtifactFile } from "./publication-artifacts.mjs";
import { inspectRemoteAsset, runCommand } from "./github-release.mjs";

const REPO_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})\/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const SEMVER_RE = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/;
// GitHub rewrites other characters in an asset name, which would make the manifest url differ from the asset.
const ASSET_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,199}$/;
// Tauri v2 updater platform keys: <os>-<arch>, optionally <os>-<arch>-<installer>.
const PLATFORM_RE = /^(?:windows|darwin|linux)-(?:x86_64|aarch64|i686|armv7)(?:-(?:nsis|msi|app|appimage|deb|rpm))?$/;
const KEY_ID_RE = /^[0-9A-F]{16}$/;

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
  try {
    const lines = Buffer.from(String(signatureText).trim(), "base64").toString("utf8").split(/\r?\n/);
    const raw = Buffer.from(lines[1] ?? "", "base64");
    if (raw.length !== 74 || raw[0] !== 0x45 || (raw[1] !== 0x64 && raw[1] !== 0x44)) return null;
    return Buffer.from(raw.subarray(2, 10)).reverse().toString("hex").toUpperCase();
  } catch {
    return null;
  }
}

/** Refuses a signature made by any key other than the configured suite key id (distribution.updaterKeyId). */
export function assertUpdaterKeyId(signatureText, expectedKeyId, label = "updater signature") {
  if (expectedKeyId === undefined) return;
  if (!KEY_ID_RE.test(expectedKeyId)) throw new PublicationError("PUBLICATION_SIGNATURE_KEY_MISMATCH", "distribution.updaterKeyId must be 16 upper-case hex digits");
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
