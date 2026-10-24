// The desktop app template contract (bogusyogi/rightkit#906 and the template rebuild): the template is what every new app starts from,
// so it must satisfy, from its first commit, the same evaluators RightKit runs on a finished app. This file instantiates the template
// the way a person does (`right-release init` on a copy, committed to a fresh git repository) and runs those evaluators on the
// instance, then checks the template files the evaluators cannot see (pins, the committed hook, advisory policy, placeholders).
// Every assertion names the thing that breaks the day it regresses.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { hookText } from "./bin/rightkit-gate.mjs";
import { assertCargoProfileContract } from "./cargo-profile-contract.mjs";
import { validateRightKitCargoContract } from "./cargo-contract.mjs";
import { initTemplate, sortCargoLock } from "./init-command.mjs";
import { assertLegalReleaseContract } from "./legal-contract.mjs";
import { loadGeneratorRegistry } from "./pipeline-generators.mjs";
import { scanApp } from "./pipeline-scan.mjs";
import { assertQaAdoption } from "./qa-contract.mjs";
import { loadReleaseConfigSync } from "./release-config.mjs";
import { parseInventory, sourceSwitchFindings } from "./release-switch-scan.mjs";
import { checkApp } from "./ownership-mandatory-set.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(here, "..", "..");
const TEMPLATE = path.join(REPO, "desktop-app-template");
const read = (...parts) => readFileSync(path.join(TEMPLATE, ...parts), "utf8");
const readJson = (...parts) => JSON.parse(read(...parts));
const versions = JSON.parse(readFileSync(path.join(here, "rightkit-versions.json"), "utf8"));

const TITLE = ["Right", "App"].join("");
const LOWER = ["right", "app"].join("");

function walk(dir, keep = () => true, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (["node_modules", "target", "dist", ".git", ".right-release"].includes(entry.name)) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, keep, out);
    else if (keep(full)) out.push(full);
  }
  return out;
}

const git = (cwd, ...args) => spawnSync("git", ["-c", "user.name=Template Contract", "-c", "user.email=noreply@example.invalid", ...args], { cwd, encoding: "utf8" });

// Created at module scope so the cleanup hook belongs to the file, not to whichever test happens to build the instance first.
const SCRATCH = tempRoot("template-contract-");
let instanceRoot;
/** A copy of the template, renamed by `init` and committed, exactly as a new app's repository starts. Built once per file. */
function instance() {
  if (instanceRoot) return instanceRoot;
  const dest = path.join(SCRATCH, "playright");
  cpSync(TEMPLATE, dest, { recursive: true, filter: (src) => !/(^|[\\/])(node_modules|target|dist|\.right-release)([\\/]|$)/.test(path.relative(TEMPLATE, src)) });
  initTemplate(dest, { name: "PlayRight", id: "app.orthiclabs.playright" });
  assert.equal(git(dest, "init", "-q", "-b", "main").status, 0);
  assert.equal(git(dest, "add", "-A").status, 0);
  assert.equal(git(dest, "commit", "-q", "-m", "instance").status, 0);
  instanceRoot = dest;
  return dest;
}

// ---- the instance passes the evaluators RightKit runs on an app ---------------------------------------------------------------

test("init renames the whole template identity and leaves a Cargo.lock that --locked accepts", () => {
  const root = instance();
  const leftovers = walk(root, (f) => /\.(json|toml|mjs|ts|tsx|rs|md|html|css|yaml|yml|sh|lock)$/.test(f) && !f.endsWith("pnpm-lock.yaml"))
    .flatMap((f) => readFileSync(f, "utf8").split("\n").map((line, i) => ({ f: path.relative(root, f), line: i + 1, text: line })))
    // `r2-rightapps` is the RightApps distribution service, not the template identity: init keeps it (renameText).
    .filter(({ text }) => new RegExp(`${LOWER}(?!s\\b)|${TITLE}(?!s\\b)|${LOWER.toUpperCase()}`).test(text.replaceAll(`r2-${LOWER}s`, "")));
  assert.deepEqual(leftovers, []);
  const lock = readFileSync(path.join(root, "Cargo.lock"), "utf8");
  assert.ok(lock.includes('name = "playright-engine"'));
  assert.equal(sortCargoLock(lock), lock, "init left Cargo.lock in a different order than cargo writes it, so every cargo --locked script fails on the first build");
  assert.equal(JSON.parse(readFileSync(path.join(root, "right-release.config.json"), "utf8")).distribution.provider, `r2-${LOWER}s`);
});

test("the release config is JSON only (M24) and loads with every command admitted", () => {
  for (const legacy of ["right-release.config.mjs", "right-release.config.js", "right-release.config.cjs", "right-release.config.ts"]) {
    assert.equal(existsSync(path.join(TEMPLATE, legacy)), false, `${legacy} is an executable release config; M24 allows right-release.config.json only`);
  }
  const loaded = loadReleaseConfigSync(path.join(instance(), "right-release.config.json"), { registry: loadGeneratorRegistry() });
  assert.equal(loaded.config.app, "playright");
  assert.ok(loaded.config.targets.win.winPackage && loaded.config.targets.mac.macPackage, "packaging is declarative recipes, not scripts");
  assert.ok(loaded.config.sidecars?.sidecars?.length, "the engine sidecar is staged by `right-release stage-sidecar`, declared in the config");
  assert.equal(loaded.config.legal.manifest, "legal/legal-manifest.json");
  assert.ok(loaded.config.qa, "the QA lane is declared in the release config");
  assert.equal(existsSync(path.join(TEMPLATE, "scripts")), false, "the template carries no packaging or helper scripts");
  assert.equal(existsSync(path.join(TEMPLATE, "crates", "tools")), false, "crates/tools (rightapp-tools) was replaced by right-release init and the declarative recipes");
});

test("the pipeline scan finds nothing (M24) and the source switch walk finds no undeclared environment read (M23)", () => {
  const root = instance();
  const scan = scanApp({ appRoot: root, registry: loadGeneratorRegistry() });
  assert.deepEqual(scan.findings.map((f) => `${f.code} ${f.file}:${f.line} ${f.message}`), []);
  const inventoryText = readFileSync(path.join(root, "switches.json"), "utf8");
  assert.deepEqual(parseInventory(inventoryText).issues, []);
  assert.deepEqual(sourceSwitchFindings({ sourceRoot: root, inventoryText }).map((f) => `${f.rule} ${f.file} ${f.reason}`), []);
});

test("the QA read of the engine path is compiled out of every build without qa-native, and says so in switches.json", () => {
  const lib = read("src-tauri", "src", "lib.rs");
  const inventory = readJson("switches.json").switches;
  const engineBin = inventory.find((s) => s.name === "RIGHTAPP_ENGINE_BIN");
  assert.equal(engineBin?.compiledOut, "feature:qa-native");
  assert.match(lib, /#\[cfg\(feature = "qa-native"\)\]\s*fn engine_override\(\) -> Option<PathBuf> \{\s*std::env::var_os\("RIGHTAPP_ENGINE_BIN"\)/, "the environment read must sit behind the qa-native cfg");
  assert.match(lib, /#\[cfg\(not\(feature = "qa-native"\)\)\]\s*fn engine_override\(\) -> Option<PathBuf> \{\s*None/, "a build without qa-native has a fixed None");
  const manifest = read("src-tauri", "Cargo.toml");
  assert.match(manifest, /^default = \[\]/m, "qa-native and custom-protocol are never default features");
  assert.match(manifest, /rightkit-qa = \{[^}]*optional = true/, "rightkit-qa is an optional dependency, present only with qa-native");
});

test("the Cargo workspace passes the RightKit Cargo contract: exact published pins and the single dev profile", { skip: spawnSync("cargo", ["--version"]).status !== 0 && "cargo is not installed on this host" }, () => {
  const root = instance();
  const published = new Map(Object.entries({ ...versions.legacyCargo, ...versions.cargo, ...versions.stagedCargo }));
  assert.equal(validateRightKitCargoContract(root, published, "template"), 4, "workspace root + src-tauri + protocol + engine");
  assertCargoProfileContract(root, "template");
});

test("the app has a QA lane, a legal block that passes the release legal contract, and a complete mandatory-set declaration", () => {
  const root = instance();
  const warnings = [];
  const qa = assertQaAdoption({ appRoot: root, repoRoot: root, app: "playright" }, { warn: (line) => warnings.push(line) });
  assert.ok(qa.lane.includes("@rightkit/qa") && qa.lane.includes("rightkit-qa"), `QA lane: ${qa.lane}`);
  assert.deepEqual(warnings, [], "the QA pins are current");
  const { config } = loadReleaseConfigSync(path.join(root, "right-release.config.json"), { registry: loadGeneratorRegistry() });
  for (const platform of ["win", "mac"]) assertLegalReleaseContract(root, config.legal, "playright", platform);
  const report = checkApp(root, {});
  assert.equal(report.profile, "desktop");
  assert.deepEqual(report.problems, []);
  assert.equal(report.violations, 0, report.owners.filter((o) => o.status !== "ok").map((o) => `${o.owner} ${o.status}`).join(", "));
  assert.deepEqual(report.notApplicable.map((n) => n.row), ["M22"], "only M22 is not applicable, with its reason in rightkit-mandatory.json");
  assert.deepEqual(readJson("rightkit-mandatory.json").M22, { role: "absent", entrypoints: [] });
  assert.deepEqual(readJson(".rightkit", "ownership-exceptions.json"), { exceptions: [] }, "a fresh app carries no ownership exception");
});

test("every journey and scenario the app declares exists, and every Rust test is tagged", () => {
  const journeys = read("journeys.toml");
  const entries = [...journeys.matchAll(/^entry = "([^"]+)"/gm)].map((m) => m[1]);
  assert.ok(entries.length >= 2, "the engine and the launch journeys");
  for (const entry of entries) assert.ok(existsSync(path.join(TEMPLATE, entry)), `journeys.toml names ${entry}, which does not exist`);
  assert.match(journeys, /^untagged_baseline = 0$/m);
  for (const file of walk(TEMPLATE, (f) => f.endsWith(".rs"))) {
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((line, i) => {
      if (!/^\s*#\[test\]/.test(line)) return;
      assert.match(lines[i - 1] ?? "", /\/\/ rk-test: (pure|contract)\b/, `${path.relative(TEMPLATE, file)}:${i + 1} is an untagged test`);
    });
  }
});

test("`rightkit-gate check` and RightKit's own preflight pass on the instance with an empty baseline", () => {
  const check = spawnSync(process.execPath, [path.join(here, "bin", "rightkit-gate.mjs"), "check", "--repo", instance(), "--commit", "HEAD"], { cwd: instance(), encoding: "utf8" });
  assert.equal(check.status, 0, `${check.stdout}\n${check.stderr}`);
  assert.match(check.stdout + check.stderr, /passed \d+ rules/);
  assert.doesNotMatch(check.stdout + check.stderr, /KNOWN|STALE/, "a fresh app carries no baseline");
  const result = spawnSync(process.execPath, [path.join(REPO, "scripts", "preflight.mjs"), "--repo", instance()], { encoding: "utf8" });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout + result.stderr, /passed \d+ rules/);
});

test("right-release doctor passes every files and host row on both platforms (only this machine's disk and tools may fail)", () => {
  for (const platform of ["win", "mac"]) {
    const result = spawnSync(process.execPath, [path.join(here, "cli", "right-release.mjs"), "doctor", "--platform", platform], { cwd: instance(), encoding: "utf8" });
    const failed = (result.stdout + result.stderr).split("\n").filter((line) => /^\s*\[FAIL\s*\]/.test(line)).map((line) => line.replace(/^\s*\[FAIL\s*\]\s*/, "").split(":")[0]);
    // disk is a property of the machine running the test, not of the template; every other row is a file or a host contract.
    assert.deepEqual(failed.filter((row) => row !== "disk"), [], `doctor --platform ${platform}:\n${result.stdout}\n${result.stderr}`);
  }
});

// ---- the template files the evaluators do not see ----------------------------------------------------------------------------

test("every RightKit pin is exact and equals the published set; tauri plugin crates equal their npm counterparts", () => {
  const manifest = readJson("package.json");
  const npmPins = {};
  for (const section of ["dependencies", "devDependencies"]) for (const [name, spec] of Object.entries(manifest[section])) if (name.startsWith("@rightkit/")) npmPins[name] = spec;
  assert.ok(Object.keys(npmPins).length >= 15);
  for (const [name, spec] of Object.entries(npmPins)) assert.equal(spec, versions.registry.npm[name], `${name} must be the published version, exactly (no range, link, git or workspace)`);
  const cargoFiles = ["src-tauri/Cargo.toml", "crates/engine/Cargo.toml", "crates/protocol/Cargo.toml"];
  let crates = 0;
  for (const file of cargoFiles) {
    for (const line of read(...file.split("/")).split("\n")) {
      const m = /^(rightkit-[a-z0-9-]+)\s*=\s*(.*)$/.exec(line);
      if (!m) continue;
      crates += 1;
      const pin = /(?:^|version\s*=\s*)"=([^"]+)"/.exec(m[2]);
      assert.ok(pin, `${file}: ${m[1]} must be pinned =x.y.z (no path, git or range): ${m[2]}`);
      assert.equal(pin[1], versions.registry.cargo[m[1]], `${file}: ${m[1]} must be the published version`);
    }
  }
  assert.ok(crates >= 15);
  const tauriManifest = read("src-tauri", "Cargo.toml");
  for (const plugin of ["os", "process", "updater"]) {
    const crate = new RegExp(`^tauri-plugin-${plugin} = "=([^"]+)"`, "m").exec(tauriManifest)?.[1];
    assert.equal(crate, manifest.dependencies[`@tauri-apps/plugin-${plugin}`], `tauri-plugin-${plugin} must equal @tauri-apps/plugin-${plugin}`);
  }
  const workspace = read("pnpm-workspace.yaml");
  for (const [name, spec] of Object.entries(npmPins)) assert.ok(workspace.includes(`'${name}@${spec}'`), `${name}@${spec} is missing from minimumReleaseAgeExclude in pnpm-workspace.yaml (the release-age guard would refuse a fresh publish)`);
});

test("the pre-push hook is committed, executable and what the pinned `rightkit-gate install-hook` writes; `prepare` installs it", () => {
  const hook = read(".githooks", "pre-push").replace(/\r\n/g, "\n");
  assert.equal(readJson("package.json").scripts.prepare, "rightkit-gate install-hook");
  const pinned = readJson("package.json").devDependencies["@rightkit/release"];
  const treeVersion = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8")).version;
  if (pinned === treeVersion) {
    // The pin is this tree's own release: the hook is exactly what `install-hook` writes. `prepare` refuses to overwrite a hook that
    // differs, so a drifted committed hook would fail `pnpm install` in every new app.
    assert.equal(hook, hookText("node_modules/@rightkit/release/bin/rightkit-gate.mjs"), "regenerate with `rightkit-gate install-hook --force`");
  } else {
    // The pin is an earlier release than this tree (the tree's hook may call subcommands the pin lacks): hold the shape every release writes.
    assert.ok(hook.startsWith("#!/bin/sh\n"));
    assert.match(hook, /^# Mandatory local .*\(rightkit-gate install-hook\)\. Never bypass with --no-verify\.$/m);
    assert.match(hook, /cat > "\$stdin"/, "git's ref lines are buffered once and fed to the gate");
    assert.match(hook, /"\$\(git rev-parse --git-dir\)\/hooks\/pre-push"/, "the repository's own .git/hooks/pre-push still runs (core.hooksPath hides it)");
    assert.match(hook, /--base origin\/main/);
    assert.ok(!/(^|[^.\w/-])\/(?:Users|home)\//.test(hook), "no absolute path is written into the hook");
  }
  if (process.platform !== "win32") assert.ok((statSync(path.join(TEMPLATE, ".githooks", "pre-push")).mode & 0o111) !== 0, "the hook file is not executable");
  const tracked = git(REPO, "ls-files", "-s", "desktop-app-template/.githooks/pre-push");
  if (tracked.status === 0 && tracked.stdout.trim()) assert.match(tracked.stdout, /^100755 /, "git must record the hook as executable, or a fresh clone cannot run it");
  const tree = git(REPO, "ls-files", "-s", "desktop-app-template/package.json");
  if (tree.status === 0 && tree.stdout.trim()) assert.match(tree.stdout, /^100644 /, "package.json is a plain file");
});

test("the template is gated for real: no fixture entry, no baseline entry, and its own advisory policy that mirrors RightKit's exceptions", () => {
  const gate = JSON.parse(readFileSync(path.join(REPO, "ci", "gate.config.json"), "utf8"));
  assert.ok(!gate.fixtures.some((dir) => dir === "desktop-app-template" || "desktop-app-template".startsWith(`${dir}/`)), "desktop-app-template must not be a gate fixture: rightkit-gate would skip its scans");
  const baseline = readFileSync(path.join(REPO, "ci", "app-check-baseline.json"), "utf8");
  assert.ok(!baseline.includes("desktop-app-template"), "the template has no baseline: it is clean");
  const ledger = JSON.parse(readFileSync(path.join(REPO, "ci", "advisory-exceptions.json"), "utf8"));
  const known = new Map(ledger.exceptions.map((e) => [e.id, e]));
  const deny = read("deny.toml");
  const audit = read(".cargo", "audit.toml");
  const denyIds = [...deny.matchAll(/\{ id = "(RUSTSEC-\d{4}-\d{4})"/g)].map((m) => m[1]);
  const auditIds = [...audit.matchAll(/"(RUSTSEC-\d{4}-\d{4})"/g)].map((m) => m[1]);
  assert.deepEqual(auditIds, denyIds, "deny.toml [advisories].ignore and .cargo/audit.toml must list the same IDs in the same order");
  for (const id of denyIds) assert.ok(known.has(id), `${id} is ignored in the template but has no entry in ci/advisory-exceptions.json (owner, evidence, reviewBy)`);
  assert.match(deny, /private = \{ ignore = true \}/, "the app's own crates are private and carry no licence field");
  assert.match(deny, /^yanked = "deny"$/m);
  assert.match(deny, /^unknown-registry = "deny"$/m);
  // Same permissive licence list as the RightKit root policy: widening it in one place must widen it in the other.
  const rootAllow = /allow = \[([^\]]+)\]/.exec(readFileSync(path.join(REPO, "deny.toml"), "utf8"))[1].replace(/\s+/g, "");
  assert.equal(/allow = \[([^\]]+)\]/.exec(deny)[1].replace(/\s+/g, ""), rootAllow);
});

test("the committed Cargo.lock holds no version that RightKit's advisory feed marks vulnerable (rustls, rustls-webpki, webpki roots, tokio)", () => {
  const lock = read("Cargo.lock");
  const version = (name) => new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`).exec(lock)?.[1];
  const atLeast = (have, floor) => {
    const a = have.split(".").map(Number); const b = floor.split(".").map(Number);
    for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] > b[i];
    return true;
  };
  // RUSTSEC-2026-0346..0349 (rustls), -0382 (rustls-webpki), -0381 and -0383 (webpki roots), -0386 (tokio): the patched floors.
  for (const [name, floor] of [["rustls", "0.23.46"], ["rustls-webpki", "0.103.16"], ["webpki-roots", "1.0.10"], ["webpki-root-certs", "1.0.10"], ["tokio", "1.53.3"]]) {
    const have = version(name);
    assert.ok(have && atLeast(have, floor), `${name} ${have} is below the patched ${floor}; a stale lock seeds every new app with known advisories`);
  }
});

test("placeholders.json lists exactly the placeholders the template carries, and the declared-only owners are still unwired", () => {
  const placeholders = readJson("placeholders.json");
  for (const entry of placeholders.replace) {
    assert.ok(existsSync(path.join(TEMPLATE, entry.file)), `${entry.file} is listed but gone`);
    if (entry.marker) assert.ok(read(...entry.file.split("/")).includes(entry.marker), `${entry.file} no longer contains ${entry.marker}; delete the entry`);
  }
  const listed = new Set(placeholders.replace.filter((e) => e.marker).map((e) => e.file));
  for (const file of walk(TEMPLATE, (f) => !f.endsWith("placeholders.json") && !f.endsWith(".lock") && !f.endsWith("pnpm-lock.yaml") && !/\.(png|ico|icns)$/.test(f))) {
    if (readFileSync(file, "utf8").includes("PLACEHOLDER")) assert.ok(listed.has(path.relative(TEMPLATE, file).split(path.sep).join("/")), `${path.relative(TEMPLATE, file)} carries a PLACEHOLDER marker that placeholders.json does not list`);
  }
  for (const owner of placeholders.declaredOnly) {
    const manifest = owner.ecosystem === "npm" ? JSON.stringify(readJson(owner.manifest)) : read(...owner.manifest.split("/"));
    assert.ok(manifest.includes(owner.owner), `${owner.owner} is listed declaredOnly but no longer declared in ${owner.manifest}`);
    const needle = owner.ecosystem === "npm" ? owner.owner : owner.owner.replaceAll("-", "_");
    const used = walk(path.join(TEMPLATE, owner.source), (f) => /\.(rs|ts|tsx)$/.test(f)).filter((f) => readFileSync(f, "utf8").includes(needle));
    assert.deepEqual(used.map((f) => path.relative(TEMPLATE, f)), [], `${owner.owner} is wired in source; remove it from placeholders.json declaredOnly, the README table and the Cargo.toml comment`);
    assert.ok(owner.blockedOn, `${owner.owner} needs a reason it is not wired`);
  }
  // Every mandatory-set owner the instance declares and no source file uses must be named here, or the README overstates what is wired.
  const readme = read("README.md");
  for (const owner of placeholders.declaredOnly) assert.ok(readme.includes(owner.owner.replace("@rightkit/", "").replace("rightkit-", "")), `README.md does not mention ${owner.owner}`);
});
