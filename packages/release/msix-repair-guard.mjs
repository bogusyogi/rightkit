// Canonical MSIX repair guard (W08): the PowerShell script an app writes outside its package so a per-user scheduled task can
// re-add a package that an interrupted update left `Modified, NeedsRemediation`. This module returns the script text and its
// SHA-256 so consumers (SwiftRight's SwiftRightBridge embeds a copy) can pin the exact bytes.
//
// The script file is stored with LF line endings and ASCII only (see .gitattributes); the hash covers those bytes.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const MSIX_REPAIR_GUARD_FILE = "msix-repair-guard.ps1";
export const MSIX_REPAIR_GUARD_PATH = path.join(path.dirname(fileURLToPath(import.meta.url)), MSIX_REPAIR_GUARD_FILE);

export function msixRepairGuardSha256(text) {
  return createHash("sha256").update(Buffer.from(String(text).replace(/\r\n/g, "\n"), "utf8")).digest("hex");
}

/** The guard script, its SHA-256 (hex, over the LF-normalised bytes) and the guard protocol version it declares. */
export function msixRepairGuard() {
  const text = readFileSync(MSIX_REPAIR_GUARD_PATH, "utf8").replace(/\r\n/g, "\n");
  const match = /^\$GuardVersion = (\d+)$/m.exec(text);
  if (!match) throw new Error("msix-repair-guard.ps1 declares no $GuardVersion");
  return { file: MSIX_REPAIR_GUARD_FILE, text, sha256: msixRepairGuardSha256(text), guardVersion: Number(match[1]) };
}
