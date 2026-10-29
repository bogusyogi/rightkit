// The managed-root guard (item 5, signed item5-sage-decisions-2.md D4 rule 1): the gate never creates the host's managed root, and on
// the Mac never accepts one on the boot volume. It lives here, not in snapshot.mjs, because build-ownership.mjs guards the root as the
// FIRST action of an acquisition and snapshot.mjs already imports build-ownership.mjs. snapshot.mjs re-exports both names.
import fs from "node:fs";

export class SnapshotError extends Error {
  /** `code` is "mismatch" (the snapshot is not the commit's tree), "busy" (another sync holds it), "refused" (the commit cannot be
   * materialised exactly, or the root is not one a snapshot may live in: a gitlink, an unsafe path, a link out of the snapshot, a
   * missing managed root, an open-mode root, an unknown revision) or "error" (could not run). `reason`, when present, names the
   * refusal: "gitlink", "unknown-revision", "managed-root-missing", "managed-root-on-boot-volume", "root-open-mode",
   * "root-not-owned", "root-parent-missing". */
  constructor(code, message, reason) { super(message); this.code = code; if (reason) this.reason = reason; }
}

const lstat = (p) => { try { return fs.lstatSync(p, { bigint: true }); } catch (e) { if (e.code === "ENOENT" || e.code === "ENOTDIR") return null; throw e; } };
const deviceOf = (p) => fs.statSync(p, { bigint: true }).dev;

/**
 * The managed root must already exist (the gate never creates it) and, on the Mac, live on another device than `/`: with the volume
 * unmounted, `/Volumes/D/.rightkit-managed` made on the boot volume (by anything) would otherwise hold the snapshots. Volume identity
 * and the disk floor for the broker's own writes are the broker's (BC B16). Throws SnapshotError("refused") with a named reason.
 */
export function requireManagedRoot(managedRoot, { platform = process.platform, device = deviceOf } = {}) {
  const st = lstat(managedRoot);
  if (!st) throw new SnapshotError("refused", `the managed root ${managedRoot} does not exist: the gate never creates it (the volume may be unmounted); nothing was written`, "managed-root-missing");
  if (!st.isDirectory()) throw new SnapshotError("refused", `the managed root ${managedRoot} is not a plain directory (a link is never followed)`, "managed-root-missing");
  if (platform === "darwin") {
    let here; let boot;
    try { here = device(managedRoot); boot = device("/"); }
    catch (e) { throw new SnapshotError("error", `cannot read the device of the managed root ${managedRoot} or of /: ${e.message}`); }
    if (here === boot) throw new SnapshotError("refused", `the managed root ${managedRoot} is on the same device as / (the boot volume): a snapshot there would not be on the managed volume (is the volume mounted?)`, "managed-root-on-boot-volume");
  }
  return managedRoot;
}
