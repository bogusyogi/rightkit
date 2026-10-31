// GB-07 (BROKER-CONTRACT section 16): where a unit publication's control plane may run. One definition for the gate's release (unit-gate.mjs) and the coordinator's in-gate entry
// (scripts/ci/rk-pipeline.mjs runPipeline), so neither can drift from the broker's own rule.
import path from "node:path";

/**
 * Why the control-plane checkout (the primary checkout, where the coordinator's uncarried root requests are admitted) is not apart from the managed snapshots, or null. The broker refuses a root
 * request whose working directory is inside a managed snapshot or a path a live snapshot lease holds (RK_CHECK_ERROR: "a root request cannot run inside a managed snapshot"), so the control plane may
 * not be inside the managed snapshots root; with a `root` (the source: the snapshot where the carried build and pack steps stay bound) it may not be inside the source either, and a source that is
 * itself a snapshot may not be its own control plane. A direct, unsnapshotted source keeps one root for both. `pathApi` is the platform's path module; comparison folds case on Windows.
 */
export function controlPlaneProblem({ root = null, checkout, snapshotsRoot = null, pathApi = path }) {
  const fold = (p) => { const n = pathApi.normalize(String(p)); return pathApi === path.win32 ? n.toLowerCase() : n; };
  const inside = (child, parent) => { const rel = pathApi.relative(fold(parent), fold(child)); return rel === "" || (!rel.startsWith("..") && !pathApi.isAbsolute(rel)); };
  if (typeof checkout !== "string" || !pathApi.isAbsolute(checkout)) return `the control-plane checkout ${JSON.stringify(checkout)} must be an absolute path`;
  if (snapshotsRoot && inside(checkout, snapshotsRoot)) return `the control-plane checkout ${checkout} is inside the managed snapshots root ${snapshotsRoot}: the broker refuses an uncarried root request there (RK_CHECK_ERROR: a root request cannot run inside a managed snapshot); the coordinator runs from the primary checkout, apart from the source snapshot`;
  if (root !== null && fold(checkout) !== fold(root) && inside(checkout, root)) return `the control-plane checkout ${checkout} is inside the source root ${root}: the source (the snapshot) and the control plane (the primary checkout) are separate roots`;
  return null;
}
