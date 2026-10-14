// Version-requirement arithmetic for apply: split a pin into prefix and numeric
// parts, build the new requirement with the same precision, and detect breaking bumps.

import { parseLatest } from "./compare.js";

export interface Requirement {
  raw: string;
  /** Operator or marker before the number: "", "^", "~", ">=", "=", "v". */
  prefix: string;
  /** Numeric components the pin names (1 to 3). */
  parts: number[];
}

const REQ_RE = /^(\^|~|>=|=|v)?(\d+)(?:\.(\d+))?(?:\.(\d+))?$/;

/**
 * Parse a requirement that apply can rewrite. Returns null for wildcards,
 * pre-release or build suffixes, ranges and anything else that is not a plain version.
 */
export function parseRequirement(raw: string): Requirement | null {
  const match = REQ_RE.exec(raw.trim());
  if (!match) return null;
  const parts = [match[2], match[3], match[4]].filter((p): p is string => p !== undefined).map(Number);
  return { raw, prefix: match[1] ?? "", parts };
}

/** True when moving from `pin` to `latest` is not safe to apply without a migration. */
export function isBreaking(pin: Requirement, latest: [number, number, number]): boolean {
  const [pMajor, pMinor = 0, pPatch = 0] = pin.parts;
  if (latest[0] > pMajor) return true;
  if (pMajor === 0) {
    // 0.x: a minor bump is breaking.
    if (pin.parts.length >= 2 && latest[1] > pMinor) return true;
    // 0.0.x: caret requirements match exactly one patch, so a patch bump is breaking too.
    if (pin.parts.length >= 3 && pMinor === 0 && latest[1] === 0 && latest[2] > pPatch) return true;
  }
  return false;
}

/** New requirement with the same prefix and the same number of components as the old pin. */
export function bumpedRequirement(pin: Requirement, latest: [number, number, number]): string {
  return pin.prefix + latest.slice(0, pin.parts.length).join(".");
}

/** Full `x.y.z` for lockfile and package-manager commands. */
export function fullVersion(latest: [number, number, number]): string {
  return latest.join(".");
}

export interface BumpDecision {
  action: "apply" | "needs-migration" | "skip";
  reason?: string;
  newRequirement?: string;
  fullVersion?: string;
}

/** Decide what apply does with one outdated dependency. */
export function decideBump(pinned: string, latest: string): BumpDecision {
  const req = parseRequirement(pinned);
  if (!req) return { action: "skip", reason: `requirement ${pinned} has a wildcard, range or suffix that apply does not rewrite` };
  const parts = parseLatest(latest);
  if (!parts) return { action: "skip", reason: `latest ${latest} is not a plain numeric version` };
  if (/[-+]/.test(latest.replace(/^v/, ""))) return { action: "skip", reason: `latest ${latest} is a pre-release or build version` };
  if (isBreaking(req, parts)) {
    return { action: "needs-migration", reason: `breaking bump ${pinned} -> ${fullVersion(parts)} (${req.parts[0] < parts[0] ? "major" : "0.x"})` };
  }
  return { action: "apply", newRequirement: bumpedRequirement(req, parts), fullVersion: fullVersion(parts) };
}
