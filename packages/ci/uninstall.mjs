// Remove every right-git-managed file from a repo, leaving zero managed
// files (invariant I5). No visibility check — uninstall must always be
// possible, including from a repo that flipped private after install.
//
// F6 fix: `.rightgit.json` is itself a file `install` writes, so it counts
// as a managed file. The old implementation removed only marker-bearing
// workflow files and never the manifest, so it leaked `.rightgit.json` —
// including after an install that failed partway (install writes the
// manifest *before* running sync, so a sync failure still leaves the
// manifest on disk with zero workflow files to key off of). Uninstall must
// remove the manifest unconditionally on presence, independent of whether
// any workflow files exist.

import fs from "node:fs";
import path from "node:path";
import { listManagedFiles } from "./sync.mjs";
import { MANIFEST_FILENAME } from "./manifest.mjs";

/** Returns { ok, removed: string[] }. Never throws. */
export function uninstallCommand(repoPath) {
  const dir = path.join(repoPath, ".github", "workflows");
  const managed = listManagedFiles(repoPath);
  const removed = [];
  for (const filename of managed) {
    fs.rmSync(path.join(dir, filename), { force: true });
    removed.push(filename);
  }

  const manifestPath = path.join(repoPath, MANIFEST_FILENAME);
  if (fs.existsSync(manifestPath)) {
    fs.rmSync(manifestPath, { force: true });
    removed.push(MANIFEST_FILENAME);
  }

  return { ok: true, removed };
}
