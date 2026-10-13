import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runtimeBundleScanFailed, scanRuntimeBundle } from "./runtime-bundle-scan.mjs";

function fixture(files) {
  const root = mkdtempSync(join(tmpdir(), "bundle-scan-"));
  for (const rel of files) {
    const full = join(root, rel);
    mkdirSync(join(full, ".."), { recursive: true });
    writeFileSync(full, "x");
  }
  return root;
}

test("a clean bundle passes", () => {
  const root = fixture(["App.app/Contents/MacOS/app", "App.app/Contents/Resources/icon.icns"]);
  try {
    const result = scanRuntimeBundle(root);
    assert.deepEqual(result.offenders, []);
    assert.equal(runtimeBundleScanFailed([result]), false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("node, python, script sources and package dirs are offenders", () => {
  const root = fixture(["bin/node", "bin/python3.11", "lib/libpython3.dylib", "res/a.py", "res/b.mjs", "res/node_modules/x/readme", "ok.txt"]);
  try {
    const result = scanRuntimeBundle(root);
    const names = result.offenders.map((p) => p.slice(root.length + 1).replaceAll("\\", "/")).sort();
    assert.deepEqual(names, ["bin/node", "bin/python3.11", "lib/libpython3.dylib", "res/a.py", "res/b.mjs", "res/node_modules"]);
    assert.equal(runtimeBundleScanFailed([result]), true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("an empty root fails and an allowed basename is exempt", () => {
  const empty = mkdtempSync(join(tmpdir(), "bundle-scan-empty-"));
  const root = fixture(["res/helper.js"]);
  try {
    assert.equal(runtimeBundleScanFailed([scanRuntimeBundle(empty)]), true);
    assert.deepEqual(scanRuntimeBundle(root, { allowNames: ["helper.js"] }).offenders, []);
  } finally { rmSync(empty, { recursive: true, force: true }); rmSync(root, { recursive: true, force: true }); }
});
