// A repository RightKit does not bind: a tool that may use RightKit packages but owes none of the app contract.
//
// The list is the shipped ownership-advise-only.json (each entry carries the owner's ruling, quoted, in `ratifiedBy`). For such a
// repository the app-contract rules report what they find as advice and refuse nothing:
//   - ownership findings (ownership.mjs adviseOnlyFindings, since 0.6.86);
//   - the RightKit Cargo dependency and profile contracts, QA adoption, and the release-switch inventory and scans (release.mjs).
// Rules that protect the release itself are NOT in this set and bind every repository: the protected signing workflow shape, secret
// scans, licence notices, signing, the hosted-workflow checks, build admission, and the test-mode feature closure.
import { readFileSync } from "node:fs";
import { spawnSync } from "./background-process.mjs";
import { canonicalRepositoryIdentity } from "./ownership-approval.mjs";
import { isSnapshotSource } from "./release-source-identity.mjs";

/** The `check()` rows of release.mjs that are app-contract rules. */
export const UNBOUND_ADVICE_CHECKS = Object.freeze(new Set(["rightkit-cargo-contract", "cargo-profile-contract", "qa-adoption"]));
/** The prebuild gates (prebuild-gates.mjs) that are the release-switch inventory rule (M21/M23 source layer). */
export const UNBOUND_ADVICE_GATES = Object.freeze(new Set(["switch-inventory", "switch-inventory-missing", "source-switch-roots", "source-switch-prepare", "release-switches"]));

let shipped;
function shippedList() {
  if (shipped === undefined) {
    try { const data = JSON.parse(readFileSync(new URL("./ownership-advise-only.json", import.meta.url), "utf8")); shipped = Array.isArray(data?.repositories) ? data.repositories : []; }
    catch { shipped = []; }
  }
  return shipped;
}

/** The entry of `repositoryIdentity` in the list (it needs a quoted ruling), or null. */
export function unboundEntry(repositoryIdentity, list = shippedList()) {
  if (!repositoryIdentity) return null;
  return list.find((entry) => entry?.repositoryIdentity === repositoryIdentity && typeof entry.ratifiedBy === "string" && entry.ratifiedBy.trim()) ?? null;
}

/**
 * The list entry of the repository a release runs for, or null. `source` is the door source split (its checkout holds the origin;
 * the snapshot has no git directory); without one the origin is read from `root`. An origin that cannot be read binds the repository.
 */
export function unboundRepository({ root, source = null, git = spawnSync, list } = {}) {
  const from = source && isSnapshotSource(source) ? source.checkout : root;
  if (!from) return null;
  const origin = git("git", ["-C", from, "remote", "get-url", "origin"], { encoding: "utf8", windowsHide: true, maxBuffer: 4 * 1024 * 1024 });
  if (origin?.status !== 0) return null;
  let identity;
  try { identity = canonicalRepositoryIdentity(String(origin.stdout ?? "").trim()); } catch { return null; }
  const entry = unboundEntry(identity, list);
  return entry ? { ...entry, repositoryIdentity: identity } : null;
}
