import "./test-support/broker-guard.mjs";
import { REAL_BUILD_SKIP } from "./test-support/temp-roots.mjs";
import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(packageRoot, "cli", "right-release.mjs");

// suite-doctor runs Cargo metadata for real workspaces across repositories: that reaches the build broker
// on a managed host, so it runs only in hosted CI (the guarded right-suite-contract suite covers it locally).
test("suite-doctor runs the complete local multi-repo contract", { skip: REAL_BUILD_SKIP }, () => {
  const result = spawnSync(process.execPath, [cli, "suite-doctor"], {
    cwd: packageRoot,
    encoding: "utf8",
    windowsHide: true,
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /\[right-release\] suite-doctor passed/);
});
