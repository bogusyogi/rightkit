import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import {
  CLASS_A_SCAN_PREFIXES, CLASS_B_SCAN_PREFIXES, canonicalJson, checkArgs, clippyArgs, digestScanSet, docArgs, fmtCheckArgs,
  defaultSpawn, deterministicScansEnabled, metadataArgs, planScans, resolveInvocation, runScans, scanClassOf, scanDeferrals, scanFailures,
} from "./deterministic-scans.mjs";

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
  for (const id of ["check:foo", "check-all-features:foo", "clippy:foo", "doc:foo", "test:rust:foo"]) assert.ok(b.includes(id), `class B has ${id}`);
  for (const s of plan.scans.filter((s) => s.tool === "rightkit")) assert.equal(s.args[0], "cargo", `${s.id} runs through rightkit cargo`);
  const doc = plan.scans.find((s) => s.id === "doc:foo");
  assert.equal(doc.env.RUSTDOCFLAGS, "-D warnings");
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

test("runner: Rust scans pass RUSTDOCFLAGS and run in the live root; content scans in the content root", async () => {
  const { fs, plan } = planFor(["crates/foo/src/lib.rs", "crates/foo/Cargo.toml", "x.mjs"], { "x.mjs": "" });
  const calls = [];
  const spawn = (command, args, options) => { calls.push({ command, args, options }); return fakeSpawn()(command, args); };
  await runScans(plan, deps({ ...fs, spawn }));
  const doc = calls.find((c) => c.args.includes("doc"));
  assert.equal(doc.options.env.RUSTDOCFLAGS, "-D warnings");
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
  const broker = ["cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list:foo", "check:foo", "check-all-features:foo", "clippy:foo", "doc:foo", "test:rust:foo"];
  assert.deepEqual([...plan.deferred].sort(), [...broker].sort());
  for (const id of broker) assert.ok(plan.scans.find((x) => x.id === id)?.deferred, `${id} stays in the plan, deferred`);
  const log = [];
  const result = await runScans(plan, deps({ ...fs, spawn: fakeSpawn({ log }) }));
  assert.equal(result.ok, false, "a deferred scan is not a pass");
  assert.equal(result.complete, false);
  for (const id of plan.deferred) assert.equal(result.scans.find((s) => s.id === id).status, "deferred");
  assert.ok(!log.some((c) => /cargo/.test(c)), "no cargo runs outside the broker");
  assert.equal(scanFailures(result).length, 0, "deferral is reported by scanDeferrals; a local gate fails on it");
  const lines = scanDeferrals(result);
  assert.equal(lines.length, plan.deferred.length);
  for (const id of plan.deferred) assert.ok(lines.some((l) => l.startsWith(`${id}: deferred, not passed`)));
  assert.ok(plan.notes.some((n) => /deferred, not passed/.test(n) && n.includes("clippy:foo")));
  // With a grant nothing is deferred.
  assert.deepEqual(planFor(["crates/foo/Cargo.toml"]).plan.deferred, []);
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
  assert.ok(["cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list:foo", "check:foo", "clippy:foo"].every((id) => cargoScans.some((s) => s.id === id)));
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
  const swift = planFor(["swift/a.swift"], { "swift/a.swift": "" }, { platform: "darwin" }).plan;
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

test("CI rollout switch: deterministic scans are OFF unless config says enabled === true", () => {
  for (const config of [undefined, null, {}, { deterministicScans: {} }, { deterministicScans: { enabled: false } }, { deterministicScans: { enabled: "true" } }, { deterministicScans: { enabled: 1 } }]) {
    assert.equal(deterministicScansEnabled(config), false, JSON.stringify(config));
  }
  assert.equal(deterministicScansEnabled({ deterministicScans: { enabled: true } }), true);
  // The shipped CI config does not enable it, and the gate prints the explicit disabled line.
  const config = JSON.parse(readFileSync(new URL("../../ci/gate.config.json", import.meta.url), "utf8"));
  assert.equal(deterministicScansEnabled(config), false);
  const gate = readFileSync(new URL("../../scripts/ci/gate.mjs", import.meta.url), "utf8");
  assert.match(gate, /deterministic scans: disabled \(not run\)/);
  assert.match(gate, /deterministicScansEnabled\(selection\.config\)/);
});
