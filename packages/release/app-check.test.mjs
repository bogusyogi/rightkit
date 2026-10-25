import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "./background-process.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  APP_BASELINE_MAX_DAYS, APP_BASELINE_RULE, BUILD_REFUSES, CHECKED_COMMANDS, RULES, UNCHECKED_COMMANDS, UNCHECKED_PROOF, addDays, appBaselineEntries, appDirectory, applyBaseline, baselineKnown, cargoTestTargets, checkModeFor,
  e2eCrates, exportCommit, exportTarget, exportWorktree, guardCommand, hasNoCommit, main, repoRoot, retireAliasText, rightkitCheckout, runAppCheck, runRules,
} from "./app-check.mjs";

// The app rules, the export, the ratchet and the commands that run the check (packages/release/app-check.mjs). The rule tests moved
// here from scripts/preflight.test.mjs with their bodies unchanged; the moved ratchet tests call main with the label preflight,
// which is what their assertions read, and RightKit's own rules stay tested there.

const tree = (t, files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [file, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), text);
  }
  return { root, files: Object.keys(files) };
};
const run = (only, t, files) => runRules({ ...tree(t, files), only: [only] }).failures;
const git = (cwd, ...args) => execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });

test("file-url refuses .pathname on an import.meta.url URL and accepts fileURLToPath", (t) => {
  // Joined at run time so this file does not itself carry the pattern the rule refuses.
  const meta = ["import", "meta", "url"].join(".");
  const failures = run("file-url", t, {
    "a.test.mjs": `const a = new URL(${meta}).pathname;\nconst b = new URL("../..", ${meta}).pathname.replace(/x/, "");\n`,
    "b.mjs": `const c = fileURLToPath(new URL("..", ${meta}));\n`,
  });
  assert.deepEqual(failures.map((f) => f.problem.split(" ")[0]), ["a.test.mjs:1", "a.test.mjs:2"]);
});

test("a rule that throws is a FAIL, never a pass", (t) => {
  const { root, files } = tree(t, {});
  const { failures } = runRules({ root, files, commit: "not-a-commit", only: ["eol-policy"] });
  assert.equal(failures.length, 1);
  assert.match(failures[0].problem, /^the rule itself failed/);
  const boom = Object.freeze([{ id: "boom", gap: "G99", run: () => { throw new Error("no"); } }]);
  assert.deepEqual(runRules({ root, files, rules: boom }).failures.map((f) => [f.rule, f.gap, f.problem]), [["boom", "G99", "the rule itself failed: no"]]);
});

test("every rule names its scan-gap family or the invariant it enforces", () => {
  for (const rule of RULES) assert.match(rule.gap, /^(G\d{2}|P\d+|SC-R\d|C3-\d{2})$/, rule.id);
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

test("P11: retireAliasText rewrites to rustright and is idempotent", () => {
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

test("G16 release-config: the JSON config (M24) is read too: provider, updater keyFile, recipe and argv profiles, invalid JSON", (t) => {
  const bad = JSON.stringify({
    schema: 1,
    app: "x",
    distribution: { provider: "r2" },
    targets: {
      win: { winPackage: { profile: "release-iterate" }, updater: { artifacts: [], keyFile: "keys/updater.key" } },
      mac: { macPackage: { profile: "defined-here" } },
    },
    development: { targets: { win: { build: [{ cmd: "pnpm", args: ["exec", "tauri", "build", "--", "--profile", "fast-dev"] }] } } },
    legal: { provider: "not-a-distribution-provider" },
  }, null, 2);
  const failures = run("release-config", t, {
    "app/right-release.config.json": bad,
    "app/Cargo.toml": '[workspace]\n[profile.defined-here]\ninherits = "dev"\n',
    "good/right-release.config.json": JSON.stringify({ schema: 1, app: "g", distribution: { provider: "github-releases" }, targets: { win: { updater: { artifacts: [] } } } }),
    "broken/right-release.config.json": '{ "schema": 1, // comment\n}\n',
  });
  assert.deepEqual(identities(failures), [
    "release-config|app/right-release.config.json|keyFile:\"keys/updater.key\"",
    "release-config|app/right-release.config.json|profile:fast-dev",
    "release-config|app/right-release.config.json|profile:release-iterate",
    "release-config|app/right-release.config.json|provider:r2",
    "release-config|broken/right-release.config.json|json:invalid",
  ]);
  assert.match(failures.find((f) => f.subject === "provider:r2").problem, /^app\/right-release\.config\.json:5 distribution provider "r2"/);
  assert.match(failures.find((f) => f.subject === "profile:release-iterate").problem, /^app\/right-release\.config\.json:10 Cargo profile "release-iterate"/);
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

test("G20 switches-inventory: a JSON release config (M24) needs switches.json or its releaseSwitches.inventory too", (t) => {
  const failures = run("switches-inventory", t, {
    "right-release.config.json": '{ "schema": 1, "app": "a" }\n',
    "app/right-release.config.json": '{ "schema": 1, "app": "b" }\n',
    "app/switches.json": "{}\n",
    "other/right-release.config.json": '{ "schema": 1, "app": "c", "releaseSwitches": { "inventory": "config/inv.json" } }\n',
    "other/config/inv.json": "{}\n",
    "missing/right-release.config.json": '{\n  "schema": 1,\n  "app": "d",\n  "releaseSwitches": { "inventory": "nope.json" }\n}\n',
  });
  assert.deepEqual(identities(failures), ["switches-inventory|missing/right-release.config.json|missing", "switches-inventory|right-release.config.json|."]);
  assert.match(failures.find((f) => f.subject === "missing").problem, /^missing\/right-release\.config\.json:4 has no switches inventory \(missing\/nope\.json does not exist\)/);
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
const preflight = async (argv) => { const lines = []; const errs = []; const code = await main(argv, { label: "preflight", out: (l) => lines.push(l), err: (l) => errs.push(l) }); return { code, lines, text: lines.join("\n"), errs }; };
const writeBaseline = (parent, known) => { const file = path.join(parent, "baseline.json"); fs.writeFileSync(file, JSON.stringify({ schema: 1, about: "t", known })); return file; };
const BAD_QA = { "tests/q.rs": 'let r = ["platform:macos"];\n' };
const KNOWN_ID = "qa-platform|tests/q.rs|platform:macos";
// An app baseline entry carries `added` and a sunset at most 90 days after it. Every date below is counted from ADDED and `now` is
// injected, so no assertion depends on the day the suite runs. (The tests that run a child process cannot inject `now`: they take
// the real UTC day as `added`, which is valid on that day and the days after it.)
const ADDED = "2026-10-24";
const NOW = new Date(`${addDays(ADDED, 7)}T12:00:00Z`);
const LONG_AFTER = new Date(`${addDays(ADDED, 400)}T00:00:00Z`);
const appEntry = (id, issue = "app#12", over = {}) => ({ id, issue, added: ADDED, sunset: addDays(ADDED, 60), ...over });
const todayUtc = () => new Date().toISOString().slice(0, 10);
const liveEntry = (id, issue) => ({ id, issue, added: todayUtc(), sunset: addDays(todayUtc(), 30) });

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

test("ratchet: applyBaseline splits findings and only rules that ran can go stale", () => {
  const found = [{ rule: "r1", identity: "r1|f|a" }, { rule: "r1", identity: "r1|f|b" }, { rule: "r2", problem: "no identity" }];
  const { fresh, known, stale } = applyBaseline(found, ["r1|f|a", "r1|f|gone", "r2|f|gone"], ["r1"]);
  assert.deepEqual([fresh.map((f) => f.identity), known.map((f) => f.identity), stale], [["r1|f|b", undefined], ["r1|f|a"], ["r1|f|gone"]]);
});

// ---- G22 e2e-under-cargo-test: the broker compiles, it does not run end-to-end tests ----------------------------------------------
// The crates below are what the rule reads: a package is end-to-end when it depends on rightkit-qa not optionally AND a non-example
// .rs file of it names a driver module (harness, control, suite, scenario, engine, in_app, interactive, page, browser, android).
// A helper module (mock, workspace, ...) or a name like `*-e2e` is not enough.
const QA_DEP = 'rightkit-qa = { path = "../../rightkit-qa" }';
const e2eApp = (extra = {}) => ({
  "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\nexclude = ["crates/legacy-e2e"]\n',
  "crates/app-e2e/Cargo.toml": `[package]\nname = "app-e2e"\n\n[dependencies]\n${QA_DEP}\n\n[[test]]\nname = "scenarios"\nharness = false\n`,
  "crates/app-e2e/tests/scenarios.rs": "use rightkit_qa::{mock, scenario::Scenario};\nfn main() {}\n",
  "crates/app-e2e/tests/qa_surface_guard.rs": "#[test]\nfn guard() { rightkit_qa::guard::check(); }\n",
  "crates/app-e2e/src/bin/app-qa.rs": "fn main() {}\n",
  // not end-to-end: only the helper module rightkit_qa::workspace / ::mock
  "crates/app-engine/Cargo.toml": `[package]\nname = "app-engine"\n\n[dev-dependencies]\n${QA_DEP}\n`,
  "crates/app-engine/tests/flow.rs": "use rightkit_qa::workspace;\n#[test]\nfn t() { workspace::create(); }\n",
  "crates/app-mock-platform/Cargo.toml": `[package]\nname = "app-mock-platform"\n\n[dependencies]\n${QA_DEP}\n`,
  "crates/app-mock-platform/src/lib.rs": "use rightkit_qa::mock::Mock;\n",
  // not end-to-end: the dependency is optional (the in-app control server), whatever the name says
  "crates/name-only-e2e/Cargo.toml": `[package]\nname = "name-only-e2e"\n\n[dependencies]\nrightkit-qa = { path = "../../rightkit-qa", optional = true }\n`,
  "crates/name-only-e2e/src/lib.rs": '#[cfg(feature = "qa-native")]\nuse rightkit_qa::control::Control;\n',
  // not end-to-end: the driver module is only named under examples/
  "crates/example-only/Cargo.toml": `[package]\nname = "example-only"\n\n[dependencies]\n${QA_DEP}\n`,
  "crates/example-only/examples/demo.rs": "use rightkit_qa::harness::Harness;\nfn main() {}\n",
  // end-to-end, renamed dependency, and not a member of the root workspace (excluded)
  "crates/legacy-e2e/Cargo.toml": '[package]\nname = "legacy-e2e"\n\n[dependencies]\nqa = { package = "rightkit-qa", path = "../../rightkit-qa" }\n',
  "crates/legacy-e2e/tests/old.rs": "#[test]\nfn t() { qa::harness::run(); }\n",
  // the app crate: an optional dependency, a dev-dependency used only for the workspace helper, and its own workspace
  "src-tauri/Cargo.toml": `[package]\nname = "app"\n\n[workspace]\n\n[dependencies]\nrightkit-qa = { path = "../../rightkit-qa", optional = true }\n\n[dev-dependencies]\n${QA_DEP}\n`,
  "src-tauri/src/main.rs": "fn main() {}\n",
  "src-tauri/tests/supervisor.rs": "use rightkit_qa::workspace;\n",
  // end-to-end, nested inside src-tauri with a [workspace] of its own, so not a member of the one around it
  "src-tauri/qa-e2e/Cargo.toml": `[package]\nname = "app-shell-qa"\n\n[workspace]\n\n[dependencies]\n${QA_DEP}\n`,
  "src-tauri/qa-e2e/tests/pdf_edit.rs": "use rightkit_qa::harness::Harness;\n",
  "src-tauri/qa-e2e/src/main.rs": "fn main() {}\n",
  // end-to-end, standalone, beside a package.json
  "tauri-app-next/qa-e2e/Cargo.toml": `[package]\nname = "native-e2e"\n\n[workspace]\n\n[dependencies]\n${QA_DEP}\n`,
  "tauri-app-next/qa-e2e/tests/installed_app.rs": "use rightkit_qa::control::launch;\n",
  ...extra,
});
const lineIn = (text, needle) => text.split("\n").findIndex((l) => l.includes(needle)) + 1;
const E2E_PROBLEM = (where, crate, via) => `${where} runs the end-to-end crate ${crate} under cargo test (${via}): the test binary runs in the broker's context and holds its compile slot for the whole run`;

test("G22 e2e-under-cargo-test: which packages are end-to-end crates (a non-optional rightkit-qa dependency AND a driver module), resolved through renames, tables and workspaces", (t) => {
  const qa = '{ path = "../../rightkit-qa" }';
  const crate = (name, manifest, sources = {}) => ({ [`${name}/Cargo.toml`]: `[package]\nname = "${name}"\n${manifest}`, ...Object.fromEntries(Object.entries(sources).map(([f, s]) => [`${name}/${f}`, s])) });
  const files = {
    "Cargo.toml": `[workspace]\nmembers = [\n  "a",\n  "b", # the second\n  "broken[", "c",\n]\nexclude = ["g"]\n\n[workspace.dependencies]\nqa = { package = "rightkit-qa", path = "rightkit-qa" }\nrightkit-qa = { path = "rightkit-qa" }\n`,
    ...crate("a", "\n[dependencies.rightkit-qa]\npath = \"../x\"\n", { "tests/t.rs": "use rightkit_qa::suite::Suite;\n" }),
    ...crate("b", `\n[target.'cfg(target_os = "macos")'.dev-dependencies]\nrightkit-qa = ${qa}\n`, { "tests/t.rs": "use rightkit_qa::{\n    mock,\n    util::x,\n    android::Device,\n};\n" }),
    ...crate("c", "\n[dev-dependencies]\nqa = { workspace = true }\n", { "tests/t.rs": "fn t() { qa::page::open(); }\n" }),
    ...crate("d", '\n[dependencies.rightkit-qa]\nworkspace = true\noptional = true\n', { "src/lib.rs": "use rightkit_qa::control::Control;\n" }),
    ...crate("e-e2e", `\n[dependencies]\nrightkit-qa = ${qa}\n`, { "tests/t.rs": "use rightkit_qa::{evidence::Evidence, mock, util::wait, media, loudness, process, storage, settings, workspace, guard};\n" }),
    ...crate("f", `\n[dependencies]\nrightkit-qa = ${qa}\n`, { "src/lib.rs": "// rightkit_qa::harness is the driver\n/* rightkit_qa::control::launch */\nuse rightkit_qa::mock;\n" }),
    ...crate("g", "\n[dependencies]\nrightkit-qa.path = \"../x\"\n", { "tests/t.rs": "use rightkit_qa::{evidence::Evidence, in_app::Probe};\n" }),
    ...crate("h", `\n[build-dependencies]\nrightkit-qa = ${qa}\n`, { "src/lib.rs": "use rightkit_qa::harness::Harness;\n" }),
    // outer has a helper-only use; the driver module belongs to inner, which is its own package
    ...crate("outer", `\n[dev-dependencies]\nrightkit-qa = ${qa}\n`, { "tests/t.rs": "use rightkit_qa::workspace;\n", "inner/Cargo.toml": `[package]\nname = "inner"\n\n[dependencies]\nrightkit-qa = ${qa}\n`, "inner/src/main.rs": "fn main() { rightkit_qa::engine::run(); }\n", "inner/tests/z.rs": "" }),
  };
  const { root, files: listed } = tree(t, files);
  const found = e2eCrates(root, listed);
  assert.deepEqual(found.crates.map((c) => c.name), ["a", "b", "c", "g", "inner"]);
  assert.deepEqual(found.nameOnly, ["e-e2e"], "a package named like one but not end-to-end is listed for a reviewer");
  const inner = found.crates.find((c) => c.name === "inner");
  assert.deepEqual([[...inner.tests], [...inner.bins]], [["z"], ["inner"]], "targets: tests/<name>.rs and src/main.rs, of this package only");
  assert.deepEqual(found.membersOf(".").map((c) => c.name), ["a", "b", "c"], "the workspace members that are end-to-end; inner is not one");
});

test("G22 e2e-under-cargo-test: a package.json script that runs an end-to-end crate under cargo test is a finding; compile-only, other crates and other cargo verbs are not", (t) => {
  const scripts = {
    "test:e2e": "rustright cargo build -p app-engine --features test-mode && rustright cargo test -p app-e2e -- --test-threads=1",
    "qa:ui-native": "rustright cargo test --locked -p legacy-e2e --test old -- --ignored --nocapture --test-threads=1",
    "verify:rust": "rustright cargo test --workspace",
    "qa:native": "rustright cargo build && rustright cargo run --manifest-path src-tauri/qa-e2e/Cargo.toml --bin app-shell-qa",
    "qa:bin": "rustright cargo run --bin app-qa",
    "qa:scenario": "rustright cargo test --test scenarios",
    "test:retired": `${OLD} cargo test --package=app-e2e`,
    "test:plain": "cargo +nightly test --package app-e2e",
    "test:nextest": "cargo nextest run -p app-e2e",
    // not findings
    "test:compile": "rustright cargo test -p app-e2e --no-run",
    "test:rust": "rustright cargo test --workspace --exclude app-e2e --no-fail-fast",
    "verify:tauri": "rustright cargo test --manifest-path src-tauri/Cargo.toml --workspace",
    "test:unit": "rustright cargo test -p app-engine",
    "test:mock": "rustright cargo test -p app-mock-platform",
    "test:optional": "rustright cargo test -p name-only-e2e",
    "test:example": "rustright cargo test -p example-only",
    "test:guard": "rustright cargo test -p app-e2e --test qa_surface_guard",
    "test:runner": "rightkit-qa run-tests --app app -p app-e2e -- --nocapture",
    "test:journeys": "rightkit-qa journeys run --build \"rustright cargo run -p app-e2e\"",
    "test:list": "cargo nextest list -p app-e2e",
    "test:build": "rustright cargo build -p app-e2e && rustright cargo check --workspace && rustright cargo clippy -p app-e2e",
    "test:libtest-args": "rustright cargo test -p app-engine -- --workspace --test scenarios",
    "test:help": "cargo test --help",
  };
  const pkg = JSON.stringify({ name: "app", scripts }, null, 2);
  const nested = JSON.stringify({ scripts: { "qa:native:journey": "rustright cargo test --manifest-path ./qa-e2e/Cargo.toml -- --nocapture", "qa:up": "rustright cargo test --manifest-path ../src-tauri/qa-e2e/Cargo.toml" } }, null, 2);
  const failures = run("e2e-under-cargo-test", t, e2eApp({ "package.json": pkg, "tauri-app-next/package.json": nested }));
  const by = (file, subject) => failures.filter((f) => f.file === file && f.subject === subject);
  assert.deepEqual(identities(failures), [
    "tauri-app-next/package.json|qa:native:journey", "tauri-app-next/package.json|qa:up",
    "package.json|qa:bin", "package.json|qa:native", "package.json|qa:scenario", "package.json|qa:ui-native", "package.json|test:e2e", "package.json|test:nextest", "package.json|test:plain", "package.json|test:retired", "package.json|verify:rust",
  ].map((id) => `e2e-under-cargo-test|${id}`).sort());
  assert.equal(failures.length, 11, "one finding per script and crate");
  // the text, with the line of the script in its package.json, the crate and how the command names it
  const e2e = by("package.json", "test:e2e")[0];
  assert.equal(e2e.problem, E2E_PROBLEM(`package.json:${lineIn(pkg, '"test:e2e"')} script "test:e2e"`, "app-e2e", "rustright cargo test -p app-e2e"));
  assert.equal(e2e.fix, "compile it with --no-run and run it outside the broker: rightkit-qa run-tests -p app-e2e [--test <target>] -- <libtest args>");
  assert.equal(e2e.rule, "e2e-under-cargo-test");
  assert.equal(e2e.gap, "G22");
  assert.match(by("package.json", "qa:ui-native")[0].problem, /end-to-end crate legacy-e2e under cargo test \(rustright cargo test -p legacy-e2e\)/, "a renamed dependency (package = \"rightkit-qa\") is resolved; -p still selects a crate the workspace excludes");
  assert.match(by("package.json", "verify:rust")[0].problem, /end-to-end crate app-e2e under cargo test \(rustright cargo test --workspace\)/, "--workspace selects the members only: not legacy-e2e (excluded), the nested workspace or the crates that are not end-to-end");
  assert.match(by("package.json", "qa:native")[0].problem, /end-to-end crate app-shell-qa under cargo test \(rustright cargo run --manifest-path src-tauri\/qa-e2e\/Cargo\.toml\)/, "cargo run; --bin names the same crate, so one finding");
  assert.match(by("package.json", "qa:bin")[0].problem, /end-to-end crate app-e2e under cargo test \(rustright cargo run --bin app-qa\)/);
  assert.match(by("package.json", "qa:scenario")[0].problem, /end-to-end crate app-e2e under cargo test \(rustright cargo test --test scenarios\)/, "--test with no -p: a test target of exactly one end-to-end crate");
  assert.match(by("package.json", "test:retired")[0].problem, new RegExp(`\\(${OLD} cargo test -p app-e2e\\)`), "the retired alias is matched, and says how it was spelled");
  assert.match(by("package.json", "test:plain")[0].problem, /\(cargo test -p app-e2e\)/, "plain cargo, with a toolchain override");
  assert.match(by("package.json", "test:nextest")[0].problem, /\(cargo nextest -p app-e2e\)/);
  // --manifest-path is resolved against the directory of the package.json that holds the script, then the repository root
  assert.match(by("tauri-app-next/package.json", "qa:native:journey")[0].problem, /^tauri-app-next\/package\.json:\d+ script "qa:native:journey" runs the end-to-end crate native-e2e /);
  assert.match(by("tauri-app-next/package.json", "qa:up")[0].problem, /end-to-end crate app-shell-qa /, "../ out of the package directory");
});

test("G22 e2e-under-cargo-test: shell, PowerShell, batch, Node and Python scripts, Makefile and justfile recipes, git hooks: every non-comment line, with the working directory a cd gives", (t) => {
  const files = e2eApp({
    // the cd of the script-directory idiom is the repository root; the exec line selects app-e2e through --workspace
    "scripts/test.sh": '#!/usr/bin/env bash\nset -euo pipefail\n# rustright cargo test -p legacy-e2e (a comment is not a command)\ncd "$(dirname "$0")/.."\nexec rustright cargo test --workspace --features app-engine/test-mode "$@"\n',
    "scripts/run-e2e.sh": 'out="$(rustright cargo test -p app-e2e --test "$target" --no-run 2>&1)"\n',
    "scripts/gate.sh": 'cargo test --no-run --message-format=json "${cargo_args[@]}"\ncargo test -p app-e2e --message-format=json "${cargo_args[@]}" --no-run\n',
    "scripts/qa.sh": [
      "cd tauri-app-next",
      "rustright cargo test --manifest-path qa-e2e/Cargo.toml -- --nocapture",
      "rustright cargo test \\",
      "  -p app-e2e \\",
      "  -- --nocapture",
      "rustright cargo test \\",
      "  -p legacy-e2e \\",
      "  --no-run",
      "(cd .. && rustright cargo test --manifest-path src-tauri/qa-e2e/Cargo.toml)",
      "",
    ].join("\n"),
    "scripts/qa.ps1": "Set-Location tauri-app-next\n# rustright cargo test -p legacy-e2e\nrustright cargo test --manifest-path qa-e2e/Cargo.toml\nrustright cargo test `\n  -p app-e2e\n",
    "scripts/qa.bat": "REM rustright cargo test -p app-e2e\n:: rustright cargo test -p app-e2e\nrem rustright cargo test -p app-e2e\n@echo off\nrustright cargo test -p legacy-e2e\n",
    "scripts/qa.mjs": 'spawnSync(`rustright cargo test -p app-e2e`, { shell: true });\n// run("rustright cargo test -p legacy-e2e");\nrun("rustright cargo test -p legacy-e2e");\nconst cmd = `rustright cargo test -p ${crate}`;\n',
    "tools/qa.py": '# subprocess.run("rustright cargo test -p legacy-e2e")\nsubprocess.run("rustright cargo run -p app-e2e --bin app-qa", shell=True)\n',
    Makefile: "e2e:\n\t@rustright cargo test -p app-e2e\n# \trustright cargo test -p legacy-e2e\nunit:\n\trustright cargo test -p app-engine\n",
    justfile: "e2e:\n    rustright cargo test -p app-e2e -- --ignored\n",
    ".githooks/pre-push": "#!/bin/sh\nrustright cargo test -p app-e2e\n",
    ".husky/pre-commit": `${OLD} cargo test -p legacy-e2e\n`,
    // outside the places: a TypeScript source file, a vendored script, a build output
    "src/notes.ts": "// rustright cargo test -p app-e2e\n",
    "vendor/x/qa.sh": "rustright cargo test -p app-e2e\n",
    "node_modules/x/qa.sh": "rustright cargo test -p app-e2e\n",
  });
  const failures = run("e2e-under-cargo-test", t, files);
  assert.deepEqual(failures.map((f) => `${f.problem.split(" ")[0]} ${f.subject}`).sort(), [
    ".githooks/pre-push:2 app-e2e",
    ".husky/pre-commit:1 legacy-e2e",
    "Makefile:2 app-e2e",
    "justfile:2 app-e2e",
    "scripts/qa.bat:5 legacy-e2e",
    "scripts/qa.mjs:1 app-e2e",
    "scripts/qa.mjs:3 legacy-e2e",
    "scripts/qa.ps1:3 native-e2e",
    "scripts/qa.ps1:4 app-e2e",
    "scripts/qa.sh:2 native-e2e",
    "scripts/qa.sh:3 app-e2e",
    "scripts/qa.sh:9 app-shell-qa",
    "scripts/test.sh:5 app-e2e",
    "tools/qa.py:2 app-e2e",
  ].sort());
  const what = (file) => failures.find((f) => f.file === file).problem.replace(/^\S+ /, "").split(" runs ")[0];
  assert.deepEqual(["scripts/test.sh", "Makefile", ".githooks/pre-push"].map(what), ["command", "recipe", "git hook"]);
  assert.match(failures.find((f) => f.file === "scripts/test.sh").problem, /\(rustright cargo test --workspace\)/);
});

test("G22 e2e-under-cargo-test: a GitHub workflow run: step, single line or block, with the step's working-directory; a step's name is not a command", (t) => {
  const workflow = [
    "name: ci",
    "on: [push]",
    "jobs:",
    "  e2e:",
    "    runs-on: macos-latest",
    "    steps:",
    "      - uses: actions/checkout@v4",
    "      - name: rustright cargo test -p app-e2e (only a title)",
    "        run: echo hello",
    "      - name: e2e",
    "        run: rustright cargo test -p app-e2e -- --test-threads=1",
    "      - name: more",
    "        run: |",
    "          rustright cargo build -p app-e2e",
    "          # rustright cargo test -p legacy-e2e",
    "          rustright cargo test -p legacy-e2e",
    "          rustright cargo test -p app-e2e --no-run",
    "      - name: tauri side",
    "        working-directory: tauri-app-next",
    "        run: rustright cargo test --manifest-path qa-e2e/Cargo.toml",
    "      - run: >-",
    "          rustright cargo test",
    "          -p legacy-e2e",
    "      - name: unit",
    "        run: rustright cargo test -p app-engine",
    "",
  ].join("\n");
  const failures = run("e2e-under-cargo-test", t, e2eApp({ ".github/workflows/ci.yml": workflow, ".github/actions/qa/action.yaml": "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: rustright cargo test -p legacy-e2e\n", ".github/CODEOWNERS": "rustright cargo test -p app-e2e\n" }));
  assert.deepEqual(failures.map((f) => `${f.problem.split(" ")[0]} ${f.subject}`).sort(), [".github/actions/qa/action.yaml:5 legacy-e2e", ".github/workflows/ci.yml:11 app-e2e", ".github/workflows/ci.yml:16 legacy-e2e", ".github/workflows/ci.yml:20 native-e2e", ".github/workflows/ci.yml:22 legacy-e2e"].sort());
  assert.match(failures.find((f) => f.file.endsWith("ci.yml") && f.subject === "app-e2e").problem, /^\.github\/workflows\/ci\.yml:11 workflow step runs the end-to-end crate app-e2e under cargo test /);
});

test("G22 e2e-under-cargo-test: a command in a runbook is a finding (fenced code, inline code), prose and history documents are not", (t) => {
  const readme = [
    "# App",
    "Unit tests: `rustright cargo test -p app-engine`.",
    "Run it with `rustright cargo test -p legacy-e2e`",
    "",
    "```sh",
    "# rustright cargo test -p app-e2e is in a comment",
    "cd tauri-app-next",
    "rustright cargo test --manifest-path qa-e2e/Cargo.toml",
    "rustright cargo test -p app-e2e --no-run",
    "```",
    "Prose names rustright cargo test -p app-e2e without making it a command.",
    "",
  ].join("\n");
  const inline = "Use `rustright cargo test -p app-e2e` here.\n";
  const failures = run("e2e-under-cargo-test", t, e2eApp({
    "README.md": readme,
    "docs/E2E.md": inline,
    "docs/qa/deep.md": `~~~\n${OLD} cargo run -p app-e2e --bin app-qa\n~~~\n`,
    "AGENTS.md": inline,
    "CLAUDE.md": inline,
    // history, by name or place: never a command to follow
    "CHANGELOG.md": inline, "docs/CHANGELOG.md": inline, "STATUS.md": inline, "docs/STATUS.md": inline, "docs/PENDING.md": inline, "docs/state.md": inline,
    "docs/archive/x.md": inline, "docs/plans/y.md": inline, "docs/provenance/z.md": inline, "docs/handoff-3/a.md": inline, "docs/handoffs/a.md": inline,
    "docs/2026-10-11-notes.md": inline, "docs/audit/run-2026-10-24.md": inline, "docs/tasks/t.md": inline, "docs/run/evidence/e.md": inline,
    // outside the runbook set
    "notes.md": inline, "crates/app-e2e/README.md": inline, "docs/readme.txt": inline,
  }));
  assert.deepEqual(failures.map((f) => `${f.problem.split(" ")[0]} ${f.subject}`).sort(), ["AGENTS.md:1 app-e2e", "CLAUDE.md:1 app-e2e", "README.md:3 legacy-e2e", "README.md:8 native-e2e", "docs/E2E.md:1 app-e2e", "docs/qa/deep.md:2 app-e2e"].sort());
  assert.match(failures.find((f) => f.file === "README.md" && f.subject === "native-e2e").problem, /^README\.md:8 runbook command runs the end-to-end crate native-e2e /, "the cd in the block gives the manifest its directory");
});

test("G22 e2e-under-cargo-test: the identity is the file and the script (package.json) or the crate, no line number; findings are per line and crate", (t) => {
  const pkg = JSON.stringify({ scripts: { "test:e2e": "rustright cargo test -p app-e2e && rustright cargo test -p legacy-e2e", both: "rustright cargo test -p app-e2e && rustright cargo run --bin app-qa" } }, null, 2);
  const files = e2eApp({ "package.json": pkg, "scripts/a.sh": "rustright cargo test -p app-e2e\n\n\nrustright cargo test -p app-e2e\n" });
  const failures = run("e2e-under-cargo-test", t, files);
  assert.deepEqual(identities(failures), [
    "e2e-under-cargo-test|package.json|both",
    "e2e-under-cargo-test|package.json|test:e2e", "e2e-under-cargo-test|package.json|test:e2e",
    "e2e-under-cargo-test|scripts/a.sh|app-e2e", "e2e-under-cargo-test|scripts/a.sh|app-e2e",
  ].sort(), "a script holding two commands on two crates is two findings of one identity; two commands on one crate in one line are one finding");
  assert.deepEqual(failures.filter((f) => f.file === "scripts/a.sh").map((f) => f.problem.split(" ")[0]), ["scripts/a.sh:1", "scripts/a.sh:4"]);
  // an edit above the command renumbers the line and changes no identity
  const moved = run("e2e-under-cargo-test", t, { ...files, "scripts/a.sh": `# header\n${files["scripts/a.sh"]}` });
  assert.deepEqual(identities(moved), identities(failures));
  assert.deepEqual(run("e2e-under-cargo-test", t, { "package.json": pkg, "Cargo.toml": "[workspace]\n" }), [], "a tree with no end-to-end crate has nothing to find");
});

test("G22 e2e-under-cargo-test: a CRLF checkout (Windows) gives the same findings as an LF one", (t) => {
  const lf = e2eApp({
    "package.json": JSON.stringify({ scripts: { "test:e2e": "rustright cargo test -p app-e2e", "verify:rust": "rustright cargo test --workspace" } }, null, 2),
    "scripts/qa.sh": "cd tauri-app-next\nrustright cargo test \\\n  --manifest-path qa-e2e/Cargo.toml\n",
    "scripts/qa.ps1": "Set-Location tauri-app-next\nrustright cargo test `\n  --manifest-path qa-e2e/Cargo.toml\n",
    ".github/workflows/ci.yml": "jobs:\n  e2e:\n    steps:\n      - working-directory: tauri-app-next\n        run: |\n          rustright cargo test --manifest-path qa-e2e/Cargo.toml\n      - run: rustright cargo test -p legacy-e2e\n",
    "README.md": "Run `rustright cargo test -p legacy-e2e`.\n\n```sh\nrustright cargo test -p app-e2e\n```\n",
  });
  const crlf = Object.fromEntries(Object.entries(lf).map(([file, text]) => [file, text.replaceAll("\n", "\r\n")]));
  const expected = identities(run("e2e-under-cargo-test", t, lf));
  assert.deepEqual(expected, [
    "e2e-under-cargo-test|.github/workflows/ci.yml|legacy-e2e", "e2e-under-cargo-test|.github/workflows/ci.yml|native-e2e",
    "e2e-under-cargo-test|README.md|app-e2e", "e2e-under-cargo-test|README.md|legacy-e2e",
    "e2e-under-cargo-test|package.json|test:e2e", "e2e-under-cargo-test|package.json|verify:rust",
    "e2e-under-cargo-test|scripts/qa.ps1|native-e2e", "e2e-under-cargo-test|scripts/qa.sh|native-e2e",
  ].sort());
  assert.deepEqual(identities(run("e2e-under-cargo-test", t, crlf)), expected);
});

test("G22 e2e-under-cargo-test: ratchet: a baseline entry tolerates the finding, a fixed one is STALE, the build does not refuse it", async (t) => {
  const pkg = (command) => JSON.stringify({ scripts: { "test:e2e": command } }, null, 2);
  const { repo, parent } = appRepo(t, e2eApp({ "package.json": pkg("rustright cargo test -p app-e2e -- --test-threads=1") }));
  const id = "e2e-under-cargo-test|package.json|test:e2e";
  const args = (known) => ["--repo", repo, "--worktree", "--only", "e2e-under-cargo-test", "--baseline", writeBaseline(parent, known)];
  const fresh = await preflight(args({}));
  assert.equal(fresh.code, 1);
  assert.match(fresh.text, /^FAIL e2e-under-cargo-test \(G22\): package\.json:3 script "test:e2e" runs the end-to-end crate app-e2e under cargo test \(rustright cargo test -p app-e2e\): /m);
  assert.match(fresh.text, /^     fix: compile it with --no-run and run it outside the broker: rightkit-qa run-tests -p app-e2e \[--test <target>\] -- <libtest args>$/m);
  const printed = await preflight(["--repo", repo, "--worktree", "--only", "e2e-under-cargo-test", "--print-baseline"]);
  assert.deepEqual(JSON.parse(printed.text), [id]);
  const known = await preflight(args({ apprepo: [id] }));
  assert.equal(known.code, 0, known.text);
  assert.match(known.text, /^KNOWN e2e-under-cargo-test: package\.json:3 /m);
  assert.match(known.text, /\(1 known finding\(s\)/);
  // the app fixes it (compile only); the entry is now a lie and the list only shrinks
  fs.writeFileSync(path.join(repo, "package.json"), pkg("rustright cargo test -p app-e2e --no-run"));
  const stale = await preflight(args({ apprepo: [id] }));
  assert.equal(stale.code, 1);
  assert.match(stale.text, /^STALE baseline entry e2e-under-cargo-test\|package\.json\|test:e2e: no longer found in apprepo; remove it from ci\/app-check-baseline\.json known\["apprepo"\]$/m);
  // an app's own file: the entry needs its issue, added and a sunset, and tolerates the finding until then (commit mode, as a build checks)
  fs.writeFileSync(path.join(repo, "package.json"), pkg("rustright cargo test -p app-e2e"));
  fs.mkdirSync(path.join(repo, ".rightkit"), { recursive: true });
  fs.writeFileSync(path.join(repo, ".rightkit/app-check-baseline.json"), JSON.stringify({ schema: 1, known: [appEntry(id, "app#22")] }));
  git(repo, "add", "-A");
  git(repo, "commit", "-q", "-m", "list it");
  const mine = runAppCheck({ repo, central: null, only: ["e2e-under-cargo-test"], now: NOW });
  assert.equal(mine.exitCode, 0, mine.text);
  assert.equal(mine.known.length, 1);
  assert.equal(BUILD_REFUSES.includes("e2e-under-cargo-test"), false, "a listed G22 finding is tolerated before a build, like G13 to G19");
  assert.equal(guardCommand({ args: ["build"], cwd: repo, run: (options) => runAppCheck({ ...options, central: null, only: ["e2e-under-cargo-test"], now: NOW }) }), null, "a listed finding does not stop a build");
  const expired = runAppCheck({ repo, central: null, only: ["e2e-under-cargo-test"], now: LONG_AFTER });
  assert.equal(expired.exitCode, 1, "past its sunset the entry tolerates nothing");
  assert.match(expired.text, /^FAIL e2e-under-cargo-test \(G22\): package\.json:3 .*\(its baseline entry expired: sunset /m);
});

// ---- export: the committed files by git alone ------------------------------------------------------------------------------------

const commitAll = (repo, message = "commit") => { git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", message); return git(repo, "rev-parse", "HEAD").trim(); };
const exportTo = (t, repo, commit = "HEAD", options) => {
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-export-"));
  t.after(() => fs.rmSync(dest, { recursive: true, force: true }));
  const files = exportCommit(repo, commit, path.join(dest, "tree"), options);
  return { dest: path.join(dest, "tree"), files };
};
const posixOnly = process.platform === "win32" && "no POSIX mode bits or symlinks on win32";

test("exportCommit writes the committed blobs exactly: raw bytes, no eol conversion, export-ignore kept, executable bit, nested paths", (t) => {
  const binary = Buffer.from(Array.from({ length: 256 }, (_, i) => i));
  const { repo } = appRepo(t, {
    ".gitattributes": "*.txt text eol=crlf\nignored.md export-ignore\n",
    "a.txt": "one\ntwo\n",
    "ignored.md": "# still exported\n",
    "deep/er/file.json": '{"k":1}\n',
    "empty.txt": "",
    "twin-a.txt": "same\n",
    "twin-b.txt": "same\n",
  });
  fs.writeFileSync(path.join(repo, "blob.bin"), binary);
  fs.writeFileSync(path.join(repo, "run.sh"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  git(repo, "add", "--chmod=+x", "run.sh");
  commitAll(repo, "more");
  const { dest, files } = exportTo(t, repo);
  assert.deepEqual([...files].sort(), [".gitattributes", "a.txt", "blob.bin", "deep/er/file.json", "empty.txt", "ignored.md", "run.sh", "twin-a.txt", "twin-b.txt"]);
  assert.equal(fs.readFileSync(path.join(dest, "a.txt"), "utf8"), "one\ntwo\n", "git archive would write CRLF here; the committed LF bytes are what is checked");
  assert.equal(fs.readFileSync(path.join(dest, "ignored.md"), "utf8"), "# still exported\n", "export-ignore does not hide a committed file from the check");
  assert.deepEqual(fs.readFileSync(path.join(dest, "blob.bin")), binary);
  assert.equal(fs.readFileSync(path.join(dest, "deep/er/file.json"), "utf8"), '{"k":1}\n');
  assert.equal(fs.readFileSync(path.join(dest, "empty.txt")).length, 0);
  assert.equal(fs.readFileSync(path.join(dest, "twin-b.txt"), "utf8"), "same\n", "one blob under two names is written under both");
  if (posixOnly) t.diagnostic(`skipped the executable-bit assertion: ${posixOnly}`);
  else assert.ok(fs.statSync(path.join(dest, "run.sh")).mode & 0o100, "mode 100755 is kept");
});

test("exportCommit reads in batches: a batch of one byte gives the same tree as the default", (t) => {
  const { repo } = appRepo(t, { "a.txt": "aaa\n", "b/c.txt": "bbbb\n", "b/d.txt": "aaa\n", "e.txt": "" });
  const same = (x, y) => { for (const f of x.files) assert.deepEqual(fs.readFileSync(path.join(x.dest, f)), fs.readFileSync(path.join(y.dest, f)), f); };
  const whole = exportTo(t, repo), tiny = exportTo(t, repo, "HEAD", { batchBytes: 1 });
  assert.deepEqual(tiny.files, whole.files);
  same(whole, tiny);
});

test("exportCommit uses no tar and no git archive, so a Windows host needs no tar on PATH", { skip: posixOnly }, (t) => {
  const source = fs.readFileSync(new URL("./app-check.mjs", import.meta.url), "utf8");
  assert.doesNotMatch(source, /["']tar["']|["']archive["']/, "the export neither spawns tar nor runs git archive");
  // A tar that fails loudly and leaves a mark, first on PATH: the export must not reach for it.
  const bin = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-bin-"));
  t.after(() => fs.rmSync(bin, { recursive: true, force: true }));
  const mark = path.join(bin, "tar-was-run");
  fs.writeFileSync(path.join(bin, "tar"), `#!/bin/sh\necho run > "${mark}"\nexit 99\n`, { mode: 0o755 });
  const { repo } = appRepo(t, { "a.txt": "x\n" });
  const before = process.env.PATH;
  process.env.PATH = `${bin}${path.delimiter}${before}`;
  t.after(() => { process.env.PATH = before; });
  const { dest } = exportTo(t, repo);
  assert.equal(fs.readFileSync(path.join(dest, "a.txt"), "utf8"), "x\n");
  assert.equal(fs.existsSync(mark), false);
});

test("exportCommit: a symlink becomes a file holding its target and a submodule an empty directory", { skip: posixOnly }, (t) => {
  const { repo } = appRepo(t, { "real.txt": "content\n" });
  fs.symlinkSync("real.txt", path.join(repo, "link.txt"));
  git(repo, "add", "link.txt");
  git(repo, "update-index", "--add", "--cacheinfo", `160000,${git(repo, "rev-parse", "HEAD").trim()},vendor/sub`);
  git(repo, "commit", "-q", "-m", "link and submodule"); // not add -A: it would stage the missing vendor/sub directory as a deletion
  const { dest, files } = exportTo(t, repo);
  assert.deepEqual([...files].sort(), ["link.txt", "real.txt", "vendor/sub"]);
  assert.equal(fs.lstatSync(path.join(dest, "link.txt")).isSymbolicLink(), false, "no symlink is created: a tree cannot point the rules outside the export");
  assert.equal(fs.readFileSync(path.join(dest, "link.txt"), "utf8"), "real.txt");
  assert.deepEqual(fs.readdirSync(path.join(dest, "vendor/sub")), []);
});

test("exportTarget refuses a path that leaves the export or enters .git, and accepts ordinary and dot paths", () => {
  const dest = path.join(os.tmpdir(), "rk-check-dest");
  for (const bad of ["../x", "a/../../x", "/etc/passwd", ".git/config", "a/.GIT/hooks/pre-push", "a//b", "./a", "", "a/./b", "a\0b"]) assert.throws(() => exportTarget(dest, bad), /refusing to export the unsafe path/, JSON.stringify(bad));
  for (const good of ["a.txt", "deep/er/file.json", ".github/workflows/ci.yml", ".gitattributes", ".rightkit/app-check-baseline.json", "..hidden", "a/b..c"]) assert.equal(exportTarget(dest, good), path.join(dest, ...good.split("/")));
});

test("exportWorktree: the tree a compile would see, tracked edits and deletions and untracked files but not ignored ones; the repository is untouched", (t) => {
  const { repo } = appRepo(t, { ".gitignore": "build/\n", "tracked.txt": "committed\n", "gone.txt": "bye\n", "keep.txt": "keep\n" });
  fs.writeFileSync(path.join(repo, "tracked.txt"), "edited on disk\n");
  fs.rmSync(path.join(repo, "gone.txt"));
  fs.writeFileSync(path.join(repo, "new.txt"), "untracked\n");
  fs.mkdirSync(path.join(repo, "build"));
  fs.writeFileSync(path.join(repo, "build/out.txt"), "ignored\n");
  const state = () => ({ status: git(repo, "status", "--porcelain=v1", "-uall"), index: git(repo, "diff", "--cached", "--name-only"), head: git(repo, "rev-parse", "HEAD") });
  const before = state();
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-wt-"));
  t.after(() => fs.rmSync(dest, { recursive: true, force: true }));
  const { tree, files } = exportWorktree(repo, dest);
  assert.match(tree, /^[0-9a-f]{40,64}$/);
  assert.deepEqual([...files].sort(), [".gitignore", "keep.txt", "new.txt", "tracked.txt"]);
  assert.equal(fs.readFileSync(path.join(dest, "tracked.txt"), "utf8"), "edited on disk\n");
  assert.equal(fs.existsSync(path.join(dest, "gone.txt")), false);
  assert.equal(fs.existsSync(path.join(dest, "build")), false);
  assert.deepEqual(state(), before, "the repository's own index, status and HEAD are as they were");
  assert.equal(fs.readdirSync(os.tmpdir()).filter((n) => n.startsWith("rk-check-index-")).length, 0, "the throwaway index is removed");
});

test("a hook's GIT_DIR and GIT_INDEX_FILE do not redirect the check to another repository", (t) => {
  const { repo } = appRepo(t, { "package.json": JSON.stringify({ scripts: { ship: "right-release build --skip-checks" } }) });
  const other = appRepo(t, { "README.md": "other\n" }).repo;
  const saved = { GIT_DIR: process.env.GIT_DIR, GIT_INDEX_FILE: process.env.GIT_INDEX_FILE, GIT_WORK_TREE: process.env.GIT_WORK_TREE };
  t.after(() => { for (const [k, v] of Object.entries(saved)) { if (v === undefined) delete process.env[k]; else process.env[k] = v; } });
  Object.assign(process.env, { GIT_DIR: path.join(other, ".git"), GIT_INDEX_FILE: path.join(other, ".git", "index"), GIT_WORK_TREE: other });
  const result = runAppCheck({ repo, central: null });
  assert.equal(result.exitCode, 1, result.errText);
  assert.deepEqual(result.findings.map((f) => f.identity), ["skip-checks|package.json|ship"]);
  assert.equal(repoRoot(repo), fs.realpathSync.native(repo));
});

// ---- runAppCheck, main and the baseline ------------------------------------------------------------------------------------------

const FIXTURE = { ...BAD_QA, "package.json": JSON.stringify({ scripts: { ship: "right-release build --skip-checks" } }) };

test("runAppCheck returns the findings, the tolerated ones, the stale ones and the exit code, and prints nothing", (t) => {
  const { repo, parent } = appRepo(t, FIXTURE);
  const writes = [];
  const realWrite = process.stdout.write;
  process.stdout.write = (chunk, ...rest) => { writes.push(String(chunk)); return realWrite.call(process.stdout, chunk, ...rest); };
  let red;
  try { red = runAppCheck({ repo, central: null, label: "preflight" }); } finally { process.stdout.write = realWrite; }
  assert.deepEqual(writes, [], "runAppCheck prints nothing; the caller does");
  assert.equal(red.exitCode, 1);
  assert.deepEqual(red.findings.map((f) => f.identity).sort(), ["qa-platform|tests/q.rs|platform:macos", "skip-checks|package.json|ship"]);
  assert.deepEqual([red.known, red.stale, red.expired], [[], [], []]);
  assert.match(red.text, /^FAIL qa-platform \(G13\): tests\/q\.rs:1 /m);
  assert.match(red.text, /^preflight: 2 problem\(s\) in \w{12} \(25 rules, /m);
  const baseline = writeBaseline(parent, { apprepo: [KNOWN_ID, "skip-checks|package.json|ship", "qa-platform|tests/gone.rs|platform:macos"] });
  const ratcheted = runAppCheck({ repo, central: null, baseline });
  assert.equal(ratcheted.exitCode, 1);
  assert.deepEqual([ratcheted.findings, ratcheted.known.length, ratcheted.stale], [[], 2, ["qa-platform|tests/gone.rs|platform:macos"]]);
  assert.match(ratcheted.text, /^check: 1 problem\(s\) in /m, "the default label is check");
  assert.equal(runAppCheck({ repo, central: null, only: ["no-such-rule"] }).exitCode, 2);
  assert.equal(runAppCheck({ repo, central: null, mode: "worktree", commit: "HEAD" }).exitCode, 2, "--worktree and --commit are exclusive");
  assert.equal(runAppCheck({ repo, central: null, commit: "no-such-rev" }).exitCode, 2);
  assert.match(runAppCheck({ repo, central: null, commit: "--upload-pack=x" }).errText, /could not export the commit to check: "--upload-pack=x" is not a revision/);
  assert.equal(runAppCheck({ repo: parent, central: null }).exitCode, 2, "a directory that is no repository cannot be checked");
});

test("a repository with no commit yet says to commit first, in every mode, and the guarded commands start nothing", async (t) => {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-fresh-"));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  const repo = path.join(parent, "Fresh");
  fs.mkdirSync(repo);
  git(repo, "init", "-q");
  for (const [k, v] of [["user.email", "t@example.test"], ["user.name", "t"], ["core.autocrlf", "false"]]) git(repo, "config", k, v);
  fs.writeFileSync(path.join(repo, "a.txt"), "x\n"); // untracked: what --worktree would read, if there were a HEAD to start from
  assert.equal(hasNoCommit(repo), true);
  assert.equal(hasNoCommit(parent), false, "a directory that is no repository has no commit yet in no sense");
  const said = "check: the repository has no commit yet; commit first";
  for (const options of [{ mode: "worktree" }, { mode: "commit" }, { mode: "commit", commit: "HEAD" }]) {
    const result = runAppCheck({ repo, central: null, ...options });
    assert.equal(result.exitCode, 2, `${JSON.stringify(options)}: the check could not run`);
    assert.equal(result.errText, said, JSON.stringify(options));
    assert.deepEqual([result.findings, result.known, result.stale, result.expired], [[], [], [], []]);
  }
  // A revision that was named and is missing keeps the exporter's own message: the repository is not what is wrong.
  assert.match(runAppCheck({ repo, central: null, commit: "no-such-rev" }).errText, /^check: could not export the commit to check: /);
  // Through the commands: rightkit-gate check, and every guarded command refuses (exit 1) and says the same thing first.
  const lines = [], errs = [];
  assert.equal(await main(["--worktree"], { cwd: repo, out: (l) => lines.push(l), err: (l) => errs.push(l) }), 2);
  assert.deepEqual([lines, errs], [[], [said]]);
  for (const args of [["dev", "--platform", "win"], ["doctor"], ["build"]]) {
    const refused = guardCommand({ args, cwd: repo });
    assert.equal(refused.exitCode, 1, args.join(" "));
    assert.equal(refused.lines[0], said);
    assert.match(refused.lines.at(-1), new RegExp(`^right-release ${args[0]}: the app check could not run in .*; nothing was started\\.`));
  }
  // The first commit ends it.
  commitAll(repo, "first");
  assert.equal(hasNoCommit(repo), false);
  assert.equal(runAppCheck({ repo, central: null, mode: "worktree", only: ["skip-checks"] }).exitCode, 0);
});

test("main: --list, a repository outside git, unknown rules and exclusive flags", async (t) => {
  const listed = await preflight(["--list"]);
  assert.equal(listed.code, 0);
  assert.deepEqual(listed.lines, RULES.map((r) => `${r.id} (${r.gap})`));
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-nogit-"));
  t.after(() => fs.rmSync(outside, { recursive: true, force: true }));
  const lines = [], errs = [];
  assert.equal(await main([], { cwd: outside, out: (l) => lines.push(l), err: (l) => errs.push(l) }), 2);
  assert.match(errs.join("\n"), /^check: not inside a git repository \(use --repo <dir>\)/);
  const { repo } = appRepo(t, { "a.txt": "x\n" });
  assert.equal((await preflight(["--repo", repo, "--only", "nope"])).code, 2);
  assert.equal((await preflight(["--repo", repo, "--worktree", "--commit", "HEAD"])).code, 2);
  // Without --repo the default is the git top-level of the working directory, from any folder below it.
  fs.mkdirSync(path.join(repo, "sub"));
  const fromSub = await main(["--only", "skip-checks"], { cwd: path.join(repo, "sub"), out: (l) => lines.push(l), err: (l) => errs.push(l) });
  assert.equal(fromSub, 0, errs.join("\n"));
});

test("baseline: an app's own .rightkit/app-check-baseline.json is read from the checked tree, with an issue, an added date and a sunset per entry", async (t) => {
  const baselineOf = (known) => JSON.stringify({ schema: 1, about: "t", known });
  const { repo } = appRepo(t, {
    ...BAD_QA,
    ".rightkit/app-check-baseline.json": baselineOf([
      appEntry(KNOWN_ID, "app#12"),
      appEntry("qa-platform|tests/gone.rs|platform:macos", "app#13"),
    ]),
  });
  const red = runAppCheck({ repo, central: null, only: ["qa-platform"], now: NOW });
  assert.equal(red.exitCode, 1);
  assert.match(red.text, /^KNOWN qa-platform: tests\/q\.rs:1 /m);
  assert.match(red.text, /^STALE baseline entry qa-platform\|tests\/gone\.rs\|platform:macos: no longer found in apprepo; remove it from \.rightkit\/app-check-baseline\.json known$/m);
  // Fix the stale entry (commit): green, and the summary names the app's file.
  fs.writeFileSync(path.join(repo, ".rightkit/app-check-baseline.json"), baselineOf([appEntry(KNOWN_ID, "app#12")]));
  commitAll(repo, "drop the stale entry");
  const green = runAppCheck({ repo, central: null, only: ["qa-platform"], now: NOW });
  assert.equal(green.exitCode, 0, green.text);
  assert.match(green.text, /passed 1 rules .*\(1 known finding\(s\) in \.rightkit\/app-check-baseline\.json; fix them, then remove their entries\)/);
  // An uncommitted edit is not what is pushed (commit mode); what a compile sees reads it (worktree mode).
  fs.writeFileSync(path.join(repo, ".rightkit/app-check-baseline.json"), baselineOf([]));
  assert.equal(runAppCheck({ repo, central: null, only: ["qa-platform"], now: NOW }).exitCode, 0, "the committed baseline still lists it");
  assert.equal(runAppCheck({ repo, central: null, only: ["qa-platform"], mode: "worktree", now: NOW }).exitCode, 1, "the edited baseline no longer does");
});

test("baseline: an entry past its sunset fails as EXPIRED, with its finding, even though it is listed", (t) => {
  const sunset = addDays(ADDED, 30);
  const { repo } = appRepo(t, { ...BAD_QA, ".rightkit/app-check-baseline.json": JSON.stringify({ schema: 1, known: [appEntry(KNOWN_ID, "app#12", { sunset })] }) });
  const on = (day) => runAppCheck({ repo, central: null, only: ["qa-platform"], now: new Date(`${day}T23:59:59Z`) });
  assert.equal(on(sunset).exitCode, 0, "the sunset day itself is still tolerated");
  const late = on(addDays(sunset, 1));
  assert.equal(late.exitCode, 1);
  assert.match(late.text, new RegExp(`^FAIL qa-platform \\(G13\\): tests/q\\.rs:1 .* \\(its baseline entry expired: sunset ${sunset}; an app baseline entry is tolerated for at most 90 days after its added date\\)$`, "m"), "its finding is no longer tolerated, and says why, with the rule");
  assert.match(late.text, new RegExp(`^EXPIRED baseline entry qa-platform\\|tests/q\\.rs\\|platform:macos: sunset ${sunset} has passed \\(app#12\\); an app baseline entry is tolerated for at most 90 days after its added date \\(${ADDED}\\); fix the finding and remove the entry from \\.rightkit/app-check-baseline\\.json known$`, "m"));
  assert.deepEqual([late.expired.map((e) => e.id), late.stale, late.known], [[KNOWN_ID], [], []]);
  assert.match(late.text, /^check: 2 problem\(s\) in /m);
  // A rule that did not run cannot be judged: --only another rule leaves the entry alone.
  assert.equal(runAppCheck({ repo, central: null, only: ["skip-checks"], now: LONG_AFTER }).exitCode, 0);
  // Once the defect is fixed and committed, the expired entry is only an expired entry.
  fs.writeFileSync(path.join(repo, "tests/q.rs"), 'let r = ["platform:darwin"];\n');
  commitAll(repo, "fix");
  const fixed = runAppCheck({ repo, central: null, only: ["qa-platform"], now: LONG_AFTER });
  assert.equal(fixed.exitCode, 1);
  assert.deepEqual([fixed.findings, fixed.stale, fixed.expired.map((e) => e.id)], [[], [], [KNOWN_ID]]);
});

test("baseline: a finding the baseline lists still fails where the build refuses it (switches-inventory), and passes elsewhere", (t) => {
  const id = "switches-inventory|right-release.config.mjs|.";
  const { repo } = appRepo(t, { "right-release.config.mjs": "export default {};\n", ".rightkit/app-check-baseline.json": JSON.stringify({ schema: 1, known: [appEntry(id, "app#9")] }) });
  assert.deepEqual(BUILD_REFUSES, ["switches-inventory"]);
  const tolerated = runAppCheck({ repo, central: null, now: NOW });
  assert.equal(tolerated.exitCode, 0, tolerated.text);
  assert.match(tolerated.text, /^KNOWN switches-inventory: right-release\.config\.mjs:1 has no switches inventory/m);
  const refused = runAppCheck({ repo, central: null, refuse: BUILD_REFUSES, now: NOW });
  assert.equal(refused.exitCode, 1);
  assert.match(refused.text, /^FAIL switches-inventory \(G20\): right-release\.config\.mjs:1 has no switches inventory \(switches\.json does not exist\) \(the baseline lists it, but the build refuses it\)$/m);
  assert.doesNotMatch(refused.text, /^KNOWN |^STALE /m, "it is not stale either: the finding is there");
  assert.deepEqual([refused.findings.map((f) => f.identity), refused.known], [[id], []]);
  assert.equal(runAppCheck({ repo, central: null, refuse: ["qa-platform"], now: NOW }).exitCode, 0, "only the listed rules");
});

test("baseline: appBaselineEntries refuses an entry without an id, an issue, a real added date or a real sunset date", () => {
  const entry = { id: "qa-platform|f|x", issue: "app#1", added: ADDED, sunset: addDays(ADDED, 60) };
  const today = addDays(ADDED, 7);
  const text = (...known) => JSON.stringify({ schema: 1, known });
  assert.deepEqual(appBaselineEntries(text(entry), { today }), [entry]);
  assert.deepEqual(appBaselineEntries(text(), { today }), []);
  for (const [bad, why] of [
    [{ ...entry, id: "" }, /known\[0\]\.id must be a finding identity/],
    [{ ...entry, issue: " " }, /known\[0\]\.issue must name the issue/],
    [{ ...entry, issue: "" }, /known\[0\]\.issue must name the issue/],
    [{ id: entry.id, added: entry.added, sunset: entry.sunset }, /known\[0\]\.issue/],
    [{ ...entry, added: undefined }, /known\[0\]\.added must be the date the entry was added, YYYY-MM-DD/],
    [{ ...entry, added: "2026-02-30" }, /known\[0\]\.added must be the date/],
    [{ ...entry, added: "soon" }, /known\[0\]\.added must be the date/],
    [{ ...entry, added: 20261024 }, /known\[0\]\.added must be the date/],
    [{ ...entry, sunset: undefined }, /known\[0\]\.sunset must be a date, YYYY-MM-DD/],
    [{ ...entry, sunset: "2026-02-30" }, /known\[0\]\.sunset must be a date/],
    [{ ...entry, sunset: "soon" }, /known\[0\]\.sunset must be a date/],
    [{ ...entry, sunset: 20261231 }, /known\[0\]\.sunset must be a date/],
    ["x", /known\[0\] must be an object/],
  ]) assert.throws(() => appBaselineEntries(text(bad), { today }), why, JSON.stringify(bad));
  assert.throws(() => appBaselineEntries(text(entry, entry), { today }), /known\[1\] repeats qa-platform\|f\|x/);
  assert.throws(() => appBaselineEntries(JSON.stringify({ known: [] })), /expected \{ "schema": 1, "known": \[/);
  assert.throws(() => appBaselineEntries(JSON.stringify({ schema: 1, known: {} })), /expected \{ "schema": 1/);
});

test("baseline: an app entry's sunset is at most 90 days after its own added date, and added cannot be later than today", () => {
  const entry = { id: "qa-platform|f|x", issue: "app#1", added: ADDED, sunset: ADDED };
  const today = ADDED;
  const text = (over) => JSON.stringify({ schema: 1, known: [{ ...entry, ...over }] });
  // the dates are counted in whole UTC days, through month ends and a leap day
  assert.equal(APP_BASELINE_MAX_DAYS, 90);
  assert.equal(addDays("2026-10-24", 90), "2027-01-22");
  assert.equal(addDays("2027-12-01", 90), "2028-02-29");
  assert.equal(addDays("2026-12-31", 1), "2027-01-01");
  // the sunset may equal added and may be exactly 90 days later; one day more is refused, with the rule in the message
  assert.deepEqual(appBaselineEntries(text({ sunset: ADDED }), { today }).map((e) => e.sunset), [ADDED]);
  assert.deepEqual(appBaselineEntries(text({ sunset: "2027-01-22" }), { today }).map((e) => e.sunset), ["2027-01-22"]);
  assert.throws(() => appBaselineEntries(text({ sunset: "2027-01-23" }), { today }), (error) => {
    assert.match(error.message, /^known\[0\]\.sunset 2027-01-23 is more than 90 days after added 2026-10-24 \(the latest is 2027-01-22\); /);
    assert.ok(error.message.endsWith(APP_BASELINE_RULE), "the message states the whole rule");
    return true;
  });
  assert.match(APP_BASELINE_RULE, /non-empty issue, an added date and a sunset date \(YYYY-MM-DD, UTC\); added is not later than today, and sunset is not before added and at most 90 days after it$/);
  // the case that matters: a sunset that silences a finding for good
  assert.throws(() => appBaselineEntries(text({ sunset: "2999-01-01" }), { today }), /sunset 2999-01-01 is more than 90 days after added 2026-10-24/);
  // a sunset before added is a typo, not a short tolerance
  assert.throws(() => appBaselineEntries(text({ sunset: "2026-10-23" }), { today }), /^Error: known\[0\]\.sunset 2026-10-23 is before added 2026-10-24; /);
  // added cannot be pushed into the future to start the 90 days later (that would tolerate a finding for good, one entry at a time)
  const forged = text({ added: "2999-01-01", sunset: "2999-02-01" });
  assert.throws(() => appBaselineEntries(forged, { today }), /^Error: known\[0\]\.added 2999-01-01 is later than today \(2026-10-24, UTC\); /);
  assert.throws(() => appBaselineEntries(text({ added: addDays(today, 1), sunset: addDays(today, 5) }), { today }), /is later than today/);
  assert.deepEqual(appBaselineEntries(text({}), { today }).map((e) => e.added), [ADDED], "added today is fine");
  // without an explicit today the real UTC day is used, so a forged added date is refused whenever the suite runs
  assert.throws(() => appBaselineEntries(forged), /is later than today/);
});

test("baseline: an app baseline that outlasts its 90 days, or has no added date, is exit 2 naming the entry and the rule, and a central entry keeps its contract", (t) => {
  const baselineOf = (known) => JSON.stringify({ schema: 1, known });
  const { repo, parent } = appRepo(t, { ...BAD_QA, ".rightkit/app-check-baseline.json": baselineOf([appEntry(KNOWN_ID, "app#12", { sunset: "2999-01-01" })]) });
  const forever = runAppCheck({ repo, central: null, only: ["qa-platform"], now: NOW });
  assert.equal(forever.exitCode, 2, forever.text);
  assert.deepEqual([forever.findings, forever.known], [[], []], "nothing is tolerated by a baseline that breaks the rule");
  assert.match(forever.errText, /^check: cannot read the baseline \.rightkit\/app-check-baseline\.json: known\[0\]\.sunset 2999-01-01 is more than 90 days after added 2026-10-24 \(the latest is 2027-01-22\); /);
  assert.ok(forever.errText.endsWith(APP_BASELINE_RULE));
  fs.writeFileSync(path.join(repo, ".rightkit/app-check-baseline.json"), baselineOf([{ id: KNOWN_ID, issue: "app#12", sunset: addDays(ADDED, 30) }]));
  commitAll(repo, "an entry without added");
  assert.match(runAppCheck({ repo, central: null, only: ["qa-platform"], now: NOW }).errText, /known\[0\]\.added must be the date the entry was added/);
  // the central file's entries are identities with no dates: no added, no cap
  assert.equal(runAppCheck({ repo, central: null, baseline: writeBaseline(parent, { apprepo: [KNOWN_ID] }), only: ["qa-platform"], now: NOW }).exitCode, 0);
  // the same through the real commands: a forged added date is refused whenever it is run
  fs.writeFileSync(path.join(repo, ".rightkit/app-check-baseline.json"), baselineOf([{ id: KNOWN_ID, issue: "app#12", added: "2999-01-01", sunset: "2999-02-01" }]));
  commitAll(repo, "a future added date");
  const real = runAppCheck({ repo, central: null, only: ["qa-platform"] });
  assert.equal(real.exitCode, 2);
  assert.match(real.errText, /known\[0\]\.added 2999-01-01 is later than today \(\d{4}-\d{2}-\d{2}, UTC\)/);
});

test("baseline: an app's own file in the central shape ({ known: { repo: [...] } }, no issue, added or sunset) is exit 2 with the shape error, never a silent baseline", (t) => {
  const central = JSON.stringify({ schema: 1, known: { apprepo: [KNOWN_ID] } });
  const { repo, parent } = appRepo(t, { ...BAD_QA, ".rightkit/app-check-baseline.json": central });
  const bypass = runAppCheck({ repo, central: null, only: ["qa-platform"], now: NOW });
  assert.equal(bypass.exitCode, 2, bypass.text);
  assert.deepEqual([bypass.findings, bypass.known, bypass.stale, bypass.expired], [[], [], [], []], "nothing is tolerated by it");
  assert.match(bypass.errText, /^check: cannot read the baseline \.rightkit\/app-check-baseline\.json: expected \{ "schema": 1, "known": \[\{ "id": /);
  // the same with a RightKit checkout's central file as the fallback: the app's file is read first, as an app file, and refused
  const rk = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-central-"));
  t.after(() => fs.rmSync(rk, { recursive: true, force: true }));
  fs.mkdirSync(path.join(rk, "ci"));
  fs.writeFileSync(path.join(rk, "ci/app-check-baseline.json"), central);
  assert.equal(runAppCheck({ repo, central: rk, only: ["qa-platform"], now: NOW }).exitCode, 2);
  // the central shape is still what --baseline and RightKit's own central file mean
  assert.equal(runAppCheck({ repo, central: null, baseline: writeBaseline(parent, { apprepo: [KNOWN_ID] }), only: ["qa-platform"], now: NOW }).exitCode, 0);
  fs.rmSync(path.join(repo, ".rightkit"), { recursive: true });
  commitAll(repo, "no app baseline");
  assert.equal(runAppCheck({ repo, central: rk, only: ["qa-platform"], now: NOW }).exitCode, 0, "the central file lists it when the app has none");
  // an app file that is neither shape is refused too
  fs.mkdirSync(path.join(repo, ".rightkit"));
  fs.writeFileSync(path.join(repo, ".rightkit/app-check-baseline.json"), JSON.stringify({ schema: 1, known: "all" }));
  commitAll(repo, "a string baseline");
  assert.equal(runAppCheck({ repo, central: null, only: ["qa-platform"], now: NOW }).exitCode, 2);
});

test("baseline: a malformed app baseline is exit 2, and the resolution order is --baseline, the app's file, then RightKit's central file only from a RightKit checkout", (t) => {
  const { repo, parent } = appRepo(t, { ...BAD_QA, ".rightkit/app-check-baseline.json": "{ nope" });
  const broken = runAppCheck({ repo, central: null });
  assert.equal(broken.exitCode, 2);
  assert.match(broken.errText, /^check: cannot read the baseline \.rightkit\/app-check-baseline\.json: /);
  // --baseline wins over the app's file, and may be in either format.
  const central = writeBaseline(parent, { apprepo: [KNOWN_ID] });
  assert.equal(runAppCheck({ repo, central: null, baseline: central, only: ["qa-platform"] }).exitCode, 0);
  const appFormat = path.join(parent, "app-format.json");
  fs.writeFileSync(appFormat, JSON.stringify({ schema: 1, known: [appEntry(KNOWN_ID, "app#1")] }));
  assert.equal(runAppCheck({ repo, central: null, baseline: appFormat, only: ["qa-platform"], now: NOW }).exitCode, 0);
  assert.equal(runAppCheck({ repo, central: null, baseline: path.join(parent, "absent.json") }).exitCode, 2, "an explicit baseline must exist");
  // The app's file beats the central one; the central one is the last resort and only where a RightKit checkout is given.
  const rk = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-central-"));
  t.after(() => fs.rmSync(rk, { recursive: true, force: true }));
  fs.mkdirSync(path.join(rk, "ci"));
  fs.writeFileSync(path.join(rk, "ci/app-check-baseline.json"), JSON.stringify({ schema: 1, known: { apprepo: [KNOWN_ID] } }));
  fs.writeFileSync(path.join(repo, ".rightkit/app-check-baseline.json"), JSON.stringify({ schema: 1, known: [] }));
  commitAll(repo, "an empty app baseline");
  assert.equal(runAppCheck({ repo, central: rk, only: ["qa-platform"] }).exitCode, 1, "the app's own (empty) baseline wins over the central one");
  fs.rmSync(path.join(repo, ".rightkit"), { recursive: true });
  commitAll(repo, "no app baseline");
  assert.equal(runAppCheck({ repo, central: rk, only: ["qa-platform"] }).exitCode, 0, "no app baseline: the central file lists it");
  assert.equal(runAppCheck({ repo, central: null, only: ["qa-platform"] }).exitCode, 1, "an installed package has no central file");
  // This module finds a RightKit checkout from where it is loaded, and only when it is one.
  assert.equal(rightkitCheckout(), path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", ".."));
  assert.equal(rightkitCheckout(path.join(rk, "node_modules", "@rightkit", "release")), null);
});

// ---- which right-release commands run the check ------------------------------------------------------------------------------------

// Read as LF whatever the checkout's line endings (a Windows checkout may carry CRLF), so the anchors below match everywhere.
const cliSource = fs.readFileSync(new URL("./cli/right-release.mjs", import.meta.url), "utf8").replace(/\r\n/g, "\n");
const dispatchable = () => {
  const table = /const commands = new Map\(\[([\s\S]*?)\n\]\);/.exec(cliSource)?.[1] ?? "";
  const mapped = [...table.matchAll(/^\s*\["([a-z][a-z-]*)", "[^"]+\.mjs"\],?$/gm)].map((m) => m[1]);
  const chained = [...cliSource.matchAll(/first === "([a-z][a-z-]*)"/g)].map((m) => m[1]);
  return { mapped, all: [...new Set([...mapped, ...chained])].sort() };
};

test("every right-release command is checked or listed unchecked with a reason, and the checked set is the specified one", () => {
  const { mapped, all } = dispatchable();
  assert.ok(mapped.length > 30 && mapped.includes("build") && mapped.includes("ios-credentials"), `read the commands map: ${mapped.length} keys`);
  assert.ok(["dev", "doctor", "check", "hooks", "qualify"].every((name) => all.includes(name)), "the commands handled outside the map are read too");
  const checked = new Set(CHECKED_COMMANDS.keys());
  assert.deepEqual([...checked].sort(), [
    "addon build", "build", "create-mac-updater", "dev", "doctor", "github", "package", "publish", "publish-update", "qa", "recipe-build", "release", "sign-candidate", "sign-dev-artifact", "sign-updater", "sign-windows",
    "stage-sidecar", "swift-package", "test", "upload", "web-build",
  ]);
  for (const name of all) assert.ok(checked.has(name) !== UNCHECKED_COMMANDS.has(name), `${name}: must be in exactly one of CHECKED_COMMANDS and UNCHECKED_COMMANDS`);
  for (const name of [...checked, ...UNCHECKED_COMMANDS.keys()]) assert.ok(all.includes(name.split(" ")[0]), `${name}: is listed but right-release has no such command`);
  // A checked subcommand ("addon build") leaves the rest of its command unchecked, and that command's reason says so by name.
  for (const name of [...checked].filter((n) => n.includes(" "))) {
    const [command] = name.split(" ");
    assert.ok(!checked.has(command) && UNCHECKED_COMMANDS.has(command), `${name}: only the subcommand is checked, so ${command} is listed unchecked for its other forms`);
    assert.ok(UNCHECKED_COMMANDS.get(command).includes(`\`${name}\``), `${command}: the reason names the checked ${name}`);
  }
  for (const [name, reason] of UNCHECKED_COMMANDS) assert.ok(typeof reason === "string" && reason.length > 8, `${name}: says why the check does not guard it`);
  assert.deepEqual([...CHECKED_COMMANDS].filter(([, mode]) => mode === "worktree").map(([name]) => name).sort(), ["dev", "doctor", "sign-dev-artifact", "stage-sidecar", "test"], "the commands a person runs against the tree on disk see what a compile would; the rest check HEAD");
  assert.deepEqual([...CHECKED_COMMANDS].filter(([, mode]) => mode !== "worktree").map(([, mode]) => mode), Array(16).fill("commit"));
});

test("the compile, package and sign commands first left out of the check: eight are checked, in the mode of their siblings; dmg is unchecked for facts the source shows", () => {
  // test runs the app's suites (cargo, vitest) against the tree on disk, as dev does; stage-sidecar builds the sidecar crates from it
  // (apps call it from their own scripts, build:engine:dev among them); web-build is `build --web`, which build checks at HEAD.
  assert.equal(CHECKED_COMMANDS.get("test"), "worktree");
  assert.equal(CHECKED_COMMANDS.get("stage-sidecar"), "worktree");
  assert.equal(CHECKED_COMMANDS.get("web-build"), CHECKED_COMMANDS.get("build"), "web-build is the alias of build --web and checks as it does");
  assert.equal(checkModeFor(["build", "--web"]), checkModeFor(["web-build"]));
  for (const name of ["swift-package", "recipe-build", "sign-candidate"]) assert.equal(CHECKED_COMMANDS.get(name), "commit", `${name} packages, signs or runs the package build: HEAD, as build, sign-windows and sign-updater`);
  // sign-dev-artifact signs from the checked-out repository (the CI jobs run it in the workspace), so it checks the tree like dev;
  // addon build runs the add-on's build command over HEAD (it refuses a dirty tree), so it checks HEAD like its siblings
  assert.equal(CHECKED_COMMANDS.get("sign-dev-artifact"), "worktree");
  assert.equal(CHECKED_COMMANDS.get("addon build"), "commit");
  // the arguments each takes make no difference: --check, --list, --dry-run and --help are not ways past it
  for (const args of [["test"], ["test", "--list"], ["test", "--runner", "vitest", "--", "run"], ["stage-sidecar", "--check"], ["stage-sidecar", "--config", "x.mjs"], ["web-build", "--dry-run"], ["swift-package", "mac-app", "--config", "p.json"], ["recipe-build", "--plan-file", "plan.json"], ["sign-candidate", "--platform", "win", "--dry-run"],
    ["sign-dev-artifact", "--artifact", "a", "--manifest", "m.json"], ["sign-dev-artifact", "--dry-run"], ["sign-dev-artifact", "--help"], ["addon", "build", "--config", "a/right-release.config.mjs", "--platform", "win"], ["addon", "build", "--dry-run"]]) {
    assert.ok(checkModeFor(args), args.join(" "));
  }
  assert.equal(checkModeFor(["addon", "build", "--platform", "mac"]), "commit");
  assert.equal(checkModeFor(["sign-dev-artifact", "--artifact", "a"]), "worktree");
  // only the build form of addon is guarded: the others compile nothing (UNCHECKED_PROOF reads the source for that)
  for (const args of [["addon"], ["addon", "doctor", "--platform", "win"], ["addon", "upload", "--tier", "patch"], ["addon", "adopt", "--lock", "l.json", "--output", "o"], ["addon", "--platform", "win", "build"]]) {
    assert.equal(checkModeFor(args), null, args.join(" "));
  }
  // dmg is the one that is not: it takes --app and --out, compiles nothing and has no repository
  assert.equal(checkModeFor(["dmg", "build", "--app", "x.app"]), null);
  assert.deepEqual([...UNCHECKED_PROOF.keys()], ["dmg", "addon"]);
  for (const name of ["test", "web-build", "swift-package", "recipe-build", "stage-sidecar", "sign-candidate", "sign-dev-artifact"]) assert.ok(!UNCHECKED_COMMANDS.has(name), `${name} is no longer listed as unchecked`);
});

test("every unchecked command whose reason is about the source has its facts shown by the source, and they hold today", () => {
  const packageFile = (name) => fs.readFileSync(new URL(`./${name}`, import.meta.url), "utf8");
  for (const [name, places] of UNCHECKED_PROOF) {
    assert.ok(UNCHECKED_COMMANDS.has(name) && !CHECKED_COMMANDS.has(name), `${name} is listed as unchecked`);
    assert.ok(places.length >= 2, `${name}: at least two places show it`);
    for (const [file, needle] of places) assert.ok(packageFile(file).includes(needle), `${name}: ${file} no longer contains ${JSON.stringify(needle)}; the reason is not shown where it says, so classify ${name} again`);
  }
  // dmg: "takes --app/--out paths, compiles nothing, has no repository"
  assert.equal(UNCHECKED_COMMANDS.get("dmg"), "takes --app/--out paths, compiles nothing, has no repository");
  const dmg = packageFile("dmg-build.mjs");
  assert.match(dmg, /"--app, --out & --volname are required"/, "it takes the app bundle and the output path as paths");
  assert.doesNotMatch(dmg, /["'](?:cargo|rustc|pnpm|npm|npx|yarn|xcodebuild|swift|swiftc|tauri|node)["']/, "it starts no compiler or package manager");
  assert.doesNotMatch(dmg, /["']git["']|rev-parse|show-toplevel/, "and reads no repository");
  // addon: only `addon build` runs target.build.cmd, so it is the one form checked; the call sits in build(), before the next function
  const addon = packageFile("addon-command.mjs");
  assert.equal(addon.split("target.build.cmd").length - 1, 2, "target.build.cmd is printed by build --dry-run and run by build, and nowhere else");
  const ranAt = addon.indexOf("run(target.build.cmd, target.build.args, root);");
  assert.ok(addon.indexOf("async function build(") < ranAt && ranAt < addon.indexOf("function signExecutableRoles("), "the add-on's build command runs inside build()");
  assert.match(UNCHECKED_COMMANDS.get("addon"), /^every form but `addon build` \(which is checked\) compiles nothing/);
});

test("the CLI's own list of build commands (the ones assertReleaseExecutionAllowed refuses off a CI runner) is all checked, but cargo-target, which compiles nothing", () => {
  const listed = [...(/const githubBuildCommands = new Set\(\[([^\]]*)\]\)/.exec(cliSource)?.[1] ?? "").matchAll(/"([a-z][a-z-]*)"/g)].map((m) => m[1]);
  assert.ok(listed.length >= 10 && listed.includes("build") && listed.includes("test") && listed.includes("web-build"), `read the list: ${listed.join(",")}`);
  const notCompiles = new Map([["cargo-target", "cargo-target.mjs"]]);
  for (const name of listed) assert.ok(CHECKED_COMMANDS.has(name) || notCompiles.has(name), `${name}: the CLI treats it as a build command, so it is checked or shown here to compile nothing`);
  const cargoTarget = fs.readFileSync(new URL("./cargo-target.mjs", import.meta.url), "utf8");
  assert.match(cargoTarget, /const args = \["metadata", "--offline", "--format-version", "1", "--no-deps"/, "cargo-target only asks cargo for its metadata");
  assert.doesNotMatch(cargoTarget, /["'](?:build|test|check|run|install)["']/, "and starts no compile");
  assert.ok(UNCHECKED_COMMANDS.has("cargo-target"));
  // the subcommands the same refusal names: addon build is checked (it runs the add-on's build command); dmg build is unchecked, with its proof
  const named = [...(/if \(githubBuildCommands\.has\(first\)([\s\S]*?)\) \{\n\s+assertReleaseExecutionAllowed/.exec(cliSource)?.[1] ?? "").matchAll(/\(first === "([a-z][a-z-]*)" && args\[1\] === "([a-z][a-z-]*)"\)/g)].map((m) => `${m[1]} ${m[2]}`);
  assert.deepEqual(named.sort(), ["addon build", "dmg build"], "read the subcommands the CLI refuses off a CI runner");
  for (const pair of named) assert.ok(CHECKED_COMMANDS.has(pair) || UNCHECKED_PROOF.has(pair.split(" ")[0]), `${pair}: the CLI treats it as a build command, so it is checked or its reason is shown by the source`);
});

test("the README states the whole checked set with its modes, the baseline contract and the no-commit message", () => {
  const readme = fs.readFileSync(new URL("./README.md", import.meta.url), "utf8");
  const section = /## The app check[^\n]*\n([\s\S]*?)\n## /.exec(readme)?.[1] ?? "";
  assert.ok(section.length > 500, "the README has a section for the app check");
  const tokens = (mode) => [...(new RegExp(`^\\| \`${mode}\` [^|]*\\| (.*) \\|$`, "m").exec(section)?.[1] ?? "").matchAll(/`([^`]+)`/g)].map((m) => m[1]);
  const commands = (mode) => tokens(mode).filter((token) => !token.includes("--") && !token.includes("|"));
  for (const mode of ["commit", "worktree"]) {
    const documented = new Set(commands(mode));
    const actual = new Set([...CHECKED_COMMANDS].filter(([, m]) => m === mode).map(([name]) => name));
    assert.deepEqual([...documented].sort(), [...actual].sort(), `the README row for ${mode} lists exactly the commands checked in that mode`);
  }
  assert.match(section, /at most 90 days\s+after\s+it/);
  assert.match(section, /issue, an `added` date/);
  assert.match(section, /the repository has no commit yet; commit first/);
  assert.match(section, /`dmg` takes `--app`\/`--out` paths, compiles nothing and has no repository/);
  assert.match(section, /central `\{ "known": \{ "<repo>": \[\.\.\.\] \} \}` shape in an app's file is refused \(exit 2\)/);
});

test("the README's rule table lists every app rule, with its gap, in the order the check runs them, and describes G22", () => {
  const readme = fs.readFileSync(new URL("./README.md", import.meta.url), "utf8");
  const section = /## The app check[^\n]*\n([\s\S]*?)\n## /.exec(readme)?.[1] ?? "";
  const rows = [...section.matchAll(/^\| `([a-z0-9-]+)` \| ((?:G\d{2}|P\d+|SC-R\d|C3-\d{2})) \| (.+) \|$/gm)].map((m) => [m[1], m[2]]);
  assert.deepEqual(rows, RULES.map((r) => [r.id, r.gap]), "one row per rule in RULES, so a new rule cannot ship undocumented");
  assert.ok(RULES.some((r) => r.id === "e2e-under-cargo-test" && r.gap === "G22"));
  for (const phrase of ["`--no-run`", "`rightkit-qa run-tests`", "`rightkit-qa journeys run`", "`--test qa_surface_guard`", "`e2e-under-cargo-test|<file>|<script or crate>`"]) assert.ok(section.includes(phrase), `the G22 paragraph names ${phrase}`);
});

test("checkModeFor: the mode a command is checked in, direct flags run build, and the report forms are not guarded", () => {
  const mode = (...args) => checkModeFor(args);
  assert.equal(mode("build"), "commit");
  assert.equal(mode("build", "--web"), "commit");
  assert.equal(mode("upload", "--release", "x", "--tier", "patch"), "commit");
  assert.equal(mode("publish"), "commit");
  assert.equal(mode("publish", "--release", "x"), "commit");
  assert.equal(mode("dev", "--platform", "win"), "worktree");
  assert.equal(mode("doctor", "--gates"), "worktree");
  assert.equal(mode("--platform", "mac", "--tier", "patch"), "commit", "direct flags are `build <flags>`");
  assert.equal(mode("--skip-checks"), "commit");
  for (const none of [[], ["--version"], ["-v"], ["--help"], ["-h"], ["publish", "cargo", "--crate", "x"], ["publish", "npm", "--package", "@rightkit/x"], ["publish", "swift"], ["qa", "audit"], ["doctor", "--all"], ["check"], ["hooks", "install"], ["deps", "--check"], ["no-such-command"]]) assert.equal(checkModeFor(none), null, none.join(" "));
  assert.equal(mode("qa", "--stage", "source", "--platform", "win"), "commit");
  assert.equal(mode("build", "--help"), "commit", "--help is not a way past the check");
});

test("guardCommand: nothing for an unguarded command or a green check, the findings and why nothing started for a red one or a check that cannot run", (t) => {
  const { repo } = appRepo(t, { "a.txt": "x\n", "sub/right-release.config.mjs": "export default {};\n" });
  const calls = [];
  const fake = (exitCode, lines = [], errLines = []) => (options) => { calls.push(options); return { exitCode, lines, errLines }; };
  assert.equal(guardCommand({ args: ["cache", "status"], cwd: repo, run: fake(1) }), null);
  assert.deepEqual(calls, [], "an unguarded command runs no check");
  assert.equal(guardCommand({ args: ["build"], cwd: repo, run: fake(0, ["check: passed"]) }), null, "green prints nothing");
  assert.deepEqual(calls.map((c) => [c.repo, c.mode, c.label]), [[fs.realpathSync.native(repo), "commit", "check"]]);
  assert.equal(calls[0].refuse, BUILD_REFUSES, "a baseline cannot excuse what the build refuses");
  const red = guardCommand({ args: ["build"], cwd: repo, run: fake(1, ["FAIL x (G99): bad", "     fix: y", "check: 1 problem(s)"]) });
  assert.equal(red.exitCode, 1);
  assert.deepEqual(red.lines.slice(0, 3), ["FAIL x (G99): bad", "     fix: y", "check: 1 problem(s)"]);
  assert.match(red.lines.at(-1), /^right-release build: the app check found problems in .*; nothing was started\./);
  const broke = guardCommand({ args: ["upload", "--release", "r"], cwd: repo, run: fake(2, [], ["check: could not export the commit to check: x"]) });
  assert.equal(broke.exitCode, 1, "a check that cannot run is red too");
  assert.match(broke.lines.join("\n"), /could not export the commit to check: x[\s\S]*right-release upload: the app check could not run/);
  // The repository is the git top-level of --config's folder (either spelling), else of the cwd; dev and doctor check the worktree.
  calls.length = 0;
  guardCommand({ args: ["doctor", "--config", "sub/right-release.config.mjs"], cwd: repo, run: fake(0) });
  guardCommand({ args: ["dev", `--config=${path.join(repo, "sub", "right-release.config.mjs")}`], cwd: os.tmpdir(), run: fake(0) });
  assert.deepEqual(calls.map((c) => [c.repo, c.mode]), [[fs.realpathSync.native(repo), "worktree"], [fs.realpathSync.native(repo), "worktree"]]);
  assert.equal(appDirectory(["build"], "/cwd"), "/cwd");
  // recipe-build runs its plan in --app-root: that folder's repository is the one checked; --config still wins when both are given
  assert.equal(appDirectory(["recipe-build", "--plan-file", "p.json", "--app-root", "sub"], "/cwd"), path.resolve("/cwd", "sub"));
  assert.equal(appDirectory(["recipe-build", `--app-root=${path.resolve("/abs/app")}`], "/cwd"), path.resolve("/abs/app"));
  assert.equal(appDirectory(["x", "--config", "c/r.mjs", "--app-root", "other"], "/cwd"), path.dirname(path.resolve("/cwd", "c/r.mjs")));
  assert.equal(appDirectory(["recipe-build", "--app-root"], "/cwd"), "/cwd", "a flag with no value falls back to the working directory");
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-nogit-"));
  t.after(() => fs.rmSync(outside, { recursive: true, force: true }));
  const nogit = guardCommand({ args: ["--platform", "win"], cwd: outside, run: fake(0) });
  assert.equal(nogit.exitCode, 1);
  assert.match(nogit.lines[0], /^right-release build: the app check needs a git repository and .* is not inside one; nothing was started$/);
});

// ---- the installed package: no RightKit checkout ---------------------------------------------------------------------------------------

const PACKAGE = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const REPO_ROOT = path.resolve(PACKAGE, "..", "..");
const findingLines = (text) => text.split("\n").filter((l) => /^(FAIL|KNOWN|STALE|EXPIRED) |^     fix: /.test(l));

/** What `npm install @rightkit/release` leaves, minus node_modules: the package's files in <empty dir>/node_modules/@rightkit/release. */
function installedCopy(t) {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-installed-"));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  const dest = path.join(parent, "node_modules", "@rightkit", "release");
  fs.cpSync(PACKAGE, dest, { recursive: true, filter: (src) => !/(^|[\\/])(node_modules|\.git)([\\/]|$)/.test(path.relative(PACKAGE, src)) && !/\.test\.mjs$/.test(src) });
  return dest;
}

test("an installed copy of the package, with no RightKit checkout, prints the same findings as scripts/preflight.mjs: rightkit-gate check --repo <app> --commit HEAD", (t) => {
  const { repo } = appRepo(t, {
    ...FIXTURE,
    ".rightkit/app-check-baseline.json": JSON.stringify({ schema: 1, known: [liveEntry("skip-checks|package.json|ship", "app#7"), liveEntry("qa-platform|tests/gone.rs|platform:macos", "app#8")] }),
    "right-release.config.mjs": 'export default { distribution: { provider: "r2" } };\n',
    "tests/scenario.rs": 'fn t() { Suite::new().scenario("x"); println!("SKIP: no display"); return; }\n',
  });
  // G04 needs a CRLF blob committed before the attributes that forbid it (git add would normalise it otherwise).
  fs.writeFileSync(path.join(repo, "a.json"), "{}\r\n");
  commitAll(repo, "crlf");
  fs.writeFileSync(path.join(repo, ".gitattributes"), "*.json text eol=lf\n");
  git(repo, "add", ".gitattributes"); // not add -A: git would renormalise a.json in the same second (racy index entry)
  git(repo, "commit", "-q", "-m", "attributes after the fact");
  const installed = installedCopy(t);
  assert.equal(fs.existsSync(path.join(installed, "..", "..", "..", "ci")), false, "nothing of a RightKit checkout is around the installed copy");
  const gate = spawnSync(process.execPath, [path.join(installed, "bin", "rightkit-gate.mjs"), "check", "--repo", repo, "--commit", "HEAD"], { cwd: path.dirname(installed), encoding: "utf8" });
  const preflightRun = spawnSync(process.execPath, [path.join(REPO_ROOT, "scripts", "preflight.mjs"), "--repo", repo, "--commit", "HEAD"], { encoding: "utf8" });
  assert.equal(gate.status, 1, gate.stdout + gate.stderr);
  assert.equal(preflightRun.status, gate.status);
  const lines = findingLines(gate.stdout);
  assert.deepEqual(lines, findingLines(preflightRun.stdout), "the findings are byte-identical");
  assert.ok(lines.some((l) => l.startsWith("FAIL eol-policy (G04):")) && lines.some((l) => l.startsWith("FAIL release-config (G16):")) && lines.some((l) => l.startsWith("FAIL scenario-skip (G15):")) && lines.some((l) => l.startsWith("FAIL qa-platform (G13):")), lines.join("\n"));
  assert.ok(lines.some((l) => l.startsWith("KNOWN skip-checks:")) && lines.some((l) => l.startsWith("STALE baseline entry qa-platform|tests/gone.rs|platform:macos")), "the app's own baseline is read in both");
  assert.match(gate.stdout, /^check: 6 problem\(s\) in \w{12} \(25 rules, [\d.]+s, 1 known\)\./m);
  assert.match(preflightRun.stdout, /^preflight: 6 problem\(s\) in \w{12} \(32 rules, [\d.]+s, 1 known\)\./m);
  assert.deepEqual(spawnSync(process.execPath, [path.join(installed, "bin", "rightkit-gate.mjs"), "check", "--list"], { cwd: path.dirname(installed), encoding: "utf8" }).stdout.trim().split("\n"), RULES.map((r) => `${r.id} (${r.gap})`));
  // The same installed copy through the right-release command, and green once the app is fixed.
  const cli = spawnSync(process.execPath, [path.join(installed, "cli", "right-release.mjs"), "check", "--repo", repo, "--commit", "HEAD"], { cwd: path.dirname(installed), encoding: "utf8" });
  assert.equal(cli.status, 1);
  assert.deepEqual(findingLines(cli.stdout), lines);
});

test("the packed tarball ships app-check.mjs and no tests, and its rightkit-gate checks an app with neither a RightKit checkout nor node_modules", { skip: process.platform === "win32" && "npm and tar are not spawned directly on win32; the copy test above covers the installed layout" }, (t) => {
  const manifest = JSON.parse(fs.readFileSync(path.join(PACKAGE, "package.json"), "utf8"));
  assert.ok(manifest.files.includes("*.mjs") && manifest.files.includes("!*.test.mjs") && manifest.files.includes("bin") && manifest.files.includes("cli"));
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "rk-check-pack-"));
  t.after(() => fs.rmSync(out, { recursive: true, force: true }));
  const packed = spawnSync("npm", ["pack", "--ignore-scripts", "--json", "--pack-destination", out], { cwd: PACKAGE, encoding: "utf8" });
  if (packed.error || packed.status !== 0) { t.skip(`npm pack is unavailable here: ${packed.error?.message ?? packed.stderr.trim().slice(0, 200)}`); return; }
  const [info] = JSON.parse(packed.stdout);
  const names = info.files.map((f) => f.path);
  for (const shipped of ["app-check.mjs", "bin/rightkit-gate.mjs", "cli/right-release.mjs", "background-process.mjs"]) assert.ok(names.includes(shipped), `${shipped} is in the tarball`);
  assert.deepEqual(names.filter((n) => n.endsWith(".test.mjs")), [], "no test file ships");
  const root = path.join(out, "node_modules", "@rightkit", "release");
  fs.mkdirSync(root, { recursive: true });
  const untar = spawnSync("tar", ["-xzf", path.join(out, info.filename), "-C", root, "--strip-components=1"], { encoding: "utf8" });
  if (untar.error || untar.status !== 0) { t.skip(`tar is unavailable here: ${untar.error?.message ?? untar.stderr.trim().slice(0, 200)}`); return; }
  const { repo } = appRepo(t, FIXTURE);
  const run = (...args) => spawnSync(process.execPath, [path.join(root, "bin", "rightkit-gate.mjs"), "check", "--repo", repo, ...args], { cwd: out, encoding: "utf8" });
  const red = run("--commit", "HEAD");
  assert.equal(red.status, 1, red.stdout + red.stderr);
  assert.deepEqual(findingLines(red.stdout).filter((l) => l.startsWith("FAIL")).map((l) => l.split(":")[0]), ["FAIL qa-platform (G13)", "FAIL skip-checks (G17)"]);
  assert.equal(run("--commit", "HEAD", "--only", "speech-link").status, 0);
});

test("app-check.mjs and the commands that use it are outside the import closure of the protected ownership policy bundle", async () => {
  const { POLICY_BUNDLE_ENTRYPOINTS, scannerSourceFiles } = await import("./ownership-policy-bundle.mjs");
  const closure = scannerSourceFiles({ entrypoints: POLICY_BUNDLE_ENTRYPOINTS }).map((f) => f.path);
  assert.ok(closure.includes("ownership-hooks.mjs"), "the closure was read");
  for (const file of ["app-check.mjs", "bin/rightkit-gate.mjs", "cli/right-release.mjs"]) assert.ok(!closure.includes(file), `${file} would need a trust-sync if it were in the closure`);
});

test("scripts/preflight.mjs runs the same app rules: its rule list is these rules plus RightKit's own, in the documented order", async () => {
  const preflightModule = await import("../../scripts/preflight.mjs");
  const ids = preflightModule.RULES.map((r) => r.id);
  assert.deepEqual(ids.filter((id) => RULES.some((r) => r.id === id)), RULES.map((r) => r.id), "every app rule, in app-check's order");
  assert.deepEqual(ids.filter((id) => !RULES.some((r) => r.id === id)), ["consumer-recipe", "npm-recipe", "npm-surface", "version-pins", "dispatch-pins", "consumer-locks", "generated-docs"]);
  for (const id of RULES.map((r) => r.id)) assert.equal(preflightModule.RULES.find((r) => r.id === id), RULES.find((r) => r.id === id), `${id} is the same rule object`);
});
