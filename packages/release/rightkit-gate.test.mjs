import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import * as shipped from "./lock-graph.mjs";
import * as ci from "../../scripts/ci/select.mjs";
import {
  buildCacheDigest, discoverNpmUnits, discoverRustUnits, globToRegExp, hookText, lockScope, main, parseWorkspaceGlobs, rustUnitOfScanId,
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
  assert.equal(lockScope({ ...base, changed: ["Cargo.lock", "Cargo.toml"] }), null);
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

test("cache digest: a cargo scan covers its lock closure and the shared build inputs, nothing else", () => {
  const lock = shipped.parseLock(lockPkg("a", "1.0.0", ["b", "serde"]) + lockPkg("b", "1.0.0") + lockPkg("c", "1.0.0") + lockPkg("serde", "1.0.0"));
  const units = [{ name: "a", dir: "crates/a" }, { name: "b", dir: "crates/b" }, { name: "c", dir: "crates/c" }];
  const ids = { "crates/a": "ta", "crates/b": "tb", "crates/c": "tc", "Cargo.lock": "lock", "Cargo.toml": "root", ".cargo": "cargo", "deny.toml": "deny", "ci/gate.config.json": "cfg", vendor: "v" };
  const digestOf = (over = {}, tree = "head") => {
    const map = { ...ids, ...over };
    return buildCacheDigest({ objectId: (p) => map[p] ?? null, headTree: tree, rustUnits: units, cargoDir: ".", rustLock: lock });
  };
  const clippyA = { id: "clippy:a", class: "B" };
  const base = digestOf()(clippyA);
  assert.equal(digestOf()(clippyA), base, "identical inputs");
  assert.equal(digestOf({}, "other-head")(clippyA), base, "the whole tree is not an input of a cargo scan");
  assert.notEqual(digestOf({ "crates/b": "tb2" })(clippyA), base, "a closure member changed");
  assert.notEqual(digestOf({ "crates/a": "ta2" })(clippyA), base, "the crate itself changed");
  assert.equal(digestOf({ "crates/c": "tc2" })(clippyA), base, "a workspace crate outside the closure changed");
  for (const shared of ["Cargo.lock", "Cargo.toml", ".cargo", "deny.toml", "ci/gate.config.json", "vendor"]) assert.notEqual(digestOf({ [shared]: "x" })(clippyA), base, shared);
  assert.notEqual(digestOf()({ id: "check:c", class: "B" }), base, "another crate, another digest");
  assert.equal(digestOf({ "crates/a": "ta2" })({ id: "check:c", class: "B" }), digestOf()({ id: "check:c", class: "B" }), "c does not depend on a");
  assert.notEqual(digestOf()({ id: "test:rust:a:1", class: "B" }), null);
});

test("cache digest: class A and an unanswerable git are never cached; non-cargo scans use the whole tree", () => {
  const lock = shipped.parseLock(lockPkg("a", "1.0.0"));
  const make = (objectId, tree = "t1") => buildCacheDigest({ objectId, headTree: tree, rustUnits: [{ name: "a", dir: "crates/a" }], cargoDir: ".", rustLock: lock });
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
