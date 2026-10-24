// The desktop app template, instantiated the way a new app is (copied, `right-release init`, committed), must pass every files-only
// RightKit contract with an empty baseline: pins, mandatory set, release config (M24), legal, QA lane, switches, hook, preflight rules
// and the doctor's file rows. This test replaces the template's own Rust gate (`pnpm check:template`), which compared pins by hand.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { parse as parseToml } from "smol-toml";
import { hookText } from "./bin/rightkit-gate.mjs";
import { assertCargoProfileContract } from "./cargo-profile-contract.mjs";
import { NEXT_STEPS, initTemplate, sortCargoLock } from "./init-command.mjs";
import { assertLegalReleaseContract } from "./legal-contract.mjs";
import { loadReleaseConfigSync } from "./release-config.mjs";
import { checkApp } from "./ownership-mandatory-set.mjs";
import { scanApp } from "./pipeline-scan.mjs";
import { loadGeneratorRegistry } from "./pipeline-generators.mjs";
import { evaluateQaAdoption } from "./qa-contract.mjs";
import { RENAMED_AWAY, RENAMED_AWAY_NPM } from "./renamed-away.mjs";
import { assertNoReleaseSwitches, parseInventory } from "./release-switch-scan.mjs";
import { registryEnv } from "./test-support/admission.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..");
const template = path.join(repoRoot, "desktop-app-template");
const versions = JSON.parse(readFileSync(path.join(here, "rightkit-versions.json"), "utf8"));
const read = (root, file) => readFileSync(path.join(root, file), "utf8");
const readJson = (root, file) => JSON.parse(read(root, file));
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");

const IDENTITY = { name: "Play Right", id: "app.orthiclabs.playright", slug: "playright" };
const SKIP = new Set(["node_modules", "dist", "target", ".right-release", ".cache", "gen", ".git"]);

/** Every file under `dir` (relative, posix), skipping build and install output. */
function walk(dir, base = dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (SKIP.has(entry.name)) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, base, out);
    else out.push(path.relative(base, full).split(path.sep).join("/"));
  }
  return out.sort();
}
const TEXT = /\.(?:json|toml|mjs|ts|tsx|rs|md|html|css|yaml|yml|sh|lock)$/;
const git = (cwd, ...args) => execFileSync("git", ["-c", "user.name=template-test", "-c", "user.email=template-test@example.invalid", "-c", "core.autocrlf=false", ...args], { cwd, encoding: "utf8" });

/** Copy the template into a new git repository, optionally rename it with init, commit everything. */
function instantiate({ rename = true } = {}) {
  const root = path.join(tempRoot("right-release-template-"), rename ? IDENTITY.slug : "desktop-app-template");
  mkdirSync(root, { recursive: true });
  cpSync(template, root, { recursive: true, filter: (source) => !SKIP.has(path.basename(source)) });
  const initResult = rename ? initTemplate(root, { name: IDENTITY.name, id: IDENTITY.id }) : null;
  git(root, "init", "-q", "-b", "main");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "instance");
  return { root, initResult };
}
// Built at module load: tempRoot ties its cleanup to the test that first calls it, so a lazily built instance would vanish after that test.
const renamed = instantiate();
const pristine = instantiate({ rename: false });
const instance = () => renamed;
const unrenamed = () => pristine;

const manifests = (root) => walk(root).filter((file) => /(^|\/)Cargo\.toml$/.test(file));
/** [name, spec] of every rightkit-* dependency across every table of every Cargo.toml. */
function rightkitCrateDeps(root) {
  const out = [];
  const tables = (doc) => [doc, ...Object.values(doc.target ?? {})].flatMap((scope) => ["dependencies", "dev-dependencies", "build-dependencies"].map((section) => scope[section] ?? {}));
  for (const file of manifests(root)) {
    const doc = parseToml(read(root, file));
    for (const table of [...tables(doc), doc.workspace?.dependencies ?? {}]) {
      for (const [name, spec] of Object.entries(table)) if (name.startsWith("rightkit-")) out.push({ file, name, spec });
    }
  }
  return out;
}

test("the template names no retired RightKit package or crate, and no app-resident gate or packaging script", () => {
  const files = walk(template).filter((file) => TEXT.test(file) || file.endsWith("Cargo.lock"));
  const retired = [...RENAMED_AWAY.keys(), ...RENAMED_AWAY_NPM.keys()];
  const escape = (name) => name.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
  const pattern = new RegExp(`(?<![\\w@/.-])(?:${retired.map(escape).join("|")})(?![\\w-])`);
  const hits = files.filter((file) => pattern.test(read(template, file)));
  assert.deepEqual(hits, [], "a retired name remains (renamed-away.mjs lists the replacements)");
  for (const gone of ["crates/tools", "scripts", "right-release.config.mjs", "placeholders.json"]) assert.equal(existsSync(path.join(template, gone)), false, `${gone} is app-resident release or gate code`);
});

test("every RightKit pin equals the registry version, exactly: npm, crates, the pnpm feed and its release-age allowance", () => {
  const { root } = unrenamed();
  const pkg = readJson(root, "package.json");
  const npm = { ...pkg.dependencies, ...pkg.devDependencies };
  const rightkit = Object.entries(npm).filter(([name]) => name.startsWith("@rightkit/"));
  assert.ok(rightkit.length >= 15, "the foundation set is declared");
  for (const [name, spec] of rightkit) assert.equal(spec, versions.registry.npm[name], `${name} must be pinned to the registry version`);
  assert.equal(pkg.devDependencies["@rightkit/release"], versions.registry.npm["@rightkit/release"]);
  assert.equal(pkg.packageManager, versions.packageManager);
  const allowance = [...read(root, "pnpm-workspace.yaml").matchAll(/^\s+- '(@rightkit\/[^']+)'\s*$/gm)].map((m) => m[1]).sort();
  assert.deepEqual(allowance, rightkit.map(([name, spec]) => `${name}@${spec}`).sort(), "minimumReleaseAgeExclude equals the @rightkit pins");
  const crates = rightkitCrateDeps(root);
  assert.ok(crates.length >= 15, "the foundation crates are declared");
  for (const { file, name, spec } of crates) {
    const version = typeof spec === "string" ? spec : spec.version;
    assert.ok(typeof spec === "string" || (!("path" in spec) && !("git" in spec) && !("registry" in spec)), `${file}: ${name} must come from crates.io`);
    assert.equal(version, `=${versions.registry.cargo[name]}`, `${file}: ${name} must be pinned =<registry version>`);
  }
  const lock = read(root, "Cargo.lock");
  assert.equal(sortCargoLock(lock), lock, "Cargo.lock is in the order cargo writes, so --locked accepts it");
  for (const { name } of crates) {
    const entry = new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "${versions.registry.cargo[name].replaceAll(".", "\\.")}"\\nsource = "registry\\+https://github.com/rust-lang/crates.io-index"\\nchecksum = "[0-9a-f]{64}"`);
    assert.match(lock, entry, `${name} is locked at the registry version from crates.io with a checksum`);
  }
});

test("the mandatory set: every desktop row is declared, and only the agent surface (M22) is not applicable, with its reason", () => {
  const { root } = instance();
  const report = checkApp(root);
  assert.equal(report.scanned, true);
  assert.deepEqual(report.problems, []);
  assert.equal(report.violations, 0, JSON.stringify(report.owners.filter((owner) => owner.status !== "ok")));
  assert.deepEqual(report.notApplicable.map((row) => row.row), ["M22"]);
  assert.ok(report.notApplicable[0].reason.length >= 10);
  assert.deepEqual(readJson(root, ".rightkit/ownership-exceptions.json"), { exceptions: [] }, "a new app is born with no ownership exceptions");
});

test("the release config is data that passes M24, and the legal block, QA lane, switches inventory and cargo profiles pass their gates", () => {
  const { root } = instance();
  const loaded = loadReleaseConfigSync(path.join(root, "right-release.config.json"));
  const { config } = loaded;
  assert.equal(config.app, IDENTITY.slug);
  assert.equal(config.version, readJson(root, "src-tauri/tauri.conf.json").version);
  assert.equal(existsSync(path.join(root, "right-release.config.mjs")), false);
  assert.equal(config.devtools, undefined, "no devtools key: DevTools stay denied");
  for (const platform of ["win", "mac"]) assertLegalReleaseContract(root, config.legal, config.app, platform);
  const qa = evaluateQaAdoption({ appRoot: root, repoRoot: root, app: config.app });
  assert.deepEqual(qa.refusals, []);
  assert.deepEqual(qa.warnings, []);
  assert.ok(qa.lane.length >= 2, `QA lane: ${qa.lane.join(", ")}`);
  assertCargoProfileContract(root, config.app);
  const inventory = read(root, config.releaseSwitches.inventory);
  assert.deepEqual(parseInventory(inventory).issues, []);
  assertNoReleaseSwitches({ sourceRoot: root, inventoryText: inventory, requireBinary: false }, "source switch inventory");
  assert.equal(readJson(root, "src-tauri/tauri.conf.json").bundle.licenseFile, undefined, "the in-app gate is the only assent surface");
  assert.equal(readJson(root, "src-tauri/tauri.conf.json").app.windows[0].visible, false, "the main window starts hidden; the shell reveals it");
  const report = scanApp({ appRoot: root, registry: loadGeneratorRegistry() });
  assert.deepEqual(report.findings, [], "M24 pipeline scan: no app-owned build, signing or packaging step");
});

test("legal: the manifest digests equal the bytes, the material documents carry no product name, and the acceptance contract names the manifest", () => {
  // The template's own committed manifest first: init re-hashes the instance, which would hide a digest that drifted in the template.
  const committed = readJson(unrenamed().root, "legal/legal-manifest.json");
  for (const item of [...committed.documents, ...committed.appendices]) assert.equal(item.sha256, sha(readFileSync(path.join(unrenamed().root, "legal", item.path))), `the template's committed digest of ${item.path} is stale: run pnpm exec legal-snapshot`);
  assertLegalReleaseContract(unrenamed().root, readJson(unrenamed().root, "right-release.config.json").legal, "rightapp", "win");
  const { root } = instance();
  const manifest = readJson(root, "legal/legal-manifest.json");
  for (const item of [...manifest.documents, ...manifest.appendices]) assert.equal(item.sha256, sha(readFileSync(path.join(root, "legal", item.path))), item.path);
  const materials = manifest.documents.filter((document) => document.material);
  assert.deepEqual(materials.map((document) => document.role).sort(), ["acceptable_use", "eula", "license", "product_schedule"]);
  for (const document of materials) {
    assert.ok(!read(root, `legal/${document.path}`).toLowerCase().includes(IDENTITY.slug), `${document.path} is hashed into the acceptance contract: it must not carry the product name`);
    assert.ok(!document.id.includes(IDENTITY.slug), `${document.id}: ids are hashed too`);
  }
  const contract = readJson(root, "src/generated/legal-contract.json");
  assert.equal(contract.acceptanceVersion, manifest.acceptanceVersion);
  assert.equal(contract.draft, false);
  const pristineContract = readJson(unrenamed().root, "src/generated/legal-contract.json");
  assert.equal(contract.materialSha256, pristineContract.materialSha256, "init leaves the acceptance digest alone");
});

test("init leaves no template identity behind, and the instance is its own Cargo workspace under the new names", () => {
  const { root, initResult } = instance();
  assert.ok(initResult.changed.includes("Cargo.lock") && initResult.changed.includes("legal/legal-manifest.json"));
  // r2-rightapps is the name of the RightApps distribution provider, not the template identity: init keeps it.
  const leaked = walk(root).filter((file) => (TEXT.test(file) || file === "Cargo.lock") && /rightapp|RightApp|RIGHTAPP|app\.rightsuite\.rightapp/.test(read(root, file).replaceAll("r2-rightapps", "")));
  assert.deepEqual(leaked, []);
  assert.equal(readJson(root, "right-release.config.json").distribution.provider, "r2-rightapps");
  const workspace = parseToml(read(root, "Cargo.toml")).workspace;
  assert.deepEqual(workspace.members, ["src-tauri", "crates/protocol", "crates/engine"]);
  assert.equal(parseToml(read(root, "crates/engine/Cargo.toml")).package.name, "playright-engine");
  assert.equal(readJson(root, "src-tauri/tauri.conf.json").identifier, IDENTITY.id);
  assert.equal(parseToml(read(root, "rightkit-qa.toml")).app, IDENTITY.slug);
  assert.equal(parseToml(read(root, "journeys.toml")).app, IDENTITY.slug);
  assert.ok(NEXT_STEPS.length >= 4);
});

test("the committed pre-push hook is exactly what `rightkit-gate install-hook` writes, is executable, and `prepare` installs it", () => {
  const { root } = instance();
  assert.equal(read(root, ".githooks/pre-push").replace(/\r\n/g, "\n"), hookText("node_modules/@rightkit/release/bin/rightkit-gate.mjs"));
  assert.match(git(root, "ls-files", "-s", ".githooks/pre-push"), /^100755 /, "committed with executable mode");
  assert.equal(readJson(root, "package.json").scripts.prepare, "rightkit-gate install-hook");
});

test("journeys: the manifest is well formed, every unit test is tagged pure, and no test launches a process or spawns the e2e", () => {
  const { root } = instance();
  const journeys = parseToml(read(root, "journeys.toml"));
  assert.equal(journeys.version, 1);
  assert.equal(journeys.tests.untagged_baseline, 0);
  const command = journeys.build.command;
  assert.deepEqual(command.slice(0, 3), ["rustright", "cargo", "build"]);
  const flag = command.indexOf("--features");
  assert.deepEqual(flag < 0 ? [] : command[flag + 1].split(","), journeys.build.features, "[build].features equals the --features of its command");
  for (const journey of journeys.journey) {
    assert.ok(existsSync(path.join(root, journey.entry)), `${journey.id}: entry ${journey.entry} exists`);
    assert.notEqual(journey.data, "real");
  }
  assert.equal(journeys.journey.filter((journey) => journey.release_critical).length >= 1, true);
  const qa = parseToml(read(root, "rightkit-qa.toml"));
  assert.equal(qa.engine.build.join(" "), command.join(" "), "single-scenario runs build the same QA engine");
  const untagged = [];
  for (const file of walk(root).filter((f) => f.endsWith(".rs") && /^(?:crates|src-tauri)\//.test(f))) {
    const lines = read(root, file).split("\n");
    lines.forEach((line, index) => {
      if (!/^\s*#\[(?:tokio::)?test\b/.test(line)) return;
      let at = index - 1;
      while (at >= 0 && (/^\s*#\[/.test(lines[at]) || /^\s*\/\/\//.test(lines[at]))) at -= 1;
      if (!/^\s*\/\/ rk-test: (?:pure|contract|pin #\d+)\s*$/.test(lines[at] ?? "")) untagged.push(`${file}:${index + 1}`);
    });
    const testCode = read(root, file).split(/^#\[cfg\(test\)\]/m).slice(1).join("\n");
    assert.ok(!/(?<![A-Za-z])Command::new\(|CARGO_BIN_EXE_/.test(testCode) && !(/(^|\/)tests\//.test(file) && /(?<![A-Za-z])Command::new\(|CARGO_BIN_EXE_/.test(read(root, file))), `${file}: a test that launches a process is an end-to-end test; journeys run it outside cargo test`);
  }
  assert.deepEqual(untagged, []);
});

test("no environment variable the release switch scanner denies is read by shipped code: the only isolation is the shared test-mode feature", () => {
  const { root } = instance();
  const shipped = walk(root).filter((f) => /\.(?:rs|ts|tsx)$/.test(f) && !/(^|\/)tests?\//.test(f));
  const reads = shipped.filter((file) => /(?:env::var(?:_os)?\(|std::env::var|process\.env|import\.meta\.env)/.test(read(root, file)));
  assert.deepEqual(reads.filter((file) => file !== "src-tauri/build.rs"), [], "no shipped source reads the environment");
});

test("the baseline for RightKit's own preflight no longer lists the template, and preflight passes on the instance with no known findings", () => {
  const baseline = JSON.parse(readFileSync(path.join(repoRoot, "ci", "app-check-baseline.json"), "utf8"));
  assert.ok(!JSON.stringify(baseline).includes("desktop-app-template"));
  const gate = JSON.parse(readFileSync(path.join(repoRoot, "ci", "gate.config.json"), "utf8"));
  assert.ok(!gate.fixtures.includes("desktop-app-template"), "the template is gated for real, not a fixture");
  const { root } = instance();
  const run = spawnSync(process.execPath, [path.join(repoRoot, "scripts", "preflight.mjs"), "--repo", root, "--commit", "HEAD"], { encoding: "utf8" });
  assert.equal(run.status, 0, `${run.stdout}\n${run.stderr}`);
  assert.ok(!/^(?:KNOWN|FAIL|STALE)/m.test(run.stdout), run.stdout);
});

test("right-release doctor: every file row passes for the instance, on this host, for both release platforms", () => {
  const { root } = instance();
  const cli = path.join(here, "cli", "right-release.mjs");
  for (const platform of ["win", "mac"]) {
    const result = spawnSync(process.execPath, [cli, "doctor", "--config", path.join(root, "right-release.config.json"), "--platform", platform], {
      cwd: root, encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_MIN_FREE_GB: "1", ...registryEnv(root) },
    });
    const output = `${result.stdout}\n${result.stderr}`;
    // A host with no enrolled protected trust reports ownership as an advisory WARN; an enrolled host fails the fixture repository on
    // ownership alone (it has no origin). Every other row must pass either way, and the doctor must exit 0 in the first case.
    const failures = output.split(/\r?\n/).filter((line) => /^\s*\[(?:FAIL|ERROR)\] /.test(line) && !/^\s*\[FAIL\] ownership: /.test(line));
    assert.deepEqual(failures, [], `${platform}:\n${output}`);
    assert.ok(!/^right-release: (?!newer RightKit version \(informational\))/m.test(result.stderr), `${platform} refusals on stderr:\n${result.stderr}`);
    if (/^\s*\[WARN\] ownership: /m.test(output)) assert.equal(result.status, 0, `${platform}:\n${output}`);
    else assert.match(output, /^\s*\[FAIL\] ownership: /m);
  }
});
