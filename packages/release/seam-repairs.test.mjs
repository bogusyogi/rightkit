// Repairs of the gate's half of the seam found by the fresh reviews of f4c18bc (astra + its Oracle, 2026-11-09), each through its real caller:
//   1. a stage child's seam call is filed PENDING before it is sent (a child killed mid-call leaves it unsettled; a pending row that cannot be written submits nothing),
//      and the stage's lock-release count reads every ledger file and counts unreadable records as unknown ownership;
//   2. a row is settled only by the terminal receipt of ITS job (id / provenance.payload.jobId equal to the client's request id, with terminalAt), or by a coded
//      refusal before admission; a receipt that merely parses, or names another job, settles nothing;
//   3. a Swift build through the seam is the broker's `rightkit-swift` adapter job: `--package-path .`, never a gate scratch path (GB-67, 833f62ae0);
//   4. a seam adapter's receipt outputDir is filed with its Cargo workspace, so the stage's output readers (and the Tauri bundle lookup) find it.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { judgeSeamOutcome, seamSettled, receiptIsTerminalFor, classAVerdict, writeClassAVerdict, verdictFileName } from "./gate-seam.mjs";
import { verdictForCwd } from "./seam-broker.mjs";
import { runShim, SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { seamRunState } from "./deterministic-scans.mjs";
import { runStageAdapter, runStageCargo, installStageCarrier, carriedOutputDir } from "./stage-seam.mjs";
import { readLedgerDir, appendLedger } from "./cargo-ledger.mjs";
import { runScans } from "./deterministic-scans.mjs";

const commit = "a".repeat(40); const tree = "b".repeat(40);
const tmp = () => fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-repair-")));
// A receipt as the broker's finish writes it for a job whose tree was reaped and whose survivor census was clean (the default), or with `proc` overriding
// the process evidence: `reap`, `processOwnership`, `startedAt`, `descendants` (rustright finish_impl.rs, service.rs descendants_claim).
const reapedProc = { reap: { status: "REAPED", reaped: true, escalated: false }, processOwnership: { pid: 1 }, startedAt: "2026-11-09T00:00:00.000Z", descendants: { status: "tree-contained" } };
const receiptFor = (dir, { jobId = "q1", argv = ["run", "build"], workspace = "/snap/app", adapter = "rightkit-pnpm", gate = {}, terminalAt = "2026-11-09T00:00:00.000Z", id = jobId, proc = {} } = {}) => {
  const f = path.join(dir, `${Math.random().toString(16).slice(2)}.json`);
  const p = { ...reapedProc, ...proc };
  const top = { id, status: "SUCCEEDED", terminalAt, gate: { commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true, ...gate }, provenance: { payload: { jobId, terminalAt, startedAt: p.startedAt, descendants: p.descendants, request: { kind: adapter ? "adapter" : "cargo", adapter, requestedArgv: argv, workspace } } } };
  for (const k of ["reap", "processOwnership", "startedAt"]) if (p[k] !== undefined) top[k] = p[k];
  if (p.descendants === undefined) delete top.provenance.payload.descendants;
  fs.writeFileSync(f, JSON.stringify(top));
  return f;
};
const line = (o) => `rustright-result ${JSON.stringify({ requestId: "q1", exit: 0, signal: null, outputDir: "/t/out", receiptPersisted: true, ...o })}\n`;
const seamEnvOf = () => ({ PATH: "/usr/bin", RUSTRIGHT_GATE_COMMIT: commit, RUSTRIGHT_CLASS_A: "/v/class-a.json" });

test("2. settled only by the terminal receipt of this request's job, or by a coded refusal before admission", () => {
  const dir = tmp();
  const expect = { argv: ["run", "build"], cwd: "/snap/app", adapter: "rightkit-pnpm" };
  const judge = (stderr, status = 0) => judgeSeamOutcome({ child: { status }, stderr, commit, tree, classA: "/v/class-a.json", expect });
  // the job's own terminal receipt: settled, whether it passed or its postcondition failed
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir) }))), true);
  const changed = judge(line({ receipt: receiptFor(dir, { gate: { snapshotUnchanged: false } }) }));
  assert.equal(changed.status, "fail"); assert.equal(seamSettled(changed), true, "a postcondition failure of the matching terminal job settles it");
  // another job's receipt (the f4c18bc regression): parses, fails, and settles NOTHING, so the lock stays
  const other = judge(line({ receipt: receiptFor(dir, { jobId: "someone-else" }) }));
  assert.equal(other.status, "fail"); assert.equal(seamSettled(other), false);
  // the right job id but no terminal time, or top-level id and signed jobId disagreeing: not proof
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir, { terminalAt: null }) }))), false);
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir, { id: "q2" }) }))), false);
  // a receipt that is not JSON: not settled
  const junk = path.join(dir, "junk.json"); fs.writeFileSync(junk, "{");
  assert.equal(seamSettled(judge(line({ receipt: junk }))), false);
  // refusal before admission with a code: settled; client failure after admission: not; exit 125 with no code: not
  assert.equal(seamSettled(judge(line({ requestId: null, exit: 125, receipt: null, receiptPersisted: false, code: "RK_CLASS_A_INVALID", message: "red" }), 125)), true);
  assert.equal(seamSettled(judge(line({ requestId: "q1", exit: 125, receipt: null, receiptPersisted: false, code: "RK_CLIENT_ERROR", message: "lost" }), 125)), false);
  assert.equal(seamSettled(judge(line({ requestId: null, exit: 125, receipt: null, receiptPersisted: false }), 125)), false);
  // a terminal receipt is not proof the job's processes are gone (astra, 2026-11-09): only a complete reap with a contained tree, or a job that never spawned
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir, { proc: { reap: { status: "REAP_PENDING", reaped: false, escalated: true, reason: "process-still-live" }, descendants: { status: "unverified", reason: "the job's process tree was not confirmed reaped" } } }) }))), false, "an unreaped FAILED receipt keeps the lock");
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir, { proc: { reap: undefined, processOwnership: undefined, startedAt: undefined, descendants: undefined } }) }))), false, "a forced terminal receipt (no process evidence) keeps the lock");
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir, { proc: { descendants: { status: "unverified", reason: "the survivor scan was incomplete" } } }) }))), false, "a reaped tree with an incomplete survivor scan keeps the lock");
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir, { proc: { reap: { status: "REAP_PENDING", reaped: false, escalated: false, reason: "spawn-may-have-run" }, processOwnership: null, startedAt: null, descendants: { status: "unverified", reason: "the job's process tree was not confirmed reaped" } } }) }))), false, "an unconfirmed spawn (no ownership, reap pending) keeps the lock");
  const neverSpawned = judge(line({ exit: 125, receipt: receiptFor(dir, { proc: { reap: null, processOwnership: null, startedAt: null, descendants: undefined } }), code: undefined }), 125);
  assert.equal(seamSettled(neverSpawned), true, "a job that never spawned settles");
  assert.equal(seamSettled(judge(line({ receipt: receiptFor(dir, { gate: { snapshotUnchanged: false } }) }))), true, "a changed snapshot whose tree is proven gone settles (and fails)");
  // exit 0 of a job the broker cancelled (its process had already exited 0): the receipt keeps the observed exit, the row is not a pass, and the stopped job is settled
  const cancelledZero = judge(line({ receipt: receiptFor(dir, { gate: {} }).replace(/$/, "") }));
  const cz = path.join(dir, "cancelled-zero.json"); { const r = JSON.parse(fs.readFileSync(receiptFor(dir), "utf8")); r.status = "CANCELLED"; fs.writeFileSync(cz, JSON.stringify(r)); }
  const czJudged = judge(line({ receipt: cz }));
  assert.equal(czJudged.status, "fail"); assert.equal(czJudged.code, "RECEIPT_NOT_SUCCEEDED"); assert.equal(seamSettled(czJudged), true);
  assert.equal(cancelledZero.status, "pass");
  // the predicate itself
  assert.equal(receiptIsTerminalFor({ id: "j", terminalAt: "t" }, "j"), true);
  assert.equal(receiptIsTerminalFor({ id: "j", terminalAt: "t" }, null), false);
  assert.equal(receiptIsTerminalFor({ provenance: { payload: { jobId: "j", terminalAt: "t" } } }, "j"), true);
});

// A stage carrier as the door installs it: snapshot workspace <snap>/app/src-tauri, checkout workspace <checkout>/app/src-tauri.
function stage() {
  const dir = tmp(); const snap = path.join(dir, "snap"); const checkout = path.join(dir, "checkout");
  for (const r of [snap, checkout]) { fs.mkdirSync(path.join(r, "app", "src-tauri"), { recursive: true }); fs.writeFileSync(path.join(r, "app", "src-tauri", "Cargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n"); }
  const base = path.join(dir, "carrier"); fs.mkdirSync(base);
  const installed = installStageCarrier({ base, snapshotRoot: snap, checkoutDirs: { "app/src-tauri": path.join(checkout, "app", "src-tauri") }, section: { client: "/opt/rustright" } });
  const map = JSON.parse(fs.readFileSync(installed.mapFile, "utf8"));
  return { dir, snap, checkout, installed, map, env: { ...seamEnvOf(), ...installed.env } };
}

test("1. a seam adapter call is pending in the stage ledger BEFORE the broker sees it; a child killed mid-call keeps the door's lock", () => {
  const { installed, map, snap, env } = stage();
  let seenPending = null;
  const spawn = () => {
    seenPending = readLedgerDir(map.ledgerDir).entries.filter((e) => e.mode === "seam" && e.status === "pending").length;
    throw new Error("the stage child died during the call"); // the process never reaches its outcome record
  };
  assert.throws(() => runStageAdapter("rightkit-pnpm", ["run", "build"], { cwd: path.join(snap, "app"), env }, { map, spawn }), /died during the call/);
  assert.equal(seenPending, 1, "the pending row was on disk when the call was sent");
  assert.equal(installed.seamUnsettled(), 1, "the unfinished call keeps the door's lock");
});

test("1. a seam cargo call is pending before it is sent, and a pending row that cannot be written submits nothing", () => {
  const { installed, map, snap, env } = stage();
  let calls = 0;
  const kill = () => { calls += 1; return { status: null, signal: "SIGKILL", stdout: "", stderr: "" }; };
  const r = runStageCargo(["build"], { cwd: path.join(snap, "app", "src-tauri"), env }, { map, spawn: kill, err: () => {} });
  assert.notEqual(r.status, 0); assert.equal(calls, 1);
  assert.equal(installed.seamUnsettled(), 1, "a killed client is never settled");
  // the ledger cannot be written: refused before any spawn
  calls = 0;
  const broken = { ...map, ledgerDir: "relative/ledger" };
  const refused = runStageCargo(["build"], { cwd: path.join(snap, "app", "src-tauri"), env }, { map: broken, spawn: kill, err: () => {} });
  assert.equal(refused.refusal?.code, "RK_CARGO_LEDGER"); assert.equal(calls, 0, "nothing was submitted");
  const adapterRefused = runStageAdapter("rightkit-pnpm", ["run", "build"], { cwd: path.join(snap, "app"), env }, { map: broken, spawn: kill, err: () => {} });
  assert.equal(adapterRefused.refusal?.code, "RK_CARGO_LEDGER"); assert.equal(calls, 0, "nothing was submitted");
});

test("1. the lock-release count reads every ledger file of the stage and counts unreadable records as unknown ownership", () => {
  const { installed, map } = stage();
  assert.equal(installed.seamUnsettled(), 0);
  // a child that carried a scan token files under it: still the stage's call
  appendLedger(map.ledgerDir, "0123456789abcdef", { id: "c1", mode: "seam", status: "pending", settled: false, cwd: "/x", argv: ["build"] });
  assert.equal(installed.seamUnsettled(), 1);
  appendLedger(map.ledgerDir, "0123456789abcdef", { id: "c1", mode: "seam", status: "verified", settled: true });
  assert.equal(installed.seamUnsettled(), 0);
  fs.appendFileSync(path.join(map.ledgerDir, "unscoped.jsonl"), "{not json\n");
  assert.equal(installed.seamUnsettled(), 1, "an unreadable record's owner is unknown");
  fs.writeFileSync(path.join(map.ledgerDir, "unscoped.jsonl"), `${JSON.stringify({ schema: "other", id: "z" })}\n`);
  assert.equal(installed.seamUnsettled(), 1, "a malformed record's owner is unknown");
});

test("4. a seam adapter's receipt outputDir is filed with its Cargo workspace: the stage readers and the Tauri bundle lookup find it", () => {
  const { installed, map, snap, checkout, env } = stage();
  const out = path.join(path.dirname(snap), "managed-target"); fs.mkdirSync(out);
  const rdir = path.dirname(snap);
  const cwd = path.join(snap, "app"); // `tauri build` runs from the app root, above src-tauri
  const receipt = receiptFor(rdir, { argv: ["tauri", "build"], workspace: cwd, adapter: "rightkit-tauri", gate: { classA: "/v/class-a.json" } });
  const spawn = () => ({ status: 0, signal: null, stdout: "", stderr: line({ outputDir: out, receipt }) });
  const r = runStageAdapter("rightkit-tauri", ["tauri", "build"], { cwd, env }, { map, spawn });
  assert.equal(r.status, 0, JSON.stringify(r.refusal ?? null));
  const row = readLedgerDir(map.ledgerDir).entries.find((e) => e.adapter === "rightkit-tauri");
  assert.deepEqual(row.workspaces, [path.join(checkout, "app", "src-tauri")]); assert.equal(row.settled, true);
  assert.deepEqual(installed.outputsByWorkspace(), [[path.join(checkout, "app", "src-tauri"), out]]);
  assert.ok(installed.outputDirs().includes(out));
  assert.equal(carriedOutputDir(env, path.join(snap, "app", "src-tauri", "Cargo.toml"), { preferNested: true }), out, "the Tauri bundle root comes from the seam receipt");
});

test("3. a Swift build through the seam is the broker's adapter job: --package-path ., no gate scratch or build path (GB-67)", async () => {
  const dir = tmp(); const snapshot = path.join(dir, "snap"); const pkg = path.join(snapshot, "Pkg"); fs.mkdirSync(pkg, { recursive: true });
  const verdictDir = path.join(dir, "class-a-verdicts");
  writeClassAVerdict(verdictDir, classAVerdict({ commit, tree, repoKey: "repo-key-0001", workspace: ".", scans: [{ id: "a", status: "pass" }], scope: "full" }));
  const calls = [];
  const spawn = async (cmd, args, opts = {}) => {
    calls.push({ cmd, args: [...args], cwd: opts.cwd });
    if (args[0] === "run" && args[1] === "rightkit-swift") {
      const sent = args.slice(3);
      const receipt = receiptFor(dir, { argv: sent, workspace: opts.cwd, adapter: "rightkit-swift", gate: { classA: opts.env.RUSTRIGHT_CLASS_A } });
      return { status: 0, stdout: "", stderr: line({ receipt, outputDir: path.join(dir, "out") }) };
    }
    return { status: 0, stdout: "swift 6.0\n", stderr: "" };
  };
  const scan = { id: "build:swift:Pkg", class: "B", tool: "swift", root: "cargoLive", cwd: "Pkg", args: ["build", "-c", "release"], command: "swift build -c release" };
  const plan = { schema: "rightkit.deterministic-scans/1", roots: { contentRoot: snapshot, liveRoot: snapshot, cargoLiveRoot: snapshot }, scans: [scan], notes: [], deferred: [], qualifyOwed: [] };
  const result = await runScans(plan, {
    classes: ["B"], classAEarlier: true, jobs: 1, classBJobs: 1, spawn, adapterSpawn: spawn, platform: "darwin",
    gateSeam: { commit, tree, repoKey: "repo-key-0001", snapshot, verdictDir, outputRoots: null, scope: null },
  });
  const swift = calls.filter((c) => c.args[0] === "run" && c.args[1] === "rightkit-swift");
  assert.equal(swift.length, 1, `one seam adapter call (${JSON.stringify(calls.map((c) => [c.cmd, ...c.args].join(" ")))}; row ${JSON.stringify(result.scans[0])})`);
  const sent = swift[0].args.slice(3);
  assert.deepEqual(sent.slice(0, 3), ["build", "--package-path", "."], "the one relative --package-path the adapter requires");
  assert.ok(!sent.some((a) => /^--(scratch|build)-path/.test(a)), `no gate scratch or build path: ${sent.join(" ")}`);
  assert.equal(result.scans[0].status, "pass", JSON.stringify(result.scans[0]));
});

test("4. an app inside an outer Cargo workspace: the adapter's target maps to the workspace below its cwd, not the outer one; two below map both", () => {
  const dir = tmp(); const snap = path.join(dir, "snap"); const checkout = path.join(dir, "checkout");
  for (const r of [snap, checkout]) for (const ws of [".", "app/src-tauri", "app/helper"]) { fs.mkdirSync(path.join(r, ws), { recursive: true }); fs.writeFileSync(path.join(r, ws, "Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\n"); }
  const base = path.join(dir, "carrier"); fs.mkdirSync(base);
  const installed = installStageCarrier({ base, snapshotRoot: snap, checkoutDirs: { ".": checkout, "app/src-tauri": path.join(checkout, "app", "src-tauri"), "app/helper": path.join(checkout, "app", "helper") }, section: { client: "/opt/rustright" } });
  const map = JSON.parse(fs.readFileSync(installed.mapFile, "utf8")); const env = { ...seamEnvOf(), ...installed.env };
  const out = path.join(dir, "managed-target"); fs.mkdirSync(out);
  const cwd = path.join(snap, "app");
  const receipt = receiptFor(dir, { argv: ["tauri", "build"], workspace: cwd, adapter: "rightkit-tauri" });
  const r = runStageAdapter("rightkit-tauri", ["tauri", "build"], { cwd, env }, { map, spawn: () => ({ status: 0, signal: null, stdout: "", stderr: line({ outputDir: out, receipt }) }) });
  assert.equal(r.status, 0, JSON.stringify(r.refusal ?? null));
  const byWs = new Map(installed.outputsByWorkspace());
  assert.equal(byWs.get(path.join(checkout, "app", "src-tauri")), out); assert.equal(byWs.get(path.join(checkout, "app", "helper")), out);
  assert.equal(byWs.has(checkout), false, "the outer workspace the adapter only sits in is not mapped");
  assert.equal(carriedOutputDir(env, path.join(snap, "app", "src-tauri", "Cargo.toml"), { preferNested: true }), out);
});

// A data root as the door lays it out: <data>/snapshots/<repoKey>/... and <data>/class-a-verdicts/<root verdict of the scope>.
function dataRoot() {
  const data = tmp(); const repoKey = "0123456789abcdef"; const snapshot = path.join(data, "snapshots", repoKey); const verdictDir = path.join(data, "class-a-verdicts");
  fs.mkdirSync(path.join(snapshot, "app", "src-tauri"), { recursive: true }); fs.writeFileSync(path.join(snapshot, "app", "src-tauri", "Cargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
  const rootVerdict = writeClassAVerdict(verdictDir, classAVerdict({ commit, tree, repoKey, workspace: ".", scans: [{ id: "a", status: "pass" }], scope: ["app"] }));
  return { data, repoKey, snapshot, verdictDir, rootVerdict };
}

test("D1. a stage child's seam call names the verdict of its own directory, derived from the root verdict of the door's scope (broker: workspace == cwd)", () => {
  const { snapshot, rootVerdict } = dataRoot();
  const env = { PATH: "/usr/bin", RUSTRIGHT_GATE_COMMIT: commit, RUSTRIGHT_CLASS_A: rootVerdict };
  assert.equal(verdictForCwd(env, snapshot), rootVerdict, "the snapshot root keeps the root verdict");
  const derived = verdictForCwd(env, path.join(snapshot, "app", "src-tauri"));
  const v = JSON.parse(fs.readFileSync(derived, "utf8"));
  assert.equal(v.workspace, "app/src-tauri"); assert.deepEqual(v.scope, ["app"]); assert.equal(v.commit, commit); assert.deepEqual(v.scans, [{ id: "a", status: "pass" }]);
  assert.equal(path.basename(derived), verdictFileName({ commit, scope: ["app"], workspace: "app/src-tauri" }));
  // through the real caller: the client is spawned with the derived verdict, and the receipt is judged against it
  const ledgerDir = path.join(path.dirname(snapshot), "..", "ledger");
  const map = { ledgerDir: path.resolve(ledgerDir), workspaces: { [path.join(snapshot, "app", "src-tauri")]: "/checkout/app/src-tauri" }, broker: { client: "/opt/rustright" } };
  let sent = null;
  const spawn = (cmd, argv, opts) => { sent = opts.env.RUSTRIGHT_CLASS_A; const receipt = receiptFor(path.dirname(snapshot), { argv: argv.slice(1), workspace: opts.cwd, adapter: null, gate: { classA: sent } }); return { status: 0, signal: null, stdout: "", stderr: line({ receipt }) }; };
  const r = runStageCargo(["build"], { cwd: path.join(snapshot, "app", "src-tauri"), env }, { map, spawn, err: () => {} });
  assert.equal(r.status, 0, JSON.stringify(r.refusal ?? null)); assert.equal(sent, derived);
  // an unreadable verdict is handed to the broker unchanged: the broker refuses it with its own code
  assert.equal(verdictForCwd({ ...env, RUSTRIGHT_CLASS_A: "/v/none.json" }, snapshot), "/v/none.json");
});

test("D6. the runtime cargo wrapper sends a suite's compile through the seam on a seam run, filed unsettled first; a refusal it makes settles", () => {
  const { snapshot, rootVerdict } = dataRoot();
  const ws = path.join(snapshot, "app", "src-tauri");
  const dir = tmp(); const ledgerDir = path.join(dir, "ledger"); const mapFile = path.join(dir, "map.json");
  fs.writeFileSync(mapFile, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", ledgerDir, realCargo: null, shim: null, workspaces: { [ws]: "/checkout/app/src-tauri" }, broker: { client: "/opt/rustright", snapshot: { path: snapshot } } }));
  const env = { PATH: "/usr/bin", RUSTRIGHT_GATE_COMMIT: commit, RUSTRIGHT_CLASS_A: rootVerdict, RIGHTKIT_CARGO_SCAN: "0123456789abcdef" };
  const calls = []; let pendingSeen = null;
  const spawn = (cmd, argv, opts) => {
    calls.push([cmd, ...argv]);
    pendingSeen = readLedgerDir(ledgerDir).entries.filter((e) => e.mode === "seam" && e.settled === false).length;
    const receipt = receiptFor(dir, { argv: argv.slice(1), workspace: opts.cwd, adapter: null, gate: { classA: opts.env.RUSTRIGHT_CLASS_A } });
    return { status: 0, signal: null, stdout: "", stderr: line({ receipt }) };
  };
  const status = runShim(["--map", mapFile, "--", "build", "--locked"], { env, cwd: ws, spawn, err: () => {} });
  assert.equal(status, 0);
  assert.deepEqual(calls[0].slice(0, 3), ["/opt/rustright", "cargo", "build"], "one rustright cargo call, never the carrier");
  assert.equal(pendingSeen, 1, "filed as an unsettled seam call before it was sent");
  const rows = readLedgerDir(ledgerDir).entries;
  assert.equal(rows.length, 1); assert.equal(rows[0].mode, "seam"); assert.equal(rows[0].status, "verified"); assert.equal(rows[0].settled, true);
  // a call the wrapper refuses submits nothing and settles
  const refusedStatus = runShim(["--map", mapFile, "--", "build", "--target-dir", "/x"], { env, cwd: ws, spawn, err: () => {} });
  assert.notEqual(refusedStatus, 0); assert.equal(calls.length, 1);
  assert.ok(readLedgerDir(ledgerDir).entries.every((e) => e.settled === true));
});

// A seam cargo row through the real runScans: the client answers with a result line and a receipt built from what it was sent.
function seamScanRig({ listing, receiptExtra = {}, exit = 0, diagnostics = "" }) {
  const { data, snapshot, verdictDir, repoKey } = dataRoot();
  writeClassAVerdict(verdictDir, classAVerdict({ commit, tree, repoKey, workspace: ".", scans: [{ id: "a", status: "pass" }], scope: "full" }));
  const calls = [];
  const spawn = async (cmd, args, opts = {}) => {
    calls.push({ cmd, args: [...args] });
    if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
    const receipt = receiptFor(data, { argv: args.slice(1), workspace: opts.cwd, adapter: null, gate: { classA: opts.env?.RUSTRIGHT_CLASS_A } });
    const r = JSON.parse(fs.readFileSync(receipt, "utf8")); Object.assign(r, receiptExtra); fs.writeFileSync(receipt, JSON.stringify(r));
    return { status: exit, stdout: listing.map((l) => JSON.stringify(l)).join("\n") + "\n", stderr: `${diagnostics}${line({ exit, receipt, outputDir: path.join(data, "out") })}` };
  };
  const gateSeam = { commit, tree, repoKey, snapshot, verdictDir, outputRoots: null, scope: null };
  return { spawn, calls, snapshot, gateSeam, opts: { classes: ["B"], classAEarlier: true, jobs: 1, classBJobs: 1, spawn, gateSeam, platform: process.platform } };
}
const testRow = { id: "test:rust:a", class: "B", tool: "rustright", root: "cargoLive", args: ["cargo", "test", "-p", "a", "--locked", "--message-format=json"], command: "rustright cargo test -p a --locked --message-format=json", runOutside: { args: [] } };
const planOf = (snapshot, scans) => ({ schema: "rightkit.deterministic-scans/1", roots: { contentRoot: snapshot, liveRoot: snapshot, cargoLiveRoot: snapshot }, scans, notes: [], deferred: [], qualifyOwed: [] });

test("D8 / zero list: a seam test compile whose listing has no executable passes only when the receipt's list is present and empty", async () => {
  const finished = [{ reason: "build-finished", success: true }];
  for (const [extra, want] of [[{ testExecutables: [] }, "pass"], [{ testExecutables: null, testExecutablesReason: "no JSON listing" }, "TEST_EXECUTABLE_UNSIGNED"], [{ testExecutables: [{ path: "/t/out/a-1", sha256: "f".repeat(64) }] }, "TEST_EXECUTABLE_MISSING"]]) {
    const rig = seamScanRig({ listing: finished, receiptExtra: extra });
    const r = (await runScans(planOf(rig.snapshot, [testRow]), rig.opts)).scans[0];
    if (want === "pass") assert.equal(r.status, "pass", JSON.stringify(r)); else { assert.equal(r.status, "fail"); assert.equal(r.code, want, JSON.stringify(r)); }
  }
});

test("D4/D5. a cached failure read on a seam run submitted nothing and is settled; a seam run raises open while it runs and lowers it after counting", async () => {
  const row = { id: "check:a", class: "B", tool: "rustright", root: "cargoLive", args: ["cargo", "check", "-p", "a", "--locked"], command: "rustright cargo check -p a --locked" };
  const rig = seamScanRig({ listing: [] });
  const store = new Map(); const cache = { read: (k) => store.get(k) ?? null, write: (k, v) => { store.set(k, { key: k, at: "x", row: v }); }, digest: (s) => `d-${s.id}` };
  // A deterministic compile failure stored by a direct (uncarried) run of the same inputs: the only way a failure enters the cache (NN41).
  const direct = async (cmd, args) => (args.includes("--version") ? { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" } : { status: 101, stdout: "", stderr: "error[E0425]: cannot find value `x` in this scope\nerror: could not compile `a` (lib) due to 1 previous error\n" });
  const stored = (await runScans(planOf(rig.snapshot, [row]), { classes: ["B"], classAEarlier: true, jobs: 1, classBJobs: 1, spawn: direct, cache, failureIdentity: "f1" })).scans[0];
  assert.equal(stored.status, "fail", JSON.stringify(stored));
  assert.ok([...store.values()].some((v) => v.row.status === "fail"), "the direct failure was stored");
  const before = { ...seamRunState };
  const seamRun = (await runScans(planOf(rig.snapshot, [row]), { ...rig.opts, cache, failureIdentity: "f1" })).scans[0];
  assert.equal(seamRun.failureCached, true, JSON.stringify(seamRun)); assert.equal(rig.calls.filter((c) => !c.args.includes("--version")).length, 0, "nothing submitted");
  assert.equal(seamRun.seam, true); assert.equal(seamRun.seamSettled, true, "a cached failure keeps no lock");
  assert.equal(seamRunState.unsettled, before.unsettled, "no unsettled row counted");
  assert.equal(seamRunState.open, before.open, "open is lowered once the seam run counted its rows");
});

test("NN41 on a broker host: a seam compile that failed with compiler diagnostics is cached like a pass, and the next run submits nothing", async () => {
  const row = { id: "check:a", class: "B", tool: "rustright", root: "cargoLive", args: ["cargo", "check", "-p", "a", "--locked"], command: "rustright cargo check -p a --locked" };
  const rig = seamScanRig({ listing: [], exit: 101, receiptExtra: { status: "FAILED" }, diagnostics: "error[E0425]: cannot find value `x` in this scope\nerror: could not compile `a` (lib) due to 1 previous error\n" });
  const store = new Map(); const cache = { read: (k) => store.get(k) ?? null, write: (k, v) => { store.set(k, { key: k, at: "x", row: v }); }, digest: (s) => `d-${s.id}` };
  const first = (await runScans(planOf(rig.snapshot, [row]), { ...rig.opts, cache, failureIdentity: "broker-1" })).scans[0];
  assert.equal(first.status, "fail"); assert.equal(first.code, "EXIT", JSON.stringify(first));
  assert.ok([...store.values()].some((v) => v.row.status === "fail" && v.row.id === "check:a"), "the authenticated failure was stored");
  const submitted = rig.calls.filter((c) => !c.args.includes("--version")).length;
  const second = (await runScans(planOf(rig.snapshot, [row]), { ...rig.opts, cache, failureIdentity: "broker-1" })).scans[0];
  assert.equal(second.failureCached, true, JSON.stringify(second)); assert.equal(rig.calls.filter((c) => !c.args.includes("--version")).length, submitted, "nothing submitted");
  // a job the broker stopped (TIMED_OUT) keeps cargo's own exit and text in its receipt, but the stop is not a deterministic source failure: never stored
  const stoppedRig = seamScanRig({ listing: [], exit: 101, receiptExtra: { status: "TIMED_OUT" }, diagnostics: "error[E0425]: x\nerror: could not compile `a`\n" });
  const store3 = new Map(); const cache3 = { read: (k) => store3.get(k) ?? null, write: (k, v) => { store3.set(k, { key: k, at: "x", row: v }); }, digest: (s) => `d-${s.id}` };
  const stopped = (await runScans(planOf(stoppedRig.snapshot, [row]), { ...stoppedRig.opts, cache: cache3, failureIdentity: "broker-1" })).scans[0];
  assert.equal(stopped.status, "fail"); assert.ok(![...store3.values()].some((v) => v.row.status === "fail"), "a timed-out job's failure is not cached");
  // a refusal, or a failure with no durable receipt, is never stored
  const refusedRig = seamScanRig({ listing: [], exit: 101, receiptExtra: { gate: { commit, tree, snapshot: "/snap", classA: "/elsewhere.json", snapshotUnchanged: false } }, diagnostics: "error[E0425]: x\nerror: could not compile `a`\n" });
  const store2 = new Map(); const cache2 = { read: (k) => store2.get(k) ?? null, write: (k, v) => { store2.set(k, { key: k, at: "x", row: v }); }, digest: (s) => `d-${s.id}` };
  const changed = (await runScans(planOf(refusedRig.snapshot, [row]), { ...refusedRig.opts, cache: cache2, failureIdentity: "broker-1" })).scans[0];
  assert.notEqual(changed.code, "EXIT"); assert.ok(![...store2.values()].some((v) => v.row.status === "fail"), "a postcondition failure is not cached");
});
