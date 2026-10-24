// Shared fixtures for sync/drift/uninstall tests. Not a *.test.mjs file, so
// `node --test *.test.mjs` never picks it up as a test suite on its own.

import fs from "node:fs";
import path from "node:path";
import os from "node:os";

export function makeTempRepo(manifest, { packageJson } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-test-"));
  if (manifest) fs.writeFileSync(path.join(dir, ".rightgit.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify(packageJson || { name: "widgets" }), "utf8");
  return dir;
}

function ghDeps(visibility) {
  return {
    execFile: async (cmd) => {
      if (cmd === "gh") return { stdout: JSON.stringify({ visibility }) };
      throw new Error(`unexpected exec: ${cmd}`);
    },
  };
}

export const PUBLIC_DEPS = ghDeps("PUBLIC");
export const PRIVATE_DEPS = ghDeps("PRIVATE");

export const SAMPLE_MANIFEST = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: ["ci", "package-smoke"],
  matrix: { os: ["ubuntu-latest"], node: ["22.13"] },
  packageManager: "pnpm@11.25.0",
  templateVersion: "1.0.0",
};
