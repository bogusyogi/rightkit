import nodeFs from "node:fs";
import path from "node:path";

/**
 * The one install rule for every app, helper and dev or QA binary (Adrian, 2026-10-17: "No new id, no new folder"):
 * macOS and Windows tie privacy grants to the path and the code identity, so an install or update replaces the
 * copy at ONE fixed path. Release installs may keep a persistent `<name>.previous` beside it (rename it back).
 * Dev and QA installs NEVER keep an old copy (Orthic-Labs/rightkit#348: "when installing dev, old copy must never be
 * backedup"): they pass `previous: null` to `swapInPlace`, or use `replaceFileInPlace`, which has no backup at all.
 * Nothing ever runs from `.previous` or from the staging copy.
 */
export const PREVIOUS_SUFFIX = ".previous";
export const STAGING_SUFFIX = ".staging";

export const previousPathOf = (destination) => `${destination}${PREVIOUS_SUFFIX}`;
/** Fixed (not per-pid) staging sibling. Nothing runs from it: it is fully written and verified, then renamed. */
export const stagingPathOf = (destination) => `${destination}${STAGING_SUFFIX}`;

const exists = (fs, file) => {
  if (typeof fs.lstatSync !== "function") return fs.existsSync(file);
  try { fs.lstatSync(file); return true; } catch (error) { if (error?.code === "ENOENT") return false; throw error; }
};

/**
 * Swap `staging` into `destination`, then `verify(destination)` runs.
 *
 * With a `previous` path (release): the old destination becomes `previous` (replacing any older `.previous`). Any
 * failure removes the new copy and renames `previous` back, so the fixed path is never left empty. Renames only.
 * @returns {{destination: string, previous: string|null}} previous is null when nothing was installed before.
 *
 * With `previous: null` (dev and QA, NO backup of any kind): the old destination is removed and `staging` is renamed
 * over the fixed path; a stale `<destination>.previous` left by an older RightKit is removed too. Any failure removes
 * the new (absent or partial) copy and the staging copy and rethrows; the old copy is NOT restored, the user reinstalls
 * from an older build. `previous` is always null in the result.
 */
export function swapInPlace({ staging, destination, previous = previousPathOf(destination), verify = () => {}, fs = nodeFs }) {
  if (previous === null) return replaceWithoutBackup({ staging, destination, verify, fs });
  if (previous === destination || staging === destination || staging === previous) throw new Error("swapInPlace needs three distinct paths");
  let movedAside = false;
  try {
    if (exists(fs, destination)) {
      if (exists(fs, previous)) fs.rmSync(previous, { recursive: true, force: true });
      fs.renameSync(destination, previous);
      movedAside = true;
    }
    fs.renameSync(staging, destination);
    verify(destination);
  } catch (error) {
    try { fs.rmSync(destination, { recursive: true, force: true }); } catch { /* keep the original error */ }
    if (movedAside && exists(fs, previous)) fs.renameSync(previous, destination);
    try { fs.rmSync(staging, { recursive: true, force: true }); } catch { /* best effort */ }
    throw error;
  }
  return { destination, previous: movedAside ? previous : null };
}

function replaceWithoutBackup({ staging, destination, verify, fs }) {
  if (staging === destination) throw new Error("swapInPlace needs distinct staging and destination paths");
  try {
    if (exists(fs, destination)) fs.rmSync(destination, { recursive: true, force: true });
    fs.renameSync(staging, destination);
    verify(destination);
  } catch (error) {
    try { fs.rmSync(destination, { recursive: true, force: true }); } catch { /* keep the original error */ }
    try { fs.rmSync(staging, { recursive: true, force: true }); } catch { /* best effort */ }
    throw error;
  }
  // No old copy may survive a dev install, including one an older RightKit left behind.
  try { fs.rmSync(previousPathOf(destination), { recursive: true, force: true }); } catch { /* best effort */ }
  return { destination, previous: null };
}

/** Copy one dev or QA file to its fixed path in place: temp sibling, then rename. NO backup of the old file. Returns the path. */
export function replaceFileInPlace(from, destination, { fs = nodeFs } = {}) {
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  const staging = stagingPathOf(destination);
  fs.rmSync(staging, { force: true });
  fs.copyFileSync(from, staging);
  swapInPlace({ staging, destination, previous: null, fs });
  return destination;
}
