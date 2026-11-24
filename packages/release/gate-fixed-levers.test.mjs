// The gate's cache has one fixed place per repository, compiles are always combined, and the resident process cannot be switched off:
// no caller's environment changes any of the three (Adrian, 2026-11-24, rightkit#1514; GC-A27 NN06, NN09 and "no waste").
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const sources = ["bin/rightkit-gate.mjs", "deterministic-scans.mjs", "class-a-cache-history.mjs", "gate-serve.mjs", "snapshot.mjs", "gate/compile.mjs", "gate/calls.mjs"]
  .filter((f) => fs.existsSync(path.join(here, f)));

for (const name of ["RK_GATE_CACHE_DIR", "RK_GATE_LEGACY_ROWS", "RK_GATE_SERVE"]) {
  test(`the gate reads no ${name} from its caller`, () => {
    const read = new RegExp(`env\\??\\.${name}\\b|env\\[["']${name}["']\\]`);
    for (const f of sources) assert.doesNotMatch(fs.readFileSync(path.join(here, f), "utf8"), read, `${f} reads ${name}`);
  });
}

test("the scan store is the folder rightkit-gate-cache in the repository's git folder, wherever the gate opens it", () => {
  const gate = fs.readFileSync(path.join(here, "bin/rightkit-gate.mjs"), "utf8");
  const stores = gate.split("\n").filter((l) => /const storeDir = /.test(l));
  assert.ok(stores.length >= 3, "the gate opens its store in at least three places");
  for (const l of stores) { assert.match(l, /"rightkit-gate-cache"\)/); assert.doesNotMatch(l, /env/); }
});
