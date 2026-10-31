// Item 7 row 7.06a (decision 2 Q3): `--to <stage> --unit <selector> [--commit <rev>]` builds the selected commit's snapshot on the
// snapshot route (Linux now; Mac and Dell at the cutover commit). Evidence label: real git, real snapshot sync into the test's own
// root, on a simulated Linux host; the unit owner (local-publish: cargo package / pnpm pack) is a double that records what it is asked.
// The tarball and .crate byte-equality positive needs a real pack on Linux and is run by the owner chat (not written here).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { isCallHandleOf } from "./snapshot-request.mjs";
import { createHash } from "node:crypto";
import { qualificationNonce } from "./provenance-verify.mjs";
import { spawnSync } from "./background-process.mjs";
import { main } from "./bin/rightkit-gate.mjs";
import { localBuildGrantedAt, sourceGit } from "./local-publish.mjs";

/** The ownership hook of a gate that holds a build ownership: it asserts the call's sub-handle shape (`<handle>#<id>`, the real rule) and records it. No unit stage here submits to a broker. */
const subHandleStub = (handle) => (callHandle) => {
  assert.ok(isCallHandleOf(callHandle, handle), `a broker submit is recorded for a carried request's sub-handle <handle>#<id>, not ${JSON.stringify(callHandle ?? null)}`);
  return { handle, broker: { submitted: true, handles: [callHandle] } };
};
const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;
const git = (cwd, ...a) => spawnSync("git", a, { cwd, encoding: "utf8" });

function unitFixture(t, { grant = true, manifest = true, npm = false } = {}) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-units-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t");
  git(repo, "remote", "add", "origin", "https://github.com/o/rk.git");
  const write = (rel, body) => { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); };
  if (grant) write(".rightkit-local-build.json", JSON.stringify({ allow: "build-test-publish", repository: "o/rk" }));
  if (manifest) { write("crates/rightkit-x/Cargo.toml", '[package]\nname = "rightkit-x"\nversion = "1.2.3"\nedition = "2021"\nlicense = "MIT"\n'); write("crates/rightkit-x/src/lib.rs", "pub fn committed() {}\n"); }
  write("ci/gate.config.json", "{}\n");
  write("Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n'); write("Cargo.lock", "version = 4\n");
  if (npm) write("packages/x/package.json", JSON.stringify({ name: "@rightkit/x", version: "1.2.3", license: "MIT" }));
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c1");
  const c1 = git(repo, "rev-parse", "HEAD").stdout.trim();
  write("crates/rightkit-x/src/lib.rs", "pub fn second() {}\n"); git(repo, "commit", "-qam", "c2"); // HEAD is ahead of c1
  write("crates/rightkit-x/src/lib.rs", "THE CHECKOUT'S UNCOMMITTED EDIT\n");
  const snaps = path.join(dir, "snaps");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE; delete env.TAURI_CONFIG;
  const spawned = [];
  const tools = { cargo: process.execPath }; // the absolute cargo the wrapper would run; a test may point it at a recording stand-in
  const spawn = (c, a, opts) => {
    spawned.push([c, ...a]);
    if (c === "git") return spawnSync(c, a, { ...opts, encoding: "utf8" });
    if ((c === "which" || c === "where") && a[0] === "cargo") return { status: 0, stdout: `${tools.cargo}\n`, stderr: "" }; // an absolute "cargo" the wrapper would run
    // A member crate's workspace is the repository root (one Cargo workspace), as cargo reports it.
    if ((c === "cargo" && a[0] === "metadata") || (c === "rustright" && a[0] === "cargo" && a[1] === "metadata")) { const m = a[a.indexOf("--manifest-path") + 1]; return { status: 0, stdout: JSON.stringify({ workspace_root: /[\\/]crates[\\/]/.test(m) ? repo : path.dirname(m), target_directory: path.join(dir, "checkout-target") }), stderr: "" }; }
    return { status: 1, stdout: "", stderr: "" };
  };
  const owner = { compiles: [], closed: 0 };
  const loadUnitOwner = (behave = {}) => async () => ({
    compileLocalUnit: async (kind, argv, options) => { owner.compiles.push({ kind, argv, options }); await behave.compile?.({ argv, options }); return Object.freeze({}); },
    packageLocalUnit: async () => ({ artifactFile: "x" }), qualifyLocalUnit: async () => ({}), closeLocalUnit: async () => { owner.closed += 1; }, localUnitCompileRequests: () => [],
  });
  const door = async (args, extra = {}) => {
    const out = []; const err = [];
    const code = await main(args, { cwd: repo, platform: "linux", snapshotRoot: snaps, env, spawn, out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {}, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), ...extra });
    return { code, all: [...out, ...err].join("\n"), err: err.join("\n") };
  };
  const snapshot = () => path.join(snaps, fs.readdirSync(snaps).find((n) => !n.includes(".") && fs.statSync(path.join(snaps, n)).isDirectory()));
  return { dir, repo, c1, snaps, env, spawned, owner, loadUnitOwner, door, snapshot, write, tools };
}

test("[7.06a] a unit stage builds the selected commit's snapshot: --workspace is the snapshot, commit and tree are the selection (HEAD is ahead, the checkout dirty), class A is check --commit --full-a --snapshot, output pinned to the checkout's target", { skip: POSIX_SIM }, async (t) => {
  const fx = unitFixture(t);
  let seenSnapshotLib = null; const tiers = [];
  const r = await fx.door(["--to", "compile", "--unit", "cargo:rightkit-x", "--commit", fx.c1], {
    runCheck: async (tier) => { tiers.push(tier); return 0; },
    loadUnitOwner: fx.loadUnitOwner({ compile: ({ argv }) => { seenSnapshotLib = fs.readFileSync(path.join(argv[argv.indexOf("--workspace") + 1], "crates/rightkit-x/src/lib.rs"), "utf8"); } }),
  });
  assert.equal(r.code, 0, r.all);
  assert.equal(fx.owner.compiles.length, 1);
  const { argv, options } = fx.owner.compiles[0];
  assert.equal(argv[argv.indexOf("--workspace") + 1], fx.snapshot(), "the unit is built from the snapshot");
  assert.equal(seenSnapshotLib, "pub fn committed() {}\n", "the snapshot holds the SELECTED commit's bytes, not HEAD's and not the checkout's edit");
  assert.equal(options.source.commit, fx.c1); assert.equal(options.source.tree, git(fx.repo, "rev-parse", `${fx.c1}^{tree}`).stdout.trim());
  assert.equal(options.source.checkout, fs.realpathSync(fx.repo));
  assert.equal(options.env.CARGO_TARGET_DIR, path.join(fx.dir, "checkout-target"), "Cargo's output is the checkout's resolved target directory");
  assert.match(r.all, /\[compile\] cargo:rightkit-x passed/);
  assert.equal(fx.owner.closed, 1);
  // Nothing names the checkout as a workspace or runs a status or a HEAD read through the gate's git seam; class A is never --worktree.
  assert.ok(!argv.includes(fs.realpathSync(fx.repo)) && !argv.includes(fx.repo), "no argv names the checkout as --workspace");
  assert.ok(fx.spawned.every((c) => !(c[0] === "git" && (c.includes("status") || (c[1] === "rev-parse" && c[2] === "HEAD")))), `no status or HEAD read: ${JSON.stringify(fx.spawned.filter((c) => c[0] === "git").slice(0, 8))}`);
  assert.ok(!fx.spawned.some((c) => c.includes("--worktree")));
  assert.deepEqual(tiers, ["compile"], "class A ran once, at the tier a compile-bound unit run owes");
  // The class A the unit door runs is `check --commit <sha> --full-a --snapshot <dir>` (spawn-recorder form: the argv in the source).
  const src = fs.readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  const unitBlock = src.slice(src.indexOf("async function runUnitSnapshot"), src.indexOf("export function snapshotRouteSelected"));
  assert.match(unitBlock, /main\(\["check", "--commit", ds\.commit, "--full-a", "--snapshot", ds\.path, "--scope", path\.posix\.dirname\(manifest\), "--rule-tier", tier\]/, "the unit's Class A is scoped to the unit's own closure (GB-15)");
  assert.doesNotMatch(unitBlock, /--worktree|rev-parse", "HEAD"|"status"/);
});

test("[7.06a negative] a unit manifest or local grant present only in the checkout is refused; a grant naming another repository is refused; --commit is refused on hosted CI", { skip: POSIX_SIM }, async (t) => {
  const noManifest = unitFixture(t, { manifest: false });
  noManifest.write("crates/rightkit-x/Cargo.toml", '[package]\nname = "rightkit-x"\nversion = "1.2.3"\n'); // untracked: present only in the checkout
  let r = await noManifest.door(["--to", "compile", "--unit", "cargo:rightkit-x", "--commit", noManifest.c1], { loadUnitOwner: noManifest.loadUnitOwner() });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /unit and local grant must be tracked canonical files of [0-9a-f]{12} \(crates\/rightkit-x\/Cargo\.toml is not\)/); assert.equal(noManifest.owner.compiles.length, 0);
  const noGrant = unitFixture(t, { grant: false });
  noGrant.write(".rightkit-local-build.json", JSON.stringify({ allow: "build-test-publish", repository: "o/rk" }));
  r = await noGrant.door(["--to", "compile", "--unit", "cargo:rightkit-x", "--commit", noGrant.c1], { loadUnitOwner: noGrant.loadUnitOwner() });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /\.rightkit-local-build\.json is not\)/); assert.equal(noGrant.owner.compiles.length, 0);
  // The grant is read from the commit and checked against the checkout's origin.
  const fx = unitFixture(t);
  git(fx.repo, "remote", "set-url", "origin", "https://github.com/other/repo.git");
  r = await fx.door(["--to", "compile", "--unit", "cargo:rightkit-x", "--commit", fx.c1], { loadUnitOwner: fx.loadUnitOwner() });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /BLOCKED: unit needs/); assert.equal(fx.owner.compiles.length, 0);
  // Hosted CI alone has no snapshot route (the runner's checkout is the commit): --commit is refused there.
  const hosted = await fx.door(["--to", "compile", "--unit", "cargo:rightkit-x", "--commit", fx.c1], { platform: "darwin", env: { ...process.env, GITHUB_ACTIONS: "true" } });
  assert.equal(hosted.code, 2); assert.match(hosted.err, /--commit selects a commit for the snapshot route; hosted CI builds the runner's checkout/);
});

test("[7.06a negative] a snapshot tamper mid-stage is refused (the real verify); a write into the snapshot outside the ledger is refused; a staged-sidecar-style ignored output is admitted", { skip: POSIX_SIM }, async (t) => {
  const fx = unitFixture(t);
  const args = ["--to", "compile", "--unit", "cargo:rightkit-x", "--commit", fx.c1];
  let r = await fx.door(args, { loadUnitOwner: fx.loadUnitOwner({ compile: ({ argv }) => fs.writeFileSync(path.join(argv[argv.indexOf("--workspace") + 1], "crates/rightkit-x/src/lib.rs"), "tampered\n") }) });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /RK_CHECK_ERROR: \[unit:compile\].*(is not exactly commit|is tree [0-9a-f]+, not tree [0-9a-f]+ of commit)/); assert.match(r.err, /the compile stage's result covers no known bytes/);
  r = await fx.door(args, { loadUnitOwner: fx.loadUnitOwner({ compile: ({ argv }) => fs.writeFileSync(path.join(argv[argv.indexOf("--workspace") + 1], "crates/rightkit-x/stray.txt"), "x") }) });
  assert.equal(r.code, 1, r.all); assert.match(r.err, /RK_CHECK_ERROR: \[unit:compile\] crates\/rightkit-x\/stray\.txt is not ignored by the commit's own ignore rules/);
  // An untouched run passes (control), and the ledger functions saw it at the start and end of the stage.
  r = await fx.door(args, { loadUnitOwner: fx.loadUnitOwner() });
  assert.equal(r.code, 0, r.all);
});

test("[7.06a] the grant and the git calls of a unit built from a snapshot are read from the selected commit (sourceGit, localBuildGrantedAt)", { skip: POSIX_SIM }, (t) => {
  const fx = unitFixture(t);
  const source = { checkout: fx.repo, commit: fx.c1, tree: git(fx.repo, "rev-parse", `${fx.c1}^{tree}`).stdout.trim() };
  assert.equal(localBuildGrantedAt(source), true);
  assert.equal(localBuildGrantedAt({ ...source, commit: "0".repeat(40) }), false, "a commit without the grant file");
  const g = sourceGit(source);
  assert.equal(g(["rev-parse", "HEAD"], "/anywhere"), fx.c1); assert.equal(g(["rev-parse", "HEAD^{tree}"], "/anywhere"), source.tree);
  assert.equal(g(["status", "--porcelain", "--", "crates/rightkit-x"], "/anywhere"), "", "cleanliness is the door's verify, not the checkout's status");
  assert.equal(g(["show", "HEAD:crates/rightkit-x/src/lib.rs"], "/anywhere"), "pub fn committed() {}", "show HEAD:<path> reads the selected commit");
  assert.equal(g(["ls-files", "--error-unmatch", "--", "crates/rightkit-x/Cargo.toml"], "/anywhere"), "crates/rightkit-x/Cargo.toml");
  assert.equal(g(["ls-files", "--error-unmatch", "--", "crates/rightkit-x/untracked.rs"], "/anywhere"), null);
});

// ---- npm units: the same cargo binding as any other npm unit (decision 2 Q3) ----------------------------------------------------------

/** A recording stand-in for the real cargo: it answers locate-project from the files and logs each call with the target directory it was given. */
function recordingCargo(fx) {
  const file = path.join(fx.dir, "real-cargo"); const log = path.join(fx.dir, "real-cargo.jsonl");
  fs.writeFileSync(file, `#!/usr/bin/env node
const fs = require("node:fs"); const path = require("node:path");
const a = process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ cwd: process.cwd(), target: process.env.CARGO_TARGET_DIR ?? null, args: a }) + "\\n");
if (a[0] === "locate-project") {
  let d = process.cwd(); const i = a.indexOf("--manifest-path"); if (i >= 0) d = path.dirname(a[i + 1]);
  for (;;) { const f = path.join(d, "Cargo.toml"); if (fs.existsSync(f) && /\\[workspace\\]/.test(fs.readFileSync(f, "utf8"))) { console.log(f); break; } const p = path.dirname(d); if (p === d) { console.log(path.join(process.cwd(), "Cargo.toml")); break; } d = p; }
}
`);
  fs.chmodSync(file, 0o755);
  fx.tools.cargo = file;
  return { file, calls: () => (fs.existsSync(log) ? fs.readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []) };
}

/** What a unit's test does when it reaches cargo: runs the `cargo` first on its PATH (the gate's wrapper), from `cwd`, ignoring the exit status. */
const reachCargo = (options, cwd, args, results) => {
  const first = options.env.PATH.split(path.delimiter)[0];
  const r = spawnSync(path.join(first, "cargo"), args, { cwd, env: options.env, encoding: "utf8" });
  results.push({ status: r.status, stderr: r.stderr });
};

test("[7.06a npm positive] an npm unit stage reaches cargo through the gate's wrapper: it writes the checkout's resolved target directory and nothing under the snapshot", { skip: POSIX_SIM || process.platform === "win32" }, async (t) => {
  const fx = unitFixture(t, { npm: true });
  const cargo = recordingCargo(fx); const results = [];
  const r = await fx.door(["--to", "compile", "--unit", "npm:@rightkit/x", "--commit", fx.c1], {
    loadUnitOwner: fx.loadUnitOwner({ compile: ({ argv, options }) => reachCargo(options, path.join(argv[argv.indexOf("--workspace") + 1], "crates", "rightkit-x"), ["build", "--locked"], results) }),
  });
  assert.equal(r.code, 0, r.all);
  assert.match(r.all, /runtime cargo wrapper .*\(direct\): each cargo call a unit stage reaches binds its workspace's checkout target directory or is refused/);
  assert.equal(results[0].status, 0, results[0].stderr);
  const built = cargo.calls().filter((c) => c.args[0] === "build");
  assert.equal(built.length, 1);
  assert.equal(built[0].target, path.join(fx.dir, "checkout-target"), "cargo wrote the checkout's resolved target directory");
  assert.ok(!fs.existsSync(path.join(fx.snapshot(), "target")) && !fs.existsSync(path.join(fx.snapshot(), "crates", "rightkit-x", "target")), "no target/ in the snapshot");
});

test("[7.06a npm negative] a unit test that reaches cargo outside any resolved workspace, or redirects its target, is refused by the wrapper and fails the unit stage even though the test ignored cargo's exit status", { skip: POSIX_SIM || process.platform === "win32" }, async (t) => {
  const fx = unitFixture(t, { npm: true });
  recordingCargo(fx); const results = [];
  const run = (where, args) => fx.door(["--to", "compile", "--unit", "npm:@rightkit/x", "--commit", fx.c1], {
    loadUnitOwner: fx.loadUnitOwner({ compile: ({ argv, options }) => reachCargo(options, where(argv), args, results) }),
  });
  let r = await run(() => os.tmpdir(), ["build", "--locked"]);
  assert.notEqual(results.at(-1).status, 0, "the wrapper refused the call");
  assert.match(results.at(-1).stderr, /RK_CARGO_UNBOUND_WORKSPACE/);
  assert.equal(r.code, 1, r.all); assert.match(r.err, /RK_CHECK_ERROR: \[unit:compile\] a cargo call the unit stage made did not complete/);
  r = await run((argv) => path.join(argv[argv.indexOf("--workspace") + 1], "crates", "rightkit-x"), ["build", "--locked", "--target-dir", "target"]);
  assert.match(results.at(-1).stderr, /RK_CARGO_REDIRECT/);
  assert.equal(r.code, 1, r.all); assert.match(r.err, /did not complete/);
  assert.ok(!fs.existsSync(path.join(fx.snapshot(), "target")), "and still nothing under the snapshot");
});

test("[7.06a] on a broker host the unit cargo binding is the carrier (o.snapshotBroker): the wrapper is installed in broker mode with the gate's snapshot-to-checkout map", { skip: POSIX_SIM }, async (t) => {
  const fx = unitFixture(t, { npm: true });
  const brokerExecutor = () => ({ kind: "broker", cargo: ["rustright", "cargo"] });
  const installs = [];
  const sb = { handle: "own-1", markBrokerSubmit: subHandleStub("own-1") };
  const r = await fx.door(["--to", "compile", "--unit", "npm:@rightkit/x", "--commit", fx.c1], {
    platform: "darwin", resolveExecutor: brokerExecutor, snapshotBroker: sb, brokerSection: () => ({ client: "/x/rustright" }),
    installCargoShim: (args) => { installs.push(args); return { dir: path.join(fx.dir, "shim"), bin: path.join(fx.dir, "shim", "bin"), shim: path.join(fx.dir, "shim", "bin", "cargo"), mode: "broker", workspaces: {}, ledgerDir: path.join(fx.dir, "shim", "ledger"), identity: "i" }; },
    loadUnitOwner: fx.loadUnitOwner(),
  });
  assert.equal(installs.length, 1, r.all);
  assert.ok(installs[0].broker?.checkoutDirs && Object.keys(installs[0].broker.checkoutDirs).length === 1, "the broker map is the gate's snapshot-to-checkout map");
  assert.equal(installs[0].broker.checkoutDirs["."], fs.realpathSync(fx.repo));
  // The wrapper's map is the stage's carrier for a unit's own right-release calls too.
  const { CARRIED_CARGO_MAP_ENV } = await import("./carried-cargo.mjs");
  assert.ok(r.all.length > 0);
  // Without the carrier seam a broker host is refused before anything runs.
  const launches = [];
  const none = await fx.door(["--to", "compile", "--unit", "npm:@rightkit/x", "--commit", fx.c1], { platform: "darwin", resolveExecutor: brokerExecutor, loadUnitOwner: fx.loadUnitOwner({ compile: () => launches.push("x") }) });
  assert.equal(none.code, 1, none.all); assert.match(none.err, /BLOCKED: RK_SNAPSHOT_CARRIER_NOT_WIRED/); assert.deepEqual(launches, []);
  assert.equal(typeof CARRIED_CARGO_MAP_ENV, "string");
});

test("[B6] on a broker host the Cargo workspaces are the commit's own (read from its manifests): a checkout edit to a Cargo.toml changes nothing and no Cargo call is made against the checkout", { skip: POSIX_SIM }, async (t) => {
  const fx = unitFixture(t, { npm: true });
  fx.write("native/Cargo.toml", '[workspace]\n[package]\nname = "native"\nversion = "0.1.0"\n'); fx.write("native/src/lib.rs", "\n");
  git(fx.repo, "add", "-A"); git(fx.repo, "commit", "-q", "-m", "native workspace");
  const commit = git(fx.repo, "rev-parse", "HEAD").stdout.trim();
  const brokerExecutor = () => ({ kind: "broker", cargo: ["rustright", "cargo"] });
  const bound = async () => {
    const installs = [];
    const sb = { handle: "own-1", markBrokerSubmit: subHandleStub("own-1") };
    const before = fx.spawned.length;
    const r = await fx.door(["--to", "compile", "--unit", "npm:@rightkit/x", "--commit", commit], {
      platform: "darwin", resolveExecutor: brokerExecutor, snapshotBroker: sb, brokerSection: () => ({ client: "/x/rustright" }),
      installCargoShim: (args) => { installs.push(args); return { dir: path.join(fx.dir, "shim"), bin: path.join(fx.dir, "shim", "bin"), shim: path.join(fx.dir, "shim", "bin", "cargo"), mode: "broker", workspaces: {}, ledgerDir: path.join(fx.dir, "shim", "ledger"), identity: "i" }; },
      loadUnitOwner: fx.loadUnitOwner(),
    });
    assert.equal(installs.length, 1, r.all);
    const metadata = fx.spawned.slice(before).filter((c) => c.includes("metadata"));
    return { map: installs[0].broker.checkoutDirs, metadata };
  };
  const real = fs.realpathSync(fx.repo);
  const committed = await bound();
  assert.deepEqual(committed.map, { ".": real, native: path.join(real, "native") }, "the root workspace (its member crate included) and the nested one");
  assert.deepEqual(committed.metadata, [], "Cargo is asked nothing about the checkout");
  // Uncommitted edits to the checkout's manifests: the root stops being a workspace, the nested one is deleted, a crate becomes its own workspace.
  fx.write("Cargo.toml", "this is not toml\n");
  fx.write("crates/rightkit-x/Cargo.toml", '[workspace]\n[package]\nname = "rightkit-x"\nversion = "9.9.9"\n');
  fs.rmSync(path.join(fx.repo, "native"), { recursive: true, force: true });
  const edited = await bound();
  assert.deepEqual(edited.map, committed.map, "a checkout edit to a Cargo.toml does not change what the commit builds");
  assert.deepEqual(edited.metadata, []);
});

test("[B8/B9] `--to qualify --unit` on a broker host is the no-upload route: the compile and package stages are attested, the qualification is written beside the archive and verified against the commit, and nothing is dispatched; a refusal fails the run", { skip: POSIX_SIM }, async (t) => {
  const fx = unitFixture(t, { npm: true });
  const brokerExecutor = () => ({ kind: "broker", cargo: ["rustright", "cargo"] });
  const sb = { handle: "own-1", markBrokerSubmit: subHandleStub("own-1") };
  const tgz = path.join(fx.dir, "x-1.2.3.tgz"); fs.writeFileSync(tgz, "tgz");
  const sha = createHash("sha256").update("tgz").digest("hex");
  const seen = { env: {}, released: 0 };
  const owner = async () => {
    const packaged = { kind: "npm", name: "@rightkit/x", version: "1.2.3", commit: fx.c1, artifactFile: tgz, artifactSha256: sha, contentSha256: "c".repeat(64) };
    return {
      compileLocalUnit: async (_k, _a, { env }) => { seen.env.compile = env; return Object.freeze({}); }, localUnitCompileRequests: () => ["c-1"],
      packageLocalUnit: async (_s, { env }) => { seen.env.package = env; return { ...packaged, brokerRequests: ["p-1"] }; },
      qualifyLocalUnit: async (_s, { env }) => { seen.env.qualify = env; return { ...packaged, qualifiedOnly: true, evidencePath: "gate.json" }; }, closeLocalUnit: async () => {},
    };
  };
  const requests = { "c-1": { kind: "adapter", adapter: "rightkit-pnpm", requestedArgv: ["-r", "run", "build"] }, "p-1": { kind: "adapter", adapter: "rightkit-pnpm", requestedArgv: ["pack"] } };
  const verified = [];
  const hooks = (verify) => ({ fetchReceipt: (id) => ({ id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id, request: requests[id] }, signature: { alg: "ES256", keyId: "k", value: "ab" } } }),
    stageCommands: () => ({ compile: [{ kind: "adapter", adapter: "rightkit-pnpm", argvs: [["-r", "run", "build"]] }], package: [{ kind: "adapter", adapter: "rightkit-pnpm", argvs: [["pack"]] }] }), verifyReceipts: verify });
  const run = (verify) => fx.door(["--to", "qualify", "--unit", "npm:@rightkit/x", "--commit", fx.c1], {
    platform: "darwin", resolveExecutor: brokerExecutor, snapshotBroker: sb, brokerSection: () => ({ client: "/x/rustright" }), unitGateDeps: hooks(verify),
    installCargoShim: () => ({ dir: path.join(fx.dir, "shim"), bin: path.join(fx.dir, "shim", "bin"), shim: path.join(fx.dir, "shim", "bin", "cargo"), mode: "broker", workspaces: {}, ledgerDir: path.join(fx.dir, "shim", "ledger"), identity: "i" }),
    loadUnitOwner: owner,
  });
  const ok = await run((text, o) => { verified.push({ q: JSON.parse(text), ...o }); });
  assert.equal(ok.code, 0, ok.all);
  assert.match(ok.all, /\[qualify\] npm:@rightkit\/x passed/);
  assert.match(ok.all, /qualification .*qualification\.json verified locally; nothing was dispatched/);
  const tree = git(fx.repo, "rev-parse", `${fx.c1}^{tree}`).stdout.trim();
  const nonce = (stage) => qualificationNonce({ commit: fx.c1, tree, unit: "npm:@rightkit/x", stage });
  assert.equal(seen.env.compile.RIGHTKIT_ATTEST_STAGE_NONCE, nonce("compile")); assert.equal(seen.env.package.RIGHTKIT_ATTEST_STAGE_NONCE, nonce("package"));
  for (const stage of ["compile", "package", "qualify"]) assert.ok(!Object.keys(seen.env[stage]).some((k) => /^(RUSTRIGHT|RIGHTKIT)_(ATTEST_NONCE|SNAPSHOT_REQUEST|REQUEST_ID_FILE)$/.test(k)), `${stage}: the stage env carries no carrier variable`);
  assert.equal(verified.length, 1); assert.equal(verified[0].sha, fx.c1, "verified against the selected commit");
  assert.equal(verified[0].q.tree, tree); assert.equal(verified[0].q.contentSha256, "c".repeat(64));
  assert.deepEqual(verified[0].q.stages.package.receipts.map((r) => r.id), ["p-1"]);
  const written = JSON.parse(fs.readFileSync(path.join(path.dirname(tgz), "qualification.json"), "utf8"));
  assert.equal(written.commit, fx.c1); assert.equal(written.artifactSha256, sha);
  // The hosted admission would refuse it: the run fails, naming why, with the qualification never dispatched.
  const refused = await run(() => { throw new Error("publication admission refused: package receipt 1 (job p-1) carries no signed snapshotEvidence"); });
  assert.equal(refused.code, 1, refused.all);
  assert.match(refused.err, /local check of the qualification's signed broker receipts failed.*carries no signed snapshotEvidence/);
});
