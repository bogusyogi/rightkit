// The public module surface of @rightkit/release (plan item 2.10, gap C4-16).
//
// package.json "exports" lists exactly the supported subpaths; every other file is private. docs/RELEASE-EXPORTS.md is the
// inventory the map was cut from (every deep import of the package that product repositories and RightKit's own tooling make,
// with file:line). These tests keep the map, the inventory and the package in step, and fail when a checked-out product
// repository imports a subpath the map does not list.
import assert from "node:assert/strict";
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import {
  exportKey,
  formOf,
  importsInText,
  importsOutsideMap,
  pathAccessesInText,
  scanTree,
} from "./test-support/release-exports-scan.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");
const pkg = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8"));
const exportsMap = pkg.exports;
const keys = Object.keys(exportsMap ?? {});
// A Windows checkout may hold the documents with CRLF line ends; the sections and rows are matched line by line on LF.
const doc = readFileSync(path.join(repoRoot, "docs", "RELEASE-EXPORTS.md"), "utf8").replace(/\r\n/g, "\n");
const readme = readFileSync(path.join(here, "README.md"), "utf8").replace(/\r\n/g, "\n");
const require = createRequire(import.meta.url);
const real = (file) => realpathSync(file);

// ---------------------------------------------------------------------------------------------- the inventory document

function section(title) {
  const start = doc.indexOf(`\n## ${title}\n`);
  assert.notEqual(start, -1, `docs/RELEASE-EXPORTS.md has no "${title}" section`);
  const next = doc.indexOf("\n## ", start + 1);
  return doc.slice(start, next === -1 ? undefined : next).split("\n");
}
const rowsOf = (title, pattern) => section(title).map((line) => pattern.exec(line)).filter(Boolean);
const mapRows = rowsOf("Exports map", /^\| `(\.\/[^`]+)` \| (bin|package|sdk|app-import) \| (.+) \|$/)
  .map(([, key, klass, why]) => ({ key, klass, why }));
const importRows = rowsOf("Import inventory", /^\| `([^`]+)` \| ([^|]+) \| `([^`]+):(\d+)` \| ([^|]+) \|$/)
  .map(([, subpath, consumer, file, line, form]) => ({ subpath, consumer: consumer.trim(), file, line: Number(line), form: form.trim() }));
const pathRows = rowsOf("Path accesses", /^\| `([^`]+)` \| ([^|]+) \| `([^`]+):(\d+)` \| ([^|]+) \| (exported|not exported) \|$/)
  .map(([, subpath, consumer, file, line, use, map]) => ({ subpath, consumer: consumer.trim(), file, line: Number(line), use: use.trim(), exported: map === "exported" }));

// ---------------------------------------------------------------------------------------------------- the map itself

test("the exports map is explicit: one ./<file> key per public file, no index, no wildcard, no conditions", () => {
  assert.ok(exportsMap && typeof exportsMap === "object" && !Array.isArray(exportsMap), "package.json must declare an exports object");
  assert.ok(keys.length > 0);
  for (const [key, target] of Object.entries(exportsMap)) {
    assert.match(key, /^\.\/[A-Za-z0-9_][A-Za-z0-9_./-]*$/, `${key} must be an explicit ./<file> subpath`);
    assert.ok(!key.includes("*"), `${key}: wildcard subpaths would publish every matching file`);
    assert.equal(target, key, `${key} must map to the same file (a string, so import and require see one surface)`);
    const file = path.join(here, key.slice(2));
    assert.ok(!key.slice(2).split("/").includes(".."), `${key} escapes the package`);
    assert.ok(lstatSync(file).isFile(), `${key} has no file`);
    assert.ok(!/\.test\.mjs$/.test(key), `${key} is a test file`);
  }
  assert.equal(Object.hasOwn(exportsMap, "."), false, "the package has no importable index");
  assert.deepEqual(keys, [...keys].sort(), "keys stay sorted: the consumer recipe's reviewed surface is the sorted key list");
});

test("package.json and every bin target are exported", () => {
  assert.ok(keys.includes("./package.json"));
  for (const [name, target] of Object.entries(pkg.bin)) assert.ok(keys.includes(`./${target}`), `bin ${name} -> ${target} is not exported`);
});

test("every exported file is part of the packed package", (t) => {
  const staticRule = (key) => /^\.\/(?:(?:cli|bin)\/[^/]+|[^/]+\.(?:mjs|json))$/.test(key);
  for (const key of keys) assert.ok(staticRule(key), `${key} is outside the package "files" (cli/, bin/, top-level *.mjs and *.json)`);
  const packed = spawnSync(process.platform === "win32" ? "npm.cmd" : "npm", ["pack", "--dry-run", "--json", "--ignore-scripts"], {
    cwd: here, encoding: "utf8", shell: process.platform === "win32", timeout: 120_000, windowsHide: true,
  });
  let files = null;
  try { if (!packed.error && packed.status === 0) files = new Set(JSON.parse(packed.stdout)[0].files.map((file) => file.path)); } catch { files = null; }
  if (files === null) { t.diagnostic("npm pack --dry-run was unavailable; only the static files rule ran"); return; }
  for (const key of keys) assert.ok(files.has(key.slice(2)), `npm would not publish ${key.slice(2)}`);
});

test("Node resolves every exported subpath to its file: import.meta.resolve and require.resolve", () => {
  for (const key of keys) {
    const specifier = `@rightkit/release/${key.slice(2)}`;
    const expected = real(path.join(here, key.slice(2)));
    assert.equal(real(fileURLToPath(import.meta.resolve(specifier))), expected, specifier);
    assert.equal(real(require.resolve(specifier)), expected, `${specifier} (require)`);
  }
});

test("everything else is private: Node refuses it with ERR_PACKAGE_PATH_NOT_EXPORTED", () => {
  const refused = (specifier) => {
    try { import.meta.resolve(specifier); } catch (error) { return error.code; }
    return "resolved";
  };
  // The package root has no index and is not exported.
  assert.equal(refused("@rightkit/release"), "ERR_PACKAGE_PATH_NOT_EXPORTED");
  // Named internals: the build engine, signing, publication and ownership code that apps must not reach into.
  const internals = ["release.mjs", "build-release.mjs", "upload-release.mjs", "sign-macos.mjs", "sign-updater.mjs", "ownership.mjs", "publish-update.mjs"];
  for (const name of internals) {
    assert.ok(existsSync(path.join(here, name)), `${name} is expected to exist so this check means something`);
    assert.ok(!keys.includes(`./${name}`), `${name} must stay private`);
    assert.equal(refused(`@rightkit/release/${name}`), "ERR_PACKAGE_PATH_NOT_EXPORTED", name);
  }
  // Every other top-level file (modules, data, tests) is private as well; so is a nested file and a path trick.
  const topLevel = readdirSync(here, { withFileTypes: true }).filter((entry) => entry.isFile()).map((entry) => entry.name);
  const privateFiles = topLevel.filter((name) => !keys.includes(`./${name}`));
  assert.ok(privateFiles.length > 100, "the package keeps its internals in the same directory");
  for (const name of privateFiles) assert.equal(refused(`@rightkit/release/${name}`), "ERR_PACKAGE_PATH_NOT_EXPORTED", name);
  assert.equal(refused("@rightkit/release/templates/cargo-profiles.toml"), "ERR_PACKAGE_PATH_NOT_EXPORTED");
  assert.equal(refused("@rightkit/release/cli/../release.mjs"), "ERR_PACKAGE_PATH_NOT_EXPORTED");
  assert.equal(refused("@rightkit/release/test-support/broker-guard.mjs"), "ERR_PACKAGE_PATH_NOT_EXPORTED");
});

// -------------------------------------------------------------------------------- the map against the inventory document

test("the inventory's exports table lists exactly the package.json keys", () => {
  assert.deepEqual(mapRows.map((row) => row.key), keys);
  assert.equal(new Set(mapRows.map((row) => row.key)).size, mapRows.length);
});

test("each exported entry has a reason: bin, package.json, README SDK, or a consumer that imports it today", () => {
  const binKeys = Object.values(pkg.bin).map((target) => `./${target}`);
  const importedSubpaths = new Set(importRows.map((row) => `./${row.subpath}`));
  const pathImported = new Set(pathRows.filter((row) => row.exported).map((row) => `./${row.subpath}`));
  for (const { key, klass } of mapRows) {
    if (klass === "bin") assert.ok(binKeys.includes(key), `${key} is not a bin target`);
    else if (klass === "package") assert.equal(key, "./package.json");
    else if (klass === "sdk") {
      assert.ok(readme.includes(key.slice(2)), `${key} is classed sdk but README.md never mentions ${key.slice(2)}`);
      assert.ok(!importedSubpaths.has(key), `${key} has an importer; class it app-import`);
    } else {
      assert.ok(importedSubpaths.has(key) || pathImported.has(key), `${key} is classed app-import but no inventory row imports it`);
    }
  }
  for (const key of binKeys) assert.equal(mapRows.find((row) => row.key === key)?.klass, "bin", `${key} must be classed bin`);
});

test("every subpath the inventory lists resolves through the exports map", () => {
  assert.ok(importRows.length >= 60, `expected the inventory rows, found ${importRows.length}`);
  const subpaths = [...new Set(importRows.map((row) => row.subpath))].sort();
  assert.ok(subpaths.length >= 24, `expected the app-imported subpaths, found ${subpaths.length}`);
  for (const subpath of subpaths) {
    assert.ok(keys.includes(exportKey(subpath)), `${subpath} is imported by ${importRows.find((row) => row.subpath === subpath).consumer} but is not in the exports map`);
    const resolved = fileURLToPath(import.meta.resolve(`@rightkit/release/${subpath}`));
    assert.equal(real(resolved), real(path.join(here, subpath)), subpath);
  }
  assert.deepEqual(importsOutsideMap(importRows, exportsMap), []);
});

test("inventory rows are well formed: a consumer, a repository-relative file and a 1-based line", () => {
  for (const row of [...importRows, ...pathRows]) {
    assert.ok(row.consumer.length > 0, JSON.stringify(row));
    assert.ok(!path.isAbsolute(row.file) && !row.file.split("/").includes(".."), `${row.file} must be repository-relative`);
    assert.ok(Number.isInteger(row.line) && row.line >= 1, `${row.file}:${row.line}`);
  }
});

test("path-access rows say truthfully whether the file is exported", () => {
  assert.ok(pathRows.length > 0);
  for (const row of pathRows) assert.equal(keys.includes(exportKey(row.subpath)), row.exported, `${row.subpath} (${row.consumer})`);
});

// ------------------------------------------------------------------------------------------------------------ the scanner

test("the scanner reads complete quoted specifiers and nothing else", () => {
  const found = importsInText([
    'import { a } from "@rightkit/release/cargo-target.mjs";',
    "const m = await import('@rightkit/release/development.mjs');",
    'const file = fileURLToPath(import.meta.resolve("@rightkit/release/sign-windows.mjs"));',
    'createRequire(root).resolve("@rightkit/release/cli/right-release.mjs");',
    '        "@rightkit/release/direct-bootstrap.mjs",',
    "// import x from '@rightkit/release/not-a-real-import.mjs'",
    " * @rightkit/release/also-a-comment.mjs",
    "`@rightkit/release/prose-in-backticks-but-unclosed.mjs",
    "const dynamic = `@rightkit/release/${name}.mjs`;",
    'const dep = "@rightkit/release";',
    'const bare = "@rightkit/release/";',
  ].join("\n"));
  assert.deepEqual(found.map((entry) => [entry.line, entry.subpath]), [
    [1, "cargo-target.mjs"],
    [2, "development.mjs"],
    [3, "sign-windows.mjs"],
    [4, "cli/right-release.mjs"],
    [5, "direct-bootstrap.mjs"],
  ]);
});

test("the scanner classifies how a specifier is used", () => {
  assert.equal(formOf('import { a } from "@rightkit/release/x.mjs";'), "static import");
  assert.equal(formOf('const { a } = await import("@rightkit/release/x.mjs");'), "dynamic import");
  assert.equal(formOf('import.meta.resolve("@rightkit/release/x.mjs")'), "import.meta.resolve");
  assert.equal(formOf('createRequire(a).resolve("@rightkit/release/x.mjs")'), "require.resolve");
  assert.equal(formOf('        "@rightkit/release/x.mjs",', ".rs"), "module name handed to node");
  assert.equal(formOf("import('@rightkit/release/x.mjs').then((m) => {", ".rs"), "dynamic import in a node snippet");
  assert.equal(formOf('import { a } from "@rightkit/release/x.mjs";', ".sh"), "static import in a node snippet");
});

test("the scanner reports path accesses by string, path.join segments and Rust joins", () => {
  const found = pathAccessesInText([
    'const a = path.join(root, "node_modules", "@rightkit", "release", "cli", "right-release.mjs");',
    'const b = "node_modules/@rightkit/release/upload-release.mjs";',
    'repository_root.join("node_modules").join("@rightkit").join("release").join("cli").join("right-release.mjs")',
    "// path.join(root, 'node_modules', '@rightkit', 'release', 'comment.mjs')",
  ].join("\n"));
  assert.deepEqual(found.map((entry) => [entry.line, entry.subpath]), [
    [2, "upload-release.mjs"],
    [1, "cli/right-release.mjs"],
    [3, "cli/right-release.mjs"],
  ]);
});

test("a deep import outside the map is reported, one inside is not", () => {
  const inside = [{ file: "a.mjs", line: 1, subpath: "cargo-target.mjs" }];
  const outside = [{ file: "b.mjs", line: 2, subpath: "signing-agent-core.mjs" }, { file: "c.mjs", line: 3, subpath: "cargo-target.mjs/../release.mjs" }];
  assert.deepEqual(importsOutsideMap(inside, exportsMap), []);
  assert.deepEqual(importsOutsideMap([...inside, ...outside], exportsMap), outside);
  assert.equal(importsOutsideMap([{ file: "x", line: 1, subpath: "release.mjs" }], exportsMap).length, 1);
});

// --------------------------------------------------------------------- scans of the trees that import the package today

// RightKit's own tooling imports the package by name too: the CI workflow templates and @rightkit/legal. The app template no longer does:
// its release config is JSON (M24) and its notarize script is gone (rightkit#1089).
test("nothing in this repository imports a subpath outside the map", () => {
  const { imports } = scanTree(repoRoot);
  // Tests, the docs history and the packed-consumer recipes (which name internal modules to prove the map refuses them) are not consumers.
  const own = imports.filter((entry) => !/(?:^|\/)[^/]*\.test\.mjs$/.test(entry.file) && !/^(?:docs|release-evidence|ci\/consumers)\//.test(entry.file));
  assert.ok(own.length >= 3, `expected the CI template and legal imports, found ${own.length}`);
  assert.deepEqual(importsOutsideMap(own, exportsMap).map((entry) => `${entry.file}:${entry.line} ${entry.subpath}`), []);
  for (const required of ["packages/ci/templates/publish-npm.yml", "packages/legal/bin/rust-licenses.mjs"]) {
    assert.ok(own.some((entry) => entry.file === required), `${required} is expected to import the package`);
  }
});

const workspace = path.resolve(process.env.RIGHT_SUITE_CONTRACT_WORKSPACE ?? fileURLToPath(new URL("../../..", import.meta.url)));
const isolatedCheckout = process.env.RIGHTKIT_ISOLATED_CHECKOUT === "1" || process.env.RK_ISOLATED_CHECKOUT === "1";
// Product checkouts sit beside rightkit in the owner's workspace; a hosted CI checkout holds rightkit alone. A missing checkout
// skips with the reason; RIGHTKIT_REQUIRE_ASSETS=1 runs it anyway, so the missing path fails (same switch as right-suite-contract).
const checkoutSkip = (dir) => {
  if (process.env.RIGHTKIT_REQUIRE_ASSETS === "1") return false;
  if (isolatedCheckout) return `${dir} is not scanned in an isolated checkout`;
  return existsSync(path.join(workspace, dir)) ? false : `${dir} is not checked out beside rightkit`;
};
const knownReadsNotExported = new Set(pathRows.filter((row) => !row.exported).map((row) => row.subpath));
const CHECKOUTS = ["coderight", "cutright", "genright", "heardright", "mailright", "postright", "pulse", "scraperight", "viewright", "sellright", "legion", "orthic-labs/ember"];

for (const dir of CHECKOUTS) {
  test(`${dir}: every deep import of @rightkit/release is a subpath the exports map lists`, { skip: checkoutSkip(dir) }, (t) => {
    const root = path.join(workspace, dir);
    assert.ok(existsSync(root), `${root} is missing`);
    const { imports, pathAccesses } = scanTree(root);
    const outside = importsOutsideMap(imports, exportsMap);
    assert.deepEqual(outside.map((entry) => `${entry.file}:${entry.line} ${entry.subpath}`), [],
      `${dir} imports subpath(s) the map does not export. Add the module to the supported surface (an inventory row in docs/RELEASE-EXPORTS.md and a package.json entry) or move the code off it`);
    // A file reached by path is not governed by the map; the unexported ones must be the reads the inventory names.
    const unknownReads = pathAccesses.filter((entry) => !keys.includes(exportKey(entry.subpath)) && !knownReadsNotExported.has(entry.subpath));
    assert.deepEqual(unknownReads.map((entry) => `${entry.file}:${entry.line} ${entry.subpath}`), [],
      `${dir} reaches into node_modules/@rightkit/release by path; list the access under "Path accesses" in docs/RELEASE-EXPORTS.md`);
    // Drift is information, not a failure: the inventory is a baseline of the day it was cut.
    const listed = new Set(importRows.filter((row) => row.consumer === path.basename(dir)).map((row) => `${row.file}:${row.line}`));
    const unlisted = imports.filter((entry) => !listed.has(`${entry.file}:${entry.line}`));
    if (unlisted.length > 0) t.diagnostic(`${dir}: ${unlisted.length} import(s) moved since docs/RELEASE-EXPORTS.md was cut (all still inside the map)`);
  });
}

// ------------------------------------------------------------------------------------------------- import side effects

test("importing an exported module in a fresh process does nothing; a program (bin target or skipImport) is spawned, never imported", () => {
  // The publish verify imports every exported module. 0.6.6's first verify (run 40168412670) imported cli/right-release.mjs and
  // sign-windows.mjs, which read process.argv at import: the CLI ran with the plan file as its command and the signer refused.
  // Bin targets are resolve-only by rule in scripts/ci/consume.mjs (their smoke run executes them); any other program is
  // listed in rightkit.publishCheck.skipImport, and that list stays honest: an entry that imports quietly is stale.
  const bins = new Set(Object.values(pkg.bin).map((target) => `./${target}`));
  const skipImport = pkg.rightkit?.publishCheck?.skipImport ?? [];
  for (const entry of skipImport) assert.ok(keys.includes(entry), `skipImport names ${entry}, which the exports map does not list`);
  const env = { ...process.env, NODE_OPTIONS: "", NODE_ENV: "production" };
  for (const key of keys) {
    if (!/\.[cm]?js$/.test(key) || bins.has(key)) continue;
    const url = pathToFileURL(path.join(here, key.slice(2))).href;
    const ran = spawnSync(process.execPath, ["--input-type=module", "-e", `await import(${JSON.stringify(url)}); process.stdout.write("RK_IMPORTED");`], { encoding: "utf8", env, timeout: 30_000, input: "" });
    const quiet = ran.status === 0 && ran.stdout === "RK_IMPORTED" && ran.stderr === "";
    const shown = `${ran.stdout}${ran.stderr}`.replace("RK_IMPORTED", "").trim().slice(0, 200);
    if (skipImport.includes(key)) assert.ok(!quiet, `${key} is listed in rightkit.publishCheck.skipImport but imports quietly: it is a module, not a program; drop the entry`);
    else assert.ok(quiet, `${key} does something at import (status ${ran.status}): ${shown || "(no output)"}. A program that reads its command line is spawned by path, never imported: list it in rightkit.publishCheck.skipImport so the publish import check only resolves it`);
  }
});
