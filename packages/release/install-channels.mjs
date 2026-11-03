#!/usr/bin/env node
// One-command install channels, generated (rightkit#1191, docs/design/ADR-0004-one-command-install.md).
//
// `right-release install-channels --facts <facts.json> --out <dir> [--check]` renders, from the facts a release already verified and uploaded, every
// channel an app ships and writes them under `<dir>`:
//   install.sh              macOS and Linux     curl -fsSL https://<host>/<app>/install.sh | sh        (--uninstall removes)
//   install.ps1             Windows             irm https://<host>/<app>/install.ps1 | iex            (-Uninstall removes)
//   Casks/<app>.rb          Homebrew            brew install <tap>/<app>
//   winget/<Publisher>.<App>.{yaml,installer.yaml,locale.en-US.yaml}   a manifest set (submitted only on request, public apps only)
//   install-channels.json   the sha256 of every file above, so a publish step can read each back from the host and compare byte for byte (ADR I2)
// Nothing is hand-written per app, and nothing is uploaded here: this module only renders and writes files.
//
// Invariants (ADR-0004): every script pins the exact version, URL and sha256 of the immutable `versions/<ver>/` object (never `current/`) and refuses to
// run an installer before the sha256 matches (I1); macOS and Windows additionally check the platform signature and the expected signer (Team ID, Authenticode
// subject); the install lands at the same path and identity right-release's dev install uses (I4). Every fact is validated against a strict alphabet before
// it reaches a script, so no fact can break out of its quoted literal.
//
// Facts: { app, productName, version, publisher, publisherId?, description, homepage, license?, host (https origin the scripts live on), tap? (Homebrew tap, "owner/name"),
//          mac?:     { url, sha256, size, teamId, bundleId, appName, destination? ("/Applications") },
//          windows?: { url, sha256, size, signerSubject, installDirectory ("%LOCALAPPDATA%\\Programs\\<Product>"), exe },
//          linux?:   { x86_64?: { url, sha256, size }, aarch64?: { url, sha256, size }, binary, glibc? } }
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export class InstallChannelsError extends Error {
  constructor(message) { super(message); this.name = "InstallChannelsError"; this.code = "RK_INSTALL_CHANNELS"; }
}
const fail = (message) => { throw new InstallChannelsError(message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

const SLUG = /^[a-z][a-z0-9-]{1,40}$/;
const TEXT = /^[A-Za-z0-9 .,:;'()&+\-_/]{1,200}$/;
const HTTPS = /^https:\/\/[A-Za-z0-9.-]+(?::\d+)?\/[A-Za-z0-9._~%/+\-]*$/;
const SHA256 = /^[0-9a-f]{64}$/;
const VERSION = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;
const TEAM = /^[A-Z0-9]{10}$/;
const BUNDLE_ID = /^[A-Za-z0-9][A-Za-z0-9.-]{2,120}$/;
const APP_BUNDLE = /^[A-Za-z0-9][A-Za-z0-9 ._-]{0,60}\.app$/;
const EXE = /^[A-Za-z0-9][A-Za-z0-9 ._-]{0,60}\.exe$/;
const BINARY = /^[A-Za-z0-9][A-Za-z0-9._-]{0,60}$/;
const SUBJECT = /^CN=[A-Za-z0-9 .,&'-]{2,100}$/;
const WINDOWS_DIR = /^(?:%LOCALAPPDATA%|[A-Za-z]:)\\[A-Za-z0-9 ._\\-]{2,160}$/;
const MAC_DESTINATION = /^\/[A-Za-z0-9 ._/-]{1,100}$/;
const LINUX_ARCHES = ["x86_64", "aarch64"];

function checked(value, pattern, label) {
  if (typeof value !== "string" || !pattern.test(value)) fail(`${label} is missing or has characters outside its allowed alphabet: ${JSON.stringify(value)}`);
  return value;
}
function artifact(value, label) {
  if (!isObject(value)) fail(`${label} must be { url, sha256, size }`);
  checked(value.url, HTTPS, `${label}.url`);
  if (/\/current\//.test(value.url)) fail(`${label}.url must be the immutable versions/<ver>/ object, never current/ (ADR-0004 I3)`);
  checked(value.sha256, SHA256, `${label}.sha256`);
  if (!Number.isSafeInteger(value.size) || value.size <= 0) fail(`${label}.size must be a positive integer`);
  return value;
}

/** Validate the facts and return them (frozen). Throws InstallChannelsError naming the first bad fact. */
export function validateChannelFacts(facts) {
  if (!isObject(facts)) fail("facts must be an object");
  checked(facts.app, SLUG, "app");
  checked(facts.productName, /^[A-Za-z0-9][A-Za-z0-9 ._-]{0,60}$/, "productName");
  checked(facts.version, VERSION, "version");
  checked(facts.publisher, TEXT, "publisher");
  if (facts.publisherId !== undefined) checked(facts.publisherId, /^[A-Za-z][A-Za-z0-9]{1,40}$/, "publisherId");
  checked(facts.description, TEXT, "description");
  checked(facts.homepage, HTTPS, "homepage");
  checked(facts.host, /^https:\/\/[A-Za-z0-9.-]+$/, "host");
  if (facts.license !== undefined) checked(facts.license, /^[A-Za-z0-9.+ -]{1,60}$/, "license");
  if (facts.tap !== undefined) checked(facts.tap, /^[a-z0-9-]+\/[a-z0-9-]+$/, "tap");
  if (!facts.mac && !facts.windows && !facts.linux) fail("facts declare no platform (mac, windows or linux)");
  if (facts.mac) {
    artifact(facts.mac, "mac");
    if (!/\.dmg$/i.test(facts.mac.url)) fail("mac.url must be the signed .dmg");
    checked(facts.mac.teamId, TEAM, "mac.teamId");
    checked(facts.mac.bundleId, BUNDLE_ID, "mac.bundleId");
    checked(facts.mac.appName, APP_BUNDLE, "mac.appName");
    checked(facts.mac.destination ?? "/Applications", MAC_DESTINATION, "mac.destination");
  }
  if (facts.windows) {
    artifact(facts.windows, "windows");
    if (!/\.exe$/i.test(facts.windows.url)) fail("windows.url must be the signed .exe installer");
    checked(facts.windows.signerSubject, SUBJECT, "windows.signerSubject");
    checked(facts.windows.installDirectory, WINDOWS_DIR, "windows.installDirectory");
    checked(facts.windows.exe, EXE, "windows.exe");
  }
  if (facts.linux) {
    if (!isObject(facts.linux)) fail("linux must be an object");
    const arches = LINUX_ARCHES.filter((arch) => facts.linux[arch] !== undefined);
    if (!arches.length) fail("linux declares no architecture (x86_64, aarch64)");
    for (const arch of arches) { artifact(facts.linux[arch], `linux.${arch}`); if (!/\.tar\.gz$/.test(facts.linux[arch].url)) fail(`linux.${arch}.url must be a .tar.gz`); }
    checked(facts.linux.binary, BINARY, "linux.binary");
  }
  return Object.freeze(facts);
}

const header = (facts, what) => `Generated by right-release install-channels for ${facts.productName} ${facts.version}. Do not edit: ${what} is regenerated on every release.`;

export function renderInstallSh(facts) {
  const { mac, linux } = facts;
  const lines = [
    "#!/bin/sh",
    `# ${header(facts, "this script")}`,
    "# Rerun the same command to upgrade. Uninstall: sh install.sh --uninstall",
    "set -eu",
    `APP='${facts.productName}'`,
    `SLUG='${facts.app}'`,
    `VERSION='${facts.version}'`,
    'TMP="$(mktemp -d)"',
    'trap \'rm -rf "$TMP"\' EXIT INT TERM',
    'die() { printf "%s install: %s\\n" "$SLUG" "$1" >&2; exit 1; }',
    'sha256_of() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d " " -f 1; else sha256sum "$1" | cut -d " " -f 1; fi; }',
    'fetch() { curl -fsSL --proto "=https" --tlsv1.2 -o "$2" "$1" || die "download failed: $1"; }',
    'verify_sha() { [ "$(sha256_of "$1")" = "$2" ] || die "sha256 mismatch for $1: refusing to install"; }',
    'OS="$(uname -s)"; ARCH="$(uname -m)"',
    'UNINSTALL=0; [ "${1:-}" = "--uninstall" ] && UNINSTALL=1',
    'case "$OS" in',
  ];
  if (mac) {
    const dest = mac.destination ?? "/Applications";
    lines.push(
      "  Darwin)",
      `    DEST='${dest}'; APPDIR='${mac.appName}'; TEAM='${mac.teamId}'; BUNDLE='${mac.bundleId}'`,
      `    URL='${mac.url}'; SHA='${mac.sha256}'`,
      '    if [ "$UNINSTALL" = 1 ]; then rm -rf "$DEST/$APPDIR"; printf "%s removed from %s\\n" "$APP" "$DEST"; exit 0; fi',
      '    fetch "$URL" "$TMP/app.dmg"; verify_sha "$TMP/app.dmg" "$SHA"',
      '    hdiutil attach -nobrowse -readonly -quiet -mountpoint "$TMP/mnt" "$TMP/app.dmg" || die "cannot mount the disk image"',
      '    trap \'hdiutil detach -quiet "$TMP/mnt" >/dev/null 2>&1 || true; rm -rf "$TMP"\' EXIT INT TERM',
      '    [ -d "$TMP/mnt/$APPDIR" ] || die "$APPDIR is not in the disk image"',
      '    STAGE="$DEST/.${APPDIR%.app}.rightkit-install.app"; rm -rf "$STAGE"',
      '    ditto "$TMP/mnt/$APPDIR" "$STAGE" || die "copy failed"',
      '    codesign --verify --deep --strict "$STAGE" || { rm -rf "$STAGE"; die "code signature invalid"; }',
      '    GOT_TEAM="$(codesign -dvv "$STAGE" 2>&1 | sed -n \'s/^TeamIdentifier=//p\')"',
      '    [ "$GOT_TEAM" = "$TEAM" ] || { rm -rf "$STAGE"; die "signed by team $GOT_TEAM, expected $TEAM"; }',
      '    GOT_ID="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$STAGE/Contents/Info.plist")"',
      '    [ "$GOT_ID" = "$BUNDLE" ] || { rm -rf "$STAGE"; die "bundle id $GOT_ID, expected $BUNDLE"; }',
      '    spctl -a -t exec "$STAGE" >/dev/null 2>&1 || { rm -rf "$STAGE"; die "Gatekeeper rejected the app"; }',
      '    osascript -e "tell application \\"$APP\\" to quit" >/dev/null 2>&1 || true',
      '    rm -rf "$DEST/$APPDIR.previous"; [ -d "$DEST/$APPDIR" ] && mv "$DEST/$APPDIR" "$DEST/$APPDIR.previous"',
      '    if mv "$STAGE" "$DEST/$APPDIR"; then rm -rf "$DEST/$APPDIR.previous"; else [ -d "$DEST/$APPDIR.previous" ] && mv "$DEST/$APPDIR.previous" "$DEST/$APPDIR"; die "install failed; the previous copy was restored"; fi',
      '    printf "%s %s installed in %s\\n" "$APP" "$VERSION" "$DEST"',
      "    ;;",
    );
  }
  if (linux) {
    lines.push(
      "  Linux)",
      `    BINARY='${linux.binary}'`,
      '    SHARE="${XDG_DATA_HOME:-$HOME/.local/share}/$SLUG"; BIN="$HOME/.local/bin"',
      '    if [ "$UNINSTALL" = 1 ]; then rm -rf "$SHARE" "$BIN/$BINARY"; printf "%s removed\\n" "$APP"; exit 0; fi',
      '    case "$ARCH" in',
      ...LINUX_ARCHES.filter((arch) => linux[arch]).map((arch) => `      ${arch}) URL='${linux[arch].url}'; SHA='${linux[arch].sha256}' ;;`),
      '      *) die "unsupported architecture $ARCH" ;;',
      "    esac",
      '    fetch "$URL" "$TMP/app.tar.gz"; verify_sha "$TMP/app.tar.gz" "$SHA"',
      '    rm -rf "$SHARE.new"; mkdir -p "$SHARE.new" "$BIN"',
      '    tar -xzf "$TMP/app.tar.gz" -C "$SHARE.new" || die "extract failed"',
      '    [ -x "$SHARE.new/$BINARY" ] || die "$BINARY is not in the archive"',
      '    rm -rf "$SHARE"; mv "$SHARE.new" "$SHARE"; ln -sfn "$SHARE/$BINARY" "$BIN/$BINARY"',
      '    printf "%s %s installed in %s (link %s/%s)\\n" "$APP" "$VERSION" "$SHARE" "$BIN" "$BINARY"',
      "    ;;",
    );
  }
  lines.push('  *) die "unsupported operating system $OS" ;;', "esac", "");
  if (!mac) lines.splice(lines.indexOf("case \"$OS\" in") + 1, 0, '  Darwin) die "no macOS build of $APP is published" ;;');
  return lines.join("\n");
}

export function renderInstallPs1(facts) {
  const w = facts.windows;
  if (!w) return null;
  return [
    `# ${header(facts, "this script")}`,
    "# Rerun the same command to upgrade. Uninstall: & ([scriptblock]::Create((irm <this url>))) -Uninstall",
    "param([switch]$Uninstall)",
    "$ErrorActionPreference = 'Stop'",
    `$App = '${facts.productName}'`,
    `$Version = '${facts.version}'`,
    `$Url = '${w.url}'`,
    `$Sha = '${w.sha256}'`,
    `$Signer = '${w.signerSubject}'`,
    `$Dest = [Environment]::ExpandEnvironmentVariables('${w.installDirectory}')`,
    `$Exe = '${w.exe}'`,
    "if ($Uninstall) {",
    "  $un = Join-Path $Dest 'uninstall.exe'",
    "  if (Test-Path -LiteralPath $un) { Start-Process -FilePath $un -ArgumentList '/S' -Wait; Write-Output \"$App removed\" } else { Write-Output \"$App is not installed at $Dest\" }",
    "  return",
    "}",
    "$tmp = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName() + '.exe')",
    "try {",
    "  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12",
    "  Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $tmp",
    "  if ((Get-FileHash -Algorithm SHA256 -LiteralPath $tmp).Hash.ToLowerInvariant() -ne $Sha) { throw \"sha256 mismatch: refusing to run the installer\" }",
    "  $sig = Get-AuthenticodeSignature -LiteralPath $tmp",
    "  if ($sig.Status -ne 'Valid') { throw \"Authenticode status is $($sig.Status), not Valid\" }",
    "  if ($sig.SignerCertificate.Subject -notlike \"$Signer*\") { throw \"signed by $($sig.SignerCertificate.Subject), expected $Signer\" }",
    "  Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($Dest, [StringComparison]::OrdinalIgnoreCase) } | Stop-Process -Force",
    "  $p = Start-Process -FilePath $tmp -ArgumentList @('/S', \"/D=$Dest\") -Wait -PassThru",
    "  if ($p.ExitCode -ne 0) { throw \"the installer exited $($p.ExitCode)\" }",
    "  if (-not (Test-Path -LiteralPath (Join-Path $Dest $Exe))) { throw \"$Exe is not in $Dest after the install\" }",
    "  Write-Output \"$App $Version installed in $Dest\"",
    "} finally { Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue }",
    "",
  ].join("\n");
}

export function renderCask(facts) {
  const m = facts.mac;
  if (!m) return null;
  return [
    `# ${header(facts, "this cask")}`,
    `cask "${facts.app}" do`,
    `  version "${facts.version}"`,
    `  sha256 "${m.sha256}"`,
    "",
    `  url "${m.url}"`,
    `  name "${facts.productName}"`,
    `  desc "${facts.description}"`,
    `  homepage "${facts.homepage}"`,
    "",
    `  app "${m.appName}"`,
    "end",
    "",
  ].join("\n");
}

export function renderWingetManifests(facts) {
  const w = facts.windows;
  if (!w) return null;
  const id = `${facts.publisherId ?? facts.publisher.replace(/[^A-Za-z0-9]/g, "")}.${facts.productName.replace(/[^A-Za-z0-9]/g, "")}`;
  const schema = "1.6.0";
  const head = (type) => `# yaml-language-server: $schema=https://aka.ms/winget-manifest.${type}.${schema}.schema.json\n`;
  return {
    id,
    files: {
      [`${id}.yaml`]: `${head("version")}PackageIdentifier: ${id}\nPackageVersion: ${facts.version}\nDefaultLocale: en-US\nManifestType: version\nManifestVersion: ${schema}\n`,
      [`${id}.installer.yaml`]: `${head("installer")}PackageIdentifier: ${id}\nPackageVersion: ${facts.version}\nInstallerType: nullsoft\nScope: user\nInstallModes:\n  - silent\n  - silentWithProgress\nInstallerSwitches:\n  Silent: /S\n  SilentWithProgress: /S\nInstallers:\n  - Architecture: x64\n    InstallerUrl: ${w.url}\n    InstallerSha256: ${w.sha256.toUpperCase()}\nManifestType: installer\nManifestVersion: ${schema}\n`,
      [`${id}.locale.en-US.yaml`]: `${head("defaultLocale")}PackageIdentifier: ${id}\nPackageVersion: ${facts.version}\nPackageLocale: en-US\nPublisher: ${JSON.stringify(facts.publisher)}\nPackageName: ${JSON.stringify(facts.productName)}\nShortDescription: ${JSON.stringify(facts.description)}\nPackageUrl: ${facts.homepage}\nLicense: ${JSON.stringify(facts.license ?? "Proprietary")}\nManifestType: defaultLocale\nManifestVersion: ${schema}\n`,
    },
  };
}

/** Render every channel the facts allow: { files: { "<relative path>": text } }. Pure: nothing is written. */
export function renderInstallChannels(rawFacts) {
  const facts = validateChannelFacts(rawFacts);
  const files = { "install.sh": renderInstallSh(facts) };
  const ps1 = renderInstallPs1(facts);
  if (ps1) files["install.ps1"] = ps1;
  const cask = renderCask(facts);
  if (cask) files[`Casks/${facts.app}.rb`] = cask;
  const winget = renderWingetManifests(facts);
  if (winget) for (const [name, text] of Object.entries(winget.files)) files[`winget/${name}`] = text;
  const sha = (text) => createHash("sha256").update(text).digest("hex");
  files["install-channels.json"] = `${JSON.stringify({ schema: 1, app: facts.app, version: facts.version, host: facts.host, files: Object.fromEntries(Object.entries(files).map(([name, text]) => [name, { sha256: sha(text), bytes: Buffer.byteLength(text) }])) }, null, 2)}\n`;
  return { files };
}

/** Write the rendered files under `out`; with `check`, write nothing and report the files whose bytes on disk differ. */
export function writeInstallChannels(rawFacts, out, { check = false, fs = { mkdirSync, readFileSync, renameSync, writeFileSync } } = {}) {
  const { files } = renderInstallChannels(rawFacts);
  const root = path.resolve(out);
  const differing = [];
  for (const [name, text] of Object.entries(files)) {
    const file = path.join(root, ...name.split("/"));
    let current = null;
    try { current = String(fs.readFileSync(file, "utf8")); } catch { current = null; }
    if (current !== text) differing.push(name);
    if (check || current === text) continue;
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const temporary = `${file}.tmp-${process.pid}`;
    fs.writeFileSync(temporary, text);
    fs.renameSync(temporary, file);
  }
  return { out: root, files: Object.keys(files), differing };
}

export function main(argv = process.argv.slice(2)) {
  let factsFile; let out; let check = false;
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--facts" && argv[i + 1]) { factsFile = argv[i + 1]; i += 1; }
    else if (argv[i] === "--out" && argv[i + 1]) { out = argv[i + 1]; i += 1; }
    else if (argv[i] === "--check") check = true;
    else { console.error(`right-release install-channels: unknown or incomplete argument ${argv[i]}\nusage: right-release install-channels --facts <facts.json> --out <dir> [--check]`); return 2; }
  }
  if (!factsFile || !out) { console.error("usage: right-release install-channels --facts <facts.json> --out <dir> [--check]"); return 2; }
  try {
    const result = writeInstallChannels(JSON.parse(readFileSync(path.resolve(factsFile), "utf8")), out, { check });
    if (check) {
      if (result.differing.length) { console.error(`right-release install-channels: ${result.differing.length} file(s) differ from the facts: ${result.differing.join(", ")}`); return 1; }
      console.log(`right-release install-channels: ${result.files.length} file(s) match`);
      return 0;
    }
    console.log(`right-release install-channels: wrote ${result.files.length} file(s) to ${result.out}${result.differing.length ? ` (${result.differing.length} changed)` : " (all unchanged)"}`);
    return 0;
  } catch (error) { console.error(`right-release install-channels: ${error.message}`); return error instanceof InstallChannelsError ? 1 : 2; }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main();
