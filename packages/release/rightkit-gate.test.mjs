import assert from "node:assert/strict";
import crypto from "node:crypto";
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
import { PINNED_SCAN_TOOLS } from "./deterministic-scans.mjs";
import { EXTERNAL_INPUT_FILE_LIMIT, QUIET_LEASE_TTL_MS, QUIET_LEASE_WAIT_DRAIN_MS, buildCacheDigest, externalInputs, discoverNpmUnits, discoverRustUnits, discoverRustWorkspaces, globToRegExp, hasTomlTable, hookText, lockScope, main, manifestChangeIsLockReflected, PARTIAL_EXIT, manifestTables, parsePushLines, parseWorkspaceGlobs, prePushPlan, quietLeaseLane, rustUnitName, rustUnitOfScanId, tomlString, tomlStrings, tomlValue, trampolineText, workspaceExcludes, exportTmpRoot } from "./bin/rightkit-gate.mjs";
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


// ---- shared fixtures (kept from the removed deferral section) ----



// Git forbids an ASCII space in a ref name and allows every other non-control character, Unicode whitespace included.
const UNICODE_SPACES = [["U+00A0 no-break space", " "], ["U+2003 em space", " "], ["U+3000 ideographic space", "　"], ["U+2028 line separator", " "], ["U+FEFF zero-width no-break space", "﻿"]];

// A repository as a file map: tracked(name) lists the files with that name, as `git ls-files :(glob)**/name` would.
const pnpmFixture = (files) => {
  const map = new Map(Object.entries(files).map(([k, v]) => [norm(path.join("/r", k)), v]));
  return {
    root: "/r", exists: (p) => map.has(norm(p)), readFile: (p) => map.get(norm(p)),
    tracked: (name) => [...map.keys()].map((k) => k.slice("/r/".length)).filter((k) => path.posix.basename(k) === name),
  };
};


const gitLine = (localRef, remoteRef, { sha = HEAD, remote = ZEROS } = {}) => `${localRef} ${sha} ${remoteRef} ${remote}\n`;


const benignLine = (name = "benign") => pushLine(`refs/heads/claude/${name}`);


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
  const env = { ...inherited, PATH: toolPath, GIT_EXEC_PATH: exec.stdout.trim(), CI: "", GITHUB_ACTIONS: "", GIT_TERMINAL_PROMPT: "0" };
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


const lock = "lockfileVersion: '9.0'\n";


const codeRight = {
  "apps/coderight-tauri/package.json": '{"name":"coderight-tauri","scripts":{"test":"vitest run"}}', "apps/coderight-tauri/pnpm-lock.yaml": lock,
  "apps/coderight-web/package.json": '{"name":"coderight-web","scripts":{"test":"vitest run"}}', "apps/coderight-web/pnpm-lock.yaml": lock,
  "src-tauri/Cargo.toml": pkg("coderight"), "README.md": "x",
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

test("install-hook registers the one-door agent hook in .claude/settings.json once, keeping the settings already there", async (t) => {
  const root = mkRepo({ "node_modules/@rightkit/release/bin/rightkit-gate.mjs": "", "node_modules/@rightkit/release/bin/rightkit-agent-hook.mjs": "", ".claude/settings.json": JSON.stringify({ permissions: { allow: ["Bash(ls)"] } }) }, t);
  const run = async () => { const c = capture(); const code = await main(["install-hook"], { ...c.io, spawn: hookSpawn(root, { hooksPath: "", sets: [] }), cwd: root }); return { code, out: c.out.join("\n") }; };
  const first = await run();
  assert.equal(first.code, 0);
  const settings = JSON.parse(fs.readFileSync(path.join(root, ".claude", "settings.json"), "utf8"));
  assert.deepEqual(settings.permissions, { allow: ["Bash(ls)"] }, "existing settings are kept");
  assert.deepEqual(settings.hooks.PreToolUse, [{ matcher: "Bash", hooks: [{ type: "command", command: "node node_modules/@rightkit/release/bin/rightkit-agent-hook.mjs" }] }]);
  const second = await run();
  assert.match(second.out, /agent hook already present/);
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, ".claude", "settings.json"), "utf8")).hooks.PreToolUse.length, 1, "never added twice");
});

test("install-hook points node at the installed package when it is in node_modules", async (t) => {
  const root = mkRepo({ "node_modules/@rightkit/release/bin/rightkit-gate.mjs": "" }, t);
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookSpawn(root, { hooksPath: "", sets: [] }), cwd: root }), 0);
  assert.equal(fs.readFileSync(path.join(root, ".githooks", "pre-push"), "utf8"), hookText("node_modules/@rightkit/release/bin/rightkit-gate.mjs"));
});

// ---- the gate with injected spawn ------------------------------------------------------------------------------

// The snapshot listing the gate asks for (`git ls-tree -r --name-only -z <rev>`, or `git ls-files -z --cached` for the working tree):
// the harness's own tracked Cargo manifests and locks, plus every other file of the fixture on disk.
const fullListing = (root, manifests, locks = null) => {
  const disk = fs.readdirSync(root, { recursive: true }).map(String).map((f) => f.replaceAll("\\", "/")).filter((f) => !f.startsWith(".git/") && f !== ".git" && fs.statSync(path.join(root, f)).isFile());
  const other = disk.filter((f) => !/(^|\/)Cargo\.(toml|lock)$/.test(f));
  const lockFiles = locks ?? disk.filter((f) => /(^|\/)Cargo\.lock$/.test(f));
  return [...new Set([...manifests, ...lockFiles, ...other])].map((f) => `${f}\0`).join("");
};
const wholeListing = (args) => args[0] === "ls-tree" || (args[0] === "ls-files" && !args.some((a) => String(a).startsWith(":(glob)")) && !args.includes("--others"));

const HEAD = "a".repeat(40);
function repoSpawn(root, { log = [], diff = "crates/a/src/lib.rs\n", cargoLines = "/bin/rustright\n" } = {}) {
  const metadata = meta(root, ".", [["a", "crates/a"], ["b", "crates/b", []]]);
  return (cmd, args) => {
    log.push([cmd, ...args]);
    const line = args.join(" ");
    if (cmd === "git") {
      if (line === "rev-parse --show-toplevel") return ok(root);
      if (line === "rev-parse HEAD") return ok(HEAD);
      if (wholeListing(args)) return ok(fullListing(root, ["Cargo.toml", "crates/a/Cargo.toml", "crates/b/Cargo.toml"]));
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
  assert.ok(!log.some((l) => l.includes("archive") || l.includes("read-tree") || l.includes("checkout-index") || l.includes("status") || l.includes("tar")), "a plan exports and checks nothing");
});

test("no local compile grant file is needed: the broker (or the gate on a direct host) admits compiles, not a file in the repository", async (t) => {
  const { ".rightkit-local-build.json": _grant, ...files } = gateFiles();
  const root = mkRepo(files, t);
  const c = capture();
  assert.equal(await main(["--plan"], { ...c.io, spawn: repoSpawn(root), cwd: root, env: {} }), 0, c.err.join("\n"));
  assert.doesNotMatch(c.out.join("\n"), /deferred: no local compile grant/);
});

test("executor: a broker-managed machine without a reachable broker is refused; any other host runs cargo directly through the same plan", async (t) => {
  const root = mkRepo(gateFiles(), t);
  // A broker-managed machine (the broker socket is configured) where `rustright` does not resolve: refused, nothing planned or run.
  const managed = capture();
  assert.equal(await main(["--plan"], { ...managed.io, spawn: repoSpawn(root, { cargoLines: "" }), cwd: root, env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/tmp/broker.sock" } }), 1);
  assert.match(managed.err.join("\n"), /BLOCKED: this machine is broker-managed \(RustRight\) but `rustright` is not reachable from this shell/);
  // An unmanaged host (a cloud container, a CI runner): direct cargo, the same scans.
  const metadata = meta(root, ".", [["a", "crates/a"], ["b", "crates/b", []]]);
  const base = repoSpawn(root, { cargoLines: "" });
  const direct = capture();
  const spawn = (cmd, args, opts) => (cmd === "cargo" && args[0] === "metadata" ? ok(metadata) : base(cmd, args, opts));
  assert.equal(await main(["--plan"], { ...direct.io, spawn, cwd: root, env: {} }), 0, direct.err.join("\n"));
  assert.match(direct.out.join("\n"), /\[B\] clippy:a :: cargo clippy -p a /, "the same compile scans, through plain cargo");
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
      if (wholeListing(args)) return ok(fullListing(root, tracked, locks));
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
    if (cmd === "git" && (args[0] === "ls-files" || args[0] === "ls-tree")) return listed === null ? no("not listed") : ok(listed.map((f) => `${f}\0`).join(""));
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

test("check is the full deterministic stage: a class A finding (here a broken package.json committed long ago) makes it red even when the app check is green", async (t) => {
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-classa-")));
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.mkdirSync(path.join(repo, "conf"));
  fs.writeFileSync(path.join(repo, "conf/settings.json"), "{ not json");
  git("add", "-A"); git("commit", "-q", "-m", "old broken file");
  fs.writeFileSync(path.join(repo, "README.md"), "later change\n");
  git("add", "-A"); git("commit", "-q", "-m", "unrelated");
  const appCheck = async () => 0;
  const red = capture();
  assert.equal(await main(["check", "--repo", repo, "--commit", "HEAD"], { ...red.io, appCheck }), 1, red.out.join("\n"));
  assert.match(red.out.join("\n"), /fail +json-parse/, "the whole tree is scanned, not the last commit's diff");
  assert.match(red.err.join("\n"), /Class A is red: nothing compiles until it is green\./);
  fs.writeFileSync(path.join(repo, "conf/settings.json"), "{}\n");
  git("commit", "-q", "-am", "fixed");
  const green = capture();
  assert.equal(await main(["check", "--repo", repo, "--commit", "HEAD"], { ...green.io, appCheck }), 0, green.err.join("\n"));
  assert.match(green.out.at(-1), /class A pass/);
});

// ---- class A ShellCheck: SellRight's own scripts through the real check, and a refused analyzer version ----------------------
// fixtures/shellcheck-sellright/README.md: the original bytes (git blob ids below), their licence and provenance.
const SC_FIXTURES = new URL("./fixtures/shellcheck-sellright/", import.meta.url);
const SC_CASES = [
  { file: "sellright-a09236e-parent.sh.fixture", at: "deploy/sellright.sh", blob: "e856d8c974fa35a44c5173baa1ffa3df2c5b576c", red: ["SC2030", "SC2031", "SC2154"] },
  { file: "sellright-a09236e.sh.fixture", at: "deploy/sellright.sh", blob: "234308df35d076032a7cf2df04f986e153736d50", red: [] },
  { file: "sellright-6c3faed-parent.sh.fixture", at: "deploy/sellright.sh", blob: "07af9d2a39fa54241aca9e0c4e4684f1a108e074", red: ["SC2016"] },
  { file: "sellright-6c3faed.sh.fixture", at: "deploy/sellright.sh", blob: "9fa44bf81ee74de9edb34869b3cd2c6450052d27", red: [] },
  { file: "test-installer-image-binding-059fe34-parent.sh.fixture", at: "deploy/tests/test-installer-image-binding.sh", blob: "3811ed592b1de00dea9a19dc001ea52a1d6ee362", red: ["SC2016", "SC2329"] },
  { file: "test-installer-image-binding-059fe34.sh.fixture", at: "deploy/tests/test-installer-image-binding.sh", blob: "a5db74a35654e22673d6515a57fdd0af04087197", red: [] },
];
const scBytes = (c) => fs.readFileSync(new URL(c.file, SC_FIXTURES));
const SC_PIN = PINNED_SCAN_TOOLS.shellcheck;
const SC_FOUND = (() => { const r = spawnSync("shellcheck", ["--version"], { encoding: "utf8" }); return r.status === 0 ? (/^version:\s*(\S+)/m.exec(String(r.stdout))?.[1] ?? "unreadable") : null; })();
// One commit holding one script at its SellRight path, checked by `rightkit-gate check` (real class A, real runScans) with
// `binDir` first on PATH. The app check is not under test here.
async function checkScript(t, bytes, at, binDir = null) {
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-sc-")));
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.mkdirSync(path.join(repo, path.dirname(at)), { recursive: true });
  fs.writeFileSync(path.join(repo, at), bytes);
  git("add", "-A"); git("commit", "-q", "-m", "script");
  const saved = process.env.PATH;
  if (binDir) process.env.PATH = `${binDir}${path.delimiter}${saved}`;
  try {
    const c = capture();
    const code = await main(["check", "--repo", repo, "--commit", "HEAD"], { ...c.io, appCheck: async () => 0 });
    return { code, out: c.out.join("\n"), err: c.err.join("\n") };
  } finally { process.env.PATH = saved; }
}

test("class A ShellCheck fixtures are SellRight's committed bytes (git blob ids), not rewrites", () => {
  for (const c of SC_CASES) {
    const bytes = scBytes(c);
    assert.equal(crypto.createHash("sha1").update(`blob ${bytes.length}\0`).update(bytes).digest("hex"), c.blob, c.file);
  }
});

test("class A ShellCheck (the pinned analyzer): SellRight's original negatives are red, its repairs and the disable-comment positive control are green", { skip: SC_FOUND !== SC_PIN && `the pinned ShellCheck ${SC_PIN} is not on PATH (found ${SC_FOUND ?? "none"}); the version refusal test below still runs` }, async (t) => {
  for (const c of SC_CASES) {
    const r = await checkScript(t, scBytes(c), c.at);
    // The progress line keeps 300 characters of the finding; the BLOCKED list carries all of it.
    const found = [...new Set(`${r.out}\n${r.err}`.match(/SC\d{4}/g) ?? [])].sort();
    if (c.red.length) {
      assert.equal(r.code, 1, `${c.file} is red\n${r.out}\n${r.err}`);
      assert.match(r.out, /fail +shellcheck/, c.file);
      for (const code of c.red) assert.ok(found.includes(code), `${c.file} reports ${code} (found ${found.join(",")})`);
    } else {
      assert.equal(r.code, 0, `${c.file} is green\n${r.out}\n${r.err}`);
      assert.match(r.out, /pass +shellcheck/, c.file);
      assert.deepEqual(found, [], c.file);
    }
  }
});

test("class A ShellCheck refuses another analyzer version (TOOL_VERSION, found and required named), even one that would pass every script", { skip: process.platform === "win32" && "a POSIX shell script stands in for shellcheck" }, async (t) => {
  const bin = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-fake-shellcheck-")));
  t.after(() => fs.rmSync(bin, { recursive: true, force: true }));
  // Answers --version as ShellCheck 0.10.0 does and finds nothing in any script: a pass from it would be a pass the pin forbids.
  fs.writeFileSync(path.join(bin, "shellcheck"), '#!/bin/sh\nif [ "$1" = "--version" ]; then printf \'ShellCheck - shell script analysis tool\\nversion: 0.10.0\\nlicense: GNU General Public License, version 3\\n\'; fi\nexit 0\n');
  fs.chmodSync(path.join(bin, "shellcheck"), 0o755);
  for (const c of [SC_CASES[1], SC_CASES[5], SC_CASES[0]]) {
    const r = await checkScript(t, scBytes(c), c.at, bin);
    assert.equal(r.code, 1, `${c.file} is refused\n${r.out}\n${r.err}`);
    assert.match(r.out, /fail +shellcheck .*TOOL_VERSION: shellcheck found "shellcheck 0\.10\.0", class A requires shellcheck 0\.11\.0/, c.file);
    assert.match(r.err, /Class A is red: nothing compiles until it is green\./);
    assert.doesNotMatch(r.out, /pass +shellcheck|deferred +shellcheck/);
  }
});

test("check delegates to the app check with its own arguments, label and cwd and reads no stdin (class A stubbed off)", async () => {
  const seen = [];
  const appCheck = async (argv, io) => { seen.push({ argv, label: io.label, cwd: io.cwd, hasOut: typeof io.out === "function" && typeof io.err === "function" }); return seen.length === 1 ? 1 : 2; };
  const spawn = () => { throw new Error("check must not spawn anything itself"); };
  const readStdin = () => { throw new Error("check must not read stdin"); };
  const c = capture();
  assert.equal(await main(["check", "--repo", "/r", "--commit", "abc", "--only", "qa-platform,skip-checks", "--print-baseline"], { ...c.io, appCheck, spawn, readStdin, cwd: "/work" }), 1);
  assert.equal(await main(["check", "--worktree"], { ...c.io, appCheck, spawn, readStdin, cwd: "/work", classA: false }), 2);
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
  // A valid package (licence and lockfile): class A, which check now runs too, is green, so only the app check decides.
  fs.writeFileSync(path.join(repo, "pnpm-lock.yaml"), "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\n\nimporters:\n\n  .: {}\n");
  fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ license: "MIT", scripts: { ship: "right-release build --skip-checks" } }));
  git("add", "-A"); git("commit", "-q", "-m", "red");
  const red = capture();
  assert.equal(await main(["check", "--repo", repo, "--commit", "HEAD"], red.io), 1);
  assert.match(red.out.join("\n"), /^FAIL skip-checks \(G17\): package\.json:1 script "ship" runs right-release with --skip-checks$/m);
  assert.match(red.out.join("\n"), /^check: 1 problem\(s\) in \w{12} \(48 rules, /m);
  fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ license: "MIT", scripts: { ship: "right-release build" } }));
  git("commit", "-q", "-am", "green");
  const green = capture();
  assert.equal(await main(["check"], { ...green.io, cwd: repo }), 0, green.err.join("\n"));
  assert.match(green.out.join("\n"), /^check: \w{12} passed 48 rules in [\d.]+s$/m, "no --repo: the git top-level of the working directory");
  assert.match(green.out.at(-1), /^rightkit-gate: class A pass: \d+ deterministic scan\(s\) over \d+ file\(s\) of HEAD$/, "then every class A scan over the whole commit");
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
    "package.json": JSON.stringify({ name: "app", license: "MIT", devDependencies: { "@rightkit/release": "0.6.5" }, scripts: { ship: "right-release build --skip-checks" } }),
    "pnpm-lock.yaml": "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\n\nimporters:\n\n  .:\n    devDependencies:\n      '@rightkit/release':\n        specifier: 0.6.5\n        version: 0.6.5\n\npackages:\n\n  '@rightkit/release@0.6.5':\n    resolution: {integrity: sha512-s2PA4qjj6q13RpWaRW4r12///Mj34IR+8DqXwCOVuqbQcZ3MsX7Zlyb6u/VWlVnwfKIwpt7/jN4yzOvPvyd+pA==}\n    hasBin: true\n\n  smol-toml@1.9.1:\n    resolution: {integrity: sha512-j7WyPdf0CJoutMzMoPLF/bVXHtKZIT9Rb4eGeEbWw8X2t5oirjPgpL+OpaENZrILYaXqYwnVH+qgUwVbl+pUJQ==}\n    engines: {node: '>= 18'}\n\nsnapshots:\n\n  '@rightkit/release@0.6.5':\n    dependencies:\n      smol-toml: 1.9.1\n\n  smol-toml@1.9.1: {}\n",
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
  fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "app", license: "MIT", devDependencies: { "@rightkit/release": "0.6.5" }, scripts: { ship: "right-release build" } }));
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
      if (wholeListing(args)) return ok(fullListing(root, [...manifests, ...Object.values(tracked).flat()]));
      if (args[0] === "ls-files") {
        const named = tracked[path.posix.basename(args.at(-1))]; // `tracked`: other tracked files by name ({ "package.json": [...] })
        return ok(named ? `${named.join("\0")}\0` : args.at(-1).endsWith("Cargo.toml") ? `${manifests.join("\0")}\0` : "");
      }
      if (args[0] === "diff") return ok(diff);
      if (args[0] === "status" || args[0] === "archive" || args[0] === "read-tree") return ok();
      if (args[0] === "checkout-index") { const prefix = args.find((x) => x.startsWith("--prefix=")).slice("--prefix=".length); fs.cpSync(root, prefix, { recursive: true }); return ok(); }
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

test("the installed pre-push hook with real git: a rev expression holding a space is refused whether or not the repository has Rust", { skip: !shAvailable && "no sh" }, async (t) => {
  // git sends the local field as typed: `HEAD^{/one thing}` and `HEAD@{1 minute ago}` make a line of more than four fields, which was
  // dropped (and its pushed-commit-is-HEAD check with it).
  for (const [name, files] of [["a Rust repository", { "Cargo.toml": pkg("app"), "Cargo.lock": "version = 4\n", "src/lib.rs": "pub fn f() {}\n" }], ["a repository with no Cargo.toml", { "docs/a.md": "# a\n" }]]) {
    const repo = realHookRepo(t, { files, message: "one thing" });
    if (!repo) return;
    // A well-formed push passes; on a host without the broker a Rust repository now compiles directly (it no longer defers), which
    // needs the whole toolchain, so the well-formed push is shown on the repository with no Cargo.toml. The refusal below happens
    // before any cargo runs, for both.
    if (!name.startsWith("a Rust")) {
      const plain = repo.git("push", "origin", "HEAD:refs/heads/claude/plain");
      assert.equal(plain.status, 0, `${name}: ${plain.stderr}`);
    }
    for (const rev of ["HEAD^{/one thing}", "HEAD@{1 minute ago}"]) {
      const pushed = repo.git("push", "origin", `${rev}:refs/heads/claude/spaced`);
      assert.notEqual(pushed.status, 0, `${name}, ${rev}: the push is refused`);
      assert.match(pushed.stderr, /BLOCKED: pre-push stdin line 1 is not "<local ref> <local sha> <remote ref> <remote sha>" \(four fields separated by single spaces\)/, `${name}, ${rev}`);
      assert.doesNotMatch(repo.remoteRefs(), /claude\/spaced/, `${name}, ${rev}: nothing reached the remote`);
    }
  }
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


// ---- the one door: rightkit-gate --to <stage> ----------------------------------------------------------------------------------

import { requireDoorRun, stagesTo, STAGES } from "./gate-stages.mjs";

function stageRepo(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-stages-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(root, "Cargo.lock"), "version = 4\n");
  git("add", "-A"); git("commit", "-q", "-m", "app");
  return { root, head: git("rev-parse", "HEAD").stdout.trim() };
}
const directExecutor = () => ({ kind: "direct", cargo: ["cargo"] });

test("--to runs every stage up to the target in order, class A first, and stops at the first failure with nothing after it", async (t) => {
  assert.deepEqual(STAGES, ["check", "compile", "package", "qualify", "release"]);
  assert.deepEqual(stagesTo("compile"), ["check", "compile"]);
  assert.equal(stagesTo("deploy"), null);
  const { root } = stageRepo(t);
  const ran = [];
  const c = capture();
  const code = await main(["--to", "compile"], { ...c.io, cwd: root, resolveExecutor: directExecutor, runCheck: async () => { ran.push("check"); return 1; }, runCompile: async () => { ran.push("compile"); return 0; } });
  assert.equal(code, 1);
  assert.deepEqual(ran, ["check"], "a red class A stops the run: compile never starts");
  assert.match(c.err.join("\n"), /--to compile stopped at check: nothing after it ran/);
});

test("--to reuses check and compile only for the exact same inputs, and needs --platform / --tier where they matter", async (t) => {
  const { root } = stageRepo(t);
  const ran = [];
  const io = { resolveExecutor: directExecutor, runCheck: async () => { ran.push("check"); return 0; }, runCompile: async () => { ran.push("compile"); return 0; } };
  const first = capture();
  assert.equal(await main(["--to", "compile"], { ...first.io, cwd: root, ...io }), 0, first.err.join("\n"));
  const again = capture();
  assert.equal(await main(["--to", "compile"], { ...again.io, cwd: root, ...io }), 0);
  assert.deepEqual(ran, ["check", "compile"], "the second run reuses both: same commit, tree, lockfiles, gate and toolchain");
  assert.match(again.out.join("\n"), /\[check\] already passed for this exact input/);
  fs.writeFileSync(path.join(root, "Cargo.lock"), "version = 4\n# changed\n");
  spawnSync("git", ["commit", "-qam", "lock change"], { cwd: root });
  assert.equal(await main(["--to", "compile"], { ...capture().io, cwd: root, ...io }), 0);
  assert.deepEqual(ran, ["check", "compile", "check", "compile"], "a new commit (here a lockfile change) runs both again");
  const noPlatform = capture();
  assert.equal(await main(["--to", "package"], { ...noPlatform.io, cwd: root, ...io }), 2);
  assert.match(noPlatform.err.join("\n"), /--to package needs --platform mac\|win/);
  const noTier = capture();
  assert.equal(await main(["--to", "release", "--platform", "win"], { ...noTier.io, cwd: root, ...io }), 2);
  assert.match(noTier.err.join("\n"), /--to release needs --tier patch\|update \(chosen explicitly, never defaulted\)/);
  const dirty = capture();
  fs.writeFileSync(path.join(root, "Cargo.lock"), "edited\n");
  assert.equal(await main(["--to", "check"], { ...dirty.io, cwd: root, ...io }), 1);
  assert.match(dirty.err.join("\n"), /the checkout differs from HEAD; every stage works on the commit/);
});

test("--to release: package seals, qualify and release use exactly that sealed set, under a live door run; a changed seal blocks", async (t) => {
  const { root, head } = stageRepo(t);
  const calls = []; let sealedDigest = "d".repeat(64);
  const io = {
    resolveExecutor: directExecutor, runCheck: async () => 0, runCompile: async () => 0,
    sealedFor: () => ({ id: `app-1.0.0-${head.slice(0, 8)}`, manifest: "m", digest: sealedDigest }),
    spawnInherit: (cmd, a, opts) => {
      const door = requireDoorRun({ env: opts.env, commonDir: path.join(root, ".git"), commit: head, command: "x" });
      calls.push({ args: a.slice(1).join(" "), door });
      return { status: 0 };
    },
  };
  const publisher = { ...process.env, GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: "github-environment" };
  // The publisher only uploads: with nothing qualified yet it refuses, running no stage at all.
  const early = capture();
  assert.equal(await main(["--to", "release", "--platform", "win", "--tier", "patch"], { ...early.io, cwd: root, ...io, env: publisher }), 1);
  assert.match(early.err.join("\n"), /--to release only uploads/);
  assert.deepEqual(calls, [], "no build or QA ran in the publisher");
  const host = capture();
  assert.equal(await main(["--to", "qualify", "--platform", "win"], { ...host.io, cwd: root, ...io }), 0, host.err.join("\n"));
  const c = capture();
  assert.equal(await main(["--to", "release", "--platform", "win", "--tier", "patch"], { ...c.io, cwd: root, ...io, env: publisher }), 0, c.err.join("\n"));
  assert.deepEqual(calls.map((x) => x.args), [
    "qa --stage source --platform win", "build --platform win",
    `qa --stage installed --platform win --release app-1.0.0-${head.slice(0, 8)}`,
    `upload --release app-1.0.0-${head.slice(0, 8)} --platform win --tier patch`,
  ]);
  assert.ok(calls.every((x) => x.door === null), "each right-release stage ran inside a live door run for HEAD");
  assert.equal(requireDoorRun({ env: { RK_GATE_RUN: "0".repeat(32) }, commonDir: path.join(root, ".git"), commit: head })?.includes("names no live door run"), true, "the run file is gone after the stage");
  const receipt = JSON.parse(fs.readFileSync(path.join(root, ".git", "rightkit-gate", "receipts", `${head}.json`), "utf8"));
  assert.deepEqual(Object.keys(receipt.stages), ["check", "compile", "package", "qualify", "release"]);
  assert.equal(receipt.stages.release.manifestSha256, "d".repeat(64), "the released set is the packaged and qualified one");
  // The sealed set changes after qualification: the publisher refuses it rather than rebuild or requalify.
  sealedDigest = "e".repeat(64); calls.length = 0;
  const swapped = capture();
  assert.equal(await main(["--to", "release", "--platform", "win", "--tier", "patch"], { ...swapped.io, cwd: root, ...io, env: publisher }), 1);
  assert.match(swapped.err.join("\n"), /--to release only uploads/);
  assert.deepEqual(calls, [], "nothing rebuilt, requalified or uploaded");
  // On the host, a changed sealed set is packaged again before it is qualified.
  const changed = capture();
  assert.equal(await main(["--to", "qualify", "--platform", "win"], { ...changed.io, cwd: root, ...io, sealedFor: () => ({ id: `app-1.0.0-${head.slice(0, 8)}`, manifest: "m", digest: sealedDigest }), spawnInherit: () => ({ status: 0 }) }), 0, "package runs again and binds the new seal");
});

test("--to release refuses outside the trusted publisher: a host or agent never writes R2", async (t) => {
  const { root } = stageRepo(t);
  const ran = [];
  const io = { resolveExecutor: directExecutor, runCheck: async () => { ran.push("check"); return 0; }, runCompile: async () => 0, spawnInherit: () => { ran.push("right-release"); return { status: 0 }; } };
  for (const env of [{ ...process.env, GITHUB_ACTIONS: undefined, RIGHTKIT_PUBLISHER: undefined }, { ...process.env, GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: undefined }, { ...process.env, GITHUB_ACTIONS: undefined, RIGHTKIT_PUBLISHER: "github-environment" }]) {
    const c = capture();
    assert.equal(await main(["--to", "release", "--platform", "win", "--tier", "patch"], { ...c.io, cwd: root, ...io, env }), 1);
    assert.match(c.err.join("\n"), /runs only in the trusted publisher/);
  }
  assert.deepEqual(ran, [], "nothing ran before the refusal");
});

test("--receipt-out / --receipt-in: a later job on another runner takes the sealed stages and does not rebuild; a foreign receipt is refused", async (t) => {
  const { root, head } = stageRepo(t);
  const id = `app-1.0.0-${head.slice(0, 8)}`;
  const calls = [];
  const io = (executor) => ({
    resolveExecutor: () => ({ kind: executor, cargo: ["cargo"] }), runCheck: async () => { calls.push("check"); return 0; }, runCompile: async () => { calls.push("compile"); return 0; },
    sealedFor: () => ({ id, manifest: "m", digest: "d".repeat(64) }),
    spawnInherit: (cmd, a) => { calls.push(a.slice(1).join(" ")); return { status: 0 }; },
  });
  const out = path.join(root, "..", `receipt-${path.basename(root)}.json`);
  t.after(() => fs.rmSync(out, { force: true }));
  const pack = capture();
  assert.equal(await main(["--to", "package", "--platform", "win", "--receipt-out", out], { ...pack.io, cwd: root, ...io("direct") }), 0, pack.err.join("\n"));
  assert.deepEqual(calls, ["check", "compile", "qa --stage source --platform win", "build --platform win"]);
  // The qualification runner: a fresh receipt store, another executor; the imported receipt binds the same source and sealed bytes.
  fs.rmSync(path.join(root, ".git", "rightkit-gate"), { recursive: true, force: true });
  calls.length = 0;
  const qual = capture();
  assert.equal(await main(["--to", "qualify", "--platform", "win", "--receipt-in", out], { ...qual.io, cwd: root, ...io("broker") }), 0, qual.err.join("\n"));
  assert.deepEqual(calls, [`qa --stage installed --platform win --release ${id}`], "check, compile and package stand on the sealed set; only qualify runs");
  // The publisher: another runner again (ubuntu, no installer can run there); qualify is taken from the receipt for these bytes.
  const qualified = path.join(root, "..", `receipt-q-${path.basename(root)}.json`);
  t.after(() => fs.rmSync(qualified, { force: true }));
  fs.copyFileSync(path.join(root, ".git", "rightkit-gate", "receipts", `${head}.json`), qualified);
  fs.rmSync(path.join(root, ".git", "rightkit-gate"), { recursive: true, force: true });
  calls.length = 0;
  const pub = capture();
  assert.equal(await main(["--to", "release", "--platform", "win", "--tier", "patch", "--receipt-in", qualified], { ...pub.io, cwd: root, ...io("direct"), env: { ...process.env, GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: "github-environment" } }), 0, pub.err.join("\n"));
  assert.deepEqual(calls, [`upload --release ${id} --platform win --tier patch`], "the publisher only uploads the qualified sealed set");
  // A receipt for another commit, or with another gate, is not taken.
  const foreign = JSON.parse(fs.readFileSync(out, "utf8"));
  fs.writeFileSync(out, JSON.stringify({ ...foreign, binding: { ...foreign.binding, gate: "0".repeat(64) } }));
  const bad = capture();
  assert.equal(await main(["--to", "qualify", "--platform", "win", "--receipt-in", out], { ...bad.io, cwd: root, ...io("broker") }), 1);
  assert.match(bad.err.join("\n"), /is not a rightkit\.gate-receipt\/1 receipt/);
});

test("stage hands only a qualified, unchanged sealed set to the publisher's draft release, then dispatches release.yml", async (t) => {
  const { root, head } = stageRepo(t);
  const id = `app-1.0.0-${head.slice(0, 8)}`;
  let digest = "d".repeat(64);
  const external = [];
  const spawn = (cmd, a, opts) => {
    if (cmd === "gh" || cmd === "tar") { external.push(`${cmd} ${a[0]} ${a[1]}`); return { status: cmd === "gh" && a[0] === "release" && a[1] === "view" ? 1 : 0, stdout: a[0] === "repo" ? "bogusyogi/app\n" : "", stderr: "" }; }
    return spawnSync(cmd, a, { ...opts, encoding: "utf8" });
  };
  const io = { spawn, resolveExecutor: directExecutor, runCheck: async () => 0, runCompile: async () => 0, sealedFor: () => ({ id, manifest: "m", digest }), spawnInherit: () => ({ status: 0 }) };
  const early = capture();
  assert.equal(await main(["stage", "--platform", "win"], { ...early.io, cwd: root, ...io }), 1, "nothing qualified yet");
  assert.match(early.err.join("\n"), /run rightkit-gate --to qualify --platform win first/);
  assert.equal(await main(["--to", "qualify", "--platform", "win"], { ...capture().io, cwd: root, ...io }), 0);
  const ok = capture();
  assert.equal(await main(["stage", "--platform", "win", "--tier", "patch"], { ...ok.io, cwd: root, ...io }), 0, ok.err.join("\n"));
  assert.deepEqual(external, [`tar -czf ${external[0].split(" ")[2]}`, `gh release view`, `gh release create`, `gh release upload`, `gh repo view`, `gh workflow run`]);
  // The sealed bytes change after qualification: nothing is staged.
  digest = "e".repeat(64); external.length = 0;
  assert.equal(await main(["stage", "--platform", "win"], { ...capture().io, cwd: root, ...io }), 1);
  assert.deepEqual(external, []);
});

test("--to dev|test: class A on the working tree first, then right-release dev|test under a live door run with the passed arguments", async (t) => {
  const { root, head } = stageRepo(t);
  fs.writeFileSync(path.join(root, "uncommitted.txt"), "work in progress\n");
  const calls = [];
  const io = (checkCode) => ({
    resolveExecutor: directExecutor, runCheck: async () => { calls.push("check"); return checkCode; },
    spawnInherit: (cmd, a, opts) => { calls.push({ args: a.slice(1).join(" "), door: requireDoorRun({ env: opts.env, commonDir: path.join(root, ".git"), commit: head, command: "x" }) }); return { status: 0 }; },
  });
  const red = capture();
  assert.equal(await main(["--to", "dev", "--", "--platform", "mac"], { ...red.io, cwd: root, ...io(1) }), 1);
  assert.deepEqual(calls, ["check"], "class A red: nothing compiles");
  calls.length = 0;
  const green = capture();
  assert.equal(await main(["--to", "dev", "--", "--platform", "mac"], { ...green.io, cwd: root, ...io(0) }), 0, green.err.join("\n"));
  assert.deepEqual(calls, ["check", { args: "dev --platform mac", door: null }], "a dirty tree is fine for dev; it ran inside a live door run");
  calls.length = 0;
  assert.equal(await main(["--to", "test"], { ...capture().io, cwd: root, ...io(0) }), 0);
  assert.deepEqual(calls.map((c) => c.args ?? c), ["check", "test"]);
});

test("class A of a commit scans every file of that commit: export-ignore hides nothing, and a live edit is never what is checked", async (t) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-snapshot-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(root, ".gitattributes"), "hidden.json export-ignore\n");
  fs.writeFileSync(path.join(root, "hidden.json"), "{ not json\n");
  fs.writeFileSync(path.join(root, "good.json"), "{}\n");
  git("add", "-A"); git("commit", "-q", "-m", "x");
  const run = async (args) => { const c = capture(); const code = await main(args, { ...c.io, cwd: root, resolveExecutor: directExecutor }); return { code, out: [...c.out, ...c.err].join("\n") }; };
  const committed = await run(["--class-a", "--commit", "HEAD"]);
  assert.equal(committed.code, 1, committed.out);
  assert.match(committed.out, /hidden\.json/, "a file marked export-ignore is still scanned");
  // Fix it in the commit, then break it only in the live checkout: the commit is what is checked.
  fs.writeFileSync(path.join(root, "hidden.json"), "{}\n"); git("commit", "-q", "-am", "fix");
  fs.writeFileSync(path.join(root, "good.json"), "{ broken live edit\n");
  const clean = await run(["--class-a", "--commit", "HEAD"]);
  assert.equal(clean.code, 0, clean.out);
  assert.doesNotMatch(clean.out, /good\.json/);
});

test("the snapshot export lives on a disk-policy scratch root where one exists: RK_GATE_TMPDIR wins, os.tmpdir() is the fallback (rightkit#1215)", () => {
  const policy = { scratchRoots: { mac: ["/Volumes/D/scratch"], win: ["D:\\scratch"] } };
  assert.equal(exportTmpRoot({ env: { RK_GATE_TMPDIR: "/x/override" }, platform: "darwin", exists: () => true, policy }), "/x/override");
  assert.equal(exportTmpRoot({ env: {}, platform: "darwin", exists: (p) => p === "/Volumes/D/scratch", policy }), "/Volumes/D/scratch");
  assert.equal(exportTmpRoot({ env: {}, platform: "win32", exists: (p) => p === "D:\\scratch", policy }), "D:\\scratch");
  assert.equal(exportTmpRoot({ env: {}, platform: "darwin", exists: () => false, policy }), os.tmpdir(), "no scratch root: the system temp dir");
});

test("the snapshot export carries the checkout's origin in a config-only .git, so rustright can bind the local-build grant to it (rightkit#1215)", async (t) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-origin-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(root, "good.json"), "{}\n");
  fs.writeFileSync(path.join(root, ".rightkit-local-build.json"), JSON.stringify({ repository: "Orthic-Labs/x", allow: "build-test-publish" }));
  git("add", "-A"); git("commit", "-q", "-m", "x");
  const writes = [];
  const run = async () => {
    const c = capture();
    const io = { ...c.io, cwd: root, resolveExecutor: directExecutor, writeFile: (f, d) => { writes.push([norm(f), d]); fs.writeFileSync(f, d); } };
    const code = await main(["--class-a", "--commit", "HEAD"], io);
    return { code, out: [...c.out, ...c.err].join("\n") };
  };
  // No remote: nothing is written into the export.
  const bare = await run();
  assert.equal(bare.code, 0, bare.out);
  assert.ok(!writes.some(([f]) => f.endsWith("/repo/.git/config")), "a checkout without an origin exports no .git");
  // With a remote: the export's .git holds only a config naming that origin, and git does not see a repository there.
  git("remote", "add", "origin", "git@github.com:Orthic-Labs/x.git");
  const seen = { dir: null, isRepo: null };
  const withOrigin = await (async () => {
    const c = capture();
    const io = { ...c.io, cwd: root, resolveExecutor: directExecutor, writeFile: (f, d) => {
      fs.writeFileSync(f, d);
      if (norm(f).endsWith("/repo/.git/config")) {
        seen.dir = fs.readdirSync(path.dirname(f));
        seen.config = d;
        seen.isRepo = spawnSync("git", ["rev-parse", "--git-dir"], { cwd: path.dirname(path.dirname(f)), encoding: "utf8", env: { ...process.env, GIT_CEILING_DIRECTORIES: os.tmpdir() } }).status === 0;
      }
    } };
    const code = await main(["--class-a", "--commit", "HEAD"], io);
    return { code, out: [...c.out, ...c.err].join("\n") };
  })();
  assert.equal(withOrigin.code, 0, withOrigin.out);
  assert.deepEqual(seen.dir, ["config"], "the export's .git holds only the config");
  assert.equal(seen.config, '[remote "origin"]\n\turl = git@github.com:Orthic-Labs/x.git\n');
  assert.equal(seen.isRepo, false, "a config-only .git is not a repository to git");
});

// ---- B5: the clean-checkout check covers every linter config the scans can read ---------------------------------------------------------------------------

// git status answers from a fixed list of dirty files, filtered by the pathspecs it is given as git would: a spec names a file or a directory.
const dirtyAware = (base, dirty) => (cmd, args, opts) => {
  if (cmd === "git" && args[0] === "status") {
    const specs = args.slice(args.indexOf("--") + 1);
    return ok(dirty.filter((f) => specs.some((s) => f === s || f.startsWith(`${s}/`))).map((f) => ` M ${f}\n`).join(""));
  }
  return base(cmd, args, opts);
};
async function dirtyGate(t, dirty, files = {}) {
  const root = mkRepo(stripUndefined(jsRepo(files)), t);
  const seen = []; const c = capture();
  const code = await main(["--base", "origin/main"], {
    ...c.io, ...noWrites, cwd: root, env: {}, readStdin: () => pushLine("refs/heads/claude/port"), runScans: fakeRun(seen), findRustfmt: () => "/x/rustfmt",
    spawn: dirtyAware(pushSpawn(root, { manifests: [], tracked: trackedJs, diff: "apps/coderight-web/src/b.ts\napps/coderight-tauri/src/a.ts\n" }), dirty),
  });
  return { code, seen, out: c.out.join("\n"), err: c.err.join("\n") };
}

// ---- B1: an app declares that a unit's test needs the broker (ci/gate.config.json npm.<unit>.requiresBroker) -----------------------------------------------------

// HeardRight's root test is `node -e` over `verify`, which is `pnpm --dir tauri-app-next run verify`, which is `node ./scripts/verify-local.mjs`, which runs
// `rustright cargo test`: no part of package.json says so, and scriptBrokerCall reads package.json only.
// The module name is joined, not written: background-process.test.mjs forbids a quoted child_process token in any release source or test, and this fixture is only text for a script field.
const OPAQUE_TEST = `node -e "const{spawnSync}=require('${["node:child", "process"].join("_")}');process.exit(spawnSync('pnpm',['run','verify'],{stdio:'inherit'}).status)"`;
const opaque = { name: "viewright", license: "MIT", scripts: { test: OPAQUE_TEST, verify: "pnpm --dir tauri-app-next run verify" } };
const REASON = "the root test runs tauri-app-next/scripts/verify-local.mjs, which spawns rustright cargo test";
const declaring = (requiresBroker, scripts = opaque.scripts) => gateFiles({
  "package.json": JSON.stringify({ ...opaque, scripts }), "src/app.ts": "",
  "ci/gate.config.json": JSON.stringify({ npm: { viewright: { requiresBroker } } }),
});

// ---- B2: install-hook in a repository whose root does not install @rightkit/release ---------------------------------------------------------------------------

// CutRight's apps/cutright and HeardRight's tauri-app-next declare and install the package; the root declares nothing. The hook's last resort used to be the path
// of the gate that wrote it: `../../../../../../home/user/rightkit-exports/packages/release/bin/rightkit-gate.mjs` from an exports checkout, and
// `apps/cutright/node_modules/.pnpm/@rightkit+release@0.6.4/node_modules/@rightkit/release/bin/rightkit-gate.mjs` from the install (stale at the next bump).
const LINK = "node_modules/@rightkit/release/bin/rightkit-gate.mjs";
const declares = (extra = {}) => JSON.stringify({ name: "x", devDependencies: { "@rightkit/release": "0.6.5" }, ...extra });
const sha256Hex = (text) => crypto.createHash("sha256").update(text).digest("hex");
const PUBLISHED_HOOK_DIGESTS = JSON.parse(fs.readFileSync(new URL("./fixtures/published-gate-hooks.json", import.meta.url), "utf8")).releases;

async function installFrom(t, files, gateFile, listed = Object.keys(files).filter((f) => f.endsWith("package.json"))) {
  const root = mkRepo(files, t);
  const state = { hooksPath: "", sets: [] };
  const c = capture();
  const code = await main(["install-hook"], { ...c.io, spawn: hookEnv(root, state, { listed }), cwd: root, gateFile: typeof gateFile === "function" ? gateFile(root) : gateFile });
  return { code, root, out: c.out.join("\n"), err: c.err.join("\n"), hook: fs.readFileSync(path.join(root, ".githooks", "pre-push"), "utf8") };
}

test("hookGatePlan: a stable repository-relative path for the gate, from what the package.json files declare and never from what is installed", () => {
  const { hookGatePlan } = gateModule;
  const root = norm(path.resolve("/work/app"));
  const outside = norm(path.resolve("/elsewhere/exports/packages/release/bin/rightkit-gate.mjs"));
  const store = norm(path.join(root, "apps/cutright", "node_modules/.pnpm/@rightkit+release@0.6.4/node_modules/@rightkit/release/bin/rightkit-gate.mjs"));
  // Install state is not an input: a plan that reads it fails here.
  const exists = () => { throw new Error("hookGatePlan must not look at what is installed"); };
  const plan = (declaring, gateFile) => hookGatePlan({ root, gateFile, declaring, exists });
  // 1. the root declares it: its link and no extra packages, whatever else declares it (Pulse: the root and hub/; RightKit itself: the root)
  assert.deepEqual(plan(["."], outside), { nodePath: LINK, packages: [], unsafe: [] });
  assert.deepEqual(plan([".", "hub"], outside), { nodePath: LINK, packages: [], unsafe: [] }, "Pulse: the root form, the 0.6.4 and 0.6.5 hook");
  assert.deepEqual(plan([".", "apps/cutright"], outside), { nodePath: LINK, packages: [], unsafe: [] });
  assert.deepEqual(plan([".", "apps/cutright"], store), { nodePath: LINK, packages: [], unsafe: [] }, "run from a store copy: still the link");
  assert.deepEqual(plan(["."], norm(path.join(root, "packages/release/bin/rightkit-gate.mjs"))), { nodePath: LINK, packages: [], unsafe: [] }, "run from inside the repository: the root declares it, so its link");
  // 2. only packages declare it (CutRight's apps/cutright, HeardRight's tauri-app-next): the first of them in sorted order, every one searched
  assert.deepEqual(plan(["apps/cutright"], outside), { nodePath: `apps/cutright/${LINK}`, packages: ["apps/cutright"], unsafe: [] });
  assert.deepEqual(plan(["apps/cutright"], store), { nodePath: `apps/cutright/${LINK}`, packages: ["apps/cutright"], unsafe: [] }, "a store path is never written");
  assert.deepEqual(plan(["apps/cutright", "tauri-app-next"], outside), { nodePath: `apps/cutright/${LINK}`, packages: ["apps/cutright", "tauri-app-next"], unsafe: [] }, "the first declaring package, not whichever is installed");
  // 3. nothing declares it: the gate's own path when it runs from inside the repository and not from a node_modules (a checkout of the package), else the root's link
  assert.deepEqual(plan([], norm(path.join(root, "packages/release/bin/rightkit-gate.mjs"))), { nodePath: "packages/release/bin/rightkit-gate.mjs", packages: [], unsafe: [] });
  assert.deepEqual(plan([], outside), { nodePath: LINK, packages: [], unsafe: [] }, "never a path outside the repository");
  assert.deepEqual(plan([], store), { nodePath: LINK, packages: [], unsafe: [] }, "never a store path");
  // a directory that cannot be written into a shell script is named, never written
  const odd = plan(["apps/my app", "apps/$(x)", "-x", "apps/../up", "apps/a;b", "apps/ok"], outside);
  assert.deepEqual([odd.packages, odd.unsafe.sort()], [["apps/ok"], ["-x", "apps/$(x)", "apps/../up", "apps/a;b", "apps/my app"]]);
  assert.deepEqual(plan([".", "apps/a;b"], outside), { nodePath: LINK, packages: [], unsafe: ["apps/a;b"] }, "the root form still names the directories it leaves out");
  for (const bad of ["apps/$(x)", "a b", "a\"b", "a`b", "a;b", "-x", "", "..", "a/../b", "./a", "a//b", "/abs", "C:\\x", "a\nb"]) assert.equal(gateModule.hookPathIsPlain(bad), false, JSON.stringify(bad));
  for (const good of ["apps/cutright", "tauri-app-next", "packages/@scope/x.y", ".tools/app", "a+b/c_d-e"]) assert.equal(gateModule.hookPathIsPlain(good), true, good);
});

test("install-hook: with the install in a nested package, the hook finds the gate through that package's stable path, never one outside the repository or in the pnpm store", async (t) => {
  const { hookGatePlan } = gateModule;
  assert.equal(typeof hookGatePlan, "function");
  const files = { "apps/cutright/package.json": declares(), [`apps/cutright/${LINK}`]: "", "README.md": "x" };
  const store = (root) => path.join(root, "apps/cutright", "node_modules/.pnpm/@rightkit+release@0.6.4/node_modules/@rightkit/release/bin/rightkit-gate.mjs");
  const wanted = hookText(`apps/cutright/${LINK}`, ["apps/cutright"]);
  for (const [name, gateFile] of [["an exports checkout beside the repository", path.resolve("/home/user/rightkit-exports/packages/release/bin/rightkit-gate.mjs")], ["the pnpm store copy of the install", store], ["a relative symlink target", path.resolve("/x/node_modules/@rightkit/release/bin/rightkit-gate.mjs")]]) {
    const r = await installFrom(t, files, gateFile);
    assert.equal(r.code, 0, `${name}: ${r.err}`);
    assert.equal(r.hook, wanted, name);
    assert.doesNotMatch(r.hook, /\.\.\//, `${name}: nothing outside the repository`);
    assert.doesNotMatch(r.hook, /\.pnpm|@rightkit\+release@/, `${name}: no version-pinned store path`);
    assert.ok(!r.hook.includes(r.root) && !/[A-Za-z]:[\\/]/.test(r.hook), `${name}: no absolute path`);
    assert.match(r.hook, /^for bin in node_modules\/\.bin\/rightkit-gate apps\/cutright\/node_modules\/\.bin\/rightkit-gate \*\//m, name);
    assert.match(r.hook, /pnpm --dir apps\/cutright exec rightkit-gate "\$@"/, name);
    assert.match(r.hook, /node "apps\/cutright\/node_modules\/@rightkit\/release\/bin\/rightkit-gate\.mjs" "\$@"/, name);
    assert.match(r.out, /else of apps\/cutright \(the packages that declare @rightkit\/release\), else of any workspace package, else through `pnpm --dir apps\/cutright exec`/, name);
    assert.match(r.out, /@rightkit\/release is declared in apps\/cutright\/package\.json/, name);
  }
  // Running it again from the same checkout changes nothing; run from the other place it is the same hook, so still current.
  const first = await installFrom(t, files, path.resolve("/home/user/rightkit-exports/packages/release/bin/rightkit-gate.mjs"));
  const state = { hooksPath: ".githooks", sets: [] };
  const c = capture();
  assert.equal(await main(["install-hook"], { ...c.io, spawn: hookEnv(first.root, state, { listed: ["apps/cutright/package.json"] }), cwd: first.root, gateFile: store(first.root) }), 0);
  assert.ok(c.out.some((l) => /\.githooks\/pre-push already current/.test(l)), c.out.join("\n"));
  // Not installed yet (a fresh clone, pnpm install --ignore-scripts): the link the package will have.
  const fresh = await installFrom(t, { "apps/cutright/package.json": declares() }, path.resolve("/home/user/rightkit-exports/packages/release/bin/rightkit-gate.mjs"));
  assert.equal(fresh.hook, wanted);
  // Several packages declare it: all are searched, the first one is the last resort and runs under pnpm, whichever of them is installed.
  const declaringBoth = { "apps/cutright/package.json": declares(), "tauri-app-next/package.json": declares() };
  const manyWanted = hookText(`apps/cutright/${LINK}`, ["apps/cutright", "tauri-app-next"]);
  for (const [name, installed] of [["nothing installed", []], ["the second one installed", [`tauri-app-next/${LINK}`]], ["the first one installed", [`apps/cutright/${LINK}`]], ["both installed", [`apps/cutright/${LINK}`, `tauri-app-next/${LINK}`]]]) {
    const many = await installFrom(t, { ...declaringBoth, ...Object.fromEntries(installed.map((f) => [f, ""])) }, path.resolve("/elsewhere/rightkit-gate.mjs"));
    assert.equal(many.hook, manyWanted, name);
  }
});

test("install-hook: a root that installs the package keeps getting the published hook byte for byte, and RightKit's own checkout keeps its path", async (t) => {
  const outside = path.resolve("/home/user/rightkit-exports/packages/release/bin/rightkit-gate.mjs");
  for (const [name, files] of [
    ["the root installs it", { "package.json": declares(), [LINK]: "" }],
    ["the root installs it and a package declares it too", { "package.json": declares(), "apps/cutright/package.json": declares(), [LINK]: "", [`apps/cutright/${LINK}`]: "" }],
  ]) {
    const r = await installFrom(t, files, outside);
    assert.equal(r.hook, hookText(LINK), name);
    assert.equal(sha256Hex(r.hook), PUBLISHED_HOOK_DIGESTS["0.6.5"], `${name}: the hook the published 0.6.5 wrote, so the template's committed hook is still "already current"`);
  }
  const own = await installFrom(t, { "packages/release/package.json": '{"name":"@rightkit/release"}' }, (root) => path.join(root, "packages/release/bin/rightkit-gate.mjs"));
  assert.equal(own.hook, hookText("packages/release/bin/rightkit-gate.mjs"), "RightKit's own hook points at its checkout, as before");
  const none = await installFrom(t, { "package.json": '{"name":"app"}' }, outside);
  assert.equal(none.hook, hookText(LINK), "nothing declares it: the root's link, not a path outside the repository");
  assert.match(none.err, /WARNING: no package\.json in this repository depends on @rightkit\/release/);
});

test("install-hook: the hook depends on what package.json files declare, not on what is installed, so a prepare script never meets a hook it wrote", async (t) => {
  const outside = path.resolve("/home/user/rightkit-exports/packages/release/bin/rightkit-gate.mjs");
  const store = (root) => path.join(root, "node_modules/.pnpm/@rightkit+release@0.6.4/node_modules/@rightkit/release/bin/rightkit-gate.mjs");
  const shapes = [
    // Pulse: the root and hub/ both declare it
    ["the root and hub", { "package.json": declares(), "hub/package.json": declares() }, [LINK, `hub/${LINK}`], hookText(LINK)],
    ["the root and a package two levels down", { "package.json": declares(), "apps/cutright/package.json": declares() }, [LINK, `apps/cutright/${LINK}`], hookText(LINK)],
    // CodeRight: two packages, neither the root
    ["two packages, neither the root", { "apps/coderight-web/package.json": declares(), "apps/coderight-tauri/package.json": declares() }, [`apps/coderight-web/${LINK}`, `apps/coderight-tauri/${LINK}`], hookText(`apps/coderight-tauri/${LINK}`, ["apps/coderight-tauri", "apps/coderight-web"])],
    ["one package (CutRight)", { "apps/cutright/package.json": declares() }, [`apps/cutright/${LINK}`], hookText(`apps/cutright/${LINK}`, ["apps/cutright"])],
  ];
  for (const [name, declared, links, wanted] of shapes) {
    // Every subset of the links that can exist, from a fresh clone to everything installed, and every place the gate can run from, writes one hook.
    const states = [[], ...links.map((link) => [link]), links];
    for (const [where, gateFile] of [["an exports checkout", outside], ["a store copy", store]]) {
      for (const installed of states) {
        const r = await installFrom(t, { ...declared, ...Object.fromEntries(installed.map((f) => [f, ""])) }, gateFile);
        assert.equal(r.code, 0, `${name}, ${where}, installed ${JSON.stringify(installed)}: ${r.err}`);
        assert.equal(r.hook, wanted, `${name}, ${where}, installed ${JSON.stringify(installed)}`);
      }
    }
    // A hook written before the install is current after it: `install-hook` as a prepare script succeeds without --force.
    const first = await installFrom(t, declared, outside);
    for (const link of links) { fs.mkdirSync(path.dirname(path.join(first.root, link)), { recursive: true }); fs.writeFileSync(path.join(first.root, link), ""); }
    const again = capture();
    assert.equal(await main(["install-hook"], { ...again.io, spawn: hookEnv(first.root, { hooksPath: ".githooks", sets: [] }, { listed: Object.keys(declared) }), cwd: first.root, gateFile: outside }), 0, `${name}: ${again.err.join("\n")}`);
    assert.ok(again.out.some((l) => /\.githooks\/pre-push already current/.test(l)), `${name}: ${again.out.join("\n")}`);
    assert.doesNotMatch(again.err.join("\n"), /already exists and differs/, name);
  }
  assert.equal(sha256Hex(hookText(LINK)), PUBLISHED_HOOK_DIGESTS["0.6.5"], "the root form is the published 0.6.5 hook");
});

test("install-hook: a declaring package whose directory is not a plain name is reported and never written into the hook", async (t) => {
  const r = await installFrom(t, { "apps/my app/package.json": declares(), "apps/$(touch x)/package.json": declares(), "apps/ok/package.json": declares() }, path.resolve("/elsewhere/rightkit-gate.mjs"));
  assert.equal(r.code, 0, r.err);
  assert.match(r.err, /WARNING: the pre-push hook does not look for the gate in apps\/\$\(touch x\), apps\/my app: not a plain directory name/);
  assert.equal(r.hook, hookText(`apps/ok/${LINK}`, ["apps/ok"]));
  assert.ok(!r.hook.includes("touch") && !r.hook.includes("my app"));
});

test("the multi-root pre-push hook (real sh): a package deeper than the glob, the pnpm fallback in the declaring package, then node at the package's link", { skip: !shAvailable && "no sh" }, (t) => {
  const tmp = mkRepo({}, t);
  const log = path.join(tmp, "calls.log");
  const hook = (packages, nodePath) => ({ ".githooks/pre-push": hookText(nodePath, packages) });
  // 1. beyond the three levels the glob covers: found through the explicit entry
  const deep = hookRepo(t, { ...hook(["apps/a/b/c"], `apps/a/b/c/${LINK}`), "apps/a/b/c/node_modules/.bin/rightkit-gate": recorder(log, "deep") });
  const ran = runHook(deep, "in\n");
  assert.equal(ran.status, 0, ran.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "deep check --commit HEAD\n--\ndeep --base origin/main\nin\n--\n");
  assert.match(ran.stderr, /rightkit-gate: using apps\/a\/b\/c\/node_modules\/\.bin\/rightkit-gate/);
  // 2. nothing installed, pnpm present: it runs in the declaring package (at the repository root it has no rightkit-gate to run)
  fs.rmSync(log, { force: true });
  const bin = mkRepo({ pnpm: recorder(log, "pnpm") }, t);
  fs.chmodSync(path.join(bin, "pnpm"), 0o755);
  const viaPnpm = runHook(hookRepo(t, hook(["apps/cutright"], `apps/cutright/${LINK}`)), "in\n", { path: [bin] });
  assert.equal(viaPnpm.status, 0, viaPnpm.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "pnpm --dir apps/cutright exec rightkit-gate check --commit HEAD\n--\npnpm --dir apps/cutright exec rightkit-gate --base origin/main\nin\n--\n");
  assert.match(viaPnpm.stderr, /rightkit-gate: using pnpm --dir apps\/cutright exec rightkit-gate/);
  // 3. no pnpm: node at the package's link, a path in the repository
  fs.rmSync(log, { force: true });
  const shim = mkRepo({}, t);
  fs.symlinkSync(process.execPath, path.join(shim, process.platform === "win32" ? "node.exe" : "node"));
  // PATH: the node link, the directory sh itself lives in and git's (Git for Windows keeps git.exe in cmd\ or mingw64\bin, not in
  // sh's usr\bin; /usr/bin and /bin elsewhere), never pnpm. The hook's first line is `git rev-parse --show-toplevel`.
  const whereDir = (exe) => path.dirname(spawnSync("where", [exe], { encoding: "utf8" }).stdout.split(/\r?\n/)[0].trim());
  const shDirs = process.platform === "win32" ? [whereDir("sh"), whereDir("git")] : ["/usr/bin", "/bin"];
  const noPnpmPath = [shim, ...shDirs].join(path.delimiter);
  const lastResort = hookRepo(t, { ...hook(["apps/cutright"], `apps/cutright/${LINK}`), [`apps/cutright/${LINK}`]: `import fs from "node:fs";\nfs.appendFileSync(${JSON.stringify(log)}, process.argv.slice(2).join(" ") + "\\n");\nfs.readFileSync(0);\n` });
  const viaNode = spawnSync("sh", [".githooks/pre-push", "origin", "https://example.test/r.git"], { cwd: lastResort, input: "in\n", encoding: "utf8", env: { ...process.env, PATH: noPnpmPath } });
  assert.equal(viaNode.status, 0, viaNode.stderr);
  assert.equal(fs.readFileSync(log, "utf8"), "check --commit HEAD\n--base origin/main\n");
  assert.match(viaNode.stderr, /rightkit-gate: using node apps\/cutright\/node_modules\/@rightkit\/release\/bin\/rightkit-gate\.mjs/);
});

test("--to compile stages every declared sidecar first, under the door run, and compiles nothing when staging fails", async (t) => {
  const { root, head } = stageRepo(t);
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  fs.mkdirSync(path.join(root, "app"), { recursive: true });
  fs.writeFileSync(path.join(root, "app", "right-release.config.mjs"), "export default { sidecars: [] };\n");
  fs.writeFileSync(path.join(root, "right-release.config.mjs"), "export default {};\n");
  git("add", "-A"); git("commit", "-q", "-m", "configs");
  const commit = git("rev-parse", "HEAD").stdout.trim();
  const calls = [];
  const io = (status) => ({
    resolveExecutor: directExecutor, runCheck: async () => 0, runCompile: async () => { calls.push("compile"); return 0; },
    spawnInherit: (cmd, a, opts) => { calls.push({ args: a.slice(1).join(" "), door: requireDoorRun({ env: opts.env, commonDir: path.join(root, ".git"), commit, command: "x" }) }); return { status }; },
  });
  assert.equal(await main(["--to", "compile"], { ...capture().io, cwd: root, ...io(0) }), 0);
  assert.deepEqual(calls, [{ args: "stage-sidecar --config app/right-release.config.mjs", door: null }, "compile"], "only the config that declares sidecars is staged, inside a live door run, before the compile");
  fs.rmSync(path.join(root, ".git", "rightkit-gate"), { recursive: true, force: true }); calls.length = 0;
  assert.equal(await main(["--to", "compile"], { ...capture().io, cwd: root, ...io(1) }), 1);
  assert.deepEqual(calls.map((c) => c.args ?? c), ["stage-sidecar --config app/right-release.config.mjs"], "a failed staging compiles nothing");
  assert.ok(head);
});

test("RightKit self-check: the door runs RightKit's own rules for the requested selector only after app rules and class A pass, and never proves another commit with HEAD's imports", async (t) => {
  // .native: Windows runners' TEMP is an 8.3 short path (RUNNER~1) that fs.realpathSync keeps and git expands.
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-self-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.mkdirSync(path.join(root, "scripts", "ci"), { recursive: true });
  fs.writeFileSync(path.join(root, "scripts", "preflight.mjs"), "");
  fs.writeFileSync(path.join(root, "scripts", "ci", "exports-import-effects.mjs"), "");
  fs.writeFileSync(path.join(root, "good.json"), "{}\n");
  git("add", "-A"); git("commit", "-q", "-m", "one");
  const first = git("rev-parse", "HEAD").stdout.trim();
  fs.writeFileSync(path.join(root, "more.json"), "{}\n"); git("add", "-A"); git("commit", "-q", "-m", "two");
  const selfPath = path.join(root, "packages", "release", "bin", "rightkit-gate.mjs");
  const run = async ({ appCheck = 0, args, status = 0 }) => {
    const launched = []; const c = capture();
    const code = await main(["check", ...args], { ...c.io, cwd: root, selfPath, resolveExecutor: directExecutor, appCheck: async () => appCheck, spawnInherit: (cmd, a) => { launched.push(a.map((x) => (x === root ? "." : x.startsWith(`${root}${path.sep}`) ? path.relative(root, x).split(path.sep).join("/") : x)).join(" ")); return { status }; } });
    return { code, launched, err: c.err.join("\n") };
  };
  // Positive control: green app rules and class A on HEAD run RightKit's rules for HEAD, then the import check.
  const green = await run({ args: ["--commit", "HEAD"] });
  assert.equal(green.code, 0, green.err);
  assert.deepEqual(green.launched, ["scripts/preflight.mjs --repo . --commit HEAD", "scripts/ci/exports-import-effects.mjs --report .git/rightkit-gate/import-effects/report.json"]);
  // Negative: a red app rule or a red class A scan executes none of the repository's own scripts.
  const redApp = await run({ appCheck: 1, args: ["--commit", "HEAD"] });
  assert.equal(redApp.code, 1); assert.deepEqual(redApp.launched, []);
  fs.writeFileSync(path.join(root, "bad.json"), "{ not json\n"); git("add", "-A"); git("commit", "-q", "-m", "bad");
  const redA = await run({ args: ["--commit", "HEAD"] });
  assert.equal(redA.code, 1); assert.deepEqual(redA.launched, [], "a red class A runs no candidate code");
  // Negative: an older requested commit is checked by its own rules, and the HEAD-bound import check refuses instead of proving it.
  const older = await run({ args: ["--commit", first] });
  assert.equal(older.code, 1, older.err);
  assert.match(older.launched[0], new RegExp(`--commit ${first}$`));
  assert.equal(older.launched.length, 1, "no import probe of HEAD for another commit");
  assert.match(older.err, /probes the checkout's HEAD/);
  // A red RightKit rule stops before the import check.
  git("rm", "-q", "bad.json"); git("commit", "-q", "-m", "fix");
  const redRules = await run({ args: ["--commit", "HEAD"], status: 1 });
  assert.equal(redRules.code, 1); assert.equal(redRules.launched.length, 1);
  // --worktree: a clean tree equals HEAD, so the HEAD-bound import probe runs (positive control) ...
  const clean = await run({ args: ["--worktree"] });
  assert.equal(clean.code, 0, clean.err);
  assert.deepEqual(clean.launched, ["scripts/preflight.mjs --repo . --worktree", "scripts/ci/exports-import-effects.mjs --report .git/rightkit-gate/import-effects/report.json"]);
  // ... but a tracked edit or an untracked file makes the tree differ from HEAD, and the probe refuses instead of proving other bytes.
  fs.writeFileSync(path.join(root, "good.json"), "{\"edited\":true}\n");
  const dirtyTracked = await run({ args: ["--worktree"] });
  assert.equal(dirtyTracked.code, 1, dirtyTracked.err);
  assert.deepEqual(dirtyTracked.launched, ["scripts/preflight.mjs --repo . --worktree"], "no import probe of HEAD for a modified tree");
  assert.match(dirtyTracked.err, /working tree differs from it/);
  git("checkout", "--", "good.json");
  fs.writeFileSync(path.join(root, "extra.json"), "{}\n");
  const dirtyUntracked = await run({ args: ["--worktree"] });
  assert.equal(dirtyUntracked.code, 1, dirtyUntracked.err);
  assert.equal(dirtyUntracked.launched.length, 1, "an untracked file also refuses the probe");
});

test("class A of an older commit runs cargo's scans on that commit's Cargo files, never the live checkout's", async (t) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-cargo-snap-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("init", "-q"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  fs.mkdirSync(path.join(root, "src"));
  fs.writeFileSync(path.join(root, "src", "lib.rs"), "");
  fs.writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "old"\nversion = "0.1.0"\nedition = "2021"\nlicense = "MIT"\n');
  fs.writeFileSync(path.join(root, "Cargo.lock"), "version = 4\n");
  git("add", "-A"); git("commit", "-q", "-m", "old");
  const old = git("rev-parse", "HEAD").stdout.trim();
  // HEAD and the live checkout move on: a different crate name in HEAD, and a live edit no commit holds.
  fs.writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "new"\nversion = "0.2.0"\nedition = "2021"\nlicense = "MIT"\n');
  git("commit", "-q", "-am", "new");
  fs.writeFileSync(path.join(root, "Cargo.toml"), "[package]\nname = \"live-only\"\n");
  const seen = [];
  const runScans = async (plan, opts) => {
    for (const s of plan.scans.filter((x) => x.class === "A" && (x.tool === "cargo" || x.tool === "rustright"))) {
      const dir = path.join(s.root === "content" ? plan.roots.contentRoot : plan.roots.liveRoot, s.cwd ?? "");
      seen.push(fs.readFileSync(path.join(dir, "Cargo.toml"), "utf8"));
    }
    return { schema: "rightkit.deterministic-scans/1", ok: true, complete: true, classes: ["A"], scans: [], toolVersions: {}, cacheHits: [], notes: [], deferred: [] };
  };
  const c = capture();
  await main(["--class-a", "--commit", old], { ...c.io, cwd: root, resolveExecutor: directExecutor, runScans });
  assert.ok(seen.length > 0, `cargo scans were planned: ${c.err.join("\n")}`);
  for (const text of seen) assert.match(text, /name = "old"/, "the scan reads the requested commit's manifest, not HEAD's or the live edit");
  // Positive control: HEAD's own check reads HEAD's manifest.
  seen.length = 0;
  await main(["--class-a", "--commit", "HEAD"], { ...capture().io, cwd: root, resolveExecutor: directExecutor, runScans });
  for (const text of seen) assert.match(text, /name = "new"/);
});

test("a partial check (--only) says it is not a class A result, and the door and the broker always run the full check", async () => {
  const c = capture();
  assert.equal(await main(["check", "--only", "release-config"], { ...c.io, cwd: process.cwd(), appCheck: async () => 0 }), PARTIAL_EXIT);
  assert.notEqual(PARTIAL_EXIT, 0);
  assert.match(c.err.join("\n"), /PARTIAL: app rules only/);
  assert.equal(await main(["check", "--only", "release-config"], { ...capture().io, cwd: process.cwd(), appCheck: async () => 1 }), 1, "a red partial stays red");
  assert.equal(await main(["check", "--list"], { ...capture().io, cwd: process.cwd(), appCheck: async () => 0 }), 0, "a listing is not a verdict");
  // Real process: a consumer that admits only exit status 0 (the broker, the --to check stage, CI `run:`) rejects a green partial view.
  const bin = fileURLToPath(new URL("./bin/rightkit-gate.mjs", import.meta.url));
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-partial-")));
  try {
    for (const a of [["init", "-q"], ["config", "user.email", "t@example.test"], ["config", "user.name", "t"], ["commit", "-q", "--allow-empty", "-m", "one"]]) spawnSync("git", a, { cwd: repo });
    const real = spawnSync(process.execPath, [bin, "check", "--only", "release-config"], { cwd: repo, encoding: "utf8" });
    assert.equal(real.status, PARTIAL_EXIT, real.stderr);
    assert.match(real.stderr, /PARTIAL/);
  } finally { fs.rmSync(repo, { recursive: true, force: true }); }
  // The door's check stage and the broker's admission call the full form.
  const src = fs.readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  assert.match(src, /main\(\["check", "--commit", "HEAD"\]/, "--to check runs the full check");
});

test("class A export: the export root exists before checkout-index and the prefix uses \"/\" (Windows refused a missing, backslashed prefix: publish run 40282215043)", async (t) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-export-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...a) => spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...a], { cwd: root, encoding: "utf8" });
  git("init", "-q"); fs.mkdirSync(path.join(root, ".github"), { recursive: true });
  fs.writeFileSync(path.join(root, ".github", "x.json"), "{}\n"); fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "x", license: "MIT" }));
  git("add", "-A"); git("commit", "-q", "-m", "x");
  const seen = [];
  const spawn = (cmd, args, opts) => {
    if (cmd === "git" && args[0] === "checkout-index") {
      const prefix = args.find((a) => a.startsWith("--prefix=")).slice("--prefix=".length);
      seen.push({ prefix, existed: fs.existsSync(prefix.slice(0, -1)) });
    }
    return spawnSync(cmd, args, { ...opts, encoding: "utf8" });
  };
  const c = capture();
  await main(["check", "--repo", root, "--commit", "HEAD"], { ...c.io, cwd: root, spawn });
  assert.equal(seen.length, 1, c.err.join("\n"));
  assert.ok(seen[0].prefix.endsWith("/") && !seen[0].prefix.includes("\\"), seen[0].prefix);
  assert.ok(seen[0].existed, "the export root exists before git writes into it");
  assert.doesNotMatch(c.err.join("\n"), /cannot export/);
});
