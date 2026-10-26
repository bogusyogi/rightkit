// The desktop app template contract (bogusyogi/rightkit#906 and the template rebuild): the template is what every new app starts from,
// so it must satisfy, from its first commit, the same evaluators RightKit runs on a finished app. This file instantiates the template
// the way a person does (`right-release init` on a copy, committed to a fresh git repository) and runs those evaluators on the
// instance, then checks the template files the evaluators cannot see (pins, the committed hook, advisory policy, placeholders).
// Every assertion names the thing that breaks the day it regresses.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cpSync, existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { hookText } from "./bin/rightkit-gate.mjs";
import { assertNoRightKitCargoOverrides } from "./cargo-contract.mjs";
import { assertCargoProfileContract } from "./cargo-profile-contract.mjs";
import { initTemplate, sortCargoLock } from "./init-command.mjs";
import { assertLegalReleaseContract } from "./legal-contract.mjs";
import { loadGeneratorRegistry } from "./pipeline-generators.mjs";
import { scanApp } from "./pipeline-scan.mjs";
import { assertQaAdoption } from "./qa-contract.mjs";
import { RENAMED_AWAY, RENAMED_AWAY_NPM } from "./renamed-away.mjs";
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
// sha256 of the pre-push hook text each PUBLISHED @rightkit/release writes (computed from the published tarballs; fixtures/published-gate-hooks.json).
const PUBLISHED_HOOKS = JSON.parse(readFileSync(path.join(here, "fixtures", "published-gate-hooks.json"), "utf8"));
const sha256 = (text) => createHash("sha256").update(text).digest("hex");
/**
 * Why `hook` is not what the published @rightkit/release `pinned` writes into an app, or null when it is. `rightkit-gate install-hook`
 * (the template's `prepare` script) refuses to overwrite a hook that differs, so a committed hook from another release fails
 * `pnpm install` in every app copied from the template. The comparison is against the recorded digest of the PUBLISHED hook, not against
 * this tree's hookText: the tree is the next release, whose hook may differ.
 */
function hookDrift(hook, pinned) {
  const recorded = PUBLISHED_HOOKS.releases[pinned];
  if (!recorded) return `fixtures/published-gate-hooks.json has no hook digest for the pinned @rightkit/release ${pinned}: record it from the published tarball (see that file's "about")`;
  const actual = sha256(hook.replace(/\r\n/g, "\n"));
  if (actual === recorded) return null;
  return `the committed hook (sha256 ${actual}) is not the hook the published @rightkit/release ${pinned} writes (sha256 ${recorded}), and its \`rightkit-gate install-hook\` refuses to overwrite a differing hook, so \`prepare\` fails in every app copied from the template; regenerate it from the pinned release's hookText ("${PUBLISHED_HOOKS.nodePath}"), mode 100755`;
}

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

test("the Cargo workspace keeps the RightKit Cargo contract: no override or source replacement, and the single dev profile", () => {
  // `validateRightKitCargoContract` also asks `cargo metadata`, which no test may launch (the broker guard); the pins it would compare are
  // checked from the manifests in the pin test below, and the override and profile contracts are pure file scans.
  const root = instance();
  for (const manifest of ["Cargo.toml", "src-tauri/Cargo.toml", "crates/engine/Cargo.toml", "crates/protocol/Cargo.toml"]) {
    assertNoRightKitCargoOverrides(path.join(root, ...manifest.split("/")), root, `template/${manifest}`);
  }
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
  // The template itself, not only an instance, satisfies the mandatory set (what `right-release ownership-mandatory <dir> --enforce` checks).
  assert.equal(checkApp(TEMPLATE, {}).violations, 0);
  const report = checkApp(root, {});
  assert.equal(report.profile, "desktop");
  assert.deepEqual(report.problems, []);
  assert.equal(report.violations, 0, report.owners.filter((o) => o.status !== "ok").map((o) => `${o.owner} ${o.status}`).join(", "));
  assert.deepEqual(report.notApplicable.map((n) => n.row), ["M22"], "only M22 is not applicable, with its reason in rightkit-mandatory.json");
  assert.deepEqual(readJson("rightkit-mandatory.json").M22, { role: "absent", entrypoints: [] });
  assert.deepEqual(readJson(".rightkit", "ownership-exceptions.json"), { exceptions: [] }, "a fresh app carries no ownership exception");
});

test("every byte-sensitive file is LF in every checkout: the legal documents, the manifest, the locks and the hook", () => {
  // The release legal contract hashes the documents and the manifest raw, --locked and --frozen-lockfile compare the locks, and the hook
  // runs under sh. A Windows checkout with core.autocrlf would otherwise turn all of them into CRLF.
  const root = instance();
  const manifest = JSON.parse(readFileSync(path.join(root, "legal", "legal-manifest.json"), "utf8"));
  const sensitive = [
    ".githooks/pre-push", "Cargo.lock", "pnpm-lock.yaml", "legal/legal-manifest.json", "src/generated/legal-contract.json", "legal-snapshot.config.json",
    ...[...(manifest.documents ?? []), ...(manifest.appendices ?? [])].map((entry) => `legal/${entry.path}`),
  ];
  assert.ok(sensitive.length >= 10);
  const attrs = git(root, "check-attr", "eol", "text", "--", ...sensitive);
  assert.equal(attrs.status, 0, attrs.stderr);
  const lines = attrs.stdout.trim().split("\n");
  assert.equal(lines.length, sensitive.length * 2);
  for (const line of lines) assert.match(line, /: eol: lf$|: text: auto$/, `a CRLF-capable checkout of ${line.split(":")[0]}: the template's .gitattributes must pin eol=lf`);
  for (const file of sensitive) assert.ok(!readFileSync(path.join(root, ...file.split("/")), "utf8").includes("\r"), `${file} is committed with CR`);
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

// The template's notApplicable.M22 is not a ratified ruling (rightkit#994 is undecided), so since rightkit#1140's check M22 applies
// and these are its only two findings. Ratifying the row (or adopting rightkit-agent-tools) makes the instance clean again.
const TEMPLATE_M22 = ["mandatory-foundation (OW-05): notApplicable.M22", "mandatory-foundation (OW-05): profile \"desktop\" requires the foundation owner rightkit-agent-tools"];
const failLines = (text) => text.split("\n").filter((line) => line.startsWith("FAIL "));
test("`rightkit-gate check` and RightKit's own preflight pass on the instance with an empty baseline, but for the unratified M22 row", () => {
  const check = spawnSync(process.execPath, [path.join(here, "bin", "rightkit-gate.mjs"), "check", "--repo", instance(), "--commit", "HEAD"], { cwd: instance(), encoding: "utf8" });
  assert.equal(check.status, 1, `${check.stdout}\n${check.stderr}`);
  assert.deepEqual(failLines(check.stdout).map((line, i) => line.startsWith(`FAIL ${TEMPLATE_M22[i]}`)), [true, true], check.stdout);
  assert.doesNotMatch(check.stdout + check.stderr, /KNOWN|STALE/, "a fresh app carries no baseline");
  const result = spawnSync(process.execPath, [path.join(REPO, "scripts", "preflight.mjs"), "--repo", instance()], { encoding: "utf8" });
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.deepEqual(failLines(result.stdout).map((line, i) => line.startsWith(`FAIL ${TEMPLATE_M22[i]}`)), [true, true], result.stdout);
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

test("THIRD_PARTY_NOTICES.md states the hashes of the locks and the npm appendix the template commits", () => {
  // legal-snapshot renders these hashes into the notice and nothing else compares them, so a lock that moves without a re-run leaves a
  // notice that describes a build nobody can reproduce. Re-run in desktop-app-template: `pnpm exec legal-snapshot --source-root ../legal`.
  const sha = (file) => createHash("sha256").update(readFileSync(path.join(TEMPLATE, ...file.split("/")))).digest("hex");
  const inputs = /^inputs: (.+)\.$/m.exec(read("legal", "THIRD_PARTY_NOTICES.md"))?.[1];
  assert.ok(inputs, "the notice carries its inputs line");
  const stated = Object.fromEntries(inputs.split("; ").map((part) => { const m = /^(\S+) sha256 ([0-9a-f]{64})$/.exec(part); return [m?.[1], m?.[2]]; }));
  assert.deepEqual(Object.keys(stated).sort(), ["Cargo.lock", "legal/THIRD_PARTY_NPM_LICENSES.html", "pnpm-lock.yaml"]);
  for (const [file, digest] of Object.entries(stated)) assert.equal(digest, sha(file), `THIRD_PARTY_NOTICES.md states a stale sha256 for ${file}: run \`pnpm exec legal-snapshot --source-root ../legal\` in desktop-app-template`);
});

/** The template's manifests name one app: the checks the retired `rightapp-tools check` made that no RightKit evaluator makes. */
function manifestDisagreements(root) {
  const text = (...parts) => readFileSync(path.join(root, ...parts), "utf8");
  const json = (...parts) => JSON.parse(text(...parts));
  const field = (toml, key) => new RegExp(`^${key}\\s*=\\s*"([^"]+)"`, "m").exec(toml)?.[1];
  const binName = (toml) => /\[\[bin\]\]\s*\nname\s*=\s*"([^"]+)"/.exec(toml)?.[1];
  const pkg = json("package.json");
  const tauri = json("src-tauri", "tauri.conf.json");
  const config = json("right-release.config.json");
  const shell = text("src-tauri", "Cargo.toml");
  const engine = text("crates", "engine", "Cargo.toml");
  const protocol = text("crates", "protocol", "Cargo.toml");
  const lib = text("src-tauri", "src", "lib.rs");
  const problems = [];
  const need = (ok, message) => { if (!ok) problems.push(message); };
  // identity: one version and one name across package.json, tauri.conf.json and the three crates
  need(tauri.version === pkg.version, `tauri.conf.json version ${tauri.version} != package.json ${pkg.version}`);
  for (const [label, toml] of [["src-tauri", shell], ["engine", engine], ["protocol", protocol]]) need(field(toml, "version") === pkg.version, `${label}/Cargo.toml version != package.json ${pkg.version}`);
  need(field(shell, "name") === pkg.name, "src-tauri package name != package.json name");
  need(config.app === pkg.name, "right-release.config.json app != package.json name");
  need(tauri.mainBinaryName === binName(shell), "tauri.conf.json mainBinaryName != the src-tauri [[bin]] name");
  need(tauri.productName.toLowerCase() === pkg.name, "tauri.conf.json productName lowercased != package.json name");
  need(tauri.identifier.endsWith(`.${pkg.name}`), "tauri.conf.json identifier must end with .<package name>");
  // sidecar: externalBin, the engine [[bin]], the staging declaration and the shell that spawns it name one binary
  const engineBin = binName(engine);
  need(JSON.stringify(tauri.bundle.externalBin) === JSON.stringify([`binaries/${engineBin}`]), `bundle.externalBin must be [binaries/${engineBin}]`);
  need(lib.includes(`"${engineBin}"`), "src-tauri/src/lib.rs does not name the engine binary the supervisor spawns");
  need(JSON.stringify(config.sidecars?.sidecars?.map((s) => [s.kind, s.package, s.manifestPath])) === JSON.stringify([["cargo-bin", field(engine, "name"), "crates/engine/Cargo.toml"]]), "right-release.config.json sidecars must stage the engine package from crates/engine/Cargo.toml");
  // dev server: tauri devUrl, vite and the dev script agree on one port; the frontend dist is vite's output
  const port = /:(\d+)\/?$/.exec(tauri.build.devUrl)?.[1];
  need(Boolean(port) && text("vite.config.ts").includes(`port: ${port}`), "vite port != tauri.conf.json devUrl port");
  need(Boolean(port) && pkg.scripts.dev.includes(`--port ${port}`), "package.json dev script port != tauri.conf.json devUrl port");
  need(tauri.build.frontendDist === "../dist" && text("vite.config.ts").includes('outDir: "dist"'), "tauri.conf.json frontendDist != vite outDir");
  // capabilities: every Tauri plugin the shell registers has its permission
  const permissions = json("src-tauri", "capabilities", "default.json").permissions.join(" ");
  for (const [plugin, permission] of [["tauri_plugin_updater", "updater:"], ["tauri_plugin_process", "process:"], ["tauri_plugin_os", "os:"]]) need(!lib.includes(plugin) || permissions.includes(permission), `capabilities/default.json has no ${permission}* permission for ${plugin}`);
  // imports: every @rightkit package the renderer imports is declared
  const declared = { ...pkg.dependencies, ...pkg.devDependencies };
  for (const file of walk(path.join(root, "src"), (f) => /\.(ts|tsx)$/.test(f))) {
    for (const m of readFileSync(file, "utf8").matchAll(/(?:from|import)\s+["'](@rightkit\/[a-z0-9-]+)[^"']*["']/g)) need(m[1] in declared, `${path.relative(root, file)} imports ${m[1]}, which package.json does not declare`);
  }
  // one harness: rightkit-qa journeys. No Node UI harness dependency, no Node file under tests/
  for (const harness of ["puppeteer", "puppeteer-core", "playwright", "@playwright/test", "selenium-webdriver", "webdriverio", "@wdio/cli"]) need(!(harness in declared), `${harness} is a Node test harness; drive the app with rightkit-qa journeys`);
  const nodeTests = existsSync(path.join(root, "tests")) ? walk(path.join(root, "tests"), (f) => /\.(mjs|js|cjs|ts)$/.test(f)) : [];
  need(nodeTests.length === 0, `Node files under tests/: ${nodeTests.map((f) => path.relative(root, f)).join(", ")}`);
  return problems;
}

test("the template's manifests agree on the app's identity, sidecar, dev port, plugin permissions and imports, before and after `init`", () => {
  assert.deepEqual(manifestDisagreements(TEMPLATE), []);
  assert.deepEqual(manifestDisagreements(instance()), [], "init left the renamed instance with manifests that disagree");
});

test("the pre-push hook is committed, executable and what the pinned `rightkit-gate install-hook` writes; `prepare` installs it", () => {
  const hook = read(".githooks", "pre-push").replace(/\r\n/g, "\n");
  assert.equal(readJson("package.json").scripts.prepare, "rightkit-gate install-hook");
  const pinned = readJson("package.json").devDependencies["@rightkit/release"];
  const treeVersion = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8")).version;
  // Whatever the pin is, the committed hook is byte-identical to what that PUBLISHED release writes (its recorded digest).
  assert.equal(hookDrift(hook, pinned), null);
  if (pinned === treeVersion) {
    // The pin is this tree's own release: the hook is exactly what `install-hook` writes. `prepare` refuses to overwrite a hook that
    // differs, so a drifted committed hook would fail `pnpm install` in every new app.
    assert.equal(hook, hookText("node_modules/@rightkit/release/bin/rightkit-gate.mjs"), "regenerate with `rightkit-gate install-hook --force`");
  } else {
    // The pin is an earlier release than this tree (the tree's hook may call subcommands the pin lacks): the digest above is the exact
    // check; these hold the shape every release writes and name a wildly wrong hook readably.
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

test("the committed hook is compared with the hook the pinned release publishes: a hook from another release is caught", () => {
  const hook = read(".githooks", "pre-push");
  const pinned = readJson("package.json").devDependencies["@rightkit/release"];
  assert.equal(hookDrift(hook, pinned), null);
  assert.equal(hookDrift(hook.replace(/\n/g, "\r\n"), pinned), null, "a CRLF checkout of the same hook is the same hook");
  // The 0.6.4 hook said "gate" and ran no `check --commit HEAD`; 0.6.5 writes both: any edit of the published bytes is drift.
  assert.match(hookDrift(hook.replace("local check and gate", "local gate"), pinned) ?? "", /is not the hook the published @rightkit\/release/);
  assert.match(hookDrift(hook.replace("rightkit_gate check --commit HEAD < /dev/null >&2 || exit $?", "true"), pinned) ?? "", /refuses to overwrite a differing hook/);
  assert.match(hookDrift(hook, "0.0.1") ?? "", /has no hook digest for the pinned @rightkit\/release 0\.0\.1/, "an unrecorded pin cannot be checked, so it fails");
});

test("fixtures/published-gate-hooks.json records every @rightkit/release the registry has served from 0.6.4, and the tree's hook equals the record of the version it is staged as", () => {
  assert.equal(PUBLISHED_HOOKS.nodePath, "node_modules/@rightkit/release/bin/rightkit-gate.mjs", "the digests are for the hook an installed app gets");
  const order = (a, b) => { const [x, y] = [a, b].map((v) => v.split(".").map(Number)); return x[0] - y[0] || x[1] - y[1] || x[2] - y[2]; };
  const treeVersion = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8")).version;
  for (const [version, digest] of Object.entries(PUBLISHED_HOOKS.releases)) {
    assert.match(version, /^\d+\.\d+\.\d+$/);
    assert.match(digest, /^[0-9a-f]{64}$/, `${version}: a sha256 hex digest`);
    assert.ok(order(version, treeVersion) <= 0, `${version} is newer than this tree's ${treeVersion}: only published releases are recorded`);
  }
  // Every release a registry has served from 0.6.4 on (the published one and the superseded ones) has a record: the bookkeeping that
  // moves registry.npm to a new release adds its digest in the same commit, or this fails.
  const served = new Set([...(versions.legacyNpm["@rightkit/release"] ?? []), versions.registry.npm["@rightkit/release"]].filter((v) => order(v, "0.6.4") >= 0));
  assert.ok(served.size >= 2 && served.has("0.6.4"), "0.6.4 and 0.6.5 are the first served releases with a recorded hook");
  for (const version of served) assert.ok(PUBLISHED_HOOKS.releases[version], `@rightkit/release ${version} is on the registry: add its hook digest to fixtures/published-gate-hooks.json, computed from the published tarball`);
  // The record of this tree's own version, once it exists, is the hook this tree writes: a published release is immutable, so changing
  // hookText after publishing means a new version, not a new digest.
  const own = PUBLISHED_HOOKS.releases[treeVersion];
  if (own !== undefined) assert.equal(own, sha256(hookText(PUBLISHED_HOOKS.nodePath)), `hookText changed after @rightkit/release ${treeVersion} was recorded as published: bump the version`);
});

test("the README warns that `prepare` acts on the enclosing repository, because the template lives inside RightKit's", () => {
  // `rightkit-gate install-hook` resolves the git toplevel. Inside the RightKit repository that is RightKit's own `.githooks`, which differs
  // from the app hook: `pnpm install` fails in `prepare`, and `--force` would replace RightKit's pre-push hook.
  const readme = read("README.md");
  assert.match(readme, /pnpm install --ignore-scripts/);
  assert.match(readme, /never run\s+`rightkit-gate install-hook --force` from this directory/);
});

test("the README names the release the template waits for exactly while the pin is older than this tree's", () => {
  // The pin is the latest published release. 0.6.5 has `check` but loads only right-release.config.mjs (it was published without the
  // loader); the JSON config (M24) is loaded by every command from this tree's version. The README says so while the pin lags, and must
  // stop saying it the day the pin catches up.
  const pinned = readJson("package.json").devDependencies["@rightkit/release"];
  const treeVersion = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8")).version;
  const stated = read("README.md").includes("**Pin sequencing.**");
  assert.equal(stated, pinned !== treeVersion, pinned === treeVersion
    ? "the pin is this tree's release: delete the 'Pin sequencing' paragraph from the template README"
    : `the template pins @rightkit/release ${pinned}, older than this tree's ${treeVersion}: the README must say which release its commands need`);
  if (stated) {
    const paragraph = /\*\*Pin sequencing\.\*\*[\s\S]*?\n\n/.exec(read("README.md"))?.[0] ?? "";
    assert.ok(paragraph.includes(pinned), `the Pin sequencing paragraph must name the pin ${pinned}`);
    assert.ok(paragraph.includes(treeVersion), `the Pin sequencing paragraph must name the release its commands wait for, ${treeVersion}`);
  }
});

// 0.6.5 was published (2026-10-25, publish run 40145700832) without the JSON release config loader; 0.6.6 is the first release that has it.
// Every place that says which release loads it names that one, and the changelog files the lane under it, never under a published heading.
const JSON_CONFIG_FIRST_RELEASE = "0.6.6";
test("the JSON release config loader is stated as shipping in the first release that has it, not in the published 0.6.5", () => {
  const treeVersion = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8")).version;
  const cmp = (a, b) => { const [x, y] = [a, b].map((v) => v.split(".").map(Number)); return x[0] - y[0] || x[1] - y[1] || x[2] - y[2]; };
  assert.ok(cmp(treeVersion, JSON_CONFIG_FIRST_RELEASE) >= 0, `this tree (${treeVersion}) is behind the release that carries the loader`);
  const sections = readFileSync(path.join(here, "CHANGELOG.md"), "utf8").split(/^## /m).slice(1);
  const bullet = "**Every command loads `right-release.config.json` (M24)";
  const holding = sections.filter((section) => section.includes(bullet)).map((section) => section.split("\n", 1)[0]);
  assert.equal(holding.length, 1, `the loader is changelogged once, found under: ${holding.join(" | ")}`);
  assert.ok(holding[0].startsWith(`${JSON_CONFIG_FIRST_RELEASE} `), `the loader bullet sits under "${holding[0]}", not under ${JSON_CONFIG_FIRST_RELEASE}`);
  const published = sections.filter((section) => /^\d+\.\d+\.\d+ \(\d{4}-\d{2}-\d{2}/.test(section));
  for (const section of published) assert.ok(!section.includes("right-release.config.json"), `the published "${section.split("\n", 1)[0]}" section claims the JSON config loader`);
  // The template README and the pipeline document: whichever release they name as loading the JSON config is the first one.
  const claims = [
    ["desktop-app-template/README.md", read("README.md")],
    ["docs/PIPELINE-ARCHITECTURE.md", readFileSync(path.join(REPO, "docs", "PIPELINE-ARCHITECTURE.md"), "utf8")],
  ].flatMap(([file, body]) => [...body.matchAll(/loaded by every command from (\d+\.\d+\.\d+)|from (\d+\.\d+\.\d+) every command loads a JSON config/g)].map((m) => [file, m[1] ?? m[2]]));
  assert.ok(claims.length >= 2, "the README and the pipeline document each say which release loads the JSON config");
  for (const [file, version] of claims) assert.equal(version, JSON_CONFIG_FIRST_RELEASE, `${file} says the JSON config loads from ${version}`);
});

test("the template is gated for real: no fixture entry, no baseline entry, and its own advisory policy that mirrors RightKit's exceptions", () => {
  const gate = JSON.parse(readFileSync(path.join(REPO, "ci", "gate.config.json"), "utf8"));
  assert.ok(!gate.fixtures.some((dir) => dir === "desktop-app-template" || "desktop-app-template".startsWith(`${dir}/`)), "desktop-app-template must not be a gate fixture: rightkit-gate would skip its scans");
  const baseline = JSON.parse(readFileSync(path.join(REPO, "ci", "app-check-baseline.json"), "utf8"));
  const templateEntries = Object.values(baseline.known).flat().filter((id) => id.includes("desktop-app-template"));
  assert.deepEqual(templateEntries, ["mandatory-foundation|desktop-app-template/rightkit-mandatory.json|notApplicable:M22", "mandatory-foundation|desktop-app-template/rightkit-mandatory.json|rightkit-agent-tools"], "the template's only baseline entries are its unratified M22 row (rightkit#994)");
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

test("no retired crate or npm name appears anywhere in the template (the owner renames of 2026-10-14)", () => {
  const retired = [...RENAMED_AWAY.keys(), ...RENAMED_AWAY_NPM.keys()];
  const hits = [];
  for (const file of walk(TEMPLATE, (f) => !/\.(png|ico|icns|html)$/.test(f) || f.endsWith("index.html"))) {
    const text = readFileSync(file, "utf8");
    for (const name of retired) {
      // `rightkit-shell` is a retired crate; `rightkit-tauri-shell` is its successor and must not match. Underscore spellings are the crate idents.
      const spellings = [name, name.replaceAll("-", "_")];
      for (const spelling of spellings) {
        const escaped = spelling.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
        if (new RegExp(`(?<![\\w@/-])${escaped}(?![\\w-])`).test(text)) hits.push(`${path.relative(TEMPLATE, file)}: ${spelling}`);
      }
    }
  }
  assert.deepEqual(hits, []);
});
