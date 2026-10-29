import { fileURLToPath } from "node:url";
// Item 7 rows 7.02 (the door's snapshot route on every host), 7.03 (`--to dev|test` build a commit), 7.04 (package), 7.05 (sidecars) and
// 7.06 (the stage table cannot drift): docs/GATE-CONTRACT.md "Commit-only build", signed map signed/item7-MAP-SIGNED.md.
//
// Evidence labels. Every door test here runs the real snapshot route (a real `git` export synced into the test's own snapshot root, real
// build ownership, real verification) on a *simulated* Linux host (`platform: "linux"`): that is fixture evidence on the Mac and the
// Dell, native only where the file runs on Linux. The tests that start the real right-release CLI are Linux-only (`LINUX`): on a Mac or
// a Dell right-release's children go through the broker, which this gate refuses until the snapshot carrier is wired (rows 7.20 to 7.24).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { main, snapshotRouteRefusal, snapshotRouteSelected, snapshotAppRootOf } from "./bin/rightkit-gate.mjs";
import { STAGE_SOURCES, STAGES, stageSourceGaps, stageSourceProblems } from "./gate-stages.mjs";
import { SourceSplitError, doorContextFor, readSourceSplit } from "./source-split.mjs";
import { stageSidecars } from "./stage-sidecar.mjs";

const LINUX = process.platform === "linux" ? false : "real right-release children run only on a Linux host (broker hosts are refused until the snapshot carrier is wired)";
/** Starts the real right-release child with its output captured, so a failed real run says why. */
const capturedChild = () => {
  const lines = [];
  return { lines, spawnInherit: (cmd, a, opts) => { const r = spawnSync(cmd, a, { ...opts, encoding: "utf8" }); lines.push(String(r.stdout ?? ""), String(r.stderr ?? "")); return { status: r.status }; } };
};
const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;

function tmp(t, prefix) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}
const git = (cwd, ...a) => spawnSync("git", a, { cwd, encoding: "utf8" });
const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel)); };

/** A repository of `files`, one commit, and a door runner on a simulated Linux host with its own snapshot root. */
function fixture(t, files, prefix = "rk-ds-") {
  const dir = tmp(t, prefix);
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t");
  const write = (rel, body) => { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); };
  for (const [rel, body] of Object.entries(files)) write(rel, body);
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c");
  const sha = git(repo, "rev-parse", "HEAD").stdout.trim();
  const snaps = path.join(dir, "snaps");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  // Real git, and the pnpm version probe the class A environment digest asks; nothing else of the host is reachable from a fixture.
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : c === "pnpm" && a[0] === "--version" ? { status: 0, stdout: "11.25.0\n", stderr: "" } : { status: 1, stdout: "", stderr: "not in this fixture" });
  const door = async (args, extra = {}) => {
    const out = []; const err = [];
    const code = await main(args, { cwd: repo, platform: "linux", snapshotRoot: snaps, env, spawn, out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {}, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), ...extra });
    return { code, out: out.join("\n"), err: err.join("\n"), all: [...out, ...err].join("\n") };
  };
  const snapshot = () => path.join(snaps, fs.readdirSync(snaps).find((n) => !n.includes(".") && fs.statSync(path.join(snaps, n)).isDirectory()));
  return { dir, repo, sha, snaps, env, write, door, snapshot, git: (...a) => git(repo, ...a) };
}

// ---- 7.06: the stage table cannot drift --------------------------------------------------------------------------------------

test("[7.06] every stage the gate dispatches has a row in STAGE_SOURCES, and none that compiles or packages lacks a snapshot source, an exclusion basis or a registered gap", () => {
  const expected = [...STAGES, "dev", "test", "stage", ...STAGES.map((s) => `unit:${s}`)].sort();
  assert.deepEqual(Object.keys(STAGE_SOURCES).sort(), expected, "the table names STAGES, dev, test, stage and every per-unit stage, and nothing else");
  assert.deepEqual(stageSourceProblems(), [], "no compiling or packaging stage without a source");
  // The two stages the contract excludes carry their stated basis, not a bare flag.
  for (const stage of ["release", "stage", "unit:release"]) assert.match(STAGE_SOURCES[stage].basis, /.{40}/, `${stage} states why it reads no snapshot`);
  // Row 7.06a closed the unit-stage gap: no stage compiles or packages from the checkout any more, and a new one fails here.
  assert.deepEqual(stageSourceGaps(), [], "a new stage that compiles from the checkout fails here");
  for (const stage of ["compile", "dev", "test", "package", "qualify", "unit:check", "unit:compile", "unit:package", "unit:qualify"]) assert.equal(STAGE_SOURCES[stage].source, "snapshot", `${stage} reads the commit's snapshot`);
  assert.equal(STAGE_SOURCES.qualify.wired, false, "qualify's source reads go through the split, which waits for the identity module (7.04)");
  assert.equal(STAGE_SOURCES.package.wired, false, "package's snapshot route is refused today, and the table says so (row 7.04 blocker)");
});

test("[7.06] negative: a stage stub that compiles from the checkout, or packages with no basis, fails the table check", () => {
  assert.equal(stageSourceProblems({ ...STAGE_SOURCES, extra: { compiles: true, packages: false, source: "checkout" } }).length, 1);
  assert.equal(stageSourceProblems({ ...STAGE_SOURCES, extra: { compiles: false, packages: true, source: "checkout", gap: "" } }).length, 1, "an empty gap note is no registered gap");
  assert.equal(stageSourceProblems({ ...STAGE_SOURCES, extra: { compiles: true, packages: false, source: "sealed" } }).length, 1, "an exclusion with no basis");
  assert.equal(stageSourceProblems({ ...STAGE_SOURCES, extra: { compiles: true, packages: false, source: "publisher", basis: "x" } }).length, 0, "a stated basis excludes (here a one-character one, which the other test forbids for real rows)");
  assert.deepEqual(stageSourceGaps({ ...STAGE_SOURCES, extra: { compiles: true, packages: false, source: "checkout", gap: "g" } }).sort(), ["extra"], "a new gap shows in the list the table test pins");
  assert.equal(stageSourceProblems({ ...STAGE_SOURCES, extra: { compiles: false, packages: false, source: "checkout" } }).length, 0, "a stage that neither compiles nor packages needs no source");
});

test("[7.06] behavior: each stage that reads a snapshot reads the selected commit's snapshot, never the checkout (observed roots and launches, one dirty checkout)", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "README.md": "committed\n", "package.json": '{"name":"x","private":true}\n' }, "rk-ds-drift-");
  fx.write("README.md", "DIRTY\n");
  const observed = {};
  const note = (stage, root) => { (observed[stage] ??= []).push(root); };
  const scans = (stage) => async (plan) => { note(stage, plan.roots.contentRoot); note(stage, plan.roots.liveRoot); return { scans: [], notes: [], cacheHits: [] }; };
  // check: the class A scans read the snapshot.
  let r = await fx.door(["--to", "check", "--commit", fx.sha], { runScans: scans("check") });
  assert.equal(r.code, 0, r.all);
  // compile: check is stubbed, the compile's scans read the snapshot.
  r = await fx.door(["--to", "compile", "--commit", fx.sha], { runCheck: async () => 0, runScans: scans("compile") });
  assert.equal(r.code, 0, r.all);
  // dev and test: right-release starts with its cwd in the snapshot.
  for (const to of ["dev", "test"]) {
    r = await fx.door(["--to", to, "--commit", fx.sha], { runCheck: async () => 0, spawnInherit: (cmd, a, opts) => { note(to, opts.cwd); return { status: 0 }; } });
    assert.equal(r.code, 0, r.all);
  }
  // package: right-release (source QA, then the build) starts with its cwd in the snapshot (row 7.04).
  r = await fx.door(["--to", "package", "--platform", "mac", "--commit", fx.sha], { runCheck: async () => 0, runCompile: async () => 0, sealedFor: () => ({ id: "x-1.0.0-00000000", manifest: "m", digest: "d".repeat(64) }), spawnInherit: (c, a, opts) => { note("package", opts.cwd); return { status: 0 }; } });
  assert.equal(r.code, 0, r.all);
  for (const [stage, roots] of Object.entries(observed)) {
    assert.ok(roots.length > 0, `${stage} was observed`);
    for (const root of roots) assert.ok(inside(root, fx.snaps) && !inside(root, fx.repo), `${stage} reads ${root}, which is not the snapshot under ${fx.snaps}`);
  }
  assert.deepEqual(Object.keys(observed).sort(), ["check", "compile", "dev", "package", "test"]);
  for (const [stage, row] of Object.entries(STAGE_SOURCES)) if (row.source === "snapshot" && row.wired !== false && !stage.startsWith("unit:")) assert.ok(observed[stage], `${stage} is declared a snapshot stage and was observed on the snapshot`);
});

// ---- 7.03: --to dev and --to test build a commit ----------------------------------------------------------------------------------

test("[7.03] --to dev|test: class A is the snapshot's, never --worktree; right-release starts inside the snapshot with the split in its door run; an uncommitted edit is absent", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "marker.txt": "committed\n", "right-release.config.json": "{}\n" }, "rk-ds-dev-");
  fx.write("marker.txt", "DIRTY\n"); fx.write("untracked.txt", "u\n");
  for (const to of ["dev", "test"]) {
    const launches = [];
    const r = await fx.door(["--to", to, "--commit", fx.sha, "--", "--platform", "mac"], {
      runCheck: undefined, runScans: async () => ({ scans: [], notes: [], cacheHits: [] }),
      spawnInherit: (cmd, a, opts) => {
        const nonce = opts.env.RK_GATE_RUN;
        const run = JSON.parse(fs.readFileSync(path.join(fx.repo, ".git", "rightkit-gate", "runs", `${nonce}.json`), "utf8"));
        launches.push({ args: a.slice(1), cwd: opts.cwd, marker: fs.readFileSync(path.join(opts.cwd, "marker.txt"), "utf8"), untracked: fs.existsSync(path.join(opts.cwd, "untracked.txt")), run, split: readSourceSplit({ env: opts.env, cwd: opts.cwd }) });
        return { status: 0 };
      },
    });
    assert.equal(r.code, 0, r.all);
    // Class A ran on the snapshot: its output says so, and no line speaks of the working tree.
    assert.match(r.all, /snapshot .* is commit [0-9a-f]{40} tree [0-9a-f]{40} \(verified before the check\)/, "the door's class A is `check --commit <sha> --full-a --snapshot <dir>`");
    assert.doesNotMatch(r.all, /working tree|--worktree/);
    assert.equal(launches.length, 1);
    const [l] = launches;
    assert.deepEqual(l.args, [to, "--platform", "mac"]);
    assert.ok(inside(l.cwd, fx.snaps) && !inside(l.cwd, fx.repo), "right-release's cwd is the snapshot's app root");
    assert.equal(l.marker, "committed\n", "the uncommitted edit is absent from what right-release reads");
    assert.equal(l.untracked, false);
    assert.equal(l.run.source.commit, fx.sha); assert.equal(l.run.source.snapshot, l.cwd); assert.equal(l.run.source.appDir, ".");
    assert.equal(l.split.checkout, fs.realpathSync(fx.repo)); assert.equal(l.split.appRoot, l.cwd);
    // The checkout was never built and keeps its edit.
    assert.equal(fs.readFileSync(path.join(fx.repo, "marker.txt"), "utf8"), "DIRTY\n");
  }
});

test("[7.03] --to dev|test: a dirty checkout neither blocks nor leaks; a red class A launches nothing; a snapshot changed during right-release voids the result (exit 2)", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "marker.txt": "committed\n" }, "rk-ds-dev2-");
  fx.write("marker.txt", "DIRTY\n");
  const launches = [];
  const spawnInherit = () => { launches.push("x"); return { status: 0 }; };
  let r = await fx.door(["--to", "dev"], { runCheck: async () => 1, spawnInherit });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /--to dev stopped at check: nothing compiled/); assert.deepEqual(launches, []);
  // --commit selects another commit: the snapshot is that commit's, whatever HEAD is.
  fx.write("marker.txt", "second\n"); fx.git("commit", "-qam", "two");
  const seen = [];
  r = await fx.door(["--to", "test", "--commit", fx.sha], { runCheck: async () => 0, spawnInherit: (c, a, opts) => { seen.push(fs.readFileSync(path.join(opts.cwd, "marker.txt"), "utf8")); return { status: 0 }; } });
  assert.equal(r.code, 0, r.all); assert.deepEqual(seen, ["committed\n"], "the first commit, not HEAD");
  // A byte changed in the snapshot while right-release ran: the stage covers no known bytes.
  r = await fx.door(["--to", "dev", "--commit", fx.sha], { runCheck: async () => 0, spawnInherit: (c, a, opts) => { fs.writeFileSync(path.join(opts.cwd, "marker.txt"), "tampered\n"); return { status: 0 }; } });
  assert.equal(r.code, 2, r.all); // Detected at the END of the stage (closeSnapshotStage): a tracked-byte rewrite passes the name walk and is caught by the tree id. The start-of-stage
  // refusal says "nothing compiled" instead, so the two phases are told apart by their closing line.
  assert.match(r.err, /RK_CHECK_ERROR: \[dev\] the snapshot at .* (is not exactly commit|is tree [0-9a-f]+, not tree [0-9a-f]+ of commit)/);
  assert.match(r.err, /--to dev stopped: the result covers no known bytes/); assert.doesNotMatch(r.err, /nothing compiled/);
  // A right-release failure is a failure.
  r = await fx.door(["--to", "test", "--commit", fx.sha], { runCheck: async () => 0, spawnInherit: () => ({ status: 3 }) });
  assert.equal(r.code, 1, r.all);
});

test("[7.03] hosted CI keeps its runner checkout (it is the commit): class A of HEAD, never --worktree, and a differing checkout is refused", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "marker.txt": "committed\n" }, "rk-ds-ci-");
  const ci = { ...fx.env, GITHUB_ACTIONS: "true" };
  const cwds = [];
  let r = await fx.door(["--to", "dev"], { env: ci, runCheck: async () => 0, spawnInherit: (c, a, opts) => { cwds.push(opts.cwd); return { status: 0 }; } });
  assert.equal(r.code, 0, r.all); assert.equal(fs.realpathSync(cwds[0]), fs.realpathSync(fx.repo), "right-release runs in the checkout's app root");
  assert.ok(!fs.existsSync(fx.snaps), "no snapshot on hosted CI");
  fx.write("marker.txt", "DIRTY\n");
  r = await fx.door(["--to", "dev"], { env: ci, runCheck: async () => 0, spawnInherit: () => ({ status: 0 }) });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /the checkout differs from HEAD/);
});

// ---- 7.02: broker hosts refuse until the carrier is wired --------------------------------------------------------------------------

const brokerExecutor = () => ({ kind: "broker", cargo: ["rustright", "cargo"] });

test("[7.02 interim] Mac and Dell keep the checkout route until the cutover commit: no carrier seam means no snapshot, no ownership, class A of the working tree for dev|test, a clean HEAD for the rest", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "README.md": "r\n" }, "rk-ds-broker-");
  const calls = [];
  const spies = { syncSnapshot: () => { calls.push("sync"); throw new Error("must not sync"); }, acquireOwnership: () => { calls.push("own"); throw new Error("must not acquire"); }, runCheck: async () => { calls.push("check"); return 0; }, runCompile: async () => { calls.push("compile"); return 0; } };
  const cwds = [];
  for (const platform of ["darwin", "win32"]) {
    for (const to of ["compile", "dev", "test"]) {
      const r = await fx.door(["--to", to], { platform, resolveExecutor: brokerExecutor, spawnInherit: (c, a, opts) => { cwds.push(opts.cwd); return { status: 0 }; }, ...spies });
      assert.equal(r.code, 0, `${platform} ${to}: ${r.all}`);
      assert.doesNotMatch(r.all, /snapshot .* is commit|build ownership/, `${platform} ${to} took the checkout route`);
    }
  }
  assert.ok(!calls.includes("sync") && !calls.includes("own"), "no snapshot and no ownership on the checkout route");
  assert.ok(cwds.length > 0 && cwds.every((c) => fs.realpathSync(c) === fs.realpathSync(fx.repo)), "right-release ran in the checkout's app root (the route as it was)");
  assert.ok(!fs.existsSync(fx.snaps), "no snapshot was written");
  // The checkout route's clean-HEAD rule is unchanged.
  fx.write("README.md", "DIRTY\n");
  const dirty = await fx.door(["--to", "compile"], { platform: "darwin", resolveExecutor: brokerExecutor, ...spies });
  assert.equal(dirty.code, 1); assert.match(dirty.err, /BLOCKED: the checkout differs from HEAD/);
  // A broker that cannot be reached is still refused on that route.
  const down = await fx.door(["--to", "compile"], { platform: "darwin", resolveExecutor: () => ({ kind: "refuse", problem: "the RightKit broker is not reachable" }), ...spies });
  assert.equal(down.code, 1);
});

test("[7.02] with the carrier seam wired (deps.snapshotBroker, tests only before the cutover) a broker host takes the snapshot route, and once selected any failure refuses (no retry on the checkout); dev, test and sidecars still wait for the per-call wrapper (row 7.24)", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "README.md": "r\n", "right-release.config.json": '{"sidecars":[{"name":"a"}]}\n' }, "rk-ds-broker2-");
  fx.write("README.md", "DIRTY\n");
  const seam = { controlRoot: path.join(fx.dir, "control") };
  const down = await fx.door(["--to", "compile", "--commit", fx.sha], { platform: "darwin", resolveExecutor: () => ({ kind: "refuse", problem: "the RightKit broker is not reachable" }), snapshotBroker: seam, runCheck: async () => 0 });
  assert.equal(down.code, 1, down.all); assert.match(down.err, /BLOCKED: RK_SNAPSHOT_BROKER_UNAVAILABLE: the RightKit broker is not reachable; nothing ran and the checkout is never built/);
  const launches = []; let compiled = false;
  let r = await fx.door(["--to", "compile", "--commit", fx.sha], { platform: "darwin", resolveExecutor: brokerExecutor, snapshotBroker: seam, runCheck: async () => 0, runCompile: async () => { compiled = true; return 0; }, spawnCapture: () => { launches.push("x"); return { status: 0, stdout: "{}" }; } });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /BLOCKED: RK_SNAPSHOT_CARRIER_NOT_WIRED: \[compile\] right-release\.config\.json stage sidecars/);
  assert.deepEqual(launches, []); assert.equal(compiled, false);
  r = await fx.door(["--to", "dev", "--commit", fx.sha], { platform: "darwin", resolveExecutor: brokerExecutor, snapshotBroker: seam, runCheck: async () => 0, spawnInherit: () => { launches.push("x"); return { status: 0 }; } });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /RK_SNAPSHOT_CARRIER_NOT_WIRED: right-release dev and test reach Cargo/); assert.deepEqual(launches, []);
  // A direct executor on any host (an unmanaged Mac or Dell) needs no carrier.
  assert.equal(snapshotRouteRefusal({ platform: "darwin", env: {}, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", resolveExecutor: () => ({ kind: "direct" }) }), null);
  assert.equal(snapshotRouteSelected({ platform: "darwin" }), false); assert.equal(snapshotRouteSelected({ platform: "win32" }), false);
  assert.equal(snapshotRouteSelected({ platform: "linux" }), true); assert.equal(snapshotRouteSelected({ platform: "darwin", snapshotBroker: {} }), true);
});

// ---- 7.04: package --------------------------------------------------------------------------------------------------------------

// 7.04 (package from the snapshot) is proven in door-package.test.mjs.

// ---- 7.05: sidecars on the snapshot (gate) and the right-release source split (stage-sidecar) ----------------------------------

test("[7.05] a commit that declares sidecars stages them ONCE from the snapshot's config, through stage-sidecar --json, and a staging step that fails compiles nothing", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "right-release.config.json": '{"sidecars":[{"name":"a"}]}\n', "README.md": "r\n" }, "rk-ds-side-");
  fx.write("right-release.config.json", "{}\n"); // the checkout hides them
  const launches = [];
  const capture = (c, a, opts) => { launches.push({ a: a.slice(1), cwd: opts.cwd, env: opts.env }); return { status: 0, stdout: JSON.stringify({ files: [], receiptPath: null }), stderr: "" }; };
  let compiled = 0;
  let r = await fx.door(["--to", "compile", "--commit", fx.sha], { runCheck: async () => 0, runCompile: async () => { compiled += 1; return 0; }, spawnCapture: capture });
  assert.equal(r.code, 0, r.all); assert.equal(compiled, 1);
  assert.equal(launches.length, 1, "one staging launch per door run");
  assert.deepEqual(launches[0].a, ["stage-sidecar", "--config", path.join(fx.snapshot(), "right-release.config.json"), "--json"]);
  assert.equal(launches[0].cwd, fx.snapshot());
  assert.ok(launches.every((l) => !Object.hasOwn(l.env, "TAURI_CONFIG")), "no child environment carries TAURI_CONFIG");
  // A failing staging step compiles nothing.
  compiled = 0;
  r = await fx.door(["--to", "compile", "--commit", fx.sha], { runCheck: async () => 0, runCompile: async () => { compiled += 1; return 0; }, spawnCapture: () => ({ status: 1, stdout: "", stderr: "boom" }) });
  assert.equal(r.code, 1, r.all); assert.equal(compiled, 0);
  // TAURI_CONFIG in the environment is refused before anything runs, never passed on.
  const refused = await fx.door(["--to", "compile", "--commit", fx.sha], { env: { ...fx.env, TAURI_CONFIG: '{"bundle":{"externalBin":["x"]}}' }, runCheck: async () => 0, runCompile: async () => 0, spawnCapture: capture });
  assert.equal(refused.code, 1); assert.match(refused.err, /BLOCKED: RK_TAURI_CONFIG_OVERRIDE/);
});

test("[7.05] stage-sidecar source split: caches and cmake trees go to the checkout's app directory, the staged file to the snapshot; cargo output is pinned to the checkout's target; catalog kinds are refused", async (t) => {
  const dir = tmp(t, "rk-ds-sc-");
  const snapshot = path.join(dir, "snapshot", "app"); const checkout = path.join(dir, "checkout", "app");
  for (const d of [snapshot, checkout]) fs.mkdirSync(d, { recursive: true });
  const bytes = Buffer.from("model-bytes");
  const sha = (b) => createHash("sha256").update(b).digest("hex");
  const fetched = [];
  const fetch = async (url) => { fetched.push(url); return { ok: true, status: 200, arrayBuffer: async () => bytes }; };
  const entry = { kind: "remote", name: "r2", url: "https://models.example.com/r2.onnx", sha256: sha(bytes), dest: "src-tauri/resources/models/r2.onnx" };
  const target = "x86_64-unknown-linux-gnu";
  const result = await stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest: [entry], target, deps: { fetch, env: {}, log: () => {}, now: () => "2026-10-29T00:00:00.000Z" } });
  assert.equal(fs.readFileSync(path.join(snapshot, "src-tauri", "resources", "models", "r2.onnx")).toString(), "model-bytes", "the staged file is beside the snapshot's sources");
  assert.ok(fs.existsSync(path.join(checkout, ".right-release", "cache", "remote", sha(bytes))), "the download cache is the checkout's");
  assert.ok(!fs.existsSync(path.join(snapshot, ".right-release")), "nothing of right-release's own under the snapshot");
  assert.equal(path.dirname(result.receiptPath), path.join(snapshot, "src-tauri", "binaries"));
  // Without a split the cache stays under the app root (unchanged behavior).
  const plain = path.join(dir, "plain"); fs.mkdirSync(plain);
  await stageSidecars({ appRoot: plain, manifest: [entry], target, deps: { fetch, env: {}, log: () => {}, now: () => "2026-10-29T00:00:00.000Z" } });
  assert.ok(fs.existsSync(path.join(plain, ".right-release", "cache", "remote", sha(bytes))));
  // cargo-bin: Cargo's output directory is resolved in the checkout and passed absolute, never left to resolve against the snapshot.
  const calls = [];
  const targetDir = path.join(dir, "checkout-target");
  const exe = path.join(targetDir, target, "release", "eng");
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    if (args[1] === "metadata") return { status: 0, stdout: JSON.stringify({ target_directory: targetDir }), stderr: "" };
    if (args[1] === "build") { fs.mkdirSync(path.dirname(exe), { recursive: true }); fs.writeFileSync(exe, "engine"); return { status: 0, stdout: `${JSON.stringify({ reason: "compiler-artifact", executable: exe, target: { name: "eng", kind: ["bin"] } })}\n`, stderr: "" }; }
    return { status: 0, stdout: "", stderr: "" };
  };
  await stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest: [{ kind: "cargo-bin", package: "eng" }], target, deps: { run, env: {}, log: () => {}, now: () => "2026-10-29T00:00:00.000Z" } });
  const meta = calls.find((c) => c.args[1] === "metadata"); const build = calls.find((c) => c.args[1] === "build");
  assert.equal(meta.options.cwd, checkout, "the output directory is resolved in the checkout");
  assert.equal(build.options.cwd, snapshot, "the build reads the snapshot's sources");
  assert.equal(build.options.env.CARGO_TARGET_DIR, targetDir, "and writes to the checkout's target directory");
  assert.ok(!fs.existsSync(path.join(snapshot, "target")), "no target/ under the snapshot");
  // The boundary (row 7.05a): a catalog or source the config names must resolve inside the snapshot, never into the checkout.
  await assert.rejects(stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest: [{ kind: "onnxruntime", version: "1.0.0", catalog: path.join(checkout, "catalog.json") }], target, deps: { env: {}, log: () => {} } }), { code: "STAGE_SIDECAR_SOURCE_OUTSIDE_SNAPSHOT" });
  await assert.rejects(stageSidecars({ appRoot: snapshot, cacheRoot: "relative", manifest: [entry], target, deps: { env: {} } }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
});

// ---- the source split: what a right-release child may believe about where it runs ---------------------------------------------

test("[7.03-7.05] source split: no checkout in the env is no split; every inconsistency is a refusal; a valid run yields the split", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "app/README.md": "r\n" }, "rk-ds-split-");
  assert.equal(readSourceSplit({ env: {}, cwd: fx.repo }), null);
  assert.equal(readSourceSplit({ env: { RK_GATE_CHECKOUT: "" }, cwd: fx.repo }), null);
  let seen = null;
  // Read inside the door run, while it is live: a split child proves its split from the run file, which exists only for the run's duration.
  const r = await fx.door(["--to", "dev", "--commit", fx.sha], { runCheck: async () => 0, spawnInherit: (c, a, opts) => {
    const probe = (cwd) => { try { return { split: readSourceSplit({ env: opts.env, cwd }) }; } catch (error) { return { error }; } };
    seen = { env: opts.env, cwd: opts.cwd, inside: probe(opts.cwd), inCheckout: probe(fx.repo), aboveSnapshot: probe(path.join(opts.cwd, "..")) };
    return { status: 0 };
  } });
  assert.equal(r.code, 0, r.all);
  const { env, cwd } = seen;
  const split = seen.inside.split;
  assert.equal(split.commit, fx.sha); assert.equal(split.appDir, "."); assert.equal(Object.isFrozen(split), true);
  // Inside the snapshot only.
  assert.ok(seen.inCheckout.error instanceof SourceSplitError && /outside the snapshot/.test(seen.inCheckout.error.message), "a process in the checkout is no split child");
  assert.match(seen.aboveSnapshot.error?.message ?? "", /outside the snapshot/);
  // The door run has ended and its run file is gone: nothing may claim its split any longer.
  assert.throws(() => readSourceSplit({ env, cwd }), (e) => e instanceof SourceSplitError && /no live door run/.test(e.message), "after the door run ends nothing may claim its split");
  // Forged shapes.
  const runsDir = path.join(fx.repo, ".git", "rightkit-gate", "runs");
  const nonce = "a".repeat(32);
  const writeRun = (run) => { fs.mkdirSync(runsDir, { recursive: true }); fs.writeFileSync(path.join(runsDir, `${nonce}.json`), JSON.stringify(run)); };
  const good = { pid: process.pid, commit: fx.sha, tree: git(fx.repo, "rev-parse", `${fx.sha}^{tree}`).stdout.trim(), stage: "dev", source: { checkout: fs.realpathSync(fx.repo), snapshot: cwd, commit: fx.sha, tree: git(fx.repo, "rev-parse", `${fx.sha}^{tree}`).stdout.trim(), appDir: "." } };
  const forged = { RK_GATE_RUN: nonce, RK_GATE_CHECKOUT: fs.realpathSync(fx.repo) };
  writeRun(good);
  assert.equal(readSourceSplit({ env: forged, cwd }).snapshot, cwd, "a well-formed live run is accepted");
  for (const [label, mutate, pattern] of [
    ["no source", (r) => { delete r.source; }, /carries no source split/],
    ["relative snapshot", (r) => { r.source.snapshot = "rel"; }, /source\.snapshot is not an absolute path/],
    ["bad commit", (r) => { r.source.commit = "nope"; }, /source\.commit is not an object id/],
    ["appDir escapes", (r) => { r.source.appDir = "../x"; }, /appDir is not a contained relative path/],
    ["another commit in run and source", (r) => { r.commit = "b".repeat(40); }, /commit and its source split's commit differ/],
    ["another checkout", (r) => { r.source.checkout = os.tmpdir(); }, /the door run was made in/],
  ]) {
    const run = JSON.parse(JSON.stringify(good)); mutate(run); writeRun(run);
    assert.throws(() => readSourceSplit({ env: forged, cwd }), pattern, label);
  }
  writeRun({ ...good, pid: 2 ** 22 + 12345 });
  assert.throws(() => readSourceSplit({ env: forged, cwd }), /no live door run \(it has ended\)/, "a dead run");
  assert.throws(() => readSourceSplit({ env: { RK_GATE_CHECKOUT: "relative" }, cwd }), /must be an absolute path/);
  assert.throws(() => readSourceSplit({ env: { RK_GATE_CHECKOUT: os.tmpdir() }, cwd }), /not a git checkout/);
  // doorContextFor: the door-checked steps (door.mjs, web-build, the CLI) read the run's checkout and commit inside a split, HEAD elsewhere.
  writeRun(good);
  const ctx = doorContextFor({ env: forged, cwd });
  assert.equal(ctx.commit, fx.sha); assert.equal(ctx.commonDir, path.join(fs.realpathSync(fx.repo), ".git")); assert.ok(ctx.split);
  const plain = doorContextFor({ env: {}, cwd: fx.repo });
  assert.equal(plain.commit, fx.sha); assert.equal(plain.split, null);
  assert.equal(doorContextFor({ env: {}, cwd: os.tmpdir() }), null);
  assert.equal(snapshotAppRootOf("/s", "."), "/s"); assert.equal(snapshotAppRootOf("/s", "apps/one"), path.join("/s", "apps", "one"));
});

test("[7.03] right-release under a split: dev, test, stage-sidecar, web-build, build, package, release and qa run (build, package, release and qa through release-source-identity.mjs); a desktop upload, publish and signing refuse with RK_SOURCE_SPLIT_UNSUPPORTED; a stray split env fails closed", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "right-release.config.json": "{}\n", "README.md": "r\n" }, "rk-ds-cli-");
  const cli = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
  let childEnv = null; let childCwd = null;
  const r = await fx.door(["--to", "dev", "--commit", fx.sha], { runCheck: async () => 0, spawnInherit: (c, a, opts) => { childEnv = opts.env; childCwd = opts.cwd; return { status: 0 }; } });
  assert.equal(r.code, 0, r.all);
  // The door run is over by now; keep one alive for the CLI by writing the same file under this process's pid.
  const runsDir = path.join(fx.repo, ".git", "rightkit-gate", "runs"); fs.mkdirSync(runsDir, { recursive: true });
  const nonce = "c".repeat(32); const tree = git(fx.repo, "rev-parse", `${fx.sha}^{tree}`).stdout.trim();
  fs.writeFileSync(path.join(runsDir, `${nonce}.json`), JSON.stringify({ pid: process.pid, commit: fx.sha, tree, stage: "dev", source: { checkout: fs.realpathSync(fx.repo), snapshot: childCwd, commit: fx.sha, tree, appDir: "." } }));
  const env = { ...childEnv, RK_GATE_RUN: nonce, RK_GATE_CHECKOUT: fs.realpathSync(fx.repo) };
  const run = (args, cwd = childCwd, e = env) => spawnSync(process.execPath, [cli, ...args], { cwd, env: e, encoding: "utf8" });
  for (const args of [["upload", "--platform", "mac", "--release", "x", "--tier", "patch"], ["publish-update", "--release", "x", "--platform", "mac", "--tier", "patch"], ["sign-windows"], ["github", "--release", "x"]]) {
    const out = run(args);
    assert.equal(out.status, 2, `${args.join(" ")}: ${out.stderr}`);
    assert.match(out.stderr, /RK_SOURCE_SPLIT_UNSUPPORTED: right-release .* cannot run from the snapshot of [0-9a-f]{12} yet/);
    assert.match(out.stderr, /nothing was started/);
  }
  // A process in the checkout holding a split env is refused (fail closed), whatever it runs.
  const stray = run(["check", "--list"], fx.repo);
  assert.equal(stray.status, 2, stray.stderr); assert.match(stray.stderr, /RK_SOURCE_SPLIT: this process runs in .*, outside the snapshot/);
  // An unrelated environment (no split) is untouched: a read-only command still works.
  const plain = spawnSync(process.execPath, [cli, "check", "--list"], { cwd: fx.repo, env: Object.fromEntries(Object.entries(env).filter(([k]) => !k.startsWith("RK_GATE"))), encoding: "utf8" });
  assert.equal(plain.status, 0, plain.stderr);
});

// ---- real right-release children on Linux --------------------------------------------------------------------------------------

test("[7.03] real right-release test from the snapshot (Linux): the suite runs in the snapshot's app root and reads the commit's bytes, not the dirty checkout's", { skip: LINUX || POSIX_SIM }, async (t) => {
  const probeOut = path.join(tmp(t, "rk-ds-real-out-"), "probe.json");
  const fx = fixture(t, {
    "right-release.config.json": JSON.stringify({ schema: 1, app: "probe", version: "1.0.0", tests: { suites: [{ name: "probe", cmd: "node", args: ["scripts/probe.mjs"] }] } }, null, 2) + "\n",
    "scripts/probe.mjs": 'import fs from "node:fs"; fs.writeFileSync(process.env.PROBE_OUT, JSON.stringify({ cwd: process.cwd(), marker: fs.readFileSync("marker.txt", "utf8"), checkout: process.env.RK_GATE_CHECKOUT }));\n',
    "marker.txt": "committed\n",
    "switches.json": JSON.stringify({ schema: 1, app: "probe", switches: [] }) + "\n",
  }, "rk-ds-real-");
  fx.write("marker.txt", "DIRTY\n");
  fx.env.PROBE_OUT = probeOut;
  const child = capturedChild();
  const r = await fx.door(["--to", "test", "--commit", fx.sha], { runCheck: async () => 0, spawnInherit: child.spawnInherit });
  assert.equal(r.code, 0, `${r.all}\n${child.lines.join("\n")}`);
  const probe = JSON.parse(fs.readFileSync(probeOut, "utf8"));
  assert.equal(probe.marker, "committed\n", "the suite read the commit's bytes");
  assert.equal(fs.realpathSync(probe.cwd), fs.realpathSync(fx.snapshot()), "its cwd is the snapshot");
  assert.equal(probe.checkout, fs.realpathSync(fx.repo));
  assert.equal(fs.readFileSync(path.join(fx.repo, "marker.txt"), "utf8"), "DIRTY\n", "the checkout keeps its edit");
});

test("[7.03] real right-release dev from the snapshot (Linux): its build step runs in the snapshot's app root with Cargo's output pinned to the checkout's target directory; the checkout's edits stay untouched", { skip: LINUX || POSIX_SIM }, async (t) => {
  const dir = tmp(t, "rk-ds-real-dev-");
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  const files = {
    // `rustright cargo build` is an admitted dev step; on Linux it is the Cargo found on PATH, direct (a recording stand-in here).
    "right-release.config.json": JSON.stringify({ schema: 1, app: "probe", version: "1.0.0", development: { targets: { linux: { build: [{ cmd: "rustright", args: ["cargo", "build", "--locked"] }] } } } }, null, 2) + "\n",
    "Cargo.toml": '[package]\nname = "probe"\nversion = "0.1.0"\nedition = "2021"\n',
    "Cargo.lock": 'version = 4\n',
    "src/lib.rs": "pub fn committed() {}\n",
    "switches.json": JSON.stringify({ schema: 1, app: "probe", switches: [] }) + "\n",
  };
  for (const [rel, body] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t"); git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c");
  const sha = git(repo, "rev-parse", "HEAD").stdout.trim();
  fs.writeFileSync(path.join(repo, "src", "lib.rs"), "THIS IS NOT RUST\n");
  const toolDir = path.join(dir, "tools"); fs.mkdirSync(toolDir);
  const log = path.join(dir, "cargo-ran.jsonl");
  // A recording Cargo: `metadata` answers like cargo with build.target-dir = rk-target (relative to the workspace it is asked about).
  fs.writeFileSync(path.join(toolDir, "cargo"), `#!/usr/bin/env node
const fs = require("node:fs"); const path = require("node:path");
const a = process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ cwd: process.cwd(), target: process.env.CARGO_TARGET_DIR ?? null, args: a }) + "\\n");
if (a[0] === "metadata") {
  const i = a.indexOf("--manifest-path"); const root = i >= 0 ? path.dirname(a[i + 1]) : process.cwd();
  process.stdout.write(JSON.stringify({ packages: [{ name: "probe", id: "probe", version: "0.1.0", manifest_path: path.join(root, "Cargo.toml"), publish: null, targets: [], features: {}, dependencies: [] }], workspace_members: ["probe"], workspace_root: root, target_directory: path.join(root, "rk-target"), metadata: null, version: 1 }));
}
`);
  fs.chmodSync(path.join(toolDir, "cargo"), 0o755);
  const snaps = path.join(dir, "snaps");
  const driver = path.join(dir, "driver.mjs");
  fs.writeFileSync(driver, `const { main } = await import(${JSON.stringify(new URL("./bin/rightkit-gate.mjs", import.meta.url).href)});
const out = [];
const code = await main(["--to", "dev", "--commit", ${JSON.stringify(sha)}, "--", "--platform", "linux"], { cwd: ${JSON.stringify(repo)}, platform: "linux", snapshotRoot: ${JSON.stringify(snaps)}, out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {} });
console.log("\\n" + JSON.stringify({ code, out: out.join("\\n") }));\n`);
  const env = { ...process.env, NODE_OPTIONS: "", PATH: `${toolDir}:${process.env.PATH}`, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const r = spawnSync(process.execPath, [driver], { env, encoding: "utf8", cwd: repo });
  let res = {}; try { res = JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop()); } catch { res = { stdout: r.stdout, stderr: r.stderr }; }
  assert.equal(res.code, 0, `${JSON.stringify(res)}\n${r.stdout}\n${r.stderr}`.slice(0, 4000));
  const snapshot = path.join(snaps, fs.readdirSync(snaps).find((n) => !n.includes(".")));
  const ran = fs.readFileSync(log, "utf8").trim().split("\n").map((l) => JSON.parse(l));
  const resolved = ran.filter((c) => c.args[0] === "metadata");
  assert.ok(resolved.length > 0 && resolved.every((c) => fs.realpathSync(c.cwd) === fs.realpathSync(repo)), "the output directory was resolved in the checkout");
  const built = ran.filter((c) => c.args[0] === "build");
  assert.equal(built.length, 1, JSON.stringify(ran));
  assert.equal(fs.realpathSync(built[0].cwd), fs.realpathSync(snapshot), "the build step ran in the snapshot's app root");
  assert.equal(built[0].target, path.join(fs.realpathSync(repo), "rk-target"), "with Cargo's output pinned to the checkout's target directory");
  assert.match(res.out, /output directory for the Cargo workspace: .*rk-target \(the checkout's\)/);
  assert.ok(!fs.existsSync(path.join(snapshot, "rk-target")) && !fs.existsSync(path.join(snapshot, "target")), "nothing was written under the snapshot");
  assert.equal(fs.readFileSync(path.join(repo, "src", "lib.rs"), "utf8"), "THIS IS NOT RUST\n", "the checkout keeps its edit");
  assert.equal(fs.readFileSync(path.join(snapshot, "src", "lib.rs"), "utf8"), "pub fn committed() {}\n", "and the snapshot holds the commit's bytes");
});

test("[7.03] --to dev|test: more than one Cargo workspace is refused (RK_SNAPSHOT_OUTPUT_UNPINNED) rather than pointed at one shared target directory", { skip: POSIX_SIM }, async (t) => {
  const fx = fixture(t, { "a/Cargo.toml": "[package]\nname=\"a\"\n", "b/Cargo.toml": "[package]\nname=\"b\"\n" }, "rk-ds-pin-");
  const spawn = (c, a, opts) => {
    if (c === "cargo" && a[0] === "metadata") { const m = a[a.indexOf("--manifest-path") + 1]; return { status: 0, stdout: JSON.stringify({ workspace_root: path.dirname(m), target_directory: path.join(path.dirname(m), "target") }), stderr: "" }; }
    return c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "" };
  };
  const launches = [];
  let r = await fx.door(["--to", "dev", "--commit", fx.sha], { spawn, runCheck: async () => 0, spawnInherit: () => { launches.push("x"); return { status: 0 }; } });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: the commit holds 2 Cargo workspaces \(a, b\)/); assert.deepEqual(launches, []);
  // One workspace is pinned; a missing manifest in the checkout, or metadata that fails, is refused too.
  const one = fixture(t, { "a/Cargo.toml": "[package]\nname=\"a\"\n" }, "rk-ds-pin1-");
  const seen = [];
  r = await one.door(["--to", "test", "--commit", one.sha], { spawn, runCheck: async () => 0, spawnInherit: (c, a, opts) => { seen.push(opts.env.CARGO_TARGET_DIR); return { status: 0 }; } });
  assert.equal(r.code, 0, r.all); assert.deepEqual(seen, [path.join(one.repo, "a", "target")]);
  fs.rmSync(path.join(one.repo, "a", "Cargo.toml"));
  r = await one.door(["--to", "test", "--commit", one.sha], { spawn, runCheck: async () => 0, spawnInherit: () => ({ status: 0 }) });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /RK_SNAPSHOT_OUTPUT_UNPINNED: the checkout has no a\/Cargo\.toml/);
  r = await fixture(t, { "Cargo.toml": "[package]\nname=\"a\"\n" }, "rk-ds-pin2-").door(["--to", "test"], { spawn: (c, a, o) => (c === "git" ? spawnSync(c, a, { ...o, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "no cargo" }), runCheck: async () => 0, spawnInherit: () => ({ status: 0 }) });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /RK_SNAPSHOT_OUTPUT_UNPINNED: cargo metadata in the checkout reported no absolute target_directory for Cargo\.toml/);
});
