// The source gate: cheap deterministic refusals run before any compile, signing, pack or native runner.
// These tests replay the defects that reached a native verify on 2026-10-25 and pin where the checks sit in the workflows.
import assert from "node:assert/strict";
import fs, { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { checkPackage, main, runtimeTargets } from "./exports-import-effects.mjs";
import { RULES } from "../preflight.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const read = (f) => fs.readFileSync(path.join(REPO, f), "utf8");
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-source-gate-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const write = (root, file, text) => { fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true }); fs.writeFileSync(path.join(root, file), text); };

test("G27 feed-legacy: a published version dropped from legacy while its successor is staged is refused (the #1188 defect)", (t) => {
  const rule = RULES.find((r) => r.id === "feed-legacy");
  assert.ok(rule, "preflight has the feed-legacy rule");
  const root = tmp(t);
  const feed = (legacy) => JSON.stringify({ registry: { npm: { "@rightkit/qa-chrome": "0.1.11" }, cargo: {} }, stagedNpm: { "@rightkit/qa-chrome": "0.1.12" }, legacyNpm: { "@rightkit/qa-chrome": legacy } });
  write(root, "packages/release/rightkit-versions.json", feed(["0.1.10"]));
  const rk = ["scripts/preflight.mjs"];
  const bad = rule.run({ root, files: rk });
  assert.equal(bad.length, 1);
  assert.match(bad[0].problem, /@rightkit\/qa-chrome 0\.1\.11 is published .* 0\.1\.12 is staged .* does not list 0\.1\.11/);
  write(root, "packages/release/rightkit-versions.json", feed(["0.1.10", "0.1.11"]));
  assert.deepEqual(rule.run({ root, files: rk }), []);
  // A feed that cannot be read is a failure, never a clean pass (simulation A4.05); an app without a feed is not checked.
  write(root, "packages/release/rightkit-versions.json", "{not json");
  assert.match(rule.run({ root, files: rk })[0]?.problem ?? "", /missing or not valid JSON/);
  assert.deepEqual(rule.run({ root, files: ["package.json"] }), []);
});

test("exports-import-effects: wildcard exports are expanded and probed; a missing non-dist target is a finding; dist output is printed as not checked (simulation A5.06/A5.07)", (t) => {
  const dir = tmp(t);
  write(dir, "src/quiet.mjs", "export const q = 1;\n");
  write(dir, "src/loud.mjs", "console.log('ran at import');\n");
  write(dir, "package.json", JSON.stringify({ name: "@rk/wild", type: "module", exports: { "./*": "./src/*.mjs", "./gone": "./lib/gone.mjs", ".": "./dist/index.js" } }));
  const r = checkPackage(dir);
  assert.deepEqual(r.findings.map((f) => f.key).sort(), ["./gone", "./loud"], JSON.stringify(r.findings));
  assert.match(r.findings.find((f) => f.key === "./loud").problem, /ran at import/);
  assert.match(r.findings.find((f) => f.key === "./gone").problem, /does not exist in the source tree and is not build output/);
  assert.equal(r.checked, 2, "both wildcard matches were imported");
  assert.deepEqual(r.notChecked.map((n) => n.key), ["."]);
  write(dir, "package.json", JSON.stringify({ name: "@rk/wild", type: "module", exports: { "./none/*": "./nowhere/*.mjs" } }));
  assert.match(checkPackage(dir).findings[0]?.problem ?? "", /matches no file/);
  // main() prints every not-checked export and exits 0 when that is all there is.
  write(dir, "package.json", JSON.stringify({ name: "@rk/wild", type: "module", exports: { ".": "./dist/index.js" } }));
  const io = { logs: [], errs: [] };
  assert.equal(main([dir], { log: (m) => io.logs.push(m), error: (m) => io.errs.push(m) }), 0);
  assert.ok(io.logs.some((l) => /not checked here: @rk\/wild \.: \.\/dist\/index\.js/.test(l)), io.logs.join("\n"));
  assert.ok(io.logs.at(-1).includes("1 build-output export(s) left to the sealed-byte check"));
});

test("exports-import-effects: an import that writes a file or starts a process without printing anything is refused by the read-only probe (simulation A5.07b)", (t) => {
  const dir = tmp(t);
  const marker = path.join(dir, "marker");
  write(dir, "silent.mjs", `import fs from "node:fs"; fs.writeFileSync(${JSON.stringify(marker)}, "x"); export {};\n`);
  write(dir, "spawner.mjs", "import { spawnSync } from \"node:child_process\"; spawnSync(process.execPath, [\"-e\", \"0\"]); export {};\n");
  write(dir, "reader.mjs", "import fs from \"node:fs\"; export const own = fs.readFileSync(new URL(import.meta.url)).length;\n");
  write(dir, "package.json", JSON.stringify({ name: "@rk/effects", type: "module", exports: { "./silent": "./silent.mjs", "./spawner": "./spawner.mjs", "./reader": "./reader.mjs" } }));
  const r = checkPackage(dir);
  assert.deepEqual(r.findings.map((f) => f.key).sort(), ["./silent", "./spawner"], JSON.stringify(r.findings));
  assert.match(r.findings.find((f) => f.key === "./silent").problem, /restricted|allow-fs-write/);
  assert.equal(existsSync(marker), false, "the probe never let the write happen");
  assert.equal(r.checked, 3, "reading its own source at import is allowed");
});

test("exports-import-effects: a module that reads its command line at import is refused; a guarded one, a bin and a skipImport program pass", (t) => {
  const dir = tmp(t);
  write(dir, "quiet.mjs", "export const x = 1;\n");
  write(dir, "program.mjs", "if (!process.argv[2]) { console.error('usage: program <file>'); process.exit(2); }\n");
  write(dir, "cli.mjs", "console.log('help'); process.exit(2);\n");
  write(dir, "signer.mjs", "console.error('must run on Windows'); process.exit(1);\n");
  const pkg = (skip) => JSON.stringify({ name: "@rk/fixture", type: "module", bin: { cli: "cli.mjs" }, exports: { "./quiet.mjs": "./quiet.mjs", "./program.mjs": "./program.mjs", "./cli.mjs": "./cli.mjs", "./signer.mjs": "./signer.mjs", "./dist/built.js": "./dist/built.js" }, rightkit: { publishCheck: { skipImport: skip } } });
  write(dir, "package.json", pkg(["./signer.mjs"]));
  const r = checkPackage(dir);
  assert.deepEqual(r.findings.map((f) => f.key), ["./program.mjs"], JSON.stringify(r.findings));
  assert.match(r.findings[0].problem, /usage: program/);
  assert.deepEqual(r.notChecked.map((n) => n.key), ["./dist/built.js"], "build output is left to the sealed-byte import check, never silently passed");
  write(dir, "package.json", pkg(["./signer.mjs", "./quiet.mjs"]));
  assert.ok(checkPackage(dir).findings.some((f) => f.key === "./quiet.mjs" && /imports quietly/.test(f.problem)), "a stale skipImport entry is refused");
  assert.deepEqual(runtimeTargets({ ".": { types: "./a.d.ts", import: "./a.mjs" } }), [{ key: ".", target: "./a.mjs" }]);
});

test("the checks sit before every expensive job: publish.yml scans (before plan, build-swap, pack, verify) and ci.yml hygiene (before every native shard)", () => {
  const publish = read(".github/workflows/publish.yml");
  const scans = publish.slice(publish.indexOf("\n  scans:\n"), publish.indexOf("\n  plan:\n"));
  const order = ["node live/scripts/preflight.mjs", "pnpm install --frozen-lockfile --ignore-scripts", "node live/scripts/ci/exports-import-effects.mjs --root live", "run-deterministic-scans.mjs"].map((s) => scans.indexOf(s));
  assert.ok(order.every((i) => i > 0) && order.every((i, k) => k === 0 || i > order[k - 1]), `scans step order ${order}`);
  for (const job of ["plan", "build-swap", "build-credentials-helper"]) assert.match(publish.slice(publish.indexOf(`\n  ${job}:\n`)), /needs: \[[^\]]*\bscans\b/, `${job} needs scans`);
  const ci = read(".github/workflows/ci.yml");
  const hygiene = ci.slice(ci.indexOf("\n  hygiene:\n"), ci.indexOf("\n  ownership:\n"));
  assert.ok(hygiene.includes("node scripts/ci/exports-import-effects.mjs"));
  assert.ok(ci.indexOf("node scripts/preflight.mjs") < ci.indexOf("\n  shards-windows:\n"), "preflight runs in plan, before the shards");
  for (const job of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) assert.match(ci.slice(ci.indexOf(`\n  ${job}:\n`)), /needs: \[plan, hygiene\]/, `${job} needs plan and hygiene`);
});
