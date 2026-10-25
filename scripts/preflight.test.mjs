import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { applyBaseline, baselineKnown, cargoPackage, cargoTestTargets, lockPathPackages, main, RULES, runRules } from "./preflight.mjs";

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
  const failures = run("consumer-locks", t, { "crates/rightkit-a/Cargo.toml": crate, "ci/consumers/wv2/Cargo.lock": stale, "spikes/old/Cargo.lock": stale });
  assert.equal(failures.length, 1);
  assert.match(failures[0].problem, /^ci\/consumers\/wv2\/Cargo\.lock lists rightkit-a 0\.6\.0 but crates\/rightkit-a\/Cargo\.toml is 0\.6\.1/);
  assert.match(failures[0].fix, /version = "0\.6\.1"/);
});

test("file-url refuses .pathname on an import.meta.url URL and accepts fileURLToPath", (t) => {
  // Joined at run time so this file does not itself carry the pattern the rule refuses.
  const meta = ["import", "meta", "url"].join(".");
  const failures = run("file-url", t, {
    "a.test.mjs": `const a = new URL(${meta}).pathname;\nconst b = new URL("../..", ${meta}).pathname.replace(/x/, "");\n`,
    "b.mjs": `const c = fileURLToPath(new URL("..", ${meta}));\n`,
  });
  assert.deepEqual(failures.map((f) => f.problem.split(" ")[0]), ["a.test.mjs:1", "a.test.mjs:2"]);
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
  // Only PATH (to find git and tar) and a throwaway HOME; every node child (the generated-docs check) inherits the guard.
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

// ---- P11 of docs/PIPELINE-ARCHITECTURE.md: one client name ---------------------------------------------------------------
// Spelled at run time so the retired-alias sweep never rewrites these fixtures.
const OLD = ["right", "kit"].join("");

test("P11: retired-alias finds every invocation form and spares history, generated files and look-alikes", (t) => {
  const failures = run("retired-alias", t, {
    "package.json": `{ "scripts": { "test:rust": "${OLD} cargo test" } }\n`,
    "scripts/build.mjs": `spawnSync("${OLD}", ["cargo", "build"]);\nconst c = isWindows ? "${OLD}.cmd" : "${OLD}";\n`,
    "journeys.toml": `[build]\ncommand = ["${OLD}", "cargo", "build"]\n`,
    "src/lib.rs": `let c = Command::new("${OLD}");\nlet v = vec!["${OLD}".into(), "cargo".into()];\n`,
    "right-release.config.mjs": `export default { stage: { cmd: "${OLD}", args: ["cargo"] } };\n`,
    "README.md": `Run \`${OLD} status <id>\`.\n`,
    // spared: history, generated files, and names that only look alike
    "CHANGELOG.md": `- ran ${OLD} cargo test\n`,
    "docs/plans/2026-10-20-plan.md": `${OLD} cargo build\n`,
    "AGENTS.md": `<!-- GENERATED FILE. Do not hand-edit. Source: docs/agent-rules.md -->\n${OLD} cargo test\n`,
    "notes.md": `${OLD}-qa run, @${OLD}/release build, the ${OLD} build-control plane, crates/${OLD} cargo, ["cargo", "rustc", "${OLD}", "rustup"]\n`,
  });
  assert.deepEqual(failures.map((f) => f.problem.split(" ")[0]).sort(), ["README.md:1", "journeys.toml:2", "package.json:1", "right-release.config.mjs:1", "scripts/build.mjs:1,2", "src/lib.rs:1,2"]);
  assert.match(failures[0].fix, /rustright/);
});

test("P11: retired-alias finds the forms the first sweep missed (Astra's review of #945 and its app PRs)", (t) => {
  const failures = run("retired-alias", t, {
    // heardright verify-local.mjs: [command, [args]] pairs, one element per line, a comment between, and a .cmd mapping
    "scripts/verify-local.mjs": `const plan = [\n  [\n    "${OLD}",\n    // test-mode keychain overrides\n    ["cargo", "test"],\n  ],\n  ["${OLD}", ["cargo", "test", "--test", "contract"]],\n];\nif (platform === "win32" && command === "${OLD}") return ["rustright.cmd", args];\n`,
    // heardright tsf-tip.ps1, coderight-profile-ab.ps1, genright qualify-local-image-backend.mjs
    "scripts/tsf-tip.ps1": `$client = Get-Command ${OLD} -ErrorAction Stop\n$out = @(& ${OLD} @arguments 2>&1)\n`,
    "scripts/qualify.mjs": "const command = `& ${OLD} ${args.join(' ')}`;\n".replace("${OLD}", OLD),
    // viewright dev-tauri-bench.mjs, scraperight generate-rust-license-appendix.mjs, deterministic-scans.mjs PROBES
    "scripts/bench.mjs": `const command = process.platform === "win32" ? (process.env.ComSpec ?? "cmd.exe") : "${OLD}";\nconst CARGO_COMMAND = "${OLD}";\nconst PROBES = { node: ["--version"], ${OLD}: ["cargo", "--version"] };\n`,
    // spared: refusals that say so, data, English, paths
    "src/refuse.mjs": `if (name === "${OLD}") return refuse("R1-retired-client");\nconst RETIRED_BROKER = "${OLD}";\n`,
    "data/update.json": `{ "kind": "${OLD}", "identity": "${OLD}" }\n`,
    "src/asr.rs": `// a test-mode redirect, which ${OLD} also refuses to resolve\nlet p = root.join("${OLD}");\n`,
  });
  assert.deepEqual(failures.map((f) => f.problem.split(" ")[0]).sort(), ["scripts/bench.mjs:1,2,3", "scripts/qualify.mjs:1", "scripts/tsf-tip.ps1:1,2", "scripts/verify-local.mjs:3,7,9"]);
});

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

// ---- G13-G20: app defects from files alone, and the ratchet --------------------------------------------------------------
const identities = (failures) => failures.map((f) => f.identity).sort();

test("G13 qa-platform: a platform requirement other than darwin or windows skips forever", (t) => {
  const failures = run("qa-platform", t, {
    "tests/bad.rs": '    // "platform:linux" in a comment is prose\nlet r = vec!["platform:macos", "platform:darwin"];\nlet s = ["platform:windows", "platform:Windows"];\n',
    "tests/good.rs": 'let r = ["platform:darwin", "platform:windows", "env:HOME"];\n',
    "vendor/x/y.rs": 'let r = "platform:macos";\n',
    "notes.md": '"platform:macos"\n',
  });
  assert.deepEqual(identities(failures), ["qa-platform|tests/bad.rs|platform:Windows", "qa-platform|tests/bad.rs|platform:macos"]);
  assert.match(failures.find((f) => f.subject === "platform:Windows").problem, /^tests\/bad\.rs:3 requirement "platform:Windows" never matches/);
  assert.match(failures.find((f) => f.subject === "platform:macos").problem, /^tests\/bad\.rs:2 /);
  assert.match(failures[0].fix, /platform:darwin/);
});

test("G14 test-harness: a [[test]] with a top-level main needs harness = false; #[test] files are fine", (t) => {
  const failures = run("test-harness", t, {
    "engine/Cargo.toml": [
      '[package]\nname = "e"\n',
      '[[test]]\nname = "matrix_runner"\npath = "tests/matrix/runner.rs"\n',
      '[[test]]\nname = "matrix_ok"\npath = "tests/matrix/ok.rs"\nharness = false\n',
      '[[test]]\nname = "plain"\n',
      '[[test]]\nname = "implicit"\n# a table with no path: tests/implicit.rs\n',
      '[[bin]]\nname = "b"\npath = "tests/bin_main.rs"\n',
    ].join("\n"),
    "engine/tests/matrix/runner.rs": "fn main() {\n    run();\n}\n",
    "engine/tests/matrix/ok.rs": "fn main() {}\n",
    "engine/tests/plain.rs": "#[test]\nfn works() {}\nmod m { fn main() {} }\n",
    "engine/tests/implicit.rs": "pub async fn main() {}\n",
    "engine/tests/bin_main.rs": "fn main() {}\n",
  });
  assert.deepEqual(identities(failures), ["test-harness|engine/Cargo.toml|implicit", "test-harness|engine/Cargo.toml|matrix_runner"]);
  assert.match(failures.find((f) => f.subject === "matrix_runner").problem, /^engine\/Cargo\.toml:4 \[\[test\]\] matrix_runner defines fn main but does not set harness = false/);
  assert.deepEqual(cargoTestTargets('[[test]]\nname = "a"\nharness = false # why\n[dependencies]\nharness = false\n'), [{ name: "a", path: undefined, harness: false, line: 1 }]);
});

test("G15 scenario-skip: SKIP then return inside a scenario file is a skip that reports green", (t) => {
  const skip = (extra) => `    println!("SKIP: no display");\n${extra}`;
  const failures = run("scenario-skip", t, {
    "crates/ui/tests/a.rs": `fn a() { Suite::new().scenario("x"); }\nfn t1() {\n${skip("    return;\n")}}\nfn t2() {\n    eprintln!("SKIP second");\n    if x {\n\n        return;\n    }\n}\nfn t3() {\n    println!("SKIP far");\n    a();\n    b();\n    c();\n    return;\n}\nfn t4() {\n    println!("SKIP: fine");\n    assert!(ok);\n}\n// println!("SKIP: comment");\n`,
    "crates/ui/tests/no_scenario.rs": `fn t() {\n${skip("    return;\n")}}\n`,
  });
  assert.deepEqual(identities(failures), ["scenario-skip|crates/ui/tests/a.rs|crates/ui/tests/a.rs#1", "scenario-skip|crates/ui/tests/a.rs|crates/ui/tests/a.rs#2"]);
  assert.match(failures[0].problem, /^crates\/ui\/tests\/a\.rs:3 prints SKIP and returns/);
});

test("G16 release-config: an unknown provider, an updater keyFile and an undefined Cargo profile", (t) => {
  const bad = [
    'export default {',
    '  distribution: { provider: "r2" },',
    '  targets: [{ updater: { artifacts: [], keyFile: process.env.KEY } }],',
    '  build: { args: ["--", "--profile", "release-iterate", "--profile", "dev", "--profile", "defined-here"] },',
    '  other: process.env.PROFILE ?? "fast-dev",',
    '  // provider: "nope", keyFile: "x"',
    '};',
  ].join("\n");
  const failures = run("release-config", t, {
    "app/right-release.config.mjs": bad,
    "app/Cargo.toml": '[workspace]\n[profile.defined-here]\ninherits = "dev"\n',
    "good/right-release.config.mjs": 'export default { distribution: { provider: \'github-releases\' }, a: ["--profile", "release-iterate"], u: { updater: { artifacts: [] } } };\n',
    "good/Cargo.toml": '[workspace]\n[profile.release-iterate.package."*"]\nopt-level = 1\n',
    "node_modules/x/right-release.config.mjs": 'export default { distribution: { provider: "bad" } };\n',
  });
  assert.deepEqual(identities(failures), [
    "release-config|app/right-release.config.mjs|keyFile:process.env.KEY",
    "release-config|app/right-release.config.mjs|profile:fast-dev",
    "release-config|app/right-release.config.mjs|profile:release-iterate",
    "release-config|app/right-release.config.mjs|provider:r2",
  ]);
  assert.match(failures.find((f) => f.subject === "profile:release-iterate").problem, /^app\/right-release\.config\.mjs:4 Cargo profile "release-iterate" is not dev, release, test or bench/);
  assert.match(failures.find((f) => f.subject === "provider:r2").problem, /publish-update\.mjs/);
});

test("G16 release-config: the nearest workspace Cargo.toml decides, up to the repository root", (t) => {
  const config = 'export default { a: ["--profile", "p1"] };\n';
  const files = { "apps/x/right-release.config.mjs": config, "apps/x/Cargo.toml": '[package]\nname = "member"\n[profile.p1]\n', "Cargo.toml": '[workspace]\nmembers = ["apps/x"]\n' };
  assert.equal(run("release-config", t, files).length, 1, "a member's own [profile] is ignored by cargo; the workspace root has none");
  assert.deepEqual(run("release-config", t, { ...files, "Cargo.toml": '[workspace]\n[profile.p1]\n' }), []);
  assert.deepEqual(run("release-config", t, { "apps/x/right-release.config.mjs": config, "engine/Cargo.toml": "[workspace]\n[profile.p1]\n" }), [], "no manifest on the way up: any manifest's profile counts");
});

test("G17 skip-checks and G18 qa-dev-server read package.json scripts", (t) => {
  const files = {
    "package.json": JSON.stringify({ scripts: { release: "right-release build --skip-checks", ok: "right-release build", other: "node x.mjs --skip-checks", qa: "tauri dev", "qa:e2e": "pnpm exec tauri dev --features qa", dev: "tauri dev", "qa-build": "tauri build --debug" } }),
    "app/package.json": JSON.stringify({ scripts: { ship: "pnpm exec right-release upload --skip-checks --tier patch" } }),
    "node_modules/x/package.json": JSON.stringify({ scripts: { release: "right-release --skip-checks", qa: "tauri dev" } }),
    "broken/package.json": "{ not json",
  };
  assert.deepEqual(identities(run("skip-checks", t, files)), ["skip-checks|app/package.json|ship", "skip-checks|package.json|release"]);
  const qa = run("qa-dev-server", t, files);
  assert.deepEqual(identities(qa), ["qa-dev-server|package.json|qa", "qa-dev-server|package.json|qa:e2e"]);
  assert.match(qa[0].problem, /^package\.json:1 script "qa" runs `tauri dev`/);
});

test("G19 ffmpeg-release-escape: allow_in_release_builds only in tests and crates/rightkit-ffmpeg", (t) => {
  const failures = run("ffmpeg-release-escape", t, {
    "src/encode.rs": "fn ok() {}\nlet cfg = Gate { allow_in_release_builds: true };\n",
    "src/commented.rs": "// allow_in_release_builds is refused\nfn ok() {}\n",
    "src/with_tests.rs": "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn t() { Gate { allow_in_release_builds: true }; }\n}\n",
    "src/all_test.rs": "#![cfg(test)]\nfn t() { Gate { allow_in_release_builds: true }; }\n",
    "tests/ffmpeg.rs": "Gate { allow_in_release_builds: true };\n",
    "crates/app/tests/x.rs": "Gate { allow_in_release_builds: true };\n",
    "crates/rightkit-ffmpeg/src/gate.rs": "pub allow_in_release_builds: bool,\n",
  });
  assert.deepEqual(identities(failures), ["ffmpeg-release-escape|src/encode.rs|src/encode.rs"]);
  assert.match(failures[0].problem, /^src\/encode\.rs:2 uses allow_in_release_builds outside test code/);
});

test("G19 ffmpeg-release-escape: DevFfmpeg::unverified outside test code", (t) => {
  const rs = (...lines) => `${lines.join("\n")}\n`;
  const fixtures = {
    // Refused: [line of the first hit, the file]
    "src/plain.rs": rs("fn start() {", "    let ffmpeg = DevFfmpeg::unverified(path);", "}"),
    "src/qualified.rs": rs("fn start() {", "    let f = rightkit_ffmpeg::DevFfmpeg::unverified(path);", "}"),
    "src/spaced.rs": rs("fn start() {", "    let f = DevFfmpeg", "        ::unverified", "        (path);", "}"),
    "src/real_then_cfg_mod.rs": rs("fn real() { DevFfmpeg::unverified(path); }", "#[cfg(test)]", "mod tests {", '    fn t() { DevFfmpeg::unverified("x"); }', "}"),
    "src/after_gate.rs": rs('#[cfg(feature = "dev-overrides")]', "fn qa() -> DevFfmpeg {", '    DevFfmpeg::unverified("ffmpeg")', "}", "fn real() -> DevFfmpeg {", "    DevFfmpeg::unverified(path)", "}"),
    "src/commented_gate.rs": rs('// #[cfg(feature = "dev-overrides")]', "fn f() { DevFfmpeg::unverified(p); }"),
    "src/block_comment_gate.rs": rs('/* #[cfg(feature = "dev-overrides")] */', "fn f() { DevFfmpeg::unverified(p); }"),
    "src/other_feature.rs": rs('#[cfg(feature = "qa")]', "fn f() { DevFfmpeg::unverified(p); }"),
    "src/negated_gate.rs": rs('#[cfg(not(feature = "dev-overrides"))]', "fn f() { DevFfmpeg::unverified(p); }"),
    "src/mod_decl.rs": rs('#[cfg(feature = "dev-overrides")]', "mod qa;", "fn f() { DevFfmpeg::unverified(p); }"),
    "src/gated_field.rs": rs("fn build() -> Cfg {", "    Cfg {", '        #[cfg(feature = "dev-overrides")]', '        ffmpeg: DevFfmpeg::unverified("ffmpeg"),', "    }", "}", "fn real() { DevFfmpeg::unverified(path); }"),
    "src/after_gated_statement.rs": rs("fn f() {", '    #[cfg(feature = "dev-overrides")]', '    let _ = DevFfmpeg::unverified("ffmpeg");', "    let real = DevFfmpeg::unverified(path);", "}"),
    "src/path_use.rs": rs("fn f(paths: Vec<PathBuf>) -> Vec<DevFfmpeg> {", "    paths.into_iter().map(DevFfmpeg::unverified).collect()", "}"),
    "src/path_use_spaced.rs": rs("fn f() {", "    let make = rightkit_ffmpeg::DevFfmpeg", "        ::unverified;", "}"),
    "src/arm_then_arm.rs": rs("fn f(mode: Mode) -> DevFfmpeg {", "    match mode {", '        #[cfg(feature = "dev-overrides")]', '        Mode::Qa => DevFfmpeg::unverified("ffmpeg"),', "        Mode::Real => DevFfmpeg::unverified(path),", "    }", "}"),
    "src/arm_then_arm_stacked.rs": rs("fn f(mode: Mode) -> DevFfmpeg {", "    match mode {", '        #[cfg(any(test, feature = "dev-overrides"))]', "        #[allow(unused)]", '        Mode::Qa => DevFfmpeg::unverified("ffmpeg"),', "        Mode::Real => DevFfmpeg::unverified(path),", "    }", "}"),
    "src/late_inner_gate.rs": rs("fn real() { DevFfmpeg::unverified(path); }", '#![cfg(feature = "dev-overrides")]'),
    "src/other_inner_gate.rs": rs('#![cfg(feature = "qa")]', "fn f() { DevFfmpeg::unverified(p); }"),
    "src/inner_gate_allow.rs": rs('#![cfg(feature = "dev-overrides")]', "fn f() { Gate { allow_in_release_builds: true }; }"),
    "src/both.rs": rs("fn a() {", "    let c = Gate { allow_in_release_builds: true };", "    let f = DevFfmpeg::unverified(path);", "}"),
    "src/allow_gated.rs": rs('#[cfg(feature = "dev-overrides")]', "fn f() { Gate { allow_in_release_builds: true }; }"),
    // Excused
    "src/real_then_tests_mod.rs": rs("fn real() {}", "#[cfg(test)]", "mod tests {", '    fn t() { DevFfmpeg::unverified("ffmpeg"); }', "}"),
    "src/whole_file_cfg.rs": rs("#![cfg(test)]", 'fn t() { DevFfmpeg::unverified("ffmpeg"); }'),
    "src/render_test.rs": rs('fn t() { DevFfmpeg::unverified("ffmpeg"); }'),
    "tests/ffmpeg.rs": rs('DevFfmpeg::unverified("ffmpeg");'),
    "crates/app/tests/x.rs": rs('DevFfmpeg::unverified("ffmpeg");'),
    "crates/rightkit-ffmpeg/src/dev.rs": rs("pub fn f() -> DevFfmpeg { DevFfmpeg::unverified(p) }"),
    "src/commented.rs": rs("// DevFfmpeg::unverified(path) is for tests", "/// DevFfmpeg::unverified(path) in a doc comment", "//! DevFfmpeg::unverified(path) in a module doc", "fn ok() {}"),
    "src/literals.rs": rs(
      'const HINT: &str = "call DevFfmpeg::unverified(path) in tests";',
      'const RAW: &str = r#"DevFfmpeg::unverified("x")"#;',
      "fn ok() {} // DevFfmpeg::unverified(path) in a trailing comment",
      "/* DevFfmpeg::unverified(path) in a block comment */",
    ),
    "src/gate_feature.rs": rs('#[cfg(feature = "dev-overrides")]', "fn qa_ffmpeg() -> DevFfmpeg {", '    DevFfmpeg::unverified("ffmpeg")', "}"),
    "src/gate_any.rs": rs('#[cfg(any(test, feature = "dev-overrides"))]', "pub mod qa {", '    pub fn f() -> DevFfmpeg { DevFfmpeg::unverified("ffmpeg") }', "}"),
    "src/gate_impl.rs": rs('#[cfg(feature = "dev-overrides")]', "impl Qa {", "    fn ffmpeg(&self) -> DevFfmpeg { DevFfmpeg::unverified(&self.path) }", "}"),
    "src/gate_block.rs": rs("fn f() {", '    #[cfg(feature = "dev-overrides")]', "    {", '        let _ = DevFfmpeg::unverified("ffmpeg");', "    }", "}"),
    "src/gate_stacked.rs": rs('#[cfg(feature = "dev-overrides")]', "#[allow(dead_code)]", "pub fn qa() -> DevFfmpeg {", '    DevFfmpeg::unverified("ffmpeg")', "}"),
    "src/gate_statement.rs": rs("fn f() {", '    #[cfg(feature = "dev-overrides")]', '    let _ = DevFfmpeg::unverified("ffmpeg");', "}"),
    "src/gate_braces_in_literals.rs": rs('#[cfg(feature = "dev-overrides")]', "fn qa() -> DevFfmpeg {", '    let close = "}";', "    let also = '}';", '    let note = r#"close with "}" now"#;', "    DevFfmpeg::unverified(close)", "}"),
    "src/gate_array_signature.rs": rs('#[cfg(feature = "dev-overrides")]', "fn qa(buf: [u8; 4]) -> DevFfmpeg {", "    DevFfmpeg::unverified(path)", "}"),
    "src/gate_lifetime.rs": rs('#[cfg(feature = "dev-overrides")]', "fn qa<'a>(path: &'a str) -> DevFfmpeg {", "    DevFfmpeg::unverified(path)", "}"),
    "src/gate_spelling.rs": rs('#[cfg( feature = "dev-overrides" )]', "fn a() { DevFfmpeg::unverified(p); }", '#[cfg(any(feature = "dev-overrides", test))]', "fn b() { DevFfmpeg::unverified(p); }"),
    "src/gate_arm.rs": rs("fn f(mode: Mode) -> DevFfmpeg {", "    match mode {", '        #[cfg(feature = "dev-overrides")]', '        Mode::Qa => DevFfmpeg::unverified("ffmpeg"),', "        Mode::Real => resolve(path),", "    }", "}"),
    "src/gate_arm_block.rs": rs("fn f(mode: Mode) -> DevFfmpeg {", "    match mode {", '        #[cfg(feature = "dev-overrides")]', '        Mode::Qa => { DevFfmpeg::unverified("ffmpeg") }', "        Mode::Real => resolve(path),", "    }", "}"),
    "src/gate_generic_fn.rs": rs('#[cfg(feature = "dev-overrides")]', "pub fn qa<A, B>(a: A, b: B) -> DevFfmpeg {", '    DevFfmpeg::unverified("ffmpeg")', "}"),
    "src/gate_path_use.rs": rs('#[cfg(feature = "dev-overrides")]', "fn all(paths: Vec<PathBuf>) -> Vec<DevFfmpeg> {", "    paths.into_iter().map(DevFfmpeg::unverified).collect()", "}"),
    "src/inner_gate_feature.rs": rs('//! QA-only helpers.', '#![cfg(feature = "dev-overrides")]', "fn qa() -> DevFfmpeg { DevFfmpeg::unverified(path) }", "fn all(p: Vec<PathBuf>) { p.into_iter().map(DevFfmpeg::unverified); }"),
    "src/inner_gate_any.rs": rs('#![cfg(any(test, feature = "dev-overrides"))]', "fn qa() -> DevFfmpeg { DevFfmpeg::unverified(path) }"),
    "src/inner_gate_after_attrs.rs": rs("#![allow(dead_code)]", '#![cfg( any( feature = "dev-overrides", test ) )]', "fn qa() -> DevFfmpeg { DevFfmpeg::unverified(path) }"),
    "src/lookalike.rs": rs("fn f() { MyDevFfmpeg::unverified(p); other::NotDevFfmpeg::unverified(p); DevFfmpeg_unverified(p); DevFfmpeg::unverified_for_docs(p); DevFfmpeg::try_unverified; }"),
    "src/resolver.rs": rs("fn f(p: &Path) {", "    let request = UnverifiedRequest::new(p);", "    let handle = resolve_ffmpeg_handle(&request);", "    let checked = DevFfmpeg::try_unverified(p);", "}"),
  };
  const refused = {
    "src/plain.rs": 2, "src/qualified.rs": 2, "src/spaced.rs": 2, "src/real_then_cfg_mod.rs": 1, "src/after_gate.rs": 6, "src/commented_gate.rs": 2, "src/block_comment_gate.rs": 2, "src/other_feature.rs": 2, "src/negated_gate.rs": 2, "src/mod_decl.rs": 3, "src/after_gated_statement.rs": 4, "src/gated_field.rs": 7,
    "src/path_use.rs": 2, "src/path_use_spaced.rs": 2, "src/arm_then_arm.rs": 5, "src/arm_then_arm_stacked.rs": 6, "src/late_inner_gate.rs": 1, "src/other_inner_gate.rs": 2,
  };
  const failures = run("ffmpeg-release-escape", t, fixtures);
  assert.deepEqual(identities(failures), [...Object.keys(refused), "src/both.rs", "src/allow_gated.rs", "src/inner_gate_allow.rs"].map((f) => `ffmpeg-release-escape|${f}|${f}`).sort());
  for (const [file, line] of Object.entries(refused)) {
    const failure = failures.find((f) => f.file === file);
    assert.ok(failure.problem.startsWith(`${file}:${line} uses DevFfmpeg::unverified outside test code: `), failure.problem);
    assert.match(failure.fix, /resolve_ffmpeg_handle.*dev-overrides/);
  }
  const both = failures.find((f) => f.file === "src/both.rs");
  assert.match(both.problem, /^src\/both\.rs:2 uses allow_in_release_builds and src\/both\.rs:3 uses DevFfmpeg::unverified outside test code: /);
  assert.match(both.fix, /remove it from non-test code.*resolve_ffmpeg_handle/);
  const gatedAllow = failures.find((f) => f.file === "src/allow_gated.rs");
  assert.match(gatedAllow.problem, /^src\/allow_gated\.rs:2 uses allow_in_release_builds outside test code: /);
  assert.doesNotMatch(gatedAllow.problem, /DevFfmpeg/);
  const innerAllow = failures.find((f) => f.file === "src/inner_gate_allow.rs");
  assert.match(innerAllow.problem, /^src\/inner_gate_allow\.rs:2 uses allow_in_release_builds outside test code: /, "an inner dev-overrides gate excuses DevFfmpeg::unverified only");
  assert.doesNotMatch(innerAllow.problem, /DevFfmpeg/);
});

test("G20 switches-inventory: a release config directory needs switches.json or an existing releaseSwitches.inventory", (t) => {
  const failures = run("switches-inventory", t, {
    "right-release.config.mjs": "export default {};\n",
    "app/right-release.config.mjs": "export default {};\n",
    "app/switches.json": "{}\n",
    "other/right-release.config.mjs": 'export default { releaseSwitches: { inventory: "config/inv.json" } };\n',
    "other/config/inv.json": "{}\n",
    "missing/right-release.config.mjs": 'export default {\n  releaseSwitches: { inventory: "nope.json" },\n};\n',
    "node_modules/x/right-release.config.mjs": "export default {};\n",
  });
  assert.deepEqual(identities(failures), ["switches-inventory|missing/right-release.config.mjs|missing", "switches-inventory|right-release.config.mjs|."]);
  assert.match(failures.find((f) => f.subject === "missing").problem, /^missing\/right-release\.config\.mjs:2 has no switches inventory \(missing\/nope\.json does not exist\)/);
});

test("G21 speech-link: import Speech is refused anywhere under RightKitSwift Sources and Tests, allowed only in the LinkProbes control", (t) => {
  const base = "swift/RightKitSwift";
  const failures = run("speech-link", t, {
    [`${base}/Sources/RightKitSpeech/A.swift`]: "import Foundation\nimport Speech\n",
    [`${base}/Sources/RightKitAudio/B.swift`]: "@preconcurrency import Speech\n",
    [`${base}/Tests/RightKitSpeechTests/C.swift`]: "import struct Speech.SFSpeechRecognizer\n",
    [`${base}/Sources/RightKitArtifacts/D.swift`]: "// import Speech\nimport RightKitSpeech\n",
    [`${base}/LinkProbes/SpeechNoApple/Sources/SpeechControl/E.swift`]: "import Speech\n",
    [`${base}/Sources/RightKitTTS/F.swift`]: "import Speech\n",
    [`${base}/Sources/RightKitSpeech/G.swift`]: "public import Speech\n",
    [`${base}/Sources/RightKitAudio/H.swift`]: "internal import Speech\n",
    [`${base}/Sources/RightKitArtifacts/I.swift`]: "package import Speech\n",
    [`${base}/Sources/RightKitSpeech/J.swift`]: "private import Speech\n",
    [`${base}/Sources/RightKitSpeech/K.swift`]: "fileprivate import Speech\n",
    [`${base}/Tests/RightKitSpeechTests/L.swift`]: "@preconcurrency public import Speech\n",
    [`${base}/Sources/RightKitSpeech/M.swift`]: "@_implementationOnly import Speech\n",
    [`${base}/Sources/RightKitAudio/N.swift`]: "@_spi(Internal) import Speech\n",
    [`${base}/Sources/RightKitArtifacts/O.swift`]: "open import Speech\n",
    [`${base}/Tests/RightKitTTSTests/P.swift`]: "public import Speech\n",
  });
  assert.deepEqual(failures.map((f) => f.file).sort(), [
    `${base}/Sources/RightKitArtifacts/I.swift`,
    `${base}/Sources/RightKitArtifacts/O.swift`,
    `${base}/Sources/RightKitAudio/B.swift`,
    `${base}/Sources/RightKitAudio/H.swift`,
    `${base}/Sources/RightKitAudio/N.swift`,
    `${base}/Sources/RightKitSpeech/A.swift`,
    `${base}/Sources/RightKitSpeech/G.swift`,
    `${base}/Sources/RightKitTTS/F.swift`,
    `${base}/Sources/RightKitSpeech/J.swift`,
    `${base}/Sources/RightKitSpeech/K.swift`,
    `${base}/Sources/RightKitSpeech/M.swift`,
    `${base}/Tests/RightKitSpeechTests/C.swift`,
    `${base}/Tests/RightKitSpeechTests/L.swift`,
    `${base}/Tests/RightKitTTSTests/P.swift`,
  ].sort());
  assert.match(failures.find((f) => f.file.endsWith("A.swift")).problem, /A\.swift:2 imports Speech/);
});

test("G01 recipe rules do not apply to an app: no recipe inventory or version feed means nothing to check", (t) => {
  const { root } = tree(t, { "crates/app-core/Cargo.toml": '[package]\nname = "app-core"\nversion = "0.1.0"\n', "packages/ui/package.json": JSON.stringify({ name: "@app/ui", version: "1.0.0" }) });
  assert.deepEqual(runRules({ root, files: [], only: ["consumer-recipe", "npm-recipe"] }).failures, []);
});

// ---- the ratchet: ci/app-check-baseline.json ------------------------------------------------------------------------------
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
const preflight = async (argv) => { const lines = []; const errs = []; const code = await main(argv, { out: (l) => lines.push(l), err: (l) => errs.push(l) }); return { code, lines, text: lines.join("\n"), errs }; };
const writeBaseline = (parent, known) => { const file = path.join(parent, "baseline.json"); fs.writeFileSync(file, JSON.stringify({ schema: 1, about: "t", known })); return file; };
const BAD_QA = { "tests/q.rs": 'let r = ["platform:macos"];\n' };
const KNOWN_ID = "qa-platform|tests/q.rs|platform:macos";

test("ratchet: a listed finding is KNOWN and passes; an unlisted one fails; a listed identity that is gone is STALE and fails", async (t) => {
  const { repo, parent } = appRepo(t, { ...BAD_QA, "tests/r.rs": 'let r = ["platform:linux"];\n' });
  const args = (known, ...more) => ["--repo", repo, "--worktree", "--only", "qa-platform", "--baseline", writeBaseline(parent, known), ...more];
  // new: not listed at all
  const fresh = await preflight(args({}));
  assert.equal(fresh.code, 1);
  assert.match(fresh.text, /^FAIL qa-platform \(G13\): tests\/q\.rs:1 /m);
  // partly listed: the listed one is KNOWN, the other still fails
  const partial = await preflight(args({ apprepo: [KNOWN_ID] }));
  assert.equal(partial.code, 1);
  assert.match(partial.text, /^KNOWN qa-platform: tests\/q\.rs:1 /m);
  assert.match(partial.text, /^FAIL qa-platform \(G13\): tests\/r\.rs:1 /m);
  // all listed: green, and the summary says how many are tolerated
  const known = await preflight(args({ apprepo: [KNOWN_ID, "qa-platform|tests/r.rs|platform:linux"] }));
  assert.equal(known.code, 0, known.text);
  assert.doesNotMatch(known.text, /^FAIL/m);
  assert.match(known.text, /^preflight: \w+ passed 1 rules .*\(2 known finding\(s\)/m);
  // another repository's entries do not excuse this one
  assert.equal((await preflight(args({ otherapp: [KNOWN_ID, "qa-platform|tests/r.rs|platform:linux"] }))).code, 1);
  // stale: the identity is listed, nothing finds it
  const stale = await preflight(args({ apprepo: [KNOWN_ID, "qa-platform|tests/r.rs|platform:linux", "qa-platform|tests/gone.rs|platform:macos"] }));
  assert.equal(stale.code, 1);
  assert.match(stale.text, /^STALE baseline entry qa-platform\|tests\/gone\.rs\|platform:macos: .*remove it from ci\/app-check-baseline\.json/m);
  // --only another rule leaves this rule's entries alone (they cannot be judged stale by a run that skipped the rule)
  const other = await preflight(["--repo", repo, "--worktree", "--only", "skip-checks", "--baseline", writeBaseline(parent, { apprepo: [KNOWN_ID] })]);
  assert.equal(other.code, 0, other.text);
});

test("ratchet: --print-baseline prints the current identities as JSON and writes nothing; a bad baseline is exit 2", async (t) => {
  const { repo, parent } = appRepo(t, { ...BAD_QA, "package.json": JSON.stringify({ scripts: { qa: "tauri dev" } }) });
  const file = writeBaseline(parent, { apprepo: ["qa-platform|stale|x"] });
  const before = fs.readFileSync(file, "utf8");
  const printed = await preflight(["--repo", repo, "--worktree", "--print-baseline", "--baseline", file]);
  assert.equal(printed.code, 0);
  assert.deepEqual(JSON.parse(printed.text), ["qa-dev-server|package.json|qa", KNOWN_ID]);
  assert.equal(fs.readFileSync(file, "utf8"), before, "preflight never writes the baseline");
  fs.writeFileSync(file, "{ nope");
  const broken = await preflight(["--repo", repo, "--worktree", "--baseline", file]);
  assert.equal(broken.code, 2);
  assert.match(broken.errs.join("\n"), /cannot read the baseline/);
  assert.equal((await preflight(["--repo", repo, "--worktree", "--baseline", path.join(parent, "absent.json")])).code, 2, "an explicit --baseline must exist");
  assert.throws(() => baselineKnown(JSON.stringify({ known: { a: "x" } }), "a"));
  assert.deepEqual(baselineKnown(null, "a"), []);
});

test("ratchet: applyBaseline splits findings and only rules that ran can go stale; this checkout's baseline is exactly its current findings", () => {
  const found = [{ rule: "r1", identity: "r1|f|a" }, { rule: "r1", identity: "r1|f|b" }, { rule: "r2", problem: "no identity" }];
  const { fresh, known, stale } = applyBaseline(found, ["r1|f|a", "r1|f|gone", "r2|f|gone"], ["r1"]);
  assert.deepEqual([fresh.map((f) => f.identity), known.map((f) => f.identity), stale], [["r1|f|b", undefined], ["r1|f|a"], ["r1|f|gone"]]);
  const baseline = JSON.parse(fs.readFileSync(new URL("../ci/app-check-baseline.json", import.meta.url), "utf8"));
  assert.equal(baseline.schema, 1);
  assert.deepEqual(Object.keys(baseline.known).sort(), ["coderight", "cutright", "genright", "mailright", "rightkit", "viewright"], "one entry per repository with known debt; apps with none are absent");
});
