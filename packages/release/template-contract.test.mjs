// The desktop app template contract (bogusyogi/rightkit#906 and the template rebuild): the template is what every new app starts from,
// so it must satisfy, from its first commit, the same evaluators RightKit runs on a finished app. This file instantiates the template
// the way a person does (`right-release init` on a copy, committed to a fresh git repository) and runs those evaluators on the
// instance, then checks the template files the evaluators cannot see (pins, the committed hook, advisory policy, placeholders).
// Every assertion names the thing that breaks the day it regresses.
// Loaded first so the spawned doctor inherits the test broker guard (cargo metadata is stubbed, never sent to this host's real RustRight broker),
// whether this file runs alone or in the package suite (which preloads it).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";
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
import { loadTrustedExternal } from "./ownership-approval.mjs";
import { enrollScanTool } from "./scan-tool-enrolment.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { managedEnv } from "./test-support/fixture.mjs";
import { collectPreflight, preflightFailures } from "./preflight.mjs";
import { resolveNativeCargoLayout } from "./native-cargo-layout.mjs";
import { resolveProjectType } from "./project-type.mjs";

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
  const listed = git(TEMPLATE, "ls-files", "-z");
  assert.equal(listed.status, 0, listed.stderr);
  const tracked = new Set(listed.stdout.split("\0").filter(Boolean));
  cpSync(TEMPLATE, dest, { recursive: true, filter: (src) => {
    const relative = path.relative(TEMPLATE, src).split(path.sep).join("/");
    if (!relative) return true;
    if (/(^|\/)(node_modules|target|dist|\.right-release)(\/|$)/.test(relative)) return false;
    if (tracked.has(relative)) return true;
    return statSync(src).isDirectory() && [...tracked].some((file) => file.startsWith(`${relative}/`));
  } });
  initTemplate(dest, { name: "PlayRight", id: "app.orthiclabs.playright" });
  assert.equal(git(dest, "init", "-q", "-b", "main").status, 0);
  assert.equal(git(dest, "add", "-A").status, 0);
  assert.equal(git(dest, "commit", "-q", "-m", "instance").status, 0);
  assert.equal(git(dest, "remote", "add", "origin", "https://github.com/example/template-contract-fixture.git").status, 0);
  instanceRoot = dest;
  return dest;
}

// ---- the instance passes the evaluators RightKit runs on an app ---------------------------------------------------------------

test("init renames the whole template identity and leaves a Cargo.lock that --locked accepts", () => {
  const root = instance();
  assert.equal(existsSync(path.join(root, "src-tauri", "gen", "schemas", "capabilities.json")), false,
    "the fixture copies tracked template source, not generated Tauri schema output from the live checkout");
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
  assert.deepEqual(report.notApplicable.map((n) => n.row), [], "no row is not applicable: M22 is mandatory (Adrian, 2026-10-29)");
  assert.deepEqual(readJson("rightkit-mandatory.json").M22, { role: "host", entrypoints: ["crates/engine/src/main.rs#contract", "crates/engine/src/main.rs#main"] });
  assert.deepEqual(readJson(".rightkit", "ownership-exceptions.json"), { exceptions: [] }, "a fresh app carries no ownership exception");
});

test("every byte-sensitive file is LF in every checkout: the legal documents, the manifest and the locks", () => {
  // The release legal contract hashes the documents and the manifest raw, and --locked and --frozen-lockfile compare the locks. A Windows
  // checkout with core.autocrlf would otherwise turn all of them into CRLF.
  const root = instance();
  const manifest = JSON.parse(readFileSync(path.join(root, "legal", "legal-manifest.json"), "utf8"));
  const sensitive = [
    "Cargo.lock", "pnpm-lock.yaml", "legal/legal-manifest.json", "src/generated/legal-contract.json", "legal-snapshot.config.json",
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

// M22 is mandatory (Adrian, 2026-10-29: "Cli for every app, yes"): the template adopts rightkit-agent-tools as a host, so an
// instance is born with no finding at all.
const failLines = (text) => text.split("\n").filter((line) => line.startsWith("FAIL "));
// On a broker-managed host the class A cargo scans of the instance go through the live broker, which tests never reach
// (test-support/broker-guard.mjs); CI (direct executor) runs this one.
// Class A runs cargo-deny and cargo-audit only from an ENROLLED executable (an absolute path, its sha256 and its version), never from PATH (scan-tool-enrolment.mjs, RA-18). A direct host
// enrols the tools it installed through the real enrolment operation, into the managed data root its gate reads; this test does the same, into a data root of its own, for the cargo-deny and
// cargo-audit this host has (CI installs the pinned ones with taiki-e/install-action). A host with neither fails here, loudly: the real check of the instance cannot run without them.
function enrolHostCargoScanTools(dataRoot) {
  for (const tool of ["cargo-deny", "cargo-audit"]) {
    const found = spawnSync(process.platform === "win32" ? "where" : "which", [tool], { encoding: "utf8" });
    const file = found.status === 0 ? String(found.stdout).split(/\r?\n/).map((l) => l.trim()).find((l) => path.isAbsolute(l)) : undefined;
    assert.ok(file, `${tool} is not installed on this host, so the real check of the instance cannot run: the class A cargo scans run only an enrolled ${tool}`);
    const reported = spawnSync(file, ["--version"], { encoding: "utf8" });
    const version = /\d+\.\d+\.\d+/.exec(String(reported.stdout))?.[0];
    assert.ok(version, `${file} --version reports no version: ${reported.stdout}${reported.stderr}`);
    enrollScanTool({ dataRoot, platform: process.platform, tool, path: file, sha256: createHash("sha256").update(readFileSync(file)).digest("hex"), version });
  }
}
test("`rightkit-gate check` and RightKit's own preflight pass on the instance with an empty baseline and no finding", { skip: brokerManagedHost(process.env) && "the class A cargo scans need the live broker on this managed host; CI runs it" }, (t) => {
  const dataRoot = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-template-scan-tools-")));
  t.after(() => rmSync(dataRoot, { recursive: true, force: true }));
  enrolHostCargoScanTools(dataRoot);
  // The class A cargo scans run the real cargo (the direct executor). A child inherits the broker guard through NODE_OPTIONS and the guard refuses a build tool outside hosted CI, so this run, whose
  // point is the real tools on a direct host, clears it (as item7-linux.test.mjs does for its real doors).
  const check = spawnSync(process.execPath, [path.join(here, "bin", "rightkit-gate.mjs"), "check", "--repo", instance(), "--commit", "HEAD"], { cwd: instance(), encoding: "utf8", env: { ...process.env, NODE_OPTIONS: "", RIGHTKIT_DATA_ROOT: dataRoot, RUSTRIGHT_DATA_ROOT: dataRoot } });
  assert.equal(check.status, 0, `${check.stdout}\n${check.stderr}`);
  assert.deepEqual(failLines(check.stdout), [], check.stdout);
  assert.doesNotMatch(check.stdout + check.stderr, /KNOWN|STALE/, "a fresh app carries no baseline");
  const result = spawnSync(process.execPath, [path.join(REPO, "scripts", "preflight.mjs"), "--repo", instance()], { encoding: "utf8" });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.deepEqual(failLines(result.stdout), [], result.stdout);
});

test("template preflight checks required host rows with fixture ownership and host probes", () => {
  const root = instance();
  const configPath = path.join(root, "right-release.config.json");
  const { config } = loadReleaseConfigSync(configPath, { registry: loadGeneratorRegistry() });
  const nativeLayout = resolveNativeCargoLayout({ appRoot: root, config });
  for (const platform of ["win", "mac"]) {
    const env = managedEnv(configPath, { platform });
    const checks = collectPreflight({
      platform, appRoot: root, repoRoot: root, app: config.app, version: config.version, configPath, nativeLayout,
      projectType: resolveProjectType({ appRoot: root, config, target: config.targets[platform] }),
      env, isBrokerManagedHost: () => true, versionProbe: () => "fixture tool", freeDisk: () => 500,
      findSdkTool: () => ({ version: "fixture SDK" }), runtimeBudgetAdoption: () => null,
      ownershipEvidenceFactory: () => ({
        ok: true, enrollmentPending: false,
        result: { problems: [], violations: [], covered: [], coverage: [] },
        floor: { ok: true, problems: [] },
        evidence: { source: "template-preflight-fixture", trustEnrolled: true },
      }),
    });
    const byName = new Map(checks.map((check) => [check.name, check]));
    for (const name of ["target-bridge", "version", "compiler-cache", "disk", "ownership"]) {
      assert.equal(byName.get(name)?.status, "ok", `${platform} ${name}: ${byName.get(name)?.detail}`);
    }
    if (platform === "win") assert.equal(byName.get("signtool")?.status, "ok", byName.get("signtool")?.detail);
    assert.deepEqual(preflightFailures(checks).map((check) => check.name), [], `${platform}: ${checks.map((check) => `${check.name}=${check.status}: ${check.detail}`).join("; ")}`);
  }
});

test("real doctor reports template rows and refuses the unregistered identity on both platforms", () => {
  const root = instance();
  const config = path.join(root, "right-release.config.json");
  // The protected ownership trust is host state, read here as the product reads it. A host that holds a protected registry refuses a fresh identity (a FAIL row); a host that
  // holds none (a hosted runner, where none can be enrolled) reports enrollment as a warning only (ownership-evidence.mjs: pending without trust is advisory, #262), so the
  // doctor passes there and the row is a WARN.
  const hostTrust = loadTrustedExternal();
  const registryHost = hostTrust.protection?.ok === true && Boolean(hostTrust.trust);
  for (const platform of ["win", "mac"]) {
    const result = spawnSync(process.execPath, [path.join(here, "cli", "right-release.mjs"), "doctor", "--platform", platform], {
      cwd: root, encoding: "utf8", env: managedEnv(config, {
        platform,
        env: { RK_TEST_LATEST: "", RK_TEST_UNPUBLISHED: "", RK_TEST_YANKED: "" },
      }),
    });
    const output = `${result.stdout}\n${result.stderr}`;
    // Disk depends on this machine; ownership is expected for this unregistered identity on a registry host. Every other preflight FAIL remains visible.
    const failedRows = output.split("\n").filter((line) => /^\s*\[FAIL\]\s/.test(line))
      .map((line) => line.replace(/^\s*\[FAIL\]\s*/, "").split(":")[0]);
    assert.equal(result.status, registryHost || failedRows.length ? 1 : 0, `${platform}:\n${output}`);
    assert.match(output, new RegExp(`platform: ${platform}\\b`), output);
    for (const row of ["target-bridge", "version"]) {
      assert.match(output, new RegExp(`\\[ok\\s*\\] ${row}:`), `${platform} ${row}:\n${output}`);
    }
    if (registryHost) assert.match(output, /\[FAIL\] ownership:/,
      `${platform}: the real doctor must require protected ownership registration for a fresh identity\n${output}`);
    else assert.match(output, /\[WARN\] ownership:.*enrollment is pending/,
      `${platform}: with no protected trust registry on this host, ownership enrollment is a warning, never a failure\n${output}`);
    assert.deepEqual(failedRows.filter((row) => row !== "ownership" && row !== "disk"), [], `${platform}:\n${output}`);
    const totals = [...output.matchAll(/right-release: preflight found (\d+) blocking problem\(s\); the build would fail on these/g)];
    assert.equal(totals.length, registryHost || failedRows.length ? 1 : 0, `${platform}: expected ${registryHost || failedRows.length ? "one doctor summary" : "no blocking summary"}\n${output}`);
    if (totals.length) assert.equal(Number(totals[0][1]), failedRows.length, `${platform}: unexpected config or admission failures\n${output}`);
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

test("the template commits no git hook: `prepare` runs `rightkit-gate install-hook`, which retires hooks and writes none", () => {
  assert.equal(readJson("package.json").scripts.prepare, "rightkit-gate install-hook");
  for (const dir of [".githooks", ".husky", "githooks"]) assert.equal(existsSync(path.join(TEMPLATE, dir)), false, `${dir} would carry a git hook the template must not ship`);
  const tracked = git(REPO, "ls-files", "desktop-app-template");
  if (tracked.status === 0 && tracked.stdout.trim()) assert.doesNotMatch(tracked.stdout, /(^|\/)(?:\.githooks|\.husky|githooks)\//m, "no tracked hook file");
  const tree = git(REPO, "ls-files", "-s", "desktop-app-template/package.json");
  if (tree.status === 0 && tree.stdout.trim()) assert.match(tree.stdout, /^100644 /, "package.json is a plain file");
});

test("the README warns that `prepare` acts on the enclosing repository, because the template lives inside RightKit's", () => {
  // `rightkit-gate install-hook` resolves the git toplevel. Inside the RightKit repository that is RightKit itself: it would register the agent
  // hook in RightKit's .claude/settings.json and edit RightKit's local hook configuration.
  const readme = read("README.md");
  assert.match(readme, /pnpm install --ignore-scripts/);
  assert.match(readme, /never run\s+`rightkit-gate install-hook` from this directory/);
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
  assert.deepEqual(templateEntries, [], "the template carries no baseline entry");
  // rightkit#1285: RightKit's exceptions live in each crate workspace's own ledger (crates/<x>/.rightkit/advisory-exceptions.json); the template's ignores must be reviewed in one of them.
  const known = new Map();
  for (const crate of readdirSync(path.join(REPO, "crates"))) {
    const file = path.join(REPO, "crates", crate, ".rightkit", "advisory-exceptions.json");
    if (existsSync(file)) for (const e of JSON.parse(readFileSync(file, "utf8")).exceptions ?? []) known.set(e.id, e);
  }
  const deny = read("deny.toml");
  const audit = read(".cargo", "audit.toml");
  const denyIds = [...deny.matchAll(/\{ id = "(RUSTSEC-\d{4}-\d{4})"/g)].map((m) => m[1]);
  const auditIds = [...audit.matchAll(/"(RUSTSEC-\d{4}-\d{4})"/g)].map((m) => m[1]);
  assert.deepEqual(auditIds, denyIds, "deny.toml [advisories].ignore and .cargo/audit.toml must list the same IDs in the same order");
  for (const id of denyIds) assert.ok(known.has(id), `${id} is ignored in the template but has no entry in any crates/<x>/.rightkit/advisory-exceptions.json (owner, evidence, reviewBy)`);
  assert.match(deny, /private = \{ ignore = true \}/, "the app's own crates are private and carry no licence field");
  assert.match(deny, /^yanked = "deny"$/m);
  assert.match(deny, /^unknown-registry = "deny"$/m);
  // Same permissive licence list as RightKit's crate policy (one identical crates/<x>/deny.toml per crate, no root policy): widening it in one place must widen it in the other.
  const rootAllow = /allow = \[([^\]]+)\]/.exec(readFileSync(path.join(REPO, "crates", "rightkit-fs", "deny.toml"), "utf8"))[1].replace(/\s+/g, "");
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
