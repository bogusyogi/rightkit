// The consumer input contracts (consumer-input-contracts.mjs) against SwiftRight's own historical bytes: each rule has the original
// failing input (red), the repair (green) and positive controls that keep legitimate shapes allowed. Fixtures under
// fixtures/consumer-input/ are copied verbatim from Orthic-Labs/swiftright with `git show <rev>:<path>` (the revision is in each
// name); long-paths.3561a4722.txt is every tracked path of 3561a4722 (the tree run 38363145934 checked out) that the rule
// reports, 829 of them, the count SwiftRight's docs/sr0-status.md recorded for that run.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  CONSUMER_INPUT_RULES, coneIncludesFile, markdownFirstCells, overLongPaths, scriptPaths, selectValues, windowsWorkspace,
} from "./consumer-input-contracts.mjs";
import { RULES } from "./app-check.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIX = path.join(HERE, "fixtures", "consumer-input");
const fixture = (name) => fs.readFileSync(path.join(FIX, name), "utf8");
const rule = (id) => CONSUMER_INPUT_RULES.find((r) => r.id === id);

/** A tree in a temporary directory named `name` (the workspace name the long-path rule reads): { root, files }. `extra` paths
 * are listed as tracked without content (the rules read only their names). */
function tree(t, name, contents, extra = []) {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), "rk-cic-"));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  const root = path.join(parent, name);
  for (const [rel, text] of Object.entries(contents)) { fs.mkdirSync(path.join(root, path.dirname(rel)), { recursive: true }); fs.writeFileSync(path.join(root, rel), text); }
  fs.mkdirSync(root, { recursive: true });
  return { root, repo: root, repositoryName: name, files: [...new Set([...Object.keys(contents), ...extra])].sort() };
}
const run = (id, input) => rule(id).run(input);

// ---- CA-07 checkout-step-input ------------------------------------------------------------------------------------------------

const WGC = "factory/native/windows/wgc-record/wgc-record.cpp";
const tracked80c9 = fixture("tracked.80c9be45d.txt").split("\n").filter(Boolean);

test("CA-07 original: factory-pairs.yml at 80c9be45d compiles the WGC recorder its windows sparse checkout leaves out", (t) => {
  const found = run("checkout-step-input", tree(t, "swiftright", { ".github/workflows/factory-pairs.yml": fixture("factory-pairs.80c9be45d.yml") }, tracked80c9));
  assert.equal(found.length, 1, JSON.stringify(found, null, 2));
  assert.equal(found[0].subject, `windows:${WGC}`);
  assert.match(found[0].problem, /job windows: step "Compile the WGC recorder \(screen_record=wgc only\)" reads factory\/native\/windows\/wgc-record\/wgc-record\.cpp, which its sparse checkout \(packages, probes\/sr0-s\/fresh-runtime, factory\/pipeline, factory\/tools\) does not materialize/);
  assert.match(found[0].fix, /add factory\/native\/windows\/wgc-record to the job's sparse-checkout list/);
});

test("CA-07 repair: 2a67fc182 adds the recorder directory to the cone and the check is green", (t) => {
  assert.deepEqual(run("checkout-step-input", tree(t, "swiftright", { ".github/workflows/factory-pairs.yml": fixture("factory-pairs.2a67fc182.yml") }, tracked80c9)), []);
});

test("CA-07 positive controls: a full checkout, git pathspecs, globs, comments and untracked words are not step inputs", (t) => {
  const full = fixture("factory-pairs.80c9be45d.yml").replace(/\n {10}sparse-checkout: \|\n(?: {12}\S.*\n)+ {10}sparse-checkout-cone-mode: true/g, "");
  assert.equal(/sparse-checkout/.test(full), false, "every sparse checkout removed");
  assert.deepEqual(run("checkout-step-input", tree(t, "swiftright", { ".github/workflows/factory-pairs.yml": full }, tracked80c9)), []);
  const wf = `on: push
jobs:
  build:
    runs-on: windows-2025
    steps:
      - uses: actions/checkout@v7
        with:
          sparse-checkout: |
            packages
      - run: |
          # factory/native/windows/wgc-record/wgc-record.cpp is compiled by another job
          git diff --name-only HEAD~1 -- factory/native/windows/wgc-record
          case "$p" in factory/native/windows/*.cpp) echo yes ;; esac
          echo "see docs/not-tracked/file.md"
          python packages/tool.py
`;
  assert.deepEqual(run("checkout-step-input", tree(t, "app", { ".github/workflows/ci.yml": wf }, [WGC, "packages/tool.py"])), []);
});

test("CA-07 distinct negative: a working-directory outside the cone, and a checkout under with.path", (t) => {
  const wf = `on: push
jobs:
  probe:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v7
        with:
          path: src
          sparse-checkout: |
            packages
      - run: swift build
        working-directory: src/probes/f0-text-facts
      - run: python src/packages/tool.py
`;
  const found = run("checkout-step-input", tree(t, "app", { ".github/workflows/probe.yml": wf }, ["probes/f0-text-facts/Package.swift", "packages/tool.py"]));
  assert.deepEqual(found.map((f) => f.subject), ["probe:probes/f0-text-facts"]);
});

test("CA-07 cone semantics: root files and files directly in a listed directory's parents are materialized", () => {
  const cone = ["factory/native/windows/wgc-record"];
  assert.equal(coneIncludesFile(cone, "package.json"), true);
  assert.equal(coneIncludesFile(cone, "factory/README.md"), true);
  assert.equal(coneIncludesFile(cone, "factory/native/windows/wgc-record/wgc-record.cpp"), true);
  assert.equal(coneIncludesFile(cone, "factory/tools/ci_pair.py"), false);
  assert.deepEqual(scriptPaths(`$src = Join-Path $env:GITHUB_WORKSPACE 'factory\\native\\windows\\wgc-record\\wgc-record.cpp'`), [WGC]);
});

// ---- CA-09 source-relation ------------------------------------------------------------------------------------------------------

const HAND = "factory/tools/proven-controls/hand-entries.json";
const STATUS = "docs/sr1-status.md";

test("CA-09 original: 7655d1d87 escaped docs/sr1-status.md into one line, so hand entry I2 names no row", (t) => {
  const found = run("source-relation", tree(t, "swiftright", { [HAND]: fixture("hand-entries.7655d1d87.json"), [STATUS]: fixture("sr1-status.7655d1d87.head.md") }));
  assert.equal(found.length, 1, JSON.stringify(found, null, 2));
  assert.equal(found[0].subject, "proven-controls-status-row:I2");
  assert.match(found[0].problem, /\[0\]\.statusDocBlock names row "I2", which docs\/sr1-status\.md does not have \(0 table row id\(s\) before its first --- line; a document escaped into one line has none\)/);
});

test("CA-09 repair: the same document with its escapes turned back into lines has the I2 row", (t) => {
  const repaired = fixture("sr1-status.7655d1d87.head.md").replaceAll("\\n", "\n");
  assert.deepEqual(run("source-relation", tree(t, "swiftright", { [HAND]: fixture("hand-entries.7655d1d87.json"), [STATUS]: repaired })), []);
});

test("CA-09 positive controls: the parent commit's document, and a tree without the source file", (t) => {
  const parent = fixture("sr1-status.7655d1d87-parent.head.md");
  assert.ok(markdownFirstCells(parent).has("I2"));
  assert.deepEqual(run("source-relation", tree(t, "swiftright", { [HAND]: fixture("hand-entries.7655d1d87.json"), [STATUS]: parent })), []);
  assert.deepEqual(run("source-relation", tree(t, "other", { [STATUS]: "# nothing\n" })), [], "the relation applies only where its source is committed");
});

test("CA-09 distinct negatives: a row only after the --- separator, a missing target, a missing key", (t) => {
  const entry = JSON.parse(fixture("hand-entries.7655d1d87.json"));
  const after = "# s\n\n| id | x |\n|---|---|\n| I3 x | y |\n---\n| I2 late | y |\n";
  assert.deepEqual(run("source-relation", tree(t, "a", { [HAND]: JSON.stringify(entry), [STATUS]: after })).map((f) => f.subject), ["proven-controls-status-row:I2"]);
  assert.deepEqual(run("source-relation", tree(t, "b", { [HAND]: JSON.stringify(entry) })).map((f) => f.subject), ["proven-controls-status-row:target"]);
  const noKey = entry.map(({ statusDocBlock, ...rest }) => rest);
  assert.deepEqual(run("source-relation", tree(t, "c", { [HAND]: JSON.stringify(noKey), [STATUS]: fixture("sr1-status.7655d1d87-parent.head.md") })).map((f) => f.subject), ["proven-controls-status-row:[0].statusDocBlock"]);
});

test("CA-09 an app declares its own relation in .rightkit/source-relations.json; a bad declaration is a finding", (t) => {
  const decl = { schema: 1, relations: [{ id: "plan-rows", source: "plan/items.json", select: "items[*].row", target: "docs/plan.md", adapter: "markdown-table-first-cell" }] };
  const doc = "| A1 first | x |\n| A2 second | y |\n";
  const ok = tree(t, "a", { ".rightkit/source-relations.json": JSON.stringify(decl), "plan/items.json": JSON.stringify({ items: [{ row: "A1" }, { row: "A2" }] }), "docs/plan.md": doc });
  assert.deepEqual(run("source-relation", ok), []);
  const bad = tree(t, "b", { ".rightkit/source-relations.json": JSON.stringify(decl), "plan/items.json": JSON.stringify({ items: [{ row: "A9" }] }), "docs/plan.md": doc });
  assert.deepEqual(run("source-relation", bad).map((f) => f.subject), ["plan-rows:A9"]);
  const wrong = tree(t, "c", { ".rightkit/source-relations.json": JSON.stringify({ schema: 1, relations: [{ id: "x", source: "a", select: "b", target: "c", adapter: "regex" }] }) });
  assert.deepEqual(run("source-relation", wrong).map((f) => f.subject), ["declaration"]);
  assert.deepEqual(selectValues({ a: [{ b: "x" }, {}] }, "a[*].b"), [{ at: "a[0].b", value: "x" }, { at: "a[1].b", value: undefined }]);
});

// ---- CA-11 checkout-longpaths ---------------------------------------------------------------------------------------------------

const LONG = fixture("long-paths.3561a4722.txt").split("\n").filter(Boolean);
const SHORT = ["package.json", "scripts/gate.sh", "examples/SliceApp/right-release.config.mjs"];
const CI = fixture("ci.93f9c1fc49.yml");
const LONGPATHS_STEP = "      - name: Enable Windows long paths\n        if: runner.os == 'Windows'\n        shell: bash\n        run: git config --system core.longpaths true\n";
const SPARSE = /\n {10}sparse-checkout: \|\n(?: {12}\S.*\n)+ {10}sparse-checkout-cone-mode: true/;
// The workflow run 38363145934 used is not on SwiftRight's master: right-git rendered a plain actions/checkout (docs/sr0-status.md,
// 3561a4722). 93f9c1fc49 is the same template rendered with the long-path step and a sparse checkout; without both it is that plain one.
const PLAIN = CI.replace(LONGPATHS_STEP, "").replace(SPARSE, "");

test("CA-11 calibration: the rule counts exactly the 829 paths SwiftRight recorded for run 38363145934", () => {
  const prefix = windowsWorkspace("swiftright");
  assert.equal(prefix, "D:\\a\\swiftright\\swiftright\\");
  const long = overLongPaths([...LONG, ...SHORT], prefix);
  assert.equal(long.files.length, 829);
  assert.equal(prefix.length + long.longest.length, 375);
});

test("CA-11 original: a windows-2025 job with a plain checkout of the 3561a4722 tree and no long paths", (t) => {
  assert.ok(CI.includes(LONGPATHS_STEP) && SPARSE.test(CI) && !PLAIN.includes("longpaths") && !PLAIN.includes("sparse-checkout"));
  const found = run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": PLAIN }, [...LONG, ...SHORT]));
  assert.equal(found.length, 1, JSON.stringify(found, null, 2));
  assert.equal(found[0].subject, "ci");
  assert.match(found[0].problem, /^\.github\/workflows\/ci\.yml job ci \(windows-2025\) checks out 829 tracked file path\(s\) longer than 259 characters under D:\\a\\swiftright\\swiftright\\ \(longest: 375 characters, .+\) without Git long paths: git fails with "Filename too long"$/);
});

test("CA-11 repair: 93f9c1fc49's ci.yml enables core.longpaths before its checkout", (t) => {
  assert.deepEqual(run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": CI }, [...LONG, ...SHORT])), []);
});

test("CA-11 positive controls: long paths stay legal off Windows, outside the cone, on a self-hosted runner, or set through GIT_CONFIG_*", (t) => {
  const files = [...LONG, ...SHORT];
  const ubuntu = PLAIN.replace('os: ["windows-2025"]', 'os: ["ubuntu-24.04"]');
  assert.notEqual(ubuntu, PLAIN);
  assert.deepEqual(run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": ubuntu }, files)), []);
  const coneOnly = CI.replace(LONGPATHS_STEP, "");
  assert.deepEqual(run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": coneOnly }, files)), [], "the cone leaves every long path out");
  const selfHosted = PLAIN.replace("runs-on: ${{ matrix.os }}", "runs-on: [self-hosted, windows]");
  assert.notEqual(selfHosted, PLAIN);
  assert.deepEqual(run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": selfHosted }, files)), []);
  const viaEnv = PLAIN.replace("    runs-on: ${{ matrix.os }}\n", "    runs-on: ${{ matrix.os }}\n    env:\n      GIT_CONFIG_COUNT: \"1\"\n      GIT_CONFIG_KEY_0: core.longpaths\n      GIT_CONFIG_VALUE_0: \"true\"\n");
  assert.notEqual(viaEnv, PLAIN);
  assert.deepEqual(run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": viaEnv }, files)), []);
  assert.deepEqual(run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": PLAIN }, SHORT)), [], "short paths need nothing");
});

test("CA-11 distinct negative: core.longpaths enabled only after the checkout does not count", (t) => {
  const late = PLAIN.replace("      - name: Restore last green commit marker\n", `${LONGPATHS_STEP}      - name: Restore last green commit marker\n`);
  assert.notEqual(late, PLAIN);
  const found = run("checkout-longpaths", tree(t, "swiftright", { ".github/workflows/ci.yml": late }, [...LONG, ...SHORT]));
  assert.equal(found.length, 1);
  assert.match(found[0].problem, /\(core\.longpaths is enabled only after the checkout\)/);
});

// ---- the gate runs them -----------------------------------------------------------------------------------------------------------

/** Commit `contents` in a fresh repository in a directory named `dir` with remote origin `origin` (null: none), run
 * `rightkit-gate check --commit HEAD --only <rules>` on it, and return { status, stdout }. */
function gate(t, dir, contents, rules, origin = "https://github.com/Orthic-Labs/swiftright.git") {
  const { root } = tree(t, dir, contents);
  const git = (...args) => spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.longpaths=true", ...args], { cwd: root, encoding: "utf8" });
  assert.equal(git("init", "-q").status, 0);
  if (origin) assert.equal(git("remote", "add", "origin", origin).status, 0);
  assert.equal(git("add", "-A").status, 0);
  assert.equal(git("commit", "-q", "-m", "fixture").status, 0);
  const r = spawnSync(process.execPath, [path.join(HERE, "bin", "rightkit-gate.mjs"), "check", "--repo", root, "--commit", "HEAD", "--only", rules.join(",")], { encoding: "utf8" });
  return { status: r.status, stdout: r.stdout + r.stderr };
}
const longFile = (length) => `deep/${"x".repeat(length - 5)}`;
/** A windows-2025 job: optional job env lines, steps before the checkout, then a plain checkout. */
const windowsJob = ({ env = "", before = "" } = {}) => `on: push
jobs:
  ci:
    runs-on: windows-2025
${env}    steps:
${before}      - uses: actions/checkout@v7
      - run: echo built
`;
const LONGPATHS_ONLY = ["checkout-longpaths"];

test("rightkit-gate check runs the three rules on a committed app tree and reports each original failure", (t) => {
  assert.deepEqual(RULES.filter((r) => r.gap.startsWith("CA-")).map((r) => `${r.id} (${r.gap})`), ["checkout-step-input (CA-07)", "source-relation (CA-09)", "checkout-longpaths (CA-11)"]);
  const result = gate(t, "swiftright", {
    ".github/workflows/factory-pairs.yml": fixture("factory-pairs.80c9be45d.yml"),
    ".github/workflows/ci.yml": PLAIN,
    [HAND]: fixture("hand-entries.7655d1d87.json"),
    [STATUS]: fixture("sr1-status.7655d1d87.head.md"),
    [WGC]: "// recorder\n",
    "factory/tools/ci_pair.py": "# planner\n",
    [LONG[0]]: "x\n",
  }, ["checkout-step-input", "source-relation", "checkout-longpaths"]);
  assert.equal(result.status, 1, result.stdout);
  assert.match(result.stdout, /^FAIL checkout-step-input \(CA-07\): .*reads factory\/native\/windows\/wgc-record\/wgc-record\.cpp/m);
  assert.match(result.stdout, /^FAIL source-relation \(CA-09\): .*names row "I2", which docs\/sr1-status\.md does not have/m);
  assert.match(result.stdout, /^FAIL checkout-longpaths \(CA-11\): \.github\/workflows\/ci\.yml job ci .*checks out 1 tracked file path\(s\) longer than 259 characters under D:\\a\\swiftright\\swiftright\\/m);
});

test("CA-11 through the gate: a step counts only when it provably runs the git config line; if: false and echo are UNSUPPORTED", (t) => {
  const file = longFile(240);
  const red = (before) => gate(t, "swiftright", { ".github/workflows/ci.yml": windowsJob({ before }), [file]: "x\n" }, LONGPATHS_ONLY);
  const ifFalse = red("      - if: false\n        run: git config --system core.longpaths true\n");
  assert.equal(ifFalse.status, 1, ifFalse.stdout);
  assert.match(ifFalse.stdout, /^FAIL checkout-longpaths \(CA-11\): UNSUPPORTED: \.github\/workflows\/ci\.yml job ci .*without provable Git long paths/m);
  const echoOnly = red("      - run: echo \"git config --global core.longpaths true\"\n");
  assert.equal(echoOnly.status, 1, echoOnly.stdout);
  assert.match(echoOnly.stdout, /^FAIL checkout-longpaths \(CA-11\): UNSUPPORTED: /m);
  const real = red("      - run: git config --system core.longpaths true\n");
  assert.equal(real.status, 0, real.stdout);
});

test("CA-11 through the gate: GIT_CONFIG_* counts only when GIT_CONFIG_COUNT is a positive integer above the pair's index", (t) => {
  const file = longFile(240);
  const run = (vars) => gate(t, "swiftright", { ".github/workflows/ci.yml": windowsJob({ env: `    env:\n${Object.entries(vars).map(([k, v]) => `      ${k}: "${v}"\n`).join("")}` }), [file]: "x\n" }, LONGPATHS_ONLY);
  const pair = { GIT_CONFIG_KEY_0: "core.longpaths", GIT_CONFIG_VALUE_0: "true" };
  for (const [label, vars] of [["COUNT absent", pair], ["COUNT 0", { GIT_CONFIG_COUNT: "0", ...pair }], ["index at COUNT", { GIT_CONFIG_COUNT: "1", GIT_CONFIG_KEY_1: "core.longpaths", GIT_CONFIG_VALUE_1: "true" }], ["non-integer COUNT", { GIT_CONFIG_COUNT: "1.0", ...pair }]]) {
    const r = run(vars);
    assert.equal(r.status, 1, `${label}: ${r.stdout}`);
    assert.match(r.stdout, /^FAIL checkout-longpaths \(CA-11\): /m, label);
  }
  const ok = run({ GIT_CONFIG_COUNT: "1", ...pair });
  assert.equal(ok.status, 0, ok.stdout);
});

test("CA-11 through the gate: the budget is the runner workspace of the GitHub repository, not the checker's directory", (t) => {
  // D:\a\swiftright\swiftright\ is 27 characters: a 240-character path is 267 there, but 249 under a checker directory named "a".
  const wf = { ".github/workflows/ci.yml": windowsJob() };
  const over = gate(t, "a", { ...wf, [longFile(240)]: "x\n" }, LONGPATHS_ONLY);
  assert.equal(over.status, 1, over.stdout);
  assert.match(over.stdout, /^FAIL checkout-longpaths \(CA-11\): .*under D:\\a\\swiftright\\swiftright\\ \(longest: 267 characters/m);
  const under = gate(t, "a", { ...wf, [longFile(230)]: "x\n" }, LONGPATHS_ONLY);
  assert.equal(under.status, 0, under.stdout);
  const unknown = gate(t, "swiftright", { ...wf, [longFile(240)]: "x\n" }, LONGPATHS_ONLY, null);
  assert.equal(unknown.status, 1, unknown.stdout);
  assert.match(unknown.stdout, /^FAIL checkout-longpaths \(CA-11\): UNSUPPORTED: .*the repository name is unknown/m);
});

test("CA-11 through the gate: the last GIT_CONFIG pair below COUNT wins", (t) => {
  const file = longFile(240);
  const run = (vars) => gate(t, "swiftright", { ".github/workflows/ci.yml": windowsJob({ env: `    env:\n${Object.entries(vars).map(([k, v]) => `      ${k}: "${v}"\n`).join("")}` }), [file]: "x\n" }, LONGPATHS_ONLY);
  const pairs = (v0, v1) => ({ GIT_CONFIG_COUNT: "2", GIT_CONFIG_KEY_0: "core.longpaths", GIT_CONFIG_VALUE_0: v0, GIT_CONFIG_KEY_1: "core.longpaths", GIT_CONFIG_VALUE_1: v1 });
  for (const [label, vars] of [["true then false", pairs("true", "false")], ["1 then 0", pairs("1", "0")]]) {
    const r = run(vars);
    assert.equal(r.status, 1, `${label}: ${r.stdout}`);
    assert.match(r.stdout, /^FAIL checkout-longpaths \(CA-11\): \.github\/workflows\/ci\.yml job ci \(windows-2025\) checks out 1 tracked file path\(s\) .* without Git long paths/m, label);
  }
  assert.equal(run(pairs("false", "true")).status, 0, "false then true: the last pair wins");
  assert.equal(run({ GIT_CONFIG_COUNT: "1", GIT_CONFIG_KEY_0: "core.longpaths", GIT_CONFIG_VALUE_0: "true" }).status, 0);
});

test("CA-11 through the gate: the value in effect at the checkout decides, not any earlier enable", (t) => {
  const file = longFile(240);
  const red = (before, checkoutEnv = "") => gate(t, "swiftright", { ".github/workflows/ci.yml": windowsJob({ before }).replace("      - uses: actions/checkout@v7\n", `      - uses: actions/checkout@v7\n${checkoutEnv}`), [file]: "x\n" }, LONGPATHS_ONLY);
  const enable = "      - run: git config --system core.longpaths true\n";
  const laterOff = red(`${enable}      - run: git config --global core.longpaths false\n`);
  assert.equal(laterOff.status, 1, laterOff.stdout);
  assert.match(laterOff.stdout, /^FAIL checkout-longpaths \(CA-11\): \.github\/workflows\/ci\.yml job ci .* without Git long paths/m);
  const envOff = red(enable, '        env:\n          GIT_CONFIG_COUNT: "1"\n          GIT_CONFIG_KEY_0: core.longpaths\n          GIT_CONFIG_VALUE_0: "false"\n');
  assert.equal(envOff.status, 1, envOff.stdout);
  assert.match(envOff.stdout, /^FAIL checkout-longpaths \(CA-11\): \.github\/workflows\/ci\.yml job ci .* without Git long paths/m);
  assert.equal(red(`${enable}      - run: echo unrelated\n`).status, 0, "an unrelated step keeps it on");
  assert.equal(red("      - run: git config --system core.longpaths false\n      - run: git config --system core.longpaths true\n").status, 0, "disable then enable");
});
