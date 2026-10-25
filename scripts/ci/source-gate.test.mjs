// The source gate: cheap deterministic refusals run before any compile, signing, pack or native runner.
// These tests replay the defects that reached a native verify on 2026-10-25 and pin where the checks sit in the workflows.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs, { existsSync } from "node:fs";
import { execFileSync, spawn } from "node:child_process";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { checkPackage, checkoutIdentity, closeIncomplete, EXIT_INCOMPLETE, expectedIncomplete, exportTree, hashInstalled, hashTree, INSTALL_COMMAND, installEnvironment, main, normalizeInstalledMetadata, packageManagerPin, pnpmIdentity, readIncompleteReport, runtimeTargets } from "./exports-import-effects.mjs";
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
  // main() exports THIS checkout (its default root), whose package.json pins a pnpm; a host whose pnpm on PATH is another version
  // is refused before any install (exit 2, the pin problem) — the host-bound negative control — and the INCOMPLETE path is then
  // exercised against a fixture root that pins nothing.
  const hostPnpm = pnpmIdentity(); const pin = packageManagerPin(REPO);
  if (pin && hostPnpm.version !== pin) {
    assert.equal(main([dir], { log: (m) => io.logs.push(m), error: (m) => io.errs.push(m) }), 2);
    assert.match(io.errs.join("\n"), new RegExp(`pins pnpm@${pin.replaceAll(".", "\\.")} .* but the pnpm on PATH is ${hostPnpm.version.replaceAll(".", "\\.")}`));
    t.diagnostic(`host pnpm ${hostPnpm.version} ≠ repository pin ${pin}: the real-checkout export is refused on this host (host-bound); INCOMPLETE path exercised on a fixture root`);
    io.logs.length = 0; io.errs.length = 0;
  }
  const { repo: plainRoot, git: plainGit } = gitRepoFixture(t); const fixtureRootArgs = ["--root", plainRoot];
  fs.cpSync(dir, path.join(plainRoot, "packages", "wild"), { recursive: true });
  execFileSync("pnpm", ["install", "--lockfile-only", "--ignore-scripts", "--ignore-pnpmfile", "--offline"], { cwd: plainRoot, encoding: "utf8", env: { ...process.env, CI: "1" } });
  plainGit("add", "-A"); plainGit("commit", "-qm", "wild");
  assert.equal(main([...fixtureRootArgs, path.join(plainRoot, "packages", "wild")], { log: (m) => io.logs.push(m), error: (m) => io.errs.push(m) }), EXIT_INCOMPLETE, io.errs.join("\n"));
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
  const DEPS = { lock: D64, installed: D64, store: D64, files: 3, hardlinks: 0, externalLinks: 0, pnpm: "11.25.0", install: INSTALL_COMMAND, pnpmIdentity: { path: "/usr/bin/pnpm", realPath: "/opt/pnpm/bin/pnpm.cjs", sha256: D64, version: "11.25.0", node: process.version }, closureStable: true };
  const good = { schema: "rightkit.import-effects/1", commit: SHA40, tree: "b".repeat(40), clean: true, dirty: 0, probe: raw.probe, node: raw.node, stable: true, closure: { kind: "exported-tree", deps: DEPS }, checked: 0, findings: 0, incomplete: raw.incomplete, covered: [] };
  const rf = (patch, expect = {}) => { write(dir, "r.json", JSON.stringify({ ...good, ...patch })); return readIncompleteReport(path.join(dir, "r.json"), { commit: SHA40, tree: "b".repeat(40), probe: raw.probe, node: raw.node, ...expect }); };
  assert.ok(rf({}).report, "positive control: exact closure, trusted probe and qualified Node");
  // Source closure: working-directory probes, a HEAD that moved, an unrecorded dependency closure, another tree: all unbound, all refused.
  assert.match(rf({ closure: { kind: "working-dir", deps: null } }).problem, /ran on a working directory, not on the exported commit/);
  assert.match(rf({ stable: false }).problem, /HEAD moved while the probe ran/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, lock: null } } }).problem, /dependency closure .* not recorded/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, store: null } } }).problem, /dependency closure .* not recorded/, "metadata alone (lockfile, installed-state) is not a dependency closure: the installed bytes digest is required");
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, hardlinks: 2 } } }).problem, /not private copies \(2 hardlinked/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, externalLinks: 1 } } }).problem, /1 link\(s\) leaving the export/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, install: "pnpm install" } } }).problem, /not installed frozen, offline, without scripts or pnpm hooks, as private copies/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, install: INSTALL_COMMAND.replace(" --ignore-pnpmfile", "") } } }).problem, /without scripts or pnpm hooks/, "an install that left pnpm hooks enabled is refused");
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, pnpmIdentity: null } } }).problem, /pnpm that installed the dependencies is not identified/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, pnpmIdentity: { ...DEPS.pnpmIdentity, version: "9.0.0" } } } }).problem, /not identified/, "the identity's version must be the recorded pnpm version");
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, closureStable: false } } }).problem, /installed bytes changed between their digest and the end of the probe/);
  assert.match(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, closureStable: null } } }).problem, /not re-digested/);
  assert.ok(rf({ closure: { kind: "exported-tree", deps: { ...DEPS, installed: null, files: 0 } } }).report, "a tree with nothing to install has no installed-state file");
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

const PROBE_CLI = fileURLToPath(new URL("./exports-import-effects.mjs", import.meta.url));
function gitRepoFixture(t, { packages = { a: { exports: { ".": "./dist/index.js", "./src": "./src/s.mjs" }, files: { "src/s.mjs": "import { b } from \"@rk/b\"; export const s = b;\n" }, deps: { "@rk/b": "workspace:*" } }, b: { exports: { ".": "./src/b.mjs" }, files: { "src/b.mjs": "export const b = 1;\n" } } } } = {}) {
  const repo = tmp(t);
  const git = (...a) => execFileSync("git", ["-C", repo, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", ...a], { encoding: "utf8" });
  git("init", "-q");
  write(repo, "package.json", JSON.stringify({ name: "@rk/fixture-root", private: true, type: "module" }));
  write(repo, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
  for (const [name, p] of Object.entries(packages)) {
    write(repo, `packages/${name}/package.json`, JSON.stringify({ name: `@rk/${name}`, version: "1.0.0", type: "module", exports: p.exports, ...(p.deps ? { dependencies: p.deps } : {}), ...(p.scripts ? { scripts: p.scripts } : {}) }));
    for (const [f, text] of Object.entries(p.files)) write(repo, `packages/${name}/${f}`, text);
  }
  write(repo, ".gitignore", "dist/\nnode_modules/\n.local.json\n");
  // The committed lockfile the exported tree installs from (no third-party dependencies: the install is offline-safe).
  execFileSync("pnpm", ["install", "--lockfile-only", "--ignore-scripts", "--offline"], { cwd: repo, encoding: "utf8", env: { ...process.env, CI: "1" } });
  git("add", "-A"); git("commit", "-qm", "base");
  return { repo, git };
}

test("exports-import-effects: the probe runs on the exported commit with dependencies installed inside it, so ignored build output, local config, untracked files and the live node_modules never reach it; a HEAD that moves during the probe is recorded as unstable", (t) => {
  const { repo, git } = gitRepoFixture(t);
  // Ignored local state a working-directory probe would have seen: a locally built dist, a local config file, an untracked module,
  // and a live node_modules whose workspace link points at a DIRTY package copy.
  write(repo, "packages/a/dist/index.js", "console.log('local build ran at import');\n");
  write(repo, "packages/a/.local.json", "{}");
  write(repo, "packages/a/src/untracked.mjs", "console.log('untracked');\n");
  fs.mkdirSync(path.join(repo, "node_modules", "@rk"), { recursive: true }); fs.symlinkSync(path.join(repo, "packages", "a"), path.join(repo, "node_modules", "@rk", "a"), "dir");
  write(repo, "node_modules/.modules.yaml", "layoutVersion: 5\n");
  const reportFile = path.join(repo, "out", "report.json"); const io = { logs: [], errs: [] };
  const between = () => write(repo, "packages/a/src/s.mjs", "export const s = 2; console.log('edited after this package was probed');\n");
  assert.equal(main(["--root", repo, "--report", reportFile], { log: (m) => io.logs.push(m), error: (m) => io.errs.push(m) }, { between }), 0, io.errs.join("\n"));
  const r = JSON.parse(fs.readFileSync(reportFile, "utf8"));
  assert.equal(r.closure.kind, "exported-tree"); assert.equal(r.stable, true); assert.equal(r.findings, 0);
  assert.deepEqual(r.incomplete.map((x) => x.key), ["."], "the locally built dist is not in the commit: '.' is INCOMPLETE, not covered by local build output");
  assert.deepEqual(r.covered.map((x) => x.key).sort(), [".", "./src"].sort(), "@rk/a's ./src imported @rk/b through the export's own workspace link; @rk/b's '.' is covered");
  for (const k of ["lock", "store"]) assert.match(r.closure.deps[k], /^[0-9a-f]{64}$/, k);
  assert.ok(r.closure.deps.installed === null || /^[0-9a-f]{64}$/.test(r.closure.deps.installed));
  assert.equal(typeof r.closure.deps.pnpm, "string"); assert.ok(r.closure.deps.files >= 1, "installed bytes were hashed");
  assert.equal(r.closure.deps.hardlinks, 0); assert.equal(r.closure.deps.externalLinks, 0, "the workspace link resolves inside the export, never to the live checkout");
  assert.equal(r.closure.deps.install, INSTALL_COMMAND);
  assert.equal(r.clean, false, "the live checkout is dirty, recorded but irrelevant to the exported probe");
  assert.ok(readIncompleteReport(reportFile, { commit: r.commit, tree: r.tree, probe: r.probe, node: r.node }).report, "usable: bound to the exported commit and its installed bytes");
  // A commit scheduled during the probe moves HEAD → unstable → refused.
  const io2 = { logs: [], errs: [] };
  let committed = false;
  const commitBetween = () => { if (committed) return; committed = true; write(repo, "packages/a/src/s.mjs", "export const s = 3;\n"); git("add", "-A"); git("commit", "-qm", "moved"); };
  assert.equal(main(["--root", repo, "--report", reportFile], { log: (m) => io2.logs.push(m), error: (m) => io2.errs.push(m) }, { between: commitBetween }), 0);
  const r2 = JSON.parse(fs.readFileSync(reportFile, "utf8"));
  assert.equal(r2.stable, false); assert.match(readIncompleteReport(reportFile).problem, /HEAD moved while the probe ran/);
  // The exporter refusing (an uninstallable lockfile, a missing commit) is exit 2, never a report.
  const io3 = { logs: [], errs: [] };
  assert.equal(main(["--root", repo, "--report", reportFile], { log: (m) => io3.logs.push(m), error: (m) => io3.errs.push(m) }, { exporter: () => ({ problem: "fixture: store lacks the dependencies" }) }), 2);
  assert.match(io3.errs.join("\n"), /store lacks the dependencies/);
});

test("exports-import-effects: dependency bytes are bound, not metadata: two exports of one commit digest equal; a mutated dependency file inside an export changes the digest while lockfile and installed-state digests stay identical; the live node_modules is never consulted", (t) => {
  const { repo, git } = gitRepoFixture(t);
  const commit = git("rev-parse", "HEAD").trim();
  const one = exportTree(repo, { commit }); const two = exportTree(repo, { commit });
  assert.ok(one.dir && two.dir, JSON.stringify([one.problem, two.problem]));
  assert.equal(one.deps.store, two.deps.store, "dependency BYTES are reproducible across exports of one commit"); assert.equal(one.deps.lock, two.deps.lock);
  // installed-state metadata (pnpm's .modules.yaml / workspace-state) carries timestamps: recorded, never relied on for equality.
  // Mutate one installed file in export two (what a tampered store/hardlink would have done to the probed bytes).
  const target = fs.realpathSync(path.join(two.dir, "packages", "a", "node_modules", "@rk", "b", "src", "b.mjs"));
  assert.ok(target.startsWith(fs.realpathSync(two.dir)), "the workspace dependency's bytes live inside the export");
  fs.appendFileSync(target, "\n// tampered\n");
  const after = hashInstalled(two.dir);
  assert.notEqual(after.sha256, two.deps.store, "content mutation is visible in the bytes digest");
  assert.equal(hashInstalled(one.dir).sha256, one.deps.store, "the untouched export still digests the same");
  const sha = (f) => crypto.createHash("sha256").update(fs.readFileSync(f)).digest("hex");
  assert.equal(sha(path.join(two.dir, "pnpm-lock.yaml")), two.deps.lock); assert.equal(after.installed, two.deps.installed, "lockfile and installed-state metadata did not move");
  assert.equal(after.hardlinks, 0); assert.equal(after.externalLinks, 0);
  // Link-target mutation with payload bytes unchanged: re-point the workspace link at another directory inside the export.
  const link = path.join(two.dir, "packages", "a", "node_modules", "@rk", "b"); const was = fs.readlinkSync(link);
  fs.unlinkSync(link); fs.symlinkSync(path.join("..", "..", "..", "a"), link, "dir");
  const relinked = hashInstalled(two.dir).sha256; assert.notEqual(relinked, after.sha256, "a changed link target is a changed closure");
  fs.unlinkSync(link); fs.symlinkSync(was, link, "dir"); assert.equal(hashInstalled(two.dir).sha256, after.sha256);
  // Meaningful metadata mutation with payload unchanged: a layout field in .modules.yaml.
  const modules = path.join(two.dir, "node_modules", ".modules.yaml");
  if (fs.existsSync(modules)) {
    // pnpm ≤10 writes this file as YAML, pnpm 11 as JSON; the mutations below are made in whichever form is present.
    const text = fs.readFileSync(modules, "utf8"); const json = /^\s*\{/.test(text);
    const set = (key, value) => json ? JSON.stringify({ ...JSON.parse(text), [key]: value }, null, 2) : text.replace(new RegExp(`^${key}: .*$`, "m"), `${key}: ${value}`);
    assert.ok(json ? JSON.parse(text).layoutVersion !== 1 : /^layoutVersion: (?!1$)\d+$/m.test(text), "fixture has a layoutVersion to change");
    fs.writeFileSync(modules, set("layoutVersion", 1));
    assert.notEqual(hashInstalled(two.dir).sha256, after.sha256, "a layout change is a changed closure");
    // Timestamp-only change: normalized away.
    fs.writeFileSync(modules, set("prunedAt", "Mon, 01 Jan 2029 00:00:00 GMT"));
    assert.equal(hashInstalled(two.dir).sha256, after.sha256, "only the demonstrated timestamp field is normalized");
    fs.writeFileSync(modules, text);
  }
  const state = path.join(two.dir, "node_modules", ".pnpm-workspace-state-v1.json");
  if (fs.existsSync(state)) {
    const j = JSON.parse(fs.readFileSync(state, "utf8")); fs.writeFileSync(state, JSON.stringify({ ...j, lastValidatedTimestamp: 1 }));
    assert.equal(hashInstalled(two.dir).sha256, after.sha256, "the validation timestamp is normalized");
    const [k] = Object.keys(j.projects); fs.writeFileSync(state, JSON.stringify({ ...j, projects: { ...j.projects, [k]: { ...j.projects[k], name: "@rk/renamed" } } }));
    assert.notEqual(hashInstalled(two.dir).sha256, after.sha256, "a project entry change is a changed closure");
    fs.writeFileSync(state, JSON.stringify(j));
  }
  // An escaping link (resolving outside the export) is counted and the report that carries it is refused.
  fs.symlinkSync(os.tmpdir(), path.join(two.dir, "packages", "a", "node_modules", "escape"), "dir");
  const escaped = hashInstalled(two.dir); assert.equal(escaped.externalLinks, 1);
  fs.unlinkSync(path.join(two.dir, "packages", "a", "node_modules", "escape"));
  // The live checkout's node_modules (a link to a dirty package) is irrelevant: tamper it and re-export → same digest as `one`.
  fs.mkdirSync(path.join(repo, "node_modules", "@rk"), { recursive: true }); fs.symlinkSync(path.join(repo, "packages", "a"), path.join(repo, "node_modules", "@rk", "a"), "dir");
  write(repo, "packages/b/src/b.mjs", "console.log('live tamper'); export const b = 9;\n");
  const three = exportTree(repo, { commit }); assert.equal(three.deps.store, one.deps.store, "live tampering never reaches the exported, copied dependency bytes");
  for (const e of [one, two, three]) fs.rmSync(e.dir, { recursive: true, force: true });
});

test("exports-import-effects: dependency setup executes no candidate code — pnpm hooks (.pnpmfile.cjs), lifecycle scripts and injected npm_config_*/NODE_OPTIONS are all inert, while a plain install of the same tree runs them (positive control); the pnpm identity and its pin are checked", (t) => {
  const marks = tmp(t);
  const mark = (name) => path.join(marks, name);
  // A marker writer the hostile inputs can call by absolute path (no quoting inside shell strings).
  const markScript = path.join(marks, "mark.cjs"); fs.writeFileSync(markScript, `require("fs").writeFileSync(require("path").join(${JSON.stringify(marks)}, process.argv[2]), "ran");\n`);
  const hostile = {
    a: { exports: { ".": "./src/a.mjs" }, files: { "src/a.mjs": "export const a = 1;\n" }, deps: { "@rk/b": "workspace:*" } },
    b: { exports: { ".": "./src/b.mjs" }, files: { "src/b.mjs": "export const b = 1;\n" }, scripts: { preinstall: `node ${markScript} lifecycle`, postinstall: `node ${markScript} lifecycle-post` } },
  };
  const { repo, git } = gitRepoFixture(t, { packages: hostile });
  // The repository's own pnpm hook: candidate code pnpm would run at install.
  write(repo, ".pnpmfile.cjs", `require("fs").writeFileSync(${JSON.stringify(mark("pnpmfile"))}, "ran"); module.exports = { hooks: { readPackage(pkg) { require("fs").writeFileSync(${JSON.stringify(mark("readPackage"))}, pkg.name); return pkg; } } };\n`);
  write(repo, ".npmrc", "ignore-scripts=false\nignore-pnpmfile=false\nside-effects-cache=true\n");
  // The committed lockfile is regenerated with the hook present (it changes nothing about resolution for this tree).
  execFileSync("pnpm", ["install", "--lockfile-only", "--ignore-scripts", "--ignore-pnpmfile", "--offline"], { cwd: repo, encoding: "utf8", env: { ...process.env, CI: "1" } });
  git("add", "-A"); git("commit", "-qm", "hostile config");
  const commit = git("rev-parse", "HEAD").trim();
  // Positive control: a plain `pnpm install` of the same tree runs the hook and the scripts (the markers are real).
  const plain = tmp(t); execFileSync("tar", ["-x", "-C", plain], { input: execFileSync("git", ["-C", repo, "archive", "--format=tar", commit]) });
  // (not frozen: with hooks enabled pnpm compares the lockfile's pnpmfile checksum, which the committed hook-less lockfile lacks)
  try { execFileSync("pnpm", ["install", "--no-frozen-lockfile", "--offline"], { cwd: plain, encoding: "utf8", env: { ...process.env, CI: "1" }, stdio: ["ignore", "pipe", "pipe"] }); }
  catch (error) { assert.fail(`positive-control install failed: ${error.stderr}\n${error.stdout}`); }
  assert.ok(existsSync(mark("pnpmfile")) && existsSync(mark("readPackage")), "positive control: the pnpm hook ran under a plain install");
  assert.ok(existsSync(mark("lifecycle")) && existsSync(mark("lifecycle-post")), "positive control: the lifecycle scripts ran under a plain install");
  for (const m of ["pnpmfile", "readPackage", "lifecycle", "lifecycle-post", "node-options"]) fs.rmSync(mark(m), { force: true });
  // Under exportTree, with every re-enabling input injected through the environment: nothing runs.
  const preload = path.join(marks, "preload.cjs"); fs.writeFileSync(preload, `require("fs").writeFileSync(${JSON.stringify(mark("node-options"))}, "ran");\n`);
  const saved = { ...process.env };
  Object.assign(process.env, { npm_config_ignore_scripts: "false", npm_config_ignore_pnpmfile: "false", npm_config_side_effects_cache: "true", NODE_OPTIONS: `--require ${preload}`, PNPM_HOME: process.env.PNPM_HOME ?? "" });
  let exported; try { exported = exportTree(repo, { commit }); } finally { for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k]; Object.assign(process.env, saved); }
  assert.ok(exported.dir, exported.problem);
  for (const m of ["pnpmfile", "readPackage", "lifecycle", "lifecycle-post", "node-options"]) assert.equal(existsSync(mark(m)), false, `${m} executed during the exported install`);
  assert.equal(exported.deps.install, INSTALL_COMMAND);
  assert.match(exported.deps.pnpmIdentity.sha256, /^[0-9a-f]{64}$/); assert.ok(path.isAbsolute(exported.deps.pnpmIdentity.realPath)); assert.equal(exported.deps.pnpmIdentity.version, exported.deps.pnpm);
  fs.rmSync(exported.dir, { recursive: true, force: true });
  // The install environment is an allowlist: injected npm_config_* and NODE_OPTIONS never reach pnpm.
  const env = installEnvironment({ PATH: "/x", HOME: "/h", npm_config_ignore_scripts: "false", NODE_OPTIONS: "--require evil", PNPM_SCRIPT_SRC_DIR: "/evil", npm_config_registry: "http://evil" });
  assert.equal(env.npm_config_ignore_scripts, "true"); assert.equal(env.NODE_OPTIONS, ""); assert.equal(env.PNPM_SCRIPT_SRC_DIR, undefined); assert.equal(env.npm_config_registry, undefined); assert.equal(env.PATH, "/x");
  // pnpmIdentity is an executable identity: a PATH without pnpm is a problem, a resolved one has real path + digest + version.
  assert.match(pnpmIdentity({ env: { ...installEnvironment(), PATH: "/nonexistent" } }).problem ?? "", /not on PATH/);
  const id = pnpmIdentity(); assert.ok(id.realPath && /^[0-9a-f]{64}$/.test(id.sha256) && /^\d+\.\d+\.\d+/.test(id.version));
  // The identity is bound to the executable that answers, not to a string: the install environment disables pnpm's package-manager
  // version management (which would exec the tree's pinned pnpm from `<pnpm home>/.tools` instead of the file on PATH), and the
  // version the runtime answers must be the one the resolved file's own package.json declares. Hermetic controls with a shim
  // pnpm package on PATH: a truthful shim is accepted with its real path + sha256 (positive); a shim whose runtime answers another
  // version — what a silently delegating pnpm looks like — is refused (negative).
  assert.equal(installEnvironment({}).npm_config_manage_package_manager_versions, "false");
  assert.ok(INSTALL_COMMAND.includes("--config.manage-package-manager-versions=false"), INSTALL_COMMAND);
  const shim = (answers) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "rk-pnpm-shim-")); t.after(() => fs.rmSync(home, { recursive: true, force: true }));
    fs.mkdirSync(path.join(home, "pkg", "bin"), { recursive: true }); fs.mkdirSync(path.join(home, "bin"));
    fs.writeFileSync(path.join(home, "pkg", "package.json"), JSON.stringify({ name: "pnpm", version: "7.7.7" }));
    fs.writeFileSync(path.join(home, "pkg", "bin", "pnpm.cjs"), `#!/usr/bin/env node\nprocess.stdout.write(${JSON.stringify(answers)} + "\\n");\n`, { mode: 0o755 });
    fs.symlinkSync(path.join(home, "pkg", "bin", "pnpm.cjs"), path.join(home, "bin", "pnpm"));
    return { home, env: { ...installEnvironment(), PATH: `${path.join(home, "bin")}${path.delimiter}${process.env.PATH}` } };
  };
  const truthful = shim("7.7.7"); const ok = pnpmIdentity({ env: truthful.env });
  assert.equal(ok.version, "7.7.7", ok.problem); assert.equal(ok.realPath, fs.realpathSync(path.join(truthful.home, "pkg", "bin", "pnpm.cjs")));
  assert.equal(ok.sha256, crypto.createHash("sha256").update(fs.readFileSync(ok.realPath)).digest("hex"));
  const lying = shim("9.9.9"); const refused = pnpmIdentity({ env: lying.env });
  assert.match(refused.problem ?? "", /is version 7\.7\.7 but "pnpm --version" answered 9\.9\.9/);
  // A distinct hook vector, separate from install-time hooks and from ignore-scripts: merely asking `pnpm --version` INSIDE a tree
  // loads that tree's configuration and executes its .pnpmfile.cjs (updateConfig), `--ignore-pnpmfile` or not. Positive control:
  // the raw query in the hostile tree leaves the marker. Negative control: the identity query (asked in an empty directory) does not.
  fs.rmSync(mark("pnpmfile"), { force: true });
  // (The query is made as a developer shell would: PATH and HOME only. pnpm 10.28 skips the file when npm_config_ignore_pnpmfile is
  // set; pnpm 11.25 loads it regardless — so the install environment alone is not what keeps the identity query clean.)
  const raw = execFileSync("pnpm", ["--version"], { cwd: repo, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.HOME, npm_config_manage_package_manager_versions: "false" } }).trim();
  assert.ok(/^\d+\.\d+\.\d+/.test(raw)); assert.equal(existsSync(mark("pnpmfile")), true, "positive control: `pnpm --version` inside the tree ran its .pnpmfile.cjs");
  fs.rmSync(mark("pnpmfile"), { force: true });
  const quietId = pnpmIdentity(); assert.equal(quietId.version, id.version); assert.equal(existsSync(mark("pnpmfile")), false, "the identity query never loads the tree's configuration");
  // Normalization removes only the top-level prunedAt; a nested key of the same name is semantic and stays.
  const yaml = "included:\n  prunedAt: nested-semantic\nlayoutVersion: 5\nprunedAt: Sun, 25 Oct 2026 14:44:50 GMT\n";
  const norm = normalizeInstalledMetadata(".modules.yaml", yaml, "/export");
  assert.ok(norm.includes("  prunedAt: nested-semantic") && !norm.includes("prunedAt: Sun"), norm);
  assert.notEqual(normalizeInstalledMetadata(".modules.yaml", yaml.replace("nested-semantic", "other"), "/export"), norm, "a nested prunedAt change changes the digest input");
  const json = JSON.stringify({ lastValidatedTimestamp: 1, projects: { "/export/a": { name: "x", lastValidatedTimestamp: 7 } } });
  const nj = normalizeInstalledMetadata(".pnpm-workspace-state-v1.json", json, "/export");
  assert.ok(!/"lastValidatedTimestamp": 1\b/.test(nj) && nj.includes('"lastValidatedTimestamp": 7'), "only the top-level timestamp is removed; a nested one stays");
  // pnpm 11 writes .modules.yaml as JSON: the same rule, top-level prunedAt only.
  const mj = JSON.stringify({ included: { prunedAt: "nested-semantic" }, layoutVersion: 5, prunedAt: "Sun, 25 Oct 2026 15:32:54 GMT", storeDir: "/s" });
  const nm = normalizeInstalledMetadata(".modules.yaml", mj, "/export");
  assert.ok(nm.includes('"prunedAt": "nested-semantic"') && !nm.includes("Sun, 25"), nm);
  assert.equal(nm, normalizeInstalledMetadata(".modules.yaml", mj.replace("15:32:54", "15:32:56"), "/export"), "the top-level timestamp alone never changes the digest input");
  assert.notEqual(nm, normalizeInstalledMetadata(".modules.yaml", mj.replace("nested-semantic", "other"), "/export"));
  // A tree that pins another pnpm is not exported with this one.
  write(repo, "package.json", JSON.stringify({ name: "@rk/fixture-root", private: true, type: "module", packageManager: "pnpm@9.0.0" })); git("add", "-A"); git("commit", "-qm", "pin");
  const pinned = exportTree(repo, { commit: git("rev-parse", "HEAD").trim() });
  assert.match(pinned.problem ?? "", /pins pnpm@9\.0\.0 .* but the pnpm on PATH is/);
});

test("exports-import-effects: HEAD moving between identity capture and archive is harmless (the captured commit is archived by id); an ABA back to the captured commit stays stable, a different final HEAD is unstable", (t) => {
  const { repo, git } = gitRepoFixture(t);
  const base = git("rev-parse", "HEAD").trim();
  const movedBetween = (aba) => {
    let calls = 0;
    const identity = (root, opts) => { const id = checkoutIdentity(root, opts); calls++; if (calls === 1) { write(repo, "packages/a/src/s.mjs", "console.log('after capture, before archive'); export const s = 0;\n"); git("add", "-A"); git("commit", "-qm", "moved"); if (aba) git("reset", "-q", "--hard", base); } return id; };
    const out = path.join(repo, "out", `${aba ? "aba" : "moved"}.json`); const io = { logs: [], errs: [] };
    assert.equal(main(["--root", repo, "--report", out], { log: (m) => io.logs.push(m), error: (m) => io.errs.push(m) }, { identity }), 0, io.errs.join("\n"));
    if (!aba) git("reset", "-q", "--hard", base);
    return JSON.parse(fs.readFileSync(out, "utf8"));
  };
  const aba = movedBetween(true);
  assert.equal(aba.commit, base); assert.equal(aba.stable, true); assert.equal(aba.findings, 0, "the never-committed-at-capture 'console.log' module was not what the archive exported");
  const moved = movedBetween(false);
  assert.equal(moved.commit, base); assert.equal(moved.stable, false, "final HEAD differs from the captured commit"); assert.equal(moved.findings, 0, "the archive was the captured commit regardless");
  assert.match(readIncompleteReport(path.join(repo, "out", "moved.json")).problem, /HEAD moved while the probe ran/);
});

test("exports-import-effects: edits, restores and a HEAD ABA performed while an import is ACTUALLY blocked (external barrier) never reach the probe; the result equals a quiet run", async (t) => {
  const barrier = path.join(tmp(t), "release");
  const waiter = `import fs from "node:fs"; const until = Date.now() + 20000; while (!fs.existsSync(process.env.RK_BARRIER) && Date.now() < until) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 250); export const waited = true;\n`;
  const packages = { w: { exports: { ".": "./dist/index.js", "./wait": "./src/wait.mjs" }, files: { "src/wait.mjs": waiter }, deps: { "@rk/z": "workspace:*" } }, z: { exports: { ".": "./src/z.mjs" }, files: { "src/z.mjs": "export const z = 1;\n" } } };
  const { repo, git } = gitRepoFixture(t, { packages });
  const base = git("rev-parse", "HEAD").trim();
  const run = (name, env) => new Promise((resolve) => { const out = path.join(repo, "out", `${name}.json`); const child = spawn(process.execPath, [PROBE_CLI, "--root", repo, "--report", out], { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, ...env } }); let err = ""; child.stderr.on("data", (d) => { err += d; }); child.on("close", (code) => resolve({ code, err, report: fs.existsSync(out) ? JSON.parse(fs.readFileSync(out, "utf8")) : null })); });
  fs.writeFileSync(barrier, ""); const quiet = await run("quiet", { RK_BARRIER: barrier }); assert.equal(quiet.code, 0, quiet.err); fs.rmSync(barrier);
  // Start the probe; wait until a grandchild is blocked importing wait.mjs (visible in /proc), then mutate everything, then release.
  const blocked = run("blocked", { RK_BARRIER: barrier });
  const importer = () => { try { for (const pid of fs.readdirSync("/proc").filter((d) => /^\d+$/.test(d))) { try { const cmd = fs.readFileSync(`/proc/${pid}/cmdline`, "utf8"); if (cmd.includes("wait.mjs") && cmd.includes("--permission")) return { pid, cmd }; } catch { /* gone */ } } } catch { /* no procfs */ } return null; };
  const blockedAt = (pid) => { try { const state = fs.readFileSync(`/proc/${pid}/stat`, "utf8").split(") ").at(-1).split(" ")[0]; const wchan = fs.readFileSync(`/proc/${pid}/wchan`, "utf8").trim(); return { state, wchan }; } catch { return null; } };
  const deadline = Date.now() + 15000; let child = null; while (!(child = importer()) && Date.now() < deadline) await new Promise((r) => setTimeout(r, 20));
  assert.ok(child, "a probe child is importing wait.mjs");
  const urlMatch = /file:\/\/(\/[^"'\0]*?)\/packages\/w\/src\/wait\.mjs/.exec(child.cmd);
  assert.ok(urlMatch, `import URL not found in the child's command line: ${JSON.stringify(child.cmd.replaceAll("\0", " ")).slice(0, 400)}`);
  const exportDir = decodeURIComponent(urlMatch[1]);
  assert.notEqual(fs.realpathSync(exportDir), fs.realpathSync(repo), "the child imports from the exported tree, not from the live checkout");
  // Blocked AT the barrier: the waiter sleeps 250 ms per Atomics.wait between 1 ms barrier checks, so over ten samples 40 ms apart
  // the child is in interruptible sleep inside a futex wait almost always (a sample may land in the wake window), and alive throughout.
  const samples = []; for (let i = 0; i < 10; i++) { await new Promise((r) => setTimeout(r, 40)); samples.push(blockedAt(child.pid)); }
  assert.ok(samples.every(Boolean), "the child is alive across every sample");
  const sleeping = samples.filter((x) => x.state === "S" && /futex/.test(x.wchan)).length;
  assert.ok(sleeping >= 8, `child sampled sleeping in a futex wait ${sleeping}/10 times: ${samples.map((x) => `${x.state}:${x.wchan}`).join(" ")}`);
  // Post-digest mutability: a byte appended to an installed dependency inside the export while the import is blocked must be
  // caught by the re-digest → closureStable:false → the report is unusable.
  const victim = path.join(exportDir, "packages", "w", "node_modules", "@rk", "z", "src", "z.mjs");
  const target = fs.existsSync(victim) ? fs.realpathSync(victim) : null;
  if (target) { fs.chmodSync(target, 0o644); fs.appendFileSync(target, "\n// mutated while blocked\n"); }
  // While it is blocked: rewrite the blocked module itself and another export, add untracked effectful files, commit, and reset back (ABA).
  write(repo, "packages/w/src/wait.mjs", "console.log('rewritten while blocked');\n");
  write(repo, "packages/z/src/z.mjs", "console.log('z rewritten while blocked');\n");
  write(repo, "packages/z/src/stray.mjs", "console.log('stray');\n");
  git("add", "-A"); git("commit", "-qm", "while blocked"); git("reset", "-q", "--hard", base);
  write(repo, "packages/w/src/wait.mjs", "console.log('dirty again');\n");
  fs.writeFileSync(barrier, ""); const done = await blocked; assert.equal(done.code, 0, done.err);
  git("checkout", "-q", "--", "."); assert.equal(git("rev-parse", "HEAD").trim(), base);
  const strip = (r) => ({ commit: r.commit, tree: r.tree, stable: r.stable, findings: r.findings, incomplete: r.incomplete.map((x) => x.key).sort(), covered: r.covered.map((x) => x.key).sort(), store: r.closure.deps.store });
  assert.deepEqual(strip(done.report), strip(quiet.report), "nothing done to the live checkout while the import was blocked reached the probe");
  assert.equal(done.report.findings, 0);
  if (target) {
    assert.equal(done.report.closure.deps.closureStable, false, "the in-export mutation during the blocked import was caught by the re-digest");
    assert.match(readIncompleteReport(path.join(repo, "out", "blocked.json"), { commit: done.report.commit, tree: done.report.tree, probe: done.report.probe, node: done.report.node }).problem ?? "", /installed bytes changed between their digest and the end of the probe/);
  } else assert.equal(done.report.closure.deps.closureStable, true);
});

test("exports-import-effects: a real concurrent edit/restore race against the probe child leaves its result equal to a quiet run, and a HEAD ABA (commit, then reset back) during the probe stays bound to the captured commit", async (t) => {
  // Several packages make the probe long enough for the race to interleave with real imports.
  const packages = Object.fromEntries(Array.from({ length: 6 }, (_, i) => [`p${i}`, { exports: { ".": `./dist/index.js`, "./m": "./src/m.mjs" }, files: { "src/m.mjs": `export const m = ${i};\n` } }]));
  const { repo, git } = gitRepoFixture(t, { packages });
  const base = git("rev-parse", "HEAD").trim();
  const run = (name) => new Promise((resolve) => { const out = path.join(repo, "out", `${name}.json`); const child = spawn(process.execPath, [PROBE_CLI, "--root", repo, "--report", out], { stdio: ["ignore", "pipe", "pipe"] }); let err = ""; child.stderr.on("data", (d) => { err += d; }); child.on("close", (code) => resolve({ code, err, report: fs.existsSync(out) ? JSON.parse(fs.readFileSync(out, "utf8")) : null })); return child; });
  const quiet = await run("quiet");
  assert.equal(quiet.code, 0, quiet.err);
  // Race 1: tracked files are rewritten and restored, untracked files appear and vanish, every 5 ms until the child exits.
  const racy = run("racy"); let ticks = 0; const original = fs.readFileSync(path.join(repo, "packages/p0/src/m.mjs"), "utf8");
  const timer = setInterval(() => { ticks++; const edited = ticks % 2 === 1; fs.writeFileSync(path.join(repo, "packages/p0/src/m.mjs"), edited ? "console.log('racing edit at import');\n" : original); if (edited) fs.writeFileSync(path.join(repo, "packages/p1/src/stray.mjs"), "console.log('stray');\n"); else fs.rmSync(path.join(repo, "packages/p1/src/stray.mjs"), { force: true }); }, 5);
  const raced = await racy; clearInterval(timer); fs.writeFileSync(path.join(repo, "packages/p0/src/m.mjs"), original); fs.rmSync(path.join(repo, "packages/p1/src/stray.mjs"), { force: true });
  assert.ok(ticks > 10, `the race interleaved (${ticks} ticks)`);
  assert.equal(raced.code, 0, raced.err);
  const strip = (r) => ({ commit: r.commit, tree: r.tree, stable: r.stable, findings: r.findings, incomplete: r.incomplete.map((x) => x.key).sort(), covered: r.covered.map((x) => x.key).sort(), store: r.closure.deps.store });
  assert.deepEqual(strip(raced.report), strip(quiet.report), "the live edits never reached the exported probe");
  assert.equal(raced.report.findings, 0, "the racing 'console.log at import' edit was never imported");
  // Race 2 (ABA): HEAD moves to a new commit and back to the captured one while the probe runs.
  const aba = run("aba"); await new Promise((r) => setTimeout(r, 150));
  fs.writeFileSync(path.join(repo, "packages/p2/src/m.mjs"), "console.log('aba');\n"); git("add", "-A"); git("commit", "-qm", "aba"); git("reset", "-q", "--hard", base);
  const after = await aba; assert.equal(after.code, 0, after.err);
  assert.equal(git("rev-parse", "HEAD").trim(), base);
  assert.equal(after.report.commit, base); assert.equal(after.report.stable, true, "before/after identity equal: bound to the captured commit, which git archive exported by its exact id");
  assert.deepEqual(strip(after.report), strip(quiet.report));
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
