// Pinned-vs-latest comparison and policy resolution.
// Precision-aware: a pin with fewer components is compared only on the
// components it names ("1.2" is current against latest 1.2.5, outdated against 1.3.0).

import { type CheckStatus, type Policy, type PolicyConfig } from "./types.js";

export interface Comparison {
  status: CheckStatus;
  reason?: string;
}

const PINNED_RE = /^\s*(?:\^|~|>=|=|v)?\s*(\d+)(?:\.(\d+|[xX*]))?(?:\.(\d+|[xX*]))?(?:[-+][0-9A-Za-z.+-]*)?\s*$/;
const LATEST_RE = /^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:[-+].*)?$/;

/** Numeric parts a pinned spec names, or null when the spec is not a single version. */
export function parsePinned(spec: string): number[] | null {
  const match = PINNED_RE.exec(spec);
  if (!match) return null;
  const parts: number[] = [];
  for (const raw of [match[1], match[2], match[3]]) {
    if (raw === undefined) break;
    if (/^[xX*]$/.test(raw)) break;
    parts.push(Number(raw));
  }
  return parts;
}

/** Latest version as three numeric parts, or null when it is not numeric dotted. */
export function parseLatest(version: string): [number, number, number] | null {
  const match = LATEST_RE.exec(version.trim());
  if (!match) return null;
  return [Number(match[1]), Number(match[2] ?? 0), Number(match[3] ?? 0)];
}

/** Compare a manifest pin with the upstream latest. Never throws. */
export function compareVersions(pinned: string, latest: string): Comparison {
  const pinnedParts = parsePinned(pinned);
  const latestParts = parseLatest(latest);
  if (pinnedParts && latestParts && pinnedParts.length > 0) {
    for (let i = 0; i < pinnedParts.length; i++) {
      if (pinnedParts[i] < latestParts[i]) return { status: "outdated" };
      if (pinnedParts[i] > latestParts[i]) return { status: "current", reason: "pin is ahead of upstream latest" };
    }
    return { status: "current" };
  }
  // Non-numeric pins (tool tags, date stamps) can only be compared by equality.
  if (!pinnedParts && !latestParts) {
    const strip = (v: string) => v.trim().replace(/^v/, "");
    if (strip(pinned) === strip(latest)) return { status: "current" };
    return { status: "unknown", reason: "version format is not comparable" };
  }
  return { status: "unknown", reason: "version format is not comparable" };
}

/** True when a pin is a registry-style version requirement (not a path, git, or workspace spec). */
export function isRegistryVersion(spec: string): boolean {
  return PINNED_RE.test(spec);
}

function matchesPattern(name: string, pattern: string): boolean {
  if (pattern.startsWith("/")) {
    const body = pattern.replace(/^\/|\/[a-z]*$/g, "");
    return new RegExp(body).test(name);
  }
  return name === pattern;
}

/** Built-in tier: signing, security, licensing and credential material default to held. */
export const DEFAULT_HELD_PATTERN = "/sign|secur|licen|credential/";

/**
 * Effective policy for one dependency. Precedence:
 * 1. policy.held exact names (always held, kind-independent)
 * 2. explicit policy on the entry or source
 * 3. tier patterns (built-in plus policy.tier.held) default to held
 * 4. policy.default
 */
export function resolvePolicy(name: string, explicit: Policy | undefined, config: PolicyConfig): Policy {
  if (config.held?.some((h) => (typeof h === "string" ? h : h.name) === name)) return "held";
  if (explicit) return explicit;
  const patterns = [DEFAULT_HELD_PATTERN, ...(config.tier?.held ?? [])];
  if (patterns.some((p) => matchesPattern(name, p))) return "held";
  return config.default;
}
