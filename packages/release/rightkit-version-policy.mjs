// Which @rightkit/* pins an app may carry, from the versions file (rightkit-versions.json).
import { compareVersions } from "./rightkit-sync.mjs";

const EXACT_SEMVER = /^\d+\.\d+\.\d+$/;

/**
 * A @rightkit/* pin is allowed when it is a recorded version, or an exact version between the lowest and the
 * highest version the versions file records for that package. A republished or skipped patch inside that
 * range needs no new release of this tool; a version above everything recorded is still refused (a tool
 * cannot know what was published after its own versions file was written).
 */
export function rightKitVersionAllowed(allowed, specifier) {
  if (allowed.has(specifier)) return true;
  if (typeof specifier !== "string" || !EXACT_SEMVER.test(specifier)) return false;
  const known = [...allowed].filter((v) => typeof v === "string" && EXACT_SEMVER.test(v)).sort(compareVersions);
  if (!known.length) return false;
  return compareVersions(specifier, known[0]) >= 0 && compareVersions(specifier, known.at(-1)) <= 0;
}
