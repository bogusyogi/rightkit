export interface ParsedVersion {
  major: number;
  minor: number;
  patch: number;
  prerelease: readonly (string | number)[];
}

const SEMVER = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/;

/** Parse a semver string (optional leading `v`, build metadata ignored). Returns null when invalid. */
export function parseVersion(input: string): ParsedVersion | null {
  const match = SEMVER.exec(input.trim());
  if (!match) return null;
  const prerelease = match[4]
    ? match[4].split('.').map((part) => (/^\d+$/.test(part) ? Number(part) : part))
    : [];
  return { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]), prerelease };
}

/** Semver precedence compare. Throws on an invalid version so callers never act on garbage. */
export function compareVersions(a: string, b: string): -1 | 0 | 1 {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (!pa) throw new Error(`invalid version: ${a}`);
  if (!pb) throw new Error(`invalid version: ${b}`);
  for (const key of ['major', 'minor', 'patch'] as const) {
    if (pa[key] !== pb[key]) return pa[key] > pb[key] ? 1 : -1;
  }
  if (pa.prerelease.length === 0 && pb.prerelease.length === 0) return 0;
  if (pa.prerelease.length === 0) return 1;
  if (pb.prerelease.length === 0) return -1;
  const len = Math.max(pa.prerelease.length, pb.prerelease.length);
  for (let i = 0; i < len; i += 1) {
    const x = pa.prerelease[i];
    const y = pb.prerelease[i];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    if (x === y) continue;
    if (typeof x === 'number' && typeof y === 'number') return x > y ? 1 : -1;
    if (typeof x === 'number') return -1;
    if (typeof y === 'number') return 1;
    return x > y ? 1 : -1;
  }
  return 0;
}

/** True when `candidate` is strictly newer than `current`. */
export function isNewerVersion(candidate: string, current: string): boolean {
  return compareVersions(candidate, current) > 0;
}
