import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import * as shipped from "./lock-graph.mjs";
import * as ci from "../../scripts/ci/select.mjs";
import { spawnSync } from "./background-process.mjs";
import {
  EXTERNAL_INPUT_FILE_LIMIT, QUIET_LEASE_TTL_MS, QUIET_LEASE_WAIT_DRAIN_MS, buildCacheDigest, externalInputs, discoverNpmUnits, discoverRustUnits, discoverRustWorkspaces, globToRegExp, hookText, lockScope, main, manifestChangeIsLockReflected, manifestTables, parseWorkspaceGlobs, quietLeaseLane, rustUnitName, rustUnitOfScanId, trampolineText, workspaceExcludes,
} from "./bin/rightkit-gate.mjs";

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
  const found = discoverRustUnits({ root, spawn, cargoCommand: ["rightkit", "cargo"], tracked, readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.deepEqual(found.units, [{ name: "a", dir: "crates/a", publish: true }, { name: "b", dir: "tools/b", publish: false }]);
  assert.equal(found.cargoDir, ".");
  assert.deepEqual(found.uncovered, ["fixtures/old/Cargo.toml"], "a crate outside the workspace is reported, not silently dropped");
  assert.deepEqual(calls[0][1], ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"]);
  assert.equal(calls[0][0], "rightkit");
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
  assert.match(text, /pnpm exec rightkit-gate --base origin\/main/);
  assert.match(text, /exec node "[^"]*bin\/rightkit-gate\.mjs" --base origin\/main/);
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
function repoSpawn(root, { log = [], diff = "crates/a/src/lib.rs\n", cargoLines = "/bin/rightkit\n" } = {}) {
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
    if (cmd === "where" || cmd === "which") return args[0] === "rightkit" ? ok(cargoLines) : no();
    if (cmd === "rightkit" && args[1] === "metadata") return ok(metadata);
    return no(`unexpected ${cmd} ${line}`);
  };
}
const gateFiles = (extra = {}) => ({
  "Cargo.toml": "[workspace]\nmembers = [\"crates/*\"]\n", "Cargo.lock": "version = 4\n",
  "crates/a/Cargo.toml": pkg("a"), "crates/a/src/lib.rs": "pub fn a() {}\n", "crates/b/Cargo.toml": pkg("b", "publish = false\n"), "crates/b/src/lib.rs": "",
  ".rightkit-local-build.json": "{}", ...extra,
});

test("--plan prints the plan for a fake repo from its own units and runs nothing", async (t) => {
  const root = mkRepo(gateFiles(), t);
  const log = []; const c = capture();
  const code = await main(["--plan", "--base", "origin/main"], { ...c.io, spawn: repoSpawn(root, { log }), cwd: root, env: {} });
  assert.equal(code, 0, c.err.join("\n"));
  const out = c.out.join("\n");
  assert.match(out, /rust units: a=crates\/a, b=crates\/b; npm units: none/);
  assert.match(out, /\[B\] clippy:a :: .*clippy -p a /);
  assert.match(out, /rightkit cargo clippy -p a --no-deps --all-targets --locked -- -D warnings/);
  assert.match(out, /\[A\] cargo-package-list:a /);
  assert.doesNotMatch(out, /clippy:b|cargo-package-list:b/);
  assert.doesNotMatch(out, /deferred/, "the compile grant is present");
  assert.ok(!log.some((l) => l[0] === "rightkit" && l[1] === "cargo" && l[2] !== "metadata"), "only discovery ran through cargo");
  assert.ok(!log.some((l) => l.includes("archive") || l.includes("status") || l.includes("tar")), "a plan exports and checks nothing");
});

test("the gate blocks without a local compile grant: Rust units cannot be discovered through the broker", async (t) => {
  const { ".rightkit-local-build.json": _grant, ...files } = gateFiles();
  const root = mkRepo(files, t);
  const c = capture();
  assert.equal(await main(["--plan"], { ...c.io, spawn: repoSpawn(root), cwd: root, env: {} }), 1);
  assert.match(c.err.join("\n"), /BLOCKED: no local compile grant/);
});

test("the gate blocks when neither rightkit nor the managed cargo shim resolves", async (t) => {
  const root = mkRepo(gateFiles(), t);
  const c = capture();
  const code = await main(["--plan"], { ...c.io, spawn: repoSpawn(root, { cargoLines: "" }), cwd: root, env: {} });
  // `where rightkit` answers nothing: the only fallback is a cargo shim, and `where cargo` fails here.
  assert.equal(code, 1);
  assert.match(c.err.join("\n"), /neither `rightkit` nor the managed cargo shim/);
});

test("pre-push: the pushed commit must be the checked-out HEAD, before anything else runs", async (t) => {
  const root = mkRepo(gateFiles(), t);
  const log = []; const c = capture();
  const line = `refs/heads/main ${"b".repeat(40)} refs/heads/main ${"0".repeat(40)}\n`;
  const code = await main([], { ...c.io, spawn: repoSpawn(root, { log }), cwd: root, env: {}, readStdin: () => line });
  assert.equal(code, 1);
  assert.match(c.err.join("\n"), /BLOCKED: pushing bbbbbbbbbb to refs\/heads\/main, but HEAD is aaaaaaaaaa/);
  assert.ok(!log.some((l) => l[0] === "rightkit"), "no cargo ran");
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

test("cache digest: class A and an unanswerable git are never cached; non-cargo scans use the whole tree", () => {
  const lock = shipped.parseLock(lockPkg("a", "1.0.0"));
  const make = (objectId, tree = "t1") => buildCacheDigest({ objectId, headTree: tree, rustUnits: [{ name: "a", dir: "crates/a" }], cargoDir: ".", rustLock: lock, ...plainSources });
  assert.equal(make(() => "x")({ id: "cargo-deny", class: "A" }), null);
  assert.equal(make(() => "x")({ id: "node-check:a.mjs", class: "A" }), null);
  assert.equal(make(() => { throw new Error("git down"); })({ id: "clippy:a", class: "B" }), null);
  const scripts = { id: "test:scripts-ci", class: "B" };
  assert.equal(make(() => "x")(scripts), make(() => "y")(scripts), "tree scans ignore per-path ids");
  assert.notEqual(make(() => "x", "t1")(scripts), make(() => "x", "t2")(scripts));
  // No lock graph: the closure is unknown, so a cargo scan falls back to the whole tree rather than a guess.
  const noLock = buildCacheDigest({ objectId: () => "x", headTree: "t1", rustUnits: [{ name: "a", dir: "crates/a" }], rustLock: null });
  assert.equal(noLock({ id: "clippy:a", class: "B" }), make(() => "x")(scripts));
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
  const found = discoverRustWorkspaces({ root, spawn: metaSpawn(root, table, calls), cargoCommand: ["rightkit", "cargo"], tracked, ...extra });
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
  for (const c of calls) { assert.equal(c.cmd, "rightkit"); assert.deepEqual(c.args, ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"]); }
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
    if (cmd === "rightkit" && args[1] === "metadata") return cargo(cmd, args, opts);
    const line = args.join(" ");
    if (cmd === "git") {
      if (line === "rev-parse --show-toplevel") return ok(root);
      if (line === "rev-parse HEAD") return ok(HEAD);
      if (args[0] === "ls-files" && args.includes("--others")) return ok(untracked);
      if (args[0] === "ls-files") return ok(args.at(-1).endsWith("Cargo.toml") ? `${tracked.join("\0")}\0` : args.at(-1).endsWith("Cargo.lock") ? `${locks.join("\0")}\0` : "");
      if (args[0] === "diff") return ok(diff);
    }
    if (cmd === "where" || cmd === "which") return args[0] === "rightkit" ? ok("/bin/rightkit\n") : no();
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
  assert.match(out, /\[B\] clippy:hub\/src-tauri:app :: rightkit cargo clippy -p app --no-deps --all-targets --locked -- -D warnings/);
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

test("the pre-push hook: runs .git/hooks/pre-push first, feeds the same stdin to it and to the gate, and stops on the first failure", { skip: !shAvailable && "no sh" }, (t) => {
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
  assert.equal(calls, `own origin https://example.test/r.git\n${ref}--\ngate --base origin/main\n${ref}--\n`, "own hook first with git's arguments, then the gate, same stdin");
  assert.match(ran.stderr, /rightkit-gate: running \.git\/hooks\/pre-push/);
  assert.match(ran.stderr, /rightkit-gate: using node_modules\/\.bin\/rightkit-gate/);
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

test("the pre-push hook: without an own hook only the gate runs; its temp file is gone afterwards", { skip: !shAvailable && "no sh" }, (t) => {
  const tmp = mkRepo({}, t);
  const log = path.join(tmp, "calls.log");
  const root = hookRepo(t, { ".githooks/pre-push": hookText("bin/rightkit-gate.mjs"), "node_modules/.bin/rightkit-gate": recorder(log, "gate") });
  const ran = runHook(root, "x y z w\n", { env: { TMPDIR: tmp } });
  assert.equal(ran.status, 0, ran.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "gate --base origin/main\nx y z w\n--\n");
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
    assert.equal(fs.readFileSync(log, "utf8"), `gate@${where} --base origin/main\nin\n--\n`);
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
  assert.equal(fs.readFileSync(log, "utf8"), "pnpm exec rightkit-gate --base origin/main\nin\n--\n");
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
