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
import { judgeSeamOutcome, seamSettled, receiptIsTerminalFor, classAVerdict, writeClassAVerdict } from "./gate-seam.mjs";
import { runCarriedAdapter, runCarriedCargo, installCarrier, carriedOutputDir } from "./carried-cargo.mjs";
import { readLedgerDir, appendLedger } from "./cargo-ledger.mjs";
import { runScans } from "./deterministic-scans.mjs";

const commit = "a".repeat(40); const tree = "b".repeat(40);
const tmp = () => fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-repair-")));
const receiptFor = (dir, { jobId = "q1", argv = ["run", "build"], workspace = "/snap/app", adapter = "rightkit-pnpm", gate = {}, terminalAt = "2026-11-09T00:00:00.000Z", id = jobId } = {}) => {
  const f = path.join(dir, `${Math.random().toString(16).slice(2)}.json`);
  fs.writeFileSync(f, JSON.stringify({ id, terminalAt, gate: { commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true, ...gate }, provenance: { payload: { jobId, terminalAt, request: { kind: adapter ? "adapter" : "cargo", adapter, requestedArgv: argv, workspace } } } }));
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
  const installed = installCarrier({ base, snapshotRoot: snap, checkoutDirs: { "app/src-tauri": path.join(checkout, "app", "src-tauri") }, section: { client: "/opt/rustright" } });
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
  assert.throws(() => runCarriedAdapter("rightkit-pnpm", ["run", "build"], { cwd: path.join(snap, "app"), env }, { map, spawn }), /died during the call/);
  assert.equal(seenPending, 1, "the pending row was on disk when the call was sent");
  assert.equal(installed.seamUnsettled(), 1, "the unfinished call keeps the door's lock");
});

test("1. a seam cargo call is pending before it is sent, and a pending row that cannot be written submits nothing", () => {
  const { installed, map, snap, env } = stage();
  let calls = 0;
  const kill = () => { calls += 1; return { status: null, signal: "SIGKILL", stdout: "", stderr: "" }; };
  const r = runCarriedCargo(["build"], { cwd: path.join(snap, "app", "src-tauri"), env }, { map, spawn: kill, err: () => {} });
  assert.notEqual(r.status, 0); assert.equal(calls, 1);
  assert.equal(installed.seamUnsettled(), 1, "a killed client is never settled");
  // the ledger cannot be written: refused before any spawn
  calls = 0;
  const broken = { ...map, ledgerDir: "relative/ledger" };
  const refused = runCarriedCargo(["build"], { cwd: path.join(snap, "app", "src-tauri"), env }, { map: broken, spawn: kill, err: () => {} });
  assert.equal(refused.refusal?.code, "RK_CARGO_LEDGER"); assert.equal(calls, 0, "nothing was submitted");
  const adapterRefused = runCarriedAdapter("rightkit-pnpm", ["run", "build"], { cwd: path.join(snap, "app"), env }, { map: broken, spawn: kill, err: () => {} });
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
  const r = runCarriedAdapter("rightkit-tauri", ["tauri", "build"], { cwd, env }, { map, spawn });
  assert.equal(r.status, 0, JSON.stringify(r.refusal ?? null));
  const row = readLedgerDir(map.ledgerDir).entries.find((e) => e.adapter === "rightkit-tauri");
  assert.equal(row.workspace, path.join(checkout, "app", "src-tauri")); assert.equal(row.settled, true);
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
    snapshotBroker: { platform: "darwin", outputRoots: [], controlRoot: dir, commit, snapshot, handle: "h", declaredInputs: [] },
  });
  const swift = calls.filter((c) => c.args[0] === "run" && c.args[1] === "rightkit-swift");
  assert.equal(swift.length, 1, `one seam adapter call (${JSON.stringify(calls.map((c) => [c.cmd, ...c.args].join(" ")))}; row ${JSON.stringify(result.scans[0])})`);
  const sent = swift[0].args.slice(3);
  assert.deepEqual(sent.slice(0, 3), ["build", "--package-path", "."], "the one relative --package-path the adapter requires");
  assert.ok(!sent.some((a) => /^--(scratch|build)-path/.test(a)), `no gate scratch or build path: ${sent.join(" ")}`);
  assert.equal(result.scans[0].status, "pass", JSON.stringify(result.scans[0]));
});
