// Minimal manifest glob: `*` matches within one path segment. No `**`, no hidden entries
// unless the pattern segment itself starts with a dot. Only regular files are returned.

import fs from "node:fs";
import path from "node:path";
import type { CargoSource, PnpmSource } from "./types.js";

function segmentRegex(segment: string): RegExp {
  const body = segment.replace(/[.+^${}()|[\]\\?]/g, "\\$&").replace(/\*/g, "[^/]*");
  return new RegExp(`^${body}$`);
}

/** Expand a glob (already validated by `safeManifestGlob`) to sorted, `/`-separated relative paths. */
export function expandGlob(repo: string, pattern: string): string[] {
  const segments = pattern.split(/[\\/]/);
  let current: string[] = [""];
  for (let i = 0; i < segments.length; i++) {
    const segment = segments[i];
    const last = i === segments.length - 1;
    const next: string[] = [];
    for (const base of current) {
      if (!segment.includes("*")) {
        const rel = base ? `${base}/${segment}` : segment;
        const abs = path.join(repo, rel);
        try {
          const stat = fs.lstatSync(abs);
          if (last ? stat.isFile() : stat.isDirectory()) next.push(rel);
        } catch {
          // Missing path: no match.
        }
        continue;
      }
      const re = segmentRegex(segment);
      let names: fs.Dirent[];
      try {
        names = fs.readdirSync(path.join(repo, base), { withFileTypes: true });
      } catch {
        continue;
      }
      for (const dirent of names) {
        if (dirent.name.startsWith(".") && !segment.startsWith(".")) continue;
        if (!re.test(dirent.name)) continue;
        if (last ? !dirent.isFile() : !dirent.isDirectory()) continue;
        next.push(base ? `${base}/${dirent.name}` : dirent.name);
      }
    }
    current = next;
    if (current.length === 0) break;
  }
  return current.sort();
}

/** Concrete manifest paths (relative to the repo) a cargo or pnpm source covers. */
export function sourceManifests(repo: string, source: CargoSource | PnpmSource): string[] {
  if (source.manifest !== undefined) return [source.manifest];
  return source.glob !== undefined ? expandGlob(repo, source.glob) : [];
}
