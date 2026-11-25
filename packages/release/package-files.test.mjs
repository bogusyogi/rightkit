// The published package must contain every module its entry points load.
//
// 0.6.57 was published without the `gate` folder: `files` did not list it, bin/rightkit-gate.mjs loads `../gate/compile.mjs` with
// `import()`, and an installed gate failed every unit-list compile on a broker host with ERR_MODULE_NOT_FOUND. A checkout has the folder,
// so nothing run from a checkout could see it. This test walks the relative imports of each entry point (static and `import()` with a
// string literal) and requires each file reached to be one the `files` list publishes.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8"));
const posix = (p) => p.split(path.sep).join("/");

/** One `files` pattern as a test of a package-relative posix path. `*` matches within one path segment, as npm reads it for these entries. */
function matcher(pattern) {
  const body = pattern.replace(/^!/, "");
  const re = new RegExp(`^${body.split("*").map((part) => part.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join("[^/]*")}(?:/.*)?$`);
  return (file) => re.test(file);
}
const rules = pkg.files.map((f) => ({ include: !f.startsWith("!"), test: matcher(f) }));
/** True when `files` publishes this package-relative path. package.json is always published. npm reads the list in order: the last
 * entry that matches decides, so an entry after a negation re-includes (acceptance-lines.test.mjs, a contract-test file promote requires). */
export const published = (file) => file === "package.json" || rules.reduce((decided, rule) => (rule.test(file) ? rule.include : decided), false);

/** Relative specifiers a module loads: `from "./x"`, `import "./x"` and `import("./x")` with a string literal. */
function relativeImports(source) {
  const found = new Set();
  for (const re of [/\bfrom\s+["'](\.{1,2}\/[^"']+)["']/g, /\bimport\s+["'](\.{1,2}\/[^"']+)["']/g, /\bimport\(\s*["'](\.{1,2}\/[^"']+)["']\s*\)/g]) {
    // Node resolves a relative ESM specifier only with its extension, so one without is prose in a comment or a string, not an import.
    for (const m of source.matchAll(re)) if (/\.(?:mjs|cjs|js|json)$/.test(m[1])) found.add(m[1]);
  }
  return [...found];
}

const entryPoints = () => {
  const bins = typeof pkg.bin === "string" ? [pkg.bin] : Object.values(pkg.bin ?? {});
  const exported = [];
  const walk = (v) => { if (typeof v === "string") exported.push(v); else if (v && typeof v === "object") Object.values(v).forEach(walk); };
  walk(pkg.exports ?? {});
  if (typeof pkg.main === "string") exported.push(pkg.main);
  return [...new Set([...bins, ...exported].filter((f) => /\.m?js$/.test(f)).map((f) => posix(path.normalize(f))))];
};

test("the files list has the shapes this test reads: no pattern with a directory wildcard", () => {
  for (const f of pkg.files) assert.ok(!f.includes("**"), `${f}: this test reads a single-segment * only; extend matcher() before using **`);
});

test("every module the entry points import is published", () => {
  const entries = entryPoints();
  assert.ok(entries.includes("bin/rightkit-gate.mjs"), `bin/rightkit-gate.mjs is an entry point (found ${entries.join(", ")})`);
  const seen = new Set(); const queue = [...entries]; const missing = [];
  while (queue.length) {
    const file = queue.pop();
    if (seen.has(file)) continue;
    seen.add(file);
    const abs = path.join(ROOT, file);
    if (!fs.existsSync(abs)) { missing.push(`${file} (does not exist in the package source)`); continue; }
    if (!published(file)) missing.push(`${file} (not covered by package.json files)`);
    if (!/\.m?js$/.test(file)) continue;
    for (const spec of relativeImports(fs.readFileSync(abs, "utf8"))) {
      const target = posix(path.normalize(path.join(path.dirname(file), spec)));
      if (target.startsWith("..")) continue; // outside the package: another workspace package, resolved by name at run time
      queue.push(target);
    }
  }
  assert.deepEqual(missing, [], `entry points load ${missing.length} file(s) the tarball would not carry`);
  assert.ok(seen.has("gate/compile.mjs"), "the walk reaches gate/compile.mjs (bin/rightkit-gate.mjs loads it with import())");
});

test("test files are not published", () => {
  assert.equal(published("gate/gate.test.mjs"), false);
  assert.equal(published("package-files.test.mjs"), false);
  assert.equal(published("gate/compile.mjs"), true);
});

test("every file of the gate's contract test is published: trusted-gate promote reads each one from the installed candidate", () => {
  const contract = JSON.parse(fs.readFileSync(path.join(ROOT, "contract-nn.json"), "utf8"));
  const missing = (contract.runnerFiles ?? []).filter((f) => !published(f));
  assert.deepEqual(missing, [], "0.6.80: promote refused because acceptance-lines.test.mjs was not in the package");
});
