import crypto from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import {
  CLASS_A_SCAN_PREFIXES, CLASS_B_SCAN_PREFIXES, canonicalJson, fileScanCache, inheritedEnvDigest, requiredScanIds, scanCacheKey, checkArgs, clippyArgs, digestScanSet, docArgs, fmtCheckArgs,
  defaultSpawn, deterministicScansEnabled, metadataArgs, planScans, resolveInvocation, runScans, scanClassOf, scanDeferrals, scanFailures,
  CHECKER_IDENTITY } from "./deterministic-scans.mjs";
import { requiredScanProblems } from "./scan-digest.mjs";
import * as scansModule from "./deterministic-scans.mjs";

// Read off the module so a test of a function that does not exist yet fails on its own, not with an import error for the whole file.
const { scriptBrokerCall, PINNED_SCAN_TOOLS = {}, pinnedVersionProblem } = scansModule;

const CONTENT = path.join(path.sep, "c", "rustright");
const LIVE = path.join(path.sep, "l", "rustright");
const norm = (p) => p.replaceAll("\\", "/");

function fakeFs(contentFiles, liveFiles = {}) {
  const content = new Map(Object.entries(contentFiles).map(([k, v]) => [norm(path.join(CONTENT, k)), v]));
  const live = new Map(Object.entries(liveFiles).map(([k, v]) => [norm(path.join(LIVE, k)), v]));
  const all = new Map([...content, ...live]);
  return {
    exists: (p) => all.has(norm(p)) || [...all.keys()].some((k) => k.startsWith(`${norm(p)}/`)),
    readFile: (p) => { const v = all.get(norm(p)); if (v === undefined) throw new Error(`ENOENT ${p}`); return v; },
    realpath: (p) => p,
  };
}

const crateFiles = {
  "crates/foo/Cargo.toml": 'edition = "2024"\nversion = "1.2.3"\nlicense = "MIT"\n',
  "crates/foo/CHANGELOG.md": "## 1.2.3\n- x\n",
  "crates/foo/src/lib.rs": "pub fn f() {}\n",
  // A valid, empty advisory-exception set (packages/release/advisory-exceptions.mjs).
  "ci/advisory-exceptions.json": JSON.stringify({ schema: 1, exceptions: [] }),
  "deny.toml": '[advisories]\nunmaintained = "workspace"\nyanked = "deny"\nignore = []\n',
  ".cargo/audit.toml": "[advisories]\nignore = []\n",
  "Cargo.lock": "version = 4\n",
};
const planFor = (changed, extra = {}, opts = {}) => {
  const fs = fakeFs({ ...crateFiles, ...extra }, { ...crateFiles, ...extra });
  return { fs, plan: planScans({ changed, contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32", ...opts }) };
};
const ids = (plan, cls) => plan.scans.filter((s) => !cls || s.class === cls).map((s) => s.id);

test("plan: a changed crate selects class A and class B scans, through rustright cargo", () => {
  const { plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const a = ids(plan, "A"); const b = ids(plan, "B");
  for (const id of ["rustfmt:2024", "toml-parse", "cargo-metadata", "cargo-deny", "cargo-audit", "changelog-version:foo", "license", "secret-scan"]) assert.ok(a.includes(id), `class A has ${id}`);
  for (const id of ["check-features:foo:0", "clippy:foo", "doc:foo", "test:rust:foo"]) assert.ok(b.includes(id), `class B has ${id}`);
  assert.ok(!b.includes("check:foo"), "default clippy already type-checks every target: no separate default check");
  for (const s of plan.scans.filter((s) => s.tool === "rustright")) assert.equal(s.args[0], "cargo", `${s.id} runs through rustright cargo`);
  const doc = plan.scans.find((s) => s.id === "doc:foo");
  assert.deepEqual(doc.args.slice(-3), ["--", "-D", "warnings"]);
  assert.ok(doc.args.includes("rustdoc") && doc.args.includes("--lib"));
  assert.equal(doc.env.RUSTDOCFLAGS, undefined, "the broker owns RUSTDOCFLAGS");
  assert.match(plan.scans.find((s) => s.id === "clippy:foo").command, /-D warnings$/);
});

test("plan: class split is total and every id maps to its class", () => {
  const { plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml", "scripts/x.mjs", "a.json"], { "scripts/x.mjs": "", "a.json": "{}" });
  for (const s of plan.scans) assert.equal(scanClassOf(s.id), s.class, `${s.id} class`);
  assert.equal(scanClassOf("clippy:x"), "B");
  assert.equal(scanClassOf("secret-scan"), "A");
  assert.equal(scanClassOf("mystery"), null);
  assert.equal(CLASS_A_SCAN_PREFIXES.filter((p) => CLASS_B_SCAN_PREFIXES.includes(p)).length, 0);
});

test("plan: JS packages select lockfile, audit, tsc and eslint when configured; unchanged areas select nothing", () => {
  const extra = { "packages/p/package.json": '{"license":"MIT","scripts":{"test":"x"}}', "packages/p/tsconfig.json": "{}", "packages/p/eslint.config.mjs": "" };
  const { plan } = planFor(["packages/p/package.json"], extra);
  assert.deepEqual(ids(plan, "A").filter((i) => i.startsWith("pnpm")), ["pnpm-lockfile", "pnpm-audit"]);
  assert.ok(ids(plan, "B").includes("tsc:p"));
  assert.ok(ids(plan, "B").includes("eslint:p"));
  assert.ok(ids(plan, "B").includes("test:npm:p"));
  const quiet = planFor(["README.md"], { "README.md": "x" }).plan;
  assert.equal(ids(quiet).filter((i) => /cargo|pnpm|clippy|tsc/.test(i)).length, 0);
});

test("plan: tests are omitted when includeTests is false; workflows select actionlint; swift is a note off macOS", () => {
  const { plan } = planFor(["crates/foo/src/lib.rs", ".github/workflows/ci.yml", "swift/a.swift"], { ".github/workflows/ci.yml": "a: b", "swift/a.swift": "" }, { includeTests: false });
  assert.equal(ids(plan).filter((i) => i.startsWith("test:")).length, 0);
  assert.ok(ids(plan, "A").includes("actionlint"));
  assert.ok(ids(plan, "A").includes("yaml-parse"));
  assert.equal(plan.notes.length, 1);
  const mac = planFor(["swift/a.swift"], { "swift/a.swift": "" }, { platform: "darwin" }).plan;
  assert.ok(ids(mac, "B").includes("swift-build"));
});

test("plan: scanSetDigest is stable and changes with the scan set", () => {
  const a = planFor(["crates/foo/src/lib.rs"]).plan;
  const b = planFor(["crates/foo/src/lib.rs"]).plan;
  assert.equal(a.scanSetDigest, b.scanSetDigest);
  const c = planFor(["crates/foo/Cargo.toml"]).plan;
  assert.notEqual(a.scanSetDigest, c.scanSetDigest);
  assert.match(a.scanSetDigest, /^[0-9a-f]{64}$/);
  assert.equal(digestScanSet(a.scans), a.scanSetDigest);
  assert.equal(canonicalJson({ b: 1, a: [2, { d: 1, c: 2 }] }), '{"a":[2,{"c":2,"d":1}],"b":1}');
});

test("builders produce the exact command lines the CI gate and local publish use", () => {
  assert.deepEqual(clippyArgs("a", { features: ["--all-features"] }), ["clippy", "-p", "a", "--no-deps", "--all-targets", "--all-features", "--", "-D", "warnings"]);
  assert.deepEqual(clippyArgs(["a", "b"], { messageFormat: "json-render-diagnostics" }), ["clippy", "-p", "a", "-p", "b", "--message-format=json-render-diagnostics", "--no-deps", "--all-targets", "--", "-D", "warnings"]);
  assert.deepEqual(checkArgs("a", { features: ["--all-features"] }), ["check", "-p", "a", "--all-targets", "--all-features"]);
  assert.deepEqual(docArgs("a"), ["doc", "-p", "a", "--no-deps", "--all-features", "--locked"]);
  assert.deepEqual(fmtCheckArgs("a"), ["fmt", "-p", "a", "--check"]);
  assert.deepEqual(fmtCheckArgs(), ["fmt", "--all", "--check"]);
  assert.deepEqual(metadataArgs(), ["metadata", "--no-deps", "--format-version", "1", "--locked"]);
});

// A recording spawn: probes answer a version, every other call is logged and returns `status`.
function fakeSpawn({ missing = [], failing = [], log = [], shellcheckVersion = PINNED_SCAN_TOOLS.shellcheck } = {}) {
  return (command, args) => {
    log.push([command, ...args]);
    const name = path.basename(command).replace(/\.exe$/, "");
    if (missing.includes(name)) return { error: Object.assign(new Error(`spawn ${name} ENOENT`), { code: "ENOENT" }), status: null };
    const isProbe = args.length === 1 && args[0].includes("version") || (args.length === 2 && args[0] === "cargo" && args[1] === "--version");
    // ShellCheck answers as the real one does (a banner, then its version), at the version class A accepts.
    if (isProbe && name === "shellcheck") return { status: 0, stdout: `ShellCheck - shell script analysis tool\nversion: ${shellcheckVersion}\nlicense: GNU General Public License, version 3\n`, stderr: "" };
    if (isProbe) return { status: 0, stdout: `${name} 1.0.0\n`, stderr: "" };
    if (failing.some((f) => args.join(" ").includes(f))) return { status: 1, stdout: "", stderr: "boom\n" };
    return { status: 0, stdout: "", stderr: "" };
  };
}
const deps = (over = {}) => ({ spawn: fakeSpawn(), ...fakeFs({}), findRustfmt: () => "rustfmt", loadToml: async () => () => ({}), scanSecrets: async () => () => [], env: {}, execPath: "node", ensureAdvisoryDb: () => "/cache/advisory-db/pinned", prepareDenyOverlay: () => ({ configPath: "/tmp/rk-deny/deny.toml", cleanup() {} }), ...over });

test("runner: all scans pass and the result is a plain receipt-ready object", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const result = await runScans(plan, deps({ ...fs }));
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.deepEqual(JSON.parse(JSON.stringify(result)), result);
  assert.equal(result.scanSetDigest, plan.scanSetDigest);
  assert.ok(result.toolVersions.rustright);
  assert.ok(result.scans.every((s) => s.status === "pass"));
  const order = result.scans.map((s) => s.class).join("");
  assert.match(order, /^A+B+$/, "class A runs before class B");
});

test("runner: the rustright version recorded for the cache is cargo's, so a toolchain upgrade invalidates cached passes", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const log = [];
  await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }) }));
  const probes = log.filter(([command]) => command === "rustright" || command === "cargo").filter((call) => call.includes("--version"));
  assert.deepEqual(probes, [["rustright", "cargo", "--version"]], "`rustright --version` would record the client, not the toolchain");
});

test("runner: a missing required tool FAILS, never skips", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ missing: ["rustright"] }) }));
  assert.equal(result.ok, false);
  const metadata = result.scans.find((s) => s.id === "cargo-metadata");
  assert.equal(metadata.status, "fail");
  assert.equal(metadata.code, "TOOL_MISSING");
  assert.match(metadata.detail, /never skipped/);
  assert.equal(result.toolVersions.rustright, null);
  assert.ok(!result.scans.some((s) => s.status === "skipped"));
});

test("runner: missing rustfmt and missing TOML parser fail", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const result = await runScans(plan, deps({ ...fs, findRustfmt: () => null, loadToml: async () => { throw new Error("nope"); } }));
  assert.equal(result.scans.find((s) => s.id === "rustfmt:2024").code, "TOOL_MISSING");
  assert.equal(result.scans.find((s) => s.id === "toml-parse").code, "TOOL_MISSING");
});

// publish.yml run 40286333676 (verify, macOS + Windows): the gate runs 4 jobs, so `spawn` is async, and a host whose rustup has only
// `stable-*` toolchains reaches findRustfmt's `which` fallback, which got a Promise (no status) and called rustfmt missing.
test("runner: concurrent scans still find rustfmt through `which` on a host with only stable-* toolchains", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const sync = fakeSpawn();
  const lookups = [];
  const lookupSpawn = (command, args) => { lookups.push([command, ...args]); return { status: 0, stdout: "/home/u/.cargo/bin/rustfmt\n", stderr: "" }; };
  const over = { ...fs, findRustfmt: scansModule.findRustfmt, readdir: () => ["stable-x86_64-unknown-linux-gnu"], platform: "linux", spawn: async (c, a) => sync(c, a), jobs: 4 };
  const result = await runScans(plan, deps({ ...over, lookupSpawn }));
  const row = result.scans.find((s) => s.id === "rustfmt:2024");
  assert.equal(row.status, "pass", JSON.stringify(row));
  assert.deepEqual(lookups, [["which", "rustfmt"]]);
  // Original: the async spawn answered the lookup.
  const red = await runScans(plan, deps({ ...over, lookupSpawn: over.spawn }));
  assert.equal(red.scans.find((s) => s.id === "rustfmt:2024").status, "fail");
});

test("findRustfmt refuses an async spawn rather than reporting rustfmt missing", () => {
  assert.throws(() => scansModule.findRustfmt({ env: {}, exists: () => false, platform: "linux", spawn: async () => ({ status: 0, stdout: "/x/rustfmt\n" }) }), /synchronous spawn/);
});

test("runner: a failed class A scan leaves class B not-run", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ failing: ["deny --offline"], log }) }));
  assert.equal(result.scans.find((s) => s.id === "cargo-deny").status, "fail");
  const b = result.scans.filter((s) => s.class === "B");
  assert.ok(b.length > 0 && b.every((s) => s.status === "not-run"));
  assert.ok(!log.some((c) => c.includes("clippy")), "no compiler command ran");
  assert.equal(result.ok, false);
  assert.ok(scanFailures(result).some((l) => l.startsWith("cargo-deny")));
});

test("runner: classes [A] runs no class B; exclude is an explicit omission", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const onlyA = await runScans(plan, deps({ ...fs, classes: ["A"] }));
  assert.ok(onlyA.scans.every((s) => s.class === "A"));
  assert.notEqual(onlyA.scanSetDigest, plan.scanSetDigest);
  assert.equal(onlyA.scanSetDigest, plan.classDigests.A);
  const noTests = await runScans(plan, deps({ ...fs, exclude: (s) => s.id.startsWith("test:") }));
  assert.ok(!noTests.scans.some((s) => s.id.startsWith("test:")));
});

test("runner: Rust scans run in the live root with rustdoc warnings as errors; content scans in the content root", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml", "x.mjs"], { "x.mjs": "" });
  const calls = [];
  const spawn = (command, args, options) => { calls.push({ command, args, options }); return fakeSpawn()(command, args); };
  await runScans(plan, deps({ ...fs, spawn }));
  const doc = calls.find((c) => c.args.includes("rustdoc"));
  assert.deepEqual(doc.args.slice(-3), ["--", "-D", "warnings"]);
  assert.equal(doc.options.env.RUSTDOCFLAGS, undefined);
  assert.equal(path.normalize(doc.options.cwd), path.normalize(LIVE));
  const nodeCheck = calls.find((c) => c.args[0] === "--check" && c.args[1] === "x.mjs");
  assert.equal(path.normalize(nodeCheck.options.cwd), path.normalize(CONTENT));
});

test("internal checks: JSON, changelog heading, license, ownership catalog and secret scan report problems", async () => {
  const files = {
    "bad.json": "{", "ok.json": "{}",
    "crates/foo/Cargo.toml": 'version = "2.0.0"\n', "crates/foo/CHANGELOG.md": "## 1.9.0\n", "crates/foo/src/lib.rs": "",
    "packages/release/rightkit-ownership.json": "a", "crates/rightkit-ownership/catalog/ownership-catalog.json": "b",
    "packages/q/package.json": '{"name":"q"}',
  };
  const fs = fakeFs(files, files);
  const plan = planScans({ changed: Object.keys(files), contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32", includeTests: false });
  const result = await runScans(plan, { ...deps({ ...fs }), classes: ["A"], scanSecrets: async () => () => ["crates/foo/.env"] });
  const by = (id) => result.scans.find((s) => s.id === id);
  assert.match(by("json-parse").detail, /bad\.json/);
  assert.match(by("changelog-version:foo").detail, /2\.0\.0/);
  assert.match(by("license").detail, /crates\/foo\/Cargo\.toml/);
  assert.match(by("license").detail, /packages\/q\/package\.json/);
  assert.equal(by("ownership-catalog").status, "fail");
  assert.match(by("secret-scan").detail, /\.env/);
});

test("license: a crate that can never be published needs no license key; the secret scan runs in the repository-gate mode", async () => {
  const crate = (name, tail) => ({ [`crates/${name}/Cargo.toml`]: `[package]\nname = "${name}"\nversion = "1.0.0"\n${tail}`, [`crates/${name}/src/lib.rs`]: "" });
  const files = {
    ...crate("nopub", "publish = false\n"),
    ...crate("emptypub", "publish = [ ]\n"),
    ...crate("metapub", "\n[package.metadata.x]\npublish = false\n"),
    ...crate("binpub", "\n[[bin]]\nname = \"b\"\npublish = false\n"),
    ...crate("arraymetapub", "\n[[package.metadata.x]]\npublish = false\n"),
    ...crate("pkgthenbin", "publish = false\n[[bin]]\nname = \"b\"\n"),
    ...crate("truepub", "publish = true\n"),
  };
  const fs = fakeFs(files, files);
  const plan = planScans({ changed: Object.keys(files), contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "linux", includeTests: false });
  const options = [];
  const result = await runScans(plan, { ...deps({ ...fs }), classes: ["A"], scanSecrets: async () => (dir, opts) => { options.push(opts); return []; } });
  const license = result.scans.find((s) => s.id === "license");
  assert.match(license.detail, /crates\/metapub\/Cargo\.toml/, "publish = false outside [package] does not exempt");
  assert.match(license.detail, /crates\/binpub\/Cargo\.toml/, "publish = false under [[bin]] does not exempt");
  assert.match(license.detail, /crates\/arraymetapub\/Cargo\.toml/, "publish = false under [[package.metadata.x]] does not exempt");
  assert.doesNotMatch(license.detail, /pkgthenbin/, "publish = false in [package] before a [[bin]] still exempts");
  assert.match(license.detail, /crates\/truepub\/Cargo\.toml/);
  assert.doesNotMatch(license.detail, /nopub|emptypub/);
  assert.ok(options.length > 0 && options.every((o) => o?.envTemplates === true && o.repoRoot === CONTENT), "the repository scan runs in the repository-gate mode, against the gated commit's records");
});

test("secret-scan: a violation found through two overlapping units is reported once, as the scanner names it", async () => {
  const files = {
    "crates/foo/Cargo.toml": '[package]\nname = "foo"\nversion = "1.0.0"\nlicense = "MIT"\n', "crates/foo/src/lib.rs": "",
    "packages/foo/package.json": '{"name":"foo","private":true}',
  };
  const fs = fakeFs(files, files);
  const plan = planScans({ changed: Object.keys(files), contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "linux", includeTests: false });
  const dirs = [];
  const scanSecrets = async () => (dir, opts) => { dirs.push(dir); return [".env.example: env template (sha256 abc) is not recorded", "docs/vec.md:3: content matches a GitHub token (gh*_) (line 3)"]; };
  const result = await runScans(plan, { ...deps({ ...fs }), classes: ["A"], scanSecrets });
  const secret = result.scans.find((s) => s.id === "secret-scan");
  assert.equal(secret.status, "fail");
  assert.ok(dirs.length >= 2, "both units were scanned");
  assert.equal(secret.detail, ".env.example: env template (sha256 abc) is not recorded; docs/vec.md:3: content matches a GitHub token (gh*_) (line 3)");
});

test("secret-scan: the gate admits an env template and a fixture only by the records of the gated commit", async (t) => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-gate-secret-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const sha = (text) => crypto.createHash("sha256").update(text).digest("hex");
  const template = "API_URL=http://localhost:3000\nPOSTGRES_PASSWORD=postgres\n";
  const vector = `// fake test vector\nlet t = "${"gh" + "p_" + "A".repeat(24)}";\n`;
  const put = (name, body) => { mkdirSync(path.dirname(path.join(root, name)), { recursive: true }); writeFileSync(path.join(root, name), body); };
  put("package.json", '{"name":"app","private":true}');
  put(".env.example", template);
  put("src/redact.rs", vector);
  const run = async () => {
    const plan = planScans({ changed: [".env.example", "src/redact.rs"], contentRoot: root, liveRoot: root, platform: "linux", includeTests: false, npmUnits: [{ name: "app", dir: "." }] });
    const result = await runScans(plan, { classes: ["A"], exclude: (scan) => scan.id !== "secret-scan" });
    return result.scans.find((s) => s.id === "secret-scan");
  };
  const refused = await run();
  assert.equal(refused.status, "fail");
  assert.match(refused.detail, new RegExp(`\\.env\\.example: env template \\(sha256 ${sha(template)}\\) is not recorded in \\.rightkit/secret-scan-records\\.json; after reviewing`));
  assert.match(refused.detail, new RegExp(`src/redact\\.rs:2: content matches a GitHub token \\(gh\\*_\\) \\(line 2\\); sha256 ${sha(vector)}`));
  put(".rightkit/secret-scan-records.json", JSON.stringify({ schema: 1, records: [
    { path: ".env.example", sha256: sha(template), kind: "env-template", note: "local dev defaults only" },
    { path: "src/redact.rs", sha256: sha(vector), kind: "fixture", note: "redaction test vector, fake value" },
  ] }));
  const admitted = await run();
  assert.equal(admitted.status, "pass", admitted.detail);
  put(".env.example", `${template}EXTRA=1\n`);
  assert.match((await run()).detail, /\.env\.example: env template changed since it was recorded/);
});

test("no local compile grant: every broker scan is deferred by name, never passed and never run outside the broker", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { localCompile: false });
  const broker = ["cargo-metadata", "cargo-deny", "cargo-audit", "check-features:foo:0", "clippy:foo", "doc:foo", "test:rust:foo"];
  assert.deepEqual([...plan.deferred].sort(), [...broker].sort());
  for (const id of broker) assert.ok(plan.scans.find((x) => x.id === id)?.deferred, `${id} stays in the plan, deferred`);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }) }));
  assert.equal(result.ok, false, "a deferred scan is not a pass");
  assert.equal(result.complete, false);
  const classA = ["cargo-metadata", "cargo-deny", "cargo-audit"];
  for (const id of classA) assert.equal(result.scans.find((s) => s.id === id).status, "deferred");
  // A deferred class A scan is not a class A pass: no class B scan runs after it.
  for (const s of result.scans.filter((x) => x.class === "B")) assert.equal(s.status, "not-run", s.id);
  assert.ok(!log.some((c) => /\bcargo\b/.test(c)), "no cargo runs outside the broker");
  const lines = scanDeferrals(result);
  assert.equal(lines.length, classA.length);
  for (const id of classA) assert.ok(lines.some((l) => l.startsWith(`${id}: deferred, not passed`)));
  assert.ok(plan.notes.some((n) => /deferred, not passed/.test(n) && n.includes("clippy:foo")));
  // With a grant nothing is deferred.
  assert.deepEqual(planFor(["crates/foo/Cargo.toml"]).plan.deferred, []);
});

test("a workspace Cargo.lock or toolchain change puts every crate in scope", () => {
  for (const changed of [["Cargo.lock"], ["Cargo.toml"], ["rust-toolchain.toml"], [".cargo/config.toml"]]) {
    const extra = { "Cargo.lock": "version = 4\n", "Cargo.toml": "[workspace]\n", "rust-toolchain.toml": "[toolchain]\n", ".cargo/config.toml": "[build]\n", "crates/bar/Cargo.toml": crateFiles["crates/foo/Cargo.toml"], "crates/bar/src/lib.rs": "pub fn g() {}\n" };
    const { plan } = planFor(changed, extra, { listDir: () => ["foo", "bar"] });
    for (const name of ["foo", "bar"]) {
      for (const kind of ["clippy", "doc", "test:rust"]) {
        assert.ok(plan.scans.some((s) => s.id === `${kind}:${name}`), `${changed[0]} plans ${kind}:${name}`);
      }
    }
  }
});

test("content scans run on the exported commit; generated-docs (repository code) is class B", () => {
  const extra = { ".github/workflows/ci.yml": "a: b", "scripts/generate-index.mjs": "", "packages/p/package.json": '{"license":"MIT"}' };
  const { plan } = planFor([".github/workflows/ci.yml", "packages/p/package.json"], extra);
  const by = (id) => plan.scans.find((s) => s.id === id);
  assert.equal(by("actionlint").root, "content");
  assert.equal(by("pnpm-lockfile").root, "content");
  assert.equal(by("pnpm-audit").root, "content");
  assert.equal(by("generated-docs").class, "B");
  assert.equal(scanClassOf("generated-docs"), "B");
  // Every class A scan is either content-rooted or an internal check, except the one named live check.
  const live = plan.scans.filter((s) => s.class === "A" && !s.internal && s.root === "live").map((s) => s.id);
  assert.deepEqual(live, []);
});

test("plan: a crate directory name with shell metacharacters becomes a failing path-policy scan", () => {
  const evil = "crates/a&echo/Cargo.toml";
  const fs = fakeFs({ [evil]: "" }, {});
  const plan = planScans({ changed: [evil], contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32" });
  const refused = plan.scans.find((s) => s.id.startsWith("path-policy:"));
  assert.match(refused.detail, /not a plain name/);
  assert.ok(!plan.scans.some((s) => s.args?.some((a) => a.includes("a&echo") && s.tool !== "rustfmt")));
});

test("spawn never uses a shell: Windows shims accept only allowlisted arguments", async () => {
  const env = { Path: "C:\\bin;C:\\tools", PATHEXT: ".COM;.EXE;.BAT;.CMD", ComSpec: "C:\\Windows\\System32\\cmd.exe" };
  const present = new Set(["C:\\bin\\pnpm.cmd", "C:\\tools\\rustright.exe", "C:\\tools\\rustright.cmd", "C:\\bin\\other.cmd"].map((p) => path.win32.normalize(p)));
  const exists = (p) => present.has(path.win32.normalize(p));
  const inv = (command, args, over = {}) => resolveInvocation(command, args, { platform: "win32", env, exists, execPath: "node.exe", ...over });
  // .exe is preferred over .cmd and started directly, with the argument as one argv element.
  const exe = inv("rustright", ["cargo", "metadata", "& echo X"]);
  assert.match(exe.file, /rustright\.exe$/i);
  assert.deepEqual(exe.args, ["cargo", "metadata", "& echo X"]);
  assert.equal(exe.verbatim, false);
  // A .cmd shim never receives a shell metacharacter.
  for (const evil of ["& echo X", "--version & echo X", "a|b", "a>b", "%PATH%", "a^b", "\"x\"", "x y", "a;b", "$(x)"]) {
    const r = inv("other", [evil]);
    assert.ok(r.error, `rejects ${JSON.stringify(evil)}`);
    assert.match(r.error.message, /refusing/);
  }
  const ok = inv("other", ["install", "--frozen-lockfile", "packages/a.test.mjs"]);
  assert.equal(ok.verbatim, true);
  assert.match(ok.args.at(-1), /^""C:\\bin\\other\.cmd" install --frozen-lockfile packages\/a\.test\.mjs"$/);
  // A missing command is ENOENT, and the current directory is never searched.
  assert.equal(inv("nothing", []).error.code, "ENOENT");
  // npm-style pnpm.cmd runs through node, so even odd arguments stay argv elements.
  present.add(path.win32.normalize("C:\\bin\\node_modules\\pnpm\\bin\\pnpm.cjs"));
  const pnpm = inv("pnpm", ["audit", "& echo X"]);
  assert.equal(pnpm.file, "node.exe");
  assert.deepEqual(pnpm.args.slice(1), ["audit", "& echo X"]);
  // POSIX passes argv through untouched.
  assert.deepEqual(resolveInvocation("x", ["& echo X"], { platform: "linux" }), { file: "x", args: ["& echo X"], verbatim: false });
  // Real spawn: an argument with `& echo` is data, not a command (a shell would print RK_INJECTED on its own line).
  const r = defaultSpawn(process.execPath, ["-e", "console.log(JSON.stringify(process.argv.slice(1)))", "--", "--version & echo RK_INJECTED"], { env: process.env });
  assert.equal(r.status, 0);
  assert.deepEqual(r.stdout.trim().split(/\r?\n/), [JSON.stringify(["--version & echo RK_INJECTED"])]);});

test("a thrown scan error is a failure, not a pass", async () => {
  const { fs, plan } = planFor(["a.json"], { "a.json": "{}" });
  const result = await runScans(plan, deps({ readFile: () => { throw new Error("disk"); }, exists: fs.exists, classes: ["A"] }));
  assert.equal(result.ok, false);
  assert.equal(result.scans[0].status, "fail");
  assert.match(result.scans[0].detail, /disk/);
});

// ---- security: path containment, shell-shim arguments, class order, trusted code ----------------------------

test("plan: absolute or escaping changed paths become failing path-policy scans and are never run", async () => {
  const files = { "x.mjs": "", "a.json": "{}" };
  const fs = fakeFs(files, files);
  const bad = ["../outside.mjs", "a/../../outside.mjs", "/etc/passwd.mjs", "C:/Windows/x.mjs", "C:\\Windows\\y.mjs", "\\\\host\\share\\z.mjs", "..", "x\0.mjs"];
  const plan = planScans({ changed: ["x.mjs", "a.json", ...bad], contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32", includeTests: false });
  const policy = plan.scans.filter((s) => s.id.startsWith("path-policy:"));
  assert.equal(policy.length, bad.length);
  assert.ok(policy.every((s) => s.class === "A" && scanClassOf(s.id) === "A"));
  assert.deepEqual(plan.scans.filter((s) => s.id.startsWith("node-check:")).map((s) => s.id), ["node-check:x.mjs"]);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }), classes: ["A"] }));
  assert.equal(result.ok, false);
  assert.equal(result.scans.filter((s) => s.id.startsWith("path-policy:")).every((s) => s.status === "fail" && /refused, never run/.test(s.detail)), true);
  assert.ok(!log.some((c) => c.some((a) => /outside|passwd|Windows|share/.test(a))), "a refused path reaches no spawn");
  assert.ok(scanFailures(result).length >= bad.length);
});

test("plan: a crate or package directory name with a shell metacharacter is refused and never spawned", async () => {
  for (const name of ["a&b", "a|b", "a^b", "a%b", 'a"b', "a b", "a'b", "a;b", "a`b", "a$b", "a(b"]) {
    const files = { [`crates/${name}/Cargo.toml`]: 'version = "1.0.0"\nlicense = "MIT"\n', [`crates/${name}/src/lib.rs`]: "", [`packages/${name}/package.json`]: '{"license":"MIT","scripts":{"test":"x"}}' };
    const fs = fakeFs(files, files);
    const plan = planScans({ changed: Object.keys(files), contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32" });
    assert.ok(plan.scans.some((s) => s.id.startsWith("path-policy:")), `${name} is refused`);
    for (const s of plan.scans.filter((x) => x.tool === "rustright" || x.tool === "pnpm" || x.tool === "cargo")) {
      assert.ok(!s.args.some((a) => a.includes(name)), `${name} reaches no shell-routed argv (${s.id})`);
    }
    const log = [];
    await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }) }));
    const shellRouted = log.filter((c) => ["rustright", "pnpm", "cargo"].includes(path.basename(c[0])));
    assert.ok(!shellRouted.some((c) => c.slice(1).some((a) => a.includes(name))), `${name} never spawned through rightkit or pnpm`);
  }
});

test("resolveInvocation: Windows .cmd shims take only strictly plain arguments; anything else is refused, not escaped", () => {
  const exists = (p) => ["C:\\bin\\rustright.cmd", "C:\\bin\\pnpm.cmd", "C:\\bin\\tool.exe"].includes(p);
  const env = { Path: "C:\\bin", PATHEXT: ".EXE;.CMD", ComSpec: "C:\\Windows\\System32\\cmd.exe" };
  const inv = (command, args) => resolveInvocation(command, args, { platform: "win32", env, exists, execPath: "node.exe" });
  for (const bad of ["a&b", "a|b", "a^b", "a%b", 'a"b', "a b", "a>b", "a<b", "a;b", "a`b", "a$b", "a(b", "a!b", "*", "a\nb"]) {
    const r = inv("rustright", ["cargo", `crates/${bad}`]);
    assert.ok(r.error, `${JSON.stringify(bad)} refused`);
    assert.match(r.error.message, /refusing/);
    assert.equal(r.file, undefined);
  }
  const ok = inv("rustright", ["cargo", "metadata", "--format-version", "1", "-p", "rightkit-fs", "crates/foo/Cargo.toml"]);
  assert.equal(ok.error, undefined);
  assert.equal(ok.verbatim, true);
  assert.equal(ok.file, "C:\\Windows\\System32\\cmd.exe");
  assert.deepEqual(ok.args.slice(0, 3), ["/d", "/s", "/c"]);
  assert.equal(ok.args[3], '""C:\\bin\\rustright.cmd" cargo metadata --format-version 1 -p rightkit-fs crates/foo/Cargo.toml"');
  // A real executable is never routed through cmd.exe, so its arguments are not shell-parsed.
  assert.deepEqual(inv("tool", ["a&b"]), { file: "C:\\bin\\tool.exe", args: ["a&b"], verbatim: false });
  // A shim whose own path is unsafe is refused.
  assert.ok(resolveInvocation("rustright", ["cargo"], { platform: "win32", env: { Path: "C:\\b&d" }, exists: (p) => p === "C:\\b&d\\rustright.cmd", execPath: "node.exe" }).error);
  // POSIX passes arguments through untouched (spawned without a shell).
  assert.deepEqual(resolveInvocation("rustright", ["cargo", "a&b"], { platform: "linux" }), { file: "rustright", args: ["cargo", "a&b"], verbatim: false });
});

test("runner: class B alone is refused without valid class A evidence, and runs with it", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const log = [];
  const alone = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }), classes: ["B"] }));
  assert.equal(alone.ok, false);
  assert.ok(alone.scans.length > 0 && alone.scans.every((s) => s.class === "B" && s.status === "not-run" && s.code === "CLASS_A_REQUIRED"));
  assert.ok(!log.some((c) => c.includes("clippy")), "no class B command ran");
  // Evidence from a real class A run of the same scan set unlocks class B.
  const a = await runScans(plan, deps({ ...fs, classes: ["A"] }));
  assert.equal(a.ok, true);
  const withEvidence = await runScans(plan, deps({ ...fs, classes: ["B"], priorClassA: a }));
  assert.ok(withEvidence.scans.length > 0 && withEvidence.scans.every((s) => s.status === "pass"), JSON.stringify(scanFailures(withEvidence)));
  // Forged, failed, foreign or mismatched evidence is refused.
  const refused = async (prior) => (await runScans(plan, deps({ ...fs, classes: ["B"], priorClassA: prior }))).scans.every((s) => s.code === "CLASS_A_REQUIRED");
  assert.equal(await refused({ ...a, scans: a.scans.map((s) => ({ ...s, status: "fail" })) }), true);
  assert.equal(await refused({ ...a, classDigests: { ...a.classDigests, A: "0".repeat(64) } }), true);
  assert.equal(await refused({ ...a, scans: a.scans.slice(1) }), true);
  assert.equal(await refused({ ...a, schema: "other/1" }), true);
  // Evidence produced by another checker never unlocks class B: another version, the same version with other bytes (the
  // identity carries a content digest), or no/unknown identity. The identity is name@version#digest, never a bare version.
  assert.match(CHECKER_IDENTITY, /^@rightkit\/release@\d+\.\d+\.\d+(?:[-+][\w.-]+)?#[0-9a-f]{16}$/);
  assert.equal(a.checker, CHECKER_IDENTITY);
  assert.equal(await refused({ ...a, checker: "@rightkit/release@0.0.0#0000000000000000" }), true);
  assert.equal(await refused({ ...a, checker: `${CHECKER_IDENTITY.split("#")[0]}#${"f".repeat(16)}` }), true, "same version, changed checker bytes");
  assert.equal(await refused({ ...a, checker: CHECKER_IDENTITY.split("#")[0] }), true, "version-only identity");
  assert.equal(await refused({ ...a, checker: undefined }), true);
  assert.equal(await refused({ ...a, checker: null }), true);
  // C-1 on the real runner: the evidence rows must be exactly the plan's class A set — a duplicate row, an unknown rule row,
  // or a missing row is refused, and excluding even one class A scan in this call never unlocks class B.
  assert.equal(await refused({ ...a, scans: [...a.scans, a.scans[0]] }), true, "duplicate row");
  assert.equal(await refused({ ...a, scans: [...a.scans, { ...a.scans[0], id: "unknown-rule", command: "internal:unknown" }] }), true, "unknown rule row");
  assert.equal(await refused({ ...a, scans: a.scans.filter((_, i) => i !== 0) }), true, "missing row");
  const oneExcluded = await runScans(plan, deps({ ...fs, exclude: (s) => s.id === plan.scans.find((x) => x.class === "A").id }));
  assert.ok(oneExcluded.scans.filter((s) => s.class === "B").length > 0 && oneExcluded.scans.filter((s) => s.class === "B").every((s) => s.code === "CLASS_A_REQUIRED"), "one excluded class A scan blocks class B");
  const other = planFor(["crates/foo/Cargo.toml"]).plan;
  assert.notEqual(other.classDigests.A, plan.classDigests.A);
  assert.equal((await runScans(other, deps({ ...fakeFs(crateFiles, crateFiles), classes: ["B"], priorClassA: a }))).scans.every((s) => s.code === "CLASS_A_REQUIRED"), true);
});

test("plan: cargoCommand selects the cargo prefix (rustright cargo locally, plain cargo in CI)", () => {
  const ci = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { cargoCommand: ["cargo"] }).plan;
  const cargoScans = ci.scans.filter((s) => s.tool === "cargo");
  assert.ok(["cargo-metadata", "cargo-deny", "cargo-audit", "clippy:foo"].every((id) => cargoScans.some((s) => s.id === id)));
  assert.ok(cargoScans.every((s) => s.args[0] !== "cargo"));
  const PIN = "21d909e4c8bd4e14c7aea58bf980c881a0053ab1";
  assert.equal(cargoScans.find((s) => s.id === "cargo-deny").command, `RK_ADVISORY_DB_COMMIT=${PIN} cargo deny --offline --metadata-path {advisoryMetadata} --config {advisoryDenyConfig} check`);
  assert.equal(cargoScans.find((s) => s.id === "cargo-audit").command, `RK_ADVISORY_DB_COMMIT=${PIN} cargo audit --db {advisoryDb} --no-fetch`);
  assert.ok(!ci.scans.some((s) => s.tool === "rustright"));
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { cargoCommand: ["sh", "-c"] }), /cargoCommand/);
});

test("plan: the docs generator runs only from the trusted checkout, with the content root as data", async () => {
  const trusted = { "scripts/generate-index.mjs": "" };
  const fs = fakeFs({ ...trusted, "a.json": "{}" }, trusted);
  const plan = planScans({ changed: ["a.json", "scripts/generate-index.mjs"], contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32", includeTests: false });
  const docs = plan.scans.find((s) => s.id === "generated-docs");
  assert.equal(docs.class, "B");
  assert.equal(docs.root, "live");
  assert.deepEqual(docs.args, ["scripts/generate-index.mjs", "--check", "--root", "{contentRoot}"]);
  assert.equal(docs.deferred, undefined);
  const calls = [];
  const spawn = (command, args, options) => { calls.push({ command, args, options }); return fakeSpawn()(command, args); };
  const a = await runScans(plan, deps({ ...fs, spawn, classes: ["A"] }));
  await runScans(plan, deps({ ...fs, spawn, classes: ["B"], priorClassA: a }));
  const run = calls.find((c) => c.args[0] === "scripts/generate-index.mjs");
  assert.equal(path.normalize(run.options.cwd), path.normalize(LIVE), "runs in the trusted checkout");
  assert.deepEqual(run.args.slice(1), ["--check", "--root", CONTENT]);
  // No separate trusted checkout: deferred as requiring a trusted-root run, never executed.
  const noTrusted = planScans({ changed: ["scripts/generate-index.mjs"], contentRoot: CONTENT, liveRoot: CONTENT, ...fakeFs(trusted), platform: "win32", includeTests: false });
  const d = noTrusted.scans.find((s) => s.id === "generated-docs");
  assert.match(d.deferred, /trusted-root run/);
  const ran = [];
  const r = await runScans(noTrusted, deps({ ...fakeFs(trusted), spawn: fakeSpawn({ log: ran }), classes: ["A", "B"] }));
  assert.equal(r.scans.find((s) => s.id === "generated-docs").status, "deferred");
  assert.ok(!ran.some((c) => c.includes("--root")), "the generator is never executed (a --check syntax pass is not execution)");
  assert.equal(r.ok, false);
});

test("class A loads the YAML and TOML parsers from the runner's own package, never from the content root", async () => {
  const wf = ".github/workflows/ci.yml";
  const evil = { [wf]: "a: b\n", "scripts/ci/workflow-yaml.mjs": "throw new Error('content-root code executed');", "c.toml": "a = 1\n" };
  const fs = fakeFs(evil, evil);
  const plan = planScans({ changed: [wf, "c.toml"], contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32", includeTests: false });
  const { loadToml, ...rest } = deps({ ...fs, classes: ["A"] });
  const result = await runScans(plan, rest); // default parsers
  assert.equal(result.scans.find((s) => s.id === "yaml-parse").status, "pass", JSON.stringify(result.scans.find((s) => s.id === "yaml-parse")));
  assert.equal(result.scans.find((s) => s.id === "toml-parse").status, "pass", JSON.stringify(result.scans.find((s) => s.id === "toml-parse")));
  // The strict parser really is in force (a tab is rejected), proving the trusted copy ran.
  const badFiles = { [wf]: "a:\n\tb: c\n" };
  const badFs = fakeFs(badFiles, badFiles);
  const badPlan = planScans({ changed: [wf], contentRoot: CONTENT, liveRoot: LIVE, ...badFs, platform: "win32", includeTests: false });
  const bad = await runScans(badPlan, { ...deps({ ...badFs }), classes: ["A"] });
  assert.equal(bad.scans.find((s) => s.id === "yaml-parse").status, "fail");
});

test("class B evidence binds to the input content and to a real class A run", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  assert.match(plan.inputDigest, /^[0-9a-f]{64}$/);
  const a = await runScans(plan, deps({ ...fs, classes: ["A"] }));
  assert.equal(a.inputDigest, plan.inputDigest);
  // Same scan descriptors, different file content: the evidence is refused.
  const changedFs = fakeFs({ ...crateFiles, "crates/foo/src/lib.rs": "pub fn g() {}\n" }, {});
  const other = planScans({ changed: ["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], contentRoot: CONTENT, liveRoot: LIVE, ...changedFs, platform: "win32" });
  assert.equal(other.classDigests.A, plan.classDigests.A);
  assert.notEqual(other.inputDigest, plan.inputDigest);
  const refused = await runScans(other, deps({ ...changedFs, classes: ["B"], priorClassA: a }));
  assert.ok(refused.scans.every((s) => s.code === "CLASS_A_REQUIRED"));
  assert.equal((await runScans(plan, deps({ ...fs, classes: ["B"], priorClassA: { ...a, inputDigest: null } }))).scans.every((s) => s.code === "CLASS_A_REQUIRED"), true);
  assert.equal((await runScans(plan, deps({ ...fs, classes: ["B"], priorClassA: { ...a, classes: ["B"] } }))).scans.every((s) => s.code === "CLASS_A_REQUIRED"), true);
  // Excluding every class A scan does not unlock class B.
  const excluded = await runScans(plan, deps({ ...fs, exclude: (s) => s.class === "A" }));
  assert.ok(excluded.scans.filter((s) => s.class === "B").every((s) => s.code === "CLASS_A_REQUIRED"));
  // A plan with zero class A scans needs explicit prior evidence.
  // (No advisory-exception set in this tree: that scan is the one class A scan every run plans.)
  const swiftOnly = fakeFs({ "swift/a.swift": "" }, { "swift/a.swift": "" });
  const swift = planScans({ changed: ["swift/a.swift"], contentRoot: CONTENT, liveRoot: LIVE, ...swiftOnly, platform: "darwin" });
  assert.equal(swift.scans.filter((s) => s.class === "A").length, 0);
  const swiftFs = fakeFs({ ...crateFiles, "swift/a.swift": "" }, {});
  const bare = await runScans(swift, deps({ ...swiftFs }));
  assert.ok(bare.scans.length > 0 && bare.scans.every((s) => s.code === "CLASS_A_REQUIRED"));
  const evidence = await runScans(swift, deps({ ...swiftFs, classes: ["A"] }));
  const ok = await runScans(swift, deps({ ...swiftFs, classes: ["B"], priorClassA: evidence }));
  assert.ok(ok.scans.every((s) => s.status === "pass"));
});

test("class A pnpm scans disable .pnpmfile hooks and lifecycle scripts", () => {
  const extra = { "packages/p/package.json": '{"license":"MIT"}' };
  const { plan } = planFor(["packages/p/package.json"], extra);
  const install = plan.scans.find((s) => s.id === "pnpm-lockfile");
  assert.deepEqual(install.args, ["install", "--frozen-lockfile", "--lockfile-only", "--ignore-pnpmfile", "--ignore-scripts"]);
  for (const id of ["pnpm-lockfile", "pnpm-audit"]) {
    const env = plan.scans.find((s) => s.id === id).env;
    for (const key of ["npm_config_ignore_pnpmfile", "npm_config_ignore_scripts", "pnpm_config_ignore_pnpmfile", "pnpm_config_ignore_scripts"]) assert.equal(env[key], "true", `${id} ${key}`);
  }
});

test("path containment is physical: a symlink whose real path leaves the content root is refused", async () => {
  const files = { "link/x.mjs": "", "ok.mjs": "" };
  const base = fakeFs(files, files);
  const realpath = (p) => (norm(p).includes("/link/") ? "/elsewhere/x.mjs" : p === CONTENT ? p : p);
  const plan = planScans({ changed: Object.keys(files), contentRoot: CONTENT, liveRoot: LIVE, ...base, realpath, platform: "win32", includeTests: false });
  assert.deepEqual(plan.scans.filter((s) => s.id.startsWith("node-check:")).map((s) => s.id), ["node-check:ok.mjs"]);
  const refused = plan.scans.find((s) => s.id.startsWith("path-policy:"));
  assert.match(refused.detail, /symlink/);
  const log = [];
  const result = await runScans(plan, deps({ ...base, spawn: fakeSpawn({ log }), classes: ["A"] }));
  assert.equal(result.ok, false);
  assert.ok(!log.some((c) => c.some((a) => a.includes("link"))));
  // An unresolvable real path is refused too.
  const broken = planScans({ changed: ["ok.mjs"], contentRoot: CONTENT, liveRoot: LIVE, ...base, realpath: () => { throw new Error("EACCES"); }, platform: "win32", includeTests: false });
  assert.ok(broken.scans.some((s) => s.id.startsWith("path-policy:")) && !broken.scans.some((s) => s.id.startsWith("node-check:")));
});

test("CI runs the deterministic scans on every lock step, with no off switch (#888)", () => {
  const gate = readFileSync(new URL("../../scripts/ci/gate.mjs", import.meta.url), "utf8");
  assert.match(gate, /let scansPassed = false;/);
  assert.match(gate, /if \(STEPS\.has\("scans"\)\) scansPassed = await scansGate\(selection\);/);
  assert.doesNotMatch(gate, /deterministicScansEnabled\(selection\.config\)/);
  assert.doesNotMatch(gate, /deterministic scans: disabled \(not run\)/);
  assert.match(gate, /scanGatePassed\(/);
});

test("feature sets follow CI's gate: featureSets and checkFeatureSets apply independently", () => {
  const crateFeatures = {
    foo: { checkFeatureSets: [["--features", "wv2-0_38"], ["--no-default-features", "--features", "wv2-0_39"]] },
  };
  const { plan } = planFor(["crates/foo/src/lib.rs"], {}, { crateFeatures });
  const args = (id) => plan.scans.find((s) => s.id === id)?.args ?? [];
  assert.ok(!plan.scans.some((s) => s.args.includes("--all-features")), "no --all-features when the crate declares its check sets");
  assert.ok(args("check-features:foo:0").includes("wv2-0_38"));
  assert.ok(args("check-features:foo:1").includes("wv2-0_39"));
  assert.ok(plan.scans.some((s) => s.id === "clippy:foo"), "clippy with default features when no featureSets");
  const db = planFor(["crates/foo/src/lib.rs"], {}, { crateFeatures: { foo: { featureSets: [["--features", "a"], ["--features", "b"]], checkFeatureSets: [["--features", "c"]] } } }).plan;
  for (const id of ["clippy:foo:0", "clippy:foo:1", "check-features:foo:0"]) assert.ok(db.scans.some((s) => s.id === id), id);
  assert.ok(!db.scans.some((s) => s.args.includes("--all-features")));
  const all = planFor(["crates/foo/src/lib.rs"], {}, { crateFeatures: { foo: { featureSets: [["--all-features"]] } } }).plan;
  assert.ok(!all.scans.some((s) => s.id.startsWith("check-features:foo")), "an --all-features lint set needs no separate check");
});

test("vendor/ keeps upstream formatting only for files recorded verbatim in vendor/upstream-files.json", () => {
  const body = "fn  upstream_style() {}\n";
  const digest = crypto.createHash("sha256").update(body).digest("hex");
  const manifest = JSON.stringify({ files: { "vendor/up/src/lib.rs": digest, "vendor/up/src/patched.rs": digest } });
  const extra = { "vendor/upstream-files.json": manifest, "vendor/up/src/lib.rs": body, "vendor/up/src/patched.rs": "fn patched() {}\n", "vendor/up/src/new.rs": body };
  const { plan } = planFor(["vendor/up/src/lib.rs", "vendor/up/src/patched.rs", "vendor/up/src/new.rs"], extra);
  const checked = plan.scans.filter((s) => s.id.startsWith("rustfmt:")).flatMap((s) => s.args);
  assert.ok(!checked.includes("vendor/up/src/lib.rs"), "a verbatim upstream file is exempt");
  assert.ok(checked.includes("vendor/up/src/patched.rs"), "a file whose bytes differ from upstream is checked");
  assert.ok(checked.includes("vendor/up/src/new.rs"), "a file not in the manifest is checked");
});

test("a patched vendor/ file keeps upstream formatting only while its bytes match the recorded patch (rightkit#915)", () => {
  const sha = (text) => crypto.createHash("sha256").update(text).digest("hex");
  const body = "fn  patched_upstream_style() {}\n";
  const record = { base: sha("fn  upstream_style() {}\n"), sha256: sha(body), note: "vendor/up/PATCH.md" };
  const manifest = (patched) => JSON.stringify({ files: {}, patched });
  const checkedWith = (patched, bytes = body) => planFor(["vendor/up/src/lib.rs"], { "vendor/upstream-files.json": manifest(patched), "vendor/up/src/lib.rs": bytes })
    .plan.scans.filter((s) => s.id.startsWith("rustfmt:")).flatMap((s) => s.args);
  assert.ok(!checkedWith({ "vendor/up/src/lib.rs": record }).includes("vendor/up/src/lib.rs"), "a recorded patch is exempt");
  assert.ok(checkedWith({ "vendor/up/src/lib.rs": record }, "fn  edited() {}\n").includes("vendor/up/src/lib.rs"), "an edit after recording is checked");
  for (const broken of [{ ...record, base: undefined }, { ...record, base: "upstream" }, { ...record, note: undefined }, { ...record, note: "../PATCH.md" }, { ...record, sha256: undefined }]) {
    assert.ok(checkedWith({ "vendor/up/src/lib.rs": broken }).includes("vendor/up/src/lib.rs"), `an incomplete record is checked: ${JSON.stringify(broken)}`);
  }
  const outside = planFor(["crates/foo/src/lib.rs"], { "vendor/upstream-files.json": manifest({ "crates/foo/src/lib.rs": { ...record, sha256: sha(crateFiles["crates/foo/src/lib.rs"]) } }) })
    .plan.scans.filter((s) => s.id.startsWith("rustfmt:")).flatMap((s) => s.args);
  assert.ok(outside.includes("crates/foo/src/lib.rs"), "a patch record outside vendor/ exempts nothing");
});

test("generated Rust (bindgen output or an @generated marker in the first five lines) is not rustfmt-checked", () => {
  const files = {
    "crates/foo/bindings-a.rs": "/* automatically generated by rust-bindgen 0.72.1 */\npub const  X: u32 = 1;\n",
    "crates/foo/bindings-b.rs": "// stamp\n// Generated by rust-bindgen. Regenerate: see VERSIONS.md.\n/* automatically generated by rust-bindgen 0.72.1 */\n",
    "crates/foo/src/gen.rs": "// @generated by build tooling\npub fn  g() {}\n",
    "crates/foo/src/late.rs": "//\n//\n//\n//\n//\n// @generated\npub fn  l() {}\n",
    "crates/foo/src/hand.rs": "// generated code lives elsewhere\npub fn  h() {}\n",
  };
  const { plan } = planFor(Object.keys(files), files);
  const checked = plan.scans.filter((s) => s.id.startsWith("rustfmt:")).flatMap((s) => s.args);
  for (const f of ["crates/foo/bindings-a.rs", "crates/foo/bindings-b.rs", "crates/foo/src/gen.rs"]) assert.ok(!checked.includes(f), `${f} is generated`);
  assert.ok(checked.includes("crates/foo/src/late.rs"), "a marker after the fifth line does not count");
  assert.ok(checked.includes("crates/foo/src/hand.rs"), "the word 'generated' alone does not count");
});

// rustfmt runs each file under the edition its crate declares; a crate that inherits it (`edition.workspace = true`) takes the owning
// workspace root's [workspace.package] edition (Pulse core/examples/chat_vectors.rs uses let-chains, Ember has 40 such manifests).
const rustfmtEditions = (plan) => Object.fromEntries(plan.scans.filter((s) => s.id.startsWith("rustfmt:")).map((s) => [s.id.slice("rustfmt:".length), s.args.filter((a) => a.endsWith(".rs"))]));
const editionRefusals = (plan) => plan.scans.filter((s) => s.internal === "refuse" && /rustfmt edition/.test(s.detail));

test("rustfmt edition: edition.workspace = true resolves through the owning workspace root, in both manifest spellings", () => {
  const files = {
    "Cargo.toml": '[workspace]\nmembers = ["core", "tools/*"]\n\n[workspace.package]\nversion = "1.0.0"\nedition = "2024"\n',
    "core/Cargo.toml": '[package]\nname = "core"\nedition.workspace = true\n',
    "core/examples/chat_vectors.rs": "fn main() { if let Some(a) = b() && a {} }\n",
    "tools/dotted/Cargo.toml": '[package]\nname = "dotted"\nedition.workspace = true # inherited\n',
    "tools/dotted/src/lib.rs": "pub fn d() {}\n",
    "tools/inline/Cargo.toml": '[package]\nname = "inline"\nedition = { workspace = true }\n',
    "tools/inline/src/lib.rs": "pub fn i() {}\n",
    "tools/literal/Cargo.toml": '[package]\nname = "literal"\nedition = "2018"\n',
    "tools/literal/src/lib.rs": "pub fn l() {}\n",
  };
  const { plan } = planFor(Object.keys(files).filter((f) => f.endsWith(".rs")), files);
  assert.deepEqual(rustfmtEditions(plan), {
    2018: ["tools/literal/src/lib.rs"],
    2024: ["core/examples/chat_vectors.rs", "tools/dotted/src/lib.rs", "tools/inline/src/lib.rs"],
  }, "inherited editions are the workspace's, a literal edition is the crate's own");
  assert.deepEqual(editionRefusals(plan), []);
});

test("rustfmt edition: the nearest workspace root owns the crate, and a root package that is its own workspace inherits from itself", () => {
  const files = {
    "Cargo.toml": '[workspace]\nmembers = ["a"]\n[workspace.package]\nedition = "2024"\n',
    "apps/inner/Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nedition = "2021"\n',
    "apps/inner/crates/m/Cargo.toml": '[package]\nname = "m"\nedition.workspace = true\n',
    "apps/inner/crates/m/src/lib.rs": "pub fn m() {}\n",
    "solo/Cargo.toml": '[package]\nname = "solo"\nedition.workspace = true\n[workspace]\n[workspace.package]\nedition = "2018"\n',
    "solo/src/lib.rs": "pub fn s() {}\n",
    // A key under another table is not the package's edition, and a [package.metadata] edition does not hide the inheritance.
    "meta/Cargo.toml": '[package]\nname = "meta"\nedition.workspace = true\n[package.metadata.docs]\nedition = "2015"\n[dependencies.x]\nedition = "2015"\n',
    "meta/src/lib.rs": "pub fn t() {}\n",
  };
  const { plan } = planFor(Object.keys(files).filter((f) => f.endsWith(".rs")), files);
  assert.deepEqual(rustfmtEditions(plan), { 2018: ["solo/src/lib.rs"], 2021: ["apps/inner/crates/m/src/lib.rs"], 2024: ["meta/src/lib.rs"] });
});

test("rustfmt edition: an inheritance that cannot be followed fails the gate with a finding and is never rustfmt-checked under 2021", async () => {
  const files = {
    // No workspace anywhere above the crate.
    "lone/Cargo.toml": '[package]\nname = "lone"\nedition.workspace = true\n',
    "lone/src/lib.rs": "pub fn l() {}\n",
    "lone/src/more.rs": "pub fn m() {}\n",
    // A workspace root that declares no [workspace.package] edition, and one whose edition is not a literal year.
    "ws/Cargo.toml": '[workspace]\nmembers = ["noedition"]\n[workspace.package]\nversion = "1.0.0"\n',
    "ws/noedition/Cargo.toml": '[package]\nname = "noedition"\nedition = { workspace = true }\n',
    "ws/noedition/src/lib.rs": "pub fn n() {}\n",
    "ok/Cargo.toml": '[package]\nname = "ok"\nedition = "2021"\n',
    "ok/src/lib.rs": "pub fn o() {}\n",
  };
  const { fs, plan } = planFor(Object.keys(files).filter((f) => f.endsWith(".rs")), files);
  assert.deepEqual(rustfmtEditions(plan), { 2021: ["ok/src/lib.rs"] }, "the unresolvable crates' files are not checked under a guessed edition");
  const refusals = editionRefusals(plan);
  assert.equal(refusals.length, 2, "one finding per manifest, not per file");
  assert.ok(refusals.every((r) => r.class === "A" && r.id.startsWith("path-policy:")));
  assert.match(refusals.map((r) => r.detail).join("\n"), /rustfmt edition of lone\/Cargo\.toml: lone\/Cargo\.toml sets edition\.workspace = true, but no Cargo\.toml at or above it declares a \[workspace\]/);
  assert.match(refusals.map((r) => r.detail).join("\n"), /rustfmt edition of ws\/noedition\/Cargo\.toml: .*workspace root ws\/Cargo\.toml declares no literal \[workspace\.package\] edition/);
  const result = await runScans(plan, { ...deps({ ...fs }), classes: ["A"] });
  assert.equal(result.ok, false, "the gate fails on an edition it cannot resolve");
  const failed = scanFailures(result).join("\n");
  assert.match(failed, /lone\/Cargo\.toml/);
  assert.match(failed, /ws\/Cargo\.toml/);
});

test("rustfmt edition: an edition entry in a spelling the scan does not recognise is a finding, never a skipped line", () => {
  const files = {
    "Cargo.toml": '[workspace]\n[workspace.package]\nedition = 2024\n',
    "num/Cargo.toml": '[package]\nname = "num"\nedition = 2021\n',
    "num/src/lib.rs": "pub fn n() {}\n",
    "inherit/Cargo.toml": '[package]\nname = "inherit"\nedition.workspace = true\n',
    "inherit/src/lib.rs": "pub fn i() {}\n",
    "quoted/Cargo.toml": '[package]\nname = "quoted"\n"edition" = "2018"\n',
    "quoted/src/lib.rs": "pub fn q() {}\n",
    "off/Cargo.toml": '[package]\nname = "off"\nedition.workspace = false\n',
    "off/src/lib.rs": "pub fn o() {}\n",
  };
  const { plan } = planFor(Object.keys(files).filter((f) => f.endsWith(".rs")), files);
  assert.deepEqual(rustfmtEditions(plan), { 2018: ["quoted/src/lib.rs"] }, "a quoted key is the same key");
  const details = editionRefusals(plan).map((r) => r.detail).join("\n");
  assert.match(details, /rustfmt edition of num\/Cargo\.toml: num\/Cargo\.toml has an edition entry the scan cannot read \(edition = 2021\)/);
  assert.match(details, /rustfmt edition of off\/Cargo\.toml: off\/Cargo\.toml has an edition entry the scan cannot read \(edition\.workspace = false\)/);
  assert.match(details, /rustfmt edition of inherit\/Cargo\.toml: inherit\/Cargo\.toml sets edition\.workspace = true, but its workspace root Cargo\.toml has a \[workspace\.package\] edition entry the scan cannot read \(edition = 2024\)/);
  assert.equal(editionRefusals(plan).length, 3);
});

test("rustfmt edition: an unreadable manifest in the chain is a finding, not a default", () => {
  const files = {
    "Cargo.toml": '[workspace]\n[workspace.package]\nedition = "2024"\n',
    "a/Cargo.toml": '[package]\nname = "a"\nedition.workspace = true\n',
    "a/src/lib.rs": "pub fn a() {}\n",
    "b/Cargo.toml": '[package]\nname = "b"\nedition = "2021"\n',
    "b/src/lib.rs": "pub fn b() {}\n",
  };
  const base = fakeFs(files, files);
  const unreadable = (target) => ({ ...base, readFile: (p) => { if (norm(p) === norm(path.join(CONTENT, target))) throw new Error("EACCES: permission denied"); return base.readFile(p); } });
  const member = planScans({ changed: ["a/src/lib.rs"], contentRoot: CONTENT, liveRoot: LIVE, ...unreadable("a/Cargo.toml"), platform: "linux" });
  assert.deepEqual(rustfmtEditions(member), {});
  assert.match(editionRefusals(member)[0].detail, /a\/Cargo\.toml cannot be read \(EACCES/);
  const root = planScans({ changed: ["a/src/lib.rs", "b/src/lib.rs"], contentRoot: CONTENT, liveRoot: LIVE, ...unreadable("Cargo.toml"), platform: "linux" });
  assert.deepEqual(rustfmtEditions(root), { 2021: ["b/src/lib.rs"] });
  assert.match(editionRefusals(root)[0].detail, /sets edition\.workspace = true, but Cargo\.toml cannot be read \(EACCES/);
});

test("a crate with no default-feature clippy set keeps its default check", () => {
  const { plan } = planFor(["crates/foo/src/lib.rs"], {}, { crateFeatures: { foo: { featureSets: [["--features", "x"]], checkFeatureSets: [] } } });
  assert.ok(plan.scans.some((s) => s.id === "check:foo"));
});

test("rustScope narrows a lock-only workspace change to the crates the lock graph reaches", () => {
  const extra = { "Cargo.lock": "version = 4\n", "crates/bar/Cargo.toml": crateFiles["crates/foo/Cargo.toml"], "crates/bar/src/lib.rs": "pub fn g() {}\n" };
  const { plan } = planFor(["Cargo.lock"], extra, { listDir: () => ["foo", "bar"], rustScope: new Set(["bar"]) });
  assert.ok(plan.scans.some((s) => s.id === "clippy:bar"));
  assert.ok(!plan.scans.some((s) => s.id === "clippy:foo"));
});

test("jobs > 1 runs class B side by side by lane, reports each result, and keeps plan order in the result", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs"]);
  let running = 0; let peak = 0; const seen = [];
  const spawn = async (cmd, args) => {
    running += 1; peak = Math.max(peak, running);
    await new Promise((r) => setTimeout(r, 5));
    running -= 1;
    return { status: 0, stdout: "ok 1.0.0", stderr: "" };
  };
  const result = await runScans(plan, { ...deps({ ...fs, spawn }), jobs: 3, onResult: (row, p) => seen.push([row.id, p.done, p.total]) });
  assert.equal(seen.length, plan.scans.length);
  assert.deepEqual(result.scans.map((r) => r.id), [...plan.scans.filter((s) => s.class === "A"), ...plan.scans.filter((s) => s.class === "B")].map((s) => s.id));
  assert.ok(peak > 1, "class B scans overlapped");
});

// ---- repository-specific units, scoping, reverse dependents and cancellation (rightkit-gate) ------------------

import { defaultSpawnAsync, killProcessTree, laneOf } from "./deterministic-scans.mjs";
import { parseLock } from "./lock-graph.mjs";

const manifest = 'edition = "2024"\nversion = "1.0.0"\nlicense = "MIT"\n';
const unitFiles = {
  "core/Cargo.toml": manifest, "core/CHANGELOG.md": "## 1.0.0\n", "core/src/lib.rs": "pub fn c() {}\n",
  "apps/ui/Cargo.toml": manifest, "apps/ui/src/main.rs": "fn main() {}\n",
};
const coreUi = [{ name: "app-core", dir: "core" }, { name: "app-ui", dir: "apps/ui" }];
const scanOf = (plan, id) => plan.scans.find((s) => s.id === id);

test("rustUnits: a changed file maps to the unit whose directory holds it, and every path uses that directory", () => {
  const { plan } = planFor(["core/src/lib.rs"], unitFiles, { rustUnits: coreUi, npmUnits: [] });
  const b = ids(plan, "B");
  for (const id of ["clippy:app-core", "doc:app-core", "test:rust:app-core"]) assert.ok(b.includes(id), id);
  assert.ok(!plan.scans.some((s) => s.id.includes("app-ui") || s.id.includes("foo")), "no other unit, no crates/ layout");
  assert.deepEqual(scanOf(plan, "license").files, ["core/Cargo.toml"]);
  assert.deepEqual(scanOf(plan, "changelog-version:app-core").files, ["core/Cargo.toml", "core/CHANGELOG.md"]);
  assert.deepEqual(scanOf(plan, "secret-scan").dirs, ["core"]);
});

test("rustUnits: a workspace-wide change uses the full list, narrowed by rustScope", () => {
  const all = planFor(["Cargo.lock"], unitFiles, { rustUnits: coreUi, npmUnits: [] }).plan;
  for (const name of ["app-core", "app-ui"]) assert.ok(all.scans.some((s) => s.id === `clippy:${name}`), name);
  const narrowed = planFor(["Cargo.lock"], unitFiles, { rustUnits: coreUi, npmUnits: [], rustScope: new Set(["app-ui"]) }).plan;
  assert.ok(narrowed.scans.some((s) => s.id === "clippy:app-ui"));
  assert.ok(!narrowed.scans.some((s) => s.id === "clippy:app-core"));
});

test("rustUnits: the deepest directory wins; a root unit takes only what no other unit holds", () => {
  const nested = { "x/Cargo.toml": manifest, "x/y/Cargo.toml": manifest, "x/y/src/lib.rs": "" };
  const a = planFor(["x/y/src/lib.rs"], nested, { rustUnits: [{ name: "outer", dir: "x" }, { name: "inner", dir: "x/y" }], npmUnits: [] }).plan;
  assert.ok(a.scans.some((s) => s.id === "clippy:inner") && !a.scans.some((s) => s.id === "clippy:outer"));
  const rooted = { "Cargo.toml": manifest, "src/lib.rs": "", "sub/Cargo.toml": manifest, "sub/src/lib.rs": "" };
  const units = [{ name: "root", dir: "." }, { name: "sub", dir: "sub" }];
  const sub = planFor(["sub/src/lib.rs"], rooted, { rustUnits: units, npmUnits: [] }).plan;
  assert.ok(sub.scans.some((s) => s.id === "clippy:sub") && !sub.scans.some((s) => s.id === "clippy:root"));
  const root = planFor(["src/lib.rs"], rooted, { rustUnits: units, npmUnits: [] }).plan;
  assert.ok(root.scans.some((s) => s.id === "clippy:root") && !root.scans.some((s) => s.id === "clippy:sub"));
  assert.deepEqual(scanOf(root, "license").files, ["Cargo.toml"]);
  assert.deepEqual(scanOf(root, "secret-scan").dirs, ["."]);
});

test("rustUnits: publish false skips only the package list; a bad directory is a path-policy refusal; malformed input throws", () => {
  const p = planFor(["core/src/lib.rs"], unitFiles, { rustUnits: [{ name: "app-core", dir: "core", publish: false }], npmUnits: [], tier: "release" }).plan;
  assert.ok(!p.scans.some((s) => s.id.startsWith("cargo-package-list")), "publish: false plans no package list even in the release tier");
  const published = planFor(["core/src/lib.rs"], unitFiles, { rustUnits: [{ name: "app-core", dir: "core" }], npmUnits: [], tier: "release" }).plan;
  assert.ok(published.scans.some((s) => s.id === "cargo-package-list:app-core"), "a published crate plans it in the release tier");
  assert.ok(p.scans.some((s) => s.id === "license") && p.scans.some((s) => s.id === "clippy:app-core"));
  for (const dir of ["a b", "../x", "/abs", "a/../../b", "a&b"]) {
    const bad = planFor(["README.md"], { "README.md": "x" }, { rustUnits: [{ name: "x", dir }], npmUnits: [] }).plan;
    assert.ok(bad.scans.some((s) => s.id.startsWith("path-policy:") && /not a plain repository-relative path/.test(s.detail)), dir);
  }
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { rustUnits: "crates" }), /array/);
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { rustUnits: [{ name: "a" }] }), /name and dir/);
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { rustUnits: [{ name: "a", dir: "a" }, { name: "a", dir: "b" }] }), /duplicate/);
});

test("rustUnits equal to the crates/ layout plan exactly what the default plan does", () => {
  const changed = ["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"];
  const dflt = planFor(changed).plan;
  const units = planFor(changed, {}, { rustUnits: [{ name: "foo", dir: "crates/foo" }], npmUnits: [] }).plan;
  assert.deepEqual(units.scans.map((s) => [s.id, s.command, s.class]), dflt.scans.map((s) => [s.id, s.command, s.class]));
  assert.equal(units.scanSetDigest, dflt.scanSetDigest);
});

test("npmUnits: units live anywhere, and tsc, eslint, tests, license use their directory", () => {
  const files = {
    "apps/web/package.json": '{"name":"web","license":"MIT","scripts":{"test":"x"}}', "apps/web/tsconfig.json": "{}", "apps/web/eslint.config.mjs": "", "apps/web/src/a.ts": "",
  };
  const { plan } = planFor(["apps/web/src/a.ts"], files, { rustUnits: [], npmUnits: [{ name: "web", dir: "apps/web" }] });
  assert.equal(scanOf(plan, "tsc:web").cwd, "apps/web");
  assert.equal(scanOf(plan, "eslint:web").cwd, "apps/web");
  assert.deepEqual(scanOf(plan, "test:npm:web").args, ["--dir", "apps/web", "run", "test"]);
  assert.deepEqual(scanOf(plan, "license").files, ["apps/web/package.json"]);
  assert.ok(!plan.scans.some((s) => /clippy|cargo/.test(s.id)));
  const wide = planFor(["package.json"], { ...files, "package.json": "{}" }, { rustUnits: [], npmUnits: [{ name: "web", dir: "apps/web" }] }).plan;
  assert.ok(wide.scans.some((s) => s.id === "tsc:web"), "a root pnpm file change puts every unit in scope");
});

test("cargoDir: cargo scans run in the workspace directory and its lock and config files are the workspace-wide ones", () => {
  const files = { "ws/core/Cargo.toml": manifest, "ws/core/src/lib.rs": "", "ws/ui/Cargo.toml": manifest, "ws/ui/src/lib.rs": "", "ws/Cargo.lock": "version = 4\n" };
  const opts = { rustUnits: [{ name: "a", dir: "ws/core" }, { name: "b", dir: "ws/ui" }], npmUnits: [], cargoDir: "ws" };
  const one = planFor(["ws/core/src/lib.rs"], files, opts).plan;
  assert.ok(one.scans.some((s) => s.id === "clippy:a") && !one.scans.some((s) => s.id === "clippy:b"));
  for (const s of one.scans.filter((x) => x.tool === "rustright")) assert.equal(s.cwd, "ws", s.id);
  const lock = planFor(["ws/Cargo.lock"], files, opts).plan;
  assert.ok(lock.scans.some((s) => s.id === "clippy:a") && lock.scans.some((s) => s.id === "clippy:b"));
  const stray = planFor(["Cargo.lock"], files, opts).plan;
  assert.ok(!stray.scans.some((s) => s.id.startsWith("clippy:")), "a lock outside the workspace is not workspace-wide for it");
  assert.ok(planFor(["crates/foo/src/lib.rs"]).plan.scans.every((s) => s.cwd === undefined), "no cwd by default");
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { cargoDir: "../x" }), /cargoDir/);
});

test("generated-docs runs only when a changed file is an input of scripts/generate-index.mjs", () => {
  const extra = {
    "scripts/generate-index.mjs": "", "scripts/generate-port-matrix.mjs": "", "Cargo.toml": "[workspace]\n", "INDEX.md": "x", "README.md": "x", "docs/ADOPTION.md": "x", "docs/CAPABILITY-MAP.md": "x",
    "docs/PORT-MATRIX.md": "x", "docs/port-matrix.html": "x", "docs/port-matrix.json": "{}",
    "crates/foo/README.md": "x", "packages/p/README.md": "x", "packages/p/package.json": '{"license":"MIT"}', "packages/p/src/a.ts": "",
    "packages/release/rightkit-versions.json": "{}", "packages/release/rightkit-ownership.json": "{}", "packages/release/ownership-availability.mjs": "",
  };
  const has = (f) => planFor([f], extra).plan.scans.some((s) => s.id === "generated-docs");
  for (const f of ["crates/foo/Cargo.toml", "crates/foo/README.md", "packages/p/package.json", "packages/p/README.md", "Cargo.toml", "packages/release/rightkit-versions.json",
    "packages/release/rightkit-ownership.json", "packages/release/ownership-availability.mjs", "scripts/generate-index.mjs", "scripts/generate-port-matrix.mjs", "INDEX.md", "docs/ADOPTION.md",
    "docs/CAPABILITY-MAP.md", "docs/PORT-MATRIX.md", "docs/port-matrix.html", "docs/port-matrix.json"]) {
    assert.ok(has(f), `${f} is a generator input`);
  }
  for (const f of ["crates/foo/src/lib.rs", "crates/foo/CHANGELOG.md", "packages/p/src/a.ts", "README.md"]) assert.ok(!has(f), `${f} is not`);
});

test("scripts/ci tests run when scripts/, ci/ or .github/ changed, or a file the scripts/ci sources reference changed", () => {
  const extra = {
    "scripts/ci/a.test.mjs": "", "scripts/ci/x.mjs": 'import "../../packages/release/foo.mjs";\nconst u = new URL("../../packages/logs/package.json", import.meta.url);\n',
    "packages/release/foo.mjs": 'import "./bar.mjs";\n', "packages/release/bar.mjs": "", "packages/release/unrelated.mjs": "", "packages/logs/package.json": '{"license":"MIT"}',
    "scripts/other.mjs": "", ".github/workflows/ci.yml": "a: b", "ci/gate.config.json": "{}", "README.md": "x",
  };
  const listDir = (d) => (norm(d).endsWith("/scripts/ci") ? ["a.test.mjs", "x.mjs"] : []);
  const has = (f) => planFor([f], extra, { listDir }).plan.scans.some((s) => s.id === "test:scripts-ci");
  for (const f of ["scripts/ci/x.mjs", "scripts/other.mjs", ".github/workflows/ci.yml", "ci/gate.config.json", "packages/release/foo.mjs", "packages/release/bar.mjs", "packages/logs/package.json"]) assert.ok(has(f), f);
  for (const f of ["crates/foo/src/lib.rs", "packages/release/unrelated.mjs", "README.md"]) assert.ok(!has(f), f);
  // Without scripts/ci: the RightKit default still plans them for a scripts/ change; unit mode does not.
  assert.ok(planFor(["scripts/x.mjs"], { "scripts/x.mjs": "" }).plan.scans.some((s) => s.id === "test:scripts-ci"));
  assert.ok(!planFor(["scripts/x.mjs"], { "scripts/x.mjs": "" }, { rustUnits: [], npmUnits: [] }).plan.scans.some((s) => s.id === "test:scripts-ci"));
});

test("reverse dependents: a crate that depends on one in scope gets a check-dependent scan in the compile lane", () => {
  const lock = parseLock('[[package]]\nname = "foo"\nversion = "1.2.3"\n\n[[package]]\nname = "bar"\nversion = "1.0.0"\ndependencies = [\n "foo",\n]\n\n[[package]]\nname = "baz"\nversion = "1.0.0"\ndependencies = [\n "bar 1.0.0",\n]\n\n[[package]]\nname = "qux"\nversion = "1.0.0"\n');
  const extra = Object.fromEntries(["bar", "baz", "qux"].flatMap((n) => [[`crates/${n}/Cargo.toml`, manifest], [`crates/${n}/src/lib.rs`, ""]]));
  const listDir = () => ["foo", "bar", "baz", "qux"];
  const dep = (changed, opts = {}) => planFor(changed, extra, { listDir, rustLock: lock, ...opts }).plan;
  const p = dep(["crates/foo/src/lib.rs"]);
  assert.deepEqual(ids(p).filter((i) => i.startsWith("check-dependent:")), ["check-dependent:bar", "check-dependent:baz"]);
  assert.deepEqual(scanOf(p, "check-dependent:bar").args.slice(-5), ["check", "-p", "bar", "--all-targets", "--locked"]);
  assert.equal(scanOf(p, "check-dependent:bar").class, "B");
  assert.equal(scanClassOf("check-dependent:bar"), "B");
  assert.equal(laneOf({ id: "check-dependent:bar" }), "compile");
  assert.ok(!p.scans.some((s) => s.id === "clippy:bar"), "a dependent is compiled, not linted or tested");
  // A dependent that is itself in scope is not repeated; an unreachable crate is never a dependent.
  assert.deepEqual(ids(dep(["crates/foo/src/lib.rs", "crates/bar/src/lib.rs"])).filter((i) => i.startsWith("check-dependent:")), ["check-dependent:baz"]);
  assert.deepEqual(ids(planFor(["crates/foo/src/lib.rs"], extra, { listDir }).plan).filter((i) => i.startsWith("check-dependent:")), [], "no lock graph, none planned");
  // The same through units.
  const units = ["foo", "bar", "baz", "qux"].map((name) => ({ name, dir: `crates/${name}` }));
  assert.deepEqual(ids(dep(["crates/foo/src/lib.rs"], { rustUnits: units, npmUnits: [] })).filter((i) => i.startsWith("check-dependent:")), ["check-dependent:bar", "check-dependent:baz"]);
  // Without a compile grant a dependent check is deferred like every other broker scan.
  assert.ok(dep(["crates/foo/src/lib.rs"], { localCompile: false }).deferred.includes("check-dependent:bar"));
});

test("cancellation: an aborted signal stops scheduling, reaches the running scan, and leaves every unfinished scan not-run", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const ac = new AbortController(); const log = []; const signals = [];
  const spawn = async (command, args, options) => {
    log.push([command, ...args]);
    if (args.includes("deny")) {
      signals.push(options.signal);
      setTimeout(() => ac.abort(), 5);
      await new Promise((r) => options.signal.addEventListener("abort", r, { once: true }));
      return { status: null, stdout: "", stderr: "", error: new Error("cancelled") };
    }
    return fakeSpawn()(command, args);
  };
  const result = await runScans(plan, { ...deps({ ...fs, spawn }), signal: ac.signal });
  assert.deepEqual(signals, [ac.signal]);
  assert.equal(result.cancelled, true);
  assert.equal(result.ok, false);
  const row = (id) => result.scans.find((s) => s.id === id);
  assert.equal(row("cargo-deny").status, "not-run");
  assert.equal(row("cargo-deny").code, "CANCELLED");
  assert.equal(row("cargo-audit").code, "CANCELLED");
  assert.ok(result.scans.filter((s) => s.class === "B").every((s) => s.code === "CANCELLED"));
  assert.equal(result.scans.length, plan.scans.length, "every scan is listed");
  assert.ok(!log.some((c) => c.includes("clippy") || c.includes("audit")), "nothing started after the abort");
  assert.ok(scanFailures(result).some((l) => l.startsWith("cargo-audit") && /not run/.test(l)));
  // Not cancelled: the result shape is unchanged.
  const normal = await runScans(plan, deps({ ...fs }));
  assert.equal("cancelled" in normal, false);
});

test("cancellation: defaultSpawnAsync kills a running child on abort and never starts one when already aborted", async () => {
  const ac = new AbortController();
  const started = Date.now();
  const running = defaultSpawnAsync(process.execPath, ["-e", "setTimeout(() => {}, 60000)"], { signal: ac.signal });
  setTimeout(() => ac.abort(), 300);
  const r = await running;
  assert.equal(r.cancelled, true);
  assert.ok(r.error);
  assert.ok(Date.now() - started < 30000, "the child did not run to its 60 s timer");
  const pre = await defaultSpawnAsync(process.execPath, ["-e", "process.exit(0)"], { signal: ac.signal });
  assert.equal(pre.cancelled, true);
  assert.equal(pre.status, null);
});

test("killProcessTree: taskkill /T /F on Windows, the process group elsewhere", () => {
  const calls = [];
  killProcessTree({ pid: 123 }, { platform: "win32", run: (...a) => calls.push(a) });
  assert.deepEqual(calls[0].slice(0, 2), ["taskkill", ["/T", "/F", "/PID", "123"]]);
  const sent = [];
  killProcessTree({ pid: 77 }, { platform: "linux", kill: (pid, sig) => sent.push([pid, sig]) });
  assert.deepEqual(sent, [[-77, "SIGKILL"]]);
  let direct = null;
  killProcessTree({ pid: 78, kill: (sig) => { direct = sig; } }, { platform: "linux", kill: () => { throw new Error("ESRCH"); } });
  assert.equal(direct, "SIGKILL", "falls back to the child itself");
  killProcessTree({}, { platform: "win32", run: () => { throw new Error("no pid: not called"); } });
});

// ---- gate result cache ----------------------------------------------------------------------------------------

function memCache(digest) {
  const store = new Map();
  return { store, digest, read: (k) => store.get(k) ?? null, write: (k, row) => store.set(k, { key: k, at: "2026-10-22T10:00:00.000Z", row }) };
}
const bRows = (r) => r.scans.filter((s) => s.class === "B");
const spawned = (log, word) => log.filter((l) => l.some((a) => String(a).includes(word)) && !l.some((a) => String(a).includes("version"))).length;

test("cache: identical inputs hit; every pass with a digest is stored, class A included; the hit is reported as a pass; a null digest opts out", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  // The caller's digest decides: here the live checks (deny, audit) opt out, everything else is cacheable.
  const cache = memCache((s) => (/^(cargo-deny|cargo-audit)/.test(s.id) ? null : "digest-1"));
  const first = await runScans(plan, deps({ ...fs, cache }));
  assert.equal(first.ok, true, JSON.stringify(scanFailures(first)));
  assert.deepEqual(first.cacheHits, []);
  const cacheable = (r) => r.scans.filter((s) => s.status === "pass" && !s.internal && !/^(cargo-deny|cargo-audit)/.test(s.id) && plan.scans.find((p) => p.id === s.id && !p.internal));
  assert.equal(cache.store.size, cacheable(first).length, "one entry per tool-run pass with a digest, class A included");
  assert.ok(cacheable(first).some((s) => s.class === "A"), "class A tool scans are among them");
  const log = [];
  const second = await runScans(plan, deps({ ...fs, cache, spawn: fakeSpawn({ log }) }));
  assert.equal(second.ok, true);
  assert.deepEqual(second.cacheHits, cacheable(second).map((r) => r.id).sort());
  assert.ok(bRows(second).every((r) => r.status === "pass" && r.cached === true && r.detail === "cached 2026-10-22"));
  assert.equal(spawned(log, "clippy") + spawned(log, "test"), 0, "no cached scan was spawned");
  assert.equal(second.scanSetDigest, plan.scanSetDigest);
  assert.ok(second.scans.filter((r) => /^(cargo-deny|cargo-audit)/.test(r.id)).every((r) => !r.cached), "the live checks ran again");
  assert.ok(spawned(log, "deny") > 0 && spawned(log, "audit") > 0, "deny and audit were spawned again");
});

test("cache: a changed input digest (a closure member's tree id) is a miss", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs"]);
  let member = "tree-1";
  const cache = memCache(() => `foo+${member}`);
  await runScans(plan, deps({ ...fs, cache }));
  member = "tree-2";
  const log = [];
  const again = await runScans(plan, deps({ ...fs, cache, spawn: fakeSpawn({ log }) }));
  assert.deepEqual(again.cacheHits, []);
  assert.ok(spawned(log, "clippy") > 0, "the scan reran");
});

test("cache: a changed tool version is a miss", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs"]);
  const cache = memCache(() => "d");
  await runScans(plan, deps({ ...fs, cache }));
  const newer = (command, args) => (args.some((a) => String(a).includes("version")) ? { status: 0, stdout: "tool 2.0.0\n", stderr: "" } : { status: 0, stdout: "", stderr: "" });
  const again = await runScans(plan, deps({ ...fs, cache, spawn: newer }));
  assert.deepEqual(again.cacheHits, []);
  assert.ok(bRows(again).every((r) => r.status === "pass" && !r.cached));
});

test("cache: a changed command is a miss", async () => {
  const one = planFor(["crates/foo/src/lib.rs"]);
  const cache = memCache(() => "d");
  await runScans(one.plan, deps({ ...one.fs, cache }));
  const two = planFor(["crates/foo/src/lib.rs"], {}, { crateFeatures: { foo: { featureSets: [["--features", "x"]] } } });
  assert.notEqual(one.plan.scans.find((s) => s.id === "clippy:foo").command, two.plan.scans.find((s) => s.id === "clippy:foo").command);
  const again = await runScans(two.plan, deps({ ...two.fs, cache }));
  assert.ok(!again.cacheHits.includes("clippy:foo"));
});

test("cache: a failure is never cached, and a null digest is never cached", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs"]);
  const cache = memCache(() => "d");
  const bad = await runScans(plan, deps({ ...fs, cache, spawn: fakeSpawn({ failing: ["clippy"] }) }));
  assert.equal(bad.ok, false);
  assert.ok(![...cache.store.values()].some((e) => e.row.id === "clippy:foo"), "the failing scan left no entry");
  assert.ok(![...cache.store.values()].some((e) => e.row.status !== "pass"));
  const log = [];
  const good = await runScans(plan, deps({ ...fs, cache, spawn: fakeSpawn({ log }) }));
  assert.ok(!good.cacheHits.includes("clippy:foo"));
  assert.ok(spawned(log, "clippy") > 0);
  const none = memCache(() => null);
  await runScans(plan, deps({ ...fs, cache: none }));
  assert.equal(none.store.size, 0);
});

test("cache: a corrupt or foreign entry is a miss; the key covers id, command, tool version, env and digest", async () => {
  const base = { id: "clippy:a", command: "c", toolVersion: "1", env: {}, inheritedEnv: inheritedEnvDigest({ CI: "false" }), digest: "d" };
  const key = scanCacheKey(base);
  assert.match(key, /^[0-9a-f]{64}$/);
  for (const change of [{ id: "clippy:b" }, { command: "c2" }, { toolVersion: "2" }, { env: { A: "1" } }, { inheritedEnv: inheritedEnvDigest({ CI: "true" }) }, { digest: "d2" }]) assert.notEqual(scanCacheKey({ ...base, ...change }), key);
  assert.throws(() => scanCacheKey({ ...base, inheritedEnv: undefined }), /inheritedEnv/, "a key without the inherited environment digest is refused");
  const { fs, plan } = planFor(["crates/foo/src/lib.rs"]);
  const foreign = { digest: () => "d", read: () => ({ key: "other", at: "x", row: { status: "pass" } }), write() {} };
  const r = await runScans(plan, deps({ ...fs, cache: foreign }));
  assert.deepEqual(r.cacheHits, []);
});

test("fileScanCache: round trip, atomic file named by key, garbage and bad keys are misses", () => {
  const dir = mkdtempSync(path.join(process.env.RK_GATE_TMPDIR || os.tmpdir(), "rk-cache-test-"));
  try {
    const cache = fileScanCache(path.join(dir, "rightkit-gate-cache"), { now: () => new Date("2026-10-22T01:02:03.000Z") });
    const key = "a".repeat(64);
    assert.equal(cache.read(key), null);
    cache.write(key, { id: "x", status: "pass" });
    assert.deepEqual(cache.read(key), { key, at: "2026-10-22T01:02:03.000Z", row: { id: "x", status: "pass" } });
    writeFileSync(path.join(dir, "rightkit-gate-cache", `${"b".repeat(64)}.json`), "{nope");
    assert.equal(cache.read("b".repeat(64)), null);
    cache.write("../escape", { id: "x" });
    assert.equal(cache.read("../escape"), null);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("cache: the inherited environment is part of the key (CI=false then CI=true is a miss; RUSTFLAGS too); volatile names are not", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs"]);
  const cache = memCache((s) => (s.class === "A" ? null : "d"));
  await runScans(plan, deps({ ...fs, cache, env: { CI: "false", PATH: "/bin" } }));
  const sameLog = [];
  const same = await runScans(plan, deps({ ...fs, cache, env: { PATH: "/bin", CI: "false" }, spawn: fakeSpawn({ log: sameLog }) }));
  assert.deepEqual(same.cacheHits, bRows(same).map((r) => r.id).sort(), "an identical environment still hits");
  const log = [];
  const flipped = await runScans(plan, deps({ ...fs, cache, env: { CI: "true", PATH: "/bin" }, spawn: fakeSpawn({ log }) }));
  assert.deepEqual(flipped.cacheHits, [], "CI=true must not reuse a pass recorded under CI=false");
  assert.ok(spawned(log, "clippy") > 0, "the clippy scan ran again");
  const flags = await runScans(plan, deps({ ...fs, cache, env: { CI: "true", PATH: "/bin", RUSTFLAGS: "--cfg x" }, spawn: fakeSpawn({}) }));
  assert.deepEqual(flags.cacheHits, [], "a new RUSTFLAGS is a miss");
  const added = await runScans(plan, deps({ ...fs, cache, env: { CI: "true", PATH: "/bin" }, spawn: fakeSpawn({}) }));
  assert.deepEqual(added.cacheHits, bRows(added).map((r) => r.id).sort(), "the CI=true run itself was stored");
  // Only build-relevant names are in the digest: a session id, a socket, a temp dir, a prompt or PATH itself never changes a verdict.
  const noise = { PWD: "/a", OLDPWD: "/b", SHLVL: "2", _: "/x", PROMPT: "p", PS1: "q", TERM_SESSION_ID: "t", WT_SESSION: "w", WEZTERM_PANE: "9", SSH_AUTH_SOCK: "/s", CLAUDE_CODE_SESSION_ID: "z", CLAUDE_CODE_MESSAGING_SOCKET: "\\\\.\\pipe\\x", RANDOM: "42", TEMP: "C:\\t", TMPDIR: "/tmp/x", PATH: "/other", FOO: "1", RK_GATE_JOBS: "8", RUSTRIGHT_QUIET_LEASE: "q-1" };
  assert.equal(inheritedEnvDigest({ CI: "true" }), inheritedEnvDigest({ CI: "true", ...noise }), "non-build names do not change the digest");
  for (const [name, value] of [["RUSTFLAGS", "-C x"], ["CARGO_BUILD_JOBS", "2"], ["RUSTUP_TOOLCHAIN", "1.98.0"], ["SCCACHE_DIR", "/s"], ["RK_LOCAL_BUILD", "1"], ["RIGHTKIT_REQUIRE_DESKTOP", "1"], ["NODE_OPTIONS", "--x"], ["npm_config_registry", "r"], ["CC", "clang"], ["MACOSX_DEPLOYMENT_TARGET", "13"]]) {
    assert.notEqual(inheritedEnvDigest({ CI: "true" }), inheritedEnvDigest({ CI: "true", [name]: value }), `${name} is a build input`);
  }
  assert.notEqual(inheritedEnvDigest({ CI: "true" }), inheritedEnvDigest({ CI: "True" }));
  assert.equal(inheritedEnvDigest({ rustflags: "-C x" }), inheritedEnvDigest({ RUSTFLAGS: "-C x" }), "names compare case-insensitively (Windows)");
});

// ---- requiredScanIds agrees with planScans ---------------------------------------------------------------------------

test("requiredScanIds: every id it requires is planned by planScans, and the per-crate ids agree exactly, for the local qualification plan", () => {
  const variants = [
    {},
    { foo: { featureSets: [["--features", "a"], ["--all-features"]] }, bar: { featureSets: [[], ["--features", "x"]], checkFeatureSets: [["--features", "y"], []] } },
    { foo: { featureSets: [["--features", "a"]], checkFeatureSets: [["--features", "b"], ["--features", "c"]] }, bar: { alsoSerial: true, featureSets: [[]] } },
    { foo: { featureSets: [["--all-features"]] } },
  ];
  for (const withLib of [true, false]) for (const crateFeatures of variants) {
    const all = {
      ...crateFiles,
      "crates/bar/Cargo.toml": 'edition = "2021"\nversion = "0.1.0"\nlicense = "MIT"\n', "crates/bar/src/main.rs": "fn main() {}\n",
      ".github/workflows/ci.yml": "a: b", "scripts/generate-index.mjs": "", "INDEX.md": "x", "package.json": "{}", "ci/gate.config.json": "{}",
      "packages/release/rightkit-ownership.json": "{}", "crates/rightkit-ownership/catalog/ownership-catalog.json": "{}",
    };
    if (!withLib) delete all["crates/foo/src/lib.rs"];
    const fs = fakeFs(all, all);
    const plan = planScans({ changed: Object.keys(all), contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32", includeTests: true, crateFeatures, localCompile: true, tier: "release", cargoCommand: ["rustright", "cargo"] });
    const planned = new Set(ids(plan));
    const required = requiredScanIds({ units: ["foo", "bar"], crateFeatures, exists: (p) => fs.exists(path.join(CONTENT, p)), includeTests: true });
    for (const id of required) assert.ok(planned.has(id), id + " is required but not planned (lib=" + withLib + ", " + JSON.stringify(crateFeatures) + ")");
    const perCrate = (set) => [...set].filter((id) => /^(check|check-features|clippy|doc|test:rust|test:rust-serial|cargo-package-list|changelog-version):(foo|bar)(:|$)/.test(id)).sort();
    assert.deepEqual(perCrate(required), perCrate(planned), "per-crate ids agree (lib=" + withLib + ", " + JSON.stringify(crateFeatures) + ")");
    assert.equal(required.includes("doc:foo"), withLib);
  }
});

test("requiredScanProblems: a missing, failed or deferred required scan is a problem; extra scans are fine", () => {
  const required = ["a", "b"];
  assert.deepEqual(requiredScanProblems([{ id: "a", status: "pass" }, { id: "b", status: "pass" }, { id: "extra", status: "pass" }, { id: "rustfmt:2021", status: "pass" }], required), []);
  assert.deepEqual(requiredScanProblems([{ id: "a", status: "pass" }, { id: "rustfmt:2021", status: "pass" }], required), ["required scan b is missing"]);
  assert.deepEqual(requiredScanProblems([{ id: "a", status: "pass" }, { id: "b", status: "deferred" }, { id: "rustfmt:2021", status: "pass" }], required), ["required scan b is deferred"]);
  assert.deepEqual(requiredScanProblems([{ id: "a", status: "pass" }, { id: "b", status: "pass" }], required), ["no passing rustfmt:* scan"]);
  assert.deepEqual(requiredScanProblems([], ["a"], []), ["required scan a is missing"]);
  assert.throws(() => requiredScanIds({ units: [], exists: () => true }), /non-empty/);
  assert.throws(() => requiredScanIds({ units: ["a"] }), /exists/);
});

// ---- several Cargo workspaces and a root npm package (rightkit-gate adoption survey, gaps 3 and 4) ----------------------------

const lockOf = (...pkgs) => parseLock(pkgs.map(([name, deps = []]) => `[[package]]\nname = "${name}"\nversion = "1.0.0"\n${deps.length ? `dependencies = [\n${deps.map((d) => ` "${d}",`).join("\n")}\n]\n` : ""}`).join("\n"));
const multiFiles = {
  "crates/core/Cargo.toml": manifest, "crates/core/src/lib.rs": "", "Cargo.lock": "version = 4\n",
  "hub/src-tauri/Cargo.toml": manifest, "hub/src-tauri/src/lib.rs": "", "hub/src-tauri/Cargo.lock": "version = 4\n",
  "engine/eng/Cargo.toml": manifest, "engine/eng/src/lib.rs": "", "engine/eng-cli/Cargo.toml": manifest, "engine/eng-cli/src/lib.rs": "", "engine/Cargo.lock": "version = 4\n",
  "engine/deny.toml": "[advisories]\nignore = []\n",
};
const multiUnits = [
  { name: "core", crate: "core", dir: "crates/core", cargoDir: "." },
  { name: "hub/src-tauri:app", crate: "app", dir: "hub/src-tauri", cargoDir: "hub/src-tauri" },
  { name: "engine:eng", crate: "eng", dir: "engine/eng", cargoDir: "engine" },
  { name: "engine:eng-cli", crate: "eng-cli", dir: "engine/eng-cli", cargoDir: "engine" },
];
const multi = (changed, opts = {}) => planFor(changed, multiFiles, { rustUnits: multiUnits, npmUnits: [], ...opts }).plan;

test("workspaces: a unit of a nested workspace runs in that workspace with its crate name, and only its own workspace's scans are planned", () => {
  const p = multi(["hub/src-tauri/src/lib.rs"]);
  const clippy = scanOf(p, "clippy:hub/src-tauri:app");
  assert.ok(clippy, ids(p).join(","));
  assert.equal(clippy.cwd, "hub/src-tauri");
  assert.deepEqual(clippy.args.slice(0, 5), ["cargo", "clippy", "-p", "app", "--no-deps"], "-p takes the crate, not the unit name");
  for (const id of ["check-features:hub/src-tauri:app:0", "doc:hub/src-tauri:app", "test:rust:hub/src-tauri:app", "cargo-metadata:hub/src-tauri"]) {
    const s = scanOf(p, id); assert.ok(s, id); assert.equal(s.cwd, "hub/src-tauri", id);
  }
  assert.ok(!ids(p).some((i) => /clippy:core|engine/.test(i)), "no unit of another workspace");
  assert.ok(!ids(p).includes("cargo-metadata"), "the root workspace's metadata is not planned for a change below it");
  for (const s of p.scans) assert.equal(scanClassOf(s.id), s.class, s.id);
  assert.equal(new Set(ids(p)).size, ids(p).length);
  // A root-workspace unit keeps its plain name and no cwd.
  const root = multi(["crates/core/src/lib.rs"]);
  assert.equal(scanOf(root, "clippy:core").cwd, undefined);
  assert.ok(scanOf(root, "cargo-metadata") && !ids(root).some((i) => i.startsWith("cargo-metadata:")));
});

test("workspaces: feature sets are keyed by <cargoDir>:<crate> or by the plain crate name, the qualified name first", () => {
  const sets = (crateFeatures) => scanOf(multi(["hub/src-tauri/src/lib.rs"], { crateFeatures }), "clippy:hub/src-tauri:app:0")?.args ?? [];
  assert.ok(sets({ "hub/src-tauri:app": { featureSets: [["--features", "x"], []] } }).includes("x"));
  assert.ok(sets({ app: { featureSets: [["--features", "y"], []] } }).includes("y"));
  assert.ok(sets({ app: { featureSets: [["--features", "y"], []] }, "hub/src-tauri:app": { featureSets: [["--features", "z"], []] } }).includes("z"), "the qualified key wins");
  const serial = scanOf(multi(["hub/src-tauri/src/lib.rs"], { crateFeatures: { "hub/src-tauri:app": { featureSets: [["--features", "x"], []], alsoSerial: true } } }), "test:rust-serial:hub/src-tauri:app:0");
  assert.deepEqual(serial.args.slice(-3), ["--no-fail-fast", "--", "--test-threads=1"]);
});

test("workspaces: a workspace-wide change reaches only its own workspace, narrowed by rustScope by unit name; deny and audit run there", () => {
  const p = multi(["engine/Cargo.lock"]);
  assert.deepEqual(ids(p).filter((i) => i.startsWith("clippy:")), ["clippy:engine:eng", "clippy:engine:eng-cli"]);
  assert.deepEqual(["cargo-deny:engine", "cargo-audit:engine", "cargo-metadata:engine"].map((i) => scanOf(p, i)?.cwd), ["engine", "engine", "engine"]);
  assert.ok(!ids(p).some((i) => /^cargo-(deny|audit|metadata)$/.test(i)), "the root workspace's own scans are not planned");
  const narrowed = multi(["engine/Cargo.lock"], { rustScope: new Set(["engine:eng-cli"]) });
  assert.deepEqual(ids(narrowed).filter((i) => i.startsWith("clippy:")), ["clippy:engine:eng-cli"]);
  // A root Cargo.lock is not engine's or the hub's; deny.toml of one workspace stays in that workspace.
  assert.ok(!ids(multi(["Cargo.lock"])).some((i) => i.startsWith("clippy:engine") || i.includes("hub/src-tauri")));
  const deny = ids(multi(["engine/deny.toml"])).filter((i) => i.startsWith("cargo-"));
  assert.deepEqual(deny, ["cargo-metadata:engine", "cargo-deny:engine", "cargo-audit:engine"]);
});

// ---- fix6: Rust configuration of ANY ancestor directory reaches every workspace below it ------------------------------------

const nestedFiles = {
  "crates/core/Cargo.toml": manifest, "crates/core/src/lib.rs": "", "Cargo.lock": "version = 4\n",
  "apps/foo/Cargo.toml": manifest, "apps/foo/src/lib.rs": "", "apps/foo/Cargo.lock": "version = 4\n",
  "apps/bar/Cargo.toml": manifest, "apps/bar/src/lib.rs": "", "apps/bar/Cargo.lock": "version = 4\n",
  "other/baz/Cargo.toml": manifest, "other/baz/src/lib.rs": "", "other/baz/Cargo.lock": "version = 4\n",
  ".cargo/config.toml": "[build]\n", "apps/.cargo/config.toml": "[build]\n", "apps/foo/.cargo/config.toml": "[build]\n", "docs/.cargo/config.toml": "[build]\n",
  "rust-toolchain.toml": "[toolchain]\n", "clippy.toml": "msrv = \"1.81\"\n", "rustfmt.toml": "edition = \"2021\"\n", "deny.toml": "[advisories]\n", "apps/deny.toml": "[advisories]\n", "apps/clippy.toml": "msrv = \"1.81\"\n",
};
const nestedUnits = [
  { name: "core", crate: "core", dir: "crates/core", cargoDir: "." },
  { name: "apps/foo:foo", crate: "foo", dir: "apps/foo", cargoDir: "apps/foo" },
  { name: "apps/bar:bar", crate: "bar", dir: "apps/bar", cargoDir: "apps/bar" },
  { name: "other/baz:baz", crate: "baz", dir: "other/baz", cargoDir: "other/baz" },
];
const nested = (changed, opts = {}) => planFor(changed, nestedFiles, { rustUnits: nestedUnits, npmUnits: [], ...opts }).plan;
const compiled = (plan) => ids(plan).filter((i) => i.startsWith("clippy:")).sort();

test("workspaces: Rust configuration at any ancestor of a workspace puts its whole crate set in scope (the repository .cargo reproduction)", () => {
  const everything = ["clippy:apps/bar:bar", "clippy:apps/foo:foo", "clippy:core", "clippy:other/baz:baz"];
  // The review's reproduction: only the repository-level .cargo/config.toml changed; before the fix it planned toml-parse alone.
  const repo = nested([".cargo/config.toml"]);
  assert.deepEqual(compiled(repo), everything);
  for (const id of ["cargo-metadata", "cargo-metadata:apps/foo", "cargo-deny:apps/foo", "cargo-audit:apps/bar", "test:rust:apps/foo:foo", "test:rust:core"]) assert.ok(ids(repo).includes(id), id);
  // Every other root-level toolchain and tool configuration reaches every nested workspace as well.
  for (const file of ["rust-toolchain.toml", "clippy.toml", "rustfmt.toml"]) assert.deepEqual(compiled(nested([file])), everything, file);
  // cargo-deny reads deny.toml from ancestors too: every workspace below runs its deny, audit and metadata scans, with no compile scan.
  const deny = nested(["deny.toml"]);
  assert.deepEqual(compiled(deny), []);
  for (const id of ["cargo-deny", "cargo-deny:apps/foo", "cargo-deny:apps/bar", "cargo-deny:other/baz", "cargo-audit:apps/foo", "cargo-metadata:apps/bar"]) assert.ok(ids(deny).includes(id), id);
  assert.deepEqual(ids(nested(["apps/deny.toml"])).filter((i) => i.startsWith("cargo-deny")).sort(), ["cargo-deny:apps/bar", "cargo-deny:apps/foo"]);
  // An intermediate directory reaches its own descendants only.
  assert.deepEqual(compiled(nested(["apps/.cargo/config.toml"])), ["clippy:apps/bar:bar", "clippy:apps/foo:foo"], "apps/.cargo reaches apps/foo and apps/bar");
  assert.deepEqual(compiled(nested(["apps/clippy.toml"])), ["clippy:apps/bar:bar", "clippy:apps/foo:foo"]);
  assert.deepEqual(compiled(nested(["apps/foo/.cargo/config.toml"])), ["clippy:apps/foo:foo"], "a workspace's own .cargo reaches only that workspace");
  assert.deepEqual(compiled(nested(["docs/.cargo/config.toml"])), [], "a directory that is no ancestor of any workspace reaches none");
});

test("workspaces: ordinary source changes still plan only their own workspace", () => {
  assert.deepEqual(compiled(nested(["apps/foo/src/lib.rs"])), ["clippy:apps/foo:foo"], "an ordinary source change is unchanged");
  assert.deepEqual(compiled(nested(["README.md"])), []);
});

test("workspaces: reverse dependents come from each workspace's own lock, never across workspaces", () => {
  const rustLocks = { ".": lockOf(["core"]), engine: lockOf(["eng"], ["eng-cli", ["eng"]]), "hub/src-tauri": lockOf(["app"]) };
  const p = multi(["engine/eng/src/lib.rs"], { rustLocks });
  assert.deepEqual(ids(p).filter((i) => i.startsWith("check-dependent:")), ["check-dependent:engine:eng-cli"]);
  const dep = scanOf(p, "check-dependent:engine:eng-cli");
  assert.equal(dep.cwd, "engine"); assert.deepEqual(dep.args.slice(0, 4), ["cargo", "check", "-p", "eng-cli"]);
  // The root workspace's lock has an edge named like engine's; it is not engine's dependent edge.
  const clash = multi(["engine/eng/src/lib.rs"], { rustLocks: { ".": lockOf(["core", ["eng"]]), engine: lockOf(["eng"], ["eng-cli", ["eng"]]) } });
  assert.deepEqual(ids(clash).filter((i) => i.startsWith("check-dependent:")), ["check-dependent:engine:eng-cli"]);
  assert.deepEqual(ids(multi(["engine/eng/src/lib.rs"])).filter((i) => i.startsWith("check-dependent:")), [], "no lock for the workspace, none planned");
});

test("workspaces: a workspace directory or crate that is not a plain name is refused, never run", () => {
  const bad = planFor(["README.md"], { "README.md": "x" }, { rustUnits: [{ name: "x:a", crate: "a", dir: "x", cargoDir: "../x" }], npmUnits: [] }).plan;
  assert.ok(bad.scans.some((s) => s.id.startsWith("path-policy:") && /cargoDir/.test(s.detail)));
  assert.ok(!bad.scans.some((s) => s.tool === "rustright"));
  const evil = planFor(["x/src/lib.rs"], { "x/Cargo.toml": manifest, "x/src/lib.rs": "" }, { rustUnits: [{ name: "x:a b", crate: "a b", dir: "x", cargoDir: "x" }], npmUnits: [] }).plan;
  assert.ok(evil.scans.some((s) => s.id.startsWith("path-policy:") && /not a plain name/.test(s.detail)));
  assert.ok(!evil.scans.some((s) => s.args.includes("a b")));
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { rustUnits: [{ name: "a", dir: "a", crate: 3 }] }), /string crate/);
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { rustUnits: [{ name: "a", dir: "a", cargoDir: 3 }] }), /string cargoDir/);
});

test("workspaces: without a per-unit cargoDir the plan is the old single-workspace plan, ids unsuffixed", () => {
  const files = { "ws/core/Cargo.toml": manifest, "ws/core/src/lib.rs": "", "ws/Cargo.lock": "version = 4\n" };
  const p = planFor(["ws/Cargo.lock"], files, { rustUnits: [{ name: "a", dir: "ws/core" }], npmUnits: [], cargoDir: "ws" }).plan;
  assert.ok(ids(p).includes("cargo-metadata") && ids(p).includes("cargo-deny") && ids(p).includes("clippy:a"));
  for (const s of p.scans.filter((x) => x.tool === "rustright")) assert.equal(s.cwd, "ws", s.id);
});

test("root npm package: it is a unit like any other, but a file inside a nested Rust unit does not trigger it", () => {
  const files = { ...multiFiles, "package.json": '{"name":"app","license":"MIT","scripts":{"test":"x"}}', "tsconfig.json": "{}", "src/main.ts": "", "README.md": "x", "eslint.config.mjs": "" };
  const opts = { rustUnits: multiUnits, npmUnits: [{ name: "app", dir: "." }] };
  const p = planFor(["src/main.ts"], files, opts).plan;
  for (const id of ["tsc:app", "eslint:app", "test:npm:app", "license"]) assert.ok(ids(p).includes(id), id);
  assert.deepEqual(scanOf(p, "test:npm:app").args, ["--dir", ".", "run", "test"]);
  assert.equal(scanOf(p, "tsc:app").cwd, ".");
  const rustOnly = planFor(["hub/src-tauri/src/lib.rs"], files, opts).plan;
  assert.ok(!ids(rustOnly).some((i) => /^(tsc|eslint|test:npm):app$/.test(i)), "a Rust-only change does not run the root package");
  const wide = planFor(["package.json"], files, opts).plan;
  assert.ok(ids(wide).includes("tsc:app"), "a root pnpm file change puts the root package in scope");
  // Nested packages still win their own files.
  const nested = { ...files, "apps/web/package.json": '{"name":"web","license":"MIT"}', "apps/web/tsconfig.json": "{}", "apps/web/src/a.ts": "" };
  const both = planFor(["apps/web/src/a.ts", "src/main.ts"], nested, { rustUnits: [], npmUnits: [{ name: "app", dir: "." }, { name: "web", dir: "apps/web" }] }).plan;
  assert.ok(ids(both).includes("tsc:app") && ids(both).includes("tsc:web"));
});

test("lanes: tests run testJobs at a time after the compile lanes; quiet test scans run last, alone, with the lease env and never in the cache key", async () => {
  const bar = { "crates/bar/Cargo.toml": 'edition = "2024"\nversion = "1.0.0"\nlicense = "MIT"\n', "crates/bar/CHANGELOG.md": "## 1.0.0\n- x\n", "crates/bar/src/lib.rs": "" };
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/bar/src/lib.rs"], bar, { crateFeatures: { foo: { quietTests: true } } });
  assert.equal(plan.scans.find((s) => s.id === "test:rust:foo").quiet, true);
  assert.equal(plan.scans.find((s) => s.id === "test:rust:bar").quiet, undefined);
  assert.equal(laneOf({ id: "test:rust:foo", quiet: true }), "quiet");
  assert.equal(laneOf({ id: "test:rust:bar" }), "tests");
  assert.equal(laneOf({ id: "tsc:x" }), "tests");
  const seen = []; let running = 0; let peakTests = 0;
  const spawn = async (command, args, options) => {
    const id = args.join(" ");
    if (/ test /.test(id)) {
      running += 1; peakTests = Math.max(peakTests, running);
      await new Promise((r) => setTimeout(r, 15));
      running -= 1;
    }
    seen.push({ id, lease: options?.env?.RUSTRIGHT_QUIET_LEASE ?? null });
    return fakeSpawn()(command, args);
  };
  const events = [];
  const quietLane = { acquire: async () => { events.push("acquire"); return { id: "q-1", env: { RUSTRIGHT_QUIET_LEASE: "q-1" } }; }, release: async (id) => events.push(`release ${id}`) };
  const result = await runScans(plan, { ...deps({ ...fs, spawn }), jobs: 2, testJobs: 2, quietLane });
  assert.equal(result.ok, true, scanFailures(result).join("\n"));
  const tests = seen.filter((s) => / test /.test(s.id));
  assert.ok(tests.length >= 2);
  assert.equal(tests.at(-1).id.includes("-p foo"), true, "the quiet crate's tests are the last scans");
  assert.equal(tests.at(-1).lease, "q-1", "quiet scans carry the lease id");
  assert.ok(tests.filter((t) => t.id.includes("-p bar")).every((t) => t.lease === null), "other tests never see the lease");
  assert.deepEqual(events, ["acquire", "release q-1"], "one lease around the quiet lane");
  const firstTestAt = seen.findIndex((s) => / test /.test(s.id));
  assert.ok(seen.slice(0, firstTestAt).every((s) => !/ test /.test(s.id)) && seen.slice(0, firstTestAt).some((s) => /clippy|check/.test(s.id)), "compile scans finish before any test starts");
  // No quietLane, or an acquire that throws: the lane still runs, without the env.
  seen.length = 0;
  const bare = await runScans(plan, { ...deps({ ...fs, spawn }), jobs: 2, quietLane: { acquire: async () => { throw new Error("broker down"); }, release: async () => assert.fail("nothing to release") } });
  assert.equal(bare.ok, true);
  assert.ok(seen.filter((s) => / test /.test(s.id)).every((s) => s.lease === null));
});

test("quietTests: 'ci' leaves the quiet crates' test scans to the CI shards with a note naming them; 'local' (default) plans them for the quiet lane", () => {
  const bar = { "crates/bar/Cargo.toml": 'edition = "2024"\nversion = "1.0.0"\nlicense = "MIT"\n', "crates/bar/CHANGELOG.md": "## 1.0.0\n- x\n", "crates/bar/src/lib.rs": "" };
  const features = { foo: { quietTests: true, alsoSerial: true } };
  const local = planFor(["crates/foo/src/lib.rs", "crates/bar/src/lib.rs"], bar, { crateFeatures: features }).plan;
  assert.ok(ids(local).includes("test:rust:foo") && ids(local).includes("test:rust-serial:foo") && ids(local).includes("test:rust:bar"));
  const ci = planFor(["crates/foo/src/lib.rs", "crates/bar/src/lib.rs"], bar, { crateFeatures: features, quietTests: "ci" }).plan;
  assert.ok(!ids(ci).includes("test:rust:foo") && !ids(ci).includes("test:rust-serial:foo"), "foo's tests are not planned locally");
  assert.ok(ids(ci).includes("test:rust:bar") && ids(ci).includes("clippy:foo"), "bar's tests and foo's compile scans still are");
  assert.ok(ci.notes.some((n) => /2 quiet test scan\(s\) left to the CI shards.*test:rust:foo.*test:rust-serial:foo/.test(n)), ci.notes.join("\n"));
  assert.equal(ci.scans.filter((s) => s.quiet).length, 0);
  // npm packages: `npm.<unit>.quietTests` defers the package's test row the same way; its tsc/eslint rows stay.
  const pkg = { ...bar, "packages/p/package.json": '{"license":"MIT","scripts":{"test":"x"}}', "packages/p/tsconfig.json": "{}", "packages/q/package.json": '{"license":"MIT","scripts":{"test":"x"}}' };
  const changedPkgs = ["crates/bar/src/lib.rs", "packages/p/package.json", "packages/q/package.json"];
  const npmCi = planFor(changedPkgs, pkg, { packageFeatures: { p: { quietTests: true } }, quietTests: "ci" }).plan;
  assert.ok(!ids(npmCi).includes("test:npm:p") && ids(npmCi).includes("tsc:p") && ids(npmCi).includes("test:npm:q"), ids(npmCi).join(" "));
  assert.ok(npmCi.notes.some((n) => /1 quiet test scan\(s\) left to the CI shards.*test:npm:p/.test(n)), npmCi.notes.join("\n"));
  const npmLocal = planFor(changedPkgs, pkg, { packageFeatures: { p: { quietTests: true } } }).plan;
  assert.deepEqual(npmLocal.scans.filter((s) => s.quiet).map((s) => s.id), ["test:npm:p"]);
});

test("suite-api: planned only when a contract input changes; the checked-in projections pass and a one-byte drift fails", async () => {
  const REAL = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
  const read = (p) => readFileSync(p, "utf8");
  const realFs = { exists: existsSync, readFile: read, readdir: readdirSync, realpath: (p) => p };
  const planFor = (changed) => planScans({ changed, contentRoot: REAL, liveRoot: REAL, ...realFs, platform: "darwin", includeTests: false });
  const suiteScan = (plan) => plan.scans.find((s) => s.id === "suite-api");
  assert.equal(scanClassOf("suite-api"), "A");
  assert.ok(suiteScan(planFor(["contracts/right-suite-api.v1.json"])), "a contract change plans the scan");
  assert.ok(suiteScan(planFor(["packages/app-update/src/suite-api.generated.ts"])), "a projection change plans the scan");
  assert.ok(suiteScan(planFor(["swift/RightKitSwift/Sources/RightKitLicense/RightSuiteAPI.generated.swift"])), "a change to the Swift projection plans the scan");
  assert.equal(suiteScan(planFor(["packages/release/README.md"])), undefined, "an unrelated change does not");
  const only = (plan, extra = {}) => runScans(plan, { ...deps({ ...realFs }), classes: ["A"], exclude: (s) => s.id !== "suite-api", ...extra });
  const passing = await only(planFor(["contracts/right-suite-api.v1.json"]));
  const row = passing.scans.find((s) => s.id === "suite-api");
  assert.equal(row.status, "pass", row.detail);
  const drifted = await only(planFor(["contracts/right-suite-api.v1.json"]), {
    readFile: (p) => (p.endsWith("suite-api.generated.ts") ? `${read(p)} ` : read(p)),
  });
  const bad = drifted.scans.find((s) => s.id === "suite-api");
  assert.equal(bad.status, "fail");
  assert.match(bad.detail, /suite-api\.generated\.ts/);
  const swiftDrift = await only(planFor(["contracts/right-suite-api.v1.json"]), {
    readFile: (p) => (p.endsWith("RightSuiteAPI.generated.swift") ? `${read(p)} ` : read(p)),
  });
  const badSwift = swiftDrift.scans.find((s) => s.id === "suite-api");
  assert.equal(badSwift.status, "fail");
  assert.match(badSwift.detail, /RightSuiteAPI\.generated\.swift/);
});

// ---- gate false positives found by full-gate dry runs on real app branches (0.6.6) ----------------------------------------------------------
// FP1 the linter an app has is the one it configures; FP2 an npm test script that runs the broker is marked; FP3 several independent pnpm roots.

const appManifest = '{"name":"app","license":"MIT","scripts":{"test":"x"}}';
const lintPlan = (files, units = [{ name: "app", dir: "." }]) => planFor(["src/a.ts"], { "package.json": appManifest, "src/a.ts": "", ...files }, { rustUnits: [], npmUnits: units }).plan;

test("lint: a Biome-only app is planned biome, never eslint (ViewRight and ScrapeRight have biome.json and no ESLint config)", () => {
  for (const config of ["biome.json", "biome.jsonc"]) {
    const p = lintPlan({ [config]: "{}" });
    const biome = scanOf(p, "biome:app");
    assert.ok(biome, `${config}: ${ids(p).join(" ")}`);
    assert.deepEqual([biome.tool, biome.args, biome.cwd, biome.class], ["pnpm", ["exec", "biome", "lint", "."], ".", "B"]);
    assert.equal(biome.command, "pnpm_config_verify_deps_before_run=false pnpm exec biome lint .", "the pnpm dependency check is off for the run (B4)");
    assert.equal(scanClassOf("biome:app"), "B");
    assert.ok(!ids(p).includes("eslint:app"), `${config}: a Biome app has no ESLint config, so eslint . would fail with "couldn't find an eslint.config": ${ids(p).join(" ")}`);
  }
});

test("lint: eslint is planned for any ESLint config (flat or legacy) and never biome; both configs plan both; neither plans neither", () => {
  for (const config of ["eslint.config.js", "eslint.config.mjs", "eslint.config.cjs", "eslint.config.ts", "eslint.config.mts", "eslint.config.cts",
    ".eslintrc", ".eslintrc.json", ".eslintrc.js", ".eslintrc.cjs", ".eslintrc.yaml", ".eslintrc.yml"]) {
    const p = lintPlan({ [config]: "" });
    assert.ok(ids(p).includes("eslint:app"), config);
    assert.ok(!ids(p).includes("biome:app"), `${config} is no Biome config`);
  }
  assert.deepEqual(scanOf(lintPlan({ "eslint.config.mjs": "" }), "eslint:app").args, ["exec", "eslint", "."]);
  const both = lintPlan({ "eslint.config.mjs": "", "biome.json": "{}" });
  assert.ok(ids(both).includes("eslint:app") && ids(both).includes("biome:app"), "both tools configured: both run");
  const neither = lintPlan({});
  assert.ok(!ids(neither).some((i) => /^(eslint|biome):/.test(i)), "no linter config: no lint scan");
  assert.ok(ids(neither).includes("test:npm:app"), "the other npm scans are unaffected");
});

test("lint: a config is found in the unit's directory or any ancestor, and only there", () => {
  const units = [{ name: "web", dir: "apps/web" }, { name: "docs", dir: "apps/docs" }, { name: "tool", dir: "tools/t" }];
  const files = {
    "apps/web/package.json": appManifest, "apps/docs/package.json": appManifest, "tools/t/package.json": appManifest,
    "apps/web/biome.json": "{}", "apps/eslint.config.js": "",
  };
  const p = planFor(["apps/web/package.json", "apps/docs/package.json", "tools/t/package.json"], files, { rustUnits: [], npmUnits: units }).plan;
  assert.deepEqual(ids(p).filter((i) => /^(eslint|biome):/.test(i)).sort(), ["biome:web", "eslint:docs", "eslint:web"], "web: its own Biome and the apps/ ESLint config; docs: the apps/ ESLint config; tools/t: nothing");
  const root = planFor(["apps/web/package.json", "tools/t/package.json"], { ...files, "biome.json": "{}" }, { rustUnits: [], npmUnits: units }).plan;
  assert.deepEqual(ids(root).filter((i) => /^biome:/.test(i)).sort(), ["biome:tool", "biome:web"], "a repository-level config covers every unit");
});

// ---- FP2: the package script chain that runs the broker -------------------------------------------------------------------------------------------

test("scriptBrokerCall: follows pnpm <script>, pnpm run <script>, npm run and yarn through the same package.json to a rustright or cargo call", () => {
  // ViewRight: test:e2e is the broker call; test:edge is plain vitest.
  assert.deepEqual(scriptBrokerCall({ test: "pnpm test:edge && pnpm test:e2e", "test:edge": "vitest run", "test:e2e": "rustright storage run --app viewright -- right-release test" }),
    { tool: "rustright", via: ["test", "test:e2e"], command: "rustright storage run --app viewright -- right-release test" });
  // PostRight: test:rust is two broker commands.
  const post = scriptBrokerCall({ test: "pnpm run test:rust && pnpm run test:e2e", "test:rust": "rustright cargo build -p postright-engine && rustright cargo test --workspace", "test:e2e": "rustright storage run -- x" });
  assert.deepEqual([post.tool, post.via, post.command], ["rustright", ["test", "test:rust"], "rustright cargo build -p postright-engine"]);
  assert.equal(scriptBrokerCall({ test: "cargo test --workspace" }).tool, "cargo", "plain cargo is the unmanaged toolchain on such a host");
  assert.equal(scriptBrokerCall({ test: "npm run unit && npm run e2e", unit: "vitest", e2e: "rustright cargo test" }).via.join(">"), "test>e2e");
  assert.equal(scriptBrokerCall({ test: "yarn e2e", e2e: "rustright x" }).via.join(">"), "test>e2e");
  assert.equal(scriptBrokerCall({ test: "yarn run e2e", e2e: "rustright x" }).via.join(">"), "test>e2e");
  assert.equal(scriptBrokerCall({ test: "pnpm -s run e2e", e2e: "rustright x" }).via.join(">"), "test>e2e", "options before the subcommand");
  assert.equal(scriptBrokerCall({ test: "pnpm run --if-present e2e", e2e: "rustright x" }).via.join(">"), "test>e2e", "options after run");
  assert.equal(scriptBrokerCall({ test: "pnpm --dir . run e2e", e2e: "rustright x" }).via.join(">"), "test>e2e", "--dir . is this package");
  assert.equal(scriptBrokerCall({ test: "pnpm --dir=. e2e", e2e: "rustright x" }).via.join(">"), "test>e2e");
  assert.equal(scriptBrokerCall({ test: "pnpm --reporter silent e2e", e2e: "rustright x" }).via.join(">"), "test>e2e", "an option value is not the subcommand");
});

test("scriptBrokerCall: sees the tool through env words, wrappers, pipes, subshells and the pre/post hooks of each script", () => {
  for (const test_ of ["FOO=1 rustright cargo test", "cross-env CI=1 rustright cargo test", "pnpm exec rustright cargo test", "npx rustright cargo test", "env -i X=1 cargo test",
    "vitest run; rustright cargo test", "vitest run | cargo test", "(cd x && cargo test)", "vitest || rustright x", "time cargo test", "C:\\tools\\rustright.exe cargo test"]) {
    assert.ok(scriptBrokerCall({ test: test_ }), test_);
  }
  assert.deepEqual(scriptBrokerCall({ test: "vitest run", pretest: "rustright cargo build" }).via, ["pretest"], "pretest runs before test");
  assert.deepEqual(scriptBrokerCall({ test: "vitest run", posttest: "cargo clean" }).via, ["posttest"], "posttest runs after it");
  assert.deepEqual(scriptBrokerCall({ test: "pnpm run a", a: "x", posta: "cargo test" }).via, ["test", "posta"], "the hooks of a followed script");
});

test("scriptBrokerCall: no call where the tool is only an argument, quoted, in a file or in another package; scripts that are not strings are ignored", () => {
  for (const scripts of [
    { test: "vitest run" },
    { test: "pnpm test:edge && pnpm build", "test:edge": "vitest run", build: "vite build" },
    { test: "echo rustright cargo" }, { test: 'echo "ok && rustright x"' }, { test: "node scripts/rustright.mjs" }, { test: "sh scripts/test.sh" }, { test: "rustrighter x" },
    { test: "pnpm run missing" }, { test: "pnpm --filter other test", other: "x" }, { test: "pnpm -r run e2e", e2e: "rustright x" }, { test: "pnpm --dir ../x run e2e", e2e: "rustright x" },
    { test: "pnpm --workspace-root run e2e", e2e: "rustright x" }, { test: "npm run -w pkg e2e", e2e: "rustright x" }, { test: "pnpm install --frozen-lockfile" },
    { test: 3 }, { test: ["cargo test"] }, { test: null }, {},
  ]) assert.equal(scriptBrokerCall(scripts), null, JSON.stringify(scripts));
  for (const bad of [null, undefined, "cargo test", ["cargo test"], 3]) assert.equal(scriptBrokerCall(bad), null);
  assert.equal(scriptBrokerCall({ test: "x".repeat(200000) }), null, "a huge script is bounded, not a hang");
  assert.equal(scriptBrokerCall({ build: "cargo build" }, "build").tool, "cargo", "any entry script");
});

test("scriptBrokerCall: bounded in depth and safe against cycles", () => {
  assert.equal(scriptBrokerCall({ test: "pnpm run a", a: "pnpm run test" }), null, "a cycle ends");
  assert.equal(scriptBrokerCall({ test: "pnpm run test" }), null, "a script that runs itself ends");
  assert.equal(scriptBrokerCall({ test: "pnpm run a && pnpm run b", a: "pnpm run c", b: "pnpm run c", c: "vitest" }), null, "a diamond is visited once");
  const chain = (length, last) => Object.fromEntries(Array.from({ length }, (_, i) => [i === 0 ? "test" : `s${i}`, i === length - 1 ? last : `pnpm run s${i + 1}`]));
  assert.ok(scriptBrokerCall(chain(8, "cargo test")), "eight scripts deep is followed");
  assert.equal(scriptBrokerCall(chain(12, "cargo test")), null, "twelve deep is past the default bound of 8");
  assert.ok(scriptBrokerCall(chain(12, "cargo test"), "test", { maxDepth: 20 }), "the bound is a parameter");
});

test("test:npm scans carry brokerCall when the test script chain runs the broker, and the plan's commands and digest do not change", () => {
  const viewright = { name: "viewright", license: "MIT", scripts: { test: "pnpm test:edge && pnpm test:e2e", "test:edge": "vitest run", "test:e2e": "rustright storage run --app viewright -- right-release test" } };
  const plain = { name: "viewright", license: "MIT", scripts: { test: "vitest run" } };
  const planOf = (manifestJson) => planFor(["package.json"], { "package.json": JSON.stringify(manifestJson) }, { rustUnits: [], npmUnits: [{ name: "viewright", dir: "." }] }).plan;
  const marked = scanOf(planOf(viewright), "test:npm:viewright");
  assert.deepEqual(marked.brokerCall, { tool: "rustright", via: ["test", "test:e2e"], command: "rustright storage run --app viewright -- right-release test" });
  assert.equal(scanOf(planOf(plain), "test:npm:viewright").brokerCall, undefined);
  assert.equal(planOf(viewright).scanSetDigest, planOf(plain).scanSetDigest, "the marker is not part of the scan set digest");
  assert.deepEqual(marked.args, ["--dir", ".", "run", "test"]);
  assert.equal(marked.tool, "pnpm", "the scan itself still runs pnpm; only the gate's deferral reads the marker");
});

// ---- FP3: several independent pnpm roots -----------------------------------------------------------------------------------------------------------

const rootsFiles = {
  "apps/a/package.json": '{"name":"a","license":"MIT","scripts":{"test":"x"}}', "apps/a/pnpm-lock.yaml": "", "apps/a/tsconfig.json": "{}", "apps/a/src/x.ts": "",
  "apps/b/package.json": '{"name":"b","license":"MIT","scripts":{"test":"x"}}', "apps/b/pnpm-lock.yaml": "", "apps/b/biome.json": "{}", "apps/b/src/y.ts": "",
};
const rootsUnits = [{ name: "apps-a", dir: "apps/a", root: "apps/a" }, { name: "apps-b", dir: "apps/b", root: "apps/b" }];
const rootsPlan = (changed, files = rootsFiles, over = {}) => planFor(changed, files, { rustUnits: [], npmUnits: rootsUnits, npmRoots: ["apps/a", "apps/b"], ...over }).plan;

test("pnpm roots: each root gets its own lockfile and audit scans in its directory, and each unit its own tsc, lint and test (CodeRight: two apps, no root package.json)", () => {
  const p = rootsPlan(["apps/a/src/x.ts", "apps/b/pnpm-lock.yaml"]);
  const want = ["pnpm-lockfile:apps/a", "pnpm-audit:apps/a", "pnpm-lockfile:apps/b", "pnpm-audit:apps/b", "tsc:apps-a", "test:npm:apps-a", "biome:apps-b", "test:npm:apps-b"];
  for (const id of want) assert.ok(ids(p).includes(id), `${id} in ${ids(p).join(" ")}`);
  assert.ok(!ids(p).includes("pnpm-lockfile") && !ids(p).includes("pnpm-audit"), "nothing is planned at the repository root, which holds no package.json (it failed with: No package.json found)");
  for (const id of ["pnpm-lockfile", "pnpm-audit"]) {
    for (const root of ["apps/a", "apps/b"]) {
      const s = scanOf(p, `${id}:${root}`);
      assert.deepEqual([s.cwd, s.root, s.class, s.tool], [root, "content", "A", "pnpm"], s.id);
      assert.equal(scanClassOf(s.id), "A");
    }
  }
  assert.deepEqual(scanOf(p, "pnpm-lockfile:apps/a").args, scanOf(planFor(["packages/p/package.json"], { "packages/p/package.json": appManifest }).plan, "pnpm-lockfile").args, "the same command as the repository-root scan");
  assert.deepEqual(scanOf(p, "pnpm-lockfile:apps/a").env, scanOf(p, "pnpm-audit:apps/b").env, "no pnpmfile hooks and no lifecycle scripts, as at the root");
  assert.equal(scanOf(p, "tsc:apps-a").cwd, "apps/a");
  assert.deepEqual(scanOf(p, "test:npm:apps-b").args, ["--dir", "apps/b", "run", "test"]);
  assert.ok(!ids(p).includes("eslint:apps-b") && !ids(p).includes("biome:apps-a"), "each unit is linted by the tool it configures");
});

test("pnpm roots: a change in one root plans that root only; a root's own install graph puts all of its units in scope", () => {
  const onlyA = rootsPlan(["apps/a/src/x.ts"]);
  assert.ok(ids(onlyA).includes("pnpm-lockfile:apps/a") && !ids(onlyA).some((i) => i.includes("apps/b") || i.endsWith("apps-b")), ids(onlyA).join(" "));
  const members = { ...rootsFiles, "apps/a/packages/ui/package.json": '{"name":"ui","license":"MIT","scripts":{"test":"x"}}', "apps/a/pnpm-workspace.yaml": "packages:\n  - 'packages/*'\n" };
  const units = [...rootsUnits, { name: "apps-a-packages-ui", dir: "apps/a/packages/ui", root: "apps/a" }];
  const wide = rootsPlan(["apps/a/pnpm-lock.yaml"], members, { npmUnits: units });
  assert.ok(ids(wide).includes("test:npm:apps-a") && ids(wide).includes("test:npm:apps-a-packages-ui"), `a root lock change reaches the member: ${ids(wide).join(" ")}`);
  assert.ok(!ids(wide).includes("test:npm:apps-b"), "another root is untouched");
  const npmrc = rootsPlan(["apps/b/.npmrc"], { ...rootsFiles, "apps/b/.npmrc": "x=1" });
  assert.ok(ids(npmrc).includes("test:npm:apps-b") && !ids(npmrc).includes("test:npm:apps-a"), "a root's .npmrc");
  const stray = rootsPlan(["package.json"], { ...rootsFiles, "package.json": "{}" });
  assert.ok(!ids(stray).some((i) => i.startsWith("pnpm-") || i.startsWith("test:npm:")), "a repository-root file belongs to no pnpm root here");
});

test("pnpm roots: the repository root keeps its unsuffixed ids beside a nested root, and a nested file does not plan the root's scans", () => {
  const files = { ...rootsFiles, "package.json": appManifest, "pnpm-lock.yaml": "", "src/r.ts": "" };
  const units = [{ name: "app", dir: "." }, rootsUnits[0]];
  const opts = { npmUnits: units, npmRoots: [".", "apps/a"] };
  const nested = rootsPlan(["apps/a/src/x.ts"], files, opts);
  assert.ok(ids(nested).includes("pnpm-lockfile:apps/a") && !ids(nested).includes("pnpm-lockfile"), "apps/a/** belongs to the nested root only");
  const root = rootsPlan(["src/r.ts"], files, opts);
  assert.ok(ids(root).includes("pnpm-lockfile") && ids(root).includes("pnpm-audit") && ids(root).includes("tsc:app") === false, ids(root).join(" "));
  assert.ok(!ids(root).some((i) => i.endsWith("apps/a") || i === "test:npm:apps-a"), "and the nested root is untouched");
  assert.equal(scanOf(root, "pnpm-lockfile").cwd, undefined, "the repository root's scans run at the root, as before");
  const both = rootsPlan(["src/r.ts", "apps/a/src/x.ts"], files, opts);
  assert.ok(ids(both).includes("pnpm-lockfile") && ids(both).includes("pnpm-lockfile:apps/a"));
});

test("pnpm roots: a repository with one root plans exactly what it did - same ids, commands and digest - whether or not the roots are listed", () => {
  const files = { "packages/p/package.json": appManifest, "packages/p/tsconfig.json": "{}", "packages/p/eslint.config.mjs": "", "package.json": appManifest };
  const units = [{ name: "app", dir: "." }, { name: "p", dir: "packages/p" }];
  const changed = ["packages/p/package.json", "package.json"];
  const legacy = planFor(changed, files, { rustUnits: [], npmUnits: units }).plan;
  const listed = planFor(changed, files, { rustUnits: [], npmUnits: units, npmRoots: ["."] }).plan;
  const dflt = planFor(changed, files, {}).plan;
  const view = (plan) => plan.scans.map((s) => [s.id, s.command, s.class, s.cwd ?? null]);
  assert.deepEqual(view(listed), view(legacy));
  assert.equal(listed.scanSetDigest, legacy.scanSetDigest);
  assert.deepEqual(ids(legacy).filter((i) => i.startsWith("pnpm-")), ["pnpm-lockfile", "pnpm-audit"]);
  assert.ok(ids(dflt).includes("pnpm-lockfile") && ids(dflt).includes("pnpm-audit"), "no npmRoots at all (CI, local-qualify): the root pair, as before");
  const none = planFor(["docs/package.json"], { "docs/package.json": "{}" }, { rustUnits: [], npmUnits: [] });
  assert.ok(ids(none.plan).includes("pnpm-lockfile"), "without a roots list, a nested package.json still plans the root pair (CI publication scans rely on it)");
  const empty = planFor(["docs/package.json"], { "docs/package.json": "{}" }, { rustUnits: [], npmUnits: [], npmRoots: [] }).plan;
  assert.ok(!ids(empty).some((i) => i.startsWith("pnpm-")), "an empty roots list: this repository has no pnpm root, so no pnpm scan");
});

test("pnpm roots: a root that is not a plain repository-relative directory, or a unit outside its root, is refused and never run", () => {
  const bad = rootsPlan(["apps/a/src/x.ts"], rootsFiles, { npmRoots: ["apps/a", "../x", "/abs"] });
  assert.equal(ids(bad).filter((i) => i.startsWith("path-policy:")).length, 2);
  assert.ok(!bad.scans.some((s) => s.args.some((a) => a.includes(".."))));
  const outside = rootsPlan(["apps/a/src/x.ts"], rootsFiles, { npmUnits: [{ name: "apps-a", dir: "apps/a", root: "apps/b" }], npmRoots: [] });
  assert.ok(ids(outside).some((i) => i.startsWith("path-policy:")), "a unit whose root does not hold it");
  assert.throws(() => rootsPlan(["apps/a/src/x.ts"], rootsFiles, { npmRoots: "apps/a" }), /npmRoots must be/);
  assert.throws(() => rootsPlan(["apps/a/src/x.ts"], rootsFiles, { npmUnits: [{ name: "a", dir: "apps/a", root: 3 }] }), /string root/);
});

test("advisory scans: cargo-deny and cargo-audit run offline against the pinned database, and a pin change is a different scan", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const log = []; const ensured = [];
  const result = await runScans(plan, deps({ ...fs, classes: ["A"], spawn: fakeSpawn({ log }), ensureAdvisoryDb: (o) => { ensured.push(o); return "/cache/advisory-db/pinned"; } }));
  assert.equal(result.ok, true);
  assert.equal(ensured.length, 1, "materialised once for both scans");
  assert.equal(ensured[0].pin.commit, "21d909e4c8bd4e14c7aea58bf980c881a0053ab1");
  const deny = log.find((c) => c.includes("deny") && c.includes("check")).slice(2);
  assert.deepEqual([...deny.slice(0, 2), ...deny.slice(4)], ["deny", "--offline", "--config", "/tmp/rk-deny/deny.toml", "check"]);
  assert.equal(deny[2], "--metadata-path");
  assert.match(deny[3], /rk-deny-metadata-[^/\\]+[/\\]metadata\.json$/, "metadata resolved online & --locked first, so --offline never needs a warm cargo cache");
  assert.ok(log.some((c) => c.includes("metadata") && c.includes("--locked") && !c.includes("--no-deps")), "the full --locked metadata is resolved before cargo-deny");
  assert.deepEqual(log.find((c) => c.includes("audit") && c.includes("--db")).slice(2), ["audit", "--db", "/cache/advisory-db/pinned", "--no-fetch"]);
  const other = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { advisoryPin: { url: "https://github.com/rustsec/advisory-db", commit: "b".repeat(40) } }).plan;
  assert.notEqual(other.scans.find((s) => s.id === "cargo-deny").command, plan.scans.find((s) => s.id === "cargo-deny").command);
});

test("advisory scans: an unavailable pinned database fails both scans and no tool runs", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, classes: ["A"], spawn: fakeSpawn({ log }), ensureAdvisoryDb: () => { throw new Error("advisory database x is not available"); } }));
  for (const id of ["cargo-deny", "cargo-audit"]) {
    const row = result.scans.find((s) => s.id === id);
    assert.equal(row.status, "fail"); assert.equal(row.code, "ADVISORY_DB"); assert.match(row.detail, /not available/);
  }
  assert.ok(!log.some((c) => c.includes("--offline") || c.includes("--no-fetch")));
  assert.equal(result.ok, false);
});

test("class A latch: an ADVISORY_DB failure in cargo-deny (the early return before the scan spawns) still stops class B, whose rows report not-run", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  assert.ok(ids(plan, "B").length > 0, "the plan has class B scans to hold back");
  const result = await runScans(plan, deps({ ...fs, ensureAdvisoryDb: () => { throw new Error("no advisory db"); } }), { classes: ["A", "B"] });
  const deny = result.scans.find((s) => s.id === "cargo-deny");
  assert.equal(deny.status, "fail");
  assert.equal(deny.code, "ADVISORY_DB");
  assert.match(deny.detail, /no advisory db/);
  assert.equal(result.ok, false);
  assert.ok(bRows(result).length > 0, "class B rows are in the result");
  for (const row of bRows(result)) {
    assert.equal(row.status, "not-run", row.id);
    assert.match(row.detail, /class A scan failed/, row.id);
  }
});

test("advisory scans: a workspace without deny.toml gets RightKit's policy, never cargo-deny's empty default", () => {
  const text = readFileSync(new URL("./deny-default.toml", import.meta.url), "utf8");
  assert.match(text, /\[licenses\][\s\S]*allow = \[[\s\S]*"MIT"[\s\S]*"Apache-2\.0"/);
  assert.match(text, /\[licenses\.private\]\nignore = true/);
  assert.match(text, /unknown-registry = "deny"/);
  const src = readFileSync(new URL("./deterministic-scans.mjs", import.meta.url), "utf8");
  assert.match(src, /deny-default\.toml/);
});

test("actionlint needs shellcheck: without it the workflow scan fails instead of passing its run: scripts unchecked", async () => {
  const { fs, plan } = planFor([".github/workflows/ci.yml"], { ".github/workflows/ci.yml": "on: push\njobs: {}\n" }, { all: true, tier: "release" });
  const scan = plan.scans.find((s) => s.id === "actionlint");
  assert.deepEqual(scan.requires, ["shellcheck"]);
  const missing = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ missing: ["shellcheck"] }) }));
  const row = missing.scans.find((s) => s.id === "actionlint");
  assert.equal(row.status, "fail");
  assert.equal(row.code, "TOOL_MISSING");
  assert.match(row.detail, /shellcheck/);
  const present = await runScans(plan, deps({ ...fs }));
  assert.equal(present.scans.find((s) => s.id === "actionlint").status, "pass");
});

test("json-parse reads tsconfig, jsconfig and .vscode settings as JSON with comments; everything else stays strict", async () => {
  const files = { "tsconfig.json": "{\n  // strict\n  \"compilerOptions\": { /* x */ \"strict\": true, },\n}\n", "app/tsconfig.app.json": "{ \"a\": 1, // c\n}", "package.json": "{ \"a\": 1 // c\n}" };
  const { fs, plan } = planFor(Object.keys(files), files);
  const result = await runScans(plan, deps({ ...fs }));
  const row = result.scans.find((s) => s.id === "json-parse");
  assert.equal(row.status, "fail");
  assert.match(row.detail, /package\.json/);
  assert.doesNotMatch(row.detail, /tsconfig/);
});

test("advisory exceptions: an app workspace whose deny.toml ignores an advisory needs its own ledger, checked like RightKit's", async () => {
  const deny = "[advisories]\nignore = [\n  { id = \"RUSTSEC-2024-0429\", reason = \"Linux only\" },\n]\n";
  // An app: no ci/advisory-exceptions.json (that file is RightKit's own ledger for its root workspace).
  const appFiles = Object.fromEntries(Object.entries({ ...crateFiles, "deny.toml": deny }).filter(([k]) => k !== "ci/advisory-exceptions.json"));
  const fs = fakeFs(appFiles, appFiles);
  const plan = planScans({ changed: ["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32" });
  const scan = plan.scans.find((s) => s.id.startsWith("advisory-exceptions") && s.exceptionsFile);
  assert.ok(scan, plan.scans.map((s) => s.id).join(" "));
  assert.equal(scan.exceptionsFile, ".rightkit/advisory-exceptions.json");
  const result = await runScans(plan, deps({ ...fs }));
  const row = result.scans.find((s) => s.id === scan.id);
  assert.equal(row.status, "fail");
  assert.match(row.detail, /\.rightkit\/advisory-exceptions\.json/);
});

// ---- B4: pnpm 11's dependency check before `pnpm run` and `pnpm exec` must not abort a gate scan -----------------------------------------------------------
// CodeRight, 2026-10-25: tsc:, biome: and test:npm: failed in two seconds with a stack through runDepsStatusCheck. pnpm 11 verifies, before every
// `pnpm run` and `pnpm exec`, that node_modules matches pnpm-lock.yaml and answers a stale tree by running `pnpm install`, which wants to purge the
// modules directory and aborts with no TTY. The scan never reached tsc, biome or the tests, and the reported error was the tail of a stack.

// What pnpm 11.25.0 printed for `pnpm exec tsc --version` in that tree (stderr, the paths of the host kept).
const PNPM_ABORT = [
  "[ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY] Aborted removal of modules directory due to no TTY",
  "",
  'If you are running pnpm in CI, set the CI environment variable to "true", or set "confirmModulesPurge" to "false".',
  "[ERROR] Command failed with exit code 1: pnpm install",
  "",
  "pnpm: Command failed with exit code 1: pnpm install",
  "    at getFinalError (file:///root/.local/share/pnpm/.tools/pnpm/11.25.0_tmp_28750/node_modules/pnpm/dist/pnpm.mjs:89986:14)",
  "    at makeError (file:///root/.local/share/pnpm/.tools/pnpm/11.25.0_tmp_28750/node_modules/pnpm/dist/pnpm.mjs:92293:21)",
  "    at boundExeca (file:///root/.local/share/pnpm/.tools/pnpm/11.25.0_tmp_28750/node_modules/pnpm/dist/pnpm.mjs:96532:49)",
  "    at sync2 (file:///root/.local/share/pnpm/.tools/pnpm/11.25.0_tmp_28750/node_modules/pnpm/dist/pnpm.mjs:96687:14)",
  "    at runPnpmCli (file:///root/.local/share/pnpm/.tools/pnpm/11.25.0_tmp_28750/node_modules/pnpm/dist/pnpm.mjs:276906:5)",
  "    at runDepsStatusCheck (file:///root/.local/share/pnpm/.tools/pnpm/11.25.0_tmp_28750/node_modules/pnpm/dist/pnpm.mjs:278122:7)",
  "",
].join("\n");

// A stand-in for pnpm 11 on a stale tree: `pnpm run` and `pnpm exec` abort unless pnpm_config_verify_deps_before_run is `false` in the child's
// environment (the pnpm_config_ spelling; npm_config_ does not turn it off); with it off the tool runs and answers `toolResult`.
function stalePnpm({ log = [], toolResult = { status: 0, stdout: "", stderr: "" } } = {}) {
  const base = fakeSpawn();
  return (command, args, options = {}) => {
    if (path.basename(command).replace(/\.exe$/, "") !== "pnpm" || args[0] === "--version" || args[0] === "install" || args[0] === "audit") return base(command, args);
    const flag = options.env?.pnpm_config_verify_deps_before_run;
    log.push({ args, flag });
    return flag === "false" ? toolResult : { status: 1, stdout: "", stderr: PNPM_ABORT };
  };
}
const jsApp = { "package.json": '{"name":"app","license":"MIT","scripts":{"test":"node --test"}}', "tsconfig.json": "{}", "eslint.config.js": "", "biome.json": "{}", "src/a.ts": "" };
const pnpmUnit = { rustUnits: [], npmUnits: [{ name: "app", dir: "." }] };

test("B4: the class B pnpm scans (tsc, eslint, biome, test:npm) turn pnpm's dependency check off; the class A lockfile and audit scans and every other scan are unchanged", () => {
  const { plan } = planFor(["src/a.ts"], jsApp, pnpmUnit);
  const ENV = { pnpm_config_verify_deps_before_run: "false" };
  for (const id of ["tsc:app", "eslint:app", "biome:app"]) assert.deepEqual(scanOf(plan, id).env, ENV, id);
  assert.deepEqual(scanOf(plan, "test:npm:app").env, { ...ENV, RIGHTKIT_ISOLATED_CHECKOUT: "1" }, "the isolated checkout flag stays");
  assert.equal(scanOf(plan, "tsc:app").command, "pnpm_config_verify_deps_before_run=false pnpm exec tsc --noEmit -p tsconfig.json");
  assert.equal(scanOf(plan, "test:npm:app").command, "RIGHTKIT_ISOLATED_CHECKOUT=1 pnpm_config_verify_deps_before_run=false pnpm --dir . run test");
  for (const id of ["pnpm-lockfile", "pnpm-audit"]) {
    const scan = scanOf(plan, id);
    assert.ok(scan, id);
    assert.equal(scan.env.pnpm_config_verify_deps_before_run, undefined, `${id} installs into no tree and runs no script: no dependency check to turn off`);
  }
  assert.ok(plan.scans.filter((s) => s.tool !== "pnpm").every((s) => s.env?.pnpm_config_verify_deps_before_run === undefined), "no other scan gets it");
});

test("B4: every class B pnpm scan reaches pnpm with the check off, so a stale node_modules runs the tool or the test instead of aborting", async () => {
  const { fs, plan } = planFor(["src/a.ts"], jsApp, pnpmUnit);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, spawn: stalePnpm({ log }) }));
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.deepEqual(log.map((c) => c.args[0] === "--dir" ? "test" : c.args[1]).sort(), ["biome", "eslint", "test", "tsc"], "tsc, eslint, biome and the tests all ran");
  assert.ok(log.every((c) => c.flag === "false"), "with the check off in each child's environment");
  // The stand-in does abort when the setting is absent: that is what the unfixed plan hit.
  const stripped = { ...plan, scans: plan.scans.map((s) => (s.tool === "pnpm" && s.class === "B" ? { ...s, env: Object.fromEntries(Object.entries(s.env ?? {}).filter(([k]) => k !== "pnpm_config_verify_deps_before_run")) } : s)) };
  const aborted = await runScans(stripped, deps({ ...fs, spawn: stalePnpm() }));
  assert.equal(aborted.ok, false);
  assert.match(aborted.scans.find((r) => r.id === "tsc:app").detail, /ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY/);
});

test("B4: turning the check off masks no real failure: a tool or test that fails still fails the scan with its own output", async () => {
  const { fs, plan } = planFor(["src/a.ts"], jsApp, pnpmUnit);
  const toolResult = { status: 2, stdout: "src/a.ts(1,7): error TS2322: Type 'string' is not assignable to type 'number'.\n", stderr: "" };
  const result = await runScans(plan, deps({ ...fs, spawn: stalePnpm({ toolResult }) }));
  assert.equal(result.ok, false);
  for (const id of ["tsc:app", "eslint:app", "biome:app", "test:npm:app"]) {
    const row = result.scans.find((r) => r.id === id);
    assert.equal(row.status, "fail", id);
    assert.match(row.detail, /error TS2322/, `${id} reports the tool's own failure`);
  }
});

test("B4: a failed scan's detail leads with pnpm's bracketed [ERR_PNPM_...] line, not the tail of a stack", () => {
  const detail = scansModule.failureDetail({ status: 1, stdout: "", stderr: PNPM_ABORT });
  assert.match(detail, /^\[ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY\] Aborted removal of modules directory due to no TTY \|/, detail);
  assert.match(detail, /runDepsStatusCheck/, "the stack tail is still there after it");
  // The other shapes it reads are unchanged: a bare code, a failed test, a compiler error, a panic.
  assert.match(scansModule.failureDetail({ stderr: " ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL  Command failed\n    at x\n" }), /^ ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL/);
  assert.match(scansModule.failureDetail({ stdout: "x\ntest foo ... FAILED\ny\n" }), /^test foo \.\.\. FAILED/);
  assert.match(scansModule.failureDetail({ stderr: "noise\nerror[E0308]: mismatched types\n" }), /^error\[E0308\]: mismatched types/);
});

// ---- B1: ci/gate.config.json npm.<unit>.requiresBroker ---------------------------------------------------------------------------------------------------------

test("B1: declaredBrokerCall reads { reason } only; whitespace is collapsed, a long reason is cut, anything else is a problem or no declaration", () => {
  const { declaredBrokerCall } = scansModule;
  assert.deepEqual(declaredBrokerCall("app", { reason: "  runs\n  verify-local.mjs  " }), { call: { tool: "rustright", via: ["test"], command: "ci/gate.config.json npm.app.requiresBroker: runs verify-local.mjs", declared: true } });
  const long = declaredBrokerCall("app", { reason: "x".repeat(500) }).call.command;
  assert.equal(long, `ci/gate.config.json npm.app.requiresBroker: ${"x".repeat(400)}...`);
  for (const none of [undefined, null, false]) assert.deepEqual(declaredBrokerCall("app", none), {}, String(none));
  for (const bad of [true, 0, "yes", [], {}, { reason: "" }, { reason: "   " }, { reason: 7 }, { reason: null }, [{ reason: "x" }], { Reason: "x" }, { why: "x" }]) {
    const r = declaredBrokerCall("app", bad);
    assert.equal(r.call, undefined, JSON.stringify(bad));
    assert.match(r.problem, /^ci\/gate\.config\.json npm\.app\.requiresBroker is ignored: it must be \{ "reason": /, JSON.stringify(bad));
  }
});

test("B1: planScans marks a declared unit's test:npm scan; the script chain's own finding wins; a bad declaration plans the scan unmarked with a note; other settings are unaffected", () => {
  const files = { "package.json": '{"name":"app","license":"MIT","scripts":{"test":"node -e \\"require(\'./verify-local.mjs\')\\""}}', "src/a.ts": "" };
  const declare = (requiresBroker, extra = {}) => planFor(["src/a.ts"], files, { ...pnpmUnit, packageFeatures: { app: { requiresBroker, ...extra } } }).plan;
  const marked = scanOf(declare({ reason: "verify-local.mjs spawns rustright cargo test" }), "test:npm:app");
  assert.deepEqual(marked.brokerCall, { tool: "rustright", via: ["test"], command: "ci/gate.config.json npm.app.requiresBroker: verify-local.mjs spawns rustright cargo test", declared: true });
  assert.deepEqual([marked.tool, marked.args], ["pnpm", ["--dir", ".", "run", "test"]], "the scan itself is unchanged");
  assert.equal(scanOf(planFor(["src/a.ts"], files, pnpmUnit).plan, "test:npm:app").brokerCall, undefined, "no declaration, no script that shows the call: unmarked");
  const bad = declare({ reason: "" });
  assert.equal(scanOf(bad, "test:npm:app").brokerCall, undefined);
  assert.ok(bad.notes.some((n) => /^ci\/gate\.config\.json npm\.app\.requiresBroker is ignored: /.test(n)), bad.notes.join("\n"));
  assert.ok(!declare({ reason: "x" }).notes.some((n) => /requiresBroker/.test(n)), "a good declaration adds no note");
  const quiet = declare({ reason: "x" }, { quietTests: true });
  assert.equal(scanOf(quiet, "test:npm:app").quiet, true, "quietTests still applies");
  assert.equal(scanOf(quiet, "test:npm:app").brokerCall.declared, true);
  const shown = planFor(["src/a.ts"], { ...files, "package.json": '{"name":"app","license":"MIT","scripts":{"test":"rustright cargo test"}}' }, { ...pnpmUnit, packageFeatures: { app: { requiresBroker: { reason: "x" } } } }).plan;
  assert.deepEqual(scanOf(shown, "test:npm:app").brokerCall, { tool: "rustright", via: ["test"], command: "rustright cargo test" }, "what the scripts show is reported as the scripts show it");
});

test("pnpm-audit runs the consumer's own declared policy, never a weaker one; with none declared, every dependency at every severity", () => {
  const scanArgs = (pkg) => planFor(["package.json"], { "package.json": JSON.stringify(pkg), "pnpm-lock.yaml": "lockfileVersion: '9.0'\n" }, { rustUnits: [], npmUnits: [{ name: "app", dir: "." }] }).plan.scans.find((s) => s.id === "pnpm-audit")?.args;
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { "deps:audit": "pnpm audit --prod" } }), ["audit", "--prod"], "RightSites' policy: production dependencies, every severity");
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { audit: "pnpm audit --audit-level=moderate" } }), ["audit", "--audit-level", "moderate"]);
  assert.deepEqual(scanArgs({ name: "app", license: "MIT" }), ["audit"], "no declared policy: all dependencies, all severities");
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { audit: "pnpm audit --prod && curl evil" } }), ["audit"], "anything but plain flags is not a policy");
  // Several declarations: the strictest wins, whatever their order.
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { quick: "pnpm audit --prod --audit-level high", strict: "pnpm audit" } }), ["audit"], "a later full audit is not hidden by an earlier narrow one");
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { strict: "pnpm audit", quick: "pnpm audit --prod --audit-level high" } }), ["audit"]);
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { a: "pnpm audit --prod --audit-level high", b: "pnpm audit --prod --audit-level moderate" } }), ["audit", "--prod", "--audit-level", "moderate"], "same scope, lowest level");
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { a: "pnpm audit --prod", b: "pnpm audit --dev --audit-level critical" } }), ["audit"], "different scopes: every dependency; one without a level: every severity");
  assert.deepEqual(scanArgs({ name: "app", license: "MIT", scripts: { a: "pnpm audit --audit-level=critical", b: "pnpm audit --prod && curl evil" } }), ["audit", "--audit-level", "critical"], "a non-policy script declares nothing");
});

test("tool version probes run outside the repository with pnpm hooks and corepack auto-pinning off; the scans themselves keep their own roots", async () => {
  const { fs, plan } = planFor(["package.json"], { "package.json": '{"name":"app","license":"MIT"}', "pnpm-lock.yaml": "lockfileVersion: '9.0'\n" }, { rustUnits: [], npmUnits: [{ name: "app", dir: "." }] });
  const calls = [];
  const base = fakeSpawn();
  const spawn = (command, args, options = {}) => { calls.push({ command: path.basename(command), args, cwd: options.cwd, env: options.env ?? {} }); return base(command, args, options); };
  await runScans(plan, deps({ ...fs, spawn, probeDir: "/neutral/probe" }));
  const probes = calls.filter((c) => c.args.length === 1 && /version/.test(c.args[0]));
  assert.ok(probes.some((c) => c.command === "pnpm"), "pnpm was probed");
  for (const p of probes) {
    assert.equal(p.cwd, "/neutral/probe", `${p.command} probe runs outside the repository`);
    assert.equal(p.env.npm_config_ignore_pnpmfile, "true"); assert.equal(p.env.pnpm_config_ignore_pnpmfile, "true");
    assert.equal(p.env.npm_config_ignore_scripts, "true"); assert.equal(p.env.COREPACK_ENABLE_AUTO_PIN, "0");
  }
  // Positive control: the audit scan itself still runs in the repository's content root.
  const audit = calls.find((c) => c.command === "pnpm" && c.args[0] === "audit");
  assert.ok(audit && audit.cwd.startsWith(CONTENT), "the scan runs where its lockfile is");
});

test("cargoLiveRoot: cargo scans take the cargoLive root, the plan records it and says why, and runScans resolves it to that directory for the command and the rustright probe", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { cargoLiveRoot: "/primary/checkout" });
  for (const s of plan.scans.filter((x) => x.tool === "rustright")) assert.equal(s.root, "cargoLive", s.id);
  assert.equal(plan.roots.cargoLiveRoot, "/primary/checkout");
  assert.ok(plan.notes.some((n) => /broker host: cargo scans run in the primary checkout \/primary\/checkout/.test(n)), plan.notes.join(" | "));
  const calls = [];
  const base = fakeSpawn();
  const spawn = (command, args, options = {}) => { calls.push({ command: path.basename(command), args, cwd: options.cwd }); return base(command, args, options); };
  await runScans(plan, deps({ ...fs, spawn, probeDir: "/neutral/probe" }), { classes: ["A"] });
  const cargoCalls = calls.filter((c) => c.command === "rustright");
  assert.ok(cargoCalls.length > 0, "rustright ran");
  for (const c of cargoCalls) assert.ok(c.cwd === "/primary/checkout" || c.cwd.startsWith("/primary/checkout" + path.sep), `${c.args.join(" ")} ran in ${c.cwd}`);
  const without = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]).plan;
  for (const s of without.scans.filter((x) => x.tool === "rustright")) assert.equal(s.root, "live", s.id);
  assert.equal(without.roots.cargoLiveRoot, undefined);
});

test("deferred package list: on a broker host (cargoLiveRoot) every cargo-package-list scan is deferred for rightkit#1216 and reports deferred; without cargoLiveRoot it has no deferral and passes", async () => {
  const broker = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { cargoLiveRoot: "/primary/checkout", tier: "release" });
  const packageLists = broker.plan.scans.filter((s) => s.id.startsWith("cargo-package-list"));
  assert.ok(packageLists.length > 0, "the plan has a package-list scan");
  for (const s of packageLists) assert.match(s.deferred, /rightkit#1216/, s.id);
  const result = await runScans(broker.plan, deps({ ...broker.fs }), { classes: ["A"] });
  const row = result.scans.find((s) => s.id === "cargo-package-list:foo");
  assert.equal(row.status, "deferred");
  assert.match(row.detail, /rightkit#1216/);
  const direct = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { tier: "release" });
  const plain = direct.plan.scans.find((s) => s.id === "cargo-package-list:foo");
  assert.equal(plain.deferred, undefined, "a direct host runs the package list");
  const plainResult = await runScans(direct.plan, deps({ ...direct.fs }), { classes: ["A"] });
  assert.equal(plainResult.scans.find((s) => s.id === "cargo-package-list:foo").status, "pass");
  // The compile tier (no tier) plans no package list at all, broker host or not.
  const compile = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { cargoLiveRoot: "/primary/checkout" });
  assert.deepEqual(compile.plan.scans.filter((s) => s.id.startsWith("cargo-package-list")).map((s) => s.id), []);
});

test("rule tiers: the compile tier (the default) plans no shellcheck and no shellcheck requirement for actionlint; the release tier plans both; an unknown tier throws", () => {
  const files = { "deploy/release.sh": "#!/bin/sh\necho hi\n", ".github/workflows/ci.yml": "on: push\njobs: {}\n" };
  const compile = planFor(Object.keys(files), files, { all: true }).plan;
  assert.equal(compile.scans.find((s) => s.id === "shellcheck"), undefined, "shellcheck was added after 0.6.6: release tier only");
  assert.deepEqual(compile.scans.find((s) => s.id === "actionlint").requires, []);
  assert.ok(compile.notes.some((n) => /compile tier: shellcheck .* release tier/.test(n)), compile.notes.join(" | "));
  const release = planFor(Object.keys(files), files, { all: true, tier: "release" }).plan;
  assert.equal(release.scans.find((s) => s.id === "shellcheck").tier, "release");
  assert.deepEqual(release.scans.find((s) => s.id === "actionlint").requires, ["shellcheck"]);
  assert.throws(() => planFor(Object.keys(files), files, { all: true, tier: "patch" }), /tier must be compile or release/);
});

test("the rustright probe runs where its scan runs (the client refuses a directory outside a Cargo workspace in a repository); every other probe stays outside the repository (rightkit#1215)", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml", "package.json"], { "package.json": '{"name":"app","license":"MIT"}', "pnpm-lock.yaml": "lockfileVersion: '9.0'\n" }, { npmUnits: [{ name: "app", dir: "." }] });
  const calls = [];
  const base = fakeSpawn();
  const spawn = (command, args, options = {}) => { calls.push({ command: path.basename(command), args, cwd: options.cwd }); return base(command, args, options); };
  await runScans(plan, deps({ ...fs, spawn, probeDir: "/neutral/probe" }));
  const rustright = calls.find((c) => c.command === "rustright" && c.args.join(" ") === "cargo --version");
  assert.ok(rustright, "rustright was probed");
  assert.notEqual(rustright.cwd, "/neutral/probe");
  assert.ok(rustright.cwd === LIVE || rustright.cwd.startsWith(LIVE + path.sep) || rustright.cwd === CONTENT || rustright.cwd.startsWith(CONTENT + path.sep), `the probe ran in the scanned tree, not ${rustright.cwd}`);
  for (const p of calls.filter((c) => c.command !== "rustright" && c.args.length === 1 && /version/.test(c.args[0]))) assert.equal(p.cwd, "/neutral/probe", `${p.command} probe stays outside the repository`);
});

test("broker shim probe: with cargoCommand [\"cargo\"] and cargoLiveRoot, the cargo --version probe runs in the primary checkout, while pnpm's probe stays in the neutral directory", async () => {
  const files = { "package.json": '{"name":"app","license":"MIT"}', "pnpm-lock.yaml": "lockfileVersion: '9.0'\n" };
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml", "package.json"], files, { npmUnits: [{ name: "app", dir: "." }], cargoLiveRoot: "/primary/checkout", cargoCommand: ["cargo"] });
  assert.ok(plan.scans.some((s) => s.tool === "cargo"), "the cargo scans run the plain cargo shim");
  const calls = [];
  const base = fakeSpawn();
  const spawn = (command, args, options = {}) => { calls.push({ command: path.basename(command), args, cwd: options.cwd }); return base(command, args, options); };
  await runScans(plan, deps({ ...fs, spawn, probeDir: "/neutral/probe" }), { classes: ["A"] });
  const cargoProbe = calls.find((c) => c.command === "cargo" && c.args.length === 1 && c.args[0] === "--version");
  assert.ok(cargoProbe, "cargo was probed");
  assert.ok(cargoProbe.cwd === "/primary/checkout" || cargoProbe.cwd.startsWith("/primary/checkout" + path.sep), `the cargo probe ran in ${cargoProbe.cwd}, not the neutral directory`);
  const pnpmProbe = calls.find((c) => c.command === "pnpm" && c.args.length === 1 && c.args[0] === "--version");
  assert.ok(pnpmProbe, "pnpm was probed");
  assert.equal(pnpmProbe.cwd, "/neutral/probe", "pnpm's probe stays outside the repository");
});

test("shellcheck: every tracked .sh/.bash script is a class A scan; a zsh script is left to its own shell", () => {
  const files = { "deploy/release.sh": "#!/usr/bin/env bash\necho hi\n", "tools/x.bash": "#!/bin/bash\n", "spikes/run-all.sh": "#!/bin/zsh\necho z\n" };
  const { plan } = planFor(Object.keys(files), files, { all: true, tier: "release" });
  const scan = plan.scans.find((s) => s.id === "shellcheck");
  assert.equal(scan.class, "A");
  assert.deepEqual(scan.args.filter((a) => !a.startsWith("--")), ["deploy/release.sh", "tools/x.bash"]);
});

// SellRight's own scripts (original bytes, red, repaired and the disable-comment positive control) run through the real
// rightkit-gate check in rightkit-gate.test.mjs ("class A ShellCheck: ..."); these hold the version pin itself.
test("shellcheck version pin: class A accepts only PINNED_SCAN_TOOLS.shellcheck, reads it past the banner, and a wrong version fails TOOL_VERSION in the scan and in actionlint, never passing or skipping", async () => {
  assert.equal(PINNED_SCAN_TOOLS.shellcheck, "0.11.0");
  assert.equal(pinnedVersionProblem("shellcheck", "shellcheck 0.11.0"), null);
  assert.match(pinnedVersionProblem("shellcheck", "shellcheck 0.10.0"), /found "shellcheck 0\.10\.0", class A requires shellcheck 0\.11\.0/);
  assert.match(pinnedVersionProblem("shellcheck", null), /no readable version/);
  assert.equal(pinnedVersionProblem("actionlint", "1.0.0"), null, "a tool with no pin is not judged here");
  const files = { "deploy/release.sh": "#!/bin/sh\necho hi\n", ".github/workflows/ci.yml": "on: push\njobs: {}\n" };
  const { fs, plan } = planFor(Object.keys(files), files, { all: true, tier: "release" });
  const good = await runScans(plan, deps({ ...fs }));
  assert.equal(good.ok, true, JSON.stringify(scanFailures(good)));
  assert.equal(good.toolVersions.shellcheck, "shellcheck 0.11.0", "the version, not the banner's first line");
  for (const [label, version] of [["older", "0.10.0"], ["newer", "0.12.0"], ["a build string", "0.11.0-dirty"]]) {
    const log = [];
    const bad = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log, shellcheckVersion: version }) }));
    assert.equal(bad.ok, false, label);
    for (const id of ["shellcheck", "actionlint"]) {
      const row = bad.scans.find((r) => r.id === id);
      assert.equal(row.status, "fail", `${label} ${id}`);
      assert.equal(row.code, "TOOL_VERSION", `${label} ${id}`);
      assert.match(row.detail, /class A requires shellcheck 0\.11\.0/);
    }
    assert.ok(bad.scans.find((r) => r.id === "shellcheck").detail.includes(`found "shellcheck ${version}"`), label);
    assert.equal(log.filter((l) => /shellcheck|actionlint/.test(l[0]) && !l.some((a) => /version/.test(a))).length, 0, `${label}: neither analyzer ran`);
  }
});

test("shellcheck version pin: a cached pass never admits another version, and the version keys the cache (actionlint's by its shellcheck too)", async () => {
  const files = { "deploy/release.sh": "#!/bin/sh\necho hi\n", ".github/workflows/ci.yml": "on: push\njobs: {}\n" };
  const { fs, plan } = planFor(Object.keys(files), files, { all: true, tier: "release" });
  const cache = memCache(() => "digest-1");
  const first = await runScans(plan, deps({ ...fs, cache }));
  assert.equal(first.ok, true, JSON.stringify(scanFailures(first)));
  const keys = [...cache.store.keys()];
  const second = await runScans(plan, deps({ ...fs, cache }));
  assert.ok(second.cacheHits.includes("shellcheck") && second.cacheHits.includes("actionlint"), "the pinned version hits");
  const wrong = await runScans(plan, deps({ ...fs, cache, spawn: fakeSpawn({ shellcheckVersion: "0.10.0" }) }));
  assert.deepEqual(wrong.cacheHits.filter((id) => id === "shellcheck" || id === "actionlint"), [], "a cached pass is not a pass under another analyzer");
  assert.equal(wrong.scans.find((r) => r.id === "shellcheck").code, "TOOL_VERSION");
  const key = (id, toolVersion) => scanCacheKey({ id, command: plan.scans.find((s) => s.id === id).command, toolVersion, env: {}, inheritedEnv: inheritedEnvDigest({}), digest: "digest-1" });
  assert.ok(keys.includes(key("shellcheck", "shellcheck 0.11.0")), "shellcheck's key carries its version");
  assert.ok(keys.includes(key("actionlint", "actionlint 1.0.0 + shellcheck 0.11.0")), "actionlint's key carries the shellcheck it runs");
  assert.notEqual(key("shellcheck", "shellcheck 0.11.0"), key("shellcheck", "shellcheck 0.10.0"));
});
