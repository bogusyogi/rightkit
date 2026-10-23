import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { cargoPackage, lockPathPackages, RULES, runRules } from "./preflight.mjs";

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
  });
  const problems = runRules({ root, files: [], only: ["consumer-recipe", "npm-recipe"] }).failures.map((f) => f.problem);
  assert.ok(problems.some((p) => p.startsWith("rightkit-gone:")), problems.join("\n"));
  assert.ok(problems.some((p) => p.startsWith("@rightkit/gone:") && /missing reviewed PK example/.test(p)), problems.join("\n"));
  // At its published version the unit has nothing to publish, so no recipe is required yet.
  fs.writeFileSync(path.join(root, "packages/release/rightkit-versions.json"), JSON.stringify({ registry: { cargo: { "rightkit-gone": "0.2.0" }, npm: { "@rightkit/gone": "0.2.0" } } }));
  assert.deepEqual(runRules({ root, files: [], only: ["consumer-recipe", "npm-recipe"] }).failures, []);
});

test("P11: retireAliasText rewrites to rustright and is idempotent", async () => {
  const { retireAliasText } = await import("./preflight.mjs");
  const text = `${OLD} cargo test && ${OLD} status x\n["${OLD}", "cargo", "build"]\n`;
  const once = retireAliasText(text);
  assert.equal(once.count, 3);
  assert.equal(once.text, `rustright cargo test && rustright status x\n["rustright", "cargo", "build"]\n`);
  assert.equal(retireAliasText(once.text).count, 0);
});
