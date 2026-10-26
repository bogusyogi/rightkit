import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { addDays } from "../packages/release/app-check.mjs";
import { cargoPackage, lockPathPackages, main, RULES, runRules } from "./preflight.mjs";

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
  for (const rule of RULES) assert.match(rule.gap, /^(G\d{2}|P\d+|SC-R\d|C3-\d{2}|CA-\d{2})$/, rule.id);
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
  assert.match(result.text, /passed 35 rules .*\(1 known finding\(s\) in \.rightkit\/app-check-baseline\.json/);
});

// ---- the shell contract (packages/release/shell-contract.mjs) runs here and is ratcheted like G13-G21 -----------------------
const SHELL_RULES = ["shell-window-hidden", "shell-plugin-order", "shell-handler", "shell-csp", "shell-grants", "qa-window-label", "devtools-declaration", "shell-lifecycle-external", "legal-receipt-native"];
const SHELL_APP = { "src-tauri/tauri.conf.json": JSON.stringify({ app: { windows: [{ label: "main", title: "App" }], security: { csp: "default-src 'self'; frame-ancestors 'none'; connect-src 'self'" } } }) };
const SHELL_ID = "shell-window-hidden|src-tauri/tauri.conf.json|base:main";

test("shell contract: --list shows the nine rules after speech-link, in order, with their SC-R gap ids", async () => {
  const listed = await preflight(["--list"]);
  assert.equal(listed.code, 0);
  // The three consumer source rules (CA-04, CA-07, CA-08) follow the shell contract.
  assert.deepEqual(listed.lines.slice(-12, -3), SHELL_RULES.map((id, i) => `${id} (SC-R${i + 1})`));
  assert.deepEqual(RULES.slice(-12, -3).map((r) => r.id), SHELL_RULES);
});

test("shell contract: a finding FAILs, is KNOWN when its identity is in the baseline, and is STALE once the app is fixed", async (t) => {
  const { repo, parent } = appRepo(t, SHELL_APP);
  const args = (known, ...more) => ["--repo", repo, "--worktree", "--only", SHELL_RULES.join(","), "--baseline", writeBaseline(parent, known), ...more];
  const fresh = await preflight(args({}));
  assert.equal(fresh.code, 1, fresh.text);
  assert.match(fresh.text, /^FAIL shell-window-hidden \(SC-R1\): src-tauri\/tauri\.conf\.json:\d+ window "main" is created at startup on base without "visible": false/m);
  assert.match(fresh.text, /^     fix: set "visible": false on window "main" in src-tauri\/tauri\.conf\.json/m);
  const known = await preflight(args({ apprepo: [SHELL_ID] }));
  assert.equal(known.code, 0, known.text);
  assert.match(known.text, /^KNOWN shell-window-hidden: src-tauri\/tauri\.conf\.json:\d+ window "main"/m);
  assert.match(known.text, /\(1 known finding\(s\)/);
  assert.equal((await preflight(args({ otherapp: [SHELL_ID] }))).code, 1, "another repository's entry does not excuse it");
  assert.equal((await preflight(["--repo", repo, "--worktree", "--only", "qa-platform", "--baseline", writeBaseline(parent, { apprepo: [SHELL_ID] })])).code, 0, "a run that skipped the rule cannot judge its entries stale");
  const fixed = { ...SHELL_APP, "src-tauri/tauri.conf.json": SHELL_APP["src-tauri/tauri.conf.json"].replace('"title":"App"', '"title":"App","visible":false') };
  fs.writeFileSync(path.join(repo, "src-tauri/tauri.conf.json"), fixed["src-tauri/tauri.conf.json"]);
  const stale = await preflight(args({ apprepo: [SHELL_ID] }));
  assert.equal(stale.code, 1, stale.text);
  assert.match(stale.text, /^STALE baseline entry shell-window-hidden\|src-tauri\/tauri\.conf\.json\|base:main: no longer found in apprepo/m);
  assert.equal((await preflight(args({}))).code, 0, "fixed, and nothing listed: green");
  const printed = await preflight(["--repo", repo, "--worktree", "--only", SHELL_RULES.join(","), "--print-baseline"]);
  assert.deepEqual(JSON.parse(printed.text), []);
});

test("shell contract: --print-baseline prints the identities of the shell findings for a person to paste", async (t) => {
  const { repo } = appRepo(t, { ...SHELL_APP, "src-tauri/tauri.macos.conf.json": JSON.stringify({ app: { windows: [{ label: "main" }] } }) });
  const printed = await preflight(["--repo", repo, "--worktree", "--only", SHELL_RULES.join(","), "--print-baseline"]);
  assert.deepEqual(JSON.parse(printed.text), [SHELL_ID, "shell-window-hidden|src-tauri/tauri.macos.conf.json|macos:main"]);
});

test("preflight keeps its rule order: the app rules sit where they always did among RightKit's own", () => {
  assert.deepEqual(RULES.map((r) => `${r.id} (${r.gap})`), [
    "eol-policy (G04)", "consumer-recipe (G01)", "npm-recipe (G01)", "npm-surface (G02)", "version-pins (G03)", "dispatch-pins (G05)", "consumer-locks (G08)", "file-url (G12)", "generated-docs (G10)", "retired-alias (P11)",
    "qa-platform (G13)", "test-harness (G14)", "scenario-skip (G15)", "release-config (G16)", "skip-checks (G17)", "qa-dev-server (G18)", "ffmpeg-release-escape (G19)", "switches-inventory (G20)", "speech-link (G21)", "e2e-under-cargo-test (G22)", "suite-key-literal (C3-04)", "suite-updater-key (C3-14)", "engine-record-store (C3-05)",
    "shell-window-hidden (SC-R1)", "shell-plugin-order (SC-R2)", "shell-handler (SC-R3)", "shell-csp (SC-R4)", "shell-grants (SC-R5)", "qa-window-label (SC-R6)", "devtools-declaration (SC-R7)", "shell-lifecycle-external (SC-R8)", "legal-receipt-native (SC-R9)",
    "workflow-action-pair (CA-04)", "referenced-input (CA-07)", "caddy-global-options (CA-08)",
  ]);
});

test("ratchet: this checkout's baseline keys one entry per repository with known debt", () => {
  const baseline = JSON.parse(fs.readFileSync(new URL("../ci/app-check-baseline.json", import.meta.url), "utf8"));
  assert.equal(baseline.schema, 1);
  assert.deepEqual(Object.keys(baseline.known).sort(), ["coderight", "cutright", "genright", "heardright", "mailright", "postright", "rightkit", "scraperight", "viewright"], "one entry per repository with known debt; apps with none are absent");
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
