import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(packageRoot, "cli", "right-release.mjs");

test("suite-doctor runs the complete local multi-repo contract", () => {
  const result = spawnSync(process.execPath, [cli, "suite-doctor"], {
    cwd: packageRoot,
    encoding: "utf8",
    windowsHide: true,
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /\[right-release\] suite-doctor passed/);
});
