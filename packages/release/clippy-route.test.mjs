// Tests of the Clippy route (item 8, rows 8.1 to 8.16 of the signed map): the argv, the flow on a direct host against a real git
// repository and a real gate-owned snapshot with a scripted cargo, the broker-host refusals, the broker receipt's gate half and the
// batch-equivalence comparison. Real cargo is exercised by the WSL run (scripts of the item 8 evidence), not here.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { main } from "./bin/rightkit-gate.mjs";
import { CLIPPY_USAGE, ROUTE_CODES, ROUTE_RECEIPT_SCHEMA, clippyCommand, packageNameOf, parseCargoMessages, parseClippyArgv, resolveScope, routeFromBrokerReceipt, sidecarBoundMembers, summarizeRun } from "./clippy-route.mjs";
import { compareEquivalence } from "./clippy-equivalence.mjs";
import { RECEIPT_SCHEMA, gateStateDir, readReceipt, writeReceipt } from "./gate-stages.mjs";
import { repositoryOf, syncSnapshot } from "./snapshot.mjs";
import { resolveExecutor as realResolveExecutor } from "./gate-executor.mjs";

// ---- rows 8.1 and 8.2: argv --------------------------------------------------------------------------------------------------------------

const ADDED = ["--message-format=json", "--keep-going", "--locked"];

test("item 8 row 8.1: the final argv is the request plus exactly the three additions, each recorded as a transformation", () => {
  const p = parseClippyArgv(["--workspace", "--all-targets", "--", "-D", "warnings"]);
  assert.deepEqual(p.final, ["--workspace", "--all-targets", ...ADDED, "--", "-D", "warnings"]);
  assert.deepEqual(p.additions, ADDED);
  assert.deepEqual(p.transformations.map((t) => [t.op, t.arg]), ADDED.map((a) => ["add", a]));
  assert.ok(p.transformations.every((t) => typeof t.reason === "string" && t.reason.length > 10));
  assert.deepEqual(parseClippyArgv([]).final, ADDED, "no request words: only the additions");
  assert.deepEqual(parseClippyArgv(["--"]).final, [...ADDED, "--"]);
  // Already present: not repeated and not recorded as an addition.
  assert.deepEqual(parseClippyArgv(["--keep-going"]).final, ["--keep-going", "--message-format=json", "--locked"]);
  for (const flag of ["--locked", "--frozen", "--offline"]) {
    const q = parseClippyArgv([flag, "-p", "a"]);
    assert.deepEqual(q.additions, ["--message-format=json", "--keep-going"], flag);
    assert.deepEqual(q.already, ["--locked|--frozen|--offline"], flag);
  }
  const both = parseClippyArgv(["--keep-going", "--locked", "--workspace"]);
  assert.deepEqual(both.additions, ["--message-format=json"]);
  assert.equal(both.final.filter((a) => a === "--keep-going").length, 1);
  assert.equal(both.final.filter((a) => a === "--locked").length, 1);
});

test("item 8 row 8.1: every refusal of the request is a refusal with its code, before anything runs", () => {
  const refused = [
    [["--message-format=json"], ROUTE_CODES.messageFormat], [["--message-format", "json"], ROUTE_CODES.messageFormat], [["--message-format=short", "--workspace"], ROUTE_CODES.messageFormat],
    [["--target-dir", "/x"], ROUTE_CODES.targetDir], [["--target-dir=/x"], ROUTE_CODES.targetDir],
    [["--config", "build.rustflags=[]"], ROUTE_CODES.config], [["--config=net.offline=true"], ROUTE_CODES.config],
    [["+nightly"], ROUTE_CODES.toolchain], [["+1.98.0", "--workspace"], ROUTE_CODES.toolchain],
    [["--fix"], ROUTE_CODES.fix], [["--allow-dirty"], ROUTE_CODES.fix], [["--fix", "--allow-dirty"], ROUTE_CODES.fix],
    [["-Z", "unstable-options"], ROUTE_CODES.unstable], [["-Zunstable-options"], ROUTE_CODES.unstable], [["-qZ", "x"], ROUTE_CODES.unstable],
    [["-C", "dir"], ROUTE_CODES.chdir],
    [["--manifest-path", "/abs/Cargo.toml"], ROUTE_CODES.manifestAbsolute], [["--manifest-path=/abs/Cargo.toml"], ROUTE_CODES.manifestAbsolute], [["--manifest-path", "C:\\x\\Cargo.toml"], ROUTE_CODES.manifestAbsolute], [["--manifest-path=\\\\host\\s\\Cargo.toml"], ROUTE_CODES.manifestAbsolute],
    [["build"], ROUTE_CODES.notClippy], [["check", "--all"], ROUTE_CODES.notClippy], [["test"], ROUTE_CODES.notClippy], [["fmt"], ROUTE_CODES.notClippy], [["clippy"], ROUTE_CODES.notClippy], [["cargo", "clippy"], ROUTE_CODES.notClippy], [["--workspace", "doc"], ROUTE_CODES.notClippy],
  ];
  for (const [args, code] of refused) {
    const p = parseClippyArgv(args);
    assert.equal(p.refusal?.code, code, JSON.stringify(args));
    assert.equal(p.final, undefined, `${JSON.stringify(args)} yields no argv`);
  }
  // Words after `--` are the lint driver's, never the route's.
  for (const args of [["--", "-Zfoo"], ["--", "build"], ["--workspace", "--", "+x", "--fix", "-D", "warnings"]]) assert.equal(parseClippyArgv(args).refusal, undefined, JSON.stringify(args));
  // A value that merely looks like a refused flag is a value.
  for (const args of [["-pCore"], ["-p", "Core"], ["-F", "fix"], ["--features", "fix"], ["--bin", "build"], ["--exclude", "check"], ["--profile", "dev"], ["--target", "x86_64-unknown-linux-gnu"], ["-j", "4"], ["--manifest-path", "crates/a/Cargo.toml"]]) assert.equal(parseClippyArgv(args).refusal, undefined, JSON.stringify(args));
  assert.deepEqual(parseClippyArgv(["-h"]), { help: true });
  assert.deepEqual(parseClippyArgv(["--workspace", "--version"]), { help: true });
});

test("item 8 row 8.2: the exact requested scope is preserved word for word: workspace, packages, features, targets, profile, lint policy (property over the cases)", () => {
  const scopes = [
    ["--workspace"], ["--workspace", "--exclude", "rightkit-tts"], ["-p", "a", "-p", "b"], ["--package=a", "--package", "b"], ["-pa", "-pb"], ["--features", "x,y"], ["-F", "x"], ["--features=x"], ["--all-features"], ["--no-default-features"], ["--no-default-features", "--features", "z"],
    ["--target", "x86_64-pc-windows-msvc"], ["--target=aarch64-apple-darwin"], ["--profile", "release"], ["--release"], ["-r"], ["--all-targets"], ["--lib"], ["--tests"], ["--bins", "--examples"], ["--bin", "tool"], ["--test", "t1"],
    ["--manifest-path", "crates/a/Cargo.toml"], ["-j", "2"], ["--color", "always"], ["-v"], ["-q"], ["--ignore-rust-version"],
  ];
  const lints = [[], ["--"], ["--", "-D", "warnings"], ["--", "-W", "clippy::pedantic", "-A", "clippy::module_name_repetitions"], ["--", "-D", "clippy::all", "-F", "unsafe_code"]];
  let n = 0;
  for (const scope of scopes) for (const extra of [[], ["--all-targets"], ["--workspace", "--all-features"]]) for (const lint of lints) {
    const request = [...scope, ...extra, ...lint];
    const p = parseClippyArgv(request);
    assert.equal(p.refusal, undefined, JSON.stringify(request));
    assert.deepEqual(p.request, request);
    // Removing the additions gives the request back, word for word and in order.
    const removed = [...p.final]; for (const a of p.additions) removed.splice(removed.indexOf(a), 1);
    assert.deepEqual(removed, request, JSON.stringify(request));
    // The additions sit before `--` (the lint words after it are untouched) and nothing else was added.
    const sep = p.final.indexOf("--");
    if (sep >= 0) assert.deepEqual(p.final.slice(sep), lint, "lint policy after -- is untouched");
    for (const a of p.final) assert.ok(request.includes(a) || ADDED.includes(a), `${a} is the request's or one of the three additions`);
    assert.ok(p.additions.every((a) => ADDED.includes(a)));
    assert.ok(!p.final.includes("--workspace") || request.includes("--workspace"), "never widened to --workspace");
    n += 1;
  }
  assert.ok(n > 400);
});

// ---- the direct-host flow -----------------------------------------------------------------------------------------------------------------

const git = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8" }); assert.equal(r.status, 0, `git ${args.join(" ")}: ${r.stderr}`); return r.stdout.trim(); };
const write = (root, files) => { for (const [f, body] of Object.entries(files)) { const p = path.join(root, f); fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, body); } };
const crateFiles = (names) => Object.assign({ "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n', "Cargo.lock": "version = 4\n" }, ...names.map((n) => ({ [`crates/${n}/Cargo.toml`]: `[package]\nname = "${n}"\nversion = "0.1.0"\nedition = "2021"\n`, [`crates/${n}/src/lib.rs`]: "pub fn committed() {}\n" })));

function makeRepo(t, files) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-clippy-route-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main");
  write(repo, files);
  git(repo, "add", "-A");
  git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "fixture");
  const snaps = path.join(dir, "snaps");
  const key = repositoryOf(repo).key;
  return { dir, repo, snaps, key, snapshotDir: path.join(snaps, key), target: path.join(dir, "checkout-target"), sha: git(repo, "rev-parse", "HEAD"), tree: git(repo, "rev-parse", "HEAD^{tree}"), stateDir: path.join(repo, ".git", "rightkit-gate", "clippy-route") };
}

const pkgId = (name) => `path+file:///w/crates/${name}#${name}@0.1.0`;
const artifact = (name, { kind = ["lib"], features = [], test = false, fresh = false } = {}) => ({ reason: "compiler-artifact", package_id: pkgId(name), target: { name, kind }, profile: { test }, features, fresh });
const diag = (name, code, { level = "error", file = "src/lib.rs", line = 3, col = 5, kind = ["lib"] } = {}) => ({ reason: "compiler-message", package_id: pkgId(name), target: { name, kind }, message: { level, code: { code }, message: `${code} in ${name}`, spans: [{ file_name: `crates/${name}/${file}`, line_start: line, line_end: line, column_start: col, column_end: col + 4, is_primary: true }], rendered: `${level}: ${code} in ${name}\n` } });
const finished = (success) => ({ reason: "build-finished", success });
const metadataOf = (root, pkgs) => ({
  packages: pkgs.map((p) => ({ name: p.name, version: "0.1.0", id: `path+file://${root}/crates/${p.name}#${p.name}@0.1.0`, manifest_path: path.join(root, "crates", p.name, "Cargo.toml"), dependencies: (p.deps ?? []).map((d) => ({ name: d })), targets: [] })),
  workspace_members: pkgs.map((p) => `path+file://${root}/crates/${p.name}#${p.name}@0.1.0`),
  workspace_default_members: pkgs.map((p) => `path+file://${root}/crates/${p.name}#${p.name}@0.1.0`), workspace_root: root,
});
const ok = (stdout = "") => ({ status: 0, stdout, stderr: "" });

/** Runs the route on a direct host against `files` committed in a real repository, with scripted cargo, class A and ownership. */
async function route(t, { crates = [{ name: "a" }, { name: "b" }], files = null, args = ["--workspace"], platform = "linux", env = {}, classA = 0, messages = [], exit = 0, stderr = "", resolve = null, which = { cargo: "/opt/fake/bin/cargo" }, acquire = null, afterCargo = null, targetDir = null, before = null, cwd = null, shared = null, exists = fs.existsSync, preMetadataCrates = null } = {}) {
  const fx = shared ?? makeRepo(t, files ?? crateFiles(crates.map((c) => c.name)));
  if (before) before(fx);
  const log = { events: [], spawn: [], cargo: [], out: [], err: [] };
  const spawn = (cmd, a, opts = {}) => {
    log.spawn.push([cmd, ...a].join(" "));
    if (cmd === "git") return spawnSync("git", a, { cwd: opts.cwd, encoding: "utf8", maxBuffer: 1 << 28 });
    if (cmd === "which" || cmd === "where") { const found = which[a[0]]; return found ? ok(`${found}\n`) : { status: 1, stdout: "", stderr: "" }; }
    if (cmd === "cargo" || cmd === "rustc" || /rk-cargo-shim/.test(cmd)) log.events.push(`spawn:${cmd === "cargo" || cmd === "rustc" ? cmd : "shim"} ${a[0]}`);
    if (cmd === "cargo" && a[0] === "locate-project") return ok(`${opts.cwd}/Cargo.toml\n`);
    // The pre-class-A look at the checkout's requested scope (sidecar-bound refusal): `metadata --no-deps --offline --locked` in the checkout.
    if (cmd === "cargo" && a[0] === "metadata" && a.includes("--locked")) return ok(JSON.stringify(metadataOf(opts.cwd, preMetadataCrates ?? crates)));
    if (cmd === "cargo" && a[0] === "metadata") return ok(JSON.stringify({ target_directory: typeof targetDir === "function" ? targetDir(fx) : (targetDir ?? fx.target) }));
    if (cmd === "cargo" && a[0] === "-vV") return ok("cargo 1.98.0 (fake 2026-10-01)\nrelease: 1.98.0\n");
    if (cmd === "rustc" && a[0] === "-vV") return ok("rustc 1.98.0 (fake 2026-10-01)\nrelease: 1.98.0\n");
    if (/rk-cargo-shim/.test(cmd) && a[0] === "metadata") return ok(JSON.stringify(metadataOf(opts.cwd, crates)));
    log.unexpected = [...(log.unexpected ?? []), [cmd, ...a].join(" ")];
    return { status: 127, stdout: "", stderr: "", error: new Error(`unexpected ${cmd}`) };
  };
  const own = { handle: "own-1", joined: false, held: () => true, release: () => ({ released: true }) };
  const o = { spawn, exists, readFile: (f) => fs.readFileSync(f, "utf8"), out: (m) => log.out.push(m), err: (m) => log.err.push(m), cwd: cwd ?? fx.repo, env: { PATH: "/usr/bin", ...env }, platform, snapshotRoot: fx.snaps, abort: new AbortController() };
  const deps = {
    resolveExecutor: resolve === "real" ? realResolveExecutor : (resolve ?? (() => ({ kind: "direct", cargo: ["cargo"] }))),
    acquireOwnership: (...a) => { log.events.push("own"); return acquire ? acquire(...a) : own; },
    syncSnapshot: (x) => { log.events.push("sync"); return syncSnapshot(x); },
    runClassA: async () => { log.events.push("classA"); return classA; },
    runCargo: async (file, a, opts) => {
      log.events.push("cargo:clippy"); log.cargo.push({ file, args: a, env: opts.env, cwd: opts.cwd });
      if (afterCargo) await afterCargo({ fx, opts, log });
      fs.writeFileSync(opts.stdoutFile, `${messages.map((m) => JSON.stringify(m)).join("\n")}\n`);
      return { status: exit, stdout: "", stderr };
    },
    absoluteEnv: [],
  };
  const code = await clippyCommand(o, args, deps);
  const files2 = fs.existsSync(fx.stateDir) ? fs.readdirSync(fx.stateDir) : [];
  const receiptFile = files2.find((f) => /^[0-9a-f-]{36}\.json$/.test(f));
  const receipt = receiptFile ? JSON.parse(fs.readFileSync(path.join(fx.stateDir, receiptFile), "utf8")) : null;
  const diagnosticsFile = files2.find((f) => f.endsWith(".diagnostics.json"));
  return { code, fx, log, receipt, receiptFile, state: files2, diagnostics: diagnosticsFile ? JSON.parse(fs.readFileSync(path.join(fx.stateDir, diagnosticsFile), "utf8")) : null, out: log.out.join("\n"), err: log.err.join("\n") };
}

test("item 8 row 8.3: on a dirty checkout the diagnostics come from the commit, and the class A row precedes the first cargo launch", async (t) => {
  const seen = {};
  const r = await route(t, {
    messages: [artifact("a"), artifact("b"), finished(true)],
    before: (fx) => { fs.writeFileSync(path.join(fx.repo, "crates/a/src/lib.rs"), "pub fn DIRTY() {}\n"); fs.writeFileSync(path.join(fx.repo, "crates/a/src/extra.rs"), "// untracked\n"); },
    afterCargo: ({ fx, opts }) => { seen.snapshotA = fs.readFileSync(path.join(opts.cwd, "crates/a/src/lib.rs"), "utf8"); seen.stray = fs.existsSync(path.join(opts.cwd, "crates/a/src/extra.rs")); seen.checkoutA = fs.readFileSync(path.join(fx.repo, "crates/a/src/lib.rs"), "utf8"); },
  });
  assert.equal(r.code, 0, r.err);
  assert.equal(seen.snapshotA, "pub fn committed() {}\n", "the compile read the committed bytes");
  assert.equal(seen.stray, false, "an untracked checkout file never reaches the snapshot");
  assert.match(seen.checkoutA, /DIRTY/, "the checkout kept its own edit");
  const order = r.log.events;
  assert.deepEqual(order.filter((e) => ["own", "sync", "classA", "cargo:clippy"].includes(e)), ["own", "sync", "classA", "cargo:clippy"]);
  const firstCargo = order.findIndex((e) => /^spawn:(cargo|shim|rustc)/.test(e) || e === "cargo:clippy");
  assert.ok(order.indexOf("classA") < firstCargo, `class A (${order.indexOf("classA")}) precedes every cargo launch (${firstCargo}): ${order.join(" > ")}`);
  assert.equal(r.receipt.commit.sha, r.fx.sha);
  assert.deepEqual(r.log.unexpected ?? [], []);
});

test("item 8 row 8.3: a red class A starts no cargo and the route exits non-zero; a checkout edit made mid-run never reaches the diagnostics", async (t) => {
  const red = await route(t, { classA: 1, messages: [artifact("a")] });
  assert.equal(red.code, 1);
  assert.match(red.err, /class A is red .*Clippy was not started/);
  assert.deepEqual(red.log.events.filter((e) => e !== "own"), ["sync", "classA"], "after class A nothing runs");
  assert.ok(!red.log.events.some((e) => e.startsWith("spawn:") || e === "cargo:clippy"), "zero cargo processes");
  assert.ok(!red.log.spawn.some((c) => /^cargo |rk-cargo-shim|^rustc /.test(c)), red.log.spawn.join("\n"));
  assert.equal(red.receipt, null);
  assert.match(red.out, /"clippyStarted":false/);
  const torn = await route(t, { classA: 2 });
  assert.equal(torn.code, 2, "an RK_CHECK_ERROR class A keeps its exit 2");
  // A checkout edit made while cargo runs: the diagnostics are still the commit's, and the snapshot is still its tree.
  const seen = {};
  const mid = await route(t, { messages: [diag("a", "clippy::needless_return"), finished(false)], exit: 101, afterCargo: ({ fx, opts }) => { fs.writeFileSync(path.join(fx.repo, "crates/a/src/lib.rs"), "pub fn EDITED_MID_RUN() {}\n"); seen.snapshot = fs.readFileSync(path.join(opts.cwd, "crates/a/src/lib.rs"), "utf8"); } });
  assert.equal(seen.snapshot, "pub fn committed() {}\n");
  assert.equal(mid.receipt.snapshot.treeAtExit, mid.fx.tree, "the snapshot held the commit's tree after the checkout edit");
  assert.equal(mid.receipt.result.aggregate, "FAILED");
});

test("item 8 row 8.4: two independent lint failures and a dependency failure: aggregate FAILED, both attributed, the dependent blocked, no completeness claim", async (t) => {
  const crates = [{ name: "a" }, { name: "b" }, { name: "c" }, { name: "d", deps: ["c"] }, { name: "e" }];
  const r = await route(t, {
    crates, exit: 101,
    messages: [artifact("e"), diag("a", "clippy::needless_return"), diag("b", "clippy::redundant_clone", { line: 9 }), diag("c", "E0308", { file: "src/main.rs", kind: ["bin"] }), finished(false)],
    stderr: "error: could not compile `a` (lib) due to 1 previous error\nerror: could not compile `b` (lib) due to 1 previous error\nerror: could not compile `c` (bin \"c\") due to 1 previous error\n",
  });
  assert.equal(r.code, 1);
  const res = r.receipt.result;
  assert.equal(res.aggregate, "FAILED");
  assert.deepEqual(res.units.filter((u) => u.status === "failed").map((u) => `${u.package}/${u.target}/${u.kind}`).sort(), ["a/a/lib", "b/b/lib", "c/c/bin"]);
  assert.deepEqual(res.units.filter((u) => u.status === "clean").map((u) => u.package), ["e"]);
  assert.deepEqual(res.blocked, [{ package: "d", blockedBy: ["c"] }], "d depends on the failed c and built nothing");
  assert.equal(res.complete, false, "no claim of completeness when a dependency blocked work");
  assert.match(res.completeness, /^not claimed: 1 requested package\(s\) were blocked/);
  assert.deepEqual(r.diagnostics.diagnostics.map((d) => [d.package, d.target, d.kind, d.code, d.span.file, d.span.line]).sort(), [["a", "a", "lib", "clippy::needless_return", "crates/a/src/lib.rs", 3], ["b", "b", "lib", "clippy::redundant_clone", "crates/b/src/lib.rs", 9], ["c", "c", "bin", "E0308", "crates/c/src/main.rs", 3]]);
  assert.match(r.out, /clippy route FAILED .*3 failed, 1 blocked; not claimed/);
  assert.match(r.out, /blocked d: dependency c failed/);
  assert.ok(!/every requested unit was built/.test(r.out + JSON.stringify(res)));
  assert.equal(r.receipt.exit.status, 101);
  assert.deepEqual(r.receipt.invocations[0].argv.slice(0, 2), ["clippy", "--workspace"]);
});

test("item 8 row 8.4: failures with nothing blocked are FAILED with complete diagnostics; a clean run is PASSED and complete; an unfinished stream is never complete", async (t) => {
  const crates = [{ name: "a" }, { name: "b" }, { name: "c" }];
  const failing = await route(t, { crates, exit: 101, messages: [artifact("c"), diag("a", "clippy::x"), diag("b", "clippy::y"), finished(false)], stderr: "error: could not compile `a`\nerror: could not compile `b`\n" });
  assert.equal(failing.code, 1);
  assert.equal(failing.receipt.result.aggregate, "FAILED");
  assert.equal(failing.receipt.result.complete, true, "every requested unit ran; the aggregate is still FAILED");
  assert.deepEqual(failing.receipt.result.blocked, []);
  const clean = await route(t, { crates, messages: [artifact("a"), artifact("b"), artifact("c"), finished(true)] });
  assert.equal(clean.code, 0);
  assert.equal(clean.receipt.result.aggregate, "PASSED");
  assert.equal(clean.receipt.result.complete, true);
  assert.match(clean.receipt.result.completeness, /every requested unit was built/);
  // Cargo stopped before it reported: nothing is claimed, and a zero exit without a successful build-finished is not a pass.
  const cut = await route(t, { crates, exit: 101, messages: [artifact("a")], stderr: "error: the lock file needs to be updated but --locked was passed\n" });
  assert.equal(cut.code, 1);
  assert.equal(cut.receipt.result.complete, false);
  assert.match(cut.receipt.result.completeness, /no build-finished/);
  const quiet = await route(t, { crates, exit: 0, messages: [artifact("a")] });
  assert.equal(quiet.code, 1, "exit 0 with no build-finished message is not PASSED");
  assert.equal(quiet.receipt.result.aggregate, "FAILED");
  const refused = await route(t, { crates, exit: 0, messages: [artifact("a"), finished(false)] });
  assert.equal(refused.code, 1, "build-finished success=false is not PASSED even at exit 0");
  // The pure summary: units of non-members (dependencies) never appear as units.
  const s = summarizeRun({ messages: [artifact("serde"), artifact("a"), finished(true)], exit: 0, scope: { members: [{ name: "a" }], all: [{ name: "a" }], unresolved: [] } });
  assert.deepEqual(s.units.map((u) => u.package), ["a"]);
  assert.equal(packageNameOf("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.1"), "serde");
  assert.equal(packageNameOf("rightkit-fs 0.1.0 (path+file:///x/crates/rightkit-fs)"), "rightkit-fs");
  assert.equal(packageNameOf("path+file:///x/crates/odd#0.1.0"), "odd");
  assert.deepEqual(parseCargoMessages('{"reason":"a"}\nnot json\n\n{"reason":"b"}').messages.length, 2);
});

test("item 8 row 8.5: the route leaves the door receipts byte-identical, and neither --to qualify nor --to compile takes a route receipt as stage evidence", async (t) => {
  const fx = makeRepo(t, crateFiles(["a"]));
  const doorReceipt = { schema: RECEIPT_SCHEMA, commit: fx.sha, app: ".", binding: { tree: fx.tree }, stages: { check: { ok: true, at: "2026-10-29T00:00:00Z" } } };
  const commonDir = path.join(fx.repo, ".git");
  writeReceipt({ commonDir, receipt: doorReceipt });
  const doorDir = path.join(gateStateDir(commonDir), "receipts");
  const bytes = () => fs.readdirSync(doorDir).sort().map((f) => [f, fs.readFileSync(path.join(doorDir, f), "utf8")]);
  const runsBefore = fs.existsSync(path.join(gateStateDir(commonDir), "runs"));
  const before = bytes();
  const r = await route(t, { shared: fx, crates: [{ name: "a" }], messages: [artifact("a"), finished(true)] });
  assert.equal(r.code, 0, r.err);
  assert.deepEqual(bytes(), before, "the door's receipts are byte-identical after a route run");
  assert.equal(fs.existsSync(path.join(gateStateDir(commonDir), "runs")), runsBefore, "no door run file was started");
  assert.equal(readReceipt({ commonDir, commit: fx.sha }).stages.check.ok, true);
  assert.notEqual(r.receipt.schema, RECEIPT_SCHEMA);
  assert.equal(r.receipt.schema, ROUTE_RECEIPT_SCHEMA);
  assert.ok(!("stages" in r.receipt), "no stage record a door run could take for a stage");
  // A route receipt placed where the door reads its own is not a door receipt.
  const other = makeRepo(t, crateFiles(["a"]));
  const copyDir = path.join(gateStateDir(path.join(other.repo, ".git")), "receipts");
  fs.mkdirSync(copyDir, { recursive: true });
  const { receiptKey } = await import("./gate-stages.mjs");
  fs.writeFileSync(path.join(copyDir, `${receiptKey(other.sha, ".")}.json`), JSON.stringify({ ...r.receipt, commit: other.sha, app: "." }));
  assert.equal(readReceipt({ commonDir: path.join(other.repo, ".git"), commit: other.sha }), null, "the door's reader refuses the route's schema");
  // The door's entries refuse it as --receipt-in (the import that stands in for stages).
  const receiptFile = path.join(r.fx.stateDir, r.receiptFile);
  const gitOnly = (cmd, a, opts) => (cmd === "git" ? spawnSync("git", a, { cwd: opts.cwd, encoding: "utf8" }) : { status: 127, stdout: "", stderr: "", error: new Error("not run") });
  for (const args of [["--to", "qualify", "--platform", "mac", "--tier", "patch", "--receipt-in", receiptFile], ["--to", "compile", "--receipt-in", receiptFile]]) {
    const out = [];
    const code = await main(args, { cwd: fx.repo, platform: "linux", env: { PATH: "/usr/bin" }, snapshotRoot: fx.snaps, out: (m) => out.push(m), err: (m) => out.push(m), spawn: gitOnly, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), acquireOwnership: () => ({ handle: "own-1", joined: false, held: () => true, release: () => ({ released: true }) }), syncSnapshot });
    assert.equal(code, 1, `${args[1]}: ${out.join("\n")}`);
    assert.match(out.join("\n"), /is not a rightkit\.gate-receipt\/1 receipt/, args[1]);
  }
});

test("item 8 rows 8.9 and 8.16: the gate reads a broker receipt's attribution and toolchain; missing evidence is FAILED, not partial success", () => {
  const good = { exit: 0, toolchain: "stable|cargo:aaa|rustc:bbb", attribution: { available: true, source: "cargo-json", truncated: 0, units: [{ package: "a", packageId: "path+file:///w/a#a@0.1.0", target: "a", kind: "lib", errors: 0, warnings: 1, otherMessages: 0, fresh: true }, { package: "a", packageId: "path+file:///w/a#a@0.1.0", target: "t1", kind: "test", errors: 0, warnings: 0, otherMessages: 0, fresh: false }, { package: "b", target: "b", kind: "bin", errors: 0, warnings: 0 }] } };
  const ok = routeFromBrokerReceipt(good);
  assert.equal(ok.aggregate, "PASSED");
  assert.deepEqual(ok.problems, []);
  assert.deepEqual(ok.units.map((u) => [u.package, u.target, u.kind, u.status]), [["a", "a", "lib", "clean"], ["a", "t1", "test", "clean"], ["b", "b", "bin", "clean"]], "per-package and per-target units map to the route's output");
  assert.equal(ok.toolchain, good.toolchain);
  for (const [name, mutate, problem] of [
    ["no attribution", (r) => { delete r.attribution; }, /no attribution/], ["no toolchain", (r) => { delete r.toolchain; }, /no toolchain identity/], ["an empty toolchain", (r) => { r.toolchain = "  "; }, /no toolchain identity/],
    ["attribution unavailable", (r) => { r.attribution = { available: false, reason: "cargo-json-message-format-not-requested" }; }, /unavailable \(cargo-json-message-format-not-requested\)/],
    ["attribution without units", (r) => { r.attribution = { available: true }; }, /lists no units/], ["truncated attribution", (r) => { r.attribution.truncated = 3; }, /truncated/], ["no exit", (r) => { delete r.exit; }, /no exit status/],
  ]) {
    const r = structuredClone(good); mutate(r);
    const res = routeFromBrokerReceipt(r);
    assert.equal(res.aggregate, "FAILED", name);
    assert.equal(res.code, ROUTE_CODES.receiptEvidence, name);
    assert.match(res.problems.join(" | "), problem, name);
    assert.equal(res.complete, false, name);
  }
  assert.equal(routeFromBrokerReceipt({ ...good, exit: 101 }).aggregate, "FAILED");
  const failing = structuredClone(good); failing.attribution.units[2].errors = 2;
  assert.equal(routeFromBrokerReceipt(failing).aggregate, "FAILED");
  assert.equal(routeFromBrokerReceipt({ ...good, exit: { code: 0 } }).aggregate, "PASSED");
  assert.equal(routeFromBrokerReceipt(null).aggregate, "FAILED");
});

test("item 8 rows 8.3, 8.10 and 8.13: on Mac and Dell the route refuses before class A, the sync and any compile, naming item 7 row 7.12, and never runs cargo directly", async (t) => {
  const hosts = {
    mac: { platform: "darwin", env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/Volumes/D/.rightkit-managed/broker.sock", PATH: "/Volumes/D/.rightkit-managed/agent-bin:/usr/bin" }, rustright: "/Volumes/D/.rightkit-managed/agent-bin/rustright" },
    dell: { platform: "win32", env: { RIGHTKIT_BUILD_BROKER_SOCKET: "\\\\.\\pipe\\rustright-broker", PATH: "D:\\.rightkit-managed\\agent-bin;C:\\Windows" }, rustright: "D:\\.rightkit-managed\\agent-bin\\rustright.cmd" },
  };
  for (const [name, h] of Object.entries(hosts)) {
    // The broker is there: the carrier is not wired, so the route is BLOCKED on 7.12 (never skipped, never passed).
    const carrier = await route(t, { platform: h.platform, env: h.env, resolve: "real", exists: () => false, which: { rustright: h.rustright, cargo: "/opt/fake/bin/cargo" }, messages: [artifact("a"), finished(true)] }).catch((e) => e);
    assert.ok(!(carrier instanceof Error), String(carrier));
    assert.equal(carrier.code, 1, name);
    assert.match(carrier.err, /BLOCKED: RK_CLIPPY_BROKER_CARRIER_NOT_WIRED: .*item 7 row 7\.12/, name);
    assert.match(carrier.out, /"blockedOn":\["item7:7\.12"\]/, name);
    assert.deepEqual(carrier.log.events, [], `${name}: no ownership, sync, class A or compile`);
    assert.equal(carrier.receipt, null);
    assert.ok(!carrier.log.spawn.some((c) => /^cargo |^rustc |rustright cargo|rk-cargo-shim/.test(c)), `${name}: no cargo launch of any kind: ${carrier.log.spawn.join(" | ")}`);
    // The broker is unreachable on a managed host: refuse, no direct cargo, and say that nothing was launched.
    const refused = await route(t, { platform: h.platform, env: h.env, resolve: "real", exists: () => false, which: { cargo: "/opt/fake/bin/cargo" }, messages: [artifact("a"), finished(true)] });
    assert.equal(refused.code, 1, name);
    assert.match(refused.err, /BLOCKED: RK_CLIPPY_BROKER_UNAVAILABLE: .*never compiles around the broker, and no launch was attempted/, name);
    assert.match(refused.out, /"launched":false/, name);
    assert.deepEqual(refused.log.events, [], `${name}: no ownership, sync, class A or compile`);
    assert.ok(!refused.log.spawn.some((c) => /^cargo |^rustc |rk-cargo-shim/.test(c)), `${name}: no direct cargo fallback: ${refused.log.spawn.join(" | ")}`);
  }
});

test("item 8 row 8.11: Linux is always direct: RustRight on PATH changes nothing, and the trace holds no broker launch", async (t) => {
  const r = await route(t, { env: { PATH: "/home/a/.rightkit-managed/agent-bin:/usr/local/bin:/usr/bin", RIGHTKIT_BUILD_BROKER_SOCKET: "/run/rustright.sock" }, resolve: "real", which: { rustright: "/home/a/.rightkit-managed/agent-bin/rustright", cargo: "/usr/local/bin/cargo" }, messages: [artifact("a"), artifact("b"), finished(true)] });
  assert.equal(r.code, 0, r.err);
  assert.equal(r.receipt.executor, "direct");
  assert.ok(!r.log.spawn.some((c) => /rustright/.test(c)), `no broker launch in the trace: ${r.log.spawn.join(" | ")}`);
  assert.ok(!/rustright/.test(r.log.cargo[0].file), "the cargo launched is the runtime wrapper, not the client");
  assert.match(r.log.cargo[0].file, /rk-cargo-shim-[0-9a-f]{16}[\\/]bin[\\/]cargo$/);
});

test("item 8 row 8.12: outputs go to the checkout's resolved target directory (relative build.target-dir or not); nothing is written under the snapshot; a caller's CARGO_TARGET_DIR into it is refused", async (t) => {
  for (const [label, rel] of [["default target/", "target"], ["a relative build.target-dir", "out/build"]]) {
    const r = await route(t, { messages: [artifact("a"), artifact("b"), finished(true)], targetDir: (fx) => path.join(fx.repo, rel), afterCargo: ({ fx }) => { fs.mkdirSync(path.join(fx.repo, rel), { recursive: true }); } });
    assert.equal(r.code, 0, `${label}: ${r.err}`);
    assert.equal(r.receipt.outputDir, path.join(r.fx.repo, rel), label);
    assert.deepEqual(r.receipt.outputDirs, { ".": path.join(r.fx.repo, rel) });
    assert.equal(r.log.cargo[0].env.CARGO_TARGET_DIR, path.join(r.fx.repo, rel), `${label}: cargo is told the checkout's directory`);
    assert.ok(!fs.existsSync(path.join(r.fx.snapshotDir, "target")) && !fs.existsSync(path.join(r.fx.snapshotDir, "out")), `${label}: nothing under the snapshot`);
    assert.deepEqual(fs.readdirSync(r.fx.snapshotDir).sort(), ["Cargo.lock", "Cargo.toml", "crates"], `${label}: the snapshot is the commit`);
    assert.equal(r.receipt.snapshot.path, r.fx.snapshotDir);
  }
  // A caller's output directory inside the snapshot: refused before anything starts.
  for (const name of ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]) {
    const fx = makeRepo(t, crateFiles(["a"]));
    const r = await route(t, { shared: fx, env: { [name]: path.join(fx.snapshotDir, "target") }, messages: [artifact("a"), finished(true)] });
    assert.equal(r.code, 2, name);
    assert.match(r.err, /RK_CLIPPY_OUTPUT_IN_SNAPSHOT: .* points into the gate-owned snapshot/, name);
    assert.deepEqual(r.log.events, [], `${name}: no ownership, sync, class A or compile`);
  }
  // A resolved output directory inside the snapshot is refused too, before cargo.
  const inside = await route(t, { messages: [artifact("a")], targetDir: (fx) => path.join(fx.snapshotDir, "target") });
  assert.equal(inside.code, 2);
  assert.match(inside.err, /RK_CLIPPY_OUTPUT_IN_SNAPSHOT: the resolved output directory/);
  assert.ok(!inside.log.events.includes("cargo:clippy"));
  // CARGO_HOME, sccache and temp are inherited only as absolute paths.
  const fx = makeRepo(t, crateFiles(["a"]));
  const log = []; const code = await clippyCommand({ spawn: (c, a, o) => (c === "git" ? spawnSync("git", a, { cwd: o.cwd, encoding: "utf8" }) : { status: 1, stdout: "" }), exists: fs.existsSync, readFile: () => "", out: () => {}, err: (m) => log.push(m), cwd: fx.repo, env: { PATH: "/usr/bin", CARGO_HOME: "relative/home" }, platform: "linux", snapshotRoot: fx.snaps }, ["--workspace"], { resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), absoluteEnv: ["CARGO_HOME", "TMPDIR"], acquireOwnership: () => { throw new Error("must not own"); } });
  assert.equal(code, 1);
  assert.match(log.join("\n"), /CARGO_HOME=relative\/home is relative/);
});

test("item 8 row 8.13: the receipt's trees equal the commit's tree; a snapshot byte or file changed mid-run discards the diagnostics; a second request never re-syncs", async (t) => {
  const r = await route(t, { messages: [artifact("a"), artifact("b"), finished(true)] });
  assert.equal(r.code, 0, r.err);
  assert.equal(r.receipt.commit.tree, r.fx.tree);
  assert.equal(r.receipt.snapshot.treeAtStart, r.fx.tree);
  assert.equal(r.receipt.snapshot.treeAtExit, r.fx.tree);
  assert.equal(r.receipt.commit.sha, r.fx.sha);
  assert.equal(r.receipt.ownership.handle, "own-1");
  assert.match(r.receipt.toolchain, /^direct\|cargo:[0-9a-f]{64}\|rustc:[0-9a-f]{64}$/);
  for (const field of ["requestId", "executor", "issuedAt", "commit", "snapshot", "ownership", "outputDir", "exit", "diagnosticsPath", "timingsMs"]) assert.ok(field in r.receipt, `the shared field ${field}`);
  assert.ok(["sync", "classA", "cargo", "afterExitBind", "diagnostics"].every((k) => k in r.receipt.timingsMs));
  assert.ok(fs.existsSync(r.receipt.diagnosticsPath));
  assert.deepEqual(r.state.filter((f) => f.endsWith(".messages.jsonl")), [], "the raw message file is removed");
  for (const [what, tamper] of [["a byte changed", ({ opts }) => fs.writeFileSync(path.join(opts.cwd, "crates/a/src/lib.rs"), "pub fn tampered() {}\n")], ["a file added", ({ opts }) => fs.writeFileSync(path.join(opts.cwd, "stray.txt"), "x")], ["a link planted", ({ opts, fx }) => fs.symlinkSync(fx.repo, path.join(opts.cwd, "crates", "into-checkout"))]]) {
    const bad = await route(t, { messages: [diag("a", "clippy::x"), finished(false)], exit: 101, afterCargo: tamper });
    assert.equal(bad.code, 2, what);
    assert.match(bad.err, /RK_CHECK_ERROR: .*after Clippy.*the diagnostics are discarded/, what);
    assert.match(bad.out, /"discarded":true/, what);
    assert.equal(bad.receipt, null, `${what}: no receipt`);
    assert.equal(bad.diagnostics, null, `${what}: no diagnostics`);
    assert.deepEqual(bad.state, [], `${what}: nothing of the run is kept (${bad.state.join(", ")})`);
  }
  // A second request while the first holds the host: refused by ownership, and the snapshot is never re-synced.
  let secondSyncs = 0; let second = null;
  const first = await route(t, {
    messages: [artifact("a"), finished(true)],
    afterCargo: async ({ fx }) => {
      const busy = () => { throw Object.assign(new Error("build ownership /x/ownership.json is held by another build"), { code: "busy" }); };
      const log2 = []; const events = [];
      second = await clippyCommand({ spawn: (c, a, o) => (c === "git" ? spawnSync("git", a, { cwd: o.cwd, encoding: "utf8" }) : { status: 127, stdout: "", error: new Error("x") }), exists: fs.existsSync, readFile: () => "", out: (m) => log2.push(m), err: (m) => log2.push(m), cwd: fx.repo, env: { PATH: "/usr/bin" }, platform: "linux", snapshotRoot: fx.snaps, abort: new AbortController() }, ["--workspace"], { resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), acquireOwnership: busy, syncSnapshot: () => { secondSyncs += 1; events.push("sync"); throw new Error("must not sync"); }, runClassA: async () => { events.push("classA"); return 0; }, absoluteEnv: [] });
      second = { code: second, log: log2.join("\n"), events };
    },
  });
  assert.equal(first.code, 0, first.err);
  assert.equal(second.code, 1);
  assert.match(second.log, /BLOCKED: .*held by another build; the route never re-syncs a snapshot another request holds; nothing ran/);
  assert.equal(secondSyncs, 0);
  assert.deepEqual(second.events, []);
});

test("item 8 row 8.8: a sidecar-bound package in the requested scope refuses the whole request before ownership, class A, the sync or any compile; the request is never rewritten", async (t) => {
  const withShell = { ...crateFiles(["a", "b", "s"]), "crates/s/tauri.conf.json": JSON.stringify({ bundle: { externalBin: ["binaries/helper"] } }) };
  const crates = [{ name: "a" }, { name: "b" }, { name: "s" }];
  const refused = (r, bound) => {
    assert.equal(r.code, 1);
    assert.match(r.err, new RegExp(`BLOCKED: RK_CLIPPY_SIDECAR_BOUND: the requested scope holds sidecar-bound package\\(s\\) ${bound}.*item 7 row 7\\.05.*does not rewrite the request.*nothing compiled`));
    assert.match(r.out, /"code":"RK_CLIPPY_SIDECAR_BOUND".*"blockedOn":\["item7:7\.05"\]/);
    assert.deepEqual(r.log.events.filter((e) => ["own", "sync", "classA", "cargo:clippy"].includes(e)), [], "no ownership, sync, class A or compile");
    assert.equal(r.log.cargo.length, 0);
    assert.equal(r.receipt, null);
    assert.ok(!r.log.spawn.some((c) => /stage-sidecar|right-release|pnpm|rk-cargo-shim/.test(c)), "nothing staged and no wrapper installed");
    assert.ok(!fs.existsSync(r.fx.snapshotDir), "no snapshot was synced");
  };
  // Every way the scope reaches the bound package: the workspace, a -p list, the default scope, a glob.
  for (const args of [["--workspace", "--all-targets", "--", "-D", "warnings"], ["-p", "a", "-p", "s"], ["--package=s"], ["-p", "s*"], []]) refused(await route(t, { crates, files: withShell, args, messages: [artifact("a"), finished(true)] }), "s");
  // The refusal reads the commit, not the checkout: a tauri.conf.json only the checkout holds is not a sidecar declaration.
  const clean = await route(t, { crates, files: crateFiles(["a", "b", "s"]), before: (fx) => fs.writeFileSync(path.join(fx.repo, "crates/s/tauri.conf.json"), JSON.stringify({ bundle: { externalBin: ["x"] } })), args: ["--workspace"], messages: [artifact("a"), artifact("b"), artifact("s"), finished(true)] });
  assert.equal(clean.code, 0, clean.err);
  assert.deepEqual(clean.log.cargo[0].args, ["clippy", "--workspace", ...ADDED], "the request is unchanged");
  // Negative: a scope with no sidecar-bound package runs, with the argv the request plus the three additions and nothing else.
  const plain = await route(t, { crates: [{ name: "a" }, { name: "b" }], messages: [artifact("a"), artifact("b"), finished(true)] });
  assert.equal(plain.code, 0);
  assert.deepEqual(plain.log.cargo[0].args, ["clippy", "--workspace", ...ADDED]);
  assert.ok(!/SIDECAR/.test(plain.out + plain.err));
  // -p of an independent package: the bound package outside the requested scope is not considered.
  const scoped = await route(t, { crates, files: withShell, args: ["-p", "a"], messages: [artifact("a"), finished(true)] });
  assert.equal(scoped.code, 0, scoped.err);
  assert.deepEqual(scoped.log.cargo[0].args, ["clippy", "-p", "a", ...ADDED]);
  assert.deepEqual(scoped.receipt.argv.transformations.map((x) => x.arg), ADDED, "only the three additions are ever recorded");
  // The backstop: the checkout's own metadata shows no bound package (it differs from the commit), the snapshot's does: refused before cargo.
  const backstop = await route(t, { crates, files: withShell, args: ["--workspace"], preMetadataCrates: [{ name: "a" }, { name: "b" }], messages: [artifact("a"), finished(true)] });
  assert.equal(backstop.code, 1);
  assert.match(backstop.err, /RK_CLIPPY_SIDECAR_BOUND: .*found in the snapshot, after the checkout's own metadata showed none/);
  assert.equal(backstop.log.cargo.length, 0, "no cargo");
  assert.equal(backstop.receipt, null);
  assert.ok(!backstop.log.spawn.some((c) => /stage-sidecar|right-release|pnpm/.test(c)));
  // The pure helper.
  const scope = { members: [{ name: "a", dir: "crates/a" }, { name: "s", dir: "crates/s" }], all: [], unresolved: [] };
  const files = { "crates/s/tauri.conf.json": '{"bundle":{"externalBin":["b/h"]}}', "crates/a/tauri.conf.json": '{"bundle":{}}' };
  const args = { scope, dirOf: (p) => p.dir, exists: (f) => f in files, readFile: (f) => files[f] };
  assert.deepEqual(sidecarBoundMembers(args), ["s"]);
  assert.deepEqual(sidecarBoundMembers({ ...args, scope: null }), []);
  assert.equal("splitForSidecars" in (await import("./clippy-route.mjs")), false, "the rewrite no longer exists");
});

test("item 8: the entry refuses a bad request through the gate's main with nothing run, and clippy's own -v, -h and --version are cargo's, not the gate's", async () => {
  const calls = [];
  const io = (out, err) => ({ spawn: (...a) => { calls.push(a); return { status: 1, stdout: "" }; }, out: (m) => out.push(m), err: (m) => err.push(m), cwd: os.tmpdir(), env: { PATH: "/usr/bin" }, platform: "linux", acquireOwnership: () => { calls.push("own"); }, syncSnapshot: () => { calls.push("sync"); } });
  for (const [args, code] of [[["build"], ROUTE_CODES.notClippy], [["--message-format=json"], ROUTE_CODES.messageFormat], [["--target-dir", "x"], ROUTE_CODES.targetDir], [["+nightly"], ROUTE_CODES.toolchain], [["--fix"], ROUTE_CODES.fix], [["-Z", "x"], ROUTE_CODES.unstable], [["--manifest-path", "/a/Cargo.toml"], ROUTE_CODES.manifestAbsolute]]) {
    const out = []; const err = [];
    assert.equal(await main(["clippy", ...args], io(out, err)), 2, JSON.stringify(args));
    assert.match(err.join("\n"), new RegExp(`${code}: .*nothing ran`), JSON.stringify(args));
  }
  assert.deepEqual(calls, [], "no process, no ownership, no sync: nothing started");
  for (const flag of ["-h", "--help", "-V", "--version"]) {
    const out = []; const err = [];
    assert.equal(await main(["clippy", flag], io(out, err)), 0, flag);
    assert.deepEqual(out, [CLIPPY_USAGE], `${flag} prints the route's usage, not the gate's version or usage`);
  }
  assert.deepEqual(calls, []);
  // `-v` is cargo's verbose flag: it reaches the route (here: not a git repository), it never prints the gate's version.
  const out = []; const err = [];
  assert.equal(await main(["clippy", "-v"], io(out, err)), 2);
  assert.deepEqual(out, []);
  assert.match(err.join("\n"), /not inside a git repository; nothing ran/);
});

test("item 8: a relative --manifest-path stays inside the snapshot; one that leaves the checkout's tree is refused before the sync", async (t) => {
  const r = await route(t, { args: ["--manifest-path", "../../outside/Cargo.toml"], cwd: null, messages: [] });
  assert.equal(r.code, 2);
  assert.match(r.err, /RK_CLIPPY_MANIFEST_ESCAPES/);
  assert.deepEqual(r.log.events, []);
  const sub = await route(t, { crates: [{ name: "a" }], args: ["--manifest-path", "crates/a/Cargo.toml"], messages: [artifact("a"), finished(true)] });
  assert.equal(sub.code, 0, sub.err);
  assert.deepEqual(sub.log.cargo[0].args.slice(0, 3), ["clippy", "--manifest-path", "crates/a/Cargo.toml"]);
});

test("item 8 scope: -p, --workspace, --exclude, globs and the default scope resolve against the snapshot's metadata", () => {
  const meta = metadataOf("/w", [{ name: "rightkit-a" }, { name: "rightkit-b", deps: ["rightkit-a"] }, { name: "tool" }]);
  const names = (s) => s.members.map((p) => p.name);
  assert.deepEqual(names(resolveScope({ before: ["--workspace"], metadata: meta, directory: "/w" })), ["rightkit-a", "rightkit-b", "tool"]);
  assert.deepEqual(names(resolveScope({ before: ["--workspace", "--exclude", "tool"], metadata: meta, directory: "/w" })), ["rightkit-a", "rightkit-b"]);
  assert.deepEqual(names(resolveScope({ before: ["--all", "--exclude=rightkit-*"], metadata: meta, directory: "/w" })), ["tool"]);
  assert.deepEqual(names(resolveScope({ before: ["-p", "rightkit-a", "--package=tool", "-prightkit-b"], metadata: meta, directory: "/w" })), ["rightkit-a", "tool", "rightkit-b"]);
  assert.deepEqual(names(resolveScope({ before: ["-p", "rightkit-*"], metadata: meta, directory: "/w" })), ["rightkit-a", "rightkit-b"]);
  assert.deepEqual(names(resolveScope({ before: ["-p", "rightkit-a@0.1.0"], metadata: meta, directory: "/w" })), ["rightkit-a"]);
  const missing = resolveScope({ before: ["-p", "nope", "-p", "tool"], metadata: meta, directory: "/w" });
  assert.deepEqual(missing.unresolved, ["nope"]);
  assert.deepEqual(names(missing), ["tool"]);
  assert.deepEqual(names(resolveScope({ before: [], metadata: meta, directory: path.join("/w", "crates", "tool") })), ["tool"], "a member's directory selects that member");
  assert.deepEqual(names(resolveScope({ before: [], metadata: meta, directory: path.join("/w") })), ["rightkit-a", "rightkit-b", "tool"], "the root selects the default members");
  // A dependent whose only failed dependency is a member is blocked by it, transitively.
  const s = summarizeRun({ messages: [diag("rightkit-a", "E0001"), finished(false)], exit: 101, scope: resolveScope({ before: ["--workspace"], metadata: metadataOf("/w", [{ name: "rightkit-a" }, { name: "mid", deps: ["rightkit-a"] }, { name: "top", deps: ["mid"] }, { name: "tool" }]), directory: "/w" }) });
  assert.deepEqual(s.blocked.map((b) => [b.package, b.blockedBy]).sort(), [["mid", ["rightkit-a"]], ["tool", []], ["top", ["rightkit-a"]]].sort(), "an unrelated package that built nothing is blocked with no named cause");
});

// ---- rows 8.7 and 8.16: batch equivalence ---------------------------------------------------------------------------------------------

const TC = "stable|cargo:c1|rustc:r1";
const group = (id, crates, { batchMsgs, perMsgs, batchToolchain = TC, perToolchain = TC, batchExit = 0, perExit = 0 }) => ({ id, units: crates, batch: { messages: batchMsgs, toolchain: batchToolchain, exit: batchExit }, perCrate: crates.map((c) => ({ unit: c, crate: c, messages: perMsgs[c], toolchain: perToolchain, exit: perExit })) });

test("item 8 row 8.7: a batch whose diagnostics and resolved features equal the per-crate runs' is equivalent (an empty diff)", () => {
  const msgs = (c) => [artifact(c, { features: ["default"] }), diag(c, "clippy::needless_return", { level: "warning" }), artifact("serde", { features: ["std"] })];
  const r = compareEquivalence({ groups: [group("g", ["a", "b", "c"], { batchMsgs: ["a", "b", "c"].flatMap(msgs), perMsgs: { a: msgs("a"), b: msgs("b"), c: msgs("c") } })] });
  assert.equal(r.equivalent, true, r.failures.join("; "));
  assert.deepEqual(r.groups[0].diagnostics.onlyInBatch, []);
  assert.deepEqual(r.groups[0].diagnostics.onlyInPerCrate, []);
  assert.equal(r.groups[0].diagnostics.batch, 3);
  assert.deepEqual(r.groups[0].features.unified, []);
  assert.equal(r.separate.size, 0);
});

test("item 8 row 8.7: a diagnostic only one side has, or a different span, makes the comparison fail", () => {
  const a = [artifact("a"), diag("a", "clippy::x", { line: 3 })];
  const missing = compareEquivalence({ groups: [group("g", ["a"], { batchMsgs: [artifact("a")], perMsgs: { a } })] });
  assert.equal(missing.equivalent, false);
  assert.equal(missing.groups[0].diagnostics.onlyInPerCrate.length, 1);
  const moved = compareEquivalence({ groups: [group("g", ["a"], { batchMsgs: [artifact("a"), diag("a", "clippy::x", { line: 4 })], perMsgs: { a } })] });
  assert.equal(moved.equivalent, false);
  assert.equal(moved.groups[0].diagnostics.onlyInBatch.length, 1);
  assert.match(moved.failures.join(" "), /diagnostics differ/);
  // A diagnostic of a package outside the batch (another member's run) is not this comparison's.
  const foreign = compareEquivalence({ groups: [group("g", ["a"], { batchMsgs: [artifact("a"), diag("other", "clippy::y")], perMsgs: { a: [artifact("a"), diag("other", "clippy::z")] } })] });
  assert.equal(foreign.equivalent, true);
});

test("item 8 row 8.7: a feature-unified fixture where batching changes what compiles is detected and kept separate", () => {
  // b depends on a with an extra feature: batched, a is built once with it; on its own, a is built without.
  const perA = [artifact("a", { features: ["default"] })];
  const perB = [artifact("b", { features: ["default"] }), artifact("a", { features: ["default", "extra"] })];
  const batch = [artifact("a", { features: ["default", "extra"] }), artifact("b", { features: ["default"] })];
  const r = compareEquivalence({ groups: [group("g", ["a", "b"], { batchMsgs: batch, perMsgs: { a: perA, b: perB } })] });
  assert.equal(r.equivalent, false);
  assert.deepEqual(r.groups[0].features.unified.map((u) => [u.unit, u.batchFeatures, u.perCrateFeatures]), [["a", ["default", "extra"], ["default"]]]);
  assert.deepEqual([...r.separate.keys()], ["a"], "only the crate whose compile changed is kept separate");
  assert.match(r.separate.get("a"), /feature unification/);
  // The planner then keeps it as its own invocation, and the re-run is equivalent.
  const split = compareEquivalence({ groups: [group("g1", ["a"], { batchMsgs: perA, perMsgs: { a: perA } }), group("g2", ["b"], { batchMsgs: perB, perMsgs: { b: perB } })] });
  assert.equal(split.equivalent, true);
  // A batch that fails because of a diagnostic of a member (the unified lint) is judged by the diagnostics and the unified feature set:
  // only the unified crate is kept separate, not every member.
  const lintBatch = compareEquivalence({ groups: [group("g", ["a", "b"], { batchMsgs: [artifact("a", { features: ["default", "extra"] }), diag("a", "clippy::needless_return"), artifact("b", { features: ["default"] })], perMsgs: { a: perA, b: perB }, batchExit: 101, perExit: 0 })] });
  assert.deepEqual([...lintBatch.separate.keys()], ["a"]);
  assert.equal(lintBatch.groups[0].diagnostics.onlyInBatch.length, 1);
  // A batch that fails to build where every per-crate run builds, and no diagnostic explains it: its features cannot all be on together.
  const exclusive = compareEquivalence({ groups: [group("g", ["a", "b"], { batchMsgs: [], perMsgs: { a: perA, b: perA }, batchExit: 101, perExit: 0 })] });
  assert.equal(exclusive.equivalent, false);
  assert.deepEqual([...exclusive.separate.keys()].sort(), ["a", "b"]);
  assert.match(exclusive.separate.get("a"), /fails to build where every per-crate run builds/);
  // Third-party packages built with a union of features are recorded, not judged.
  const dep = compareEquivalence({ groups: [group("g", ["a", "b"], { batchMsgs: [artifact("a"), artifact("b"), artifact("serde", { features: ["derive", "std"] })], perMsgs: { a: [artifact("a"), artifact("serde", { features: ["std"] })], b: [artifact("b"), artifact("serde", { features: ["derive"] })] } })] });
  assert.equal(dep.equivalent, true);
  assert.deepEqual(dep.groups[0].dependenciesBuiltWithOtherFeatures, [pkgId("serde")]);
});

test("item 8 row 8.7: a member blocked in the batch by a failing member is the route's blocked state, not a difference; one the batch never built with nothing failing is", () => {
  // a fails Clippy in both runs. b depends on a: its per-crate run lints only b and builds, the batch builds nothing for it.
  const a = [diag("a", "clippy::needless_return")];
  const perB = [artifact("b", { features: ["default"] }), diag("b", "clippy::redundant_clone", { level: "warning" })];
  const blocked = compareEquivalence({ groups: [group("g", ["a", "b", "c"], { batchMsgs: [...a, artifact("c", { features: ["default"] })], perMsgs: { a, b: perB, c: [artifact("c", { features: ["default"] })] }, batchExit: 101, perExit: 0 })] });
  assert.equal(blocked.equivalent, true, blocked.failures.join("; "));
  assert.deepEqual(blocked.groups[0].blockedInBatch, ["b"]);
  assert.equal(blocked.groups[0].diagnosticsOfBlockedMembers, 1, "b's own warning is the coverage the batch could not collect, recorded");
  assert.equal(blocked.separate.size, 0);
  // The same batch with no failing member: b not built is a difference, b is kept separate.
  const unbuilt = compareEquivalence({ groups: [group("g", ["b", "c"], { batchMsgs: [artifact("c", { features: ["default"] })], perMsgs: { b: perB, c: [artifact("c", { features: ["default"] })] }, batchExit: 0, perExit: 0 })] });
  assert.equal(unbuilt.equivalent, false);
  assert.deepEqual([...unbuilt.separate.keys()], ["b"]);
  assert.match(unbuilt.separate.get("b"), /never built b although nothing in it failed/);
  // A member that neither run builds (a build script failing on this host) is the same in both: not a difference.
  const neither = compareEquivalence({ groups: [group("g", ["q"], { batchMsgs: [], perMsgs: { q: [] }, batchExit: 101, perExit: 101 })] });
  assert.equal(neither.equivalent, true, neither.failures.join("; "));
  assert.equal(neither.separate.size, 0);
  // A diagnostic only the batch has keeps that crate separate (not the whole batch).
  const only = compareEquivalence({ groups: [group("g", ["a", "c"], { batchMsgs: [diag("a", "clippy::x"), artifact("c")], perMsgs: { a: [artifact("a")], c: [artifact("c")] }, batchExit: 101, perExit: 0 })] });
  assert.deepEqual([...only.separate.keys()], ["a"]);
  assert.match(only.separate.get("a"), /changed the diagnostics of a/);
});

test("item 8 row 8.16: the gate compares the toolchain identity of the batch and every per-crate run; a different or missing one fails the equivalence run", () => {
  const msgs = [artifact("a")];
  const base = { batchMsgs: msgs, perMsgs: { a: msgs } };
  assert.equal(compareEquivalence({ groups: [group("g", ["a"], base)] }).equivalent, true);
  const other = compareEquivalence({ groups: [group("g", ["a"], { ...base, perToolchain: "stable|cargo:c2|rustc:r1" })] });
  assert.equal(other.equivalent, false);
  assert.equal(other.groups[0].toolchain.equal, false);
  assert.match(other.failures.join(" "), /toolchain identity of the batch and its per-crate runs is not one equal, present string/);
  for (const bad of [null, "", "  "]) {
    const r = compareEquivalence({ groups: [group("g", ["a"], { ...base, batchToolchain: bad })] });
    assert.equal(r.equivalent, false, `toolchain ${JSON.stringify(bad)}`);
    assert.match(r.failures.join(" "), /toolchain identity/);
  }
  const absent = group("g", ["a"], base); delete absent.batch.toolchain; delete absent.perCrate[0].toolchain;
  assert.equal(compareEquivalence({ groups: [absent] }).equivalent, false, "an absent identity is not an equal one");
  const mixed = group("g", ["a", "b"], { batchMsgs: msgs, perMsgs: { a: msgs, b: msgs } });
  mixed.perCrate[1].toolchain = "stable|cargo:c3|rustc:r1";
  assert.equal(compareEquivalence({ groups: [mixed] }).equivalent, false, "one per-crate run on another toolchain fails the group");
});

test("the route's cargo and wrapper use: the runtime wrapper's directory holds the first cargo on PATH and is removed afterwards", async (t) => {
  const r = await route(t, { messages: [artifact("a"), finished(true)] });
  const env = r.log.cargo[0].env;
  const first = env.PATH.split(path.delimiter)[0];
  assert.match(first, /rk-cargo-shim-[0-9a-f]{16}[\\/]bin$/);
  assert.equal(env.CARGO, r.log.cargo[0].file);
  assert.ok(!fs.existsSync(path.dirname(first)), "the wrapper directory is removed when the route ends");
  assert.equal(crypto.createHash("sha256").update(r.receipt.cargoWrapper.identity).digest("hex").length, 64);
});

// ---- item 8 row 8.15 support: the result cache keys a batch on its members ------------------------------------------------------------

test("item 8 row 8.15 support: a batched Clippy scan's cache key is the union of its members' own keys, never the whole tree", async () => {
  const { buildCacheDigest } = await import("./bin/rightkit-gate.mjs");
  const { parseLock } = await import("./lock-graph.mjs");
  const lockPkg = (name, deps = []) => `[[package]]\nname = "${name}"\nversion = "1.0.0"\n${deps.length ? `dependencies = [\n${deps.map((d) => ` "${d}",`).join("\n")}\n]\n` : ""}\n`;
  const lock = parseLock(lockPkg("a", ["d"]) + lockPkg("b") + lockPkg("c") + lockPkg("d"));
  const units = ["a", "b", "c", "d"].map((n) => ({ name: n, dir: `crates/${n}` }));
  const ids = { "crates/a": "ta", "crates/b": "tb", "crates/c": "tc", "crates/d": "td", "Cargo.lock": "lock", "Cargo.toml": "root", ".cargo": "cargo", "deny.toml": "deny", "ci/gate.config.json": "cfg", vendor: "v" };
  const sources = { listFiles: (dir) => [`${dir}/src/lib.rs`], readBlob: () => "pub fn f() {}\n" };
  const digestOf = (over = {}, tree = "head") => buildCacheDigest({ objectId: (p) => ({ ...ids, ...over })[p] ?? null, headTree: tree, rustUnits: units, cargoDir: ".", rustLock: lock, ...sources });
  const batch = (members, id = "clippy:batch:0a1b2c3d") => ({ id, class: "B", batch: { units: members, order: "independent", reason: null } });
  const ab = batch(["a", "b"]);
  const base = digestOf()(ab);
  assert.equal(typeof base, "string");
  assert.equal(digestOf()(ab), base, "identical inputs");
  assert.equal(digestOf({}, "another-head")(ab), base, "not the whole tree: another HEAD tree id alone changes nothing");
  assert.equal(digestOf({ "crates/c": "tc2" })(ab), base, "a crate outside the batch and its closure");
  assert.notEqual(digestOf({ "crates/a": "ta2" })(ab), base, "a member changed");
  assert.notEqual(digestOf({ "crates/b": "tb2" })(ab), base, "another member changed");
  assert.notEqual(digestOf({ "crates/d": "td2" })(ab), base, "a dependency in a member's lock closure changed");
  // The members matter, not just the id: the same id over other members is another key.
  assert.notEqual(digestOf()(batch(["a", "c"])), base);
  assert.notEqual(digestOf()(batch(["b"])), base);
  // Its own key differs from either member's per-crate key (a batch is not a per-crate scan).
  assert.notEqual(digestOf()({ id: "clippy:a", class: "B" }), base);
  // A member the planner does not know, or no member list: the safe whole-tree fallback (another HEAD tree id changes the key).
  for (const odd of [batch(["a", "nope"]), { id: "clippy:batch:0a1b2c3d", class: "B" }, batch([])]) assert.notEqual(digestOf({}, "t1")(odd), digestOf({}, "t2")(odd), JSON.stringify(odd.batch ?? null));
});

test("D4 (item 5): the clippy route maps build ownership's managed-root refusal to BLOCKED, names it, and syncs and creates nothing", async (t) => {
  const { acquireBuildOwnership } = await import("./build-ownership.mjs");
  const absent = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-clippy-d4-")), "managed");
  t.after(() => fs.rmSync(path.dirname(absent), { recursive: true, force: true }));
  const r = await route(t, { messages: [finished(true)], acquire: (q) => acquireBuildOwnership({ ...q, platform: "darwin", managedRoot: absent, device: (p) => (p === "/" ? 1n : 2n), env: {}, exportEnv: false }) });
  assert.equal(r.code, 1, r.err);
  assert.match(r.err, /BLOCKED: .*managed root .* does not exist.*nothing ran, nothing created/);
  assert.equal(r.log.events.includes("sync"), false, "no snapshot was synced");
  assert.equal(fs.existsSync(absent), false, "the managed root was not created");
});
