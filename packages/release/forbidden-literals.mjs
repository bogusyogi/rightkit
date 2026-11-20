// Release-time absence scan: fail when any file under the scanned targets contains one of a set of
// forbidden byte strings (secret names, secret values read from the environment). The app declares
// WHAT is forbidden and WHERE to look; nothing here names a product, a secret or a path.
//
// Used by an app's own check script (`scripts/assert-...mjs`), which holds only its configuration:
//
//   import { runForbiddenLiteralScan } from "@rightkit/release/forbidden-literals.mjs";
//   runForbiddenLiteralScan({ root, defaults: [...], literals: [...], envNames: [...],
//                             failure: "reusable model bearer material found in", ok: "...: ok" });
//
// Symlinks are never followed. A scan target that does not exist is an error, never a pass.
import { existsSync, lstatSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";

/** Every regular file under `target` (a file, or a directory walked recursively), symlinks skipped. */
export function filesWithin(target) {
  if (!existsSync(target)) throw new Error(`scan target does not exist: ${target}`);
  const stat = lstatSync(target);
  if (stat.isSymbolicLink()) return [];
  if (stat.isFile()) return [target];
  if (!stat.isDirectory()) return [];
  return readdirSync(target, { withFileTypes: true }).flatMap((entry) => {
    const child = path.join(target, entry.name);
    if (entry.isSymbolicLink()) return [];
    return entry.isDirectory() ? filesWithin(child) : entry.isFile() ? [child] : [];
  });
}

/**
 * The files under `targets` that contain any needle.
 * @param {{ targets: string[], needles: Buffer[] }} input
 * @returns {string[]} the offending file paths
 */
export function scanForbiddenLiterals({ targets, needles }) {
  const found = [];
  for (const file of targets.flatMap(filesWithin)) {
    const bytes = readFileSync(file);
    if (needles.some((needle) => needle.length > 0 && bytes.includes(needle))) found.push(file);
  }
  return found;
}

/**
 * The whole CLI behaviour. Exits 1 on a finding or an error; prints `ok` on success.
 * @param {{
 *   root: string,                 // repo-relative paths in messages are printed relative to this
 *   defaults: string[],           // scan targets (absolute) used when no argv targets are given
 *   literals: string[],           // forbidden literal strings
 *   envNames?: string[],          // env vars whose CURRENT non-empty value is also forbidden
 *   failure: string,              // message prefix printed with the relative file path
 *   ok: string,                   // message printed on success
 *   argv?: string[],              // extra targets (default: process.argv.slice(2))
 * }} config
 */
export function runForbiddenLiteralScan(config) {
  const targets = (config.argv ?? process.argv.slice(2)).map((entry) => path.resolve(entry));
  const needles = config.literals.map((literal) => Buffer.from(literal));
  for (const name of config.envNames ?? []) {
    const value = process.env[name]?.trim();
    if (value) needles.push(Buffer.from(value));
  }
  let failed = false;
  try {
    const offenders = scanForbiddenLiterals({
      targets: targets.length > 0 ? targets : config.defaults,
      needles,
    });
    for (const file of offenders) {
      console.error(`${config.failure} ${path.relative(config.root, file)}`);
      failed = true;
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
  if (failed) process.exit(1);
  console.log(config.ok);
}
