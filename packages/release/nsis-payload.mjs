// Prove the installer actually ships the bytes we signed.
//
// Signing the raw EXE and signing the installer are two separate signatures.
// Between them Tauri copies the EXE into the NSIS payload, and a single byte
// changed anywhere in that hand-off invalidates the inner Authenticode signature
// while leaving the outer installer signature perfectly valid. Every check that
// stops at "the installer is signed" therefore passes on a broken build — which
// is how invalid binaries shipped.
//
// So we open the finished installer, pull the embedded executable back out, and
// require it to be byte-identical to the EXE we signed. No inference, no proxy.
// 7-Zip reads the NSIS container format; if it is absent we fail closed rather
// than record an unverified pass.

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const SEVEN_ZIP_CANDIDATES = [
  "C:\\Program Files\\7-Zip\\7z.exe",
  "C:\\Program Files (x86)\\7-Zip\\7z.exe",
  "/opt/homebrew/bin/7z",
  "/usr/local/bin/7z",
  "/usr/bin/7z",
];

export function resolveSevenZip({ env = process.env, exists = existsSync, which = commandPath } = {}) {
  const explicit = env.RIGHT_RELEASE_SEVENZIP;
  if (explicit) {
    if (!exists(explicit)) throw new Error(`RIGHT_RELEASE_SEVENZIP does not exist: ${explicit}`);
    return explicit;
  }
  for (const candidate of SEVEN_ZIP_CANDIDATES) {
    if (exists(candidate)) return candidate;
  }
  for (const name of ["7z", "7za", "7zz"]) {
    const found = which(name);
    if (found) return found;
  }
  throw new Error(
    "7-Zip is required to verify the NSIS installer payload but was not found; install 7-Zip (https://www.7-zip.org) or set RIGHT_RELEASE_SEVENZIP to its 7z executable",
  );
}

/**
 * Names of the files NSIS will install, parsed from 7-Zip's technical listing
 * (`-slt`). The column-aligned default listing leaves the date blank for entries
 * NSIS synthesises, which makes positional parsing quietly wrong; the key/value
 * form has no such ambiguity. Directory entries are dropped.
 *
 * The first record describes the archive itself and is separated from the member
 * records by a `----------` rule, so parsing starts after that rule.
 */
export function parseSevenZipListing(stdout) {
  const lines = String(stdout).replace(/\r\n?/g, "\n").split("\n");
  const start = lines.findIndex((line) => line.trim() === "----------");
  if (start === -1) return [];
  const entries = [];
  let current = null;
  for (const line of lines.slice(start + 1)) {
    const match = line.match(/^([A-Za-z][A-Za-z0-9 ]*?)\s=\s?(.*)$/);
    if (!match) continue;
    const [, key, value] = match;
    if (key === "Path") {
      if (current) entries.push(current);
      current = { name: value.replace(/\\/g, "/"), sizeBytes: null, folder: false };
    } else if (!current) continue;
    else if (key === "Size") current.sizeBytes = value.trim() === "" ? null : Number(value.trim());
    else if (key === "Folder") current.folder = value.trim() === "+";
    else if (key === "Attributes") current.folder = current.folder || value.includes("D");
  }
  if (current) entries.push(current);
  return entries.filter((entry) => !entry.folder).map(({ name, sizeBytes }) => ({ name, sizeBytes }));
}

export function listNsisPayload(installer, { sevenZip = resolveSevenZip(), run = runSevenZip } = {}) {
  const result = run(sevenZip, ["l", "-tnsis", "-slt", "--", path.resolve(installer)]);
  if (result.status !== 0) {
    throw new Error(`could not read the NSIS payload of ${installer}: ${(result.stderr || result.stdout || "").trim()}`);
  }
  return parseSevenZipListing(result.stdout);
}

/**
 * Extract one payload entry to a temporary directory and return its bytes.
 * The caller owns nothing: the temporary directory is always removed.
 */
export function extractNsisEntry(installer, entryName, { sevenZip = resolveSevenZip(), run = runSevenZip, tempRoot = os.tmpdir() } = {}) {
  const resolvedInstaller = path.resolve(installer);
  const base = path.posix.basename(String(entryName).replace(/\\/g, "/"));
  if (!base || base === "." || base === "..") throw new Error(`invalid NSIS payload entry name: ${entryName}`);
  const destination = path.join(tempRoot, `right-nsis-${process.pid}-${Math.random().toString(16).slice(2)}`);
  mkdirSync(destination, { recursive: true });
  try {
    // NSIS entries may live below `$PLUGINSDIR/release`. A basename-only 7-Zip
    // filter then reports `No files to process`, so resolve the actual archive
    // member first & extract that exact path.
    const requested = String(entryName).replace(/\\/g, "/");
    const members = listNsisPayload(resolvedInstaller, { sevenZip, run });
    const exact = members.filter((member) => member.name.toLowerCase() === requested.toLowerCase());
    const matches = exact.length > 0
      ? exact
      : members.filter((member) => path.posix.basename(member.name).toLowerCase() === base.toLowerCase());
    if (matches.length === 0) {
      throw new Error(`could not extract ${base} from ${resolvedInstaller}: no such payload entry`);
    }
    if (matches.length !== 1) {
      throw new Error(`could not uniquely identify ${base} in ${resolvedInstaller}: found ${matches.length} payload entries`);
    }
    const result = run(sevenZip, ["e", "-tnsis", "-y", `-o${destination}`, "--", resolvedInstaller, matches[0].name]);
    const extracted = path.join(destination, path.posix.basename(matches[0].name));
    if (result.status !== 0 || !existsSync(extracted)) {
      throw new Error(
        `could not extract ${base} from ${resolvedInstaller}: ${(result.stderr || result.stdout || "no such entry").trim()}`,
      );
    }
    return { name: base, bytes: readFileSync(extracted) };
  } finally {
    rmSync(destination, { recursive: true, force: true });
  }
}

/**
 * The load-bearing check: the executable inside the installer must be the exact
 * executable we signed. Returns a receipt; throws with the byte-level reason if
 * the bytes drifted.
 */
export function verifyNsisEmbeddedBinary({
  installer,
  entryName,
  expectedSha256,
  sevenZip = resolveSevenZip(),
  run = runSevenZip,
  tempRoot = os.tmpdir(),
}) {
  if (!/^[0-9a-f]{64}$/i.test(String(expectedSha256 ?? ""))) {
    throw new Error(`expected a sha256 digest for the signed ${entryName}, received ${JSON.stringify(expectedSha256)}`);
  }
  const { name, bytes } = extractNsisEntry(installer, entryName, { sevenZip, run, tempRoot });
  const embeddedSha256 = createHash("sha256").update(bytes).digest("hex");
  if (embeddedSha256.toLowerCase() !== String(expectedSha256).toLowerCase()) {
    throw new Error(
      `the installer does not embed the signed ${name}: signed sha256 ${expectedSha256}, embedded sha256 ${embeddedSha256}. `
      + "Something rewrote the executable after it was signed, so its Authenticode signature is invalid inside the installer.",
    );
  }
  return {
    installer: path.resolve(installer),
    entry: name,
    sha256: embeddedSha256,
    sizeBytes: bytes.length,
    installerSha256: createHash("sha256").update(readFileSync(path.resolve(installer))).digest("hex"),
  };
}

function runSevenZip(command, args) {
  return spawnSync(command, args, { encoding: "utf8", windowsHide: true, maxBuffer: 32 * 1024 * 1024 });
}

function commandPath(name) {
  const probe = process.platform === "win32" ? "where" : "which";
  const result = spawnSync(probe, [name], { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) return null;
  const first = String(result.stdout).split(/\r?\n/).map((line) => line.trim()).find(Boolean);
  return first && existsSync(first) ? first : null;
}
