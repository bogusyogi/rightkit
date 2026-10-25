import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { addDays } from "../packages/release/app-check.mjs";
import { cargoPackage, lockPathPackages, looksBinary, main, RULES, runRules } from "./preflight.mjs";

const tree = (t, files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-preflight-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [file, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), text);
  }
  return { root, files: Object.keys(files) };
};
const run = (only, t, files) => runRules({ ...tree(t, files), only: [only] }).failures;

test("cargoPackage reads [package] only, never a dependency's version", () => {
  const pkg = cargoPackage('[package]\nname = "rightkit-a"\nversion = "0.2.0"\npublish = false\n\n[dependencies]\nrightkit-b = { version = "=9.9.9" }\nversion = "1.0.0"\n');
  assert.deepEqual(pkg, { name: "rightkit-a", version: "0.2.0", publish: false });
  assert.equal(cargoPackage('[package]\nname = "x"\nversion = "1.0.0"\n').publish, true);
});

test("lockPathPackages keeps path packages and drops registry ones", () => {
  const lock = 'version = 4\n\n[[package]]\nname = "rightkit-a"\nversion = "0.6.0"\ndependencies = [\n "serde",\n]\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n';
  assert.deepEqual(lockPathPackages(lock), [{ name: "rightkit-a", version: "0.6.0", source: false }]);
});

test("consumer-locks refuses a consumer lock behind its path crate and ignores ungated locks", (t) => {
  const crate = '[package]\nname = "rightkit-a"\nversion = "0.6.1"\n';
  const stale = '[[package]]\nname = "rightkit-a"\nversion = "0.6.0"\n';
  const failures = run("consumer-locks", t, { "crates/rightkit-a/Cargo.toml": crate, "ci/consumers/wv2/Cargo.lock": stale, "spikes/old/Cargo.lock": stale, "desktop-app-template/Cargo.lock": stale });
  assert.equal(failures.length, 2, "the template is gated (rightkit-gate builds it --locked); the spike is not");
  assert.match(failures[0].problem, /^ci\/consumers\/wv2\/Cargo\.lock lists rightkit-a 0\.6\.0 but crates\/rightkit-a\/Cargo\.toml is 0\.6\.1/);
  assert.match(failures[1].problem, /^desktop-app-template\/Cargo\.lock lists rightkit-a 0\.6\.0/);
  assert.match(failures[0].fix, /version = "0\.6\.1"/);
});

test("dispatch-pins refuses a changed helper and a pin it cannot map to a file", (t) => {
  const helper = "export const x = 1;\n";
  const dispatch = (pin) => `const PREFLIGHT_SHA256 = "${pin}";\nconst ADMISSION_DEPENDENCIES = {\n};\n`;
  const changed = run("dispatch-pins", t, { "scripts/ci/rightkit-dispatch.mjs": dispatch("0".repeat(64)), "scripts/ci/rk-preflight.mjs": helper });
  assert.equal(changed.length, 1);
  assert.match(changed[0].problem, /^scripts\/ci\/rk-preflight\.mjs changed/);
  const unknown = run("dispatch-pins", t, { "scripts/ci/rightkit-dispatch.mjs": `const NEW_HELPER_SHA256 = "${"0".repeat(64)}";\n` });
  assert.match(unknown[0].problem, /NEW_HELPER_SHA256, which preflight cannot map to a file/);
});

test("version-pins refuses a crate's dispatch exception left at the old version", (t) => {
  const failures = run("version-pins", t, {
    "crates/rightkit-a/Cargo.toml": '[package]\nname = "rightkit-a"\nversion = "0.6.1"\n',
    "ci/rk-pipeline.json": JSON.stringify({ dispatch: { exceptions: { "rightkit-a": { version: "0.6.0", allow: [] } } } }),
  });
  assert.equal(failures.length, 1);
  assert.match(failures[0].problem, /dispatch\.exceptions\["rightkit-a"\]\.version is "0\.6\.0" but crates\/rightkit-a\/Cargo\.toml is 0\.6\.1/);
});

test("a rule that throws is a FAIL, never a pass", (t) => {
  const { root, files } = tree(t, {});
  const { failures } = runRules({ root, files, commit: "not-a-commit", only: ["eol-policy"] });
  assert.equal(failures.length, 1);
  assert.match(failures[0].problem, /^the rule itself failed/);
});

test("every rule names its scan-gap family or the invariant it enforces", () => {
  for (const rule of RULES) assert.match(rule.gap, /^(G\d{2}|P\d+)$/, rule.id);
});

// ---- regression fixtures from tonight's refusals, built from this checkout's real recipe data -----------------------------
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const copyFromRepo = (root, rel) => {
  fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
  fs.cpSync(path.join(REPO, rel), path.join(root, rel), { recursive: true });
};

test("G01: a changed consumer recipe fails with the file and its committed hash (rightkit-automation 0.6.1, e0901f6)", (t) => {
  const { root } = tree(t, {});
  for (const rel of ["ci/consumer-recipes.json", "crates/rightkit-automation/Cargo.toml", "ci/consumers/rightkit-automation"]) copyFromRepo(root, rel);
  const mine = () => runRules({ root, files: [], only: ["consumer-recipe"] }).failures.filter((f) => f.problem.startsWith("rightkit-automation:"));
  assert.deepEqual(mine(), [], "the committed recipe passes");
  fs.appendFileSync(path.join(root, "ci/consumers/rightkit-automation/Cargo.toml"), "\n# changed\n");
  const [failure, ...rest] = mine();
  assert.equal(rest.length, 0);
  assert.match(failure.problem, /recipe differs from reviewed PK\d+ inventory/);
  assert.match(failure.problem, /Cargo\.toml: inventory [0-9a-f]{12}…, committed [0-9a-f]{64}/);
  assert.match(failure.fix, /^review the recipe change/, "the fix asks for review; preflight never writes an approved hash");
});

test("G02: an export missing from importClosures fails (@rightkit/app-update ./suite-api, d6eec41)", (t) => {
  const { root } = tree(t, {});
  for (const rel of ["ci/consumer-recipes.json", "packages/app-update/package.json"]) copyFromRepo(root, rel);
  const mine = () => runRules({ root, files: [], only: ["npm-surface"] }).failures.filter((f) => f.problem.startsWith("@rightkit/app-update:"));
  assert.deepEqual(mine(), [], "the committed entry passes");
  const file = path.join(root, "ci/consumer-recipes.json");
  const inventory = JSON.parse(fs.readFileSync(file, "utf8"));
  delete inventory.npm["@rightkit/app-update"].importClosures["./suite-api"];
  fs.writeFileSync(file, JSON.stringify(inventory));
  assert.match(mine()[0].problem, /importClosures must cover reviewed surface exactly once/);
});

// ---- P2 and P9 of docs/PIPELINE-ARCHITECTURE.md -------------------------------------------------------------------------
const NETWORK_GUARD = `data:text/javascript,${encodeURIComponent(`
import net from "node:net"; import tls from "node:tls"; import dns from "node:dns"; import http from "node:http"; import https from "node:https";
const block = (what) => function () { process.stderr.write("OFFLINE-GUARD " + what + "\\n"); throw new Error("offline guard: " + what); };
net.Socket.prototype.connect = block("net.Socket.connect"); net.connect = net.createConnection = block("net.connect");
tls.connect = block("tls.connect"); dns.lookup = block("dns.lookup"); dns.promises.lookup = block("dns.promises.lookup");
http.request = http.get = block("http.request"); https.request = https.get = block("https.request"); globalThis.fetch = block("fetch");
`)}`;
const PREFLIGHT = fileURLToPath(new URL("./preflight.mjs", import.meta.url));
const git = (cwd, ...args) => execFileSync("git", args, { cwd, encoding: "utf8" });

test("P2: preflight on this checkout needs no environment or install and never touches the network; P9: it never writes to the checkout", (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "rk-preflight-home-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const before = { status: git(REPO, "status", "--porcelain=v1", "-uall"), index: git(REPO, "diff", "--cached", "--name-only") };
  // Only PATH (to find git) and a throwaway HOME; every node child (the generated-docs check) inherits the guard.
  const r = spawnSync(process.execPath, [PREFLIGHT], { cwd: REPO, encoding: "utf8", env: { PATH: process.env.PATH, HOME: home, NODE_OPTIONS: `--import=${NETWORK_GUARD}` } });
  assert.doesNotMatch(`${r.stdout}${r.stderr}`, /OFFLINE-GUARD/, "a rule reached for the network");
  assert.ok(r.status === 0 || r.status === 1, `preflight could not run (exit ${r.status}): ${r.stderr}`);
  assert.match(r.stdout, /^preflight: /m);
  assert.deepEqual({ status: git(REPO, "status", "--porcelain=v1", "-uall"), index: git(REPO, "diff", "--cached", "--name-only") }, before, "preflight changed the checkout");
});

test("P2: --repo checks another repository's committed files, not its working tree (G04 fixture: CRLF against eol=lf)", (t) => {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "rk-preflight-repo-"));
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  git(repo, "init", "-q");
  for (const [k, v] of [["user.email", "t@example.test"], ["user.name", "t"], ["core.autocrlf", "false"]]) git(repo, "config", k, v);
  fs.writeFileSync(path.join(repo, "a.json"), "{}\r\n");
  git(repo, "add", "a.json");
  git(repo, "commit", "-q", "-m", "crlf");
  fs.writeFileSync(path.join(repo, ".gitattributes"), "*.json text eol=lf\n");
  git(repo, "add", ".gitattributes");
  git(repo, "commit", "-q", "-m", "attributes after the fact");
  const run = (...args) => spawnSync(process.execPath, [PREFLIGHT, "--repo", repo, ...args], { encoding: "utf8" });
  const red = run();
  assert.equal(red.status, 1, red.stdout + red.stderr);
  assert.match(red.stdout, /FAIL eol-policy \(G04\): 1 file\(s\) committed with CRLF.*: a\.json/);
  fs.writeFileSync(path.join(repo, "a.json"), "{}\n");
  assert.equal(run().status, 1, "an uncommitted fix is not what is pushed");
  assert.equal(run("--worktree").status, 0, "the working tree a compile would see is clean");
  git(repo, "add", "--renormalize", "a.json");
  git(repo, "commit", "-q", "-m", "renormalize");
  const green = run();
  assert.equal(green.status, 0, green.stdout + green.stderr);
});

// ---- P11 of docs/PIPELINE-ARCHITECTURE.md: one client name (the rule moved to packages/release/app-check.mjs and is tested there) ----
const OLD = ["right", "kit"].join("");

test("G01: a unit about to publish is checked even when its inventory entry and recipe are deleted", (t) => {
  const { root } = tree(t, {
    "packages/release/rightkit-versions.json": JSON.stringify({ registry: { cargo: { "rightkit-gone": "0.1.0" }, npm: { "@rightkit/gone": "0.1.0" } } }),
    "crates/rightkit-gone/Cargo.toml": `[package]\nname = "rightkit-gone"\nversion = "0.2.0"\n`,
    "packages/gone/package.json": JSON.stringify({ name: "@rightkit/gone", version: "0.2.0" }),
    "ci/consumer-recipes.json": JSON.stringify({ schema: 1, cargo: {}, npm: {} }),
    // Listed as uncovered: that excuses a unit with nothing to publish, never one about to publish.
    "ci/recipe-coverage.json": JSON.stringify({ schema: 1, uncovered: { cargo: { "rightkit-gone": "no recipe yet" }, npm: { "@rightkit/gone": "no recipe yet" } } }),
  });
  const problems = runRules({ root, files: [], only: ["consumer-recipe", "npm-recipe"] }).failures.map((f) => f.problem);
  assert.ok(problems.some((p) => p.startsWith("rightkit-gone:")), problems.join("\n"));
  assert.ok(problems.some((p) => p.startsWith("@rightkit/gone:") && /missing reviewed PK example/.test(p)), problems.join("\n"));
  // At its published version the unit has nothing to publish, so no recipe is required yet.
  fs.writeFileSync(path.join(root, "packages/release/rightkit-versions.json"), JSON.stringify({ registry: { cargo: { "rightkit-gone": "0.2.0" }, npm: { "@rightkit/gone": "0.2.0" } } }));
  assert.deepEqual(runRules({ root, files: [], only: ["consumer-recipe", "npm-recipe"] }).failures, []);
});

test("G01: recipe coverage is explicit: a publishable unit has a recipe or is listed, and the list holds only those", (t) => {
  const { root } = tree(t, {
    "packages/release/rightkit-versions.json": JSON.stringify({ registry: { cargo: { "rightkit-new": "0.1.0", "rightkit-old": "0.1.0" }, npm: { "@rightkit/new": "0.1.0" } } }),
    "crates/rightkit-new/Cargo.toml": `[package]\nname = "rightkit-new"\nversion = "0.1.0"\n`,
    "crates/rightkit-old/Cargo.toml": `[package]\nname = "rightkit-old"\nversion = "0.1.0"\n`,
    "crates/rightkit-internal/Cargo.toml": `[package]\nname = "rightkit-internal"\nversion = "0.1.0"\npublish = false\n`,
    "packages/new/package.json": JSON.stringify({ name: "@rightkit/new", version: "0.1.0" }),
    "packages/private/package.json": JSON.stringify({ name: "@rightkit/private", version: "0.1.0", private: true }),
    "ci/consumer-recipes.json": JSON.stringify({ schema: 1, cargo: {}, npm: {} }),
    "ci/recipe-coverage.json": JSON.stringify({ schema: 1, uncovered: { cargo: { "rightkit-old": "why", "rightkit-renamed": "why" }, npm: {} } }),
  });
  const problems = runRules({ root, files: [], only: ["consumer-recipe", "npm-recipe"] }).failures.map((f) => f.problem.split(": ").slice(0, 2).join(": "));
  assert.deepEqual(problems.sort(), [
    "@rightkit/new: publishable but has no reviewed consumer recipe (ci/consumer-recipes.json npm) and is not listed in ci/recipe-coverage.json uncovered.npm",
    "rightkit-new: publishable but has no reviewed consumer recipe (ci/consumer-recipes.json cargo) and is not listed in ci/recipe-coverage.json uncovered.cargo",
    "rightkit-renamed: listed in ci/recipe-coverage.json uncovered.cargo but no publishable crate has that name",
  ].sort());
});

test("G01: this tree's recipe coverage list names exactly its publishable units without a recipe", () => {
  const coverage = JSON.parse(fs.readFileSync(new URL("../ci/recipe-coverage.json", import.meta.url), "utf8"));
  const inventory = JSON.parse(fs.readFileSync(new URL("../ci/consumer-recipes.json", import.meta.url), "utf8"));
  for (const unit of Object.keys(coverage.uncovered.cargo)) assert.ok(!inventory.cargo[unit], `${unit} has a recipe; take it off the list`);
  for (const unit of Object.keys(coverage.uncovered.npm)) assert.ok(!inventory.npm[unit], `${unit} has a recipe; take it off the list`);
  assert.deepEqual(runRules({ root: fileURLToPath(new URL("..", import.meta.url)), files: [], only: ["consumer-recipe", "npm-recipe"] }).failures.filter((f) => /recipe-coverage/.test(f.problem)), []);
});

test("P11: retireAliasText rewrites to rustright and is idempotent", async () => {
  const { retireAliasText } = await import("./preflight.mjs");
  const text = `${OLD} cargo test && ${OLD} status x\n["${OLD}", "cargo", "build"]\n`;
  const once = retireAliasText(text);
  assert.equal(once.count, 3);
  assert.equal(once.text, `rustright cargo test && rustright status x\n["rustright", "cargo", "build"]\n`);
  assert.equal(retireAliasText(once.text).count, 0);
});

test("G01 recipe rules do not apply to an app: no recipe inventory or version feed means nothing to check", (t) => {
  const { root } = tree(t, { "crates/app-core/Cargo.toml": '[package]\nname = "app-core"\nversion = "0.1.0"\n', "packages/ui/package.json": JSON.stringify({ name: "@app/ui", version: "1.0.0" }) });
  assert.deepEqual(runRules({ root, files: [], only: ["consumer-recipe", "npm-recipe"] }).failures, []);
});

// ---- G25 and G26: what only a Windows or a macOS runner showed ---------------------------------------------------------------------
// Both police RightKit's own tree: they switch on scripts/preflight.mjs being committed, so an app checked from here stays quiet.
const RIGHTKIT = { "scripts/preflight.mjs": "// the file that makes this tree RightKit's\n" };
// The Right Suite API projections as .gitattributes pinned them when rightkit#1175 added the Swift one: four of five.
const SUITE_PINS = ["/contracts/right-suite-api.v1.json", "/crates/rightkit-license/src/suite_api_generated.rs", "/packages/app-update/src/suite-api.generated.ts", "/packages/release/suite-api.generated.mjs"].map((f) => `${f} text eol=lf\n`).join("");
const SWIFT_PROJECTION = "swift/RightKitSwift/Sources/RightKitLicense/RightSuiteAPI.generated.swift";

test("G25: a text file whose line ending is left to the host is refused (the Swift Right Suite API projection of rightkit#1175); one default pin removes the class", (t) => {
  const suite = { ...RIGHTKIT, "contracts/right-suite-api.v1.json": "{}\n", "crates/rightkit-license/src/suite_api_generated.rs": "pub const A: u32 = 1;\n", "packages/app-update/src/suite-api.generated.ts": "export const A = 1;\n", "packages/release/suite-api.generated.mjs": "export const A = 1;\n", [SWIFT_PROJECTION]: "// generated\npublic enum RightSuiteAPI {}\n" };
  // These two lines pin the files the fixture itself needs, so the Swift projection is the only text file left to the host.
  const historical = tree(t, { ...suite, ".gitattributes": `${SUITE_PINS}/.gitattributes text eol=lf\n/scripts/preflight.mjs text eol=lf\n` });
  const red = runRules({ ...historical, only: ["eol-pinned"] }).failures;
  assert.equal(red.length, 1);
  assert.match(red[0].problem, new RegExp(`^1 tracked text file\\(s\\) have no pinned line ending: .*\\(${SWIFT_PROJECTION.replaceAll(".", "\\.")}\\)$`));
  assert.match(red[0].fix, /make `\* text=auto eol=lf` the first line of \.gitattributes/);
  // The repository's answer: one default for every text file, so the next generated file needs no line of its own.
  const fixed = tree(t, { ...suite, ".gitattributes": `* text=auto eol=lf\n${SUITE_PINS}` });
  assert.deepEqual(runRules({ ...fixed, only: ["eol-pinned"] }).failures, []);
});

test("G25: -text, eol=crlf, binary files and files without a line break do not depend on the host; text-set, vendored and unpinned files do", (t) => {
  const { root, files } = tree(t, {
    ...RIGHTKIT,
    ".gitattributes": "/.gitattributes text eol=lf\n/scripts/preflight.mjs text eol=lf\n/kept.txt -text\n/run.cmd text eol=crlf\n/notes.md text\n",
    "kept.txt": "bytes\r\nkept\r\n", // -text: git converts nothing, on any host
    "run.cmd": "@echo off\r\n", // eol=crlf: the same conversion on every host
    "image.bin": "\0\0\u0089PNG\r\n\0", // binary by content
    "one-line.json": '{"a":1}', // no line break, so nothing to convert
    "empty.txt": "",
    "notes.md": "text is set, eol is not\n", // the host decides
    "vendor/lib/a.txt": "vendored bytes are the ones that get hashed\n",
    "node_modules/x/b.txt": "installed, not tracked\n",
  });
  const [failure, ...rest] = runRules({ root, files, only: ["eol-pinned"] }).failures;
  assert.equal(rest.length, 0);
  assert.match(failure.problem, /^2 tracked text file\(s\) have no pinned line ending: .*\(notes\.md, vendor\/lib\/a\.txt\)$/);
});

test("G25 and G26 stay quiet on a repository that is not RightKit's; looksBinary is git's own test", (t) => {
  const { root, files } = tree(t, { "src/a.txt": "no attributes at all\n", "src/a.test.mjs": "const app = tempDir(t);\nassert.equal(f(), path.join(app, 'x'));\nrealpathSync(app);\n" });
  assert.deepEqual(runRules({ root, files, only: ["eol-pinned", "tmpdir-realpath"] }).failures, []);
  assert.equal(looksBinary(Buffer.from("plain\r\ntext\n")), false);
  assert.equal(looksBinary(Buffer.from("tab\tand é as utf-8\n")), false);
  assert.equal(looksBinary(Buffer.from("a\0b")), true, "a NUL");
  assert.equal(looksBinary(Buffer.from("a\rb")), true, "a lone CR");
  assert.equal(looksBinary(Buffer.alloc(300, 1)), true, "mostly non-printable bytes");
});

const OUTDATED_TEST = "packages/release/outdated-command.test.mjs";
// packages/release/outdated-command.test.mjs at 8e14bf6d (before #1187), its first test verbatim but for how `app` is bound. Every fixture
// line starts with "|", stripped here, so none of them reads as a test( of this file.
const outdatedTest = (bind) => `
|import assert from "node:assert/strict";
|import { realpathSync } from "node:fs";
|import path from "node:path";
|import test from "node:test";
|import { findOutdatedPackage, resolveOutdatedBin } from "./outdated-command.mjs";
|
|test("npm-flat layout: the updater installed beside @rightkit/release is found and run with the caller's arguments and cwd", (t) => {
|  const app = ${bind};
|  installRelease(path.join(app, "node_modules", "@rightkit", "release"));
|  installOutdated(path.join(app, "node_modules", "@rightkit", "outdated"));
|  const releaseDir = path.join(app, "node_modules", "@rightkit", "release");
|  assert.equal(findOutdatedPackage(releaseDir), path.join(app, "node_modules", "@rightkit", "outdated"));
|  assert.equal(resolveOutdatedBin(releaseDir), path.join(app, "node_modules", "@rightkit", "outdated", "dist", "bin.js"));
|  const ok = runCli(releaseDir, ["update", "run", "--repo", "r with space", "--dry-run"], app);
|  assert.equal(ok.status, 0, ok.stderr);
|  assert.deepEqual(JSON.parse(ok.stdout), { args: ["run", "--repo", "r with space", "--dry-run"], cwd: realpathSync(app) });
|});
`.replace(/^\|/gm, "");

test("G26: a temp directory passed to realpath and also compared unresolved is refused (outdated-command.test.mjs before #1187); resolving it where it is made passes", (t) => {
  const source = outdatedTest("tempDir(t)");
  const line = source.split("\n").findIndex((l) => l.includes("assert.equal(findOutdatedPackage")) + 1;
  const red = run("tmpdir-realpath", t, { ...RIGHTKIT, [OUTDATED_TEST]: source });
  assert.equal(red.length, 1);
  assert.match(red[0].problem, new RegExp(`^${OUTDATED_TEST.replaceAll(".", "\\.")}:${line} compares app, a temp directory this test keeps unresolved, although the same test passes app to realpath`));
  assert.equal(red[0].identity, `tmpdir-realpath|${OUTDATED_TEST}|app`);
  assert.match(red[0].fix, /bind app to realpathSync\(\.\.\.\) where the test creates it/);
  // #1187's fix, and the same thing spelled the other ways it can be spelled.
  for (const bind of ["realpathSync(tempDir(t))", "fs.realpathSync(mkdtempSync(path.join(os.tmpdir(), 'x-')))", "await realpath(tmp())"]) {
    assert.deepEqual(run("tmpdir-realpath", t, { ...RIGHTKIT, [OUTDATED_TEST]: outdatedTest(bind) }), [], bind);
  }
});

test("G26 reports only the contradiction: a nested call, a derived path, a test that never resolves it and another test's directory are not it", (t) => {
  const block = (name, ...lines) => [`test("${name}", (t) => {`, ...lines.map((l) => `  ${l}`), "});"].join("\n");
  const subjects = (...tests) => run("tmpdir-realpath", t, { ...RIGHTKIT, "a.test.mjs": tests.join("\n") }).map((f) => f.subject);
  // disk-command.test.mjs: the directory is inside existsSync(...), not an operand of the assertion.
  assert.deepEqual(subjects(block("nested", "const mount = tempDir(t);", "const real = realpathSync(mount);", 'assert.equal(existsSync(path.join(mount, "x")), false);')), []);
  // disk-command.test.mjs: `target` comes from another call, so it is not known to be an unresolved temp root.
  assert.deepEqual(subjects(block("derived", "const base = tempDir(t);", "const target = installedPackage(base);", "realpathSync(target);", "assert.equal(resolveInstalledCli(base), target);")), []);
  // The assertion resolves the expected side too.
  assert.deepEqual(subjects(block("both sides", "const app = tempDir(t);", 'assert.equal(realpathSync(find(app)), realpathSync(path.join(app, "x")));')), []);
  // One test resolves its directory; another, with its own binding, compares without resolving.
  assert.deepEqual(subjects(block("resolves", "const app = tempDir(t);", "realpathSync(app);"), block("echoes", "const app = tempDir(t);", 'assert.equal(echo(app), path.join(app, "x"));')), []);
  // The contradiction itself, with let, a qualified realpath, a bare operand and an object operand.
  assert.deepEqual(subjects(block("let", "let dir = tmpDir();", "fs.realpathSync(dir);", "assert.equal(found.dir, dir);")), ["dir"]);
  assert.deepEqual(subjects(block("object", "const app = mkdtempSync(path.join(os.tmpdir(), 'x-'));", "realpathSync.native(app);", 'assert.deepEqual(result, { dir: path.resolve(app, "y") });')), ["app"]);
});

// ---- the thin caller: app-check.mjs's rules and ratchet, behind this file's command line and RightKit's own baseline ---------------
// The rule, export and baseline tests are packages/release/app-check.test.mjs. These run them through scripts/preflight.mjs.
const appRepo = (t, files) => {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), "rk-preflight-app-"));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  const repo = path.join(parent, "AppRepo"); // the baseline key is the lowercased basename: "apprepo"
  fs.mkdirSync(repo);
  git(repo, "init", "-q");
  for (const [k, v] of [["user.email", "t@example.test"], ["user.name", "t"], ["core.autocrlf", "false"]]) git(repo, "config", k, v);
  for (const [file, text] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, file)), { recursive: true }); fs.writeFileSync(path.join(repo, file), text); }
  git(repo, "add", "-A");
  git(repo, "commit", "-q", "-m", "app");
  return { repo, parent };
};
const preflight = async (argv, io = {}) => { const lines = []; const errs = []; const code = await main(argv, { out: (l) => lines.push(l), err: (l) => errs.push(l), ...io }); return { code, lines, text: lines.join("\n"), errs }; };
const writeBaseline = (parent, known) => { const file = path.join(parent, "baseline.json"); fs.writeFileSync(file, JSON.stringify({ schema: 1, about: "t", known })); return file; };
const KNOWN_ID = "qa-platform|tests/q.rs|platform:macos";

test("ratchet: a listed finding is KNOWN and passes; an unlisted one fails; a listed identity that is gone is STALE and fails", async (t) => {
  const { repo, parent } = appRepo(t, { "tests/q.rs": 'let r = ["platform:macos"];\n', "tests/r.rs": 'let r = ["platform:linux"];\n' });
  const args = (known, ...more) => ["--repo", repo, "--worktree", "--only", "qa-platform", "--baseline", writeBaseline(parent, known), ...more];
  const fresh = await preflight(args({}));
  assert.equal(fresh.code, 1);
  assert.match(fresh.text, /^FAIL qa-platform \(G13\): tests\/q\.rs:1 /m);
  const partial = await preflight(args({ apprepo: [KNOWN_ID] }));
  assert.equal(partial.code, 1);
  assert.match(partial.text, /^KNOWN qa-platform: tests\/q\.rs:1 /m);
  assert.match(partial.text, /^FAIL qa-platform \(G13\): tests\/r\.rs:1 /m);
  const known = await preflight(args({ apprepo: [KNOWN_ID, "qa-platform|tests/r.rs|platform:linux"] }));
  assert.equal(known.code, 0, known.text);
  assert.match(known.text, /^preflight: \w+ passed 1 rules .*\(2 known finding\(s\) in ci\/app-check-baseline\.json/m, "the summary carries preflight's label and the central file's name");
  const stale = await preflight(args({ apprepo: [KNOWN_ID, "qa-platform|tests/r.rs|platform:linux", "qa-platform|tests/gone.rs|platform:macos"] }));
  assert.equal(stale.code, 1);
  assert.match(stale.text, /^STALE baseline entry qa-platform\|tests\/gone\.rs\|platform:macos: .*remove it from ci\/app-check-baseline\.json known\["apprepo"\]/m);
});

test("preflight checks an app with its own .rightkit/app-check-baseline.json the way rightkit-gate check does, and RightKit's rules stay quiet on an app", async (t) => {
  const { repo } = appRepo(t, {
    "tests/q.rs": 'let r = ["platform:macos"];\n',
    // An app entry carries its added date and a sunset at most 90 days after it (packages/release/app-check.mjs); `now` is injected below.
    ".rightkit/app-check-baseline.json": JSON.stringify({ schema: 1, known: [{ id: KNOWN_ID, issue: "app#1", added: "2026-10-24", sunset: addDays("2026-10-24", 60) }] }),
    "crates/app-core/Cargo.toml": '[package]\nname = "app-core"\nversion = "0.1.0"\n',
  });
  const result = await preflight(["--repo", repo, "--commit", "HEAD"], { now: new Date(`${addDays("2026-10-24", 7)}T12:00:00Z`) });
  assert.equal(result.code, 0, result.text);
  assert.match(result.text, /^KNOWN qa-platform: tests\/q\.rs:1 /m);
  assert.match(result.text, /passed 23 rules .*\(1 known finding\(s\) in \.rightkit\/app-check-baseline\.json/);
});

test("preflight keeps its rule order: the app rules sit where they always did among RightKit's own", () => {
  assert.deepEqual(RULES.map((r) => `${r.id} (${r.gap})`), [
    "eol-policy (G04)", "eol-pinned (G25)", "consumer-recipe (G01)", "npm-recipe (G01)", "npm-surface (G02)", "version-pins (G03)", "feed-legacy (G27)", "dispatch-pins (G05)", "consumer-locks (G08)", "file-url (G12)", "generated-docs (G10)", "tmpdir-realpath (G26)", "retired-alias (P11)",
    "qa-platform (G13)", "test-harness (G14)", "scenario-skip (G15)", "release-config (G16)", "skip-checks (G17)", "qa-dev-server (G18)", "ffmpeg-release-escape (G19)", "switches-inventory (G20)", "speech-link (G21)", "e2e-under-cargo-test (G22)",
  ]);
});

test("ratchet: this checkout's baseline keys one entry per repository with known debt", () => {
  const baseline = JSON.parse(fs.readFileSync(new URL("../ci/app-check-baseline.json", import.meta.url), "utf8"));
  assert.equal(baseline.schema, 1);
  assert.deepEqual(Object.keys(baseline.known).sort(), ["coderight", "cutright", "genright", "mailright", "rightkit", "viewright"], "one entry per repository with known debt; apps with none are absent");
});

test("scripts/retire-rightkit-alias.mjs still finds what it takes from preflight: the exempt list, the generated-file test and the rewrite", async () => {
  const preflightModule = await import("./preflight.mjs");
  assert.equal(typeof preflightModule.retireAliasText, "function");
  assert.equal(typeof preflightModule.isGeneratedFile, "function");
  assert.ok(preflightModule.RETIRED_ALIAS_EXEMPT.test("scripts/preflight.mjs") && preflightModule.RETIRED_ALIAS_EXEMPT.test("packages/release/app-check.mjs") && preflightModule.RETIRED_ALIAS_EXEMPT.test("packages/release/app-check.test.mjs"));
  assert.equal(preflightModule.RETIRED_ALIAS_EXEMPT.test("packages/release/cli/right-release.mjs"), false);
  const dry = spawnSync(process.execPath, [fileURLToPath(new URL("./retire-rightkit-alias.mjs", import.meta.url)), "--dry-run", "--repo", fileURLToPath(new URL("../desktop-app-template", import.meta.url))], { encoding: "utf8" });
  assert.equal(dry.status, 0, dry.stderr);
  assert.match(dry.stdout, /invocation\(s\) in \d+ file\(s\) would move to rustright/);
});
