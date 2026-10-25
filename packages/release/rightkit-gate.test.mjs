import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import * as shipped from "./lock-graph.mjs";
import * as ci from "../../scripts/ci/select.mjs";
import { spawnSync } from "./background-process.mjs";
import { HOOK_MARKER, renderPreCommitHook } from "./ownership-hooks.mjs";
import { digestScanSet } from "./scan-digest.mjs";
import {
  DEFER_OPT_OUT_ENV, EXTERNAL_INPUT_FILE_LIMIT, PROTECTED_BRANCHES, QUIET_LEASE_TTL_MS, QUIET_LEASE_WAIT_DRAIN_MS, buildCacheDigest, deferredBlock, defaultBranchNames, externalInputs, discoverNpmUnits, discoverRustUnits, discoverRustWorkspaces, globToRegExp, hasTomlTable, hookText, isDeferrableRef, lockScope, main, manifestChangeIsLockReflected, manifestTables, manifestWorkspaceMetadata, parsePushLines, parseWorkspaceGlobs, prePushPlan, quietLeaseLane, rustDeferral, rustUnitName, rustUnitOfScanId, tomlString, tomlStrings, tomlValue, trampolineText, unmatchedManifests, withoutScans, workspaceExcludes,
} from "./bin/rightkit-gate.mjs";
import * as gateModule from "./bin/rightkit-gate.mjs";

// Read off the module so a test of a function that does not exist yet fails on its own, not with an import error for the whole file.
const { discoverPnpmRoots } = gateModule;

const ok = (stdout = "") => ({ status: 0, stdout, stderr: "" });
const no = (stderr = "") => ({ status: 1, stdout: "", stderr });
const norm = (p) => p.replaceAll("\\", "/");

function mkRepo(files, t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-test-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [name, body] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
    fs.writeFileSync(path.join(root, name), body);
  }
  return root;
}
const pkg = (name, extra = "") => `[package]\nname = "${name}"\nversion = "1.0.0"\nlicense = "MIT"\nedition = "2021"\n${extra}`;
const lockPkg = (name, version, deps = []) => `[[package]]\nname = "${name}"\nversion = "${version}"\n${deps.length ? `dependencies = [\n${deps.map((d) => ` "${d}",`).join("\n")}\n]\n` : ""}\n`;
const meta = (root, wsDir, members) => JSON.stringify({
  workspace_root: path.join(root, wsDir),
  packages: members.map(([name, dir, publish = null]) => ({ name, manifest_path: path.join(root, dir, "Cargo.toml"), publish })),
});

// ---- lock graph parity -----------------------------------------------------------------------------------------

const fnSource = (text, name) => {
  const start = text.indexOf(`export function ${name}(`);
  assert.ok(start >= 0, `${name} found`);
  return text.slice(start, text.indexOf("\n}\n", start) + 3);
};

test("lock-graph.mjs is identical to scripts/ci/select.mjs: same source, same results on a fixture", () => {
  const a = fs.readFileSync(new URL("./lock-graph.mjs", import.meta.url), "utf8");
  const b = fs.readFileSync(new URL("../../scripts/ci/select.mjs", import.meta.url), "utf8");
  for (const name of ["parseLock", "changedLockNames", "reach"]) assert.equal(fnSource(a, name), fnSource(b, name), `${name} differs`);
  const before = lockPkg("serde", "1.0.0") + lockPkg("foo", "1.0.0", ["serde"]) + lockPkg("bar", "1.0.0", ["foo 1.0.0"]) + lockPkg("baz", "1.0.0");
  const after = lockPkg("serde", "1.0.1") + lockPkg("foo", "1.0.0", ["serde"]) + lockPkg("bar", "1.0.0", ["foo 1.0.0"]) + lockPkg("baz", "1.0.0") + lockPkg("new", "0.1.0", ["bar"]);
  const view = (lock) => [...lock].map(([n, e]) => [n, e.signatures, [...e.deps].sort()]);
  assert.deepEqual(view(shipped.parseLock(after)), view(ci.parseLock(after)));
  const s = [shipped.parseLock(before), shipped.parseLock(after)]; const c = [ci.parseLock(before), ci.parseLock(after)];
  assert.deepEqual([...shipped.changedLockNames(...s)].sort(), [...ci.changedLockNames(...c)].sort());
  assert.deepEqual([...shipped.changedLockNames(...s)].sort(), ["new", "serde"]);
  for (const name of ["foo", "bar", "baz", "new"]) assert.deepEqual([...shipped.reach(s[1], name)].sort(), [...ci.reach(c[1], name)].sort(), name);
  assert.deepEqual([...shipped.reach(s[1], "new")].sort(), ["bar", "foo", "serde"]);
});

// ---- unit discovery --------------------------------------------------------------------------------------------

test("pnpm workspace globs: block and flow lists, negations, one-segment and any-depth stars", () => {
  const yaml = "packages:\n  - 'packages/*'\n  - \"apps/**\"\n  - '!packages/b' # skip\nother:\n  - nope\n";
  assert.deepEqual(parseWorkspaceGlobs(yaml), ["packages/*", "apps/**", "!packages/b"]);
  assert.deepEqual(parseWorkspaceGlobs("packages: ['a/*', \"b\"]\n"), ["a/*", "b"]);
  assert.deepEqual(parseWorkspaceGlobs("packages:\n- x/*\n"), ["x/*"]);
  assert.ok(globToRegExp("packages/*").test("packages/a") && !globToRegExp("packages/*").test("packages/a/b"));
  assert.ok(globToRegExp("apps/**").test("apps/a/b") && globToRegExp("a/**/c").test("a/c") && globToRegExp("a/**/c").test("a/x/y/c"));
  assert.ok(globToRegExp("a.b").test("a.b") && !globToRegExp("a.b").test("axb"));
});

test("npmUnits: workspace globs over tracked manifests, scoped names, collisions fall back to directories; no workspace uses the root package", () => {
  const files = new Map([
    ["pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n  - 'apps/**'\n  - '!packages/b'\n"],
    ["packages/a/package.json", '{"name":"@scope/a"}'], ["packages/b/package.json", '{"name":"b"}'], ["tools/t/package.json", '{"name":"t"}'],
    ["apps/web/package.json", '{"name":"web"}'], ["apps/web2/package.json", '{"name":"web"}'], ["package.json", '{"name":"root"}'],
  ].map(([k, v]) => [norm(path.join("/r", k)), v]));
  const exists = (p) => files.has(norm(p));
  const readFile = (p) => files.get(norm(p));
  const tracked = () => ["package.json", "packages/a/package.json", "packages/b/package.json", "tools/t/package.json", "apps/web/package.json", "apps/web2/package.json"];
  assert.deepEqual(discoverNpmUnits({ root: "/r", tracked, readFile, exists }), [
    { name: "apps-web", dir: "apps/web" }, { name: "apps-web2", dir: "apps/web2" }, { name: "scope-a", dir: "packages/a" },
  ]);
  const solo = (p) => norm(p) !== norm("/r/pnpm-workspace.yaml") && exists(p);
  assert.deepEqual(discoverNpmUnits({ root: "/r", tracked, readFile, exists: solo }), [{ name: "root", dir: "." }]);
  assert.deepEqual(discoverNpmUnits({ root: "/r", tracked, readFile, exists: () => false }), []);
});

test("rustUnits: names and directories come from cargo metadata, run through the cargo command in the workspace", (t) => {
  const root = mkRepo({ "Cargo.toml": "[workspace]\n", "crates/a/Cargo.toml": pkg("a"), "tools/b/Cargo.toml": pkg("b", "publish = false\n"), "fixtures/old/Cargo.toml": pkg("old") }, t);
  const calls = [];
  const spawn = (cmd, args, opts) => { calls.push([cmd, args, opts.cwd]); return ok(meta(root, ".", [["b", "tools/b", []], ["a", "crates/a"]])); };
  const tracked = () => ["Cargo.toml", "crates/a/Cargo.toml", "tools/b/Cargo.toml", "fixtures/old/Cargo.toml"];
  const found = discoverRustUnits({ root, spawn, cargoCommand: ["rustright", "cargo"], tracked, readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.deepEqual(found.units, [{ name: "a", dir: "crates/a", publish: true }, { name: "b", dir: "tools/b", publish: false }]);
  assert.equal(found.cargoDir, ".");
  assert.deepEqual(found.uncovered, ["fixtures/old/Cargo.toml"], "a crate outside the workspace is reported, not silently dropped");
  assert.deepEqual(calls[0][1], ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"]);
  assert.equal(calls[0][0], "rustright");
  assert.equal(path.normalize(calls[0][2]), path.normalize(root));
});

test("rustUnits: a workspace below the root, no Rust at all, several workspaces and a failing metadata", (t) => {
  const root = mkRepo({ "src-tauri/Cargo.toml": pkg("app"), "a/Cargo.toml": pkg("x"), "b/Cargo.toml": pkg("y") }, t);
  const read = (f) => fs.readFileSync(f, "utf8");
  const calls = [];
  const sub = discoverRustUnits({ root, spawn: (c, a, o) => { calls.push(o.cwd); return ok(meta(root, "src-tauri", [["app", "src-tauri"]])); }, cargoCommand: ["cargo"], tracked: () => ["src-tauri/Cargo.toml"], readFile: read });
  assert.equal(sub.cargoDir, "src-tauri");
  assert.deepEqual(sub.units, [{ name: "app", dir: "src-tauri", publish: true }]);
  assert.equal(path.normalize(calls[0]), path.normalize(path.join(root, "src-tauri")));
  assert.deepEqual(discoverRustUnits({ root, spawn: () => assert.fail("not run"), cargoCommand: ["cargo"], tracked: () => [], readFile: read }), { present: false, units: [], cargoDir: ".", uncovered: [] });
  assert.throws(() => discoverRustUnits({ root, spawn: () => ok("{}"), cargoCommand: ["cargo"], tracked: () => ["a/Cargo.toml", "b/Cargo.toml"], readFile: read }), /more than one Cargo workspace/);
  assert.throws(() => discoverRustUnits({ root, spawn: () => no("error: no lock"), cargoCommand: ["cargo"], tracked: () => ["a/Cargo.toml"], readFile: read }), /cargo metadata failed: error: no lock/);
  assert.throws(() => discoverRustUnits({ root, spawn: () => ok("nope"), cargoCommand: ["cargo"], tracked: () => ["a/Cargo.toml"], readFile: read }), /not JSON/);
});

// ---- lock scope ------------------------------------------------------------------------------------------------

test("lockScope: a lock-only change reaches the crates whose closure holds a changed package; anything else keeps every crate", () => {
  const before = lockPkg("serde", "1.0.0") + lockPkg("foo", "1.0.0", ["serde"]) + lockPkg("bar", "1.0.0", ["foo"]) + lockPkg("baz", "1.0.0");
  const after = lockPkg("serde", "1.0.1") + lockPkg("foo", "1.0.0", ["serde"]) + lockPkg("bar", "1.0.0", ["foo"]) + lockPkg("baz", "1.0.0");
  const base = { unitNames: new Set(["foo", "bar", "baz"]), showBase: () => before, readLock: () => after };
  assert.deepEqual([...lockScope({ ...base, changed: ["Cargo.lock"] })].sort(), ["bar", "foo"]);
  assert.equal(lockScope({ ...base, changed: ["Cargo.lock", "Cargo.toml"] }), null, "a manifest change with no manifest reader keeps every crate");
  // A root manifest change confined to lock-reflected tables ([patch.*], [workspace.dependencies], membership) narrows by the lock graph.
  const manifestBefore = '[workspace]\nmembers = ["crates/*"]\n\n[workspace.dependencies]\nserde = "1.0.0"\n';
  const showBoth = (f) => (f === "Cargo.toml" ? manifestBefore : before);
  assert.deepEqual([...lockScope({ ...base, changed: ["Cargo.lock", "Cargo.toml"], showBase: showBoth, readManifest: () => manifestBefore.replace('serde = "1.0.0"', 'serde = "1.0.1"') })].sort(), ["bar", "foo"], "a workspace dependency bump");
  assert.deepEqual([...lockScope({ ...base, changed: ["Cargo.lock", "Cargo.toml"], showBase: showBoth, readManifest: () => `${manifestBefore}\n[patch.crates-io]\nserde = { path = "vendor/serde" }\n` })].sort(), ["bar", "foo"], "a patch");
  assert.equal(lockScope({ ...base, changed: ["Cargo.lock", "Cargo.toml"], showBase: showBoth, readManifest: () => `${manifestBefore}\n[profile.release]\nopt-level = 2\n` }), null, "a profile change keeps every crate");
  assert.equal(lockScope({ ...base, changed: ["Cargo.lock", "Cargo.toml"], showBase: showBoth, readManifest: () => { throw new Error("unreadable"); } }), null);
  assert.equal(lockScope({ ...base, changed: ["Cargo.toml"], showBase: showBoth, readManifest: () => manifestBefore }), null, "an unchanged manifest text with no lock change is no narrowing");
  assert.equal(lockScope({ ...base, changed: ["src/a.rs"] }), null);
  assert.equal(lockScope({ ...base, changed: ["Cargo.lock"], showBase: () => null }), null, "no lock at the base");
  assert.deepEqual([...lockScope({ ...base, changed: ["Cargo.lock", "vendor/baz/src/lib.rs"] })].sort(), ["bar", "baz", "foo"]);
  const sub = lockScope({ ...base, cargoDir: "ws", changed: ["ws/Cargo.lock"], showBase: (f) => { assert.equal(f, "ws/Cargo.lock"); return before; } });
  assert.deepEqual([...sub].sort(), ["bar", "foo"]);
  assert.equal(lockScope({ ...base, cargoDir: "ws", changed: ["Cargo.lock"] }), null, "a root lock is not this workspace's");
});

// ---- install-hook ----------------------------------------------------------------------------------------------

function hookSpawn(root, state) {
  return (cmd, args) => {
    const line = args.join(" ");
    if (cmd === "git" && line === "rev-parse --show-toplevel") return ok(root);
    if (cmd === "git" && line === "config --get core.hooksPath") return state.hooksPath ? ok(`${state.hooksPath}\n`) : no();
    if (cmd === "git" && args[0] === "config" && args[1] === "core.hooksPath") { state.hooksPath = args[2]; state.sets.push(args[2]); return ok(); }
    return no(`unexpected ${cmd} ${line}`);
  };
}
const capture = () => { const out = []; const err = []; return { out, err, io: { out: (m) => out.push(m), err: (m) => err.push(m) } }; };

test("install-hook writes the hook, sets core.hooksPath, and refuses to overwrite a different hook without --force", async (t) => {
  const root = mkRepo({}, t);
  const state = { hooksPath: "", sets: [] };
  const hook = path.join(root, ".githooks", "pre-push");
  const run = async (args) => { const c = capture(); const code = await main(args, { ...c.io, spawn: hookSpawn(root, state), cwd: root }); return { code, ...c }; };

  const first = await run(["install-hook"]);
  assert.equal(first.code, 0);
  const text = fs.readFileSync(hook, "utf8");
  assert.match(text, /pnpm exec rightkit-gate "\$@"/);
  assert.match(text, /node "[^"]*bin\/rightkit-gate\.mjs" "\$@"/);
  assert.match(text, /rightkit_gate check --commit HEAD/);
  assert.match(text, /rightkit_gate --base origin\/main/);
  assert.deepEqual(state.sets, [".githooks"]);
  assert.ok(first.out.some((l) => /wrote \.githooks\/pre-push/.test(l)) && first.out.some((l) => /set core\.hooksPath to \.githooks/.test(l)));

  const again = await run(["install-hook"]);
  assert.equal(again.code, 0);
  assert.ok(again.out.some((l) => /already current/.test(l)));
  assert.deepEqual(state.sets, [".githooks"], "nothing set twice");

  fs.writeFileSync(hook, "#!/bin/sh\necho mine\n");
  state.hooksPath = ""; state.sets.length = 0;
  const refused = await run(["install-hook"]);
  assert.equal(refused.code, 1);
  assert.match(refused.err.join("\n"), /already exists and differs/);
  assert.equal(fs.readFileSync(hook, "utf8"), "#!/bin/sh\necho mine\n", "the existing hook is untouched");
  assert.deepEqual(state.sets, [], "no config change either");

  const forced = await run(["install-hook", "--force"]);
  assert.equal(forced.code, 0);
  assert.equal(fs.readFileSync(hook, "utf8"), text);
  assert.ok(forced.out.some((l) => /replaced \.githooks\/pre-push/.test(l)));

  state.hooksPath = ".husky"; state.sets.length = 0; fs.rmSync(hook);
  const other = await run(["install-hook"]);
  assert.equal(other.code, 1);
  assert.match(other.err.join("\n"), /core\.hooksPath is already ".husky"/);
  assert.equal(fs.existsSync(hook), false);
  assert.equal((await run(["install-hook", "--force"])).code, 0);
  assert.deepEqual(state.sets, [".githooks"]);
});

test("install-hook points node at the installed package when it is in node_modules", async (t) => {
  const root = mkRepo({ "node_modules/@rightkit/release/bin/rightkit-gate.mjs": "" }, t);
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookSpawn(root, { hooksPath: "", sets: [] }), cwd: root }), 0);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-push"), "utf8"), hookText("node_modules/@rightkit/release/bin/rightkit-gate.mjs"));
});

// ---- the gate with injected spawn ------------------------------------------------------------------------------

const HEAD = "a".repeat(40);
function repoSpawn(root, { log = [], diff = "crates/a/src/lib.rs\n", cargoLines = "/bin/rustright\n" } = {}) {
  const metadata = meta(root, ".", [["a", "crates/a"], ["b", "crates/b", []]]);
  return (cmd, args) => {
    log.push([cmd, ...args]);
    const line = args.join(" ");
    if (cmd === "git") {
      if (line === "rev-parse --show-toplevel") return ok(root);
      if (line === "rev-parse HEAD") return ok(HEAD);
      if (args[0] === "ls-files") return ok(args.at(-1).endsWith("Cargo.toml") ? "Cargo.toml\0crates/a/Cargo.toml\0crates/b/Cargo.toml\0" : "");
      if (args[0] === "diff") return ok(diff);
    }
    if (cmd === "where" || cmd === "which") return args[0] === "rustright" ? ok(cargoLines) : no();
    if (cmd === "rustright" && args[1] === "metadata") return ok(metadata);
    return no(`unexpected ${cmd} ${line}`);
  };
}
const gateFiles = (extra = {}) => ({
  "Cargo.toml": "[workspace]\nmembers = [\"crates/*\"]\n", "Cargo.lock": "version = 4\n",
  "crates/a/Cargo.toml": pkg("a"), "crates/a/src/lib.rs": "pub fn a() {}\n", "crates/b/Cargo.toml": pkg("b", "publish = false\n"), "crates/b/src/lib.rs": "",
  ".rightkit-local-build.json": "{}", ...extra,
});

test("repository pre-push selects every class A & only focused CI tests/generated docs, preserving evidence bindings", () => {
  const scans = [
    ...["cargo-deny", "cargo-audit", "cargo-package-list:a", "secret-scan"].map((id) => ({ id, class: "A", command: id })),
    ...["test:scripts-ci", "generated-docs", "clippy:a", "check:a", "doc:a", "test:rust:a", "test:npm:p", "tsc:p", "swift-build"].map((id) => ({ id, class: "B", command: id })),
  ];
  const full = { scans, deferred: ["generated-docs", "doc:a"], notes: [], inputDigest: "content", roots: { contentRoot: "export", liveRoot: "checkout" } };
  const selected = prePushPlan(full);
  assert.deepEqual(selected.scans.map((s) => s.id), ["cargo-deny", "cargo-audit", "cargo-package-list:a", "secret-scan", "test:scripts-ci", "generated-docs"]);
  assert.deepEqual(selected.deferred, ["generated-docs"], "retained deferred checks still fail");
  assert.equal(selected.inputDigest, full.inputDigest);
  assert.equal(selected.roots, full.roots);
  assert.equal(selected.classDigests.A, digestScanSet(scans.filter((s) => s.class === "A")), "no class A checks removed");
  assert.equal(selected.classDigests.B, digestScanSet(selected.scans.filter((s) => s.class === "B")));
  assert.equal(selected.scanSetDigest, digestScanSet(selected.scans));
  assert.equal(full.scans, scans, "full plan stays intact");
  assert.deepEqual(full.deferred, ["generated-docs", "doc:a"]);
});

test("--pre-push --plan excludes native checks while default CLI retains them", async (t) => {
  const root = mkRepo(gateFiles(), t);
  const selected = capture(); const full = capture();
  for (const [args, c] of [[["--plan", "--pre-push"], selected], [["--plan"], full]]) {
    assert.equal(await main(args, { ...c.io, spawn: repoSpawn(root), cwd: root, env: {} }), 0, c.err.join("\n"));
  }
  assert.match(selected.out.join("\n"), /\[A\] cargo-package-list:a /);
  assert.doesNotMatch(selected.out.join("\n"), /\[B\] (?:clippy|check|doc|test:rust):/);
  assert.match(full.out.join("\n"), /\[B\] clippy:a /);
  assert.match(full.out.join("\n"), /\[B\] doc:a /);
  const wrapper = fs.readFileSync(new URL("../../scripts/local-gate.mjs", import.meta.url), "utf8");
  assert.match(wrapper, /main\(\["--pre-push", \.\.\.process\.argv\.slice\(2\)\]\)/);
});

test("--plan prints the plan for a fake repo from its own units and runs nothing", async (t) => {
  const root = mkRepo(gateFiles(), t);
  const log = []; const c = capture();
  const code = await main(["--plan", "--base", "origin/main"], { ...c.io, spawn: repoSpawn(root, { log }), cwd: root, env: {} });
  assert.equal(code, 0, c.err.join("\n"));
  const out = c.out.join("\n");
  assert.match(out, /rust units: a=crates\/a, b=crates\/b; npm units: none/);
  assert.match(out, /\[B\] clippy:a :: .*clippy -p a /);
  assert.match(out, /rustright cargo clippy -p a --no-deps --all-targets --locked -- -D warnings/);
  assert.match(out, /\[A\] cargo-package-list:a /);
  assert.doesNotMatch(out, /clippy:b|cargo-package-list:b/);
  assert.doesNotMatch(out, /deferred/, "the compile grant is present");
  assert.ok(!log.some((l) => l[0] === "rustright" && l[1] === "cargo" && l[2] !== "metadata"), "only discovery ran through cargo");
  assert.ok(!log.some((l) => l.includes("archive") || l.includes("status") || l.includes("tar")), "a plan exports and checks nothing");
});

test("the gate blocks without a local compile grant: Rust units cannot be discovered through the broker", async (t) => {
  const { ".rightkit-local-build.json": _grant, ...files } = gateFiles();
  const root = mkRepo(files, t);
  const c = capture();
  assert.equal(await main(["--plan"], { ...c.io, spawn: repoSpawn(root), cwd: root, env: {} }), 1);
  assert.match(c.err.join("\n"), /BLOCKED: no local compile grant/);
});

test("the gate blocks when neither rustright nor the managed cargo shim resolves", async (t) => {
  const root = mkRepo(gateFiles(), t);
  const c = capture();
  const code = await main(["--plan"], { ...c.io, spawn: repoSpawn(root, { cargoLines: "" }), cwd: root, env: {} });
  // `where rustright` answers nothing: the only fallback is a cargo shim, and `where cargo` fails here.
  assert.equal(code, 1);
  assert.match(c.err.join("\n"), /neither `rustright` nor the managed cargo shim/);
});

test("pre-push: the pushed commit must be the checked-out HEAD, before anything else runs", async (t) => {
  const root = mkRepo(gateFiles(), t);
  const log = []; const c = capture();
  const line = `refs/heads/main ${"b".repeat(40)} refs/heads/main ${"0".repeat(40)}\n`;
  const code = await main([], { ...c.io, spawn: repoSpawn(root, { log }), cwd: root, env: {}, readStdin: () => line });
  assert.equal(code, 1);
  assert.match(c.err.join("\n"), /BLOCKED: pushing bbbbbbbbbb to refs\/heads\/main, but HEAD is aaaaaaaaaa/);
  assert.ok(!log.some((l) => l[0] === "rustright"), "no cargo ran");
});

test("--version prints the package version", async () => {
  const c = capture();
  assert.equal(await main(["--version"], c.io), 0);
  assert.equal(c.out[0], JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8")).version);
  assert.equal(await main(["bogus"], capture().io), 2);
});

test("package.json ships the bin and lock-graph.mjs", () => {
  const pkgJson = JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8"));
  assert.equal(pkgJson.bin["rightkit-gate"], "bin/rightkit-gate.mjs");
  assert.ok(pkgJson.files.includes("bin") && pkgJson.files.includes("*.mjs") && !pkgJson.files.includes("!lock-graph.mjs"));
  assert.ok(pkgJson.rightkit.publishCheck.binSmoke["rightkit-gate"]);
});

// ---- result cache inputs ----------------------------------------------------------------------------------------

// Sources with no reach outside their crate: every member is one src/lib.rs that includes nothing.
const plainSources = { listFiles: (dir) => [`${dir}/src/lib.rs`], readBlob: () => "pub fn f() {}\n" };

test("cache digest: a cargo scan covers its lock closure and the shared build inputs, nothing else", () => {
  const lock = shipped.parseLock(lockPkg("a", "1.0.0", ["b", "serde"]) + lockPkg("b", "1.0.0") + lockPkg("c", "1.0.0") + lockPkg("serde", "1.0.0"));
  const units = [{ name: "a", dir: "crates/a" }, { name: "b", dir: "crates/b" }, { name: "c", dir: "crates/c" }];
  const ids = { "crates/a": "ta", "crates/b": "tb", "crates/c": "tc", "Cargo.lock": "lock", "Cargo.toml": "root", ".cargo": "cargo", "deny.toml": "deny", "ci/gate.config.json": "cfg", vendor: "v" };
  const digestOf = (over = {}, tree = "head") => {
    const map = { ...ids, ...over };
    return buildCacheDigest({ objectId: (p) => map[p] ?? null, headTree: tree, rustUnits: units, cargoDir: ".", rustLock: lock, ...plainSources });
  };
  const clippyA = { id: "clippy:a", class: "B" };
  const base = digestOf()(clippyA);
  assert.equal(digestOf()(clippyA), base, "identical inputs");
  assert.equal(digestOf({}, "other-head")(clippyA), base, "the whole tree is not an input of a cargo scan");
  assert.notEqual(digestOf({ "crates/b": "tb2" })(clippyA), base, "a closure member changed");
  assert.notEqual(digestOf({ "crates/a": "ta2" })(clippyA), base, "the crate itself changed");
  assert.equal(digestOf({ "crates/c": "tc2" })(clippyA), base, "a workspace crate outside the closure changed");
  // The shared inputs are projected to what the crate consumes (content), never the shared files' own object ids.
  assert.equal(digestOf({ "Cargo.lock": "x", "Cargo.toml": "x", "deny.toml": "x", "ci/gate.config.json": "x", vendor: "x" })(clippyA), base, "shared file ids are not inputs");
  const withLock = (l) => buildCacheDigest({ objectId: (p) => ids[p] ?? null, headTree: "head", rustUnits: units, cargoDir: ".", rustLock: l, ...plainSources })(clippyA);
  assert.notEqual(withLock(shipped.parseLock(lockPkg("a", "1.0.0", ["b", "serde"]) + lockPkg("b", "1.0.0") + lockPkg("c", "1.0.0") + lockPkg("serde", "1.0.1"))), base, "a lock entry in the closure changed");
  assert.equal(withLock(shipped.parseLock(lockPkg("a", "1.0.0", ["b", "serde"]) + lockPkg("b", "1.0.0") + lockPkg("c", "1.0.1") + lockPkg("serde", "1.0.0"))), base, "a lock entry outside the closure changed");
  const withManifest = (text) => buildCacheDigest({ objectId: (p) => ids[p] ?? null, headTree: "head", rustUnits: units, cargoDir: ".", rustLock: lock, listFiles: plainSources.listFiles, readBlob: (p) => (p === "Cargo.toml" ? text : "pub fn f() {}\n") })(clippyA);
  const m0 = withManifest('[workspace]\nmembers = ["crates/*"]\n\n[profile.release]\nopt-level = 3\n');
  assert.equal(withManifest('[workspace]\nmembers = ["crates/*", "x"]\n# note\n\n[profile.release]\nopt-level = 3\n\n[patch.crates-io]\nserde = { path = "vendor/serde" }\n'), m0, "membership, comments and a patch table are lock-reflected, not manifest inputs");
  assert.notEqual(withManifest('[workspace]\nmembers = ["crates/*"]\n\n[profile.release]\nopt-level = 2\n'), m0, "a profile change");
  assert.notEqual(withManifest('[workspace]\nmembers = ["crates/*"]\n\n[workspace.dependencies]\nserde = { version = "1", features = ["derive"] }\n\n[profile.release]\nopt-level = 3\n'), m0, "a workspace dependency table (feature lists reach every dependent without the lock)");
  assert.notEqual(digestOf({ "vendor/serde": "v1" })(clippyA), digestOf({ "vendor/serde": "v2" })(clippyA), "a vendored crate in the closure");
  assert.equal(digestOf({ "vendor/c": "v1" })(clippyA), digestOf({ "vendor/c": "v2" })(clippyA), "a vendored crate outside the closure");
  const withConfig = (cf) => buildCacheDigest({ objectId: (p) => ids[p] ?? null, headTree: "head", rustUnits: units, cargoDir: ".", rustLock: lock, crateFeatures: cf, ...plainSources })(clippyA);
  assert.notEqual(withConfig({ a: { featureSets: [["--all-features"]] } }), base, "the crate's own gate config entry");
  assert.equal(withConfig({ c: { featureSets: [["--all-features"]] } }), base, "another crate's entry");
  assert.notEqual(digestOf()({ id: "check:c", class: "B" }), base, "another crate, another digest");
  assert.equal(digestOf({ "crates/a": "ta2" })({ id: "check:c", class: "B" }), digestOf()({ id: "check:c", class: "B" }), "c does not depend on a");
  // Rust tests: keyed on the closure like a compile scan when no source navigates out of the crate without a path; on the whole
  // HEAD tree when one does (current_dir, a bare "..", the manifest directory's parent).
  const testA = { id: "test:rust:a", class: "B" };
  assert.equal(digestOf({}, "other-head")(testA), digestOf()(testA), "a test of a crate whose sources name no outside path is reused across commits");
  assert.notEqual(digestOf()(testA), base, "but a test key is not a compile key");
  const leaving = (tree) => buildCacheDigest({ objectId: (p) => ids[p] ?? null, headTree: tree, rustUnits: units, cargoDir: ".", rustLock: lock, listFiles: plainSources.listFiles, readBlob: (p) => (p === "crates/a/src/lib.rs" ? 'fn root() { let _ = std::env::current_dir(); }\n' : "pub fn f() {}\n") });
  assert.notEqual(leaving("h1")(testA), leaving("h2")(testA), "a test whose crate navigates out at run time is keyed on the whole tree");
  assert.equal(leaving("h1")(clippyA), leaving("h2")(clippyA), "its compile scan is not");
});

test("cache digest: live class A checks and an unanswerable git are never cached; content class A scans key on their files; tree scans on the tree", () => {
  const lock = shipped.parseLock(lockPkg("a", "1.0.0"));
  const make = (objectId, tree = "t1") => buildCacheDigest({ objectId, headTree: tree, rustUnits: [{ name: "a", dir: "crates/a" }], cargoDir: ".", rustLock: lock, ...plainSources });
  for (const live of ["cargo-deny", "cargo-deny:x", "cargo-audit", "pnpm-audit", "pnpm-lockfile", "advisory-exceptions"]) assert.equal(make(() => "x")({ id: live, class: "A" }), null, `${live} is a live check`);
  const byId = (ids) => (p) => ids[p] ?? null;
  const nodeCheck = { id: "node-check:a.mjs", class: "A", args: ["--check", "a.mjs"] };
  assert.notEqual(make(byId({ "a.mjs": "1" }))(nodeCheck), null, "node-check is cached");
  assert.notEqual(make(byId({ "a.mjs": "1" }))(nodeCheck), make(byId({ "a.mjs": "2" }))(nodeCheck), "keyed on the file");
  assert.equal(make(byId({ "a.mjs": "1" }), "t1")(nodeCheck), make(byId({ "a.mjs": "1" }), "t2")(nodeCheck), "not on the tree");
  const fmt = { id: "rustfmt:2021", class: "A", args: ["--check", "--edition", "2021", "crates/a/src/lib.rs", "crates/b/src/lib.rs"] };
  assert.notEqual(make(byId({ "crates/a/src/lib.rs": "1", "crates/b/src/lib.rs": "1" }))(fmt), make(byId({ "crates/a/src/lib.rs": "1", "crates/b/src/lib.rs": "2" }))(fmt), "rustfmt keys on the listed files");
  const parse = { id: "toml-parse", class: "A", files: ["Cargo.toml", "crates/a/Cargo.toml"] };
  assert.notEqual(make(byId({ "Cargo.toml": "1", "crates/a/Cargo.toml": "1" }))(parse), make(byId({ "Cargo.toml": "2", "crates/a/Cargo.toml": "1" }))(parse), "a parse scan keys on its files");
  const pkgList = { id: "cargo-package-list:a", class: "A" };
  assert.notEqual(make(byId({ "crates/a": "1" }))(pkgList), make(byId({ "crates/a": "2" }))(pkgList), "a package list keys on the crate");
  assert.equal(make(byId({ "crates/a": "1" }), "t1")(pkgList), make(byId({ "crates/a": "1" }), "t2")(pkgList), "not on the tree");
  assert.notEqual(make(() => "x")({ id: "cargo-metadata", class: "A" }), null, "metadata is cached on the tree");
  assert.equal(make(() => { throw new Error("git down"); })({ id: "clippy:a", class: "B" }), null);
  const scripts = { id: "test:scripts-ci", class: "B" };
  assert.equal(make(byId({ scripts: "s1", ci: "c1" }), "t1")(scripts), make(byId({ scripts: "s1", ci: "c1" }), "t2")(scripts), "scripts-ci keys on its directories, not the tree");
  assert.notEqual(make(byId({ scripts: "s1", ci: "c1" }))(scripts), make(byId({ scripts: "s2", ci: "c1" }))(scripts));
  const docs = { id: "generated-docs", class: "B" };
  assert.equal(make(() => "x")(docs), make(() => "y")(docs), "tree scans ignore per-path ids");
  assert.notEqual(make(() => "x", "t1")(docs), make(() => "x", "t2")(docs));
  // No lock graph: the closure is unknown, so a cargo scan falls back to the whole tree rather than a guess.
  const noLock = buildCacheDigest({ objectId: () => "x", headTree: "t1", rustUnits: [{ name: "a", dir: "crates/a" }], rustLock: null });
  assert.equal(noLock({ id: "clippy:a", class: "B" }), make(() => "x")(docs));
});

test("cache digest: an npm package scan keys on the package's workspace closure and the root npm inputs, never the tree", () => {
  const manifests = {
    "packages/release/package.json": JSON.stringify({ name: "@rightkit/release", dependencies: { "@rightkit/core": "workspace:*", express: "^4" } }),
    "packages/core/package.json": JSON.stringify({ name: "@rightkit/core" }),
    "packages/qa/package.json": JSON.stringify({ name: "@rightkit/qa", devDependencies: { "@rightkit/release": "workspace:^" } }),
    "packages/lost/package.json": JSON.stringify({ name: "@rightkit/lost", dependencies: { "@rightkit/elsewhere": "workspace:*" } }),
  };
  const units = [{ name: "rightkit-release", dir: "packages/release" }, { name: "rightkit-core", dir: "packages/core" }, { name: "rightkit-qa", dir: "packages/qa" }, { name: "rightkit-lost", dir: "packages/lost" }];
  const make = (ids, tree = "t1") => buildCacheDigest({ objectId: (p) => ids[p] ?? null, headTree: tree, rustUnits: [], npmUnits: units, listFiles: () => [], readBlob: (p) => { if (!(p in manifests)) throw new Error(`no ${p}`); return manifests[p]; } });
  const base = { "packages/release": "r1", "packages/core": "c1", "packages/qa": "q1", "pnpm-lock.yaml": "l1", "package.json": "p1" };
  const rel = { id: "test:npm:rightkit-release", class: "B" };
  assert.equal(make(base, "t1")(rel), make(base, "t2")(rel), "not keyed on the tree");
  assert.notEqual(make({ ...base, "packages/core": "c2" })(rel), make(base)(rel), "a workspace dependency changed");
  assert.equal(make({ ...base, "packages/qa": "q2" })(rel), make(base)(rel), "a dependent (not a dependency) is not an input");
  assert.notEqual(make({ ...base, "pnpm-lock.yaml": "l2" })(rel), make(base)(rel), "the install graph is an input");
  assert.notEqual(make(base)({ id: "tsc:rightkit-qa", class: "B" }), make({ ...base, "packages/release": "r2" })({ id: "tsc:rightkit-qa", class: "B" }), "qa's closure holds release (transitively core)");
  const lost = { id: "test:npm:rightkit-lost", class: "B" };
  assert.notEqual(make(base, "t1")(lost), make(base, "t2")(lost), "an unresolvable workspace dependency falls back to the whole tree");
});

test("rustUnitOfScanId maps every cargo scan id to its crate", () => {
  const cases = { "clippy:a": "a", "clippy:a:1": "a", "check:a": "a", "check-features:a:0": "a", "check-dependent:a": "a", "doc:a": "a", "test:rust:a": "a", "test:rust:a:2": "a", "test:rust-serial:a:1": "a" };
  for (const [id, unit] of Object.entries(cases)) assert.equal(rustUnitOfScanId(id), unit, id);
  for (const id of ["test:npm:a", "test:scripts-ci", "tsc:a", "generated-docs", "cargo-deny"]) assert.equal(rustUnitOfScanId(id), null, id);
});

// ---- result cache: inputs outside the closure --------------------------------------------------------------------------

function sourceTree(files) {
  return { listFiles: (dir) => Object.keys(files).filter((f) => f.startsWith(`${dir}/`)), readBlob: (p) => { if (!(p in files)) throw new Error(`no ${p}`); return files[p]; } };
}
function ownershipDigest({ ids = {}, tree = "head", files = {}, crateFeatures = {}, scanId = "test:rust:rightkit-ownership" } = {}) {
  const lock = shipped.parseLock(lockPkg("rightkit-ownership", "1.0.0"));
  const base = { "crates/rightkit-ownership": "own1", "Cargo.lock": "lock", "Cargo.toml": "root", "ci/gate.config.json": "cfg", "packages/release/fixtures/ownership/cases": "cases1", "packages/release/fixtures": "fx1" };
  const map = { ...base, ...ids };
  const src = { "crates/rightkit-ownership/src/lib.rs": "pub fn f() {}\n", ...files };
  return buildCacheDigest({ objectId: (p) => map[p] ?? null, headTree: tree, rustUnits: [{ name: "rightkit-ownership", dir: "crates/rightkit-ownership" }], cargoDir: ".", rustLock: lock, crateFeatures, ...sourceTree(src) })({ id: scanId, class: "B" });
}

test("cache digest: the ownership_fixtures reproduction - changing the fixture cases changes the test digest, with or without testInputs", () => {
  const cases = "packages/release/fixtures/ownership/cases";
  // The crate directory is untouched and its sources name no outside path: the fixture directory is not an input, whatever the tree.
  assert.equal(ownershipDigest({ ids: { [cases]: "cases2" }, tree: "head2" }), ownershipDigest(), "no reference to the fixture: not an input");
  const viaLiteral = { "crates/rightkit-ownership/tests/cases.rs": 'let dir = Path::new("../../packages/release/fixtures/ownership/cases");\n' };
  assert.notEqual(ownershipDigest({ files: viaLiteral, ids: { [cases]: "cases2" } }), ownershipDigest({ files: viaLiteral }), "a literal path to the fixture is an input");
  const viaParent = { "crates/rightkit-ownership/tests/cases.rs": 'let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();\n' };
  assert.notEqual(ownershipDigest({ files: viaParent, tree: "head2" }), ownershipDigest({ files: viaParent }), "tests that navigate out of the crate without a path are keyed on the whole tree");
  assert.equal(ownershipDigest({ files: viaParent, scanId: "clippy:rightkit-ownership", tree: "head2" }), ownershipDigest({ files: viaParent, scanId: "clippy:rightkit-ownership" }), "a compile scan of the same crate is not");
  const declared = { "rightkit-ownership": { testInputs: [cases] } };
  const withInputs = ownershipDigest({ crateFeatures: declared });
  assert.notEqual(ownershipDigest({ crateFeatures: declared, ids: { [cases]: "cases2" }, tree: "head2" }), withInputs, "declared testInputs: the fixture path is an input");
  assert.equal(ownershipDigest({ crateFeatures: declared, tree: "head3", ids: { "packages/release/fixtures": "other" } }), withInputs, "declared testInputs: an unrelated tree change is not an input, so the key is reused across commits");
  assert.notEqual(ownershipDigest({ crateFeatures: declared, ids: { "crates/rightkit-ownership": "own2" } }), withInputs, "the closure is still an input");
  for (const bad of [[], ["../x"], ["/abs"], ["a/../b"], ["C:/x"], [""], "cases"]) {
    const cf = { "rightkit-ownership": { testInputs: bad } };
    assert.notEqual(ownershipDigest({ crateFeatures: cf, tree: "head9" }), ownershipDigest({ crateFeatures: cf, tree: "head10" }), `an unusable testInputs ${JSON.stringify(bad)} falls back to the whole tree`);
  }
});

test("cache digest: a compile scan also keys on a repository path the crate's sources include from outside its directory", () => {
  const files = { "crates/rightkit-ownership/src/lib.rs": 'const SCHEMA: &str = include_str!("../../../packages/release/schema.json");\n' };
  const clippy = ({ id = "s1", tree = "head", other = {} } = {}) => ownershipDigest({ scanId: "clippy:rightkit-ownership", files, ids: { "packages/release/schema.json": id, ...other }, tree });
  const base = clippy();
  assert.notEqual(clippy({ id: "s2" }), base, "the included file changed");
  assert.equal(clippy({ tree: "head2", other: { "packages/release/fixtures": "unrelated" } }), base, "an unrelated change is not an input");
  // The same crate with no outside reference ignores that path entirely.
  const plain = (id) => ownershipDigest({ scanId: "clippy:rightkit-ownership", ids: { "packages/release/schema.json": id } });
  assert.equal(plain("s2"), plain("s1"), "no reference, no dependency on the path");
  // include_bytes! through a CARGO_MANIFEST_DIR concat resolves the same way.
  const viaConcat = (id) => ownershipDigest({ scanId: "check:rightkit-ownership", files: { "crates/rightkit-ownership/src/lib.rs": 'static B: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/release/schema.json"));\n' }, ids: { "packages/release/schema.json": id } });
  assert.notEqual(viaConcat("s1"), viaConcat("s2"));
  // A "../" literal in ordinary code counts when it names an existing repository path.
  const viaLiteral = (id) => ownershipDigest({ scanId: "doc:rightkit-ownership", files: { "crates/rightkit-ownership/tests/t.rs": 'let p = dir.join("../../packages/release/schema.json");\n' }, ids: { "packages/release/schema.json": id } });
  assert.notEqual(viaLiteral("s1"), viaLiteral("s2"));
});

test("cache digest: a reference that cannot be resolved statically keys the scan on the whole tree", () => {
  const differsByTree = (scanId, files, label) => assert.notEqual(ownershipDigest({ scanId, files, tree: "head-A" }), ownershipDigest({ scanId, files, tree: "head-B" }), label);
  differsByTree("clippy:rightkit-ownership", { "crates/rightkit-ownership/src/lib.rs": "const S: &str = include_str!(path_macro!());\n" }, "a computed include path");
  differsByTree("check:rightkit-ownership", {
    "crates/rightkit-ownership/build.rs": 'fn main() { let root = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).parent().unwrap().to_path_buf(); let _ = std::fs::read_to_string(root.join("x")); }\n',
  }, "a build.rs that reads files and leaves its crate");
  differsByTree("clippy:rightkit-ownership", { "crates/rightkit-ownership/src/lib.rs": 'const S: &str = include_str!("../../../../../../etc/x");\n' }, "a path that leaves the repository");
  const lock = shipped.parseLock(lockPkg("a", "1.0.0"));
  const make = (tree, sources) => buildCacheDigest({ objectId: () => "x", headTree: tree, rustUnits: [{ name: "a", dir: "crates/a" }], rustLock: lock, ...sources })({ id: "clippy:a", class: "B" });
  const unreadable = { listFiles: () => ["crates/a/src/lib.rs"], readBlob: () => { throw new Error("git down"); } };
  assert.notEqual(make("head-A", unreadable), make("head-B", unreadable), "an unreadable source");
  assert.notEqual(make("head-A", {}), make("head-B", {}), "no way to read the sources at all");
});

test("externalInputs: include targets resolve against the including file, build.rs rerun-if-changed targets against the crate", () => {
  const exists = (p) => ["shared/a.json", "shared/b.txt", "shared/gen.rs", "shared/deep.txt"].includes(p);
  const files = {
    "crates/c/src/lib.rs": 'include_str!("../../../shared/a.json"); include!("../../../shared/gen.rs"); include_str!("local.txt");',
    "crates/c/src/sub/mod.rs": 'include_bytes!( "../../../../shared/b.txt" )',
    "crates/c/build.rs": 'fn main() { let _ = std::fs::read("x"); println!("cargo:rerun-if-changed=../../shared/b.txt"); }',
    "shared/gen.rs": 'include_str!("deep.txt");',
  };
  const t = sourceTree(files);
  const out = externalInputs({ dir: "crates/c", files: t.listFiles("crates/c"), readBlob: t.readBlob, exists });
  assert.equal(out.unresolved, null);
  assert.deepEqual(out.paths, ["shared/a.json", "shared/b.txt", "shared/deep.txt", "shared/gen.rs"]);
});

// ---- fix6: ancestor configuration in the cache key and the lock scope, transitive includes -------------------------------------

test("cache digest: Rust configuration of EVERY ancestor directory of a nested workspace is an input, the intermediate ones included", () => {
  const units = [{ name: "apps/foo:foo", crate: "foo", dir: "apps/foo/foo", cargoDir: "apps/foo" }];
  const rustLocks = { "apps/foo": shipped.parseLock(lockPkg("foo", "1.0.0")) };
  // Every .cargo directory holds a config.toml and an audit.toml; the ids below name the files, the digest lists the directory.
  const cargoDirs = [".cargo", "apps/.cargo", "apps/foo/.cargo", "other/.cargo", "apps/bar/.cargo"];
  const ids = { "apps/foo/foo": "f1", "apps/foo/Cargo.lock": "l", "ci/gate.config.json": "cfg", ...Object.fromEntries(cargoDirs.flatMap((d) => [[d, `${d}-dir`], [`${d}/config.toml`, "c1"], [`${d}/audit.toml`, "a1"]])) };
  const cfgSources = { listFiles: (dir) => (dir.endsWith(".cargo") ? [`${dir}/config.toml`, `${dir}/audit.toml`] : [`${dir}/src/lib.rs`]), readBlob: () => "pub fn f() {}\n" };
  const digest = (over = {}) => buildCacheDigest({ objectId: (p) => ({ ...ids, ...over })[p] ?? null, headTree: "head", rustUnits: units, cargoDir: ".", rustLocks, ...cfgSources })({ id: "clippy:apps/foo:foo", class: "B" });
  const base = digest();
  assert.equal(digest(), base);
  // The review's reproduction: apps/.cargo sits between the repository root and the workspace and was not an input.
  assert.notEqual(digest({ "apps/.cargo/config.toml": "intermediate" }), base, "apps/.cargo/config.toml");
  for (const file of [".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml", "clippy.toml", ".clippy.toml", "rustfmt.toml", ".rustfmt.toml"]) {
    assert.notEqual(digest({ [file]: "root" }), base, `root ${file}`);
    assert.notEqual(digest({ [`apps/${file}`]: "mid" }), base, `apps/${file}`);
    assert.notEqual(digest({ [`apps/foo/${file}`]: "own" }), base, `apps/foo/${file}`);
  }
  // Advisory policy is no compile input, at any level: it feeds class A scans, which are never cached.
  for (const policy of ["deny.toml", "apps/deny.toml", "apps/foo/deny.toml", ".cargo/audit.toml", "apps/.cargo/audit.toml", "apps/foo/.cargo/audit.toml"]) assert.equal(digest({ [policy]: "changed" }), base, policy);
  assert.equal(digest({ "other/.cargo/config.toml": "x", "apps/bar/.cargo/config.toml": "x" }), base, "a directory that is no ancestor of the workspace is no input");
});

test("lockScope: Rust configuration of an ancestor directory keeps every crate in scope, even beside a lock-only change", () => {
  const lock = shipped.parseLock(lockPkg("foo", "1.0.0"));
  const base = { cargoDir: "apps/foo", unitNames: new Set(["foo"]), showBase: () => lockPkg("foo", "0.9.0"), readLock: () => lockPkg("foo", "1.0.0") };
  assert.ok(lockScope({ ...base, changed: ["apps/foo/Cargo.lock"] }) instanceof Set, "a lock-only change is scoped by the lock graph");
  for (const file of [".cargo/config.toml", "apps/.cargo/config.toml", "rust-toolchain.toml", "apps/clippy.toml", "rustfmt.toml"]) assert.equal(lockScope({ ...base, changed: ["apps/foo/Cargo.lock", file] }), null, file);
  assert.ok(lockScope({ ...base, changed: ["apps/foo/Cargo.lock", "docs/.cargo/config.toml"] }) instanceof Set, "a directory that is no ancestor changes nothing");
  for (const policy of [".cargo/audit.toml", "apps/.cargo/audit.toml", "deny.toml"]) assert.ok(lockScope({ ...base, changed: ["apps/foo/Cargo.lock", policy] }) instanceof Set, `${policy} is advisory policy: it changes no compilation`);
  assert.ok(lock);
});

test("externalInputs and the cache digest follow includes to a fixed point: lib.rs -> shared/one.rs -> two.rs -> three.rs", () => {
  const files = {
    "crates/rightkit-ownership/src/lib.rs": 'include!("../../../shared/one.rs");\n',
    "shared/one.rs": 'include!("two.rs");\n', "shared/two.rs": 'include!("three.rs");\n', "shared/three.rs": "pub fn three() {}\n",
  };
  const t = sourceTree(files);
  const found = externalInputs({ dir: "crates/rightkit-ownership", files: t.listFiles("crates/rightkit-ownership"), readBlob: t.readBlob, exists: (p) => p in files });
  assert.equal(found.unresolved, null);
  assert.deepEqual(found.paths, ["shared/one.rs", "shared/three.rs", "shared/two.rs"]);
  // The review's reproduction: changing three.rs must change the digest of a compile scan.
  const digest = (three) => ownershipDigest({ scanId: "clippy:rightkit-ownership", files, ids: { "shared/one.rs": "o", "shared/two.rs": "t", "shared/three.rs": three } });
  assert.equal(digest("3a"), digest("3a"));
  assert.notEqual(digest("3a"), digest("3b"), "three.rs is four hops from the crate");
  // Another depth and other reference forms: #[path], mod declarations of an out-of-crate file, "../" literals.
  const deep = { "crates/rightkit-ownership/src/lib.rs": '#[path = "../../../shared/a.rs"] mod a;\n' };
  for (let i = 0; i < 12; i += 1) deep[`shared/f${i}.rs`] = i < 11 ? `include!("f${i + 1}.rs");\n` : "pub fn end() {}\n";
  deep["shared/a.rs"] = 'include!("f0.rs");\n';
  const reach = (files2) => { const x = sourceTree(files2); return externalInputs({ dir: "crates/rightkit-ownership", files: x.listFiles("crates/rightkit-ownership"), readBlob: x.readBlob, exists: (p) => p in files2 }); };
  assert.ok(reach(deep).paths.includes("shared/f11.rs"), "twelve hops deep, through a #[path] attribute");
  const viaMod = { "crates/rightkit-ownership/src/lib.rs": '#[path = "../../../shared/m.rs"]\nmod m;\n', "shared/m.rs": "mod inner;\npub mod other;\n", "shared/m/inner.rs": 'include_str!("../data.txt");\n', "shared/m/other/mod.rs": "", "shared/data.txt": "x" };
  const modPaths = reach(viaMod);
  assert.equal(modPaths.unresolved, null);
  assert.deepEqual(modPaths.paths, ["shared/data.txt", "shared/m.rs", "shared/m/inner.rs", "shared/m/other/mod.rs"]);
  const viaLiteral = { "crates/rightkit-ownership/src/lib.rs": 'let _ = dir.join("../../shared/x.rs");\n', "shared/x.rs": 'let _ = d.join("../y/z.rs");\n', "y/z.rs": "" };
  assert.deepEqual(reach(viaLiteral).paths, ["shared/x.rs", "y/z.rs"]);
  // Cycles end: a and b include each other.
  const cyc = { "crates/rightkit-ownership/src/lib.rs": 'include!("../../../shared/a.rs");\n', "shared/a.rs": 'include!("b.rs");\n', "shared/b.rs": 'include!("a.rs");\n' };
  assert.deepEqual(reach(cyc).paths, ["shared/a.rs", "shared/b.rs"]);
  // An unreadable out-of-crate file, and a traversal beyond the bound, both fall back to the whole tree (never a silent stop).
  const unreadable = { listFiles: (dir) => (dir === "crates/rightkit-ownership" ? ["crates/rightkit-ownership/src/lib.rs"] : []), readBlob: (p) => { if (p.startsWith("crates/")) return 'include!("../../../shared/one.rs");\n'; throw new Error("git down"); } };
  assert.match(externalInputs({ dir: "crates/rightkit-ownership", files: unreadable.listFiles("crates/rightkit-ownership"), readBlob: unreadable.readBlob, exists: () => true }).unresolved, /unreadable/);
  const many = Array.from({ length: EXTERNAL_INPUT_FILE_LIMIT + 1 }, (_, i) => `crates/c/src/f${i}.rs`);
  assert.match(externalInputs({ dir: "crates/c", files: many, readBlob: () => "pub fn f() {}\n", exists: () => false }).unresolved, /more than 10000 source files/);
});

// ---- npm: the root package.json (adoption gap 4) ----------------------------------------------------------------------------

const npmFixture = (workspaceYaml, extra = {}) => {
  const files = new Map(Object.entries({
    "package.json": '{"name":"app-root"}', "apps/web/package.json": '{"name":"web"}', "packages/ui/package.json": '{"name":"ui"}', "tools/t/package.json": '{"name":"t"}', ...extra,
  }).map(([k, v]) => [norm(path.join("/r", k)), v]));
  if (workspaceYaml !== null) files.set(norm("/r/pnpm-workspace.yaml"), workspaceYaml);
  const tracked = (name) => [...files.keys()].map((k) => k.slice("/r/".length)).filter((k) => path.posix.basename(k) === name);
  return { root: "/r", tracked, readFile: (p) => files.get(norm(p)), exists: (p) => files.has(norm(p)) };
};
const unitDirs = (fixture) => discoverNpmUnits(fixture).map((u) => `${u.name}=${u.dir}`);

test("npmUnits: the root package.json is a unit when pnpm-workspace.yaml is absent, has no packages: or declares the root", () => {
  assert.deepEqual(unitDirs(npmFixture(null)), ["app-root=."], "no workspace file: the root only, as before");
  assert.deepEqual(unitDirs(npmFixture("minimumReleaseAge: 1440\nonlyBuiltDependencies:\n  - esbuild\n")), ["app-root=."], "a settings-only workspace file has no packages:");
  assert.deepEqual(unitDirs(npmFixture("packages: []\n")), ["app-root=."], "an empty packages: list");
  assert.deepEqual(unitDirs(npmFixture("packages:\n  - '.'\n")), ["app-root=."], "viewright: packages declares the root");
  assert.deepEqual(unitDirs(npmFixture("packages:\n  - './'\n  - 'apps/*'\n")), ["app-root=.", "web=apps/web"], "the root and the nested globs together");
  assert.deepEqual(unitDirs(npmFixture("packages:\n  - 'packages/*'\n  - 'apps/*'\n")), ["web=apps/web", "ui=packages/ui"], "a packages: list that does not name the root: the root is only the workspace manifest");
  assert.deepEqual(unitDirs(npmFixture("packages:\n  - '.'\n  - '!.'\n")), [], "an explicit exclusion of the root wins");
  assert.deepEqual(unitDirs(npmFixture("packages:\n  - '.'\n  - 'packages/*'\n", { "packages/ui/package.json": '{"name":"app-root"}' })), ["root=.", "packages-ui=packages/ui"], "a name collision falls back to directories");
  assert.deepEqual(unitDirs({ ...npmFixture("minimumReleaseAge: 1\n"), tracked: () => ["apps/web/package.json"] }), [], "a root package.json that is not tracked is no unit");
});

// ---- rust workspaces (adoption gap 3) ---------------------------------------------------------------------------------------

// A fake cargo: metadata for the directory it runs in, from a table { dir: { root, members: [[name, dir, publish?]] } }.
function metaSpawn(root, table, calls = []) {
  return (cmd, args, opts) => {
    calls.push({ cmd, args, cwd: norm(path.relative(root, opts.cwd)) || "." });
    const entry = table[norm(path.relative(root, opts.cwd)) || "."];
    if (!entry) return no("error: could not find `Cargo.toml` here");
    if (entry.fail) return no(entry.fail);
    return ok(meta(root, entry.root ?? ".", entry.members.map(([name, dir, publish = null]) => [name, dir, publish])));
  };
}
// Every manifest's directory has a committed Cargo.lock unless `locks` names the ones that do (a workspace with none is a fixture).
const discover = (root, table, manifests, { locks = null, ...extra } = {}) => {
  const calls = [];
  const lockFiles = locks ?? manifests.map((m) => m.replace(/Cargo\.toml$/, "Cargo.lock"));
  const tracked = (name) => (name === "Cargo.lock" ? lockFiles : manifests);
  const found = discoverRustWorkspaces({ root, spawn: metaSpawn(root, table, calls), cargoCommand: ["rustright", "cargo"], tracked, ...extra });
  return { found, calls };
};

test("rustWorkspaces: a fixture directory, a parent-excluded crate and a lockless nested manifest are skipped with a reason, never asked of cargo", (t) => {
  const root = mkRepo({ "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\nexclude = ["vendor/patched", "crates/.audit"]\n\n[patch.crates-io]\npatched = { path = "vendor/patched" }\n' }, t);
  const table = {
    ".": { root: ".", members: [["core", "crates/core"]] },
    "apps/demo": { root: "apps/demo", members: [["demo", "apps/demo"]] },
  };
  const manifests = ["Cargo.toml", "crates/core/Cargo.toml", "apps/demo/Cargo.toml", "vendor/patched/Cargo.toml", "ci/consumers/core-user/Cargo.toml", "spikes/try/Cargo.toml"];
  const { found, calls } = discover(root, table, manifests, { fixtures: ["ci/consumers"], locks: ["Cargo.lock", "apps/demo/Cargo.lock", "vendor/patched/Cargo.lock"] });
  assert.deepEqual(found.units.map((u) => u.name), ["core", "apps/demo:demo"]);
  assert.deepEqual(calls.map((c) => c.cwd), [".", "apps/demo"], "cargo is asked only for the workspaces that are gated");
  const why = Object.fromEntries(found.skipped.map((k) => [k.manifest, k.why]));
  assert.match(why["ci/consumers/core-user/Cargo.toml"], /fixture directory ci\/consumers.*consumer rows/);
  assert.match(why["vendor/patched/Cargo.toml"], /excluded from the Cargo workspace \. by its manifest \(vendor\/patched\).*lock closure/);
  assert.match(why["spikes/try/Cargo.toml"], /no committed Cargo\.lock.*not reproducible/);
  assert.equal(Object.keys(why).length, 3);
  assert.deepEqual(workspaceExcludes((f) => fs.readFileSync(f, "utf8"), root, "."), ["vendor/patched", "crates/.audit"]);
  assert.deepEqual(workspaceExcludes(() => { throw new Error("none"); }, root, "x"), []);
});

test("manifestChangeIsLockReflected: patch, workspace dependency and membership changes are in the lock; profile, package and lint changes are not", () => {
  const base = '[workspace]\nmembers = ["crates/*"]\nexclude = ["vendor/a"]\nresolver = "2"\n\n[workspace.package]\nedition = "2021"\n\n[workspace.dependencies]\nserde = "1.0.0"\n\n[profile.release]\nopt-level = 3\n';
  const edit = (from, to) => { assert.ok(base.includes(from), from); return base.replace(from, to); };
  assert.equal(manifestChangeIsLockReflected(base, base), false, "no change narrows nothing");
  assert.equal(manifestChangeIsLockReflected(null, base), false);
  assert.equal(manifestChangeIsLockReflected(base, `${base}\n[patch.crates-io]\nquick-xml = { path = "vendor/quick-xml" }\n`), true, "a new patch table");
  assert.equal(manifestChangeIsLockReflected(base, edit('serde = "1.0.0"', 'serde = "1.0.200"')), true, "a workspace dependency bump");
  assert.equal(manifestChangeIsLockReflected(base, edit('exclude = ["vendor/a"]', 'exclude = ["vendor/a", "vendor/b"]')), true, "membership");
  assert.equal(manifestChangeIsLockReflected(base, edit('members = ["crates/*"]', 'members = ["crates/*", "apps/*"]')), true, "members");
  assert.equal(manifestChangeIsLockReflected(base, edit('resolver = "2"', 'resolver = "3"')), false, "the resolver is not in the lock");
  assert.equal(manifestChangeIsLockReflected(base, edit('edition = "2021"', 'edition = "2024"')), false, "workspace.package");
  assert.equal(manifestChangeIsLockReflected(base, edit("opt-level = 3", "opt-level = 2")), false, "a profile");
  assert.equal(manifestChangeIsLockReflected(base, `${base}\n[workspace.lints.rust]\nunsafe_code = "forbid"\n`), false, "lints");
  assert.equal(manifestChangeIsLockReflected(base, `${base}\n# why the patch below exists\n[patch.crates-io]\nx = { path = "vendor/x" }\n`), true, "a comment-only line before a new patch table lands in the previous table and changes no build");
  assert.equal(manifestChangeIsLockReflected(base, `${base}# a trailing comment\n`), true, "a comment alone changes no build: the lock graph (unchanged) then reaches no crate");
  assert.equal(manifestChangeIsLockReflected(base, edit("opt-level = 3", "opt-level = 3 # tuned")), false, "a trailing comment on a value line is kept: a # may sit inside a string");
  const tables = manifestTables(base);
  assert.deepEqual([...tables.keys()], ["preamble", "workspace", "workspace.package", "workspace.dependencies", "profile.release"]);
});

test("rustWorkspaces: the root workspace and every standalone workspace below it, each read by its own cargo metadata", (t) => {
  const root = mkRepo({}, t);
  const table = {
    ".": { root: ".", members: [["core", "crates/core"], ["util", "crates/util", []]] },
    "hub/src-tauri": { root: "hub/src-tauri", members: [["app", "hub/src-tauri"]] },
    engine: { root: "engine", members: [["eng", "engine/eng"], ["eng-cli", "engine/eng-cli"]] },
  };
  const manifests = ["Cargo.toml", "crates/core/Cargo.toml", "crates/util/Cargo.toml", "hub/src-tauri/Cargo.toml", "engine/Cargo.toml", "engine/eng/Cargo.toml", "engine/eng-cli/Cargo.toml"];
  const { found, calls } = discover(root, table, manifests);
  assert.equal(found.present, true);
  assert.deepEqual(found.workspaces.map((w) => w.cargoDir), [".", "engine", "hub/src-tauri"]);
  assert.deepEqual(found.units.map((u) => [u.name, u.crate, u.dir, u.cargoDir, u.publish]), [
    ["core", "core", "crates/core", ".", true], ["util", "util", "crates/util", ".", false],
    ["engine:eng", "eng", "engine/eng", "engine", true], ["engine:eng-cli", "eng-cli", "engine/eng-cli", "engine", true],
    ["hub/src-tauri:app", "app", "hub/src-tauri", "hub/src-tauri", true],
  ]);
  assert.deepEqual(calls.map((c) => c.cwd), [".", "engine", "hub/src-tauri"], "one cargo metadata per workspace, in the workspace, shallowest first; members are not asked again");
  for (const c of calls) { assert.equal(c.cmd, "rustright"); assert.deepEqual(c.args, ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"]); }
  assert.deepEqual(found.skipped, []);
  assert.equal(rustUnitName(".", "core"), "core"); assert.equal(rustUnitName("a/b", "c"), "a/b:c");
});

test("rustWorkspaces: a repository with no workspace at the root (nested only) and one with no Rust at all", (t) => {
  const root = mkRepo({}, t);
  const nested = discover(root, { "src-tauri": { root: "src-tauri", members: [["app", "src-tauri"]] } }, ["src-tauri/Cargo.toml"]).found;
  assert.deepEqual(nested.units.map((u) => u.name), ["src-tauri:app"]);
  assert.equal(nested.cargoDir, "src-tauri");
  assert.deepEqual(discoverRustWorkspaces({ root, spawn: () => assert.fail("not run"), cargoCommand: ["cargo"], tracked: () => [] }), { present: false, workspaces: [], units: [], cargoDir: ".", skipped: [] });
});

test("rustWorkspaces: a manifest below a workspace is a workspace of its own when cargo reads it there (an excluded or self-contained crate), a fixture otherwise", (t) => {
  const root = mkRepo({}, t);
  const table = {
    ".": { root: ".", members: [["core", "crates/core"]] },
    "apps/demo": { root: "apps/demo", members: [["demo", "apps/demo"]] },
    "fixtures/old": { fail: "error: current package believes it's in a workspace when it's not" },
  };
  const { found, calls } = discover(root, table, ["Cargo.toml", "crates/core/Cargo.toml", "apps/demo/Cargo.toml", "fixtures/old/Cargo.toml"]);
  assert.deepEqual(found.units.map((u) => u.name), ["core", "apps/demo:demo"]);
  assert.equal(found.skipped.length, 1);
  assert.equal(found.skipped[0].manifest, "fixtures/old/Cargo.toml");
  assert.match(found.skipped[0].why, /below the Cargo workspace \. but not one of its members.*believes it's in a workspace/);
  assert.deepEqual(calls.map((c) => c.cwd), [".", "apps/demo", "fixtures/old"]);
});

test("rustWorkspaces: a manifest outside every workspace that cargo refuses stops the gate, as do a failed or malformed metadata", (t) => {
  const root = mkRepo({}, t);
  // "probe/x" is below the root workspace ".", so cargo's refusal skips it; a manifest outside every workspace has no such excuse.
  const below = discover(root, { ".": { members: [["a", "a"]] }, "probe/x": { fail: "error: broken" } }, ["Cargo.toml", "a/Cargo.toml", "probe/x/Cargo.toml"]).found;
  assert.deepEqual(below.skipped.map((k) => k.manifest), ["probe/x/Cargo.toml"]);
  assert.throws(() => discover(root, { x: { members: [["a", "x"]], root: "x" }, y: { fail: "error: no lock" } }, ["x/Cargo.toml", "y/Cargo.toml"]), /cargo metadata failed: error: no lock \(in y\)/);
  assert.throws(() => discover(root, {}, ["z/Cargo.toml"]), /cargo metadata failed.*\(in z\)/);
  assert.throws(() => discoverRustWorkspaces({ root, spawn: () => ok("nope"), cargoCommand: ["cargo"], tracked: () => ["a/Cargo.toml"] }), /not JSON/);
});

test("rustWorkspaces: an untracked or ignored crate is skipped with a note, never scanned", (t) => {
  const root = mkRepo({}, t);
  const table = { ".": { root: ".", members: [["core", "crates/core"], ["scratchpad", "crates/scratchpad"]] } };
  const { found } = discover(root, table, ["Cargo.toml", "crates/core/Cargo.toml"], { untracked: () => ["notes/new/Cargo.toml", "Cargo.toml"] });
  assert.deepEqual(found.units.map((u) => u.name), ["core"], "the ignored member crates/scratchpad is not a unit");
  assert.deepEqual(found.skipped.map((k) => k.manifest).sort(), ["crates/scratchpad/Cargo.toml", "notes/new/Cargo.toml"]);
  assert.ok(found.skipped.every((k) => /untracked|ignored/.test(k.why)));
});

test("rustUnitOfScanId: a unit named <cargoDir>:<crate> is found among the known names, with or without the feature-set index", () => {
  const known = new Set(["core", "engine:eng", "hub/src-tauri:app", "engine:eng:2"]);
  const cases = {
    "clippy:core": "core", "clippy:core:1": "core", "check-features:core:0": "core", "clippy:engine:eng": "engine:eng", "clippy:engine:eng:1": "engine:eng", "check-features:engine:eng:0": "engine:eng",
    "check-dependent:engine:eng": "engine:eng", "doc:hub/src-tauri:app": "hub/src-tauri:app", "test:rust:hub/src-tauri:app": "hub/src-tauri:app", "test:rust-serial:hub/src-tauri:app:1": "hub/src-tauri:app",
    "clippy:engine:eng:2": "engine:eng:2",
  };
  for (const [id, unit] of Object.entries(cases)) assert.equal(rustUnitOfScanId(id, known), unit, id);
  assert.equal(rustUnitOfScanId("clippy:other", known), null, "an unknown unit is no unit");
  assert.equal(rustUnitOfScanId("clippy:engine:eng:1"), "engine", "without the set the first segment is the name, as for a root workspace");
  for (const id of ["test:npm:a", "tsc:a", "cargo-deny:engine", "cargo-metadata:hub/src-tauri"]) assert.equal(rustUnitOfScanId(id, known), null, id);
});

test("cache digest: each workspace keys on its own lock and members, and the root's shared inputs", () => {
  const units = [
    { name: "a", crate: "a", dir: "crates/a", cargoDir: "." },
    { name: "x:b", crate: "b", dir: "x/b", cargoDir: "x" }, { name: "x:c", crate: "c", dir: "x/c", cargoDir: "x" }, { name: "y:b", crate: "b", dir: "y/b", cargoDir: "y" },
  ];
  const rustLocks = { ".": shipped.parseLock(lockPkg("a", "1.0.0")), x: shipped.parseLock(lockPkg("b", "1.0.0", ["c"]) + lockPkg("c", "1.0.0")), y: shipped.parseLock(lockPkg("b", "1.0.0")) };
  const ids = { "crates/a": "ta", "x/b": "xb", "x/c": "xc", "y/b": "yb", "x/Cargo.lock": "xl", "x/Cargo.toml": "xt", ".cargo": "rc", ".cargo/config.toml": "rcf", "x/.cargo": "xc2", "x/.cargo/config.toml": "xcf", "Cargo.lock": "rl", "ci/gate.config.json": "cfg" };
  const cfgSources = { listFiles: (dir) => (dir.endsWith(".cargo") ? [`${dir}/config.toml`] : [`${dir}/src/lib.rs`]), readBlob: () => "pub fn f() {}\n" };
  const digest = (over = {}, tree = "head", crateFeatures = {}, locks = rustLocks) => buildCacheDigest({ objectId: (p) => ({ ...ids, ...over })[p] ?? null, headTree: tree, rustUnits: units, cargoDir: ".", rustLocks: locks, crateFeatures, ...cfgSources });
  const clippyB = { id: "clippy:x:b", class: "B" };
  const base = digest()(clippyB);
  assert.equal(digest()(clippyB), base);
  assert.equal(digest({}, "other-head")(clippyB), base, "a cargo scan is not keyed on the whole tree");
  assert.notEqual(digest({ "x/b": "xb2" })(clippyB), base, "the unit changed");
  assert.notEqual(digest({ "x/c": "xc9" })(clippyB), base, "b's closure holds c, through x's own lock");
  assert.notEqual(digest({}, "head", {}, { ...rustLocks, x: shipped.parseLock(lockPkg("b", "1.0.0", ["c"]) + lockPkg("c", "1.0.1")) })(clippyB), base, "x's own lock: a closure entry changed");
  assert.equal(digest({ "x/Cargo.lock": "other" })(clippyB), base, "the lock file's own id is not an input; its closure entries are");
  assert.notEqual(digest({ ".cargo/config.toml": "other" })(clippyB), base, "cargo reads the repository root's .cargo from a nested workspace");
  assert.equal(digest({ "crates/a": "ta2" })(clippyB), base, "another workspace's unit is not an input");
  assert.equal(digest({ "y/b": "yb2" })(clippyB), base, "the crate b of workspace y is not the crate b of workspace x");
  assert.equal(digest({ "Cargo.lock": "other" })(clippyB), base, "the root lock is not x's");
  assert.notEqual(digest()({ id: "clippy:y:b", class: "B" }), base, "another unit, another key");
  assert.notEqual(digest()({ id: "clippy:x:c", class: "B" }), base);
  assert.equal(digest({ "x/b": "xb2" })({ id: "clippy:x:c", class: "B" }), digest()({ id: "clippy:x:c", class: "B" }), "c does not depend on b");
  // A test scan of a nested unit is keyed on its declared testInputs, under the qualified or the plain name.
  const testB = { id: "test:rust:x:b", class: "B" };
  assert.equal(digest({}, "t2")(testB), digest({}, "t1")(testB), "no testInputs and no outside navigation in the sources: closure-keyed, reused across trees");
  const withInputs = (extra) => digest({ "fixtures/x": "f1", ...extra }, "t1", { "x:b": { testInputs: ["fixtures/x"] } })(testB);
  assert.equal(digest({ "fixtures/x": "f1" }, "t9", { b: { testInputs: ["fixtures/x"] } })(testB), withInputs(), "the plain crate name works too, and the tree id is not an input");
  assert.notEqual(withInputs({ "fixtures/x": "f2" }), withInputs());
  // Without a lock for the workspace the closure is unknown: the whole tree.
  const noLock = buildCacheDigest({ objectId: () => "x", headTree: "t1", rustUnits: units, cargoDir: ".", rustLocks: { ".": rustLocks["."] }, ...plainSources });
  assert.notEqual(noLock(clippyB), base);
  assert.equal(noLock(clippyB), canonicalJsonOfTree("t1"));
});
const canonicalJsonOfTree = (tree) => `{"kind":"tree","tree":${JSON.stringify(tree)}}`;

// ---- the gate with several workspaces ------------------------------------------------------------------------------------

const MULTI_MANIFESTS = ["Cargo.toml", "crates/a/Cargo.toml", "hub/src-tauri/Cargo.toml", "fixtures/old/Cargo.toml"];
// fixtures/old has a committed lock so that cargo is asked about it (and refuses it); a lockless nested manifest is skipped before cargo.
const MULTI_LOCKS = ["Cargo.lock", "hub/src-tauri/Cargo.lock", "fixtures/old/Cargo.lock"];
function multiSpawn(root, { log = [], diff = "hub/src-tauri/src/lib.rs\n", untracked = "", tracked = MULTI_MANIFESTS, locks = MULTI_LOCKS } = {}) {
  const table = { ".": { root: ".", members: [["a", "crates/a"]] }, "hub/src-tauri": { root: "hub/src-tauri", members: [["app", "hub/src-tauri"]] }, "fixtures/old": { fail: "error: not a member" } };
  const cargo = metaSpawn(root, table, log);
  return (cmd, args, opts = {}) => {
    if (cmd === "rustright" && args[1] === "metadata") return cargo(cmd, args, opts);
    const line = args.join(" ");
    if (cmd === "git") {
      if (line === "rev-parse --show-toplevel") return ok(root);
      if (line === "rev-parse HEAD") return ok(HEAD);
      if (args[0] === "ls-files" && args.includes("--others")) return ok(untracked);
      if (args[0] === "ls-files") return ok(args.at(-1).endsWith("Cargo.toml") ? `${tracked.join("\0")}\0` : args.at(-1).endsWith("Cargo.lock") ? `${locks.join("\0")}\0` : "");
      if (args[0] === "diff") return ok(diff);
    }
    if (cmd === "where" || cmd === "which") return args[0] === "rustright" ? ok("/bin/rustright\n") : no();
    return no(`unexpected ${cmd} ${line}`);
  };
}
const multiGateFiles = () => gateFiles({
  "crates/a/Cargo.toml": pkg("a"), "crates/a/src/lib.rs": "", "hub/src-tauri/Cargo.toml": pkg("app"), "hub/src-tauri/src/lib.rs": "pub fn f() {}\n", "hub/src-tauri/Cargo.lock": "version = 4\n",
  "fixtures/old/Cargo.toml": pkg("old"),
});

test("--plan with several workspaces: each unit runs in its own workspace, the fixture is a note, and the unit names carry the workspace", async (t) => {
  const files = multiGateFiles();
  const root = mkRepo(files, t);
  const log = []; const c = capture();
  const code = await main(["--plan", "--base", "origin/main"], { ...c.io, spawn: multiSpawn(root, { log, untracked: "scratch/Cargo.toml\0" }), cwd: root, env: {} });
  assert.equal(code, 0, c.err.join("\n"));
  const out = c.out.join("\n");
  assert.match(out, /rust units: a=crates\/a, hub\/src-tauri:app=hub\/src-tauri \(workspaces \., hub\/src-tauri\)/);
  assert.match(out, /\[B\] clippy:hub\/src-tauri:app :: rustright cargo clippy -p app --no-deps --all-targets --locked -- -D warnings/);
  assert.match(out, /\[A\] cargo-package-list:hub\/src-tauri:app :: /);
  assert.match(out, /\[A\] cargo-metadata:hub\/src-tauri :: /);
  assert.doesNotMatch(out, /clippy:a\b|cargo-package-list:a\b/, "the root workspace's crate did not change");
  assert.match(out, /note: fixtures\/old\/Cargo\.toml is skipped, not scanned: below the Cargo workspace \. but not one of its members/);
  assert.match(out, /note: scratch\/Cargo\.toml is skipped, not scanned: untracked/);
  const metadata = log.filter((l) => l.cwd !== undefined);
  assert.deepEqual(metadata.map((l) => l.cwd), [".", "fixtures/old", "hub/src-tauri"], "shallowest first, by name; the member of a workspace is not asked again");
});

test("--plan: a workspace-wide change in one workspace puts only its crates in scope; no scan is planned for the others", async (t) => {
  const files = multiGateFiles();
  const root = mkRepo(files, t);
  const c = capture();
  const code = await main(["--plan"], { ...c.io, spawn: multiSpawn(root, { diff: "hub/src-tauri/Cargo.lock\n" }), cwd: root, env: {} });
  assert.equal(code, 0, c.err.join("\n"));
  const out = c.out.join("\n");
  assert.match(out, /\[B\] clippy:hub\/src-tauri:app ::/);
  assert.match(out, /\[A\] cargo-deny:hub\/src-tauri ::/);
  assert.doesNotMatch(out, /clippy:a\b/);
  assert.doesNotMatch(out, /\[A\] cargo-deny ::/);
});

// ---- install-hook: chaining and the binary search (adoption gaps 2 and 5) ------------------------------------------------------

function hookEnv(root, state, { listed = null } = {}) {
  return (cmd, args) => {
    const line = args.join(" ");
    if (cmd === "git" && line === "rev-parse --show-toplevel") return ok(root);
    if (cmd === "git" && line === "rev-parse --git-dir") return ok(".git\n");
    if (cmd === "git" && line === "config --get core.hooksPath") return state.hooksPath ? ok(`${state.hooksPath}\n`) : no();
    if (cmd === "git" && args[0] === "config" && args[1] === "core.hooksPath") { state.hooksPath = args[2]; state.sets.push(args[2]); return ok(); }
    if (cmd === "git" && args[0] === "ls-files") return listed === null ? no("not listed") : ok(listed.map((f) => `${f}\0`).join(""));
    return no(`unexpected ${cmd} ${line}`);
  };
}
const withGitHooks = (root, names) => { fs.mkdirSync(path.join(root, ".git", "hooks"), { recursive: true }); for (const n of names) fs.writeFileSync(path.join(root, ".git", "hooks", n), "#!/bin/sh\nexit 0\n"); };

test("install-hook: every hook present in .git/hooks gets a trampoline, pre-push chains at run time, and it says which hooks it chained", async (t) => {
  const root = mkRepo({ "package.json": '{"name":"app","devDependencies":{"@rightkit/release":"0.5.0"}}', "node_modules/@rightkit/release/bin/rightkit-gate.mjs": "" }, t);
  withGitHooks(root, ["pre-commit", "commit-msg", "pre-push", "pre-receive", "update", "post-receive", "pre-commit.sample", "not-a-hook", "applypatch-msg.sample", "pre-receive.sample"]);
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  const code = await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root });
  assert.equal(code, 0, c.err.join("\n"));
  assert.deepEqual(fs.readdirSync(path.join(root, ".githooks")).sort(), ["commit-msg", "not-a-hook", "post-receive", "pre-commit", "pre-push", "pre-receive", "update"], "every file without a .sample suffix is chained, pre-receive, update and post-receive included; samples get none");
  for (const name of ["pre-commit", "commit-msg", "pre-receive", "update", "post-receive", "not-a-hook"]) {
    const body = fs.readFileSync(path.join(root, ".githooks", name), "utf8");
    assert.equal(body, trampolineText(name));
    assert.match(body, new RegExp(`"\\$\\(git rev-parse --git-dir\\)/hooks/${name}"`));
    assert.match(body, /exec "\$hook" "\$@"/);
    assert.ok(!body.includes(root) && !/[A-Za-z]:[\\/]/.test(body), "no absolute path in a committed hook");
  }
  const prePush = fs.readFileSync(path.join(root, ".githooks", "pre-push"), "utf8");
  assert.match(prePush, /own="\$\(git rev-parse --git-dir\)\/hooks\/pre-push"/);
  assert.ok(!prePush.includes(root) && !/[A-Za-z]:[\\/]/.test(prePush.replace(/\\\//g, "")), "no absolute path in the pre-push hook");
  const out = c.out.join("\n");
  assert.match(out, /chained hooks from \.git\/hooks: commit-msg, not-a-hook, post-receive, pre-commit, pre-push \(run first, then the gate\), pre-receive, update/);
  for (const name of ["pre-receive", "update", "post-receive"]) assert.match(out, new RegExp(`wrote \\.githooks/${name} \\(runs \\.git/hooks/${name}\\)`));
  assert.match(out, /wrote \.githooks\/pre-commit \(runs \.git\/hooks\/pre-commit\)/);
  assert.match(out, /git add --chmod=\+x \.githooks\/pre-push \.githooks\/commit-msg \.githooks\/not-a-hook \.githooks\/post-receive \.githooks\/pre-commit \.githooks\/pre-receive \.githooks\/update/);
  assert.match(out, /@rightkit\/release is declared in package\.json/);
  assert.ok(!c.err.join("\n").includes("WARNING"));
  // Running it again changes nothing.
  const again = capture();
  assert.equal(await main(["install-hook"], { ...again.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 0);
  assert.ok(again.out.some((l) => /\.githooks\/pre-commit already current/.test(l)) && again.out.some((l) => /\.githooks\/pre-push already current/.test(l)));
});

test("install-hook: a .git/hooks file whose name is not a plain hook name is reported and never written into a shell script", async (t) => {
  const root = mkRepo({ "package.json": '{"name":"app","devDependencies":{"@rightkit/release":"0.5.0"}}' }, t);
  withGitHooks(root, ["pre-commit", "pre-commit;touch x", "post-receive", ".DS_Store"]);
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 0, c.err.join("\n"));
  assert.deepEqual(fs.readdirSync(path.join(root, ".githooks")).sort(), ["post-receive", "pre-commit", "pre-push"]);
  assert.match(c.err.join("\n"), /WARNING: not chaining \.git\/hooks\/\.DS_Store, \.git\/hooks\/pre-commit;touch x: not a plain hook file name/);
  for (const name of fs.readdirSync(path.join(root, ".githooks"))) assert.ok(!fs.readFileSync(path.join(root, ".githooks", name), "utf8").includes("touch x"), name);
});

test("install-hook: a different hook already in .githooks is never replaced without --force, and nothing changes then", async (t) => {
  const root = mkRepo({ ".githooks/pre-commit": "#!/bin/sh\necho mine\n" }, t);
  withGitHooks(root, ["pre-commit"]);
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state), cwd: root }), 1);
  assert.match(c.err.join("\n"), /\.githooks\/pre-commit already exists and differs; nothing changed/);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), "#!/bin/sh\necho mine\n");
  assert.equal(fs.existsSync(path.join(root, ".githooks", "pre-push")), false, "no partial install");
  assert.deepEqual(state.sets, []);
  const forced = capture();
  assert.equal(await main(["install-hook", "--force"], { ...forced.io, spawn: hookEnv(root, state), cwd: root }), 0);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), trampolineText("pre-commit"));
  assert.deepEqual(state.sets, [".githooks"]);
});

test("install-hook: without .git/hooks files nothing is chained, and it warns when no package depends on @rightkit/release", async (t) => {
  const root = mkRepo({ "package.json": '{"name":"app","dependencies":{"left-pad":"1.0.0"}}', "tauri-app-next/package.json": '{"name":"ui"}' }, t);
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state, { listed: ["package.json", "tauri-app-next/package.json"] }), cwd: root }), 0);
  assert.match(c.out.join("\n"), /chained hooks from \.git\/hooks: none; the pre-push hook still runs \.git\/hooks\/pre-push first if one appears later/);
  assert.match(c.err.join("\n"), /WARNING: no package\.json in this repository depends on @rightkit\/release/);
  assert.deepEqual(fs.readdirSync(path.join(root, ".githooks")), ["pre-push"]);
  // A workspace package that declares it silences the warning, and the package itself counts.
  const nested = mkRepo({ "tauri-app-next/package.json": '{"name":"ui","devDependencies":{"@rightkit/release":"0.5.0"}}' }, t);
  const n = capture();
  assert.equal(await main(["install-hook"], { ...n.io, spawn: hookEnv(nested, { hooksPath: "", sets: [] }, { listed: ["tauri-app-next/package.json"] }), cwd: nested }), 0);
  assert.ok(!n.err.join("\n").includes("WARNING"));
  assert.match(n.out.join("\n"), /declared in tauri-app-next\/package\.json/);
  const self = mkRepo({ "packages/release/package.json": '{"name":"@rightkit/release"}' }, t);
  const s = capture();
  assert.equal(await main(["install-hook"], { ...s.io, spawn: hookEnv(self, { hooksPath: "", sets: [] }, { listed: ["packages/release/package.json"] }), cwd: self }), 0);
  assert.ok(!s.err.join("\n").includes("WARNING"));
});

// The hook itself, run by a real sh in a real scratch git repository with stub executables.
const SH = spawnSync("sh", ["-c", "exit 0"], { encoding: "utf8" });
const shAvailable = !SH.error && SH.status === 0;
function hookRepo(t, files) {
  const root = mkRepo({}, t);
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  assert.equal(git("init", "-q").status, 0);
  for (const [name, body] of Object.entries(files)) { const file = path.join(root, name); fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); fs.chmodSync(file, 0o755); }
  return root;
}
const recorder = (log, label) => `#!/bin/sh\n{ echo "${label} $*"; cat; echo "--"; } >> "${log.replaceAll("\\", "/")}"\n`;
const runHook = (root, input, { path: extraPath = [], env = {} } = {}) => spawnSync("sh", [".githooks/pre-push", "origin", "https://example.test/r.git"], {
  cwd: root, input, encoding: "utf8", env: { ...process.env, ...env, PATH: [...extraPath, process.env.PATH].join(path.delimiter) },
});

test("the pre-push hook: runs .git/hooks/pre-push first, then the check, then the gate; the ref lines reach the own hook and the gate, and the first failure stops the push", { skip: !shAvailable && "no sh" }, (t) => {
  const log = path.join(mkRepo({}, t), "calls.log");
  const root = hookRepo(t, {
    ".githooks/pre-push": hookText("bin/rightkit-gate.mjs"),
    ".git/hooks/pre-push": recorder(log, "own"),
    "node_modules/.bin/rightkit-gate": recorder(log, "gate"),
  });
  const ref = `refs/heads/main ${"a".repeat(40)} refs/heads/main ${"0".repeat(40)}\n`;
  const ran = runHook(root, ref);
  assert.equal(ran.status, 0, ran.stderr);
  const calls = fs.readFileSync(log, "utf8");
  assert.equal(calls, `own origin https://example.test/r.git\n${ref}--\ngate check --commit HEAD\n--\ngate --base origin/main\n${ref}--\n`, "own hook first with git's arguments, then the check (no stdin), then the gate with the ref lines");
  assert.match(ran.stderr, /rightkit-gate: running \.git\/hooks\/pre-push/);
  assert.equal(ran.stderr.match(/rightkit-gate: using node_modules\/\.bin\/rightkit-gate/g).length, 1, "the binary it chose is named once");
  // A failing own hook blocks the push and the gate does not run.
  fs.writeFileSync(path.join(root, ".git", "hooks", "pre-push"), "#!/bin/sh\ncat >/dev/null\nexit 7\n");
  fs.rmSync(log);
  const failed7 = runHook(root, ref);
  assert.equal(failed7.status, 7);
  assert.equal(fs.existsSync(log), false, "the gate never ran");
  // A failing gate fails the push with its code, even when the own hook passed.
  fs.writeFileSync(path.join(root, ".git", "hooks", "pre-push"), "#!/bin/sh\ncat >/dev/null\nexit 0\n");
  fs.writeFileSync(path.join(root, "node_modules", ".bin", "rightkit-gate"), "#!/bin/sh\ncat >/dev/null\nexit 3\n");
  assert.equal(runHook(root, ref).status, 3);
});

// A gate binary that records each call and fails the one whose first argument is in FAIL.
const failing = (log, ...fail) => `#!/bin/sh\ncat >/dev/null\necho "$*" >> "${log.replaceAll("\\", "/")}"\nfor f in ${fail.map((f) => `"${f}"`).join(" ")}; do if [ "$1" = "$f" ]; then echo "gate stub: $1 failed" >&2; exit ${"${CODE:-1}"}; fi; done\nexit 0\n`;

test("the pre-push hook: a red check stops the push with its own exit code before the gate starts", { skip: !shAvailable && "no sh" }, (t) => {
  const log = path.join(mkRepo({}, t), "calls.log");
  const root = hookRepo(t, { ".githooks/pre-push": hookText("bin/rightkit-gate.mjs"), "node_modules/.bin/rightkit-gate": failing(log, "check") });
  const ran = runHook(root, `refs/heads/main ${"a".repeat(40)} refs/heads/main ${"0".repeat(40)}\n`);
  assert.equal(ran.status, 1);
  assert.match(ran.stderr, /gate stub: check failed/);
  assert.equal(fs.readFileSync(log, "utf8"), "check --commit HEAD\n", "the gate never ran");
  const two = runHook(root, `refs/heads/main ${"a".repeat(40)} refs/heads/main ${"0".repeat(40)}\n`, { env: { CODE: "2" } });
  assert.equal(two.status, 2, "a check that could not run (exit 2) stops the push too");
});

test("the pre-push hook: a push that only deletes refs has no content to check and skips the check, a mixed push checks", { skip: !shAvailable && "no sh" }, (t) => {
  const log = path.join(mkRepo({}, t), "calls.log");
  const root = hookRepo(t, { ".githooks/pre-push": hookText("bin/rightkit-gate.mjs"), "node_modules/.bin/rightkit-gate": failing(log, "check") });
  const zero = "0".repeat(40);
  const deletion = runHook(root, `(delete) ${zero} refs/heads/old ${"b".repeat(40)}\n`);
  assert.equal(deletion.status, 0, deletion.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "--base origin/main\n", "only the gate ran, and it skips deletions itself");
  assert.match(deletion.stderr, /only deletions are pushed; nothing to check/);
  fs.rmSync(log);
  const mixed = runHook(root, `(delete) ${zero} refs/heads/old ${"b".repeat(40)}\nrefs/heads/main ${"a".repeat(40)} refs/heads/main ${zero}\n`);
  assert.equal(mixed.status, 1, "a push with content is checked even when it also deletes");
  assert.equal(fs.readFileSync(log, "utf8"), "check --commit HEAD\n");
  // Nothing on stdin (the hook run by hand): the check runs, as it does for the gate's own HEAD.
  fs.rmSync(log);
  assert.equal(runHook(root, "").status, 1);
  assert.equal(fs.readFileSync(log, "utf8"), "check --commit HEAD\n");
});

test("the pre-push hook: without an own hook only the gate runs; its temp file is gone afterwards", { skip: !shAvailable && "no sh" }, (t) => {
  const tmp = mkRepo({}, t);
  const log = path.join(tmp, "calls.log");
  const root = hookRepo(t, { ".githooks/pre-push": hookText("bin/rightkit-gate.mjs"), "node_modules/.bin/rightkit-gate": recorder(log, "gate") });
  const ran = runHook(root, "x y z w\n", { env: { TMPDIR: tmp } });
  assert.equal(ran.status, 0, ran.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "gate check --commit HEAD\n--\ngate --base origin/main\nx y z w\n--\n");
  assert.deepEqual(fs.readdirSync(tmp).sort(), ["calls.log"], "the buffered stdin is removed");
  assert.ok(!ran.stderr.includes("running"), "nothing to chain");
});

test("the pre-push hook: the gate binary is the root install, else a workspace package's (one to three levels down), else pnpm exec; it says which", { skip: !shAvailable && "no sh" }, (t) => {
  const tmp = mkRepo({}, t);
  const log = path.join(tmp, "calls.log");
  const base = { ".githooks/pre-push": hookText("bin/rightkit-gate.mjs") };
  // heardright / cutright / coderight: the dependency lives in tauri-app-next (or apps/*), never at the root.
  for (const where of ["tauri-app-next", "apps/web", "packages/x/y"]) {
    fs.rmSync(log, { force: true });
    const root = hookRepo(t, { ...base, [`${where}/node_modules/.bin/rightkit-gate`]: recorder(log, `gate@${where}`) });
    const ran = runHook(root, "in\n");
    assert.equal(ran.status, 0, ran.stderr);
    assert.equal(fs.readFileSync(log, "utf8"), `gate@${where} check --commit HEAD\n--\ngate@${where} --base origin/main\nin\n--\n`);
    assert.match(ran.stderr, new RegExp(`rightkit-gate: using ${where}/node_modules/\\.bin/rightkit-gate`));
  }
  // The root install wins over a workspace package's.
  fs.rmSync(log, { force: true });
  const both = hookRepo(t, { ...base, "node_modules/.bin/rightkit-gate": recorder(log, "root"), "tauri-app-next/node_modules/.bin/rightkit-gate": recorder(log, "nested") });
  assert.equal(runHook(both, "in\n").status, 0);
  assert.match(fs.readFileSync(log, "utf8"), /^root /);
  // Nothing installed: pnpm exec, which is what finds a root dependency.
  fs.rmSync(log, { force: true });
  const bin = mkRepo({ pnpm: recorder(log, "pnpm") }, t);
  fs.chmodSync(path.join(bin, "pnpm"), 0o755);
  const none = hookRepo(t, base);
  const viaPnpm = runHook(none, "in\n", { path: [bin] });
  assert.equal(viaPnpm.status, 0, viaPnpm.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "pnpm exec rightkit-gate check --commit HEAD\n--\npnpm exec rightkit-gate --base origin/main\nin\n--\n");
  assert.match(viaPnpm.stderr, /rightkit-gate: using pnpm exec rightkit-gate/);
});

test("the trampoline hook runs the repository's own hook with its arguments and passes when there is none", { skip: !shAvailable && "no sh" }, (t) => {
  const tmp = mkRepo({}, t);
  const log = path.join(tmp, "calls.log");
  const root = hookRepo(t, { ".githooks/commit-msg": trampolineText("commit-msg"), ".git/hooks/commit-msg": recorder(log, "own") });
  const first = spawnSync("sh", [".githooks/commit-msg", "MSG_FILE"], { cwd: root, encoding: "utf8", input: "" });
  assert.equal(first.status, 0, first.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "own MSG_FILE\n--\n");
  fs.writeFileSync(path.join(root, ".git", "hooks", "commit-msg"), "#!/bin/sh\nexit 9\n");
  assert.equal(spawnSync("sh", [".githooks/commit-msg", "MSG_FILE"], { cwd: root, encoding: "utf8" }).status, 9, "the own hook's failure fails the commit");
  fs.rmSync(path.join(root, ".git", "hooks", "commit-msg"));
  assert.equal(spawnSync("sh", [".githooks/commit-msg", "MSG_FILE"], { cwd: root, encoding: "utf8" }).status, 0, "no own hook: nothing to run");
});

// ---- quiet lease lane --------------------------------------------------------------------------------------------

test("quietLeaseLane: acquires through rustright with the gate pid, hands the lease id to the lane as RUSTRIGHT_QUIET_LEASE, releases it; no rustright or a refusal runs the lane without one", async () => {
  const calls = []; const out = [];
  const spawnWith = (answers) => (cmd, args) => {
    calls.push([cmd, ...args]);
    if (cmd === "where" || cmd === "which") return answers.present ? ok("D:\bin\rustright.cmd\n") : no();
    if (cmd === "rustright" && args[1] === "acquire") return answers.acquire;
    if (cmd === "rustright" && args[1] === "release") return answers.release ?? ok("");
    return no(`unexpected ${cmd}`);
  };
  const o = (answers, platform = "win32") => ({ spawn: spawnWith(answers), out: (m) => out.push(m), platform });
  const held = quietLeaseLane(o({ present: true, acquire: ok(JSON.stringify({ type: "QUIET_LEASE", granted: true, state: "HELD", id: "q-abc" })) }), { reason: "gate test", pid: 4242 });
  const lease = await held.acquire();
  assert.deepEqual(lease.env, { RUSTRIGHT_QUIET_LEASE: "q-abc" });
  assert.equal(lease.id, "q-abc");
  const acquire = calls.find((c) => c[0] === "rustright" && c[2] === "acquire");
  assert.deepEqual(acquire.slice(3), ["--ttl-ms", String(QUIET_LEASE_TTL_MS), "--reason", "gate test", "--wait-drain-ms", String(QUIET_LEASE_WAIT_DRAIN_MS), "--holder-pid", "4242", "--json"]);
  await held.release(lease.id);
  assert.deepEqual(calls.at(-1), ["rustright", "quiet", "release", "q-abc"]);
  assert.ok(out.some((m) => /quiet lease q-abc held/.test(m)) && out.some((m) => /quiet lease q-abc released/.test(m)));
  // Refused by the broker (non-zero exit with the typed event), granted:false, no id, and no rustright at all: null, with a note each.
  for (const [label, answers] of [
    ["refused", { present: true, acquire: no('{"type":"QUIET_LEASE","granted":false,"state":"DRAIN_TIMEOUT","code":"LEASE_TIMEOUT","error":"the host did not go quiet"}') }],
    ["granted:false", { present: true, acquire: ok('{"type":"QUIET_LEASE","granted":false}') }],
    ["no id", { present: true, acquire: ok("{}") }],
    ["absent", { present: false }],
  ]) {
    out.length = 0;
    assert.equal(await quietLeaseLane(o(answers)).acquire(), null, label);
    assert.ok(out.some((m) => /without a (broker )?lease/.test(m)), `${label} is reported`);
  }
});

// ---- the check subcommand ----------------------------------------------------------------------------------------------------

test("check delegates to the app check with its own arguments, label and cwd, reads no stdin and runs no git of its own", async () => {
  const seen = [];
  const appCheck = async (argv, io) => { seen.push({ argv, label: io.label, cwd: io.cwd, hasOut: typeof io.out === "function" && typeof io.err === "function" }); return seen.length === 1 ? 1 : 2; };
  const spawn = () => { throw new Error("check must not spawn anything itself"); };
  const readStdin = () => { throw new Error("check must not read stdin"); };
  const c = capture();
  assert.equal(await main(["check", "--repo", "/r", "--commit", "abc", "--only", "qa-platform,skip-checks", "--print-baseline"], { ...c.io, appCheck, spawn, readStdin, cwd: "/work" }), 1);
  assert.equal(await main(["check", "--worktree"], { ...c.io, appCheck, spawn, readStdin, cwd: "/work" }), 2);
  assert.deepEqual(seen, [
    { argv: ["--repo", "/r", "--commit", "abc", "--only", "qa-platform,skip-checks", "--print-baseline"], label: "check", cwd: "/work", hasOut: true },
    { argv: ["--worktree"], label: "check", cwd: "/work", hasOut: true },
  ]);
});

test("check runs the real app check: findings on stdout, exit 0 green and 1 red, and the unknown-command usage names it", async (t) => {
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-check-")));
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ scripts: { ship: "right-release build --skip-checks" } }));
  git("add", "-A"); git("commit", "-q", "-m", "red");
  const red = capture();
  assert.equal(await main(["check", "--repo", repo, "--commit", "HEAD"], red.io), 1);
  assert.match(red.out.join("\n"), /^FAIL skip-checks \(G17\): package\.json:1 script "ship" runs right-release with --skip-checks$/m);
  assert.match(red.out.at(-1), /^check: 1 problem\(s\) in \w{12} \(13 rules, /);
  fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ scripts: { ship: "right-release build" } }));
  git("commit", "-q", "-am", "green");
  const green = capture();
  assert.equal(await main(["check"], { ...green.io, cwd: repo }), 0, green.err.join("\n"));
  assert.match(green.out.at(-1), /^check: \w{12} passed 13 rules in [\d.]+s$/, "no --repo: the git top-level of the working directory");
  const unknown = capture();
  assert.equal(await main(["bogus"], unknown.io), 2);
  assert.match(unknown.err.join("\n"), /rightkit-gate check \[--repo <dir>\] \[--commit <rev> \| --worktree\] \[--only <rule,\.\.\.>\] \[--list\] \[--print-baseline\] \[--baseline <file>\]/);
});

// ---- install-hook: the ownership pre-commit step (right-release hooks install is an alias of install-hook) ----------------------

const PINNED = "node_modules/@rightkit/release/cli/right-release.mjs";
const ownershipRepo = (t, extra = {}) => mkRepo({ "package.json": '{"name":"app","devDependencies":{"@rightkit/release":"0.6.5"}}', "node_modules/@rightkit/release/bin/rightkit-gate.mjs": "", [PINNED]: "", ...extra }, t);

test("install-hook also writes the ownership pre-commit step where @rightkit/release is installed, and running it again changes nothing", async (t) => {
  const root = ownershipRepo(t);
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 0, c.err.join("\n"));
  const hooks = path.join(root, ".githooks");
  assert.deepEqual(fs.readdirSync(hooks).sort(), ["pre-commit", "pre-push"]);
  assert.equal(fs.readFileSync(path.join(hooks, "pre-commit"), "utf8"), renderPreCommitHook(), "the very hook right-release hooks install writes");
  assert.match(fs.readFileSync(path.join(hooks, "pre-commit"), "utf8"), new RegExp(HOOK_MARKER));
  assert.match(c.out.join("\n"), /wrote \.githooks\/pre-commit \(the advisory ownership gate: right-release ownership --staged\)/);
  assert.match(c.out.join("\n"), /git add --chmod=\+x \.githooks\/pre-push \.githooks\/pre-commit/);
  assert.ok(!c.out.join("\n").includes("no ownership pre-commit step"));
  const again = capture();
  assert.equal(await main(["install-hook"], { ...again.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 0);
  assert.ok(again.out.some((l) => /\.githooks\/pre-commit already current/.test(l)) && again.out.some((l) => /\.githooks\/pre-push already current/.test(l)));
  assert.deepEqual(state.sets, [".githooks"]);
});

test("install-hook without @rightkit/release installed keeps the plain trampoline for the repository's own pre-commit hook and says the ownership step is missing", async (t) => {
  const root = mkRepo({ "package.json": '{"name":"app","devDependencies":{"@rightkit/release":"0.6.5"}}' }, t);
  withGitHooks(root, ["pre-commit"]);
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(root, { hooksPath: "", sets: [] }, { listed: ["package.json"] }), cwd: root }), 0);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), trampolineText("pre-commit"));
  assert.match(c.out.join("\n"), new RegExp(`no ownership pre-commit step: ${PINNED.replaceAll(".", "\\.")} is not installed here`));
});

test("install-hook: the repository's own pre-commit hook keeps running first, through a trampoline beside the ownership step, and is never rewritten", async (t) => {
  const root = ownershipRepo(t);
  withGitHooks(root, ["pre-commit"]);
  const own = fs.readFileSync(path.join(root, ".git", "hooks", "pre-commit"), "utf8");
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 0, c.err.join("\n"));
  const hooks = path.join(root, ".githooks");
  assert.equal(fs.readFileSync(path.join(hooks, "pre-commit"), "utf8"), renderPreCommitHook());
  assert.equal(fs.readFileSync(path.join(hooks, "pre-commit.rightkit-foreign"), "utf8"), trampolineText("pre-commit"), "the ownership hook runs this one first, and it runs .git/hooks/pre-commit");
  assert.equal(fs.readFileSync(path.join(root, ".git", "hooks", "pre-commit"), "utf8"), own, "the user's hook is untouched");
  assert.match(c.out.join("\n"), /wrote \.githooks\/pre-commit\.rightkit-foreign \(runs \.git\/hooks\/pre-commit, before the ownership gate\)/);
  const again = capture();
  assert.equal(await main(["install-hook"], { ...again.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 0);
  assert.ok(again.out.some((l) => /pre-commit\.rightkit-foreign already current/.test(l)));
  // An earlier plain `hooks install` left the ownership hook in .git/hooks and the user's hook beside it: that one is chained, not the ownership hook twice.
  const old = ownershipRepo(t);
  withGitHooks(old, ["pre-commit.rightkit-foreign"]);
  fs.writeFileSync(path.join(old, ".git", "hooks", "pre-commit"), renderPreCommitHook());
  assert.equal(await main(["install-hook"], { ...capture().io, spawn: hookEnv(old, { hooksPath: "", sets: [] }, { listed: ["package.json"] }), cwd: old }), 0);
  assert.equal(fs.readFileSync(path.join(old, ".githooks", "pre-commit.rightkit-foreign"), "utf8"), trampolineText("pre-commit.rightkit-foreign"));
  assert.equal(fs.readdirSync(path.join(old, ".githooks")).includes("pre-commit.rightkit-foreign"), true);
});

test("install-hook: installing @rightkit/release later upgrades its own earlier trampoline and an older ownership hook in place, without --force", async (t) => {
  const root = mkRepo({ "package.json": '{"name":"app","devDependencies":{"@rightkit/release":"0.6.5"}}' }, t);
  withGitHooks(root, ["pre-commit"]);
  const state = { hooksPath: "", sets: [] };
  const io = () => ({ ...capture().io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root });
  assert.equal(await main(["install-hook"], io()), 0);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), trampolineText("pre-commit"), "no @rightkit/release yet: the plain trampoline");
  fs.mkdirSync(path.join(root, "node_modules/@rightkit/release/cli"), { recursive: true });
  fs.writeFileSync(path.join(root, PINNED), "");
  const c = capture();
  assert.equal(await main(["install-hook"], { ...io(), ...c.io }), 0, c.err.join("\n"));
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), renderPreCommitHook(), "now the ownership gate");
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit.rightkit-foreign"), "utf8"), trampolineText("pre-commit"), "and the repository's own hook still runs first");
  assert.match(c.out.join("\n"), /replaced \.githooks\/pre-commit \(the advisory ownership gate/);
  // An ownership hook of an older text (same marker) is updated too.
  fs.writeFileSync(path.join(root, ".githooks", "pre-commit"), `#!/bin/sh\n# ${HOOK_MARKER}: an older text\nexit 0\n`);
  const again = capture();
  assert.equal(await main(["install-hook"], { ...io(), ...again.io }), 0, again.err.join("\n"));
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), renderPreCommitHook());
});

test("install-hook: a pre-commit hook of the user's own already in .githooks is never replaced without --force, and nothing changes then", async (t) => {
  const root = ownershipRepo(t, { ".githooks/pre-commit": "#!/bin/sh\necho mine\n" });
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 1);
  assert.match(c.err.join("\n"), /\.githooks\/pre-commit already exists and differs; nothing changed\. .* replace it with the ownership pre-commit hook/);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), "#!/bin/sh\necho mine\n");
  assert.equal(fs.existsSync(path.join(root, ".githooks", "pre-push")), false, "no partial install");
  assert.deepEqual(state.sets, []);
  // The same for a foreign trampoline file that is not ours.
  const other = ownershipRepo(t, { ".githooks/pre-commit.rightkit-foreign": "#!/bin/sh\necho mine too\n" });
  withGitHooks(other, ["pre-commit"]);
  const o = capture();
  assert.equal(await main(["install-hook"], { ...o.io, spawn: hookEnv(other, { hooksPath: "", sets: [] }, { listed: ["package.json"] }), cwd: other }), 1);
  assert.match(o.err.join("\n"), /\.githooks\/pre-commit\.rightkit-foreign already exists and differs/);
  assert.equal(fs.readFileSync(path.join(other, ".githooks", "pre-commit.rightkit-foreign"), "utf8"), "#!/bin/sh\necho mine too\n");
  // --force replaces both.
  const forced = capture();
  assert.equal(await main(["install-hook", "--force"], { ...forced.io, spawn: hookEnv(root, state, { listed: ["package.json"] }), cwd: root }), 0);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-commit"), "utf8"), renderPreCommitHook());
});

test("the installed pre-commit hook: the repository's own hook runs first and can stop the commit, then the pinned right-release runs ownership --staged", { skip: !shAvailable && "no sh" }, async (t) => {
  const log = path.join(mkRepo({}, t), "calls.log");
  const logPath = log.replaceAll("\\", "/");
  const root = hookRepo(t, {
    "package.json": '{"name":"app","devDependencies":{"@rightkit/release":"0.6.5"}}',
    "node_modules/@rightkit/release/bin/rightkit-gate.mjs": "",
    [PINNED]: `console.log("ownership " + process.argv.slice(2).join(" ")); require("node:fs").appendFileSync("${logPath}", "pinned " + process.argv.slice(2).join(" ") + "\\n"); process.exit(Number(process.env.OWNERSHIP_EXIT ?? 0));\n`.replace("require(", "process.getBuiltinModule("),
    ".git/hooks/pre-commit": `#!/bin/sh\necho "own pre-commit" >> "${logPath}"\nexit \${OWN_EXIT:-0}\n`,
  });
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  assert.equal(await main(["install-hook"], { ...capture().io, cwd: root, spawn: (cmd, args, opts) => spawnSync(cmd, args, { ...opts, encoding: "utf8" }) }), 0);
  assert.equal(git("config", "--get", "core.hooksPath").stdout.trim(), ".githooks");
  fs.writeFileSync(path.join(root, "a.txt"), "x\n");
  git("add", "a.txt");
  const commit = (env = {}) => spawnSync("git", ["commit", "-q", "-m", "m"], { cwd: root, encoding: "utf8", env: { ...process.env, ...env } });
  const blocked = commit({ OWN_EXIT: "5" });
  assert.notEqual(blocked.status, 0, "the user's hook stops the commit");
  assert.equal(fs.readFileSync(log, "utf8"), "own pre-commit\n", "and the ownership gate never ran");
  fs.rmSync(log);
  const ok = commit();
  assert.equal(ok.status, 0, ok.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "own pre-commit\npinned ownership --staged\n");
  fs.writeFileSync(path.join(root, "b.txt"), "y\n");
  git("add", "b.txt");
  fs.rmSync(log);
  const refused = commit({ OWNERSHIP_EXIT: "1" });
  assert.notEqual(refused.status, 0, "a block-tier ownership finding stops the commit");
});

// ---- the whole hook, with the real check: a red tree cannot be pushed -------------------------------------------------------------

test("a scratch repository with a red tree rejects git push through the installed hook with the check's findings, and a green one reaches the gate", { skip: !shAvailable && "no sh" }, async (t) => {
  const tmp = mkRepo({}, t);
  const log = path.join(tmp, "gate.log").replaceAll("\\", "/");
  const gateCli = path.join(path.dirname(fileURLToPath(import.meta.url)), "bin", "rightkit-gate.mjs").replaceAll("\\", "/");
  // The gate binary is the real one for `check` and a recorder for the scans, which need a broker.
  const shim = `#!/bin/sh\nif [ "$1" = check ]; then exec "${process.execPath.replaceAll("\\", "/")}" "${gateCli}" "$@"; fi\ncat >/dev/null\necho "gate $*" >> "${log}"\nexit 0\n`;
  const root = hookRepo(t, {
    "package.json": JSON.stringify({ name: "app", devDependencies: { "@rightkit/release": "0.6.5" }, scripts: { ship: "right-release build --skip-checks" } }),
    "node_modules/.bin/rightkit-gate": shim,
  });
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  for (const [k, v] of [["user.email", "t@example.test"], ["user.name", "t"], ["core.autocrlf", "false"]]) git("config", k, v);
  fs.writeFileSync(path.join(root, ".gitignore"), "node_modules/\n");
  const remote = path.join(tmp, "remote.git");
  assert.equal(spawnSync("git", ["init", "-q", "--bare", remote], { encoding: "utf8" }).status, 0);
  git("remote", "add", "origin", remote);
  assert.equal(await main(["install-hook"], { ...capture().io, cwd: root, spawn: (cmd, args, opts) => spawnSync(cmd, args, { ...opts, encoding: "utf8" }) }), 0);
  git("add", "-A"); git("add", "--chmod=+x", ".githooks/pre-push");
  assert.equal(git("commit", "-q", "-m", "red tree").status, 0);
  const red = git("push", "origin", "HEAD:refs/heads/main");
  assert.notEqual(red.status, 0, "the push is rejected");
  assert.match(red.stderr, /FAIL skip-checks \(G17\): package\.json:1 script "ship" runs right-release with --skip-checks/);
  assert.match(red.stderr, /fix: drop --skip-checks/);
  assert.equal(fs.existsSync(log), false, "the gate never started");
  assert.notEqual(git("ls-remote", "origin").stdout.trim(), git("rev-parse", "HEAD").stdout.trim(), "nothing reached the remote");
  fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "app", devDependencies: { "@rightkit/release": "0.6.5" }, scripts: { ship: "right-release build" } }));
  assert.equal(git("commit", "-q", "-am", "green tree").status, 0);
  const green = git("push", "origin", "HEAD:refs/heads/main");
  assert.equal(green.status, 0, green.stderr);
  assert.match(fs.readFileSync(log, "utf8"), /^gate --base origin\/main$/m, "the gate ran after the check passed");
  assert.equal(git("ls-remote", "origin", "refs/heads/main").stdout.split("\t")[0], git("rev-parse", "HEAD").stdout.trim());
  // Deleting the branch is not blocked by a red tree: there is nothing to check.
  fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ scripts: { ship: "right-release build --skip-checks" } }));
  assert.equal(git("commit", "-q", "-am", "red again").status, 0);
  assert.equal(git("push", "origin", ":refs/heads/main").status, 0);
});

// ---- the Rust gate on a host without the broker: deferral to host qualification ------------------------------------------------

const ZEROS = "0".repeat(40);
const pushLine = (ref, { sha = HEAD, remote = ZEROS } = {}) => `${ref} ${sha} ${ref} ${remote}\n`;
const NO_BROKER = /BLOCKED: neither `rustright` nor the managed cargo shim \(cargo-guard\.mjs\) is first on PATH; plain cargo is never used/;
const snapshot = (root) => fs.readdirSync(root, { recursive: true }).map(String).sort().map((rel) => {
  const abs = path.join(root, rel);
  return [rel, fs.statSync(abs).isFile() ? fs.readFileSync(abs, "utf8") : null];
});

// A repository whose gate runs the whole of main(): git answers from the fixture, `tar` copies it as the exported HEAD, and the
// scan runner is the recorded fake below, so nothing real runs. `broker`: a rustright on PATH; `shim`: a cargo on PATH that is the
// managed shim (its file names cargo-guard.mjs, see shimRead).
// `manifests`: what git ls-files answers for Cargo.toml; `shows`: "<rev>:<path>" -> body for `git show`; `originHead`: what
// `git symbolic-ref refs/remotes/origin/HEAD` answers (null: not set).
function pushSpawn(root, { log = [], diff = "crates/a/src/lib.rs\n", broker = false, shim = false, rustfmtAnswers = true, manifests = ["Cargo.toml", "crates/a/Cargo.toml", "crates/b/Cargo.toml"], shows = {}, originHead = null, tracked = {} } = {}) {
  const metadata = meta(root, ".", [["a", "crates/a"], ["b", "crates/b", []]]);
  return (cmd, args, opts = {}) => {
    log.push([cmd, ...args]);
    const line = args.join(" ");
    if (cmd === "git") {
      if (line === "rev-parse --show-toplevel") return ok(root);
      if (line === "rev-parse HEAD") return ok(HEAD);
      if (args[0] === "ls-files") {
        const named = tracked[path.posix.basename(args.at(-1))]; // `tracked`: other tracked files by name ({ "package.json": [...] })
        return ok(named ? `${named.join("\0")}\0` : args.at(-1).endsWith("Cargo.toml") ? `${manifests.join("\0")}\0` : "");
      }
      if (args[0] === "diff") return ok(diff);
      if (args[0] === "status" || args[0] === "archive") return ok();
      if (args[0] === "show") return shows[args[1]] === undefined ? no("fatal: path does not exist") : ok(shows[args[1]]);
      if (args[0] === "symbolic-ref") return originHead ? ok(`${originHead}\n`) : no("fatal: ref refs/remotes/origin/HEAD is not a symbolic ref");
      return no(`unexpected ${cmd} ${line}`);
    }
    if (cmd === "tar") { fs.cpSync(root, path.join(opts.cwd, "repo"), { recursive: true }); return ok(); }
    if (cmd === "where" || cmd === "which") {
      if (args[0] === "rustright") return broker ? ok("/bin/rustright\n") : no();
      if (args[0] === "cargo") return shim ? ok("/shim/cargo\n") : no();
      return no();
    }
    if (cmd === "rustright" && args[1] === "metadata") return ok(metadata);
    if (cmd === "cargo" && args[0] === "metadata") return ok(metadata);
    if (cmd === "/x/rustfmt" && line === "--version") return rustfmtAnswers ? ok("rustfmt 1.8.0\n") : no("error: rustup could not choose a version of rustfmt to run");
    return no(`unexpected ${cmd} ${line}`);
  };
}
const shimRead = (f) => (norm(f) === "/shim/cargo" ? "#!/usr/bin/env node\nimport \"./cargo-guard.mjs\";\n" : fs.readFileSync(f, "utf8"));
function fakeRun(seen, { rows = {} } = {}) {
  return async (plan, options) => {
    seen.push({ plan, options });
    return {
      scans: plan.scans.map((s) => ({ id: s.id, class: s.class, status: "pass", ...(rows[s.id] ?? {}) })),
      notes: plan.notes, cacheHits: [], deferred: [],
    };
  };
}
const noWrites = { writeFile: () => assert.fail("the gate wrote a file"), mkdir: () => assert.fail("the gate made a directory"), chmod: () => assert.fail("the gate changed a mode") };
async function pushGate(t, { stdin = pushLine("refs/heads/claude/port"), env = {}, files = gateFiles(), spawnOptions = {}, rustfmt = "/x/rustfmt", rows = {}, args = ["--base", "origin/main"], readFile = undefined, io = {} } = {}) {
  const root = mkRepo(files, t);
  const log = []; const seen = []; const c = capture();
  const before = snapshot(root);
  const code = await main(args, {
    ...c.io, ...noWrites, spawn: pushSpawn(root, { log, ...spawnOptions }), cwd: root, env, readStdin: () => stdin,
    runScans: fakeRun(seen, { rows }), ...(rustfmt === false ? {} : { findRustfmt: () => rustfmt }), ...(readFile ? { readFile } : {}), ...io,
  });
  return { code, log, seen, root, before, after: snapshot(root), out: c.out.join("\n"), err: c.err.join("\n") };
}

test("deferral: a claude/* push from a host without the broker runs the non-Rust scans, names the deferred Rust units and scans, exits 0 and writes nothing", async (t) => {
  const r = await pushGate(t);
  assert.equal(r.code, 0, r.err);
  assert.equal(r.seen.length, 1, "the scan runner ran once");
  const ids = r.seen[0].plan.scans.map((s) => s.id);
  for (const id of ["rustfmt:2021", "license", "secret-scan"]) assert.ok(ids.includes(id), `${id} still runs: ${ids}`);
  assert.ok(r.seen[0].plan.scans.every((s) => s.tool !== "rustright" && s.tool !== "cargo"), "no scan goes through the broker");
  assert.ok(!r.log.some((l) => l[0] === "rustright" || l[0] === "cargo"), "neither the broker nor plain cargo was asked anything");
  assert.doesNotMatch(r.err, /BLOCKED/);
  const block = r.err.split("\n").filter((l) => /DEFERRED/.test(l));
  assert.equal(block.length, 1, "one headline");
  assert.match(block[0], /^rightkit-gate: rust gate DEFERRED to host qualification \(no RustRight broker on this host\): rust units a, b; scans not run [^;]+; the pull request needs a HOST RECEIPT before merge$/);
  for (const id of ["cargo-metadata", "cargo-package-list:a", "check-features:a:0", "clippy:a", "doc:a", "test:rust:a"]) assert.ok(block[0].includes(id), `${id} named as not run`);
  assert.ok(!block[0].includes("license") && !block[0].includes("secret-scan") && !block[0].includes("rustfmt"), "scans that ran are not listed as deferred");
  assert.match(r.err, /pushed refs\/heads\/claude\/port \(non-protected, not CI\)\. The HOST RECEIPT .* stays the merge gate; RIGHTKIT_GATE_NO_DEFER=1 keeps the refusal/);
  assert.match(r.out, /note: no RustRight broker on this host: 6 Rust scan\(s\) are deferred to host qualification, not run and not passed/);
  assert.match(r.out, /the scans that could run here passed/);
  assert.deepEqual(r.after, r.before, "the tree is byte for byte what it was");
  assert.deepEqual(r.seen[0].plan.deferred, [], "nothing is left in the plan as deferred");
  assert.equal(r.seen[0].plan.scanSetDigest, digestScanSet(r.seen[0].plan.scans), "the plan digest describes what runs");
});

test("deferral: the units come from the tracked manifests, not cargo, and a Rust-free diff still prints the block with no scan named", async (t) => {
  const r = await pushGate(t, { spawnOptions: { diff: "README.md\n" } });
  assert.equal(r.code, 0, r.err);
  assert.deepEqual(r.seen[0].plan.scans, []);
  assert.match(r.err, /DEFERRED to host qualification \(no RustRight broker on this host\): rust units a, b; scans not run none \(no Rust scan was due for this diff\); the pull request needs a HOST RECEIPT before merge/);
});

test("deferral: every deferrable shape - several non-protected branches, a branch named like a protected one below another name, the deletion of one", async (t) => {
  for (const [name, stdin] of [
    ["two branches", pushLine("refs/heads/claude/a") + pushLine("refs/heads/feature/b")],
    ["claude/main", pushLine("refs/heads/claude/main")],
    ["a release-like name that is not release/*", pushLine("refs/heads/releases")],
    ["a deletion of a feature branch", pushLine("refs/heads/claude/gone", { sha: ZEROS })],
  ]) {
    const r = await pushGate(t, { stdin });
    assert.equal(r.code, 0, `${name}: ${r.err}`);
    assert.match(r.err, /rust gate DEFERRED/, name);
  }
});

test("deferral refused: a protected branch in the push keeps today's refusal, before any scan, export or git work", async (t) => {
  for (const [name, stdin] of [
    ["main", pushLine("refs/heads/main")], ["master", pushLine("refs/heads/master")], ["trunk", pushLine("refs/heads/trunk")],
    ["release/1.2", pushLine("refs/heads/release/1.2")], ["release/x/y", pushLine("refs/heads/release/x/y")], ["Main (case)", pushLine("refs/heads/Main")],
    ["a protected branch among others", pushLine("refs/heads/claude/a") + pushLine("refs/heads/main")],
    ["the deletion of main", pushLine("refs/heads/main", { sha: ZEROS })],
    ["a tag", pushLine("refs/tags/v1")], ["a ref outside refs/heads", pushLine("refs/for/claude/a")],
  ]) {
    const r = await pushGate(t, { stdin });
    assert.equal(r.code, 1, name);
    assert.match(r.err, NO_BROKER, name);
    assert.equal(r.seen.length, 0, `${name}: no scan ran`);
    assert.ok(!r.log.some((l) => l[0] === "tar" || (l[0] === "git" && (l[1] === "archive" || l[1] === "status"))), `${name}: nothing was exported or checked`);
    assert.doesNotMatch(r.err, /DEFERRED/, name);
  }
});

test("deferral refused: CI, the opt-out, no pushed refs and a plan keep the refusal", async (t) => {
  for (const [name, o] of [
    ["CI=1", { env: { CI: "1" } }], ["CI=true", { env: { CI: "true" } }], ["GITHUB_ACTIONS=true", { env: { GITHUB_ACTIONS: "true" } }],
    ["RIGHTKIT_GATE_NO_DEFER=1", { env: { [DEFER_OPT_OUT_ENV]: "1" } }],
    ["CI with the opt-out unset", { env: { CI: "1", [DEFER_OPT_OUT_ENV]: "0" } }],
    ["no stdin (a manual run)", { stdin: "" }], ["a --plan (reads no stdin)", { args: ["--plan"] }],
  ]) {
    const r = await pushGate(t, o);
    assert.equal(r.code, 1, name);
    assert.match(r.err, NO_BROKER, name);
    assert.equal(r.seen.length, 0, name);
  }
  assert.equal(DEFER_OPT_OUT_ENV, "RIGHTKIT_GATE_NO_DEFER");
  // CI=false and CI=0 are not CI; the gate defers there.
  for (const value of ["false", "0", ""]) assert.equal((await pushGate(t, { env: { CI: value } })).code, 0, `CI=${JSON.stringify(value)}`);
});

test("deferral: the repository's own default branch (.rightgit.json defaultBranch) is protected too", async (t) => {
  const files = gateFiles({ ".rightgit.json": JSON.stringify({ schemaVersion: 1, defaultBranch: "develop" }) });
  const dev = await pushGate(t, { files, stdin: pushLine("refs/heads/develop") });
  assert.equal(dev.code, 1);
  assert.match(dev.err, NO_BROKER);
  assert.equal((await pushGate(t, { files, stdin: pushLine("refs/heads/claude/port") })).code, 0);
  assert.equal((await pushGate(t, { files: gateFiles({ ".rightgit.json": "{not json" }), stdin: pushLine("refs/heads/claude/port") })).code, 0, "an unreadable manifest adds nothing and removes nothing");
});

test("deferral unchanged: a host with the broker or the managed shim runs the Rust gate whatever the branch, CI or the opt-out", async (t) => {
  for (const [name, o] of [
    ["rustright, claude/*", { spawnOptions: { broker: true } }],
    ["rustright, main", { spawnOptions: { broker: true }, stdin: pushLine("refs/heads/main") }],
    ["rustright, CI", { spawnOptions: { broker: true }, env: { CI: "1", GITHUB_ACTIONS: "true" } }],
    ["rustright, opt-out", { spawnOptions: { broker: true }, env: { [DEFER_OPT_OUT_ENV]: "1" } }],
    ["the managed shim, claude/*", { spawnOptions: { shim: true }, readFile: shimRead }],
  ]) {
    const r = await pushGate(t, o);
    assert.equal(r.code, 0, `${name}: ${r.err}`);
    const ids = r.seen[0].plan.scans.map((s) => s.id);
    for (const id of ["clippy:a", "test:rust:a", "cargo-metadata", "cargo-package-list:a"]) assert.ok(ids.includes(id), `${name}: ${id} runs`);
    assert.ok(r.log.some((l) => (l[0] === "rustright" && l[2] === "metadata") || (l[0] === "cargo" && l[1] === "metadata")), `${name}: cargo metadata ran through the broker`);
    assert.doesNotMatch(r.err, /DEFERRED/, name);
    assert.match(r.out, /pass \(1 changed file\(s\) vs origin\/main\); 0 scan\(s\) deferred, not passed/, name);
    assert.doesNotMatch(r.out, /no RustRight broker/, name);
  }
  // A broker without its compile grant still refuses on a claude/* branch: the host has the broker, so nothing defers.
  const { ".rightkit-local-build.json": _grant, ...noGrant } = gateFiles();
  const r = await pushGate(t, { files: noGrant, spawnOptions: { broker: true } });
  assert.equal(r.code, 1);
  assert.match(r.err, /BLOCKED: no local compile grant/);
  assert.equal(r.seen.length, 0);
});

test("deferral: a failing scan that needs no broker still blocks the push, with no deferral block, and a deferred non-broker scan still blocks", async (t) => {
  const failed = await pushGate(t, { rows: { license: { status: "fail", detail: "no license field" } } });
  assert.equal(failed.code, 1);
  assert.match(failed.err, /BLOCKED \(1\)\n  - license: no license field/);
  assert.doesNotMatch(failed.err, /rust gate DEFERRED/);
  const waiting = await pushGate(t, { rows: { "secret-scan": { status: "deferred", detail: "needs a trusted root" } } });
  assert.equal(waiting.code, 1);
  assert.match(waiting.err, /DEFERRED secret-scan: deferred, not passed \(needs a trusted root\)/);
});

test("deferral: without rustfmt on the host the rustfmt scan is named as not run; with it the scan runs and is not named", async (t) => {
  const without = await pushGate(t, { rustfmt: null });
  assert.equal(without.code, 0, without.err);
  assert.ok(!without.seen[0].plan.scans.some((s) => s.tool === "rustfmt"));
  assert.match(without.err, /scans not run [^;]*rustfmt:2021/);
  assert.match(without.out, /7 Rust scan\(s\) are deferred .*\(rustfmt is not usable on this host either\)/);
  const withIt = await pushGate(t);
  assert.ok(withIt.seen[0].plan.scans.some((s) => s.id === "rustfmt:2021" && s.tool === "rustfmt"));
  assert.doesNotMatch(withIt.err, /rustfmt:2021/);
});

test("deferral: the default rustfmt lookup needs the found rustfmt to answer --version; a rustup proxy without a toolchain counts as none", async (t) => {
  // $RUSTFMT wins in findRustfmt; the fake host has that file and answers (or refuses) its --version.
  const io = { exists: (p) => norm(p) === "/x/rustfmt" || fs.existsSync(p) };
  const answers = await pushGate(t, { rustfmt: false, env: { RUSTFMT: "/x/rustfmt" }, io });
  assert.equal(answers.code, 0, answers.err);
  assert.ok(answers.seen[0].plan.scans.some((s) => s.id === "rustfmt:2021"));
  assert.ok(answers.log.some((l) => l[0] === "/x/rustfmt" && l[1] === "--version"));
  assert.doesNotMatch(answers.err, /rustfmt:2021/);
  const refuses = await pushGate(t, { rustfmt: false, env: { RUSTFMT: "/x/rustfmt" }, io, spawnOptions: { rustfmtAnswers: false } });
  assert.equal(refuses.code, 0, refuses.err);
  assert.ok(!refuses.seen[0].plan.scans.some((s) => s.tool === "rustfmt"));
  assert.match(refuses.err, /scans not run [^;]*rustfmt:2021/);
});

test("deferral: a workspace below the root and the lock scope are read from the tracked manifests; the pre-push plan keeps its class A scans", async (t) => {
  const files = { ...gateFiles(), "hub/src-tauri/Cargo.toml": pkg("app", "\n[workspace]\n"), "hub/src-tauri/src/lib.rs": "pub fn f() {}\n", "hub/src-tauri/Cargo.lock": "version = 4\n" };
  const root = mkRepo(files, t);
  const log = []; const seen = []; const c = capture();
  const base = pushSpawn(root, { log, diff: "hub/src-tauri/src/lib.rs\n" });
  const spawn = (cmd, args, opts) => {
    if (cmd === "git" && args[0] === "ls-files" && args.at(-1).endsWith("Cargo.toml")) return ok("Cargo.toml\0crates/a/Cargo.toml\0crates/b/Cargo.toml\0hub/src-tauri/Cargo.toml\0");
    if (cmd === "git" && args[0] === "ls-files" && args.at(-1).endsWith("Cargo.lock")) return ok("Cargo.lock\0hub/src-tauri/Cargo.lock\0");
    return base(cmd, args, opts);
  };
  const code = await main(["--pre-push", "--base", "origin/main"], { ...c.io, ...noWrites, spawn, cwd: root, env: {}, readStdin: () => pushLine("refs/heads/claude/port"), runScans: fakeRun(seen), findRustfmt: () => "/x/rustfmt" });
  assert.equal(code, 0, c.err.join("\n"));
  assert.match(c.err.join("\n"), /rust units a, b, hub\/src-tauri:app; scans not run /);
  assert.ok(seen[0].plan.scans.every((s) => s.class === "A" || ["test:scripts-ci", "generated-docs"].includes(s.id)), "the pre-push selection still applies");
  assert.ok(seen[0].plan.scans.some((s) => s.id === "license"));
  assert.match(c.err.join("\n"), /cargo-package-list:hub\/src-tauri:app/);
  assert.ok(!log.some((l) => l[0] === "rustright" || l[0] === "cargo"));
});

test("isDeferrableRef and rustDeferral: protected names, case, non-branch refs, CI and the opt-out", () => {
  assert.deepEqual([...PROTECTED_BRANCHES], ["main", "master", "trunk"]);
  for (const ref of ["refs/heads/claude/x", "refs/heads/feature", "refs/heads/claude/main", "refs/heads/releases", "refs/heads/mainline"]) assert.equal(isDeferrableRef(ref), true, ref);
  for (const ref of ["refs/heads/main", "refs/heads/MASTER", "refs/heads/trunk", "refs/heads/release/1", "refs/heads/Release/2.0/rc", "refs/tags/v1", "refs/for/main", "HEAD", "", undefined]) assert.equal(isDeferrableRef(ref), false, String(ref));
  assert.equal(isDeferrableRef("refs/heads/develop", ["develop"]), false);
  assert.equal(isDeferrableRef("refs/heads/develop", ["Develop"]), false);
  const refs = ["refs/heads/claude/x"];
  assert.equal(rustDeferral({ refs, env: {} }).defer, true);
  assert.equal(rustDeferral({ refs: [], env: {} }).defer, false);
  assert.equal(rustDeferral({ refs, env: { CI: "1" } }).defer, false);
  assert.equal(rustDeferral({ refs, env: { GITHUB_ACTIONS: "true" } }).defer, false);
  assert.equal(rustDeferral({ refs, env: { [DEFER_OPT_OUT_ENV]: "1" } }).defer, false);
  assert.match(rustDeferral({ refs, env: { [DEFER_OPT_OUT_ENV]: "1" } }).why, /RIGHTKIT_GATE_NO_DEFER/);
  assert.equal(rustDeferral({ refs: [...refs, "refs/heads/main"], env: {} }).defer, false);
  assert.match(rustDeferral({ refs: [...refs, "refs/heads/main"], env: {} }).why, /refs\/heads\/main/);
  assert.equal(rustDeferral({ refs, env: { CI: "false", GITHUB_ACTIONS: "0", [DEFER_OPT_OUT_ENV]: "false" } }).defer, true);
});

test("deferredBlock and withoutScans: the lines name units, scans and the pushed refs; the plan keeps its other scans and digests them", () => {
  const lines = deferredBlock({ units: ["a", "hub:app"], notRun: ["clippy:a", "test:rust:a"], refs: ["refs/heads/claude/x", "refs/heads/claude/y"] });
  assert.equal(lines.length, 2);
  assert.equal(lines[0], "rightkit-gate: rust gate DEFERRED to host qualification (no RustRight broker on this host): rust units a, hub:app; scans not run clippy:a, test:rust:a; the pull request needs a HOST RECEIPT before merge");
  assert.match(lines[1], /pushed refs\/heads\/claude\/x, refs\/heads\/claude\/y .*HOST RECEIPT.*merge gate/);
  assert.match(deferredBlock({ units: [], notRun: [], refs: ["refs/heads/x"] })[0], /rust units none found; scans not run none \(no Rust scan was due for this diff\); the pull request needs a HOST RECEIPT before merge/);
  const scans = [{ id: "license", class: "A" }, { id: "cargo-deny", class: "A" }, { id: "clippy:a", class: "B" }, { id: "test:scripts-ci", class: "B" }];
  const plan = { scans, deferred: ["clippy:a", "generated-docs"], notes: ["n"], inputDigest: "d" };
  const next = withoutScans(plan, scans.filter((s) => s.id === "cargo-deny" || s.id === "clippy:a"), "dropped");
  assert.deepEqual(next.scans.map((s) => s.id), ["license", "test:scripts-ci"]);
  assert.deepEqual(next.deferred, ["generated-docs"]);
  assert.deepEqual(next.notes, ["n", "dropped"]);
  assert.equal(next.scanSetDigest, digestScanSet(next.scans));
  assert.equal(next.classDigests.A, digestScanSet([scans[0]]));
  assert.equal(next.classDigests.B, digestScanSet([scans[3]]));
  assert.equal(next.inputDigest, "d");
  assert.equal(plan.scans.length, 4, "the original plan is untouched");
  assert.deepEqual(withoutScans(plan, [], "").notes, ["n"]);
});

test("manifestWorkspaceMetadata: members by glob and exclude, the root package, publish = false; a nested manifest under a workspace is refused as cargo would", (t) => {
  const root = mkRepo({
    "Cargo.toml": '[package]\nname = "rootpkg"\nversion = "1.0.0"\n\n[workspace]\nmembers = [\n  "crates/*", # all of them\n  "tools/b",\n]\nexclude = ["crates/old"]\n',
    "crates/a/Cargo.toml": pkg("a"), "crates/old/Cargo.toml": pkg("old"), "tools/b/Cargo.toml": pkg("b", "publish = false\n"), "tools/c/Cargo.toml": pkg("c"),
    "crates/a/vendored/Cargo.toml": pkg("deep"),
    "hub/src-tauri/Cargo.toml": pkg("app"), "virtual/Cargo.toml": "[profile.release]\nlto = true\n",
  }, t);
  const all = ["Cargo.toml", "crates/a/Cargo.toml", "crates/a/vendored/Cargo.toml", "crates/old/Cargo.toml", "tools/b/Cargo.toml", "tools/c/Cargo.toml", "hub/src-tauri/Cargo.toml", "virtual/Cargo.toml"];
  const tracked = (n) => (n === "Cargo.toml" ? all : []);
  const readFile = (f) => fs.readFileSync(f, "utf8");
  const found = manifestWorkspaceMetadata({ root, dir: ".", tracked, readFile });
  assert.equal(found.cargoDir, ".");
  assert.deepEqual(found.members, [
    { name: "a", dir: "crates/a", manifest: "crates/a/Cargo.toml", publish: true },
    { name: "b", dir: "tools/b", manifest: "tools/b/Cargo.toml", publish: false },
    { name: "rootpkg", dir: ".", manifest: "Cargo.toml", publish: true },
  ], "crates/old is excluded, crates/a/vendored is not a one-segment match, tools/c is not listed");
  assert.throws(() => manifestWorkspaceMetadata({ root, dir: "tools/c", tracked, readFile }), /believes it's in a workspace when it's not/);
  assert.throws(() => manifestWorkspaceMetadata({ root, dir: "virtual", tracked, readFile }), /believes it's in a workspace/);
  const alone = mkRepo({ "src-tauri/Cargo.toml": pkg("app"), "empty/Cargo.toml": "[profile.dev]\n" }, t);
  assert.deepEqual(manifestWorkspaceMetadata({ root: alone, dir: "src-tauri", tracked: () => ["src-tauri/Cargo.toml", "empty/Cargo.toml"], readFile }), { cargoDir: "src-tauri", members: [{ name: "app", dir: "src-tauri", manifest: "src-tauri/Cargo.toml", publish: true }], unplaced: [] });
  assert.throws(() => manifestWorkspaceMetadata({ root: alone, dir: "empty", tracked: () => ["src-tauri/Cargo.toml", "empty/Cargo.toml"], readFile }), /neither a \[package\] with a name nor a \[workspace\]/);
});

test("discoverRustWorkspaces with a metadata reader never asks cargo, and skips what cargo would refuse the way it does with the broker", (t) => {
  const files = {
    "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\nexclude = ["crates/old"]\n', "Cargo.lock": "version = 4\n",
    "crates/a/Cargo.toml": pkg("a"), "crates/old/Cargo.toml": pkg("old"),
    "hub/src-tauri/Cargo.toml": pkg("app", "\n[workspace]\n"), "hub/src-tauri/Cargo.lock": "version = 4\n",
    "apps/x/Cargo.toml": pkg("x"), "apps/x/Cargo.lock": "version = 4\n", "fixtures/f/Cargo.toml": pkg("f"),
  };
  const root = mkRepo(files, t);
  const names = Object.keys(files).filter((f) => f.endsWith("Cargo.toml") || f.endsWith("Cargo.lock"));
  const tracked = (n) => names.filter((f) => f === n || f.endsWith(`/${n}`));
  const readFile = (f) => fs.readFileSync(f, "utf8");
  const found = discoverRustWorkspaces({
    root, spawn: () => assert.fail("cargo was asked"), cargoCommand: ["rustright", "cargo"], tracked, readFile, fixtures: ["fixtures"],
    metadata: (dir) => manifestWorkspaceMetadata({ root, dir, tracked, readFile }),
  });
  assert.deepEqual(found.workspaces.map((w) => w.cargoDir), [".", "hub/src-tauri"]);
  assert.deepEqual(found.units.map((u) => u.name), ["a", "hub/src-tauri:app"]);
  const why = (m) => found.skipped.find((k) => k.manifest === m)?.why ?? "";
  assert.match(why("crates/old/Cargo.toml"), /excluded from the Cargo workspace \./);
  assert.match(why("fixtures/f/Cargo.toml"), /fixture directory fixtures/);
  assert.match(why("apps/x/Cargo.toml"), /below the Cargo workspace \. but not one of its members, and cargo refuses it on its own \(current package believes it's in a workspace when it's not/);
});

// ---- review fixes: git's real stdin shape, every pushed ref, the default branch from the remote's side, the reason, the reader ------------

// Git forbids an ASCII space in a ref name and allows every other non-control character, Unicode whitespace included.
const UNICODE_SPACES = [["U+00A0 no-break space", " "], ["U+2003 em space", " "], ["U+3000 ideographic space", "　"], ["U+2028 line separator", " "], ["U+FEFF zero-width no-break space", "﻿"]];
const gitLine = (localRef, remoteRef, { sha = HEAD, remote = ZEROS } = {}) => `${localRef} ${sha} ${remoteRef} ${remote}\n`;
const benignLine = (name = "benign") => pushLine(`refs/heads/claude/${name}`);

test("parsePushLines: git's four fields split on single ASCII spaces only, so a Unicode space in a ref name stays inside its field; nothing is dropped", () => {
  for (const [name, space] of UNICODE_SPACES) {
    const local = `refs/heads/a${space}b`; const remote = `refs/heads/ma${space}in`;
    const { lines, malformed } = parsePushLines(gitLine(local, remote, { remote: "b".repeat(40) }));
    assert.deepEqual(malformed, [], name);
    assert.deepEqual(lines, [{ localRef: local, localSha: HEAD, remoteRef: remote, remoteSha: "b".repeat(40) }], name);
  }
  const mixed = parsePushLines(`${gitLine("a b", "refs/heads/main")}\r\n${benignLine()}\n\n`);
  assert.deepEqual(mixed.lines.map((l) => l.remoteRef), ["refs/heads/main", "refs/heads/claude/benign"], "CRLF endings and blank lines are tolerated");
  assert.deepEqual(mixed.malformed, []);
  // Anything that is not four non-empty ASCII-space separated fields is reported, with its line number, never dropped.
  const bad = parsePushLines(`${benignLine("ok")}three fields here\nfour  fields with two spaces ${ZEROS}\n${HEAD} ${HEAD} a b c d\n \n${"\t"}tab\tseparated\tfields\there\n`);
  assert.equal(bad.lines.length, 1);
  assert.deepEqual(bad.malformed.map((m) => m.number), [2, 3, 4, 5, 6]);
  assert.deepEqual(parsePushLines("").lines, []);
  assert.deepEqual(parsePushLines(undefined), { lines: [], malformed: [] });
});

test("deferral refused (HIGH): a local ref name with Unicode whitespace cannot hide a protected ref, a tag or a release branch behind a benign one", async (t) => {
  for (const [spaceName, space] of UNICODE_SPACES) {
    const local = `refs/heads/a${space}b`;
    for (const [name, remoteRef] of [["main", "refs/heads/main"], ["a tag", "refs/tags/v1"], ["release/*", "refs/heads/release/1.0"], ["a ref outside refs/heads", "refs/for/claude/a"]]) {
      for (const [order, stdin] of [["alone", gitLine(local, remoteRef)], ["before a benign ref", gitLine(local, remoteRef) + benignLine()], ["after a benign ref", benignLine() + gitLine(local, remoteRef)]]) {
        const r = await pushGate(t, { stdin });
        assert.equal(r.code, 1, `${spaceName}, ${name}, ${order}`);
        assert.match(r.err, NO_BROKER, `${spaceName}, ${name}, ${order}`);
        assert.equal(r.seen.length, 0, `${spaceName}, ${name}, ${order}: no scan ran`);
        assert.doesNotMatch(r.err, /DEFERRED/, `${spaceName}, ${name}, ${order}`);
      }
    }
  }
  // The dropped line also skipped the check that the pushed commit is the checked-out HEAD.
  const elsewhere = await pushGate(t, { stdin: gitLine("refs/heads/a b", "refs/heads/claude/x", { sha: "c".repeat(40) }) });
  assert.equal(elsewhere.code, 1);
  assert.match(elsewhere.err, /BLOCKED: pushing cccccccccc to refs\/heads\/claude\/x, but HEAD is aaaaaaaaaa/);
  assert.equal(elsewhere.seen.length, 0);
  // A Unicode space in the remote ref is part of its name: that ref is no protected one, and the push defers like any branch.
  const ok1 = await pushGate(t, { stdin: gitLine("HEAD", "refs/heads/claude/a b") });
  assert.equal(ok1.code, 0, ok1.err);
  assert.match(ok1.err, /pushed refs\/heads\/claude\/a b \(non-protected, not CI\)/, "the block names the ref as git sent it");
  // And a local ref with a Unicode space going to a benign branch still defers: the line is read, not refused.
  const ok2 = await pushGate(t, { stdin: gitLine("refs/heads/a b", "refs/heads/claude/benign") });
  assert.equal(ok2.code, 0, ok2.err);
  assert.match(ok2.err, /pushed refs\/heads\/claude\/benign \(non-protected, not CI\)/);
});

test("a stdin line that does not read as four fields refuses the push on every host and in every repository, and the refusal says why", async (t) => {
  // git hands the local field over verbatim, so a rev expression with a space (`HEAD@{1 minute ago}`, `HEAD^{/one thing}`) is not four
  // fields. The line used to be dropped outside the deferral path, which also skipped the pushed-commit-is-HEAD check.
  const bad = [
    ["three fields", `refs/heads/main ${HEAD} refs/heads/main\n${benignLine()}`, 1],
    ["five fields", `${benignLine()}refs/heads/main ${HEAD} x refs/heads/main ${ZEROS}\n`, 2],
    ["a doubled space", `refs/heads/x ${HEAD}  refs/heads/main ${ZEROS}\n${benignLine()}`, 1],
    ["tabs", `refs/heads/x\t${HEAD}\trefs/heads/main\t${ZEROS}\n${benignLine()}`, 1],
    ["a rev expression with a space, another commit", gitLine("HEAD@{1 minute ago}", "refs/heads/claude/x", { sha: "c".repeat(40) }), 1],
    ["a rev expression with a space after a benign line", benignLine() + gitLine("HEAD^{/one thing}", "refs/heads/claude/x"), 2],
  ];
  const hosts = [
    ["no broker, a Rust repository", {}],
    ["a broker host", { spawnOptions: { broker: true } }],
    ["a repository with no Cargo.toml", { spawnOptions: { manifests: [] } }],
    ["a broker host and no Cargo.toml", { spawnOptions: { broker: true, manifests: [] } }],
  ];
  for (const [name, stdin, number] of bad) {
    for (const [host, o] of hosts) {
      const label = `${name}, ${host}`;
      const r = await pushGate(t, { stdin, ...o });
      assert.equal(r.code, 1, label);
      assert.match(r.err, new RegExp(`BLOCKED: pre-push stdin line ${number} is not "<local ref> <local sha> <remote ref> <remote sha>" \\(four fields separated by single spaces\\), so the pushed refs cannot all be read`), label);
      assert.equal(r.seen.length, 0, `${label}: no scan ran`);
      assert.doesNotMatch(r.err, /DEFERRED/, label);
      assert.deepEqual(r.after, r.before, `${label}: nothing was written`);
    }
  }
  // The same four fields are read, not refused, once they are four fields, wherever the host stands.
  for (const [host, o] of hosts.slice(1)) assert.equal((await pushGate(t, { stdin: benignLine(), ...o })).code, 0, host);
  // rustDeferral keeps its own lock for a caller that hands it unparsed lines.
  assert.equal(rustDeferral({ refs: ["refs/heads/claude/x"], env: {}, unparsed: [{ number: 3, line: "x" }] }).defer, false);
  assert.match(rustDeferral({ refs: ["refs/heads/claude/x"], env: {}, unparsed: [{ number: 3, line: "x" }] }).why, /line 3/);
  assert.equal(rustDeferral({ refs: ["refs/heads/claude/x"], env: {}, unparsed: [] }).defer, true);
});

test("deferral refused: every pushed ref is judged, whichever place a protected one has in the push", async (t) => {
  const protectedRefs = [["main", "refs/heads/main"], ["a tag", "refs/tags/v1"], ["release/2", "refs/heads/release/2"]];
  for (const [name, ref] of protectedRefs) {
    for (const [place, stdin] of [
      ["first of two", pushLine(ref) + benignLine("a")],
      ["last of two", benignLine("a") + pushLine(ref)],
      ["middle of three", benignLine("a") + pushLine(ref) + benignLine("b")],
      ["first of three", pushLine(ref) + benignLine("a") + benignLine("b")],
      ["last of three", benignLine("a") + benignLine("b") + pushLine(ref)],
    ]) {
      const r = await pushGate(t, { stdin });
      assert.equal(r.code, 1, `${name}, ${place}`);
      assert.match(r.err, NO_BROKER, `${name}, ${place}`);
      assert.match(r.err, new RegExp(`not deferred to host qualification: ${ref.replaceAll("/", "\\/")} is not a plain non-protected branch`), `${name}, ${place}`);
      assert.equal(r.seen.length, 0, `${name}, ${place}`);
    }
  }
});

test("deferral does not need the local compile grant a cloud host never has, and names the same scans as not run with or without it", async (t) => {
  const { ".rightkit-local-build.json": _grant, ...noGrant } = gateFiles();
  const without = await pushGate(t, { files: noGrant });
  assert.equal(without.code, 0, without.err);
  assert.doesNotMatch(without.out + without.err, /no local compile grant/);
  assert.ok(!without.seen[0].plan.notes.some((n) => /grant/.test(n)), `no grant note in the plan: ${without.seen[0].plan.notes}`);
  assert.deepEqual(without.seen[0].plan.deferred, [], "nothing is left deferred for want of the grant");
  const withIt = await pushGate(t);
  const notRun = (r) => /scans not run ([^;]+);/.exec(r.err)?.[1];
  assert.ok(notRun(withIt)?.includes("clippy:a"));
  assert.equal(notRun(without), notRun(withIt), "the grant changes nothing about what is named as not run");
  assert.deepEqual(without.seen[0].plan.scans.map((s) => s.id), withIt.seen[0].plan.scans.map((s) => s.id), "or about what runs");
});

test("deferral: a refusal prints the old message first and the reason on a second line", async (t) => {
  for (const [name, o, reason] of [
    ["main", { stdin: pushLine("refs/heads/main") }, /refs\/heads\/main is not a plain non-protected branch \(main, master, trunk, release\/\*, the repository's default branch and tags are never deferred\)$/],
    ["a tag", { stdin: pushLine("refs/tags/v1") }, /refs\/tags\/v1 is not a plain non-protected branch/],
    ["--tags with a branch", { stdin: benignLine() + pushLine("refs/tags/v2") }, /refs\/tags\/v2 is not a plain non-protected branch/],
    ["release/1", { stdin: pushLine("refs/heads/release/1") }, /refs\/heads\/release\/1 is not a plain non-protected branch/],
    ["CI", { env: { CI: "1" } }, /running in CI$/],
    ["GITHUB_ACTIONS", { env: { GITHUB_ACTIONS: "true" } }, /running in CI$/],
    ["the opt-out", { env: { [DEFER_OPT_OUT_ENV]: "1" } }, /RIGHTKIT_GATE_NO_DEFER is set$/],
    ["no stdin", { stdin: "" }, /no pushed refs on stdin: not a git pre-push run$/],
    ["--plan", { args: ["--plan"] }, /no pushed refs on stdin: not a git pre-push run$/],
  ]) {
    const r = await pushGate(t, o);
    assert.equal(r.code, 1, name);
    const lines = r.err.split("\n");
    assert.equal(lines.length, 2, `${name}: ${r.err}`);
    assert.equal(lines[0], "rightkit-gate: BLOCKED: neither `rustright` nor the managed cargo shim (cargo-guard.mjs) is first on PATH; plain cargo is never used", `${name}: the old message, verbatim`);
    assert.match(lines[1], /^rightkit-gate: {3}not deferred to host qualification: /, name);
    assert.match(lines[1], reason, name);
  }
});

test("deferral: the default branch is read from the base, the remote commit being replaced and origin/HEAD, so a commit cannot unprotect it by editing .rightgit.json", async (t) => {
  const manifest = (defaultBranch) => JSON.stringify({ schemaVersion: 1, defaultBranch });
  const edited = gateFiles({ ".rightgit.json": manifest("other") });
  const REMOTE = "d".repeat(40);
  const refused = (r, why) => { assert.equal(r.code, 1, r.err); assert.match(r.err, NO_BROKER); assert.match(r.err, why); assert.equal(r.seen.length, 0); };
  // 1. The pushed commit renames the default branch away; the base ref (origin/main) still names develop.
  refused(await pushGate(t, { files: edited, stdin: pushLine("refs/heads/develop"), spawnOptions: { shows: { "origin/main:.rightgit.json": manifest("develop") } } }), /refs\/heads\/develop is not a plain non-protected branch/);
  // 2. The remote commit being replaced (an existing develop) names it, with no base available.
  refused(await pushGate(t, { files: edited, stdin: pushLine("refs/heads/develop", { remote: REMOTE }), spawnOptions: { shows: { [`${REMOTE}:.rightgit.json`]: manifest("develop") } } }), /refs\/heads\/develop is not a plain non-protected branch/);
  // 3. origin/HEAD's manifest names it: a new branch push has neither a base nor a remote commit.
  refused(await pushGate(t, { files: edited, stdin: pushLine("refs/heads/develop"), spawnOptions: { shows: { "origin/HEAD:.rightgit.json": manifest("develop") } } }), /refs\/heads\/develop is not a plain non-protected branch/);
  // 4. origin/HEAD itself names the default branch, and no .rightgit.json exists anywhere.
  refused(await pushGate(t, { stdin: pushLine("refs/heads/develop"), spawnOptions: { originHead: "refs/remotes/origin/develop" } }), /refs\/heads\/develop is not a plain non-protected branch/);
  // 5. The checkout's own value still counts (the earlier behaviour), alongside the others, and the case does not matter.
  refused(await pushGate(t, { files: gateFiles({ ".rightgit.json": manifest("Develop") }), stdin: pushLine("refs/heads/develop") }), /refs\/heads\/develop is not a plain non-protected branch/);
  // Nothing else changes: a claude/* branch defers under every one of those setups, and an unreadable remote manifest adds nothing.
  for (const spawnOptions of [
    { shows: { "origin/main:.rightgit.json": manifest("develop") } }, { originHead: "refs/remotes/origin/develop" },
    { shows: { "origin/main:.rightgit.json": "{not json", "origin/HEAD:.rightgit.json": manifest(42) } },
  ]) {
    const r = await pushGate(t, { files: edited, stdin: benignLine(), spawnOptions });
    assert.equal(r.code, 0, r.err);
    assert.match(r.err, /rust gate DEFERRED/);
  }
});

test("defaultBranchNames: the union of the checkout, origin/HEAD, each revision and the branch origin/HEAD names; a revision git cannot show adds nothing", () => {
  const manifest = (defaultBranch) => JSON.stringify({ defaultBranch });
  const shown = { "origin/HEAD:.rightgit.json": manifest("trunk2"), "base:.rightgit.json": `﻿${manifest("develop")}`, "r1:.rightgit.json": manifest("stable") };
  const asked = [];
  const git = (...a) => {
    asked.push(a.join(" "));
    if (a[0] === "show") return shown[a[1]] === undefined ? no("fatal") : ok(shown[a[1]]);
    if (a[0] === "symbolic-ref") return ok("refs/remotes/origin/main-line\n");
    return no();
  };
  const names = defaultBranchNames({ git, readCheckout: () => manifest("mine"), revisions: ["base", "r1", "gone", "base"] });
  assert.deepEqual([...names].sort(), ["develop", "main-line", "mine", "stable", "trunk2"]);
  assert.equal(asked.filter((a) => a === "show base:.rightgit.json").length, 1, "a revision is shown once");
  defaultBranchNames({ git, readCheckout: () => "{}", revisions: ["--output=/x", "-p", "", undefined] });
  assert.ok(!asked.some((a) => /^show -/.test(a)), "a revision that git would read as an option is never passed to it");
  assert.deepEqual(defaultBranchNames({ git: () => no(), readCheckout: () => { throw new Error("ENOENT"); } }), []);
  assert.deepEqual(defaultBranchNames({ git: () => no(), readCheckout: () => "[]" }), []);
  assert.deepEqual(defaultBranchNames({ git: (...a) => (a[0] === "symbolic-ref" ? ok("refs/heads/odd\n") : no()), readCheckout: () => "{}" }), [], "only a refs/remotes/origin/ target names a branch");
});

// A real git repository with the installed hook, a bare remote and a PATH of symlinks to just what the hook and the gate need, so a
// rustright or cargo on the developer's PATH cannot change the outcome. The app check is a shim that passes. `origin/main` is the parent
// commit, so the gate has a base to diff against; `files` are committed with `message`. Returns null (after t.skip) when a tool is missing.
function realHookRepo(t, { files, message = "app" }) {
  const tools = mkRepo({}, t);
  const exec = spawnSync("git", ["--exec-path"], { encoding: "utf8" });
  // On Windows sh answers MSYS paths and an MSYS tool needs its runtime DLL beside it, so PATH is the Git for Windows directories that
  // hold the tools (cygpath -w), which carry no rustright or cargo; elsewhere it is a directory of symlinks to just these tools.
  const toolDirs = new Set();
  for (const name of ["git", "which", "awk", "mktemp", "cat", "rm", "tar"]) {
    const found = spawnSync("sh", ["-c", `command -v ${name}`], { encoding: "utf8" }).stdout.trim();
    if (!found) { t.skip(`no ${name} on this host`); return null; }
    if (process.platform !== "win32") { fs.symlinkSync(found, path.join(tools, name)); continue; }
    const dir = spawnSync("sh", ["-c", 'cygpath -w "$(dirname "$1")"', "sh", found], { encoding: "utf8" }).stdout.trim();
    if (!dir) { t.skip(`cannot map ${found} to a Windows path`); return null; }
    toolDirs.add(dir);
  }
  const toolPath = process.platform === "win32" ? [...toolDirs].join(path.delimiter) : tools;
  const tmp = mkRepo({}, t);
  const gateCli = path.join(path.dirname(fileURLToPath(import.meta.url)), "bin", "rightkit-gate.mjs").replaceAll("\\", "/");
  const shim = `#!/bin/sh\nif [ "$1" = check ]; then exit 0; fi\nexec "${process.execPath.replaceAll("\\", "/")}" "${gateCli}" "$@"\n`;
  const root = hookRepo(t, {
    ".githooks/pre-push": hookText(gateCli),
    "node_modules/.bin/rightkit-gate": shim, ".gitignore": "node_modules/\n", "README.md": "# app\n",
  });
  // Windows spells it Path; a second PATH key beside it would leave which one wins to the platform.
  const inherited = Object.fromEntries(Object.entries(process.env).filter(([key]) => key.toUpperCase() !== "PATH"));
  const env = { ...inherited, PATH: toolPath, GIT_EXEC_PATH: exec.stdout.trim(), CI: "", GITHUB_ACTIONS: "", [DEFER_OPT_OUT_ENV]: "", GIT_TERMINAL_PROMPT: "0" };
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8", env });
  for (const [k, v] of [["user.email", "t@example.test"], ["user.name", "t"], ["core.autocrlf", "false"], ["core.hooksPath", ".githooks"]]) git("config", k, v);
  const remote = path.join(tmp, "remote.git");
  assert.equal(spawnSync("git", ["init", "-q", "--bare", remote], { encoding: "utf8", env }).status, 0);
  git("remote", "add", "origin", remote);
  git("add", "-A");
  assert.equal(git("commit", "-q", "-m", "base").status, 0);
  git("update-ref", "refs/remotes/origin/main", "HEAD");
  for (const [name, body] of Object.entries(files)) { const file = path.join(root, name); fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); }
  git("add", "-A");
  assert.equal(git("commit", "-q", "-m", message).status, 0);
  return { root, git, remoteRefs: () => git("ls-remote", "origin").stdout.trim() };
}

test("the installed pre-push hook with real git: a local ref name with Unicode whitespace cannot push main, a tag or release/* past the deferral", { skip: !shAvailable && "no sh" }, async (t) => {
  const repo = realHookRepo(t, { files: { "Cargo.toml": pkg("app"), "src/lib.rs": "pub fn f() {}\n" } });
  if (!repo) return;
  const { git, remoteRefs } = repo;
  const odd = "a\u00a0b";
  assert.equal(git("branch", odd, "HEAD").status, 0, "git accepts a branch name holding U+00A0");
  for (const [name, refspec] of [["main", "refs/heads/main"], ["a tag", "refs/tags/v1"], ["release/9", "refs/heads/release/9"]]) {
    const pushed = git("push", "origin", `${odd}:${refspec}`, "HEAD:refs/heads/claude/benign");
    assert.notEqual(pushed.status, 0, `${name}: the push is refused`);
    assert.match(pushed.stderr, NO_BROKER, name);
    assert.match(pushed.stderr, new RegExp(`not deferred to host qualification: ${refspec.replaceAll("/", "\\/")} is not a plain non-protected branch`), name);
    assert.doesNotMatch(pushed.stderr, /DEFERRED/, name);
    assert.equal(remoteRefs(), "", `${name}: nothing reached the remote`);
  }
  // The same split hid it when the odd ref was the only line, too: still refused.
  assert.notEqual(git("push", "origin", `${odd}:refs/heads/main`).status, 0);
  assert.equal(remoteRefs(), "");
});

test("the installed pre-push hook with real git: a standalone package (no [workspace]) pushed to a claude/* branch defers", { skip: !shAvailable && "no sh" }, async (t) => {
  for (const [name, files, units] of [
    ["a single crate", { "Cargo.toml": pkg("app"), "Cargo.lock": "version = 4\n", "src/lib.rs": "pub fn f() {}\n" }, "app"],
    ["a Tauri src-tauri", { "src-tauri/Cargo.toml": pkg("tauri-app"), "src-tauri/Cargo.lock": "version = 4\n", "src-tauri/src/lib.rs": "pub fn f() {}\n" }, "src-tauri:tauri-app"],
  ]) {
    const repo = realHookRepo(t, { files });
    if (!repo) return;
    const pushed = repo.git("push", "origin", "HEAD:refs/heads/claude/ok");
    assert.equal(pushed.status, 0, `${name}: ${pushed.stderr}`);
    assert.doesNotMatch(pushed.stderr, /cannot discover the repository's units|not iterable/, name);
    assert.match(pushed.stderr, new RegExp(`rust gate DEFERRED to host qualification \\(no RustRight broker on this host\\): rust units ${units};`), name);
    assert.match(repo.remoteRefs(), /refs\/heads\/claude\/ok$/m, `${name}: the branch reached the remote`);
    const main = repo.git("push", "origin", "HEAD:refs/heads/main");
    assert.notEqual(main.status, 0, `${name}: main is still refused`);
    assert.match(main.stderr, NO_BROKER, name);
    assert.doesNotMatch(repo.remoteRefs(), /refs\/heads\/main$/m, name);
  }
});

test("the installed pre-push hook with real git: a rev expression holding a space is refused whether or not the repository has Rust", { skip: !shAvailable && "no sh" }, async (t) => {
  // git sends the local field as typed: `HEAD^{/one thing}` and `HEAD@{1 minute ago}` make a line of more than four fields, which was
  // dropped (and its pushed-commit-is-HEAD check with it) everywhere but on the deferral path.
  for (const [name, files] of [["a Rust repository", { "Cargo.toml": pkg("app"), "Cargo.lock": "version = 4\n", "src/lib.rs": "pub fn f() {}\n" }], ["a repository with no Cargo.toml", { "docs/a.md": "# a\n" }]]) {
    const repo = realHookRepo(t, { files, message: "one thing" });
    if (!repo) return;
    const plain = repo.git("push", "origin", "HEAD:refs/heads/claude/plain");
    assert.equal(plain.status, 0, `${name}: ${plain.stderr}`);
    for (const rev of ["HEAD^{/one thing}", "HEAD@{1 minute ago}"]) {
      const pushed = repo.git("push", "origin", `${rev}:refs/heads/claude/spaced`);
      assert.notEqual(pushed.status, 0, `${name}, ${rev}: the push is refused`);
      assert.match(pushed.stderr, /BLOCKED: pre-push stdin line 1 is not "<local ref> <local sha> <remote ref> <remote sha>" \(four fields separated by single spaces\)/, `${name}, ${rev}`);
      assert.doesNotMatch(repo.remoteRefs(), /claude\/spaced/, `${name}, ${rev}: nothing reached the remote`);
    }
  }
});

test("manifestWorkspaceMetadata reads what cargo accepts: single-quoted strings, a ] in a comment, dotted keys, path dependencies that cargo makes members", (t) => {
  const readFile = (f) => fs.readFileSync(f, "utf8");
  const members = (files, dir = ".") => {
    const root = mkRepo(files, t);
    const all = Object.keys(files).filter((f) => f.endsWith("Cargo.toml"));
    return manifestWorkspaceMetadata({ root, dir, tracked: (n) => (n === "Cargo.toml" ? all : []), readFile }).members;
  };
  const names = (m) => m.map((x) => x.name);
  assert.deepEqual(names(members({ "Cargo.toml": "[workspace]\nmembers = ['crates/*']\n", "crates/a/Cargo.toml": "[package]\nname = 'a'\nversion = '1.0.0'\n" })), ["a"], "single-quoted members and name");
  assert.deepEqual(names(members({ "Cargo.toml": '[workspace]\nmembers = [\n  "crates/a", # the a crate ]\n  "crates/b",\n]\n', "crates/a/Cargo.toml": pkg("a"), "crates/b/Cargo.toml": pkg("b") })), ["a", "b"], "a ] in a comment does not end the array");
  assert.deepEqual(names(members({ "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\nexclude = [\n  \'crates/old\', # ] vendored\n  "crates/older",\n]\n', "crates/a/Cargo.toml": pkg("a"), "crates/old/Cargo.toml": pkg("old"), "crates/older/Cargo.toml": pkg("older") })), ["a"], "exclude reads the same way");
  assert.deepEqual(names(members({ "Cargo.toml": 'package.name = "rootpkg"\npackage.version = "1.0.0"\n' })), ["rootpkg"], "dotted package keys in a standalone manifest");
  assert.deepEqual(names(members({ "Cargo.toml": 'workspace.members = [\n  "crates/*", # ]\n]\nworkspace.exclude = ["crates/old"]\n', "crates/a/Cargo.toml": 'package.name = "a"\n', "crates/old/Cargo.toml": pkg("old") })), ["a"], "dotted workspace keys, multi-line, with a dotted package name in the member");
  assert.deepEqual(names(members({ "Cargo.toml": '[workspace.package]\nversion = "1"\n[workspace.dependencies]\nserde = "1"\n', "crates/a/Cargo.toml": pkg("a") })), [], "a workspace table without members lists no member and does not throw");
  assert.equal(members({ "Cargo.toml": "[workspace]\nmembers = [\"a\", \"b\", \"c\", \"d\"]\n", "a/Cargo.toml": pkg("a", "publish = false # not on crates.io\n"), "b/Cargo.toml": pkg("b", "publish = []\n"), "c/Cargo.toml": pkg("c", "publish = ['internal']\n"), "d/Cargo.toml": pkg("d") })
    .map((m) => `${m.name}:${m.publish}`).join(" "), "a:false b:false c:true d:true", "publish = false or [] is private; a registry list or nothing is publishable");
  // A path dependency below the workspace root is a member cargo adds on its own, transitively; exclude and the outside of the workspace are not.
  const deps = members({
    "Cargo.toml": '[package]\nname = "rootpkg"\n[workspace]\nexclude = ["vendor/skip"]\n[dependencies]\ndep = { path = "crates/dep", version = "1" }\nskip = { path = "vendor/skip" }\n[build-dependencies.tool]\npath = \'tools/tool\'\n[target.\'cfg(unix)\'.dependencies]\nunixy = { path = "crates/unixy" }\n',
    "crates/dep/Cargo.toml": '[package]\nname = "dep"\n[dev-dependencies]\ninner.path = "../inner"\n',
    "crates/inner/Cargo.toml": pkg("inner"), "crates/unixy/Cargo.toml": pkg("unixy"), "tools/tool/Cargo.toml": pkg("tool"),
    "vendor/skip/Cargo.toml": pkg("skip"), "crates/unrelated/Cargo.toml": pkg("unrelated"),
  });
  assert.deepEqual(names(deps), ["dep", "inner", "rootpkg", "tool", "unixy"], "dep, inner (through dep), tool and unixy are members; skip is excluded; unrelated is reached by nobody");
  assert.deepEqual(names(members({ "Cargo.toml": '[workspace]\n[workspace.dependencies]\nlib = { path = "libs/lib" }\nweb = "1"\n', "libs/lib/Cargo.toml": pkg("lib") })), ["lib"], "a virtual workspace's [workspace.dependencies] path");
  assert.deepEqual(names(members({ "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\n[dependencies]\nelsewhere = { path = "../../outside" }\nme = { path = "." }\n' })), ["a"], "a path outside the workspace, or the package itself, adds nothing");
});

test("tomlValue, tomlString, tomlStrings and hasTomlTable: the manifest reader's pieces", () => {
  const t1 = manifestTables('package.name = "x"\nworkspace.members = [\n "a", \'b\' # ]\n, "c"]\n[dependencies]\nfoo = "1"\n');
  assert.equal(tomlString(tomlValue(t1, "package", "name")), "x");
  assert.deepEqual(tomlStrings(tomlValue(t1, "workspace", "members")), ["a", "b", "c"]);
  assert.equal(tomlValue(t1, "package", "missing"), null);
  assert.equal(hasTomlTable(t1, "package"), true);
  assert.equal(hasTomlTable(t1, "workspace"), true);
  assert.equal(hasTomlTable(t1, "dependencies"), true);
  assert.equal(hasTomlTable(t1, "lib"), false);
  const t2 = manifestTables('[package]\nname = \'it"s\'\ndescription = "name = x"\n[workspace.dependencies]\nserde = "1"\n');
  assert.equal(tomlString(tomlValue(t2, "package", "name")), 'it"s');
  assert.equal(hasTomlTable(t2, "workspace"), true, "[workspace.dependencies] makes a workspace");
  assert.equal(hasTomlTable(t2, "package"), true);
  assert.equal(tomlString('"a\\"b"'), 'a"b');
  assert.equal(tomlString("'a\\b'"), "a\\b", "a literal string keeps its backslash");
  assert.equal(tomlString("12"), null);
  assert.equal(tomlString(null), null);
  assert.deepEqual(tomlStrings("true"), []);
  assert.deepEqual(tomlStrings('["a\\"]b", \'c]\']'), ['a"]b', "c]"], "a ] inside a string does not end the array");
  assert.deepEqual(tomlStrings("[]"), []);
  assert.deepEqual(tomlStrings('["unterminated'), ["unterminated"]);
});

test("workspaceExcludes reads single-quoted, commented and dotted exclude lists", (t) => {
  const root = mkRepo({ "Cargo.toml": "[workspace]\nexclude = [\n 'old', # ] gone\n \"older/\",\n]\n", "ws/Cargo.toml": 'workspace.exclude = ["x"]\n' }, t);
  const readFile = (f) => fs.readFileSync(f, "utf8");
  assert.deepEqual(workspaceExcludes(readFile, root, "."), ["old", "older"]);
  assert.deepEqual(workspaceExcludes(readFile, root, "ws"), ["ws/x"]);
  assert.deepEqual(workspaceExcludes(readFile, root, "missing"), []);
});

test("deferredBlock: with tracked manifests and no unit found it says so instead of claiming no Rust scan was due; unmatched manifests get a line", () => {
  const none = deferredBlock({ units: [], notRun: [], refs: ["refs/heads/x"], manifests: 3 });
  assert.match(none[0], /rust units none found among the 3 tracked Cargo\.toml \(read without cargo\); scans not run unknown \(no unit was found, so no Rust scan could be planned\); the pull request needs a HOST RECEIPT before merge$/);
  assert.doesNotMatch(none.join("\n"), /no Rust scan was due/);
  assert.equal(none.length, 2);
  assert.match(deferredBlock({ units: [], notRun: [], refs: ["refs/heads/x"] })[0], /rust units none found; scans not run none \(no Rust scan was due for this diff\)/, "no manifest at all keeps the old words");
  assert.match(deferredBlock({ units: ["a"], notRun: [], refs: ["refs/heads/x"], manifests: 2 })[0], /rust units a; scans not run none \(no Rust scan was due for this diff\)/, "a unit found and nothing due is the old words too");
  const some = deferredBlock({ units: ["a"], notRun: ["clippy:a"], refs: ["refs/heads/x"], manifests: 3, unmatched: ["crates/z/Cargo.toml", "crates/y/Cargo.toml"] });
  assert.equal(some.length, 3);
  assert.equal(some.filter((l) => /DEFERRED/.test(l)).length, 1, "still one headline");
  assert.match(some[2], /tracked Cargo\.toml with a package that matched no unit .*: crates\/z\/Cargo\.toml, crates\/y\/Cargo\.toml$/);
});

test("deferral: a manifest the reader cannot place is named in the block, and no unit found at all is not reported as nothing due", async (t) => {
  // A triple-quoted name is valid TOML the reader does not read: the workspace has a member it cannot see.
  const files = { "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n', "Cargo.lock": "version = 4\n", "crates/a/Cargo.toml": '[package]\nname = """a"""\nversion = "1.0.0"\n', "crates/a/src/lib.rs": "pub fn a() {}\n", ".rightkit-local-build.json": "{}" };
  const r = await pushGate(t, { files, spawnOptions: { manifests: ["Cargo.toml", "crates/a/Cargo.toml"] } });
  assert.equal(r.code, 0, r.err);
  assert.match(r.err, /rust units none found among the 2 tracked Cargo\.toml \(read without cargo\); scans not run unknown \(no unit was found, so no Rust scan could be planned\)/);
  assert.doesNotMatch(r.err, /no Rust scan was due/);
  assert.match(r.err, /matched no unit .*: crates\/a\/Cargo\.toml$/m, "named although discoverRustWorkspaces also skips it, for want of a lock");
  // The same repository with a name the reader can read finds the unit and has no such line.
  const fine = await pushGate(t, { files: { ...files, "crates/a/Cargo.toml": pkg("a") }, spawnOptions: { manifests: ["Cargo.toml", "crates/a/Cargo.toml"] } });
  assert.equal(fine.code, 0, fine.err);
  assert.match(fine.err, /rust units a; scans not run [^;]*clippy:a/);
  assert.doesNotMatch(fine.err, /matched no unit/);
});

test("deferral discovers single-quoted, dotted and path-dependency members from the tracked manifests", async (t) => {
  const files = {
    "Cargo.toml": "package.name = 'rootpkg'\npackage.version = '1.0.0'\nworkspace.members = ['crates/*'] # ]\n[dependencies]\nviapath = { path = 'tools/viapath' }\n", "Cargo.lock": "version = 4\n",
    "crates/a/Cargo.toml": pkg("a").replace('"a"', "'a'"), "crates/a/src/lib.rs": "pub fn a() {}\n", "tools/viapath/Cargo.toml": pkg("viapath"),
  };
  const r = await pushGate(t, { files, spawnOptions: { manifests: ["Cargo.toml", "crates/a/Cargo.toml", "tools/viapath/Cargo.toml"] } });
  assert.equal(r.code, 0, r.err);
  assert.match(r.err, /rust units a, rootpkg, viapath; scans not run /);
  assert.doesNotMatch(r.err, /matched no unit/);
  const gone = unmatchedManifests({ root: mkRepo(files, t), manifests: ["Cargo.toml", "crates/a/Cargo.toml", "tools/viapath/Cargo.toml"], rust: { units: [{ dir: "." }, { dir: "crates/a" }], skipped: [] }, readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.deepEqual(gone, ["tools/viapath/Cargo.toml"], "a package no unit covers is unmatched; a unit's manifest is not");
  assert.deepEqual(unmatchedManifests({ root: "/nowhere", manifests: ["x/Cargo.toml"], rust: { units: [], skipped: [{ manifest: "x/Cargo.toml" }] }, readFile: () => { throw new Error("never read"); } }), [], "a skipped manifest is accounted for");
  assert.deepEqual(unmatchedManifests({ root: "/nowhere", manifests: ["x/Cargo.toml"], rust: { units: [], skipped: [] }, readFile: () => { throw new Error("unreadable"); } }), ["x/Cargo.toml"], "an unreadable one is not");
});

test("manifestWorkspaceMetadata: a standalone package has nothing unplaced, and says so", (t) => {
  const files = { "Cargo.toml": pkg("app"), "src/lib.rs": "pub fn f() {}\n" };
  const root = mkRepo(files, t);
  const found = manifestWorkspaceMetadata({ root, dir: ".", tracked: () => ["Cargo.toml"], readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.deepEqual(found, { cargoDir: ".", members: [{ name: "app", dir: ".", manifest: "Cargo.toml", publish: true }], unplaced: [] });
  const nested = mkRepo({ "src-tauri/Cargo.toml": pkg("tauri-app", "publish = false\n") }, t);
  const inner = manifestWorkspaceMetadata({ root: nested, dir: "src-tauri", tracked: () => ["src-tauri/Cargo.toml"], readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.deepEqual(inner.unplaced, []);
  assert.deepEqual(inner.members.map((m) => `${m.name}:${m.publish}`), ["tauri-app:false"]);
});

test("deferral: a repository of standalone packages (no [workspace]) defers like a workspace: a single crate, a Tauri src-tauri, several packages", async (t) => {
  // main() added `read.unplaced` to its list for every repository; the standalone branch returned none, so the spread of undefined threw
  // and every such push was refused with "cannot discover the repository's units: read.unplaced is not iterable".
  const lib = "pub fn f() {}\n";
  const layouts = [
    ["a single crate", { "Cargo.toml": pkg("app"), "Cargo.lock": "version = 4\n", "src/lib.rs": lib }, ["Cargo.toml"], "src/lib.rs", ["app"]],
    ["a Tauri src-tauri", { "package.json": "{}", "src-tauri/Cargo.toml": pkg("tauri-app"), "src-tauri/Cargo.lock": "version = 4\n", "src-tauri/src/lib.rs": lib }, ["src-tauri/Cargo.toml"], "src-tauri/src/lib.rs", ["src-tauri:tauri-app"]],
    ["two standalone packages", { "tools/one/Cargo.toml": pkg("one"), "tools/one/Cargo.lock": "version = 4\n", "tools/one/src/lib.rs": lib, "tools/two/Cargo.toml": pkg("two"), "tools/two/Cargo.lock": "version = 4\n", "tools/two/src/lib.rs": lib }, ["tools/one/Cargo.toml", "tools/two/Cargo.toml"], "tools/one/src/lib.rs", ["tools/one:one", "tools/two:two"]],
    ["a workspace beside a standalone package", { "core/Cargo.toml": "[workspace]\nmembers = [\"crates/*\"]\n", "core/Cargo.lock": "version = 4\n", "core/crates/a/Cargo.toml": pkg("a"), "core/crates/a/src/lib.rs": lib, "tools/solo/Cargo.toml": pkg("solo"), "tools/solo/Cargo.lock": "version = 4\n", "tools/solo/src/lib.rs": lib }, ["core/Cargo.toml", "core/crates/a/Cargo.toml", "tools/solo/Cargo.toml"], "tools/solo/src/lib.rs", ["core:a", "tools/solo:solo"]],
  ];
  for (const [name, files, manifests, changed, units] of layouts) {
    const r = await pushGate(t, { files: { ...files, ".rightkit-local-build.json": "{}" }, spawnOptions: { manifests, diff: `${changed}\n` } });
    assert.equal(r.code, 0, `${name}: ${r.err}`);
    assert.doesNotMatch(r.err, /cannot discover the repository's units|not iterable/, name);
    assert.match(r.err, new RegExp(`rust gate DEFERRED to host qualification \\(no RustRight broker on this host\\): rust units ${units.join(", ")};`), name);
    assert.doesNotMatch(r.err, /matched no unit/, name);
    assert.deepEqual(r.after, r.before, `${name}: nothing was written`);
    // The refusals are unchanged: the same layout pushing main is still refused.
    const refused = await pushGate(t, { files: { ...files, ".rightkit-local-build.json": "{}" }, stdin: pushLine("refs/heads/main"), spawnOptions: { manifests, diff: `${changed}\n` } });
    assert.equal(refused.code, 1, name);
    assert.match(refused.err, NO_BROKER, name);
  }
});

test("manifestWorkspaceMetadata returns the manifests a glob or a path dependency reaches but whose package name it cannot read", (t) => {
  const files = { "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n[workspace.dependencies]\nviadep = { path = "libs/viadep" }\n', "crates/a/Cargo.toml": pkg("a"), "crates/odd/Cargo.toml": '[package]\nname = """odd"""\n', "libs/viadep/Cargo.toml": '[package]\nversion = "1.0.0"\n' };
  const root = mkRepo(files, t);
  const all = Object.keys(files);
  const found = manifestWorkspaceMetadata({ root, dir: ".", tracked: (n) => (n === "Cargo.toml" ? all : []), readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.deepEqual(found.members.map((m) => m.name), ["a"]);
  assert.deepEqual(found.unplaced, ["crates/odd/Cargo.toml", "libs/viadep/Cargo.toml"]);
});

// ---- gate false positives found by full-gate dry runs on real app branches (0.6.6) ----------------------------------------------------------
// FP3 several independent pnpm roots (CodeRight); FP2 an npm test script that runs the broker, on a host without it (ViewRight, PostRight).

// A repository as a file map: tracked(name) lists the files with that name, as `git ls-files :(glob)**/name` would.
const pnpmFixture = (files) => {
  const map = new Map(Object.entries(files).map(([k, v]) => [norm(path.join("/r", k)), v]));
  return {
    root: "/r", exists: (p) => map.has(norm(p)), readFile: (p) => map.get(norm(p)),
    tracked: (name) => [...map.keys()].map((k) => k.slice("/r/".length)).filter((k) => path.posix.basename(k) === name),
  };
};
const lock = "lockfileVersion: '9.0'\n";
const codeRight = {
  "apps/coderight-tauri/package.json": '{"name":"coderight-tauri","scripts":{"test":"vitest run"}}', "apps/coderight-tauri/pnpm-lock.yaml": lock,
  "apps/coderight-web/package.json": '{"name":"coderight-web","scripts":{"test":"vitest run"}}', "apps/coderight-web/pnpm-lock.yaml": lock,
  "src-tauri/Cargo.toml": pkg("coderight"), "README.md": "x",
};

test("pnpm roots: CodeRight's two independent roots are found, the repository root is not one, and each root's package is a unit named from its directory", () => {
  const fixture = pnpmFixture(codeRight);
  assert.deepEqual(discoverPnpmRoots(fixture), ["apps/coderight-tauri", "apps/coderight-web"]);
  assert.deepEqual(discoverNpmUnits(fixture), [
    { name: "apps-coderight-tauri", dir: "apps/coderight-tauri", root: "apps/coderight-tauri" },
    { name: "apps-coderight-web", dir: "apps/coderight-web", root: "apps/coderight-web" },
  ], "it returned [] before, so the gate planned no tsc, lint or test scan and a failing lockfile and audit at the repository root");
});

test("pnpm roots: a lock beside no package.json, inside node_modules, deeper than six levels or untracked is no root; a workspace file beside a package.json is", () => {
  const files = {
    "a/pnpm-lock.yaml": lock, // no package.json beside it
    "b/node_modules/dep/package.json": '{"name":"dep"}', "b/node_modules/dep/pnpm-lock.yaml": lock,
    "c/d/e/f/g/h/i/package.json": '{"name":"deep"}', "c/d/e/f/g/h/i/pnpm-lock.yaml": lock, // seven levels
    "c/d/e/f/g/h/package.json": '{"name":"six"}', "c/d/e/f/g/h/pnpm-lock.yaml": lock, // six levels
    "w/package.json": '{"name":"w"}', "w/pnpm-workspace.yaml": "packages: []\n",
  };
  const fixture = pnpmFixture(files);
  assert.deepEqual(discoverPnpmRoots(fixture), ["c/d/e/f/g/h", "w"]);
  const untracked = { ...fixture, tracked: (name) => fixture.tracked(name).filter((p) => !p.startsWith("w/")) };
  assert.deepEqual(discoverPnpmRoots(untracked), ["c/d/e/f/g/h"], "only tracked files make a root");
  assert.deepEqual(discoverPnpmRoots({ ...fixture, tracked: () => ["w/package.json", "w/pnpm-lock.yaml", "a/package.json"] }), ["w"], "a lister that ignores the name is filtered by file name");
  assert.deepEqual(discoverNpmUnits(pnpmFixture({})), []);
});

test("pnpm roots: the repository root is a root with a package.json or a workspace file, as before; nested roots come with their own workspace members", () => {
  const files = {
    "package.json": '{"name":"@rk/root"}', "pnpm-workspace.yaml": "packages:\n  - 'packages/*'\n  - 'apps/**'\n", "pnpm-lock.yaml": lock,
    "packages/a/package.json": '{"name":"@rk/a"}',
    "apps/web/package.json": '{"name":"web"}', // a glob of the root workspace
    "tpl/package.json": '{"name":"tpl"}', "tpl/pnpm-workspace.yaml": 'packages:\n  - "."\n  - "packages/*"\n', "tpl/pnpm-lock.yaml": lock,
    "tpl/packages/ui/package.json": '{"name":"ui"}',
    "apps/inner/package.json": '{"name":"inner"}', "apps/inner/pnpm-lock.yaml": lock, // a nested root below a root glob: the deeper root owns it
  };
  const fixture = pnpmFixture(files);
  assert.deepEqual(discoverPnpmRoots(fixture), [".", "apps/inner", "tpl"]);
  assert.deepEqual(discoverNpmUnits(fixture), [
    { name: "apps-inner", dir: "apps/inner", root: "apps/inner" },
    { name: "rk-a", dir: "packages/a" },
    { name: "web", dir: "apps/web" },
    { name: "tpl", dir: "tpl", root: "tpl" },
    { name: "tpl-packages-ui", dir: "tpl/packages/ui", root: "tpl" },
  ].sort((x, y) => (x.dir < y.dir ? -1 : 1)), "the repository root's units keep their package names and carry no root; the others are named from their directory");
  const noPackage = pnpmFixture({ "pnpm-workspace.yaml": "packages:\n  - 'packages/*'\n", "packages/a/package.json": '{"name":"a"}' });
  assert.deepEqual(discoverPnpmRoots(noPackage), ["."], "a workspace file without a root package.json is a root, as before");
  assert.deepEqual(discoverNpmUnits(noPackage), [{ name: "a", dir: "packages/a" }]);
});

test("pnpm roots: the walk is bounded and a collision or an unreadable manifest is an error, never a silent gap", () => {
  const many = Object.fromEntries(Array.from({ length: 65 }, (_, i) => [`apps/a${i}/package.json`, `{"name":"a${i}"}`]).concat(Array.from({ length: 65 }, (_, i) => [`apps/a${i}/pnpm-lock.yaml`, lock])));
  assert.throws(() => discoverPnpmRoots(pnpmFixture(many)), /65 pnpm roots \(the gate covers at most 64/);
  assert.throws(() => discoverNpmUnits(pnpmFixture({ "x/a-b/package.json": "{}", "x/a-b/pnpm-lock.yaml": lock, "x-a/b/package.json": "{}", "x-a/b/pnpm-lock.yaml": lock })), /two npm units are named x-a-b/);
  assert.throws(() => discoverNpmUnits(pnpmFixture({ "apps/z/package.json": "{nope", "apps/z/pnpm-lock.yaml": lock })), /^Error: apps\/z\/package\.json: /);
  assert.deepEqual(discoverNpmUnits(pnpmFixture({ "apps/z/package.json": "﻿{\"name\":\"z\"}", "apps/z/pnpm-lock.yaml": lock })).map((u) => u.name), ["apps-z"], "a byte order mark is skipped");
});

// The whole of main() over a repository of JavaScript only: git answers from the fixture, the scan runner records the plan.
const jsRepo = (extra = {}) => ({
  ...codeRight, "src-tauri/Cargo.toml": undefined,
  "apps/coderight-tauri/tsconfig.json": "{}", "apps/coderight-tauri/biome.json": "{}", "apps/coderight-tauri/src/a.ts": "",
  "apps/coderight-web/tsconfig.json": "{}", "apps/coderight-web/eslint.config.js": "", "apps/coderight-web/src/b.ts": "",
  ...extra,
});
const stripUndefined = (files) => Object.fromEntries(Object.entries(files).filter(([, v]) => v !== undefined));
const trackedJs = { "package.json": ["apps/coderight-tauri/package.json", "apps/coderight-web/package.json"], "pnpm-lock.yaml": ["apps/coderight-tauri/pnpm-lock.yaml", "apps/coderight-web/pnpm-lock.yaml"] };

test("gate: a repository of independent pnpm roots plans the lockfile and audit per root, and tsc, the linter it configures and the tests per unit", async (t) => {
  const r = await pushGate(t, {
    stdin: pushLine("refs/heads/claude/port"), files: stripUndefined(jsRepo()),
    spawnOptions: { manifests: [], tracked: trackedJs, diff: "apps/coderight-tauri/package.json\napps/coderight-web/src/b.ts\n" },
  });
  assert.equal(r.code, 0, r.err);
  const scans = r.seen[0].plan.scans; const ids = scans.map((s) => s.id);
  for (const id of [
    "pnpm-lockfile:apps/coderight-tauri", "pnpm-audit:apps/coderight-tauri", "pnpm-lockfile:apps/coderight-web", "pnpm-audit:apps/coderight-web",
    "tsc:apps-coderight-tauri", "biome:apps-coderight-tauri", "test:npm:apps-coderight-tauri", "tsc:apps-coderight-web", "eslint:apps-coderight-web", "test:npm:apps-coderight-web",
  ]) assert.ok(ids.includes(id), `${id} planned: ${ids.join(" ")}`);
  assert.ok(!ids.includes("pnpm-lockfile") && !ids.includes("pnpm-audit"), "nothing at the repository root, which holds no package.json (No package.json found)");
  assert.ok(!ids.includes("eslint:apps-coderight-tauri") && !ids.includes("biome:apps-coderight-web"), "a Biome app is not linted with ESLint, nor an ESLint app with Biome");
  assert.equal(scans.find((s) => s.id === "pnpm-audit:apps/coderight-web").cwd, "apps/coderight-web");
  assert.equal(scans.find((s) => s.id === "tsc:apps-coderight-tauri").cwd, "apps/coderight-tauri");
  assert.deepEqual(scans.find((s) => s.id === "test:npm:apps-coderight-web").args, ["--dir", "apps/coderight-web", "run", "test"]);
  assert.match(r.out, /\d+ scan\(s\) for 2 changed file\(s\)/);
  assert.deepEqual(r.after, r.before, "the tree is what it was");
  // The checkout must equal HEAD where these scans read it: each root's install graph and linter configuration is checked, not only the units' directories.
  const status = r.log.find((l) => l[0] === "git" && l[1] === "status");
  for (const p of ["apps/coderight-web/pnpm-lock.yaml", "apps/coderight-tauri/package.json", "apps/coderight-web/.npmrc", "apps/coderight-tauri/biome.json", "apps/coderight-web/eslint.config.js", "apps/coderight-web/biome.jsonc"]) assert.ok(status.includes(p), `${p} is checked against HEAD`);
});

test("gate --plan: lists the pnpm roots' units with their root-carrying names and the per-root scans", async (t) => {
  const root = mkRepo(stripUndefined(jsRepo()), t);
  const c = capture();
  const code = await main(["--plan", "--base", "origin/main"], { ...c.io, spawn: pushSpawn(root, { manifests: [], tracked: trackedJs, diff: "apps/coderight-web/src/b.ts\n" }), cwd: root, env: {} });
  assert.equal(code, 0, c.err.join("\n"));
  const out = c.out.join("\n");
  assert.match(out, /npm units: apps-coderight-tauri=apps\/coderight-tauri, apps-coderight-web=apps\/coderight-web/);
  assert.match(out, /\[A\] pnpm-lockfile:apps\/coderight-web :: .*pnpm install --frozen-lockfile/);
  assert.doesNotMatch(out, /pnpm-lockfile:apps\/coderight-tauri/, "only the root whose files changed");
});

test("gate: a repository with no pnpm root plans no pnpm scan, whatever package.json changed", async (t) => {
  const r = await pushGate(t, {
    stdin: pushLine("refs/heads/claude/port"), files: stripUndefined({ "docs/site/package.json": '{"name":"site"}', "README.md": "x" }),
    spawnOptions: { manifests: [], tracked: { "package.json": ["docs/site/package.json"] }, diff: "docs/site/package.json\n" },
  });
  assert.equal(r.code, 0, r.err);
  assert.ok(!r.seen[0].plan.scans.some((s) => /^pnpm-|^tsc:|^test:npm:/.test(s.id)), r.seen[0].plan.scans.map((s) => s.id).join(" "));
});

test("cache digest: a unit of a nested pnpm root keys on that root's install graph, resolves workspace dependencies inside its root, and a one-root repository keeps its key", () => {
  const manifests = {
    "apps/foo/packages/ui/package.json": JSON.stringify({ name: "ui", dependencies: { core: "workspace:*" } }),
    "apps/foo/packages/core/package.json": '{"name":"core"}',
    "apps/bar/package.json": '{"name":"core"}', // the same package name in another root
  };
  const units = [
    { name: "apps-bar", dir: "apps/bar", root: "apps/bar" },
    { name: "apps-foo-packages-core", dir: "apps/foo/packages/core", root: "apps/foo" },
    { name: "apps-foo-packages-ui", dir: "apps/foo/packages/ui", root: "apps/foo" },
  ];
  const make = (ids, tree = "t1") => buildCacheDigest({ objectId: (p) => ids[p] ?? null, headTree: tree, rustUnits: [], npmUnits: units, listFiles: () => [], readBlob: (p) => { if (!(p in manifests)) throw new Error(`no ${p}`); return manifests[p]; } });
  const base = { "apps/foo/packages/ui": "u1", "apps/foo/packages/core": "c1", "apps/bar": "b1", "apps/foo/pnpm-lock.yaml": "fl1", "apps/bar/pnpm-lock.yaml": "bl1" };
  const ui = { id: "test:npm:apps-foo-packages-ui", class: "B" };
  assert.equal(make(base, "t1")(ui), make(base, "t2")(ui), "not keyed on the tree");
  assert.notEqual(make({ ...base, "apps/foo/pnpm-lock.yaml": "fl2" })(ui), make(base)(ui), "the unit's own root's lock is an input (it sits above the member's directory)");
  assert.equal(make({ ...base, "apps/bar/pnpm-lock.yaml": "bl2" })(ui), make(base)(ui), "another root's lock is not");
  assert.notEqual(make({ ...base, "apps/foo/packages/core": "c2" })(ui), make(base)(ui), "its workspace dependency, found inside its own root");
  assert.equal(make({ ...base, "apps/bar": "b2" })(ui), make(base)(ui), "the same package name in another root is not that dependency");
  assert.notEqual(make({ ...base, "apps/foo/.npmrc": "n1" })(ui), make(base)(ui), "the root's .npmrc");
  // Linter configuration of an ancestor directory counts when the commit has it, and a commit without one keeps its key.
  assert.notEqual(make({ ...base, "apps/foo/eslint.config.cjs": "e1" })(ui), make(base)(ui), "an ESLint config of the root, under a name the shared list lacks");
  assert.notEqual(make({ ...base, "apps/biome.jsonc": "b1" })(ui), make(base)(ui), "a Biome config of an ancestor directory");
  const biome = { id: "biome:apps-bar", class: "B" };
  assert.equal(make(base, "t1")(biome), make(base, "t2")(biome), "a biome scan is keyed like eslint and tsc: its package, not the tree");
  assert.notEqual(make({ ...base, "apps/bar": "b2" })(biome), make(base)(biome));
  // One root: the shared list is what it always was, whatever lint config names the commit lacks.
  const one = buildCacheDigest({ objectId: (p) => ({ "packages/p": "p1", "pnpm-lock.yaml": "l" })[p] ?? null, headTree: "t", rustUnits: [], npmUnits: [{ name: "p", dir: "packages/p" }], listFiles: () => [], readBlob: () => '{"name":"p"}' });
  const shared = JSON.parse(one({ id: "tsc:p", class: "B" })).shared.map(([p]) => p);
  assert.deepEqual(shared, ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", "tsconfig.json", "tsconfig.base.json", "eslint.config.js", "eslint.config.mjs", ".eslintrc.json", "biome.json", "vitest.config.ts", "vitest.config.mjs", "vitest.workspace.ts"]);
});

// ---- FP2: an npm test script that runs the broker, on a host without it -------------------------------------------------------------------------------------

const viewright = {
  name: "viewright", license: "MIT",
  scripts: { test: "pnpm test:edge && pnpm test:e2e", "test:edge": "vitest run", "test:e2e": "rustright storage run --app viewright -- right-release test" },
};
const viewrightFiles = (scripts = viewright.scripts) => gateFiles({ "package.json": JSON.stringify({ ...viewright, scripts }), "src/app.ts": "" });
const viewrightSpawn = { tracked: { "package.json": ["package.json"] }, diff: "package.json\ncrates/a/src/lib.rs\n" };
const E2E_COMMAND = "rustright storage run --app viewright -- right-release test";

test("deferral: an npm test scan whose script chain runs rustright is deferred with the Rust gate, named with its reason, and never reported as passed", async (t) => {
  const r = await pushGate(t, { files: viewrightFiles(), spawnOptions: viewrightSpawn });
  assert.equal(r.code, 0, r.err);
  const ids = r.seen[0].plan.scans.map((s) => s.id);
  assert.ok(!ids.includes("test:npm:viewright"), "it is not run (pnpm run test would end in: command not found: rustright)");
  assert.ok(ids.includes("license") && ids.includes("json-parse"), "the other scans still run");
  const headline = r.err.split("\n").find((l) => /rust gate DEFERRED/.test(l));
  assert.match(headline, /scans not run [^;]*test:npm:viewright/, "listed by name in the DEFERRED block");
  assert.match(r.err, new RegExp(`npm test scan\\(s\\) deferred with the Rust gate, never run and never passed, because the package script runs rustright and this host has no broker: test:npm:viewright \\(script test -> test:e2e: ${E2E_COMMAND.replaceAll(".", "\\.")}\\)`));
  assert.match(r.out, /note: no RustRight broker on this host: 6 Rust scan\(s\) are deferred to host qualification, not run and not passed; 1 npm test scan\(s\) whose script runs the broker or cargo are deferred with them, not run and not passed: test:npm:viewright/);
  assert.doesNotMatch(r.out, /\[\d+\/\d+\] pass +test:npm:viewright/, "never reported as passed");
  assert.deepEqual(r.seen[0].plan.deferred, [], "nothing is left in the plan as deferred");
  assert.equal(r.seen[0].plan.scanSetDigest, digestScanSet(r.seen[0].plan.scans), "the plan digest describes what runs");
  assert.deepEqual(r.after, r.before, "the tree is byte for byte what it was");
});

test("deferral: plain cargo in a test script chain is the unmanaged toolchain on such a host, and is deferred the same way (PostRight's test:rust)", async (t) => {
  const scripts = { test: "pnpm run test:rust && pnpm run test:e2e", "test:rust": "rustright cargo build -p x && rustright cargo test --workspace", "test:e2e": "cargo test -p e2e" };
  const r = await pushGate(t, { files: viewrightFiles(scripts), spawnOptions: viewrightSpawn });
  assert.equal(r.code, 0, r.err);
  assert.ok(!r.seen[0].plan.scans.some((s) => s.id === "test:npm:viewright"));
  assert.match(r.err, /because the package script runs rustright and this host has no broker: test:npm:viewright \(script test -> test:rust: rustright cargo build -p x\)/);
  const cargoOnly = await pushGate(t, { files: viewrightFiles({ test: "cargo test" }), spawnOptions: viewrightSpawn });
  assert.match(cargoOnly.err, /the package script runs cargo and this host has no broker: test:npm:viewright \(script test: cargo test\)/);
});

test("deferral: an npm test script that never reaches the broker still runs on a host without it", async (t) => {
  const r = await pushGate(t, { files: viewrightFiles({ test: "pnpm test:edge", "test:edge": "vitest run" }), spawnOptions: viewrightSpawn });
  assert.equal(r.code, 0, r.err);
  assert.ok(r.seen[0].plan.scans.some((s) => s.id === "test:npm:viewright"), "it runs");
  assert.doesNotMatch(r.err, /npm test scan\(s\) deferred/);
  assert.match(r.err, /scans not run [^;]*cargo-metadata/, "the Rust scans are still deferred");
  assert.doesNotMatch(r.out, /npm test scan\(s\) whose script/);
});

test("deferral unchanged for the npm test scan: a host with the broker, CI, a protected ref and the opt-out run it as before", async (t) => {
  const withBroker = await pushGate(t, { files: viewrightFiles(), spawnOptions: { ...viewrightSpawn, broker: true } });
  assert.equal(withBroker.code, 0, withBroker.err);
  const brokerScan = withBroker.seen[0].plan.scans.find((s) => s.id === "test:npm:viewright");
  assert.ok(brokerScan, "a host that has the broker runs the test scan");
  assert.equal(brokerScan.brokerCall.tool, "rustright");
  assert.doesNotMatch(withBroker.err, /DEFERRED|npm test scan\(s\) deferred/);
  assert.doesNotMatch(withBroker.out, /npm test scan\(s\) whose script/);
  const shim = await pushGate(t, { files: viewrightFiles(), spawnOptions: { ...viewrightSpawn, shim: true }, readFile: shimRead });
  assert.ok(shim.seen[0].plan.scans.some((s) => s.id === "test:npm:viewright"), "the managed shim counts as the broker");
  for (const [name, o] of [
    ["CI", { env: { CI: "1" } }], ["GITHUB_ACTIONS", { env: { GITHUB_ACTIONS: "true" } }], ["the opt-out", { env: { [DEFER_OPT_OUT_ENV]: "1" } }],
    ["a protected ref", { stdin: pushLine("refs/heads/main") }], ["a release branch", { stdin: pushLine("refs/heads/release/1.0") }],
  ]) {
    const r = await pushGate(t, { files: viewrightFiles(), spawnOptions: viewrightSpawn, ...o });
    assert.equal(r.code, 1, name);
    assert.match(r.err, NO_BROKER, `${name}: it refuses as before, nothing is deferred`);
    assert.equal(r.seen.length, 0, `${name}: nothing ran, so the npm test scan is neither skipped nor reported passed`);
    assert.doesNotMatch(r.err, /npm test scan\(s\) deferred/, name);
  }
});

test("deferredBlock: npm test scans get a line of their own with the tool, the script chain and the command; without them the block is what it was", () => {
  const base = { units: ["a"], notRun: ["clippy:a", "test:npm:viewright"], refs: ["refs/heads/claude/x"] };
  const plain = deferredBlock({ ...base, npmCalls: [] });
  assert.deepEqual(plain, deferredBlock(base));
  assert.equal(plain.length, 2);
  const withNpm = deferredBlock({ ...base, npmCalls: [{ id: "test:npm:viewright", tool: "rustright", via: ["test", "test:e2e"], command: E2E_COMMAND }, { id: "test:npm:other", tool: "cargo", via: ["test"], command: "cargo test" }] });
  assert.deepEqual(withNpm.slice(0, 2), plain);
  assert.equal(withNpm.length, 3);
  assert.equal(withNpm[2], `rightkit-gate:   npm test scan(s) deferred with the Rust gate, never run and never passed, because the package script runs rustright or cargo and this host has no broker: test:npm:viewright (script test -> test:e2e: ${E2E_COMMAND}); test:npm:other (script test: cargo test)`);
  assert.match(withNpm[0], /scans not run clippy:a, test:npm:viewright;/, "named in the headline too");
});
