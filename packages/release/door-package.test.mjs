// Item 7 row 7.04 and Q1 positive 3 (decision 2 Q2 and Q1): `--to package` from the snapshot. The identity module is release-source-identity.mjs
// (lane L7S); the wiring under test is the door's (rightkit-gate.mjs), build-release.mjs, release.mjs, cli/right-release.mjs, admission*.mjs.
//
// Evidence labels. Part A drives the REAL door (real git, real snapshot sync into the test's own root, real ownership) on a simulated Linux
// host with right-release's children as doubles that write what the real ones write; it is fixture evidence on every host. Part B starts the
// REAL `build-release.mjs --dry-run` as a split child under a live door run (a real run file, a real snapshot of a dirty, detached,
// HEAD-ahead checkout): native on whichever host runs the file. A full sign-and-seal on M and D needs the dev broker (rows 7.20 to 7.24) and
// is run by the owner chat.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { subHandleStub } from "./test-support/fake-broker.mjs";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { main } from "./bin/rightkit-gate.mjs";
import { fixture, git as fixtureGit, managedEnv } from "./test-support/fixture.mjs";

const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;
const HERE = path.dirname(fileURLToPath(import.meta.url));
const sha256 = (b) => createHash("sha256").update(b).digest("hex");
const git = (cwd, ...a) => spawnSync("git", a, { cwd, encoding: "utf8" });
const triple = "x86_64-unknown-linux-gnu";

function tmp(t, prefix) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// ---- Part A: the door ---------------------------------------------------------------------------------------------------------------

const APP = {
  ".gitignore": "src-tauri/binaries/\nsrc-tauri/gen/\n",
  "right-release.config.json": JSON.stringify({ schema: 1, app: "app", version: "1.0.0", sidecars: [{ kind: "cargo-bin", package: "eng" }] }) + "\n",
  "src-tauri/Cargo.toml": '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n',
  "src-tauri/tauri.conf.json": '{"bundle":{"externalBin":["binaries/eng"]}}\n',
  "README.md": "committed\n",
};

/** A repository whose checkout is DIRTY, DETACHED and AHEAD of the commit the door is asked to build. */
function appFixture(t, extra = {}) {
  const dir = tmp(t, "rk-pkg-");
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t");
  const write = (rel, body) => { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); };
  for (const [rel, body] of Object.entries({ ...APP, ...extra })) write(rel, body);
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c1");
  const c1 = git(repo, "rev-parse", "HEAD").stdout.trim(); const tree = git(repo, "rev-parse", `${c1}^{tree}`).stdout.trim();
  write("README.md", "second commit\n"); git(repo, "commit", "-qam", "c2");
  git(repo, "checkout", "-q", "--detach");
  write("README.md", "THE CHECKOUT'S UNCOMMITTED EDIT\n"); write("src-tauri/Cargo.toml", "this is not toml\n");
  const snaps = path.join(dir, "snaps");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE; delete env.TAURI_CONFIG;
  const spawned = [];
  // `cargo metadata` in the CHECKOUT (the door's own resolution of the output directory, row 7.09): one workspace per manifest, its target outside the snapshot.
  const checkoutTarget = path.join(dir, "checkout-target");
  const workspaces = { current: (manifest) => ({ target_directory: checkoutTarget, workspace_root: path.dirname(manifest) }) };
  const spawn = (c, a, opts) => {
    spawned.push([c, ...a]);
    if (c === "cargo" && a[0] === "metadata") return { status: 0, stdout: JSON.stringify(workspaces.current(a[a.indexOf("--manifest-path") + 1])), stderr: "" };
    // `which cargo` names a real executable (the per-call wrapper of a multi-workspace stage needs one to wrap; nothing is ever run through it here).
    if (c === "which" && a[0] === "cargo") return { status: 0, stdout: `${process.execPath}\n`, stderr: "" };
    return c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : c === "pnpm" && a[0] === "--version" ? { status: 0, stdout: "11.25.0\n", stderr: "" } : { status: 1, stdout: "", stderr: "" };
  };
  const snapshot = () => path.join(snaps, fs.readdirSync(snaps).find((n) => !n.includes(".") && fs.statSync(path.join(snaps, n)).isDirectory()));
  const receipts = new Map();
  const launches = []; const captures = [];
  const staged = `src-tauri/binaries/eng-${triple}`;
  // `stage-sidecar --json`: writes what the real step writes under the snapshot and prints its result.
  const spawnCapture = (cmd, args, opts) => {
    captures.push({ args: args.slice(1), cwd: opts.cwd, env: opts.env });
    const s = snapshot();
    fs.mkdirSync(path.join(s, "src-tauri/binaries"), { recursive: true });
    fs.writeFileSync(path.join(s, staged), "engine-bytes"); fs.writeFileSync(path.join(s, "src-tauri/binaries/stage-sidecar.json"), "{}\n");
    return { status: 0, stderr: "", stdout: JSON.stringify({ files: [{ path: staged }], receiptPath: path.join(s, "src-tauri/binaries/stage-sidecar.json") }) };
  };
  // `right-release qa|build`: build seals into the CHECKOUT's `.right-release`, naming the commit, its tree and the bytes it read from the snapshot.
  const spawnInherit = (cmd, args, opts) => {
    const verb = args[1];
    launches.push({ verb, args: args.slice(1), cwd: opts.cwd, env: opts.env });
    if (verb === "build") {
      const s = snapshot();
      const dirOf = path.join(repo, ".right-release", "sealed", `app-1.0.0-${c1.slice(0, 8)}`, "mac");
      fs.mkdirSync(dirOf, { recursive: true });
      // What the real build records: the Cargo target it bound (build-release.mjs sealRelease boundTargets), read from the CARGO_TARGET_DIR it was handed.
      const boundDir = opts.env.CARGO_TARGET_DIR;
      fs.writeFileSync(path.join(dirOf, "release-manifest.json"), JSON.stringify({ schema: 1, commit: c1, tree, source: "snapshot", ...(boundDir ? { boundTargets: [{ prefix: "src-tauri/target/", manifest: "src-tauri/Cargo.toml", dir: boundDir }] } : {}), files: [{ path: staged, sha256: sha256(fs.readFileSync(path.join(s, staged))) }] }));
    }
    return { status: 0 };
  };
  const door = async (args, extra = {}) => {
    const out = []; const err = [];
    const code = await main(args, {
      cwd: repo, platform: "linux", snapshotRoot: snaps, env, spawn, out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0, runCheck: async () => 0,
      runCompile: async () => { const s = snapshot(); fs.mkdirSync(path.join(s, "src-tauri/gen/schemas"), { recursive: true }); fs.writeFileSync(path.join(s, "src-tauri/gen/schemas/x.json"), "{}\n"); return 0; },
      readReceipt: ({ commit }) => receipts.get(commit) ?? null, writeReceipt: ({ receipt }) => receipts.set(receipt.commit, receipt),
      resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), verifySnapshot: () => ({}), spawnCapture, spawnInherit, ...extra,
    });
    return { code, all: [...out, ...err].join("\n"), err: err.join("\n") };
  };
  return { dir, repo, c1, tree, snaps, env, spawned, receipts, launches, captures, door, snapshot, staged, checkoutTarget, workspaces, spawnInherit };
}

test("[7.04 positive] --to package seals the SELECTED commit from the snapshot though the checkout is dirty, detached and ahead: right-release starts inside the snapshot, sidecars are staged once, the manifest names the commit, its tree and source: snapshot, the sealed sidecar sha256 equals the ledger's", { skip: POSIX_SIM }, async (t) => {
  const fx = appFixture(t);
  const r = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.c1]);
  assert.equal(r.code, 0, r.all);
  // Sidecars once, then qa (source) and build, each inside the snapshot with the split in its environment.
  assert.equal(fx.captures.length, 1, "the sidecars are staged once per door run");
  assert.deepEqual(fx.launches.map((l) => l.verb), ["qa", "build"]);
  assert.deepEqual(fx.launches[0].args, ["qa", "--stage", "source", "--platform", "mac"]); assert.deepEqual(fx.launches[1].args, ["build", "--platform", "mac"]);
  for (const l of [...fx.captures, ...fx.launches]) {
    assert.equal(fs.realpathSync(l.cwd), fs.realpathSync(fx.snapshot()), "right-release runs inside the snapshot");
    assert.equal(l.env.RK_GATE_CHECKOUT, fs.realpathSync(fx.repo));
    assert.ok(!Object.hasOwn(l.env, "TAURI_CONFIG"));
  }
  // Row 7.04c (R-T1, 7.09): the checkout's output directory, resolved by the door in the primary checkout, is handed to qa and build absolute; the sealed manifest records it.
  for (const l of fx.launches) assert.equal(l.env.CARGO_TARGET_DIR, fx.checkoutTarget, `${l.verb} is handed the bound target`);
  assert.equal(fs.existsSync(path.join(fx.snapshot(), "src-tauri", "target")), false, "no target in the snapshot");
  // The checkout keeps its edits and is never what the build read.
  assert.equal(fs.readFileSync(path.join(fx.repo, "README.md"), "utf8"), "THE CHECKOUT'S UNCOMMITTED EDIT\n");
  assert.equal(fs.readFileSync(path.join(fx.snapshot(), "README.md"), "utf8"), "committed\n");
  assert.equal(fs.readFileSync(path.join(fx.snapshot(), "src-tauri/Cargo.toml"), "utf8").startsWith("[package]"), true);
  // The manifest and the receipt: commit C1 (HEAD is C2), its tree, source snapshot; the receipt's package stage names the seal.
  const manifest = JSON.parse(fs.readFileSync(path.join(fx.repo, ".right-release/sealed", `app-1.0.0-${fx.c1.slice(0, 8)}`, "mac", "release-manifest.json"), "utf8"));
  assert.equal(manifest.commit, fx.c1); assert.equal(manifest.tree, fx.tree); assert.equal(manifest.source, "snapshot");
  const stage = fx.receipts.get(fx.c1).stages.package;
  assert.equal(stage.ok, true); assert.equal(stage.sealed, `app-1.0.0-${fx.c1.slice(0, 8)}`); assert.equal(stage.snapshot.tree, fx.tree);
  // Q1 positive 3: the sealed sidecar's sha256 equals the ledger's (the bytes the compile read are the bytes sealed).
  const ledgerEntry = stage.snapshot.ledger.find((e) => e.path === fx.staged);
  assert.equal(ledgerEntry.kind, "staged");
  assert.equal(manifest.files.find((f) => f.path === fx.staged).sha256, ledgerEntry.sha256);
  assert.equal(ledgerEntry.sha256, sha256("engine-bytes"));
});

test("[Q1 positive 3] --to package ALONE after a compile-only run re-stages (the sync removed the ledger) and seals; the sealed sidecar sha256 equals the ledger's", { skip: POSIX_SIM }, async (t) => {
  const fx = appFixture(t);
  let r = await fx.door(["--to", "compile", "--commit", fx.c1]);
  assert.equal(r.code, 0, r.all);
  assert.equal(fx.captures.length, 1); assert.deepEqual(fx.launches, [], "compile launches no right-release but the staging step");
  assert.deepEqual(fx.receipts.get(fx.c1).stages.compile.snapshot.ledger.map((e) => [e.path, e.kind]), [[fx.staged, "staged"], ["src-tauri/binaries/stage-sidecar.json", "staged"], ["src-tauri/gen/schemas/x.json", "generated"]]);
  // A separate door run: the receipt says compile passed (so it is not run again), the sync removes every ledger entry, and the package stage stages the sidecars itself.
  r = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.c1], { runCompile: async () => { throw new Error("compile must not run again"); } });
  assert.equal(r.code, 0, r.all);
  assert.match(r.all, /\[compile\] already passed/);
  assert.equal(fx.captures.length, 2, "the package-only run staged the sidecars again, once");
  assert.deepEqual(fx.launches.map((l) => l.verb), ["qa", "build"]);
  const stage = fx.receipts.get(fx.c1).stages.package;
  const entry = stage.snapshot.ledger.find((e) => e.path === fx.staged);
  const manifest = JSON.parse(fs.readFileSync(path.join(fx.repo, ".right-release/sealed", `app-1.0.0-${fx.c1.slice(0, 8)}`, "mac", "release-manifest.json"), "utf8"));
  assert.equal(manifest.files[0].sha256, entry.sha256); assert.equal(entry.kind, "staged");
});

test("[7.04 negative] a snapshot byte changed during package voids the stage (exit 2) and no manifest is recorded as passed; right-release failing fails the stage", { skip: POSIX_SIM }, async (t) => {
  const fx = appFixture(t);
  const failing = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.c1], { spawnInherit: (c, a, opts) => { fx.launches.push({ verb: a[1], args: a.slice(1) }); return { status: a[1] === "build" ? 3 : 0 }; } });
  assert.equal(failing.code, 1, failing.all); assert.match(failing.err, /--to package stopped at package/);
  const tampered = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.c1], { readReceipt: () => null, spawnInherit: (c, a, opts) => { if (a[1] === "build") fs.writeFileSync(path.join(opts.cwd, "README.md"), "tampered\n"); return { status: 0 }; }, verifySnapshot: undefined });
  assert.equal(tampered.code, 2, tampered.all); assert.match(tampered.err, /RK_CHECK_ERROR: \[package\]/); assert.match(tampered.err, /the result covers no known bytes/);
  assert.equal(fx.receipts.get(fx.c1).stages.package.ok, false);
});

test("[7.04c broker] on a broker host the package stage binds no CARGO_TARGET_DIR: its children carry the stage's carrier, and the sealed manifest may record only a target a verified receipt of the stage named (RK_SPLIT_TARGET_DRIFT otherwise)", { skip: POSIX_SIM }, async (t) => {
  const OUT = "/managed/rightkit-build-control/ws/0123456789ab/g/001/target";
  const broker = (fx, outputDirs) => ({ platform: "darwin", resolveExecutor: () => ({ kind: "broker", cargo: ["rustright", "cargo"] }),
    snapshotBroker: { handle: "own-1", markBrokerSubmit: subHandleStub("own-1") }, brokerSection: () => ({ client: "/x/rustright" }),
    installCarrier: () => ({ mode: "test", env: { RIGHTKIT_CARRIED_CARGO_MAP: "/x/map.json" }, problems: async () => [], outputDirs: () => (typeof outputDirs === "function" ? outputDirs() : outputDirs) }),
    spawnInherit: (cmd, args, opts) => { assert.equal(opts.env.CARGO_TARGET_DIR, undefined, "no output directory is bound"); assert.equal(opts.env.RIGHTKIT_CARRIED_CARGO_MAP, "/x/map.json"); return fx.spawnInherit(cmd, args, { ...opts, env: { ...opts.env, CARGO_TARGET_DIR: OUT } }); } });
  const fx = appFixture(t);
  const ok = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.c1], broker(fx, [OUT]));
  assert.equal(ok.code, 0, ok.all);
  assert.ok(!fx.spawned.some((c) => c[0] === "cargo" && c[1] === "metadata"), "no carrier-less cargo metadata resolves the target in the checkout");
  // The manifest records a target no receipt of the stage named: the stage is void.
  const fx2 = appFixture(t);
  const drift = await fx2.door(["--to", "package", "--platform", "mac", "--commit", fx2.c1], broker(fx2, ["/managed/rightkit-build-control/ws/ffffffffffff/g/001/target"]));
  assert.equal(drift.code, 2, drift.all); assert.match(drift.err, /RK_SPLIT_TARGET_DRIFT: \[package\] boundTargets\[0\] is \/managed\/.*not the directory the gate bound/);
  assert.equal(fx2.receipts.get(fx2.c1).stages.package.voided, "RK_SPLIT_TARGET_DRIFT");
  // THIS stage's receipts only (L1): the compile stage's sidecar build named the directory, but no receipt of the package stage did.
  const fx3 = appFixture(t); let now = [OUT];
  const base3 = broker(fx3, () => now);
  const other = await fx3.door(["--to", "package", "--platform", "mac", "--commit", fx3.c1], { ...base3, spawnInherit: (cmd, args, opts) => { const r = base3.spawnInherit(cmd, args, opts); now = []; return r; } });
  assert.equal(other.code, 2, other.all); assert.match(other.err, /RK_SPLIT_TARGET_DRIFT: \[package\] no verified receipt of this package stage names an output directory/);
  // A sealed manifest that records no bound target at all is refused where the commit has a Cargo workspace.
  const fx4 = appFixture(t);
  const base4 = broker(fx4, [OUT]);
  const none = await fx4.door(["--to", "package", "--platform", "mac", "--commit", fx4.c1], { ...base4, spawnInherit: (cmd, args, opts) => fx4.spawnInherit(cmd, args, { ...opts, env: { ...opts.env, CARGO_TARGET_DIR: undefined } }) });
  assert.equal(none.code, 2, none.all); assert.match(none.err, /RK_SPLIT_TARGET_DRIFT: \[package\] the sealed manifest records no bound Cargo target/);
});

test("[7.04c N6 on L] the door cross-checks the Cargo target the sealed manifest records against its own resolution: a manifest naming another directory voids the package stage (RK_SPLIT_TARGET_DRIFT, exit 2) and nothing after it runs", { skip: POSIX_SIM }, async (t) => {
  const fx = appFixture(t);
  const other = path.join(fx.dir, "another-target");
  const drifting = (cmd, args, opts) => {
    fx.launches.push({ verb: args[1], args: args.slice(1), cwd: opts.cwd, env: opts.env });
    if (args[1] === "build") {
      const dirOf = path.join(fx.repo, ".right-release", "sealed", `app-1.0.0-${fx.c1.slice(0, 8)}`, "mac");
      fs.mkdirSync(dirOf, { recursive: true });
      fs.writeFileSync(path.join(dirOf, "release-manifest.json"), JSON.stringify({ schema: 1, commit: fx.c1, tree: fx.tree, source: "snapshot", boundTargets: [{ prefix: "src-tauri/target/", manifest: "src-tauri/Cargo.toml", dir: other }], files: [] }));
    }
    return { status: 0 };
  };
  const r = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.c1], { spawnInherit: drifting });
  assert.equal(r.code, 2, r.all);
  assert.match(r.err, /RK_SPLIT_TARGET_DRIFT: \[package\] boundTargets\[0\] is .*another-target, not the directory the gate bound/);
  assert.match(r.err, /the sealed set is void and nothing after it ran/);
  const stage = fx.receipts.get(fx.c1).stages.package;
  assert.equal(stage.ok, false); assert.equal(stage.voided, "RK_SPLIT_TARGET_DRIFT");
  // A manifest that records no bound target at all is the same refusal (the child's value is never trusted alone).
  const fx2 = appFixture(t);
  const silent = await fx2.door(["--to", "package", "--platform", "mac", "--commit", fx2.c1], { spawnInherit: (cmd, args, opts) => {
    if (args[1] === "build") { const dirOf = path.join(fx2.repo, ".right-release", "sealed", `app-1.0.0-${fx2.c1.slice(0, 8)}`, "mac"); fs.mkdirSync(dirOf, { recursive: true }); fs.writeFileSync(path.join(dirOf, "release-manifest.json"), JSON.stringify({ schema: 1, commit: fx2.c1, files: [] })); }
    return { status: 0 };
  } });
  assert.equal(silent.code, 2, silent.all); assert.match(silent.err, /records no bound Cargo target/);
});

test("[7.04c] more than one Cargo workspace in the commit is bound per workspace, each to its own target (GB-17), and the sealed manifest must record every one; a broker host binds nothing here (the carrier's rows 7.20 to 7.24)", { skip: POSIX_SIM }, async (t) => {
  const fx = appFixture(t, { "other/Cargo.toml": '[package]\nname = "other"\nversion = "0.1.0"\nedition = "2021"\n' });
  fx.workspaces.current = (manifest) => ({ target_directory: path.join(fx.dir, `t-${path.basename(path.dirname(manifest))}`), workspace_root: path.dirname(manifest) });
  const many = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.c1]);
  assert.doesNotMatch(many.all, /RK_SNAPSHOT_OUTPUT_UNPINNED/, "two workspaces are no refusal");
  assert.ok(fx.launches.length >= 1, `right-release started (the per-workspace binding): ${many.all}`);
  // A broker host: the gate pins nothing (the broker's generation target is the carrier's, rows 7.20 to 7.24): asserted in source form.
  assert.match(fs.readFileSync(path.join(HERE, "bin/rightkit-gate.mjs"), "utf8"), /if \(ok && ds && executor\.kind !== "broker"\) \{\n\s+packagePin = cargoOutputPin/);
});

// ---- Part B: the real build-release.mjs as a split child ----------------------------------------------------------------------------

/** A fixture app whose checkout is dirty, detached and ahead of C1, a snapshot of C1, and a LIVE door run naming it (this process is its pid). */
function splitFixture(t) {
  const config = fixture({ platform: "mac", macSigning: true, packageCommand: { cmd: "false" } });
  const repo = path.dirname(config);
  const c1 = fixtureGit(repo, "rev-parse", "HEAD"); const tree = fixtureGit(repo, "rev-parse", `${c1}^{tree}`);
  fs.writeFileSync(path.join(repo, "NOTES.md"), "second\n"); fixtureGit(repo, "add", "NOTES.md"); fixtureGit(repo, "commit", "-m", "c2"); // HEAD ahead of C1
  fixtureGit(repo, "checkout", "--detach");
  fs.appendFileSync(path.join(repo, "package.json"), "\n"); // dirty: an input the packaged app reads
  const snap = path.join(tmp(t, "rk-pkg-snap-"), "snapshot"); fs.mkdirSync(snap);
  const archive = path.join(path.dirname(snap), "c1.tar");
  assert.equal(spawnSync("git", ["-C", repo, "archive", "-o", archive, c1]).status, 0);
  assert.equal(spawnSync("tar", ["-xf", archive, "-C", snap]).status, 0);
  const real = fs.realpathSync(repo);
  const nonce = "ab".repeat(16);
  const runsDir = path.join(real, ".git", "rightkit-gate", "runs"); fs.mkdirSync(runsDir, { recursive: true });
  const writeRun = (over = {}) => fs.writeFileSync(path.join(runsDir, `${nonce}.json`), JSON.stringify({ pid: process.pid, commit: c1, tree, stage: "package", source: { checkout: real, snapshot: fs.realpathSync(snap), commit: c1, tree, appDir: ".", ...(over.source ?? {}) }, ...(over.run ?? {}) }));
  writeRun();
  const env = { ...managedEnv(config, { platform: "mac" }), RK_GATE_RUN: nonce, RK_GATE_CHECKOUT: real };
  const buildJs = path.join(HERE, "build-release.mjs");
  const run = (cwd, args, e = env) => spawnSync(process.execPath, [buildJs, ...args, "--platform", "mac", "--dry-run"], { cwd, env: e, encoding: "utf8" });
  return { config, repo: real, snap: fs.realpathSync(snap), c1, tree, env, writeRun, run, name: path.basename(config) };
}

test("[7.04 positive, real] build --dry-run from the snapshot of C1 passes admission with the checkout dirty, detached and ahead; the identity is the selected commit", async (t) => {
  const fx = splitFixture(t);
  const r = fx.run(fx.snap, ["--config", path.join(fx.snap, fx.name)]);
  assert.equal(r.status, 0, `${r.stdout}\n${r.stderr}`);
  assert.match(r.stdout, new RegExp(`commit: ${fx.c1}`), "the commit is the selected one, not HEAD's");
  assert.match(r.stdout, /\[ok {4}\] dirty-inputs/, "no dirty-checkout condition inside a split");
  assert.match(r.stdout, /admission: \d+ rows, 0 failed/);
});

test("[7.04 negative, real] the same checkout WITHOUT a split is still refused by the clean checks (R1)", async (t) => {
  const fx = splitFixture(t);
  const env = { ...fx.env }; delete env.RK_GATE_RUN; delete env.RK_GATE_CHECKOUT;
  const r = fx.run(fx.repo, ["--config", fx.config], env);
  assert.notEqual(r.status, 0, `${r.stdout}\n${r.stderr}`);
  assert.match(`${r.stdout}\n${r.stderr}`, /detached HEAD is forbidden|dirty files can change the packaged app|checkout/);
});

test("[7.04 negative, real] an unprovable split is refused and never falls back to the checkout checks (R2): another cwd, an ended run, a forged tree (R3), another checkout; a config in the checkout is refused (R5)", async (t) => {
  const fx = splitFixture(t);
  const configInSnapshot = ["--config", path.join(fx.snap, fx.name)];
  const refused = (r, re, label) => { assert.notEqual(r.status, 0, `${label}: ${r.stdout}\n${r.stderr}`); assert.match(`${r.stdout}\n${r.stderr}`, re, label); assert.doesNotMatch(`${r.stdout}\n${r.stderr}`, /build dry-run: fixture/, `${label}: nothing was admitted`); };
  // R2: a process in the checkout holding the split env.
  refused(fx.run(fx.repo, configInSnapshot), /RK_SOURCE_SPLIT: this process runs in .*, outside the snapshot/, "cwd outside the snapshot");
  // R5: a config in the checkout, whatever the cwd.
  refused(fx.run(fx.snap, ["--config", fx.config]), /outside the snapshot|RK_SOURCE_SPLIT|RK_SOURCE_IDENTITY/, "config in the checkout");
  // R3: the run file's tree is not the object database's tree for its commit.
  fx.writeRun({ source: { tree: "0".repeat(40) }, run: { tree: "0".repeat(40) } });
  refused(fx.run(fx.snap, configInSnapshot), /R3|tree/, "forged tree");
  // The door run ended.
  fx.writeRun({ run: { pid: 2 ** 22 + 4321 } });
  refused(fx.run(fx.snap, configInSnapshot), /no live door run/, "ended run");
  // Another checkout.
  fx.writeRun({ source: { checkout: os.tmpdir() } });
  refused(fx.run(fx.snap, configInSnapshot), /the door run was made in|RK_SOURCE_SPLIT/, "another checkout");
});

test("[7.04] the wiring that makes package work in a split is present in every file it touches (source form): the CLI allows build/package/release/qa and mobile upload through assertReleaseSource; build-release, release and admission take the source", () => {
  const read = (f) => fs.readFileSync(path.join(HERE, f), "utf8");
  const cli = read("cli/right-release.mjs");
  assert.match(cli, /const SPLIT_COMMANDS = new Set\(\["dev", "test", "stage-sidecar", "web-build", "build", "package", "release", "qa"\]\)/);
  assert.match(cli, /key === "upload" && MOBILE_UPLOAD\(args\)/);
  const br = read("build-release.mjs");
  assert.match(br, /resolveReleaseSource\(/); assert.match(br, /assertReleaseSource\(releaseSource/); assert.match(br, /qaAdmissionSourceOptions\(releaseSource\)/);
  assert.doesNotMatch(br, /git\(repoRoot, \["rev-parse", "HEAD"\]\)/, "no HEAD read left in the build");
  assert.doesNotMatch(br, /git\(invocationRoot, \["rev-parse", "HEAD"\]\)/);
  assert.match(br, /splitSealSource \? \{ tree: splitSealSource\.tree, source: "snapshot" \}/, "the manifest records the tree and the snapshot source");
  assert.match(read("release.mjs"), /workerSource \? workerSource\.commit : git\(root, \["rev-parse", "HEAD"\]\)/);
  assert.match(read("admission-files.mjs"), /ctx\.releaseSource\?\.mode === "snapshot"\) return undefined/);
});

test("[GB-58] --to dev|test stages the app's top-level declared sidecars ONCE under the snapshot's ownership before the dependent build; a failed staging launches nothing", { skip: POSIX_SIM }, async (t) => {
  for (const to of ["dev", "test"]) {
    const fx = appFixture(t);
    const staged = await fx.door(["--to", to, "--commit", fx.c1]);
    assert.equal(staged.code, 0, staged.all);
    assert.equal(fx.captures.length, 1, `${to}: the declared sidecars are staged once for a fresh snapshot (no duplicate prepare stage owes it)`);
    assert.deepEqual(fx.launches.map((l) => l.verb), [to], `${to}: right-release ${to} starts after the staging`);
    assert.ok(fs.existsSync(path.join(fx.snapshot(), fx.staged)), `${to}: the staged sidecar is in the snapshot the build reads`);
    // The staged bytes are ledgered immutable entries the build cannot silently replace; the snapshot is the commit plus the ledger.
    assert.match(staged.all, /\[(dev|test)\] right-release stage-sidecar --config /);
  }
  // A failed staging stops before the dependent build: the stale-sidecar fallback never runs.
  const fx = appFixture(t);
  const failing = await fx.door(["--to", "dev", "--commit", fx.c1], { spawnCapture: () => ({ status: 3, stderr: "stage failed", stdout: "" }) });
  assert.equal(failing.code, 1, failing.all);
  assert.match(failing.err, /the declared sidecars could not be staged; nothing compiled/);
  assert.deepEqual(fx.launches, [], "no right-release dev after a failed staging");
  // An app that declares no sidecars stages nothing and builds as before.
  const plain = appFixture(t, { "right-release.config.json": JSON.stringify({ schema: 1, app: "app", version: "1.0.0" }) + "\n" });
  const ran = await plain.door(["--to", "dev", "--commit", plain.c1]);
  assert.equal(ran.code, 0, ran.all);
  assert.equal(plain.captures.length, 0);
  assert.deepEqual(plain.launches.map((l) => l.verb), ["dev"]);
});
