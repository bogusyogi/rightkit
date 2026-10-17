import nodeFs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { previousPathOf } from "./in-place-install.mjs";

/**
 * `right-release install-app --installer <Setup.exe> --dest <install folder> --exe <App.exe> [--payload <file>]... [--payload-manifest <json>]` on Windows (Orthic-Labs/rightkit#258).
 *
 * The same rule as the macOS install: the app is installed and updated IN PLACE at one fixed folder (`--dest`, the
 * folder the NSIS installer already owns, for example `%LOCALAPPDATA%\Programs\Pulse`). The silent installer is pinned to the
 * fixed folder with `/D=` so it cannot choose another one.
 *
 * DATA SAFETY (Orthic-Labs/rightkit#348): the install folder can also hold USER DATA (CodeRight keeps coderight.db, its WAL,
 * state and models in `%LOCALAPPDATA%\CodeRight`). So this module NEVER copies, restores, removes or overwrites the whole
 * install folder. Rollback is PAYLOAD-ONLY, from an explicit allowlist of the files the installer itself writes:
 *   - the allowlist is `--payload <relative file>` (repeatable) and/or `--payload-manifest <json file>` (`{ "files": [...] }` or a bare array)
 *     plus the `--exe` file; entries are relative regular files inside `--dest`, no directories, wildcards, `..` or drive paths;
 *   - with NO declared payload the allowlist is just the `--exe` file (always backed up and restored; nothing else is);
 *   - no path segment from `--dest` down to an allowlisted file may be a symlink or junction (reparse point); one is refused before backup and before restore or removal;
 *   - the backup is `<dest>.previous`, holding only the allowlisted files that existed before (replaced on each install; nothing runs from it);
 *   - a failed install restores only allowlisted files that existed before and removes only allowlisted files the failed run newly wrote;
 *     it never touches any other file in `--dest`.
 * Dev installs do not pass through this module: they keep no backup at all (see development.mjs, install-app.mjs --dev).
 */
export class InstallAppWindowsError extends Error {
  constructor(code, message) { super(message); this.name = "InstallAppWindowsError"; this.code = code; }
}

export function windowsInstallArguments(dest) {
  // NSIS: /D must be the last argument, unquoted, and names the install folder.
  return ["/S", `/D=${dest}`];
}

const sha256 = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

const SEGMENT_BAD = /[<>:"|?*\0]/;

/** One allowlist entry as a normalized `a/b/c` relative file path, or a usage error. */
function normalizePayloadEntry(entry) {
  if (typeof entry !== "string" || !entry.trim()) throw new InstallAppWindowsError("usage", "a payload entry must be a non-empty relative file path");
  if (path.win32.isAbsolute(entry) || path.posix.isAbsolute(entry) || /^[A-Za-z]:/.test(entry)) throw new InstallAppWindowsError("usage", `payload entry ${JSON.stringify(entry)} must be relative to --dest, not absolute`);
  const segments = entry.split(/[\\/]/);
  if (segments.some((segment) => segment === "" || segment === "." || segment === ".." || SEGMENT_BAD.test(segment))) {
    throw new InstallAppWindowsError("usage", `payload entry ${JSON.stringify(entry)} must be a plain relative file path (no directories, wildcards, "..", or empty segments)`);
  }
  return segments.join("/");
}

/** The payload allowlist: `--exe`, `--payload` entries and a `--payload-manifest` file, normalized and de-duplicated. Empty when nothing is declared. */
export function resolvePayloadAllowlist({ exe, payload = [], payloadManifest = null }, { fs = nodeFs } = {}) {
  const declared = [...(Array.isArray(payload) ? payload : [payload])];
  if (payloadManifest) {
    let parsed;
    try { parsed = JSON.parse(String(fs.readFileSync(payloadManifest, "utf8"))); }
    catch (error) { throw new InstallAppWindowsError("usage", `--payload-manifest ${payloadManifest} cannot be read as JSON: ${error.message}`); }
    const files = Array.isArray(parsed) ? parsed : parsed?.files;
    if (!Array.isArray(files)) throw new InstallAppWindowsError("usage", `--payload-manifest ${payloadManifest} must be a JSON array of relative files, or { "files": [...] }`);
    declared.push(...files);
  }
  // `--exe` alone is the minimal allowlist: the executable is always backed up and restored; data files never are.
  const normalized = [...new Set([exe, ...declared].map(normalizePayloadEntry))];
  const lower = new Set();
  for (const entry of normalized) {
    if (lower.has(entry.toLowerCase())) throw new InstallAppWindowsError("usage", `payload entry ${JSON.stringify(entry)} is listed twice (Windows paths are case-insensitive)`);
    lower.add(entry.toLowerCase());
  }
  return normalized.sort();
}

export function installAppWindows(opts, { platform = process.platform, fs = nodeFs, run = spawnSync, event = () => {} } = {}) {
  if (platform !== "win32") throw new InstallAppWindowsError("unsupported", `the Windows installer mode runs on win32, not ${platform}`);
  const { installer, dest, exe } = opts;
  if (!installer || !path.win32.isAbsolute(installer)) throw new InstallAppWindowsError("usage", "--installer must be an absolute path to the NSIS setup executable");
  if (!dest || !path.win32.isAbsolute(dest) || path.win32.parse(dest).root === dest) throw new InstallAppWindowsError("usage", "--dest must be the fixed absolute install folder, not a drive root");
  if (!exe || exe !== path.win32.basename(exe)) throw new InstallAppWindowsError("usage", "--exe must be the bare executable name inside --dest");
  if (!fs.existsSync(installer)) throw new InstallAppWindowsError("installer-missing", `installer not found: ${installer}`);
  const allowlist = resolvePayloadAllowlist({ exe, payload: opts.payload, payloadManifest: opts.payloadManifest }, { fs });
  const previous = previousPathOf(dest);
  const installed = path.win32.join(dest, exe);
  const hadInstall = fs.existsSync(dest);
  const inDest = (relative) => path.win32.join(dest, ...relative.split("/"));
  const inPrevious = (relative) => path.win32.join(previous, ...relative.split("/"));
  const isFile = (file) => { try { return (fs.lstatSync ?? fs.statSync).call(fs, file).isFile(); } catch { return false; } };
  // lstat every segment from dest down to the file; a symlink or junction anywhere in the chain is refused. A missing segment ends the walk.
  const assertNoLinks = (relative) => {
    const lstat = fs.lstatSync ?? fs.statSync;
    let current = dest;
    for (const segment of [null, ...relative.split("/")]) {
      if (segment !== null) current = path.win32.join(current, segment);
      let stat;
      try { stat = lstat.call(fs, current); } catch { return; }
      if (stat.isSymbolicLink?.() || stat.isReparsePoint?.()) throw new InstallAppWindowsError("usage", `${current} is a symlink or junction; refusing to back up, restore or remove payload entry ${relative} through it`);
    }
  };
  event("start", { installer, dest, replacing: hadInstall, payloadFiles: allowlist.length });

  // Backup: ONLY the allowlisted files that exist now, into `<dest>.previous`. The rest of --dest (user data) is never copied.
  const backedUp = [];
  const absentBefore = [];
  let backupMade = false;
  if (hadInstall && allowlist.length > 0) {
    for (const relative of allowlist) {
      const file = inDest(relative);
      assertNoLinks(relative);
      if (!fs.existsSync(file)) { absentBefore.push(relative); continue; }
      if (!isFile(file)) throw new InstallAppWindowsError("usage", `payload entry ${relative} is not a regular file in ${dest}; refusing to back it up or roll it back`);
      backedUp.push(relative);
    }
    // One payload-only rollback copy, replaced on every install (this also clears a whole-folder copy an older RightKit left).
    fs.rmSync(previous, { recursive: true, force: true });
    for (const relative of backedUp) {
      fs.mkdirSync(path.win32.dirname(inPrevious(relative)), { recursive: true });
      fs.copyFileSync(inDest(relative), inPrevious(relative));
    }
    backupMade = true;
    event("previous-kept", { previous, files: backedUp.length, payloadOnly: true });
  }
  // Roll back the payload and nothing else: restore allowlisted files that existed, remove allowlisted files this run newly wrote.
  // Each file is restored independently; failures are collected and reported together, never masking the installer error.
  const restoreFailures = [];
  const restore = () => {
    if (!backupMade) return false;
    for (const relative of backedUp) {
      try {
        assertNoLinks(relative);
        fs.mkdirSync(path.win32.dirname(inDest(relative)), { recursive: true });
        fs.copyFileSync(inPrevious(relative), inDest(relative));
      } catch (error) { restoreFailures.push(`${relative}: ${error.message}`); }
    }
    for (const relative of absentBefore) {
      try {
        assertNoLinks(relative);
        const file = inDest(relative);
        if (fs.existsSync(file) && isFile(file)) fs.rmSync(file, { force: true });
      } catch (error) { restoreFailures.push(`${relative}: ${error.message}`); }
    }
    return true;
  };
  const failureNote = () => (restoreFailures.length ? `; ROLLBACK INCOMPLETE, ${restoreFailures.length} file(s) failed: ${restoreFailures.join(" | ")}` : "");
  const noRollback = "nothing was installed before, so nothing was rolled back and no other file in the install folder was touched";
  const result = run(installer, windowsInstallArguments(dest), { stdio: "inherit", windowsHide: true, shell: false });
  if (result.status !== 0) {
    const restored = restore();
    throw new InstallAppWindowsError("installer-failed", `installer exited ${result.status ?? "abnormally"}${result.error ? `: ${result.error.message}` : ""}; ${restored ? `restored the ${backedUp.length} payload file(s) of ${dest} from ${previous}; no other file was touched` : noRollback}${failureNote()}`);
  }
  // The installer must have updated the fixed folder: the executable is at the same path as before, not in a new folder.
  if (!fs.existsSync(installed) || fs.statSync(installed).size === 0) {
    const restored = restore();
    throw new InstallAppWindowsError("path-changed", `the installer did not place ${exe} at the fixed path ${installed}; ${restored ? `restored the ${backedUp.length} payload file(s) from ${previous}; no other file was touched` : noRollback}${failureNote()}`);
  }
  event("replaced", { installed, previous: backupMade ? previous : null, sha256: sha256(fs, installed) });
  return { ok: true, installed, previous: backupMade ? previous : null };
}
