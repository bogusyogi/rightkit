#!/usr/bin/env node
// Fails when a built or installed bundle carries a Node or Python runtime,
// interpreter libraries, or script sources (M19 owner gap 2; replaces the
// per-app scan-bundle.mjs copies in CutRight and GenRight).
// Usage: right-release bundle-scan [--allow-name <basename>]... <bundle-root>...
import { readdirSync, statSync } from "node:fs";
import { basename, extname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const BANNED_NAMES = Object.freeze([
  /^node(js)?(\.exe)?$/i, /^libnode/i, /^python[\d.]*w?(\.exe)?$/i, /^libpython/i,
  /^python\d+\.dll$/i, /^pythonw?\d*\.zip$/i,
]);
export const BANNED_EXTENSIONS = Object.freeze(new Set([".js", ".mjs", ".cjs", ".py", ".pyc", ".pyo", ".pyd", ".pyz"]));
export const BANNED_DIRECTORIES = Object.freeze(new Set(["node_modules", "site-packages", "__pycache__"]));

/** Scan one bundle root. `allowNames` exempts exact basenames (product-declared exceptions). */
export function scanRuntimeBundle(root, { allowNames = [] } = {}) {
  const allowed = new Set(allowNames);
  const offenders = [];
  let files = 0;
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) {
        if (BANNED_DIRECTORIES.has(entry.name) && !allowed.has(entry.name)) offenders.push(path);
        walk(path);
        continue;
      }
      files += 1;
      const name = basename(path);
      if (allowed.has(name)) continue;
      if (BANNED_EXTENSIONS.has(extname(name).toLowerCase()) || BANNED_NAMES.some((re) => re.test(name))) offenders.push(path);
    }
  };
  if (statSync(root).isDirectory()) walk(root); else files = 1;
  return { root, files, offenders };
}

export function runtimeBundleScanFailed(results) {
  return results.some((r) => r.offenders.length > 0 || r.files === 0);
}

function main(argv) {
  const roots = [];
  const allowNames = [];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--allow-name") allowNames.push(argv[++i] ?? "");
    else roots.push(argv[i]);
  }
  if (!roots.length || allowNames.includes("")) {
    console.error("usage: right-release bundle-scan [--allow-name <basename>]... <bundle-root>...");
    return 2;
  }
  const results = roots.map((root) => scanRuntimeBundle(root, { allowNames }));
  console.log(JSON.stringify(results, null, 2));
  return runtimeBundleScanFailed(results) ? 1 : 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exit(main(process.argv.slice(2)));
