// Test-only temp directory registry for @rightkit/release. Not shipped: lives under test-support/.
//
// `tempRoot(prefix)` creates a directory under the OS temp dir and removes it when the importing test
// file finishes (node:test `after` hook), with a synchronous process-exit fallback for a forced exit.
// A fixture is therefore always removed, even when a test throws before its own cleanup.
import { mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { after } from "node:test";

const roots = new Set();
let hooked = false;

function removeAll() {
  for (const root of roots) {
    try {
      rmSync(root, { recursive: true, force: true, maxRetries: 3 });
    } catch {
      // A directory that cannot be removed is reported by the OS temp sweep; it never changes a test verdict.
    }
  }
  roots.clear();
}

export function tempRoot(prefix) {
  if (!hooked) {
    hooked = true;
    after(removeAll);
    process.on("exit", removeAll);
  }
  const root = mkdtempSync(path.join(os.tmpdir(), prefix));
  roots.add(root);
  return root;
}

// Real builds (a compiler, Cargo or Tauri run against a fixture) are allowed only on hosted CI.
// Everywhere else the default run uses stubs and skips the real variant with this reason.
export const REAL_BUILD_SKIP = process.env.CI === "true" ? false : "real cargo/tauri build runs only in hosted CI (CI=true); the default run uses stubs";
