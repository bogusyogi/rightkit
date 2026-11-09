import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { builtToolBinary } from "./tool-build.mjs";

test("broker tool output uses its target-qualified path & refuses a stale unqualified decoy", () => {
  const outputDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-tool-output-"));
  const executable = "rightkit-qa";
  try {
    fs.mkdirSync(path.join(outputDir, "release"));
    fs.writeFileSync(path.join(outputDir, "release", executable), "stale");
    const targetTriple = "aarch64-apple-darwin";
    assert.throws(() => builtToolBinary({ outputDir, executable, targetTriple }), /does not exist/);
    const actual = path.join(outputDir, targetTriple, "release", executable);
    fs.mkdirSync(path.dirname(actual), { recursive: true });
    fs.writeFileSync(actual, "current");
    assert.equal(builtToolBinary({ outputDir, executable, targetTriple }), actual);
    assert.equal(builtToolBinary({ outputDir, executable }), path.join(outputDir, "release", executable));
    for (const bad of ["../escape", "", "C:\\escape", "/escape", "a/../b"]) {
      assert.throws(() => builtToolBinary({ outputDir, executable, targetTriple: bad }), /invalid receipt target/);
    }
  } finally { fs.rmSync(outputDir, { recursive: true, force: true }); }
});
