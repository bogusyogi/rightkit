// rightkit#1311: on a broker host the gate's Cargo reads inside the managed snapshot (unit discovery, workspace members) are carried read-only
// requests with a verified signed receipt, never root requests (RustRight refuses those: "a root request cannot run inside a managed snapshot").
// The fake broker (test-support/fake-broker.mjs) refuses a root request inside the snapshots directory the way the real one does and logs every
// uncarried request (`rootRequests()`).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { discoverRustWorkspaces, main } from "./bin/rightkit-gate.mjs";
import { carriedReadOnlyScan, carriedScanRefusal, snapshotCargoSpawn } from "./snapshot-metadata.mjs";
import { runScans } from "./deterministic-scans.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { makeFakeBroker, subHandleStub } from "./test-support/fake-broker.mjs";

const emptyConfig = path.join(fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-cfg-"))), "gitconfig");
fs.writeFileSync(emptyConfig, "");
Object.assign(process.env, { GIT_CONFIG_GLOBAL: emptyConfig, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" });

const CARGO = ["rustright", "cargo"];
const GATE = "/gate/bin/rightkit-gate.mjs";
const tracked = (name) => ["Cargo.toml", "crates/a/Cargo.toml", "native/Cargo.toml"].filter((f) => f === name || f.endsWith(`/${name}`));
const root = (cmd, args, opts) => spawnSync(cmd, args, { encoding: "utf8", ...opts });

function git(cwd, ...args) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
}

test("the shapes the broker admits as a carried read-only request: metadata, fetch --locked, deny, audit --db --no-fetch; nothing else", () => {
  const ok = [
    ["metadata", "--no-deps", "--format-version", "1", "--locked"], ["metadata", "--format-version", "1", "--no-deps"], ["metadata", "--frozen"], ["metadata", "--locked", "--offline"],
    ["metadata", "--format-version", "1", "--locked"], // full metadata, online (cargo-deny's aux metadata)
    ["fetch", "--locked"],
    ["deny", "--frozen", "--metadata-path", "/abs/m.json", "--config", "/abs/deny.toml", "check"], ["deny", "check"], ["deny", "--config", "/abs/deny.toml", "check"],
    ["audit", "--db", "/abs/db", "--no-fetch"], ["audit", "--no-fetch", "--db", "/abs/db"],
    ["fmt", "--all", "--check"], ["fmt", "-p", "a", "--check"], // rustright command_policy.rs fmt_check_shape
    ["package", "-p", "a", "--list", "--locked"], ["package", "-p", "a", "--list", "--locked", "--allow-dirty"], ["package", "-p", "a", "--list", "--allow-dirty", "--locked"], // rustright command_policy.rs:407-409
  ];
  for (const argv of ok) assert.equal(carriedReadOnlyScan(argv), true, `${argv.join(" ")}: ${carriedScanRefusal(argv)}`);
  const no = [
    ["metadata", "--offline"], ["metadata"], ["metadata", "--format-version", "2", "--locked"], ["metadata", "--locked", "--locked"],
    ["metadata", "--no-deps", "--config", "x=y"], ["metadata", "--no-deps", "-Zfoo"], ["metadata", "--", "--no-deps"], ["metadata", "--no-deps", "--manifest-path", "/x/Cargo.toml"], ["+nightly", "metadata", "--no-deps"],
    ["fetch"], ["fetch", "--locked", "--offline"], ["fetch", "--locked", "--config", "x=y"], ["fetch", "--frozen"],
    ["deny", "check", "--hide-inclusion-graph"], ["deny", "--frozen", "--frozen", "check"], ["deny", "--config=/abs/deny.toml", "check"], ["deny", "--metadata-path", "m.json", "check"], ["deny", "--frozen"],
    ["deny", "--offline", "check"], ["deny", "--config", "/abs/deny.toml", "--", "check"], ["deny", "--workspace", "check"], ["deny", "-Z", "x", "check"],
    ["fmt"], ["fmt", "--check"], ["fmt", "--all"], ["fmt", "-p", "a"], ["fmt", "-p", "--check"], ["fmt", "--all", "--check", "--", "--edition", "2021"], ["fmt", "--check", "--all"], ["fmt", "-p", "a", "--check", "--all"],
    ["audit"], ["audit", "--db", "/abs/db"], ["audit", "--no-fetch"], ["audit", "--db", "rel", "--no-fetch"], ["audit", "--db", "/abs/db", "--no-fetch", "--deny", "warnings"], ["audit", "--db=/abs/db", "--no-fetch"],
    ["package", "--list", "--locked"], ["package", "-p", "a", "--list"], ["package", "-p", "a", "--locked"], ["package", "-p", "a", "--list", "--locked", "--list"], ["package", "-p", "a", "--list", "--locked", "--no-verify"], ["package", "-p", "--list", "--locked"], ["package", "--workspace", "--list", "--locked"], ["package", "-p", "a", "--list", "--locked", "--config", "x=y"],
    ["build", "--locked"], ["check"], ["--version"], ["tree", "--locked"], ["about", "generate", "--frozen"],
  ];
  for (const argv of no) assert.equal(carriedReadOnlyScan(argv), false, argv.join(" "));
});

test("the files a carried deny reads and the database a carried audit reads must exist, be of the right kind and lie outside the live checkout", (t) => {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-shape-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const checkout = path.join(base, "checkout"); const gateOwned = path.join(base, "gate");
  fs.mkdirSync(path.join(checkout, "db"), { recursive: true }); fs.mkdirSync(path.join(gateOwned, "db"), { recursive: true });
  fs.writeFileSync(path.join(checkout, "deny.toml"), ""); fs.writeFileSync(path.join(gateOwned, "deny.toml"), ""); fs.writeFileSync(path.join(gateOwned, "m.json"), "{}");
  fs.symlinkSync(path.join(checkout, "deny.toml"), path.join(gateOwned, "link.toml"));
  const refusal = (argv) => carriedScanRefusal(argv, { checkout });
  assert.equal(refusal(["deny", "--frozen", "--metadata-path", path.join(gateOwned, "m.json"), "--config", path.join(gateOwned, "deny.toml"), "check"]), null);
  assert.equal(refusal(["audit", "--db", path.join(gateOwned, "db"), "--no-fetch"]), null);
  assert.match(refusal(["deny", "--config", path.join(checkout, "deny.toml"), "check"]), /inside the live checkout/, "the checkout's own deny.toml is never named");
  assert.match(refusal(["deny", "--config", path.join(gateOwned, "link.toml"), "check"]), /inside the live checkout/, "a link into the checkout is the checkout");
  assert.match(refusal(["audit", "--db", path.join(checkout, "db"), "--no-fetch"]), /inside the live checkout/);
  assert.match(refusal(["deny", "--config", path.join(gateOwned, "absent.toml"), "check"]), /does not exist/);
  assert.match(refusal(["deny", "--config", path.join(gateOwned, "db"), "check"]), /not a file/);
  assert.match(refusal(["audit", "--db", path.join(gateOwned, "m.json"), "--no-fetch"]), /not a directory/);
});

test("[1311] discovery inside the snapshot is carried: units are found, no root request reaches the broker, each call has a verified receipt", (t) => {
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  const records = [];
  const spawn = snapshotCargoSpawn({ spawn: root, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => fake.sb, env: { ...process.env, RUSTRIGHT_CALLING_GATE: GATE }, records });
  // The fake's own rule, as the real broker's: a root request inside the snapshot is refused.
  const plain = root(fake.client, ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], { cwd: fake.snapshot, env: process.env });
  assert.equal(plain.status, 2); assert.match(plain.stderr, /a root request cannot run inside a managed snapshot/);
  fake.setBehavior({});
  const before = fake.rootRequests().length;
  const found = discoverRustWorkspaces({ root: fake.snapshot, spawn, cargoCommand: CARGO, tracked, readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.ok(found.workspaces.some((w) => w.cargoDir === "."), JSON.stringify(found.workspaces.map((w) => w.cargoDir)));
  assert.ok(found.units.some((u) => u.crate === "a"), "the member crate is discovered from the carried metadata");
  assert.equal(fake.rootRequests().length, before, "the discovery made no uncarried request");
  assert.ok(fake.calls().length >= 1 && fake.calls().every((c) => c.argv[0] === "metadata" && c.argv.includes("--no-deps")));
  assert.ok(fake.calls().every((c) => c.cwd === fs.realpathSync(fake.checkout) || c.cwd.startsWith(`${fs.realpathSync(fake.checkout)}${path.sep}`)), "each call ran in the primary checkout's counterpart of the snapshot directory");
  // Each call is recorded pending, then verified (the receipt the gate checked against its pins, with the broker's request id).
  const settled = records.filter((r) => r.status !== "pending");
  assert.ok(settled.length === fake.calls().length && settled.every((r) => r.status === "verified" && typeof r.requestId === "string"), JSON.stringify(records.map((r) => r.status)));
  assert.ok(fake.submits.length >= 1, "the ownership recorded the broker submit");
});

test("[1311] a carried request without a valid receipt, or a call the broker would not admit, fails discovery and never becomes a root request", (t) => {
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  const errors = [];
  const spawn = snapshotCargoSpawn({ spawn: root, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => fake.sb, env: { ...process.env, RUSTRIGHT_CALLING_GATE: GATE }, err: (m) => errors.push(m) });
  const discover = () => discoverRustWorkspaces({ root: fake.snapshot, spawn, cargoCommand: CARGO, tracked, readFile: (f) => fs.readFileSync(f, "utf8") });
  for (const tamper of ["signedByOtherKey", "noProvenance", "treeAtExit", "otherHandle", "otherNonce", "requestOtherArgv"]) {
    fake.setBehavior({ tamper });
    assert.throws(discover, /cargo metadata failed/, tamper);
  }
  assert.ok(errors.some((m) => /RK_CARGO_BROKER_UNVERIFIED/.test(m)));
  fake.setBehavior({ refuse: true });
  assert.throws(discover, /cargo metadata failed/);
  fake.setBehavior({});
  // Not admitted by the broker inside a snapshot (an unlocked fetch or metadata, a package list, a deny or audit of another shape, anything with
  // cargo's own --config or -Z): refused here, before anything is sent.
  const calls = fake.calls().length;
  for (const argv of [["fetch"], ["deny", "check", "--hide-inclusion-graph"], ["metadata"], ["audit"], ["package", "-p", "a", "--list", "--no-verify", "--locked"], ["build", "--locked"], ["metadata", "--no-deps", "--config", "x=y"], ["deny", "--config", "deny.toml", "check"]]) {
    const r = spawn("rustright", [...CARGO.slice(1), ...argv], { cwd: fake.snapshot });
    assert.notEqual(r.status, 0); assert.match(r.stderr, /RK_SNAPSHOT_CARRIED_REFUSED/);
  }
  assert.equal(fake.calls().length, calls, "nothing was sent to the broker");
  assert.deepEqual(fake.rootRequests().filter((r) => r.refused), [], "and no root request was made inside the snapshot");
  // No gate named: the carrier is never forged.
  const noGate = snapshotCargoSpawn({ spawn: root, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => fake.sb, env: { ...process.env, RUSTRIGHT_CALLING_GATE: "" } });
  assert.notEqual(noGate("rustright", [...CARGO.slice(1), "metadata", "--no-deps"], { cwd: fake.snapshot }).status, 0);
});

test("calls outside the snapshot (the primary checkout, git) pass through unchanged", (t) => {
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  const seen = [];
  const spawn = snapshotCargoSpawn({ spawn: (c, a, o) => { seen.push([c, o.cwd]); return { status: 0, stdout: "x" }; }, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => { throw new Error("no broker needed"); }, env: {} });
  assert.equal(spawn("rustright", ["cargo", "metadata", "--no-deps"], { cwd: fake.checkout }).stdout, "x");
  assert.equal(spawn("git", ["status"], { cwd: fake.snapshot }).stdout, "x");
  assert.equal(seen.length, 2);
});

/** A Cargo repository with a committed workspace, a gate-owned snapshot of it, and a fixture broker that serves both. */
function scenario(t) {
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-repo-")));
  const snaps = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-snaps-")));
  t.after(() => { fs.rmSync(repo, { recursive: true, force: true }); fs.rmSync(snaps, { recursive: true, force: true }); });
  git(repo, "init", "-q", "-b", "main");
  const files = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "Cargo.lock": "version = 4\n", "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n", "README.md": "r\n" };
  for (const [f, body] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, f)), { recursive: true }); fs.writeFileSync(path.join(repo, f), body); }
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "A");
  const sha = git(repo, "rev-parse", "HEAD");
  const snap = syncSnapshot({ checkout: repo, rev: sha, root: snaps });
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  fake.rebase({ checkout: repo, snapshotsRoot: path.dirname(snap.path) });
  const sb = { ...fake.sb, checkout: repo, markBrokerSubmit: subHandleStub(fake.sb.handle), get snapshot() { return { path: snap.path, expectedTree: fake.sb.snapshot.expectedTree }; } };
  return { repo, sha, snap, fake, sb };
}

function gateIo(t, s, { runScans }) {
  const out = []; const err = [];
  const gatePackage = fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-gatepkg-"));
  t.after(() => fs.rmSync(gatePackage, { recursive: true, force: true }));
  fs.writeFileSync(path.join(gatePackage, "rightkit-gate.stub.mjs"), "// stand-in gate package\n");
  const env = { ...process.env, RK_GATE_CACHE: "0", RUSTRIGHT_CALLING_GATE: GATE, PATH: `${s.fake.root}${path.delimiter}${process.env.PATH}` };
  return {
    out, err,
    io: {
      cwd: s.repo, gatePackage, env, snapshotRoot: path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-own-")), "snapshots"), out: (m) => out.push(m), err: (m) => err.push(m),
      appCheck: async () => 0, selfPath: path.join(gatePackage, "rightkit-gate.mjs"), platform: "darwin",
      resolveExecutor: () => ({ kind: "broker", cargo: CARGO }), snapshotBroker: s.sb, runScans,
      // `rustright` is the fixture broker's client (the guard stub would answer a checkout metadata call on its own).
      spawn: (cmd, args, opts = {}) => spawnSync(cmd === "rustright" ? s.fake.client : cmd, args, { encoding: "utf8", ...opts, env: opts.env ?? env }),
    },
  };
}

test("[1311] the check stage of the snapshot route (class A of the snapshot) makes zero root requests inside the snapshot: discovery is carried", async (t) => {
  const s = scenario(t);
  const plans = [];
  const run = gateIo(t, s, { runScans: async (plan) => { plans.push(plan); return { schema: "rightkit.deterministic-scans/1", ok: true, complete: true, classes: ["A"], scans: [], toolVersions: {}, cacheHits: [], notes: [], deferred: [] }; } });
  const code = await main(["check", "--commit", s.sha, "--full-a", "--snapshot", s.snap.path], run.io);
  assert.equal(code, 0, run.err.join("\n"));
  assert.ok(plans.length >= 1, "the scans were planned from the discovered units");
  assert.deepEqual(s.fake.rootRequests().filter((r) => r.refused), [], "no root request inside the snapshot");
  assert.ok(s.fake.calls().length >= 1, "discovery was a carried request");
  assert.ok(s.fake.calls().every((c) => c.argv[0] === "metadata" && c.cwd.startsWith(s.repo)), "carried, read-only, from the primary checkout's counterpart");
  // A carried request whose receipt does not verify blocks the stage with the discovery message, not a root fallback.
  s.fake.setBehavior({ tamper: "signedByOtherKey" });
  const bad = gateIo(t, s, { runScans: async () => ({ ok: true, scans: [] }) });
  assert.notEqual(await main(["check", "--commit", s.sha, "--full-a", "--snapshot", s.snap.path], bad.io), 0);
  assert.match(bad.err.join("\n"), /cannot discover the repository's units: cargo metadata failed/);
  assert.deepEqual(s.fake.rootRequests().filter((r) => r.refused), []);
});

// ---- plain `check --commit` and the class A cargo scans of a snapshot (docs/GATE-CONTRACT.md: no export or copy; no dirty-tree condition) ----

const ADMITTED_SCAN_SHAPES = [
  (a) => a.length === 5 && a[0] === "metadata" && a[1] === "--no-deps" && a[2] === "--format-version" && a[3] === "1" && a[4] === "--locked",
  (a) => a.length === 4 && a[0] === "metadata" && a[1] === "--format-version" && a[2] === "1" && a[3] === "--locked",
  (a) => a.length === 2 && a[0] === "fetch" && a[1] === "--locked",
  (a) => a.length === 7 && a[0] === "deny" && a[1] === "--frozen" && a[2] === "--metadata-path" && path.isAbsolute(a[3]) && a[4] === "--config" && path.isAbsolute(a[5]) && a[6] === "check",
  (a) => a.length === 4 && a[0] === "audit" && a[1] === "--db" && path.isAbsolute(a[2]) && a[3] === "--no-fetch",
];

test("[plain check] `check --commit` on a broker host reads the gate's warm snapshot: no tmp export, every class A cargo scan is carried in the snapshot, and an uncommitted Cargo edit in the checkout is ignored", async (t) => {
  const s = scenario(t);
  const advisoryDb = fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-advdb-"));
  const gateTmp = fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-gatetmp-"));
  t.after(() => { fs.rmSync(advisoryDb, { recursive: true, force: true }); fs.rmSync(gateTmp, { recursive: true, force: true }); });
  // Uncommitted edits to every Cargo input of the checkout: none of them is the commit's bytes.
  const dirty = { "Cargo.toml": '[workspace]\nmembers = []\n# uncommitted\n', "Cargo.lock": "not a lockfile\n", "crates/a/Cargo.toml": "garbage = [\n" };
  for (const [f, body] of Object.entries(dirty)) fs.writeFileSync(path.join(s.repo, f), body);
  fs.writeFileSync(path.join(s.repo, "deny.toml"), "# untracked\n");
  const seen = { plan: null, spawns: [], tmpEntries: [], liveTmp: [] };
  const run = gateIo(t, s, { runScans: async (plan, opts) => {
    seen.plan = plan; seen.liveTmp = fs.readdirSync(os.tmpdir());
    seen.tmpEntries = [...fs.readdirSync(os.tmpdir()), ...fs.readdirSync(gateTmp)].filter((n) => n.startsWith("rk-gate-"));
    // The client's own version probe starts in the checkout (a workspace of the primary checkout); the guard refuses a live launch, so it is answered here.
    const spawn = (cmd, args, o) => { seen.spawns.push({ cmd, args, cwd: o?.cwd }); return args.includes("--version") ? { status: 0, stdout: "cargo 1.90.0 (fake)\n", stderr: "" } : opts.spawn(cmd, args, o); };
    return runScans(plan, { ...opts, spawn, env: run.io.env, jobs: 1, exclude: (x) => x.tool !== "rustright", ensureAdvisoryDb: () => advisoryDb });
  } });
  run.io.snapshotRoot = path.dirname(s.snap.path);
  run.io.env = { ...run.io.env, RK_GATE_TMPDIR: gateTmp };
  const tmpBefore = new Set(fs.readdirSync(os.tmpdir()));
  const code = await main(["check", "--commit", s.sha], run.io);
  assert.equal(code, 0, `${run.err.join("\n")}\n${run.out.join("\n")}`);
  assert.deepEqual(seen.tmpEntries, [], "no rk-gate-* export directory was made");
  assert.deepEqual(fs.readdirSync(os.tmpdir()).filter((n) => !tmpBefore.has(n) && n !== "build-ownership"), [], "no rk-check-* export and no other tree was left in the temporary directory (the ownership record is the gate's lock, not a tree)");
  assert.deepEqual(seen.liveTmp.filter((n) => /^rk-check-/.test(n)), [], "and none existed while the app rules and class A ran");
  assert.ok(run.out.some((l) => /build ownership held/.test(l)) && run.out.some((l) => /snapshot .* is commit/.test(l)), "the snapshot was selected under host ownership");
  // The plan reads the snapshot: no scan is rooted in the checkout, and the checkout is only the client's starting directory.
  const snapDir = fs.realpathSync(s.snap.path);
  assert.equal(fs.realpathSync(seen.plan.roots.contentRoot), snapDir); assert.equal(fs.realpathSync(seen.plan.roots.liveRoot), snapDir);
  const cargoScans = seen.plan.scans.filter((x) => x.class === "A" && x.tool === "rustright");
  assert.ok(cargoScans.length >= 1 && cargoScans.every((x) => x.root === "live"), JSON.stringify(cargoScans.map((x) => [x.id, x.root])));
  // Every cargo call a scan made had its working directory in the snapshot, except the client's version probe (it reads no file).
  const cargoSpawns = seen.spawns.filter((x) => x.cmd === "rustright" && x.args[0] === "cargo" && !x.args.includes("--version"));
  assert.ok(cargoSpawns.length >= 3 && cargoSpawns.every((x) => fs.realpathSync(x.cwd).startsWith(snapDir)), JSON.stringify(cargoSpawns.map((x) => x.cwd)));
  assert.deepEqual(s.fake.rootRequests(), [], "zero root requests: nothing inside the snapshot, nothing in the checkout");
  // The argvs the broker received: exactly the admitted shapes, and the scans' shapes all appeared.
  const argvs = s.fake.calls().map((c) => c.argv);
  assert.ok(argvs.every((a) => ADMITTED_SCAN_SHAPES.some((ok) => ok(a))), JSON.stringify(argvs));
  for (const [i, name] of [[0, "metadata --no-deps"], [1, "full metadata"], [2, "fetch"], [3, "deny"], [4, "audit"]]) assert.ok(argvs.some((a) => ADMITTED_SCAN_SHAPES[i](a)), `${name} was carried: ${JSON.stringify(argvs)}`);
  const deny = argvs.find((a) => a[0] === "deny");
  for (const file of [deny[3], deny[5]]) assert.ok(file.startsWith(`${gateTmp}${path.sep}`), `${file} is in the gate-owned temporary directory, outside the checkout`);
  // The probe is the one call that starts in the checkout: it reads no file of it.
  const probes = seen.spawns.filter((x) => x.args.includes("--version"));
  assert.ok(probes.length >= 1 && probes.every((x) => fs.realpathSync(x.cwd).startsWith(fs.realpathSync(s.repo))));
  // The checkout's uncommitted bytes are untouched.
  for (const [f, body] of Object.entries(dirty)) assert.equal(fs.readFileSync(path.join(s.repo, f), "utf8"), body);
  // The release tier also plans the package listing; the broker admits it carried (rustright command_policy.rs:407-409), so it is sent
  // from the snapshot like the other read-only shapes, never as a root request in the checkout, and the stage passes.
  const before = s.fake.calls().length;
  const red = gateIo(t, s, { runScans: async (plan, opts) => runScans(plan, { ...opts, spawn: (cmd, args, o) => (args.includes("--version") ? { status: 0, stdout: "cargo 1.90.0 (fake)\n", stderr: "" } : opts.spawn(cmd, args, o)), env: red.io.env, jobs: 1, exclude: (x) => x.tool !== "rustright", ensureAdvisoryDb: () => advisoryDb }) });
  red.io.snapshotRoot = path.dirname(s.snap.path); red.io.env = { ...red.io.env, RK_GATE_TMPDIR: gateTmp };
  assert.equal(await main(["check", "--commit", s.sha, "--rule-tier", "release"], red.io), 0, `${red.err.join("\n")}`);
  assert.doesNotMatch(red.err.join("\n"), /RK_SNAPSHOT_CARRIED_REFUSED/);
  const packageCalls = s.fake.calls().slice(before).filter((c) => c.argv[0] === "package");
  assert.ok(packageCalls.length >= 1, "the package listing was carried to the broker");
  assert.ok(packageCalls.every((c) => carriedReadOnlyScan(c.argv) && c.carrierEnvNames.length > 0), "and sent as a carried request (a carrier set), in an admitted shape");
  assert.deepEqual(s.fake.rootRequests(), []);
});

test("[plain check] a direct host scans the snapshot in place with no tmp export, and the checkout's own files are never the scanned root", async (t) => {
  const s = scenario(t);
  const gateTmp = fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-gatetmp-"));
  t.after(() => fs.rmSync(gateTmp, { recursive: true, force: true }));
  let seen = null;
  const run = gateIo(t, s, { runScans: async (plan) => { seen = { plan, tmpAll: fs.readdirSync(os.tmpdir()), tmp: [...fs.readdirSync(os.tmpdir()), ...fs.readdirSync(gateTmp)].filter((n) => n.startsWith("rk-gate-")) }; return { schema: "rightkit.deterministic-scans/1", ok: true, complete: true, classes: ["A"], scans: [], toolVersions: {}, cacheHits: [], notes: [], deferred: [] }; } });
  Object.assign(run.io, { platform: "linux", resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), snapshotBroker: undefined, snapshotRoot: path.dirname(s.snap.path) });
  run.io.env = { ...run.io.env, RK_GATE_TMPDIR: gateTmp };
  fs.writeFileSync(path.join(s.repo, "README.md"), "uncommitted\n");
  const tmpBefore = new Set(fs.readdirSync(os.tmpdir()));
  assert.equal(await main(["check", "--commit", "HEAD"], { ...run.io, spawn: (cmd, args, opts = {}) => spawnSync(cmd === "cargo" && args[0] === "metadata" ? process.execPath : cmd, cmd === "cargo" && args[0] === "metadata" ? ["-e", "process.stdout.write(JSON.stringify({packages:[{name:'a',manifest_path:process.cwd()+'/crates/a/Cargo.toml',version:'0.1.0'}],workspace_root:process.cwd()}))"] : args, { encoding: "utf8", ...opts, env: opts.env ?? run.io.env }) }), 0, `${run.err.join("\n")}`);
  assert.deepEqual(seen.tmp, [], "no rk-gate-* export directory was made");
  assert.deepEqual(seen.tmpAll.filter((n) => /^rk-check-/.test(n)), [], "no rk-check-* export existed while the scans ran");
  assert.deepEqual(fs.readdirSync(os.tmpdir()).filter((n) => !tmpBefore.has(n) && n !== "build-ownership"), [], "no rk-check-* export and no other tree was left in the temporary directory (the ownership record is the gate's lock, not a tree)");
  const snapDir = fs.realpathSync(s.snap.path);
  assert.equal(fs.realpathSync(seen.plan.roots.contentRoot), snapDir, "the scans read the warm snapshot");
  assert.notEqual(fs.realpathSync(seen.plan.roots.contentRoot), fs.realpathSync(s.repo));
});
