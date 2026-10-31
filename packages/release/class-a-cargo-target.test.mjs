// A Class A Cargo scan (cargo-metadata, cargo-package-list, cargo-deny, cargo-audit) that runs natively, with no broker carrying it, must never
// write into the snapshot it reads. Cargo puts target/.rustc_info.json beside the manifest it reads when nothing names an output directory, and
// the gate then refused the run ("snapshot ... is not exactly commit ... after class A: crates/x/target is not in the commit's tree").
// What is real here: the planner (planScans over a committed fixture repository and its snapshot), the scan runner (runScans), the snapshot check
// (verifySnapshotDir over real git and real files) and the gate's check stage (main). What is injected is the runner's own `spawn` seam, as in every
// scan-runner test of this package: a recording spawn that logs each call's directory and CARGO_TARGET_DIR and, where a test says so, leaves the
// file real cargo leaves in its output directory. Nothing starts a real cargo, a broker or a build; that cargo writes nothing else into a snapshot
// when it is given an output directory is proved only by a gate run on a host (the checkout that fails it is license-026-gate.log:1313).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { main } from "./bin/rightkit-gate.mjs";
import { cargoReadTargetDir, planScans, runScans, scanFailures } from "./deterministic-scans.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { verifySnapshotDir } from "./snapshot-check.mjs";

const FILES = {
  "crates/foo/Cargo.toml": '[package]\nname = "foo"\nversion = "1.2.3"\nlicense = "MIT"\nedition = "2021"\n',
  "crates/foo/CHANGELOG.md": "## 1.2.3\n- x\n",
  "crates/foo/src/lib.rs": "pub fn f() {}\n",
  "Cargo.lock": "version = 4\n",
  "ci/advisory-exceptions.json": JSON.stringify({ schema: 1, exceptions: [] }),
  "deny.toml": '[advisories]\nunmaintained = "workspace"\nyanked = "deny"\nignore = []\n',
  ".cargo/audit.toml": "[advisories]\nignore = []\n",
  // Outside RightKit a repository commits its fixed build definition: the list of every Cargo workspace (this one holds a single workspace, crates/foo).
  "ci/gate.config.json": JSON.stringify({ cargoWorkspaces: ["crates/foo"] }),
};

/** A committed repository, the gate's kind of snapshot of that commit, and an empty scratch root outside both. */
function fixture(t) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-cargo-read-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const repo = path.join(base, "repo"); fs.mkdirSync(repo);
  const git = (...a) => { const r = spawnSync("git", a, { cwd: repo, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout.trim(); };
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  for (const [rel, body] of Object.entries(FILES)) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git("add", "-A"); git("commit", "-q", "-m", "c");
  const synced = syncSnapshot({ checkout: repo, rev: git("rev-parse", "HEAD"), root: path.join(base, "snapshots") });
  const scratch = path.join(base, "scratch"); fs.mkdirSync(scratch);
  const verify = (when = "after class A") => verifySnapshotDir({ checkout: repo, dir: synced.path, commit: synced.commit, tree: synced.tree, when });
  return { base, repo, snap: synced.path, commit: synced.commit, tree: synced.tree, scratch, verify };
}

const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };

/** The Class A Cargo scans of the snapshot, planned as a direct host plans them (`cargo`) or as a broker host does (`rustright cargo`). */
function cargoScans(snap, { cargoCommand = ["cargo"], tool = "cargo", ...extra } = {}) {
  const planned = planScans({ changed: ["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], contentRoot: snap, liveRoot: snap, platform: process.platform, cargoCommand, ...extra });
  const scans = planned.scans.filter((s) => s.class === "A" && s.tool === tool);
  assert.deepEqual(scans.map((s) => s.id), ["cargo-metadata", "cargo-package-list:foo", "cargo-deny", "cargo-audit"]);
  return { ...planned, scans };
}

const digest = (tool) => crypto.createHash("sha256").update(`${tool} enrolled bytes`).digest("hex");
const POLICY = Object.freeze(Object.fromEntries(["cargo-deny", "cargo-audit"].map((tool) => [tool, { path: `/opt/rk-tools/${tool}/${tool}`, sha256: digest(tool), version: "1.0.0" }])));
const TOOL_OPS = { realpath: (p) => p, stat: () => ({ isFile: () => true, mode: 0o755 }), digestFile: (p) => digest(path.posix.basename(p)) };

/**
 * A recording stand-in for cargo. A version probe answers; any other call is logged with its directory and the CARGO_TARGET_DIR it was given. With
 * `write`, it also leaves what real cargo leaves where it keeps its output (CARGO_TARGET_DIR, else <cwd>/target): .rustc_info.json.
 */
function recordingCargo({ write = true, fail = [] } = {}) {
  const calls = [];
  const spawn = (command, args, options = {}) => {
    const name = path.basename(command).replace(/\.exe$/, "");
    if (args.at(-1) === "--version") return { status: 0, stdout: `${name} 1.0.0\n`, stderr: "" };
    const target = options.env?.CARGO_TARGET_DIR;
    calls.push({ command, sub: args.find((a) => !a.startsWith("-") && a !== "cargo"), args, cwd: options.cwd, target });
    if (write && name === "cargo") {
      const out = target ?? path.join(options.cwd, "target");
      fs.mkdirSync(out, { recursive: true }); fs.writeFileSync(path.join(out, ".rustc_info.json"), "{}\n");
    }
    if (fail.some((f) => args.includes(f))) return { status: 1, stdout: "", stderr: "boom\n" };
    return { status: 0, stdout: args.includes("metadata") ? "{}" : "", stderr: "" };
  };
  return { spawn, calls };
}

const runOptions = (f, over = {}) => ({
  classes: ["A"], env: {}, execPath: "node", toolPolicy: POLICY, toolOps: TOOL_OPS, cargoReadTargetRoot: f.scratch,
  ensureAdvisoryDb: () => "/cache/advisory-db/pinned", prepareDenyOverlay: () => ({ configPath: path.join(f.base, "deny-overlay.toml"), cleanup() {} }), ...over,
});

test("the defect: what cargo leaves in <cwd>/target when no output directory is named makes the snapshot no longer the commit's tree, and the snapshot check refuses it", (t) => {
  const f = fixture(t);
  assert.equal(f.verify("before class A"), f.tree, "the fixture snapshot is exactly the commit");
  // The file a cargo started in the snapshot with no CARGO_TARGET_DIR writes (rustc's probe cache), written for real.
  fs.mkdirSync(path.join(f.snap, "target"), { recursive: true });
  fs.writeFileSync(path.join(f.snap, "target", ".rustc_info.json"), "{}\n");
  assert.throws(() => f.verify(), /is not exactly commit .* after class A: target is not in the commit's tree \(a directory the commit does not have\)/);
});

test("Class A Cargo scans run natively get an output directory of their own outside the snapshot, and the snapshot stays exactly the commit", async (t) => {
  const f = fixture(t);
  const cargo = recordingCargo();
  const result = await runScans(cargoScans(f.snap), { ...runOptions(f), spawn: cargo.spawn });
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.deepEqual(result.scans.map((s) => [s.id, s.status]), [["cargo-metadata", "pass"], ["cargo-package-list:foo", "pass"], ["cargo-deny", "pass"], ["cargo-audit", "pass"]]);
  // metadata, package --list, deny's own fetch and metadata and then deny itself, audit: every call, not only the scan's main one, is bound outside.
  assert.deepEqual(cargo.calls.map((c) => c.sub), ["metadata", "package", "fetch", "metadata", "deny", "audit"]);
  for (const c of cargo.calls) {
    assert.equal(typeof c.target, "string", `${c.args.join(" ")} was given an output directory`);
    assert.ok(!inside(c.target, f.snap) && !inside(c.target, f.repo), `${c.target} is outside the snapshot and the checkout`);
    assert.ok(inside(c.target, f.scratch), `${c.target} is under the gate's scratch root`);
  }
  assert.equal(f.verify(), f.tree, "the snapshot is still exactly the commit's tree after class A");
  assert.equal(fs.existsSync(path.join(f.snap, "target")), false);
  assert.deepEqual(fs.readdirSync(f.scratch), [], "every scan's output directory is removed when the scan ends");
});

test("each concurrent scan has its own output directory, removed after a failing scan too", async (t) => {
  const f = fixture(t);
  const cargo = recordingCargo({ fail: ["package"] });
  const result = await runScans(cargoScans(f.snap), { ...runOptions(f), spawn: cargo.spawn, jobs: 4 });
  assert.equal(result.scans.find((s) => s.id === "cargo-package-list:foo").status, "fail");
  assert.equal(result.scans.find((s) => s.id === "cargo-metadata").status, "pass");
  const dirs = new Set(cargo.calls.map((c) => c.target));
  assert.equal(dirs.size, 4, "metadata, package list, deny and audit each ran in a directory of their own");
  assert.equal(f.verify(), f.tree);
  assert.deepEqual(fs.readdirSync(f.scratch), []);
});

test("what already binds its own output is left alone: carried reads (the broker), the broker client, a planned CARGO_TARGET_DIR, a wrapped compile's sentinel", async (t) => {
  const f = fixture(t);
  const targets = (cargo) => cargo.calls.map((c) => c.target);
  // A carried read: the broker owns the output and a carried child gets no CARGO_TARGET_DIR, so none is added (the broker path is unchanged).
  let cargo = recordingCargo({ write: false });
  let result = await runScans(cargoScans(f.snap), { ...runOptions(f, { toolPolicy: {} }), spawn: cargo.spawn, cargoReadsCarried: true });
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.deepEqual(targets(cargo), cargo.calls.map(() => undefined));
  // The broker's own client is never given one either, carried or not (here not carried, so the analyzers are enrolled).
  cargo = recordingCargo({ write: false });
  result = await runScans(cargoScans(f.snap, { cargoCommand: ["rustright", "cargo"], tool: "rustright" }), { ...runOptions(f), spawn: cargo.spawn });
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.ok(cargo.calls.length > 0 && targets(cargo).every((x) => x === undefined));
  // A scan that the plan binds to the checkout's output (a snapshot compile's targetDirs) keeps exactly that.
  cargo = recordingCargo({ write: false });
  const pinned = planScans({ changed: ["crates/foo/src/lib.rs"], contentRoot: f.snap, liveRoot: f.snap, platform: process.platform, cargoCommand: ["cargo"], targetDirs: { ".": "/checkout/target" } });
  result = await runScans({ ...pinned, scans: pinned.scans.filter((s) => s.class === "A" && s.tool === "cargo") }, { ...runOptions(f), spawn: cargo.spawn });
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.deepEqual(cargo.calls.map((c) => c.sub), ["metadata", "package"]);
  assert.deepEqual(targets(cargo), ["/checkout/target", "/checkout/target"]);
  // A wrapped compile's sentinel stays the only binding: an added directory would let a cargo that bypassed the wrapper pass unseen.
  cargo = recordingCargo({ write: false });
  const sentinelRoot = path.join(f.base, "sentinel"); fs.mkdirSync(sentinelRoot);
  result = await runScans(cargoScans(f.snap), { ...runOptions(f), spawn: cargo.spawn, cargoSentinel: sentinelRoot });
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  const bound = cargo.calls.filter((c) => c.target !== undefined);
  assert.equal(bound.length, 4, "each scan's own call is bound to its sentinel");
  assert.ok(bound.every((c) => inside(c.target, sentinelRoot)), JSON.stringify(targets(cargo)));
  assert.deepEqual(cargo.calls.filter((c) => c.target === undefined).map((c) => c.sub), ["fetch", "metadata"], "cargo-deny's own fetch and metadata are the wrapper's to bind, as before");
  assert.deepEqual(fs.readdirSync(f.scratch), [], "no scan of these runs made an output directory of its own");
});

test("an output root inside the snapshot is refused: the scan fails by name, cargo never runs, and nothing is left behind", async (t) => {
  const f = fixture(t);
  for (const root of [f.snap, path.join(f.snap, "crates", "foo")]) {
    const cargo = recordingCargo();
    const result = await runScans(cargoScans(f.snap), { ...runOptions(f, { cargoReadTargetRoot: root }), spawn: cargo.spawn });
    assert.equal(result.ok, false);
    assert.deepEqual([...new Set(result.scans.map((s) => s.code))], ["CARGO_READ_TARGET"], root);
    assert.match(result.scans[0].detail, /would be inside .*; set RK_GATE_TMPDIR outside the repository/);
    assert.deepEqual(cargo.calls, [], "a refused directory means no cargo call at all");
    assert.equal(f.verify(), f.tree, `nothing was left in ${root}`);
  }
});

test("cargoReadTargetDir: a unique directory under the root, refused (and removed) when it resolves into a root, symlinks followed", (t) => {
  const f = fixture(t);
  const a = cargoReadTargetDir({ tmpRoot: f.scratch, roots: [f.snap, f.repo, undefined, null] });
  const b = cargoReadTargetDir({ tmpRoot: f.scratch, roots: [f.snap] });
  assert.notEqual(a, b);
  for (const dir of [a, b]) { assert.ok(fs.statSync(dir).isDirectory()); assert.equal(path.dirname(dir), f.scratch); assert.match(path.basename(dir), /^rk-cargo-read-/); }
  const link = path.join(f.base, "scratch-link");
  fs.symlinkSync(f.snap, link, "dir");
  assert.throws(() => cargoReadTargetDir({ tmpRoot: link, roots: [f.snap] }), /would be inside/);
  assert.deepEqual(fs.readdirSync(f.snap).filter((n) => n.startsWith("rk-cargo-read-")), [], "the refused directory was removed");
  assert.throws(() => cargoReadTargetDir({ tmpRoot: path.join(f.base, "absent"), roots: [f.snap] }), /ENOENT/, "an unusable root fails, never falls back to the snapshot");
});

test("the gate hands a direct host's scans its scratch root as their output root (the check stage, snapshot of a commit)", async (t) => {
  const f = fixture(t);
  const ok = (stdout = "") => ({ status: 0, stdout, stderr: "" });
  const spawn = (cmd, args, opts = {}) => {
    if (cmd === "cargo" && args[0] === "metadata") return ok(JSON.stringify({ workspace_root: opts.cwd, packages: [{ name: "foo", manifest_path: path.join(opts.cwd, "Cargo.toml"), publish: null }] }));
    if (cmd === "cargo") return ok("cargo 1.0.0\n");
    if (cmd === "pnpm" && args[0] === "--version") return ok("11.25.0\n");
    return spawnSync(cmd, args, { ...opts, encoding: "utf8" });
  };
  const seen = [];
  const fakeRun = async (plan, options) => { seen.push({ plan, options }); return { scans: plan.scans.map((s) => ({ id: s.id, class: s.class, status: "pass" })), notes: plan.notes, cacheHits: [], deferred: [] }; };
  const out = []; const err = [];
  // The environment binding walks the gate package; a private stand-in keeps other suites' writes to the live package out of this run.
  const gatePackage = path.join(f.base, "gate-package"); fs.mkdirSync(gatePackage); fs.writeFileSync(path.join(gatePackage, "stub.mjs"), "// stand-in gate package for the environment binding\n");
  const code = await main(["check", "--commit", f.commit, "--full-a", "--snapshot", f.snap], {
    out: (m) => out.push(m), err: (m) => err.push(m), cwd: f.repo, gatePackage, env: { ...process.env, RK_GATE_CACHE: "0", RK_GATE_TMPDIR: f.scratch }, platform: process.platform,
    snapshotRoot: path.join(f.base, "ownership"), classA: undefined, selfPath: path.join(f.base, "elsewhere", "rightkit-gate.mjs"), appCheck: async () => 0, spawn, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), runScans: fakeRun,
  });
  assert.equal(code, 0, `${out.join("\n")}\n${err.join("\n")}`);
  assert.equal(seen[0].options.cargoReadTargetRoot, f.scratch);
  assert.equal(seen[0].options.tmpRoot, undefined, "the deny and swift temporary files stay where they were on a direct host");
  assert.ok(seen[0].plan.scans.some((s) => s.class === "A" && s.tool === "cargo"), "the plan is the direct host's: plain cargo");
  assert.equal(f.verify(), f.tree);
});
