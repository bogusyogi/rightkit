// The test classes of @rightkit/release (test-support/suites.json): every test file is declared, and a "unit" file never needs the installed RustRight broker.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const declared = JSON.parse(fs.readFileSync(path.join(root, "test-support", "suites.json"), "utf8")).files;
const testFilesIn = (dir) => fs.readdirSync(path.join(root, dir)).filter((name) => name.endsWith(".test.mjs")).sort().map((name) => (dir ? `${dir}/${name}` : name));
const actual = [...testFilesIn(""), ...testFilesIn("cli")];

test("every *.test.mjs is declared in test-support/suites.json as unit or broker, and nothing else is", () => {
  assert.deepEqual(Object.keys(declared).sort(), [...actual].sort());
  for (const [file, kind] of Object.entries(declared)) assert.ok(kind === "unit" || kind === "broker", `${file}: class ${kind} is neither unit nor broker`);
});

test("a unit-class file never imports test-support/real-broker.mjs; a broker-class file does", () => {
  for (const file of actual) {
    const imports = /from\s+["'][^"']*real-broker\.mjs["']|import\(\s*["'][^"']*real-broker\.mjs["']\s*\)/.test(fs.readFileSync(path.join(root, file), "utf8"));
    if (declared[file] === "unit") assert.equal(imports, false, `${file} imports real-broker.mjs: declare it "broker" in test-support/suites.json`);
    else assert.equal(imports, true, `${file} is declared "broker" but does not import real-broker.mjs: declare it "unit"`);
  }
});
