#!/usr/bin/env node
// Resolves the Cargo target directory for a manifest by asking Cargo itself.
//
// `cargo metadata` is authoritative on BOTH broker-managed and unmanaged
// hosts: through a managed shim it reports the broker's own workspace target
// directory; without one it reports the local `target/` directory. LOCATING
// the target directory therefore never needs broker detection — the metadata
// command already reflects whichever host it runs on. `brokerManagedHost()`
// (see cargo-contract.mjs) remains a separate, cache-policy-only decision:
// whether Cache V2's lease/slot/prune/floor machinery should run at all, not
// where the target directory lives.
import { realpathSync } from "node:fs";
import { execFileSync } from "./background-process.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function resolveTargetRoot(manifestPath, options = {}) {
  if (!manifestPath) throw new Error("resolveTargetRoot requires a Cargo.toml manifest path");
  const execute = options.execFileSync ?? execFileSync;
  let output;
  try {
    const args = ["metadata", "--offline", "--format-version", "1", "--no-deps", "--manifest-path", manifestPath];
    if (options.execFileSync) {
      output = execute("cargo", args, { cwd: path.dirname(manifestPath), encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    } else {
      const result = runCargoSync(args, { cwd: path.dirname(manifestPath), encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
      if (result.error) throw result.error;
      if (result.status !== 0) throw new Error(String(result.stderr ?? "").trim() || `exited ${result.status}`);
      output = result.stdout;
    }
  } catch (error) {
    throw new Error(`metadata failed for ${manifestPath}: ${error.message}`);
  }
  const { target_directory: directory } = JSON.parse(output);
  if (typeof directory !== "string" || directory.length === 0 || !path.isAbsolute(directory)) {
    throw new Error(`metadata for ${manifestPath} did not report an absolute target_directory`);
  }
  return path.resolve(directory);
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const manifestPath = process.argv[2];
  if (!manifestPath) {
    console.error("usage: cargo-target.mjs <path-to-Cargo.toml>");
    process.exit(1);
  }
  try {
    process.stdout.write(`${resolveTargetRoot(manifestPath)}\n`);
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
