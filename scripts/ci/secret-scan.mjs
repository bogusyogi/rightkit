#!/usr/bin/env node
// Secret scan of the crates and npm packages a push touches (job `hygiene` of .github/workflows/ci.yml).
// It is the same scan the publication gate runs (scanCrateForSecrets in packages/release/publish-cargo.mjs):
// key and token patterns in any text file, and files that must never ship (.env, *.pem, *.p8, *.p12, *.key,
// target/, node_modules/). Usage:
//   node scripts/ci/secret-scan.mjs --crates "<crate names>" --dirs "<package dirs>"
// Exits 1 when any directory has a violation, 2 on a bad argument. It reads files only.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const argOf = (flag) => { const i = process.argv.indexOf(flag); return i > 0 ? process.argv[i + 1] : ""; };
const list = (flag) => argOf(flag).split(/\s+/).filter(Boolean);

/** Directories to scan, as repo-relative paths; a name or directory that could leave the repository is refused. */
export function scanTargets(crates, dirs) {
  const targets = [];
  for (const name of crates) {
    if (!/^[a-z0-9][a-z0-9_-]*$/.test(name)) throw new Error(`refusing crate name ${JSON.stringify(name)}`);
    targets.push(`crates/${name}`);
  }
  for (const dir of dirs) {
    if (!/^packages\/[a-z0-9][a-z0-9._-]*$/.test(dir)) throw new Error(`refusing package directory ${JSON.stringify(dir)}`);
    targets.push(dir);
  }
  return [...new Set(targets)];
}

async function main() {
  let targets;
  try { targets = scanTargets(list("--crates"), list("--dirs")); } catch (error) { console.error(`secret-scan: ${error.message}`); process.exit(2); }
  const { scanCrateForSecrets } = await import("../../packages/release/publish-cargo.mjs");
  const failures = [];
  let scanned = 0;
  for (const target of targets) {
    const dir = path.join(ROOT, target);
    if (!fs.existsSync(dir)) { failures.push(`${target}: directory not found`); console.log(`FAIL ${target}: directory not found`); continue; }
    scanned += 1;
    const violations = scanCrateForSecrets(dir);
    console.log(`${violations.length ? "FAIL" : "ok  "} ${target}${violations.length ? `: ${violations.join(", ")}` : ""}`);
    if (violations.length) failures.push(`${target}: ${violations.join(", ")}`);
  }
  console.log(`secret-scan: ${scanned} director${scanned === 1 ? "y" : "ies"} scanned, ${failures.length} with violations`);
  process.exit(failures.length ? 1 : 0);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
