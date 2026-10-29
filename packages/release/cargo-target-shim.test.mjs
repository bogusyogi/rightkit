// Item 7: the runtime cargo wrapper of a commit-snapshot compile binds each cargo invocation to its own workspace's checkout target
// directory, or refuses it (GATE-CONTRACT §1 "Broker interface", output directory; row E16).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { SHIM_REFUSAL_STATUS, decideCargoInvocation, installCargoShim, shimScanEnv } from "./cargo-target-shim.mjs";
import { inheritedEnvDigest, planScans, scanCacheKey } from "./deterministic-scans.mjs";
import { verifySnapshotDir } from "./snapshot-check.mjs";
import { main } from "./bin/rightkit-gate.mjs";

const POSIX = process.platform === "win32" ? "POSIX wrapper: a Windows child finds only an .exe on PATH" : false;

function tmp(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-cargo-shim-test-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}
function write(root, files) {
  for (const [rel, body] of Object.entries(files)) {
    const f = path.join(root, rel);
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, body);
  }
}

// Two independent workspaces A (the root) and B (native/), and their checkout target directories.
function mixedSnapshot(t) {
  const dir = tmp(t);
  const snap = path.join(dir, "snap");
  write(snap, {
    "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n",
    "native/Cargo.toml": '[workspace]\nmembers = []\n[package]\nname = "b"\nversion = "0.1.0"\n', "native/src/lib.rs": "\n",
  });
  const targets = { ".": path.join(dir, "checkout", "target"), native: path.join(dir, "checkout", "native", "target") };
  return { dir, snap, targets };
}

// A locate-project stand-in: the nearest enclosing directory whose Cargo.toml declares [workspace].
const locateIn = (manifestPath, cwd) => {
  for (let d = manifestPath ? path.dirname(manifestPath) : cwd; ; d = path.dirname(d)) {
    const m = path.join(d, "Cargo.toml");
    if (fs.existsSync(m) && /\[workspace\]/.test(fs.readFileSync(m, "utf8"))) return m;
    if (path.dirname(d) === d) throw new Error("could not find `Cargo.toml`");
  }
};

test("[behavior] item 7 wrapper: each cargo invocation binds its own workspace's checkout target; unknown workspaces, redirects and unbound commands are refused", (t) => {
  const { dir, snap, targets } = mixedSnapshot(t);
  const map = { shim: "/gate/bin/cargo", workspaces: { [snap]: targets["."], [path.join(snap, "native")]: targets.native } };
  const decide = (args, cwd) => decideCargoInvocation({ args, cwd, env: { PATH: "/bin" }, map, locate: locateIn });
  // Mixed A+B: each binds its own, never the other's.
  const a = decide(["test", "--locked", "-p", "a"], path.join(snap, "crates", "a"));
  assert.equal(a.exec?.env.CARGO_TARGET_DIR, targets["."]); assert.equal(a.exec.env.CARGO_BUILD_TARGET_DIR, targets["."]);
  const b = decide(["build", "--locked"], path.join(snap, "native", "src"));
  assert.equal(b.exec?.env.CARGO_TARGET_DIR, targets.native);
  // The real cargo runs with CARGO naming the wrapper, so its build scripts and tests ($CARGO, env!("CARGO")) come back to it.
  assert.equal(a.exec.env.CARGO, "/gate/bin/cargo"); assert.equal(b.exec.env.CARGO, "/gate/bin/cargo");
  assert.equal(decide(["fetch", "--locked"], snap).exec?.env.CARGO_TARGET_DIR, targets["."], "a locked fetch is bound to its workspace");
  assert.equal(decide(["--frozen", "fetch"], path.join(snap, "native")).exec?.env.CARGO_TARGET_DIR, targets.native);
  assert.equal(decide(["+stable", "--locked", "check", "--manifest-path", "native/Cargo.toml"], snap).exec?.env.CARGO_TARGET_DIR, targets.native, "--manifest-path and +toolchain resolve too");
  assert.equal(decide(["clippy", "--", "-D", "warnings", "-C", "x"], snap).exec?.env.CARGO_TARGET_DIR, targets["."], "arguments after -- are not cargo's");
  assert.equal(decide(["build", "--target-dir", targets["."]], snap).exec?.env.CARGO_TARGET_DIR, targets["."], "an explicit --target-dir equal to the bound one is accepted");
  // Unknown workspaces: outside the snapshot, a fixture workspace the compile did not resolve, no workspace at all.
  write(dir, { "outside/Cargo.toml": "[workspace]\n", "snap/fixtures/f/Cargo.toml": "[workspace]\n" });
  for (const cwd of [path.join(dir, "outside"), path.join(snap, "fixtures", "f"), os.tmpdir()]) {
    const r = decide(["build"], cwd);
    assert.equal(r.refuse?.code, "RK_CARGO_UNBOUND_WORKSPACE", `${cwd}: ${JSON.stringify(r)}`);
  }
  // Redirects and commands that write no bound output or rewrite the commit.
  for (const args of [["--config", "build.target-dir='x'", "build"], ["-C", "native", "build"], ["-Zunstable-options", "build"], ["-vC", "x", "build"], ["build", "--target-dir", "/tmp/x"], ["build", "--target-dir=rel"], ["test", "--config=x"]]) {
    assert.equal(decide(args, snap).refuse?.code, "RK_CARGO_REDIRECT", args.join(" "));
  }
  for (const sub of ["clean", "install", "publish", "update", "generate-lockfile", "xtask", "make", "nextest"]) {
    assert.equal(decide([sub], snap).refuse?.code, "RK_CARGO_UNBOUND_COMMAND", sub);
  }
  // Review item7-wrapper-r1 #7: a fetch that may rewrite Cargo.lock, a fetch outside every bound workspace, and fmt that rewrites
  // tracked files are refused.
  assert.equal(decide(["fetch"], snap).refuse?.code, "RK_CARGO_UNBOUND_COMMAND", "fetch without --locked");
  assert.equal(decide(["fetch", "--locked"], path.join(dir, "outside")).refuse?.code, "RK_CARGO_UNBOUND_WORKSPACE", "fetch outside the snapshot");
  for (const args of [["fmt"], ["fmt", "--all"], ["+nightly", "fmt", "--", "--emit", "files"]]) assert.equal(decide(args, snap).refuse?.code, "RK_CARGO_UNBOUND_COMMAND", args.join(" "));
  // Commands with no compile output run unchanged, with no workspace needed.
  for (const args of [["--version"], ["-V"], ["version"], ["fmt", "--check"], ["fmt", "--all", "--check"], ["deny", "check"], ["locate-project"]]) {
    const r = decide(args, os.tmpdir());
    assert.ok(r.exec && r.exec.env.CARGO_TARGET_DIR === undefined && r.exec.env.CARGO === "/gate/bin/cargo", `${args.join(" ")}: ${JSON.stringify(r)}`);
  }
});

test("[behavior] item 7 wrapper (real chain): a `node --test` file spawning cargo in workspaces A and B writes each to its own checkout target; an unknown workspace fails the unit", { skip: POSIX }, (t) => {
  const { dir, snap, targets } = mixedSnapshot(t);
  // The real cargo stand-in: answers locate-project like cargo, and records every other invocation with its target directory.
  const log = path.join(dir, "cargo-ran");
  const realCargo = path.join(dir, "toolchain", "cargo");
  write(path.join(dir, "toolchain"), {
    cargo: `#!/bin/sh
if [ "$1" = "locate-project" ]; then
  d="$PWD"; while [ "$d" != "/" ]; do if [ -f "$d/Cargo.toml" ] && grep -q '\\[workspace\\]' "$d/Cargo.toml"; then echo "$d/Cargo.toml"; exit 0; fi; d=$(dirname "$d"); done
  echo "error: could not find \\\`Cargo.toml\\\`" >&2; exit 101
fi
echo "$* | $(pwd -P) | CARGO_TARGET_DIR=$CARGO_TARGET_DIR" >> '${log}'
exit 0
`,
  });
  fs.chmodSync(realCargo, 0o755);
  const installed = installCargoShim({ base: path.join(dir, "shim"), snapshotRoot: snap, targetDirs: targets, realCargo });
  assert.deepEqual(installed.workspaces, { [snap]: targets["."], [path.join(snap, "native")]: targets.native });
  write(dir, { "outside/Cargo.toml": "[workspace]\n" });
  write(snap, {
    "tests/spawns.test.mjs": `import { spawnSync } from "node:child_process";
import test from "node:test";
import assert from "node:assert/strict";
test("cargo in A then B", () => {
  for (const cwd of ["crates/a", "native"]) { const r = spawnSync("cargo", ["build", "--locked"], { cwd, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); }
});
test("cargo in an unknown workspace", () => {
  const r = spawnSync("cargo", ["build"], { cwd: ${JSON.stringify(path.join(dir, "outside"))}, encoding: "utf8" });
  console.log("UNKNOWN", r.status, r.stderr.trim());
  assert.equal(r.status, 0, "this one fails the unit");
});
`,
  });
  const env = { ...process.env, PATH: `${installed.bin}${path.delimiter}${process.env.PATH}`, NODE_OPTIONS: "" };
  delete env.CARGO_TARGET_DIR; delete env.NODE_TEST_CONTEXT;
  const r = spawnSync(process.execPath, ["--test", "tests/spawns.test.mjs"], { cwd: snap, encoding: "utf8", env });
  assert.notEqual(r.status, 0, `the unit fails: one cargo was refused\n${r.stdout}\n${r.stderr}`);
  assert.match(r.stdout, new RegExp(`UNKNOWN ${SHIM_REFUSAL_STATUS} rightkit-gate: RK_CARGO_UNBOUND_WORKSPACE: \`cargo build\` builds the Cargo workspace ${path.join(dir, "outside").replaceAll("/", "\\/")}`), r.stdout);
  assert.match(r.stdout, /✔ cargo in A then B/, "the A+B test passed");
  const ran = fs.readFileSync(log, "utf8").trim().split("\n");
  assert.deepEqual(ran, [
    `build --locked | ${path.join(snap, "crates", "a")} | CARGO_TARGET_DIR=${targets["."]}`,
    `build --locked | ${path.join(snap, "native")} | CARGO_TARGET_DIR=${targets.native}`,
  ], "each real cargo ran bound to its own workspace's checkout target; the refused one never ran");
  assert.equal(fs.existsSync(path.join(snap, "target")) || fs.existsSync(path.join(snap, "native", "target")), false, "the snapshot holds no target/");
});

test("[behavior] item 7 wrapper: with the wrapper installed the static walk is advisory; a `node --test` unit that reaches cargo is planned, not refused", () => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-cargo-shim-plan-")));
  try {
    write(dir, {
      "package.json": '{"name":"root","private":true}\n', "pnpm-workspace.yaml": "packages:\n  - packages/*\n",
      "packages/js/package.json": '{"name":"js","private":true,"scripts":{"test":"node --test"}}\n',
      "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n',
    });
    const files = ["package.json", "pnpm-workspace.yaml", "packages/js/package.json", "Cargo.toml", "crates/a/Cargo.toml"];
    const plan = (cargoShim) => planScans({ changed: files, contentRoot: dir, liveRoot: dir, trustedRoot: dir, includeTests: true, quietTests: "local", tier: "compile", cargoCommand: ["cargo"],
      rustUnits: [{ name: "a", dir: "crates/a", crate: "a", cargoDir: "." }], npmUnits: [{ name: "js", dir: "packages/js" }], npmRoots: ["."], liveExecution: true,
      targetDirs: { ".": "/checkout/target" }, cargoShim, platform: process.platform });
    const without = plan(false);
    assert.equal(without.scans.some((s) => s.id === "test:npm:js"), false, "without the wrapper the static walk still refuses");
    const withShim = plan(true);
    const row = withShim.scans.find((s) => s.id === "test:npm:js");
    assert.ok(row, withShim.scans.map((s) => `${s.id} ${s.detail ?? ""}`).join("\n"));
    assert.equal(row.env.CARGO_TARGET_DIR, undefined, "the wrapper binds per invocation, not one directory for the whole test");
    assert.equal(withShim.scans.some((s) => s.internal === "refuse"), false);
    assert.ok(withShim.notes.some((n) => /npm unit js: the static walk cannot prove .*runtime cargo wrapper binds each invocation/.test(n)), withShim.notes.join("\n"));
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("[behavior] item 7 wrapper: the door's snapshot compile puts the wrapper first on PATH for every scan and records it in the direct receipt", { skip: POSIX }, async (t) => {
  const dir = tmp(t);
  const repo = path.join(dir, "repo");
  write(repo, { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n", "README.md": "r\n" });
  const g = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  g("init", "-q", "-b", "main"); g("add", "-A"); g("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c");
  const sha = g("rev-parse", "HEAD").stdout.trim();
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const checkoutTarget = path.join(repo, "target");
  const fakeCargo = path.join(dir, "toolchain", "cargo");
  const spawn = (c, a, opts) => {
    if (c === "git") return spawnSync(c, a, { ...opts, encoding: "utf8" });
    if (c === "which" && a[0] === "cargo") return { status: 0, stdout: `${fakeCargo}\n`, stderr: "" };
    if (c === "cargo" && a.includes("metadata")) return { status: 0, stdout: JSON.stringify({ target_directory: checkoutTarget, workspace_root: opts.cwd, packages: [{ name: "a", manifest_path: path.join(opts.cwd, "crates", "a", "Cargo.toml") }] }), stderr: "" };
    return { status: 1, stdout: "", stderr: "not in this fixture" };
  };
  const seen = [];
  const out = [];
  const code = await main(["--to", "compile", "--commit", sha], {
    cwd: repo, platform: "linux", snapshotRoot: path.join(dir, "snaps"), env, spawn, out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {},
    runScans: async (plan, options) => {
      const bin = options.env?.PATH?.split(path.delimiter)[0];
      seen.push({ plan, bin, env: options.env, shim: bin ? fs.readFileSync(path.join(bin, "cargo"), "utf8") : null, map: bin ? JSON.parse(fs.readFileSync(path.join(path.dirname(bin), "cargo-target-map.json"), "utf8")) : null });
      return { scans: [], notes: [], cacheHits: [] };
    },
  });
  const text = out.join("\n");
  assert.equal(code, 0, text);
  const run = seen.find((s) => s.plan);
  assert.ok(run?.bin, `the scans ran with the wrapper first on PATH\n${text}`);
  assert.match(run.shim, /cargo-target-shim\.mjs' --map /);
  assert.equal(run.map.realCargo, fakeCargo);
  const snapRoot = Object.keys(run.map.workspaces)[0];
  assert.deepEqual(Object.values(run.map.workspaces), [checkoutTarget], "the snapshot's workspace maps to the checkout's target");
  assert.notEqual(snapRoot, fs.realpathSync(repo), "keyed by the snapshot's workspace, not the checkout's");
  assert.match(text, /runtime cargo wrapper .* each cargo invocation binds its workspace's checkout target directory or is refused/);
  const receipt = JSON.parse(text.match(/direct receipt (\{.*\})/)?.[1] ?? "null");
  assert.deepEqual(receipt?.cargoWrapper, { identity: run.map.identity, workspaces: run.map.workspaces });
  // Every scan starts with the wrapper named in CARGO and, one workspace here, its checkout target as the scan-wide default.
  assert.equal(run.env.CARGO, path.join(run.bin, "cargo")); assert.equal(run.env.CARGO_TARGET_DIR, checkoutTarget);
  assert.match(path.basename(path.dirname(run.bin)), new RegExp(`^rk-cargo-shim-${run.map.identity.slice(0, 16)}$`), "the wrapper directory is named by its identity");
  assert.equal(fs.existsSync(path.dirname(run.bin)), false, "the wrapper directory is removed after the compile");
});

test("[behavior] item 7 wrapper (review r1 #1): a build script's $CARGO comes back through the wrapper; a cargo that bypasses PATH and CARGO still writes to the one workspace's checkout target; in a multi-workspace snapshot a bypass that writes into the snapshot fails the after-compile verification", { skip: POSIX }, (t) => {
  const dir = tmp(t);
  const files = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
  const snap = path.join(dir, "snap"); write(snap, files);
  const target = path.join(dir, "checkout", "target");
  const log = path.join(dir, "cargo-ran");
  const realCargo = path.join(dir, "toolchain", "cargo");
  // The real cargo stand-in: answers locate-project, logs every other call with its target and CARGO, and on `build` runs "$CARGO
  // check --locked" once, as a build script or test that shells out to $CARGO would.
  write(path.join(dir, "toolchain"), {
    cargo: `#!/bin/sh
if [ "$1" = "locate-project" ]; then
  d="$PWD"; while [ "$d" != "/" ]; do if [ -f "$d/Cargo.toml" ] && grep -q '\\[workspace\\]' "$d/Cargo.toml"; then echo "$d/Cargo.toml"; exit 0; fi; d=$(dirname "$d"); done
  exit 101
fi
echo "$* | CARGO_TARGET_DIR=$CARGO_TARGET_DIR | CARGO=$CARGO" >> '${log}'
if [ "$1" = "build" ] && [ -z "$RK_TEST_NESTED" ]; then RK_TEST_NESTED=1 "$CARGO" check --locked || exit $?; fi
exit 0
`,
  });
  fs.chmodSync(realCargo, 0o755);
  const installed = installCargoShim({ base: path.join(dir, "shim"), snapshotRoot: snap, targetDirs: { ".": target }, realCargo });
  const clean = { ...process.env, NODE_OPTIONS: "" }; delete clean.CARGO_TARGET_DIR; delete clean.CARGO; delete clean.RK_TEST_NESTED;
  const scanEnv = shimScanEnv(clean, installed);
  assert.equal(scanEnv.CARGO, installed.shim); assert.equal(scanEnv.CARGO_TARGET_DIR, target, "one workspace: its target is the scan-wide default");
  // Through PATH: bound, and the nested $CARGO call is the wrapper again (it runs locate-project, then the real cargo, bound).
  // (The test runner's broker guard refuses a bare `cargo`; the wrapper's path is what PATH resolves `cargo` to in the scan.)
  let r = spawnSync(installed.shim, ["build", "--locked"], { cwd: path.join(snap, "crates", "a"), env: scanEnv, encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  // Bypasses: the real cargo by absolute path (as `rustup run` or ~/.cargo/bin/cargo would), and with PATH reset.
  r = spawnSync(realCargo, ["test", "--no-run"], { cwd: snap, env: { ...scanEnv, RK_TEST_NESTED: "1" }, encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  r = spawnSync(realCargo, ["clippy"], { cwd: snap, env: { ...scanEnv, PATH: "/usr/bin:/bin", RK_TEST_NESTED: "1" }, encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(fs.readFileSync(log, "utf8").trim().split("\n"), [
    `build --locked | CARGO_TARGET_DIR=${target} | CARGO=${installed.shim}`,
    `check --locked | CARGO_TARGET_DIR=${target} | CARGO=${installed.shim}`,
    `test --no-run | CARGO_TARGET_DIR=${target} | CARGO=${installed.shim}`,
    `clippy | CARGO_TARGET_DIR=${target} | CARGO=${installed.shim}`,
  ], "the nested $CARGO call was bound by the wrapper; both bypasses inherited the one workspace's checkout target");

  // Two workspaces: no single default exists, so a bypass is unbound; whatever it writes into the snapshot is caught after the compile.
  const multi = { ...files, "native/Cargo.toml": '[workspace]\nmembers = []\n[package]\nname = "b"\nversion = "0.1.0"\n', "native/src/lib.rs": "\n" };
  const checkout = path.join(dir, "repo"); write(checkout, multi);
  const g = (...a) => spawnSync("git", a, { cwd: checkout, encoding: "utf8" });
  g("init", "-q", "-b", "main"); g("add", "-A"); g("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c");
  const commit = g("rev-parse", "HEAD").stdout.trim(); const tree = g("rev-parse", "HEAD^{tree}").stdout.trim();
  const snap2 = path.join(dir, "snap2"); write(snap2, multi);
  const two = installCargoShim({ base: path.join(dir, "shim"), snapshotRoot: snap2, targetDirs: { ".": target, native: path.join(dir, "checkout", "native", "target") }, realCargo });
  assert.equal(shimScanEnv(clean, two).CARGO_TARGET_DIR, undefined, "two workspaces: no scan-wide default");
  assert.notEqual(two.dir, installed.dir, "another map is another wrapper identity and directory");
  assert.equal(verifySnapshotDir({ checkout, dir: snap2, commit, tree, when: "after the compile" }), tree, "the untouched snapshot verifies");
  write(snap2, { "native/target/debug/libb.rlib": "bypass output\n" });
  assert.throws(() => verifySnapshotDir({ checkout, dir: snap2, commit, tree, when: "after the compile" }), /native\/target/, "a bypass's output in the snapshot fails the after-compile verification");
});

test("[behavior] item 7 wrapper (review r1 #2): a snapshot compile's scans are keyed on its target map and the wrapper, so no result cached by a live-checkout run or another map is reused", () => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-cargo-shim-key-")));
  try {
    write(dir, {
      "package.json": '{"name":"root","private":true}\n', "pnpm-workspace.yaml": "packages:\n  - packages/*\n",
      "packages/js/package.json": '{"name":"js","private":true,"scripts":{"test":"node --test"}}\n',
      "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n',
    });
    const files = ["package.json", "pnpm-workspace.yaml", "packages/js/package.json", "Cargo.toml", "crates/a/Cargo.toml"];
    const plan = (targetDirs, cargoShim) => planScans({ changed: files, contentRoot: dir, liveRoot: dir, trustedRoot: dir, includeTests: true, quietTests: "local", tier: "compile", cargoCommand: ["cargo"],
      rustUnits: [{ name: "a", dir: "crates/a", crate: "a", cargoDir: "." }], npmUnits: [{ name: "js", dir: "packages/js" }], npmRoots: ["."], liveExecution: true, targetDirs, cargoShim, platform: process.platform });
    const base = { PATH: "/usr/bin" };
    const keyOf = (p, id, env = base) => { const s = p.scans.find((x) => x.id === id); assert.ok(s, `${id} planned`); return scanCacheKey({ id: s.id, command: s.command, toolVersion: "v", env: s.env, inheritedEnv: inheritedEnvDigest({ ...env, ...s.env }), digest: "d" }); };
    const live = plan(null, false); const mapA = plan({ ".": "/checkout/target" }, true); const mapB = plan({ ".": "/other/target" }, true);
    const shimA = { ...base, CARGO: "/tmp/rk-cargo-shim-aaaa/bin/cargo" }; const shimB = { ...base, CARGO: "/tmp/rk-cargo-shim-bbbb/bin/cargo" };
    for (const id of ["test:npm:js", mapA.scans.find((s) => s.tool === "cargo" && s.class === "B").id]) {
      assert.notEqual(keyOf(live, id), keyOf(mapA, id, shimA), `${id}: a live-checkout result is not the snapshot compile's`);
      assert.notEqual(keyOf(mapA, id, shimA), keyOf(mapB, id, shimA), `${id}: another target map is another key`);
      assert.notEqual(keyOf(mapA, id, shimA), keyOf(mapA, id, shimB), `${id}: another wrapper identity is another key`);
      assert.equal(keyOf(mapA, id, shimA), keyOf(plan({ ".": "/checkout/target" }, true), id, shimA), `${id}: the same map and wrapper reuse their own results`);
    }
    assert.equal(live.scans.some((s) => s.env.RK_SNAPSHOT_TARGETS), false, "a live-checkout plan carries no target map");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

// Linux with a real cargo on PATH (never a RustRight shim): the direct host the wrapper exists for.
const REAL_CARGO = (() => {
  if (process.platform !== "linux") return null;
  const r = spawnSync("sh", ["-c", "command -v cargo"], { encoding: "utf8" });
  const at = String(r.stdout ?? "").trim();
  return r.status === 0 && at && !at.includes(".rightkit-managed") ? at : null;
})();

test("[behavior] E16 real cargo (review r1 #3): on Linux the door compiles the same commit twice from its snapshot with a real cargo; a repository with a relative build.target-dir and one without write only to the checkout's target_directory, the second build reuses it, and the direct receipt names it", { skip: REAL_CARGO ? false : "needs Linux with a real cargo on PATH" }, (t) => {
  for (const variant of ["default target", "relative build.target-dir"]) {
    const dir = tmp(t);
    const repo = path.join(dir, "repo");
    write(repo, {
      "Cargo.toml": '[workspace]\nmembers = ["crates/core"]\nresolver = "2"\n',
      "crates/core/Cargo.toml": '[package]\nname = "core"\nversion = "0.1.0"\nedition = "2021"\npublish = false\n',
      "crates/core/src/lib.rs": "/// Adds one.\npub fn one(x: u32) -> u32 { x + 1 }\n#[cfg(test)]\nmod tests { #[test] fn adds() { assert_eq!(super::one(1), 2); } }\n",
      // The lockfile of a crate with no dependencies (written here: the test runner's broker guard refuses cargo in-process).
      "Cargo.lock": '# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = "core"\nversion = "0.1.0"\n',
      ...(variant === "relative build.target-dir" ? { ".cargo/config.toml": '[build]\ntarget-dir = "out"\n' } : {}),
    });
    const g = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
    g("init", "-q", "-b", "main"); g("add", "-A"); g("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c");
    const sha = g("rev-parse", "HEAD").stdout.trim();
    const expected = path.join(fs.realpathSync(repo), variant === "relative build.target-dir" ? "out" : "target");
    const driver = path.join(dir, "driver.mjs");
    // Real runScans over the plan's cargo scans (class B) and the class A rows that need no other tool, with the gate's own options
    // (the wrapper's env included): the compile, the wrapper and the real cargo are all real.
    fs.writeFileSync(driver, `const { main } = await import(${JSON.stringify(new URL("./bin/rightkit-gate.mjs", import.meta.url).href)});
const { runScans } = await import(${JSON.stringify(new URL("./deterministic-scans.mjs", import.meta.url).href)});
const out = [];
const keep = (s) => (s.class === "B" ? s.tool === "cargo" : s.internal || s.tool === "node" || (s.tool === "cargo" && s.args.includes("metadata")));
const code = await main(["--to", "compile", "--commit", ${JSON.stringify(sha)}], { cwd: ${JSON.stringify(repo)}, platform: "linux", snapshotRoot: ${JSON.stringify(path.join(dir, "snaps"))},
  out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0,
  runScans: (plan, options) => runScans({ ...plan, scans: plan.scans.filter(keep) }, options) });
console.log(JSON.stringify({ code, out: out.join("\\n").slice(-30000) }));\n`);
    const env = { ...process.env, NODE_OPTIONS: "", RK_GATE_CACHE: "0" };
    delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.CARGO_TARGET_DIR; delete env.CARGO_BUILD_TARGET_DIR;
    const run = () => {
      const r = spawnSync(process.execPath, [driver], { env, encoding: "utf8", cwd: repo, timeout: 600_000 });
      let res; try { res = JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop()); } catch { res = { code: null, out: `${r.stdout}\n${r.stderr}` }; }
      return res;
    };
    const artifacts = () => {
      const deps = path.join(expected, "debug", "deps");
      return fs.existsSync(deps) ? Object.fromEntries(fs.readdirSync(deps).filter((n) => /^libcore-.*\.(rlib|rmeta)$/.test(n)).map((n) => [n, fs.statSync(path.join(deps, n)).mtimeMs])) : {};
    };
    const first = run();
    assert.equal(first.code, 0, `${variant}: ${first.out.slice(-4000)}`);
    const snapDir = (first.out.match(/snapshot (\S+) is commit/) ?? [])[1];
    assert.ok(snapDir, `${variant}: the door synced a snapshot`);
    assert.match(first.out, /runtime cargo wrapper /, `${variant}: the wrapper was installed`);
    const built = artifacts();
    assert.ok(Object.keys(built).length > 0, `${variant}: the real cargo wrote the crate's outputs to ${expected}`);
    for (const stray of ["target", "out"]) assert.equal(fs.existsSync(path.join(snapDir, stray)), false, `${variant}: no ${stray}/ in the snapshot`);
    if (variant === "relative build.target-dir") assert.equal(fs.existsSync(path.join(repo, "target")), false, "nothing in the checkout's default target");
    const receiptLine = first.out.split("\n").find((l) => l.startsWith("rightkit-gate: direct receipt "));
    const receipt = JSON.parse(receiptLine.slice("rightkit-gate: direct receipt ".length));
    assert.equal(receipt.outputDir, expected, `${variant}: the receipt names the checkout's target_directory`);
    assert.equal(receipt.exitStatus, 0); assert.ok(receipt.cargoWrapper?.identity, "the receipt names the wrapper");
    // The same commit again: the wrapper binds the same directory and cargo reuses its outputs (no artifact rewritten).
    const second = run();
    assert.equal(second.code, 0, `${variant} (second): ${second.out.slice(-4000)}`);
    assert.deepEqual(artifacts(), built, `${variant}: the second build reused the checkout's outputs`);
    for (const stray of ["target", "out"]) assert.equal(fs.existsSync(path.join(snapDir, stray)), false, `${variant} (second): no ${stray}/ in the snapshot`);
  }
});
