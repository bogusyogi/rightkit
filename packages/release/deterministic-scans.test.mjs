import crypto from "node:crypto";
import { readFileSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import {
  CLASS_A_SCAN_PREFIXES, CLASS_B_SCAN_PREFIXES, canonicalJson, fileScanCache, inheritedEnvDigest, requiredScanIds, scanCacheKey, checkArgs, clippyArgs, digestScanSet, docArgs, fmtCheckArgs,
  defaultSpawn, deterministicScansEnabled, metadataArgs, planScans, resolveInvocation, runScans, scanClassOf, scanDeferrals, scanFailures,
} from "./deterministic-scans.mjs";
import { requiredScanProblems } from "./scan-digest.mjs";

const CONTENT = path.join(path.sep, "c", "rightkit");
const LIVE = path.join(path.sep, "l", "rightkit");
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

test("plan: a changed crate selects class A and class B scans, through rightkit cargo", () => {
  const { plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const a = ids(plan, "A"); const b = ids(plan, "B");
  for (const id of ["rustfmt:2024", "toml-parse", "cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list:foo", "changelog-version:foo", "license", "secret-scan"]) assert.ok(a.includes(id), `class A has ${id}`);
  for (const id of ["check-features:foo:0", "clippy:foo", "doc:foo", "test:rust:foo"]) assert.ok(b.includes(id), `class B has ${id}`);
  assert.ok(!b.includes("check:foo"), "default clippy already type-checks every target: no separate default check");
  for (const s of plan.scans.filter((s) => s.tool === "rightkit")) assert.equal(s.args[0], "cargo", `${s.id} runs through rightkit cargo`);
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
function fakeSpawn({ missing = [], failing = [], log = [] } = {}) {
  return (command, args) => {
    log.push([command, ...args]);
    const name = path.basename(command).replace(/\.exe$/, "");
    if (missing.includes(name)) return { error: Object.assign(new Error(`spawn ${name} ENOENT`), { code: "ENOENT" }), status: null };
    const isProbe = args.length === 1 && args[0].includes("version") || (args.length === 2 && args[0] === "cargo" && args[1] === "--version");
    if (isProbe) return { status: 0, stdout: `${name} 1.0.0\n`, stderr: "" };
    if (failing.some((f) => args.join(" ").includes(f))) return { status: 1, stdout: "", stderr: "boom\n" };
    return { status: 0, stdout: "", stderr: "" };
  };
}
const deps = (over = {}) => ({ spawn: fakeSpawn(), ...fakeFs({}), findRustfmt: () => "rustfmt", loadToml: async () => () => ({}), scanSecrets: async () => () => [], env: {}, execPath: "node", ...over });

test("runner: all scans pass and the result is a plain receipt-ready object", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const result = await runScans(plan, deps({ ...fs }));
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.deepEqual(JSON.parse(JSON.stringify(result)), result);
  assert.equal(result.scanSetDigest, plan.scanSetDigest);
  assert.ok(result.toolVersions.rightkit);
  assert.ok(result.scans.every((s) => s.status === "pass"));
  const order = result.scans.map((s) => s.class).join("");
  assert.match(order, /^A+B+$/, "class A runs before class B");
});

test("runner: a missing required tool FAILS, never skips", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ missing: ["rightkit"] }) }));
  assert.equal(result.ok, false);
  const metadata = result.scans.find((s) => s.id === "cargo-metadata");
  assert.equal(metadata.status, "fail");
  assert.equal(metadata.code, "TOOL_MISSING");
  assert.match(metadata.detail, /never skipped/);
  assert.equal(result.toolVersions.rightkit, null);
  assert.ok(!result.scans.some((s) => s.status === "skipped"));
});

test("runner: missing rustfmt and missing TOML parser fail", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const result = await runScans(plan, deps({ ...fs, findRustfmt: () => null, loadToml: async () => { throw new Error("nope"); } }));
  assert.equal(result.scans.find((s) => s.id === "rustfmt:2024").code, "TOOL_MISSING");
  assert.equal(result.scans.find((s) => s.id === "toml-parse").code, "TOOL_MISSING");
});

test("runner: a failed class A scan leaves class B not-run", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ failing: ["deny check"], log }) }));
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

test("no local compile grant: every broker scan is deferred by name, never passed and never run outside the broker", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { localCompile: false });
  const broker = ["cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list:foo", "check-features:foo:0", "clippy:foo", "doc:foo", "test:rust:foo"];
  assert.deepEqual([...plan.deferred].sort(), [...broker].sort());
  for (const id of broker) assert.ok(plan.scans.find((x) => x.id === id)?.deferred, `${id} stays in the plan, deferred`);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }) }));
  assert.equal(result.ok, false, "a deferred scan is not a pass");
  assert.equal(result.complete, false);
  const classA = ["cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list:foo"];
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
  const present = new Set(["C:\\bin\\pnpm.cmd", "C:\\tools\\rightkit.exe", "C:\\tools\\rightkit.cmd", "C:\\bin\\other.cmd"].map((p) => path.win32.normalize(p)));
  const exists = (p) => present.has(path.win32.normalize(p));
  const inv = (command, args, over = {}) => resolveInvocation(command, args, { platform: "win32", env, exists, execPath: "node.exe", ...over });
  // .exe is preferred over .cmd and started directly, with the argument as one argv element.
  const exe = inv("rightkit", ["cargo", "metadata", "& echo X"]);
  assert.match(exe.file, /rightkit\.exe$/i);
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
    for (const s of plan.scans.filter((x) => x.tool === "rightkit" || x.tool === "pnpm" || x.tool === "cargo")) {
      assert.ok(!s.args.some((a) => a.includes(name)), `${name} reaches no shell-routed argv (${s.id})`);
    }
    const log = [];
    await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }) }));
    const shellRouted = log.filter((c) => ["rightkit", "pnpm", "cargo"].includes(path.basename(c[0])));
    assert.ok(!shellRouted.some((c) => c.slice(1).some((a) => a.includes(name))), `${name} never spawned through rightkit or pnpm`);
  }
});

test("resolveInvocation: Windows .cmd shims take only strictly plain arguments; anything else is refused, not escaped", () => {
  const exists = (p) => ["C:\\bin\\rightkit.cmd", "C:\\bin\\pnpm.cmd", "C:\\bin\\tool.exe"].includes(p);
  const env = { Path: "C:\\bin", PATHEXT: ".EXE;.CMD", ComSpec: "C:\\Windows\\System32\\cmd.exe" };
  const inv = (command, args) => resolveInvocation(command, args, { platform: "win32", env, exists, execPath: "node.exe" });
  for (const bad of ["a&b", "a|b", "a^b", "a%b", 'a"b', "a b", "a>b", "a<b", "a;b", "a`b", "a$b", "a(b", "a!b", "*", "a\nb"]) {
    const r = inv("rightkit", ["cargo", `crates/${bad}`]);
    assert.ok(r.error, `${JSON.stringify(bad)} refused`);
    assert.match(r.error.message, /refusing/);
    assert.equal(r.file, undefined);
  }
  const ok = inv("rightkit", ["cargo", "metadata", "--format-version", "1", "-p", "rightkit-fs", "crates/foo/Cargo.toml"]);
  assert.equal(ok.error, undefined);
  assert.equal(ok.verbatim, true);
  assert.equal(ok.file, "C:\\Windows\\System32\\cmd.exe");
  assert.deepEqual(ok.args.slice(0, 3), ["/d", "/s", "/c"]);
  assert.equal(ok.args[3], '""C:\\bin\\rightkit.cmd" cargo metadata --format-version 1 -p rightkit-fs crates/foo/Cargo.toml"');
  // A real executable is never routed through cmd.exe, so its arguments are not shell-parsed.
  assert.deepEqual(inv("tool", ["a&b"]), { file: "C:\\bin\\tool.exe", args: ["a&b"], verbatim: false });
  // A shim whose own path is unsafe is refused.
  assert.ok(resolveInvocation("rightkit", ["cargo"], { platform: "win32", env: { Path: "C:\\b&d" }, exists: (p) => p === "C:\\b&d\\rightkit.cmd", execPath: "node.exe" }).error);
  // POSIX passes arguments through untouched (spawned without a shell).
  assert.deepEqual(resolveInvocation("rightkit", ["cargo", "a&b"], { platform: "linux" }), { file: "rightkit", args: ["cargo", "a&b"], verbatim: false });
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
  const other = planFor(["crates/foo/Cargo.toml"]).plan;
  assert.notEqual(other.classDigests.A, plan.classDigests.A);
  assert.equal((await runScans(other, deps({ ...fakeFs(crateFiles, crateFiles), classes: ["B"], priorClassA: a }))).scans.every((s) => s.code === "CLASS_A_REQUIRED"), true);
});

test("plan: cargoCommand selects the cargo prefix (rightkit cargo locally, plain cargo in CI)", () => {
  const ci = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], {}, { cargoCommand: ["cargo"] }).plan;
  const cargoScans = ci.scans.filter((s) => s.tool === "cargo");
  assert.ok(["cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list:foo", "clippy:foo"].every((id) => cargoScans.some((s) => s.id === id)));
  assert.ok(cargoScans.every((s) => s.args[0] !== "cargo"));
  assert.equal(cargoScans.find((s) => s.id === "cargo-deny").command, "cargo deny check");
  assert.ok(!ci.scans.some((s) => s.tool === "rightkit"));
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
  assert.ok(scanOf(plan, "cargo-package-list:app-core"));
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
  const p = planFor(["core/src/lib.rs"], unitFiles, { rustUnits: [{ name: "app-core", dir: "core", publish: false }], npmUnits: [] }).plan;
  assert.ok(!p.scans.some((s) => s.id.startsWith("cargo-package-list")));
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
  for (const s of one.scans.filter((x) => x.tool === "rightkit")) assert.equal(s.cwd, "ws", s.id);
  const lock = planFor(["ws/Cargo.lock"], files, opts).plan;
  assert.ok(lock.scans.some((s) => s.id === "clippy:a") && lock.scans.some((s) => s.id === "clippy:b"));
  const stray = planFor(["Cargo.lock"], files, opts).plan;
  assert.ok(!stray.scans.some((s) => s.id.startsWith("clippy:")), "a lock outside the workspace is not workspace-wide for it");
  assert.ok(planFor(["crates/foo/src/lib.rs"]).plan.scans.every((s) => s.cwd === undefined), "no cwd by default");
  assert.throws(() => planFor(["README.md"], { "README.md": "x" }, { cargoDir: "../x" }), /cargoDir/);
});

test("generated-docs runs only when a changed file is an input of scripts/generate-index.mjs", () => {
  const extra = {
    "scripts/generate-index.mjs": "", "Cargo.toml": "[workspace]\n", "INDEX.md": "x", "README.md": "x", "docs/ADOPTION.md": "x", "docs/CAPABILITY-MAP.md": "x",
    "crates/foo/README.md": "x", "packages/p/README.md": "x", "packages/p/package.json": '{"license":"MIT"}', "packages/p/src/a.ts": "",
    "packages/release/rightkit-versions.json": "{}", "packages/release/rightkit-ownership.json": "{}", "packages/release/ownership-availability.mjs": "",
  };
  const has = (f) => planFor([f], extra).plan.scans.some((s) => s.id === "generated-docs");
  for (const f of ["crates/foo/Cargo.toml", "crates/foo/README.md", "packages/p/package.json", "packages/p/README.md", "Cargo.toml", "packages/release/rightkit-versions.json",
    "packages/release/rightkit-ownership.json", "packages/release/ownership-availability.mjs", "scripts/generate-index.mjs", "INDEX.md", "docs/ADOPTION.md", "docs/CAPABILITY-MAP.md"]) {
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

test("cache: identical inputs hit; only class B passes are stored and the hit is reported as a pass", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"]);
  const cache = memCache(() => "digest-1");
  const first = await runScans(plan, deps({ ...fs, cache }));
  assert.equal(first.ok, true, JSON.stringify(scanFailures(first)));
  assert.deepEqual(first.cacheHits, []);
  assert.equal(cache.store.size, bRows(first).length, "one entry per class B pass, none for class A");
  const log = [];
  const second = await runScans(plan, deps({ ...fs, cache, spawn: fakeSpawn({ log }) }));
  assert.equal(second.ok, true);
  assert.deepEqual(second.cacheHits, bRows(second).map((r) => r.id).sort());
  assert.ok(bRows(second).every((r) => r.status === "pass" && r.cached === true && r.detail === "cached 2026-10-22"));
  assert.equal(spawned(log, "clippy") + spawned(log, "test"), 0, "no cached scan was spawned");
  assert.equal(second.scanSetDigest, plan.scanSetDigest);
  assert.ok(second.scans.filter((r) => r.class === "A").every((r) => !r.cached), "class A is never cached");
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
  const cache = memCache(() => "d");
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
  const volatile = { PWD: "/a", OLDPWD: "/b", SHLVL: "2", _: "/x", PROMPT: "p", PS1: "q", TERM_SESSION_ID: "t", WT_SESSION: "w", WEZTERM_PANE: "9", SSH_AUTH_SOCK: "/s", CLAUDE_SESSION: "z", RANDOM: "42" };
  assert.equal(inheritedEnvDigest({ CI: "true", PATH: "/bin" }), inheritedEnvDigest({ CI: "true", PATH: "/bin", ...volatile }), "volatile names do not change the digest");
  assert.notEqual(inheritedEnvDigest({ CI: "true" }), inheritedEnvDigest({ CI: "true", FOO: "1" }));
  assert.notEqual(inheritedEnvDigest({ CI: "true" }), inheritedEnvDigest({ CI: "True" }));
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
    const plan = planScans({ changed: Object.keys(all), contentRoot: CONTENT, liveRoot: LIVE, ...fs, platform: "win32", includeTests: true, crateFeatures, localCompile: true, cargoCommand: ["rightkit", "cargo"] });
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
  for (const id of ["check-features:hub/src-tauri:app:0", "doc:hub/src-tauri:app", "test:rust:hub/src-tauri:app", "cargo-metadata:hub/src-tauri", "cargo-package-list:hub/src-tauri:app"]) {
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
  assert.ok(!bad.scans.some((s) => s.tool === "rightkit"));
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
  for (const s of p.scans.filter((x) => x.tool === "rightkit")) assert.equal(s.cwd, "ws", s.id);
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
