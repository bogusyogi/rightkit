// Item 7: the runtime cargo wrapper of a commit-snapshot compile binds each cargo invocation to its own workspace's checkout target
// directory, or refuses it (GATE-CONTRACT §1 "Broker interface", output directory; row E16).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { SHIM_REFUSAL_STATUS, decideCargoInvocation, installCargoShim } from "./cargo-target-shim.mjs";
import { planScans } from "./deterministic-scans.mjs";
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
  const map = { workspaces: { [snap]: targets["."], [path.join(snap, "native")]: targets.native } };
  const decide = (args, cwd) => decideCargoInvocation({ args, cwd, env: { PATH: "/bin" }, map, locate: locateIn });
  // Mixed A+B: each binds its own, never the other's.
  const a = decide(["test", "--locked", "-p", "a"], path.join(snap, "crates", "a"));
  assert.equal(a.exec?.env.CARGO_TARGET_DIR, targets["."]); assert.equal(a.exec.env.CARGO_BUILD_TARGET_DIR, targets["."]);
  const b = decide(["build", "--locked"], path.join(snap, "native", "src"));
  assert.equal(b.exec?.env.CARGO_TARGET_DIR, targets.native);
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
  // Commands with no compile output run unchanged, with no workspace needed.
  for (const args of [["--version"], ["-V"], ["version"], ["fmt", "--check"], ["deny", "check"], ["locate-project"]]) {
    const r = decide(args, os.tmpdir());
    assert.ok(r.exec && r.exec.env.CARGO_TARGET_DIR === undefined, `${args.join(" ")}: ${JSON.stringify(r)}`);
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
  const shimDir = path.join(dir, "shim");
  const installed = installCargoShim({ dir: shimDir, snapshotRoot: snap, targetDirs: targets, realCargo });
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
      seen.push({ plan, bin, shim: bin ? fs.readFileSync(path.join(bin, "cargo"), "utf8") : null, map: bin ? JSON.parse(fs.readFileSync(path.join(path.dirname(bin), "cargo-target-map.json"), "utf8")) : null });
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
  assert.deepEqual(receipt?.cargoWrapper, { workspaces: run.map.workspaces });
  assert.equal(fs.existsSync(path.dirname(run.bin)), false, "the wrapper directory is removed after the compile");
});
