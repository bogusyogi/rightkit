import nodeFs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { previousPathOf } from "./in-place-install.mjs";

/**
 * `right-release install-app --installer <Setup.exe> --dest <install folder> --exe <App.exe>` on Windows (Orthic-Labs/rightkit#258).
 *
 * The same rule as the macOS install: the app is installed and updated IN PLACE at one fixed folder (`--dest`, the
 * folder the NSIS installer already owns, for example `%LOCALAPPDATA%\Programs\Pulse`). Rollback is one
 * `<dest>.previous` beside it (a copy of the folder as it was; replaced on each install; nothing runs from it), never a
 * versioned, digest or per-run folder. The silent installer is pinned to the fixed folder with `/D=` so it cannot choose another one.
 *
 * Why a copy and not a rename: a running app holds its folder open on Windows, so a rename fails; the installer closes
 * the app and updates the same path in place instead. A failed install copies `<dest>.previous` back over `<dest>`.
 */
export class InstallAppWindowsError extends Error {
  constructor(code, message) { super(message); this.name = "InstallAppWindowsError"; this.code = code; }
}

export function windowsInstallArguments(dest) {
  // NSIS: /D must be the last argument, unquoted, and names the install folder.
  return ["/S", `/D=${dest}`];
}

const sha256 = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

export function installAppWindows(opts, { platform = process.platform, fs = nodeFs, run = spawnSync, event = () => {} } = {}) {
  if (platform !== "win32") throw new InstallAppWindowsError("unsupported", `the Windows installer mode runs on win32, not ${platform}`);
  const { installer, dest, exe } = opts;
  if (!installer || !path.win32.isAbsolute(installer)) throw new InstallAppWindowsError("usage", "--installer must be an absolute path to the NSIS setup executable");
  if (!dest || !path.win32.isAbsolute(dest) || path.win32.parse(dest).root === dest) throw new InstallAppWindowsError("usage", "--dest must be the fixed absolute install folder, not a drive root");
  if (!exe || exe !== path.win32.basename(exe)) throw new InstallAppWindowsError("usage", "--exe must be the bare executable name inside --dest");
  if (!fs.existsSync(installer)) throw new InstallAppWindowsError("installer-missing", `installer not found: ${installer}`);
  const previous = previousPathOf(dest);
  const installed = path.win32.join(dest, exe);
  const hadInstall = fs.existsSync(dest);
  event("start", { installer, dest, replacing: hadInstall });
  if (hadInstall) {
    // One rollback copy, replaced on every install.
    fs.rmSync(previous, { recursive: true, force: true });
    fs.cpSync(dest, previous, { recursive: true });
    event("previous-kept", { previous });
  }
  const restore = () => {
    if (!hadInstall || !fs.existsSync(previous)) return false;
    fs.cpSync(previous, dest, { recursive: true, force: true });
    return true;
  };
  const result = run(installer, windowsInstallArguments(dest), { stdio: "inherit", windowsHide: true, shell: false });
  if (result.status !== 0) {
    const restored = restore();
    throw new InstallAppWindowsError("installer-failed", `installer exited ${result.status ?? "abnormally"}${result.error ? `: ${result.error.message}` : ""}; ${restored ? `restored ${dest} from ${previous}` : "nothing was installed before"}`);
  }
  // The installer must have updated the fixed folder: the executable is at the same path as before, not in a new folder.
  if (!fs.existsSync(installed) || fs.statSync(installed).size === 0) {
    const restored = restore();
    throw new InstallAppWindowsError("path-changed", `the installer did not place ${exe} at the fixed path ${installed}; ${restored ? `restored from ${previous}` : "nothing was installed before"}`);
  }
  event("replaced", { installed, previous: hadInstall ? previous : null, sha256: sha256(fs, installed) });
  return { ok: true, installed, previous: hadInstall ? previous : null };
}
