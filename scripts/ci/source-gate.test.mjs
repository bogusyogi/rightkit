// The source gate: cheap deterministic refusals run before any compile, signing, pack or native runner.
// These tests replay the defects that reached a native verify on 2026-10-25 and pin where the checks sit in the workflows.
import assert from "node:assert/strict";
import fs, { existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { checkPackage, closeIncomplete, EXIT_INCOMPLETE, expectedIncomplete, main, readIncompleteReport, runtimeTargets } from "./exports-import-effects.mjs";
import { RULES } from "../preflight.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const read = (f) => fs.readFileSync(path.join(REPO, f), "utf8");
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-source-gate-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const write = (root, file, text) => { fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true }); fs.writeFileSync(path.join(root, file), text); };

test("G27 feed-legacy: a published version dropped from legacy while its successor is staged is refused (the #1188 defect)", (t) => {
  const rule = RULES.find((r) => r.id === "feed-legacy");
  assert.ok(rule, "preflight has the feed-legacy rule");
  const root = tmp(t);
  const feed = (legacy) => JSON.stringify({ registry: { npm: { "@rightkit/qa-chrome": "0.1.11" }, cargo: {} }, stagedNpm: { "@rightkit/qa-chrome": "0.1.12" }, stagedCargo: {}, legacyNpm: { "@rightkit/qa-chrome": legacy }, legacyCargo: {} });
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
  // The shape is checked, not only the JSON: {} and [] and a feed with the wrong field types never pass as "nothing to check".
  for (const [text, why] of [["{}", /registry is missing/], ["[]", /not a JSON object/], [JSON.stringify({ registry: { npm: {} }, stagedNpm: {}, stagedCargo: {}, legacyNpm: {}, legacyCargo: {} }), /registry\.cargo is missing/], [JSON.stringify({ registry: { npm: { "@rightkit/x": 1 }, cargo: {} }, stagedNpm: {}, stagedCargo: {}, legacyNpm: {}, legacyCargo: {} }), /registry\.npm\["@rightkit\/x"\] is not a version string/], [JSON.stringify({ registry: { npm: {}, cargo: {} }, stagedNpm: {}, stagedCargo: {}, legacyNpm: { "@rightkit/x": "0.1.0" }, legacyCargo: {} }), /legacyNpm\["@rightkit\/x"\] is not an array/]]) {
    write(root, "packages/release/rightkit-versions.json", text);
    const out = rule.run({ root, files: rk });
    assert.ok(out.length >= 1 && why.test(out[0].problem), `${text} → ${JSON.stringify(out)}`);
  }
  // The real feed has the shape.
  assert.deepEqual(rule.run({ root: path.resolve(fileURLToPath(import.meta.url), "../../.."), files: rk }), []);
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
  // Build output is INCOMPLETE, not green: without --report main() exits 3; with --report it writes the list the sealed-byte
  // stage must close and says so.
  write(dir, "package.json", JSON.stringify({ name: "@rk/wild", type: "module", exports: { ".": "./dist/index.js" } }));
  const io = { logs: [], errs: [] };
  assert.equal(main([dir], { log: (m) => io.logs.push(m), error: (m) => io.errs.push(m) }), EXIT_INCOMPLETE);
  assert.ok(io.logs.some((l) => /INCOMPLETE: @rk\/wild \. \(\.\/dist\/index\.js\)/.test(l)), io.logs.join("\n"));
  assert.match(io.logs.at(-1), /INCOMPLETE \(no --report/);
  const reportFile = path.join(dir, "out", "report.json"); const io2 = { logs: [], errs: [] };
  assert.equal(main(["--root", dir, "--report", reportFile, dir], { log: (m) => io2.logs.push(m), error: (m) => io2.errs.push(m) }), 0);
  assert.match(io2.logs.at(-1), /INCOMPLETE \(handed to the sealed-byte check\)/);
  // Outside a git checkout the probe can only run on the working directory: the report says so and is unusable downstream.
  const raw = JSON.parse(fs.readFileSync(reportFile, "utf8"));
  assert.deepEqual(raw.incomplete.map((r) => [r.package, r.key, r.target]), [["@rk/wild", ".", "./dist/index.js"]]);
  assert.equal(raw.commit, null); assert.equal(raw.closure.kind, "working-dir"); assert.match(readIncompleteReport(reportFile).problem, /no checkout commit\/tree recorded/);
  assert.match(raw.probe, /^[0-9a-f]{64}$/); assert.equal(raw.node, process.version);
  const SHA40 = "a".repeat(40); const D64 = "d".repeat(64);
  const good = { schema: "rightkit.import-effects/1", commit: SHA40, tree: "b".repeat(40), clean: true, dirty: 0, probe: raw.probe, node: raw.node, stable: true, closure: { kind: "exported-tree", deps: { lock: D64, installed: D64, linked: ["."] } }, checked: 0, findings: 0, incomplete: raw.incomplete, covered: [] };
  const rf = (patch, expect = {}) => { write(dir, "r.json", JSON.stringify({ ...good, ...patch })); return readIncompleteReport(path.join(dir, "r.json"), { commit: SHA40, tree: "b".repeat(40), probe: raw.probe, node: raw.node, ...expect }); };
  assert.ok(rf({}).report, "positive control: exact closure, trusted probe and qualified Node");
  // Source closure: working-directory probes, a HEAD that moved, an unrecorded dependency closure, another tree: all unbound, all refused.
  assert.match(rf({ closure: { kind: "working-dir", deps: null } }).problem, /ran on a working directory, not on the exported commit/);
  assert.match(rf({ stable: false }).problem, /HEAD moved while the probe ran/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { lock: null, installed: D64 } } }).problem, /dependency closure .* not recorded/);
  assert.match(rf({ tree: null }).problem, /no checkout commit\/tree recorded/);
  assert.match(rf({ tree: "c".repeat(40) }).problem, /written for tree c{40}, not this checkout's b{40}/);
  // Trusted identities: the probe digest and the Node version are compared to what the verifier trusts, not merely well-formed.
  assert.match(rf({ probe: "short" }).problem, /no probe code digest/);
  assert.match(rf({ probe: "e".repeat(64) }).problem, /written by probe eeeeeeeeeeee…, not the trusted/);
  assert.match(rf({ node: "v20.0.0" }).problem, /written under Node v20\.0\.0, not the qualified/);
  assert.ok(rf({}, { probe: null, node: null }).report, "a verifier that passes no expectation only checks shape");
  const read = rf({}); assert.ok(read.report, JSON.stringify(read));
  assert.match(readIncompleteReport(path.join(dir, "missing.json")).problem, /cannot be read/);
  // Residuals from the slice 3 review: a red probe, bad counts, malformed rows and a stale commit are all unusable.
  assert.match(rf({ findings: 1 }).problem, /findings is 1/);
  assert.match(rf({ checked: 3 }).problem, /checked \(3\) must equal/);
  assert.match(rf({ checked: -1 }).problem, /checked/);
  assert.match(rf({ incomplete: [{ key: 1 }] }).problem, /incomplete rows must be/);
  assert.match(rf({ incomplete: [{ package: "@rk/wild", key: ".", target: "dist/index.js" }] }).problem, /incomplete rows must be/);
  assert.match(rf({ covered: "x" }).problem, /covered rows must be/);
  assert.match(rf({ schema: "other" }).problem, /wrong schema/);
  assert.match(rf({ commit: "0".repeat(40) }).problem, /written for commit 0{40}, not the dispatched a{40}/);
  // The closure: a sealed-byte import check that imported the export closes it; one that did not leaves a refusal.
  assert.deepEqual(closeIncomplete({ report: read.report, name: "@rk/wild", importedSpecs: ["@rk/wild"] }), []);
  assert.match(closeIncomplete({ report: read.report, name: "@rk/wild", importedSpecs: ["@rk/wild/other"] })[0], /INCOMPLETE not closed: @rk\/wild \(\.\/dist\/index\.js\)/);
  assert.deepEqual(closeIncomplete({ report: { incomplete: [{ package: "@rk/other", key: "./x", target: "./dist/x.js" }] }, name: "@rk/wild", importedSpecs: [] }), [], "another package's rows are not this unit's");
  // Forged COMPLETE: a report that accounts for nothing although the sealed package.json exports build output is refused.
  const sealedPkg = { name: "@rk/wild", exports: { ".": "./dist/index.js", "./x": "./dist/x.js", "./src": "./src/s.mjs" } };
  assert.deepEqual(expectedIncomplete(sealedPkg).map((e) => [e.key, e.pattern]), [[".", false], ["./x", false]]);
  const specs = ["@rk/wild", "@rk/wild/x"];
  const forged = closeIncomplete({ report: { incomplete: [], covered: [] }, name: "@rk/wild", importedSpecs: specs, expected: expectedIncomplete(sealedPkg) });
  assert.equal(forged.length, 2); assert.match(forged[0], /accounts for neither an INCOMPLETE nor a covered \. \(\.\/dist\/index\.js\)/);
  const rows = [{ package: "@rk/wild", key: ".", target: "./dist/index.js" }, { package: "@rk/wild", key: "./x", target: "./dist/x.js" }];
  assert.deepEqual(closeIncomplete({ report: { incomplete: rows, covered: [] }, name: "@rk/wild", importedSpecs: specs, expected: expectedIncomplete(sealedPkg) }), [], "every row imported from the tarball closes");
  // Positive control (review residual): a pre-built dist the probe imported itself is a covered row, not a false reject.
  assert.deepEqual(closeIncomplete({ report: { incomplete: [rows[1]], covered: [rows[0]] }, name: "@rk/wild", importedSpecs: specs, expected: expectedIncomplete(sealedPkg) }), []);
  // Target binding: a row whose target differs from the sealed package's is not this candidate's.
  assert.match(closeIncomplete({ report: { incomplete: [{ ...rows[0], target: "./dist/other.js" }, rows[1]], covered: [] }, name: "@rk/wild", importedSpecs: specs, expected: expectedIncomplete(sealedPkg) })[0], /binds \. to \.\/dist\/other\.js, the sealed package to \.\/dist\/index\.js/);
  // Exact set: duplicate rows, a key in both lists, and a row for a dist export the sealed package does not declare are refused.
  const dup = closeIncomplete({ report: { incomplete: [rows[0], rows[0], rows[1]], covered: [] }, name: "@rk/wild", importedSpecs: specs, expected: expectedIncomplete(sealedPkg) });
  assert.equal(dup.length, 1); assert.match(dup[0], /lists \. twice/);
  const conflict = closeIncomplete({ report: { incomplete: rows, covered: [rows[0]] }, name: "@rk/wild", importedSpecs: specs, expected: expectedIncomplete(sealedPkg) });
  assert.match(conflict[0], /lists \. as both covered .* and incomplete/);
  const extra = closeIncomplete({ report: { incomplete: [...rows, { package: "@rk/wild", key: "./ghost", target: "./dist/ghost.js" }], covered: [] }, name: "@rk/wild", importedSpecs: [...specs, "@rk/wild/ghost"], expected: expectedIncomplete(sealedPkg) });
  assert.match(extra[0], /names \.\/ghost \(\.\/dist\/ghost\.js\), which the sealed package does not export from build output/);
  // Wildcard build output can never be closed (neither side imports a pattern): refused, not silently passed.
  const wild = { name: "@rk/wild", exports: { "./*": "./dist/*.js" } };
  assert.deepEqual(expectedIncomplete(wild), [{ key: "./*", target: "./dist/*.js", pattern: true }]);
  assert.match(closeIncomplete({ report: { incomplete: [], covered: [] }, name: "@rk/wild", importedSpecs: [], expected: expectedIncomplete(wild) })[0], /wildcard export \.\/\* → \.\/dist\/\*\.js points at build output; .* cannot be closed/);
});

test("exports-import-effects: the probe runs on the exported commit, so ignored build output, local config, untracked files and a mid-probe edit never reach it; a HEAD that moves during the probe is recorded as unstable", (t) => {
  const repo = tmp(t);
  const git = (...a) => execFileSync("git", ["-C", repo, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", ...a], { encoding: "utf8" });
  git("init", "-q");
  write(repo, "packages/a/package.json", JSON.stringify({ name: "@rk/a", type: "module", exports: { ".": "./dist/index.js", "./src": "./src/s.mjs" } }));
  write(repo, "packages/a/src/s.mjs", "export const s = 1;\n");
  write(repo, ".gitignore", "dist/\nnode_modules/\n.local.json\n");
  write(repo, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
  git("add", "-A"); git("commit", "-qm", "base");
  // Ignored local state that a working-directory probe would have seen: a locally built dist, a local config file, an untracked module.
  write(repo, "packages/a/dist/index.js", "console.log('local build ran at import');\n");
  write(repo, "packages/a/.local.json", "{}");
  write(repo, "packages/a/src/untracked.mjs", "console.log('untracked');\n");
  fs.mkdirSync(path.join(repo, "node_modules"), { recursive: true }); write(repo, "node_modules/.modules.yaml", "layoutVersion: 5\n");
  const reportFile = path.join(repo, "out", "report.json"); const io = { logs: [], errs: [] };
  // A tracked file is edited in the live checkout in the middle of the probe: the export is immutable, the identity stays stable.
  const between = () => write(repo, "packages/a/src/s.mjs", "export const s = 2; console.log('edited mid-probe');\n");
  assert.equal(main(["--root", repo, "--report", reportFile], { log: (m) => io.logs.push(m), error: (m) => io.errs.push(m) }, { between }), 0, io.errs.join("\n"));
  const r = JSON.parse(fs.readFileSync(reportFile, "utf8"));
  assert.equal(r.closure.kind, "exported-tree"); assert.equal(r.stable, true); assert.equal(r.findings, 0);
  assert.deepEqual(r.incomplete.map((x) => x.key), ["."], "the locally built dist is not in the commit, so '.' is INCOMPLETE (not covered by local build output)");
  assert.deepEqual(r.covered.map((x) => x.key), ["./src"]);
  assert.match(r.closure.deps.lock, /^[0-9a-f]{64}$/); assert.match(r.closure.deps.installed, /^[0-9a-f]{64}$/); assert.deepEqual(r.closure.deps.linked, ["."]);
  assert.equal(r.clean, false, "the live checkout is dirty (untracked files), recorded but irrelevant to the exported probe");
  assert.ok(readIncompleteReport(reportFile, { commit: r.commit, tree: r.tree, probe: r.probe, node: r.node }).report, "usable: bound to the exported commit");
  // A repository without a packages/ directory still exports (explicit package dirs are resolved inside the export).
  const flat = tmp(t); const gitf = (...a) => execFileSync("git", ["-C", flat, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", ...a], { encoding: "utf8" });
  gitf("init", "-q"); write(flat, "package.json", JSON.stringify({ name: "@rk/flat", type: "module", exports: { ".": "./lib.mjs" } })); write(flat, "lib.mjs", "export const f = 1;\n"); gitf("add", "-A"); gitf("commit", "-qm", "flat");
  const flatReport = path.join(flat, "out", "r.json"); const io3 = { logs: [], errs: [] };
  assert.equal(main(["--root", flat, "--report", flatReport, "."], { log: (m) => io3.logs.push(m), error: (m) => io3.errs.push(m) }), 0, io3.errs.join("\n"));
  assert.equal(JSON.parse(fs.readFileSync(flatReport, "utf8")).closure.kind, "exported-tree");
  // Now the mid-probe action commits: HEAD moves during the probe → unstable → refused.
  const io2 = { logs: [], errs: [] };
  const commitBetween = () => { write(repo, "packages/a/src/s.mjs", "export const s = 3;\n"); git("add", "-A"); git("commit", "-qm", "moved"); };
  assert.equal(main(["--root", repo, "--report", reportFile], { log: (m) => io2.logs.push(m), error: (m) => io2.errs.push(m) }, { between: commitBetween }), 0);
  const r2 = JSON.parse(fs.readFileSync(reportFile, "utf8"));
  assert.equal(r2.stable, false); assert.match(readIncompleteReport(reportFile).problem, /HEAD moved while the probe ran/);
  assert.ok(io2.logs.some((l) => /HEAD moved during the probe/.test(l)));
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
  // File-only class A (preflight, run-deterministic-scans) first; the probe imports candidate code, so it comes after.
  const order = ["node live/scripts/preflight.mjs", "pnpm install --frozen-lockfile --ignore-scripts", "run-deterministic-scans.mjs", "node live/scripts/ci/exports-import-effects.mjs --root live --report"].map((s) => scans.indexOf(s));
  // INCOMPLETE exports travel to verify as an artifact and are required there (RK_IMPORT_EFFECTS_REPORT): no green by omission.
  assert.ok(scans.includes("name: import-effects-${{ github.run_attempt }}"), "scans uploads the import-effects report");
  for (const job of ["verify", "verify-linux"]) {
    const body = publish.slice(publish.indexOf(`\n  ${job}:\n`));
    const section = body.slice(0, body.indexOf("\n  ", 1) === -1 ? undefined : body.indexOf("\n  qualified:") > 0 && job === "verify-linux" ? body.indexOf("\n  qualified:") : body.indexOf("\n  verify-linux:") > 0 ? body.indexOf("\n  verify-linux:") : undefined);
    assert.ok(section.includes("name: import-effects-${{ github.run_attempt }}") && section.indexOf("import-effects-${{ github.run_attempt }}") < section.indexOf("publish.mjs verify"), `${job} downloads the report before verify`);
    assert.ok(section.includes("RK_IMPORT_EFFECTS_REPORT: ${{ runner.temp }}/import-effects/report.json"), `${job} hands the report to publish.mjs verify`);
  }
  assert.ok(order.every((i) => i > 0) && order.every((i, k) => k === 0 || i > order[k - 1]), `scans step order ${order}`);
  for (const job of ["plan", "build-swap", "build-credentials-helper"]) assert.match(publish.slice(publish.indexOf(`\n  ${job}:\n`)), /needs: \[[^\]]*\bscans\b/, `${job} needs scans`);
  const ci = read(".github/workflows/ci.yml");
  const hygiene = ci.slice(ci.indexOf("\n  hygiene:\n"), ci.indexOf("\n  ownership:\n"));
  assert.ok(hygiene.includes("node scripts/ci/exports-import-effects.mjs --report"), "hygiene writes the report (ci.yml has no sealed-byte stage; publish verify closes it)");
  assert.ok(hygiene.indexOf("--steps scans") < hygiene.indexOf("exports-import-effects.mjs --report"), "hygiene runs the file-only class A scans before the probe imports candidate code");
  assert.ok(hygiene.includes("name: import-effects-hygiene-${{ github.run_attempt }}"));
  assert.ok(ci.indexOf("node scripts/preflight.mjs") < ci.indexOf("\n  shards-windows:\n"), "preflight runs in plan, before the shards");
  for (const job of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) assert.match(ci.slice(ci.indexOf(`\n  ${job}:\n`)), /needs: \[plan, hygiene\]/, `${job} needs plan and hygiene`);
});
