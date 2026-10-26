// The INCOMPLETE closure driven end to end by real processes: a fixture package is packed with `npm pack`, installed from that
// tarball by the real `npm install --ignore-scripts` consume.mjs runs, imported by the real import-check child, and the source
// probe's INCOMPLETE rows are closed (or refused) against what that child actually imported. No network: no dependencies.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { consumeNpm } from "./consume.mjs";
import { closeIncomplete, expectedIncomplete } from "./exports-import-effects.mjs";

const NAME = "@rk/real"; const VERSION = "1.2.3";
function packFixture(t, { skipImport = [] } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-real-pkg-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const pkg = { name: NAME, version: VERSION, type: "module", files: ["dist", "src"], exports: { ".": "./dist/index.js", "./x": "./dist/x.js", "./src": "./src/s.mjs" }, ...(skipImport.length ? { rightkit: { publishCheck: { skipImport } } } : {}) };
  fs.mkdirSync(path.join(dir, "dist")); fs.mkdirSync(path.join(dir, "src"));
  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify(pkg));
  fs.writeFileSync(path.join(dir, "dist", "index.js"), "export const index = 1;\n");
  fs.writeFileSync(path.join(dir, "dist", "x.js"), "export const x = 2;\n");
  fs.writeFileSync(path.join(dir, "src", "s.mjs"), "export const s = 3;\n");
  const packDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-real-pack-")); t.after(() => fs.rmSync(packDir, { recursive: true, force: true }));
  const out = execFileSync("npm", ["pack", "--ignore-scripts", "--pack-destination", packDir, dir], { encoding: "utf8", env: { ...process.env, npm_config_loglevel: "error" } }).trim().split("\n").at(-1);
  const file = path.join(packDir, out); const bytes = fs.readFileSync(file);
  return { pkg, packDir, file: out, sha256: crypto.createHash("sha256").update(bytes).digest("hex"), size: bytes.length };
}
const rows = (keys) => keys.map((key) => ({ package: NAME, key, target: key === "." ? "./dist/index.js" : "./dist/x.js" }));
const strip = (r) => r.problems.filter((p) => !/missing reviewed PK example/.test(p));

test("real pack → real npm install → real import child: every INCOMPLETE dist export is imported from the sealed tarball and closed", (t) => {
  const f = packFixture(t); const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), "rk-real-scratch-")); t.after(() => fs.rmSync(scratchRoot, { recursive: true, force: true }));
  const r = consumeNpm({ units: [{ name: NAME, version: VERSION, file: f.file, sha256: f.sha256, size: f.size }], packDir: f.packDir, scratchRoot, incomplete: { schema: "rightkit.import-effects/1", incomplete: rows([".", "./x"]), covered: [] }, npmConsumer: () => { throw new Error("not used"); } }).get(NAME);
  assert.deepEqual(strip(r), [], JSON.stringify(r.problems));
  assert.deepEqual(r.report.imported.sort(), [NAME, `${NAME}/src`, `${NAME}/x`].sort(), "the real child imported every export");
  assert.equal(r.report.incompleteClosed, 2);
});

test("real pack/install/import: an export the sealed package declares skipImport for is resolved only, so its INCOMPLETE row stays open and the unit is refused", (t) => {
  const f = packFixture(t, { skipImport: ["./x"] }); const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), "rk-real-scratch-")); t.after(() => fs.rmSync(scratchRoot, { recursive: true, force: true }));
  const r = consumeNpm({ units: [{ name: NAME, version: VERSION, file: f.file, sha256: f.sha256, size: f.size }], packDir: f.packDir, scratchRoot, incomplete: { schema: "rightkit.import-effects/1", incomplete: rows([".", "./x"]), covered: [] }, npmConsumer: () => { throw new Error("not used"); } }).get(NAME);
  const problems = strip(r);
  assert.ok(problems.some((p) => /INCOMPLETE not closed: @rk\/real\/x \(\.\/dist\/x\.js\)/.test(p)), JSON.stringify(problems));
  assert.ok(r.report.resolvedOnly.includes(`${NAME}/x`)); assert.equal(r.report.incompleteClosed, 1);
});

test("real pack/install/import: a forged COMPLETE report (no rows) is refused once per dist export the sealed package.json declares", (t) => {
  const f = packFixture(t); const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), "rk-real-scratch-")); t.after(() => fs.rmSync(scratchRoot, { recursive: true, force: true }));
  const r = consumeNpm({ units: [{ name: NAME, version: VERSION, file: f.file, sha256: f.sha256, size: f.size }], packDir: f.packDir, scratchRoot, incomplete: { schema: "rightkit.import-effects/1", incomplete: [], covered: [] }, npmConsumer: () => { throw new Error("not used"); } }).get(NAME);
  assert.equal(strip(r).length, 2); assert.deepEqual(expectedIncomplete(f.pkg).map((e) => e.key), [".", "./x"]);
  assert.deepEqual(closeIncomplete({ report: { incomplete: rows([".", "./x"]), covered: [] }, name: NAME, importedSpecs: r.report.imported, expected: expectedIncomplete(f.pkg) }), []);
});
