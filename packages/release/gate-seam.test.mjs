// The gate's half of the seam (reset plan section 3): verdict writer, lock body, result-line parser, receipt judge. Pure checks, no broker;
// the integration against the installed broker runs after the cutover (plan section 7, step 4).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { CLASS_A_ENV, CLASS_A_VERDICT_SCHEMA, GATE_COMMIT_ENV, classAVerdict, judgeSeamOutcome, normalizeWorkspace, parseIdentity, parseResultLine, processIdentity, readSeamReceipt, samePath, seamEnv, snapshotLockBody, verdictFileName, verdictForRun, writeClassAVerdict } from "./gate-seam.mjs";
import { SNAPSHOT_STATUS_SCHEMA, snapshotStatusAllowsReclaim, snapshotStatusMessage, snapshotStatusSync } from "./gate-seam.mjs";
import { spawn } from "node:child_process";
import { RECLAIM_QUERY_MS, snapshotReclaimJudge } from "./seam-broker.mjs";
import { acquireSnapshotLock } from "./snapshot.mjs";

test("only an exact SNAPSHOT_STATUS echo with state IDLE proves a dead holder's snapshot idle; the query crosses a real socket", async () => {
  const owner = { pid: 4242, identity: "darwin:4242:Sun Nov  9 10:00:00 2026", commit: "a".repeat(40), token: "t-1" };
  const snapshot = "/Volumes/D/.rightkit-managed/snapshots/abc";
  const message = snapshotStatusMessage({ protocol: "rustright.build-control.client.v2", runtime: { id: "r" }, snapshot, owner });
  assert.equal(message.type, "SNAPSHOT_STATUS"); assert.deepEqual(message.owner, owner);
  const idle = { type: "SNAPSHOT_STATUS", schema: SNAPSHOT_STATUS_SCHEMA, snapshot, owner, state: "IDLE", reasons: [] };
  assert.equal(snapshotStatusAllowsReclaim(idle, { snapshot, owner }), true);
  assert.equal(snapshotStatusAllowsReclaim({ ...idle, state: "BUSY" }, { snapshot, owner }), false);
  assert.equal(snapshotStatusAllowsReclaim({ ...idle, state: "UNKNOWN" }, { snapshot, owner }), false);
  assert.equal(snapshotStatusAllowsReclaim({ ...idle, owner: { ...owner, token: "t-2" } }, { snapshot, owner }), false);
  assert.equal(snapshotStatusAllowsReclaim({ ...idle, snapshot: `${snapshot}2` }, { snapshot, owner }), false);
  assert.equal(snapshotStatusAllowsReclaim({ ...idle, schema: "rustright.snapshot-status/0" }, { snapshot, owner }), false);
  assert.equal(snapshotStatusAllowsReclaim(null, { snapshot, owner }), false);
  if (process.platform === "win32") return;
  // A real unix socket answering the query from ANOTHER process (the client is synchronous and would block a server in this one): the client gets exactly the first line,
  // and the server saw the query with this owner.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-sock-")); const sock = path.join(dir, "b.sock"); const seenFile = path.join(dir, "seen.json");
  // Like the real broker (service.rs: HANDSHAKE emitted on connect, before any answer), the fake greets first, then answers the query after an unrelated frame.
  const serverScript = `const net=require("node:net"),fs=require("node:fs");const [sock,seen,reply]=process.argv.slice(1);net.createServer((c)=>{c.write(JSON.stringify({type:"HANDSHAKE",runtime:{},capabilities:[]})+"\\n");c.on("data",(d)=>{fs.writeFileSync(seen,String(d));c.write(JSON.stringify({type:"NOTE",text:"unrelated"})+"\\n"+reply+"\\n");});}).listen(sock);`;
  const server = spawn(process.execPath, ["-e", serverScript, sock, seenFile, JSON.stringify(idle)], { stdio: "ignore" });
  try {
    for (let i = 0; i < 100 && !fs.existsSync(sock); i += 1) await new Promise((r) => setTimeout(r, 50));
    assert.ok(fs.existsSync(sock), "the fake broker listens");
    const reply = snapshotStatusSync({ endpoint: sock, message, timeoutMs: 5000 });
    assert.deepEqual(reply, idle);
    const seen = JSON.parse(fs.readFileSync(seenFile, "utf8").trim());
    assert.equal(seen.type, "SNAPSHOT_STATUS"); assert.deepEqual(seen.owner, owner);
    assert.equal(snapshotStatusSync({ endpoint: `${sock}.missing`, message, timeoutMs: 2000 }), null, "no broker: no proof");
    // The REAL closure the door installs (snapshotReclaimJudge), bound to a fake installed client whose runtime manifest is valid and whose endpoint is the fake
    // broker: it reclaims on the IDLE echo for exactly this holder, and nothing else; without a client it never asks.
    const clientDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-client-")); const client = path.join(clientDir, "rustright");
    fs.writeFileSync(client, "#!/bin/sh\n", { mode: 0o755 });
    fs.writeFileSync(path.join(clientDir, "runtime-manifest.json"), JSON.stringify({ schema: "rustright.build-control.runtime.v2", version: "0.1.1", clientDigest: "a".repeat(64), brokerDigest: "b".repeat(64) }));
    const o = { platform: process.platform, env: {}, readFile: (f) => fs.readFileSync(f, "utf8"), realpath: (p) => p, lstat: fs.lstatSync, uid: process.getuid?.() ?? null, discoverEndpoint: () => sock };
    const judge = snapshotReclaimJudge({ o, client, snapshotPath: snapshot });
    assert.equal(judge(owner), true, "the door's judge reclaims on the broker's IDLE echo");
    // The query's deadline is the judge's bounded 60 s (the broker's census of ~20k completion proofs answered in 8.1 s; the former 10 s read as
    // "no proof", rightkit#1548): the judge hands it to the query, and an exact IDLE echo that arrives within it still reclaims.
    const asked = [];
    const slowJudge = snapshotReclaimJudge({ o, client, snapshotPath: snapshot, query: (args) => { asked.push(args); return snapshotStatusSync(args); } });
    assert.equal(slowJudge(owner), true);
    assert.equal(RECLAIM_QUERY_MS, 60_000);
    assert.equal(asked[0]?.timeoutMs, RECLAIM_QUERY_MS, "the judge's query carries the bounded reclaim deadline");
    assert.equal(judge({ ...owner, token: "t-2" }), false, "another token is not this owner");
    assert.equal(snapshotReclaimJudge({ o, client: null, snapshotPath: snapshot })(owner), false, "no client: no proof");
    assert.equal(snapshotReclaimJudge({ o: { ...o, discoverEndpoint: () => `${sock}.missing` }, client, snapshotPath: snapshot })(owner), false, "no broker: no proof");
    // A broker that greets and then REJECTS the query (its refusal frame is a RESULT with status REJECTED): the refusal is the answer, and the judge says no.
    const rejectSock = path.join(dir, "reject.sock");
    const rejecting = spawn(process.execPath, ["-e", `const net=require("node:net");net.createServer((c)=>{c.write(JSON.stringify({type:"HANDSHAKE"})+"\\n");c.on("data",()=>c.write(JSON.stringify({type:"RESULT",exitCode:125,status:"REJECTED",error:"unknown request type",code:"RK_UNKNOWN_REQUEST"})+"\\n"));}).listen(process.argv[1]);`, rejectSock], { stdio: "ignore" });
    try {
      for (let i = 0; i < 100 && !fs.existsSync(rejectSock); i += 1) await new Promise((r) => setTimeout(r, 50));
      const rejected = snapshotStatusSync({ endpoint: rejectSock, message, timeoutMs: 5000 });
      assert.equal(rejected?.status, "REJECTED", "the refusal frame is returned as the answer");
      assert.equal(snapshotStatusAllowsReclaim(rejected, { snapshot, owner }), false);
      assert.equal(snapshotReclaimJudge({ o: { ...o, discoverEndpoint: () => rejectSock }, client, snapshotPath: snapshot })(owner), false, "a rejected query is no proof");
    } finally { rejecting.kill(); }
    // A broker that greets and then never answers: the deadline ends the query with no proof, not a hang.
    const silentSock = path.join(dir, "silent.sock");
    const silent = spawn(process.execPath, ["-e", `const net=require("node:net");net.createServer((c)=>{c.write(JSON.stringify({type:"HANDSHAKE"})+"\\n");}).listen(process.argv[1]);`, silentSock], { stdio: "ignore" });
    try {
      for (let i = 0; i < 100 && !fs.existsSync(silentSock); i += 1) await new Promise((r) => setTimeout(r, 50));
      assert.equal(snapshotStatusSync({ endpoint: silentSock, message, timeoutMs: 1500 }), null, "HANDSHAKE alone is not an answer");
    } finally { silent.kill(); }
  } finally { server.kill(); }
});

test("a stage child's adapter runs through the seam when its environment carries the two variables, judged like a cargo row", async () => {
  const { runSeamAdapter, seamEnvPresent } = await import("./seam-broker.mjs");
  const dir = tmp(); const receiptFile = path.join(dir, "r.json");
  fs.writeFileSync(receiptFile, JSON.stringify(receiptOf({ commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true }, { jobId: "a1", argv: ["run", "build"], workspace: "/snap/app", adapter: "rightkit-pnpm", extra: { testExecutables: null } })));
  const env = { PATH: "/usr/bin", RUSTRIGHT_GATE_COMMIT: commit, RUSTRIGHT_CLASS_A: "/v/class-a.json" };
  assert.equal(seamEnvPresent(env), true); assert.equal(seamEnvPresent({ RUSTRIGHT_GATE_COMMIT: commit }), false);
  const calls = [];
  const spawnSync = (cmd, args, opts) => { calls.push({ cmd, args, cwd: opts.cwd }); return { status: 0, signal: null, stdout: "built\n", stderr: `rustright-result ${JSON.stringify({ requestId: "a1", exit: 0, signal: null, outputDir: "/t/out", receipt: receiptFile, receiptPersisted: true })}\n` }; };
  const ok = runSeamAdapter("rightkit-pnpm", ["run", "build"], { cwd: "/snap/app", env, client: "/opt/rustright", spawnSync });
  assert.deepEqual(calls[0], { cmd: "/opt/rustright", args: ["run", "rightkit-pnpm", "--", "run", "build"], cwd: "/snap/app" });
  assert.equal(ok.status, 0); assert.equal(ok.requestId, "a1"); assert.equal(ok.outputDir, "/t/out"); assert.equal(ok.refusal, undefined);
  const refusedSpawn = () => ({ status: 125, signal: null, stdout: "", stderr: `rustright-result ${JSON.stringify({ requestId: null, exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "RK_GATE_ADMISSION", message: "no verdict" })}\n` });
  const refused = runSeamAdapter("rightkit-node", ["x.mjs"], { cwd: "/snap/app", env, spawnSync: refusedSpawn });
  assert.equal(refused.status, 125); assert.equal(refused.refusal.code, "RK_GATE_ADMISSION"); assert.equal(refused.outputDir, undefined);
  assert.throws(() => runSeamAdapter("rightkit-qa", [], { env: { PATH: "/usr/bin" }, spawnSync }), /carries no seam/);
  assert.throws(() => runSeamAdapter("Bad Adapter", [], { env, spawnSync }), /not an adapter id/);
});

test("a stage child's seam call is filed in the stage ledger and judged by the parent: a failed judge is never status 0 and never a clean ledger (caller path, stage-seam.mjs)", async () => {
  const { runStageAdapter } = await import("./stage-seam.mjs");
  const { readLedger, UNSCOPED } = await import("./cargo-ledger.mjs");
  const dir = tmp(); const ledgerDir = path.join(dir, "ledger"); const receiptFile = path.join(dir, "r.json");
  const mine = { jobId: "q1", argv: ["run", "build"], workspace: "/snap/app", adapter: "rightkit-pnpm" };
  fs.writeFileSync(receiptFile, JSON.stringify(receiptOf({ commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true }, mine)));
  const badReceipt = path.join(dir, "bad.json");
  fs.writeFileSync(badReceipt, JSON.stringify(receiptOf({ commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: false }, mine)));
  const otherCommand = path.join(dir, "other.json");
  fs.writeFileSync(otherCommand, JSON.stringify(receiptOf({ commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true }, { ...mine, argv: ["run", "evil"] })));
  const otherDir = path.join(dir, "otherdir.json");
  fs.writeFileSync(otherDir, JSON.stringify(receiptOf({ commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true }, { ...mine, workspace: "/snap/other" })));
  const otherJob = path.join(dir, "otherjob.json");
  fs.writeFileSync(otherJob, JSON.stringify(receiptOf({ commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true }, { ...mine, jobId: "someone-else" })));
  const noRequest = path.join(dir, "norequest.json");
  fs.writeFileSync(noRequest, JSON.stringify({ gate: { commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true } }));
  const env = { PATH: "/usr/bin", RUSTRIGHT_GATE_COMMIT: commit, RUSTRIGHT_CLASS_A: "/v/class-a.json" };
  const map = { ledgerDir, broker: { client: "/opt/rustright" } };
  const line = (o) => `rustright-result ${JSON.stringify({ requestId: "q1", exit: 0, signal: null, outputDir: "/t/out", receiptPersisted: true, ...o })}\n`;
  const run = (stderr, status = 0) => runStageAdapter("rightkit-pnpm", ["run", "build"], { cwd: "/snap/app", env }, { map, spawn: () => ({ status, signal: null, stdout: "", stderr }) });
  const ledger = () => readLedger(ledgerDir, UNSCOPED);
  // every call is filed pending before it is sent, then its outcome under the same id (one entry per call)
  // 1. verified: status 0, ledger clean, entry settled
  const ok = run(line({ receipt: receiptFile }));
  assert.equal(ok.status, 0); assert.equal(ok.outputDir, "/t/out");
  assert.equal(ledger().problems.length, 0); assert.deepEqual(ledger().entries.map((e) => [e.mode, e.status, e.settled]), [["seam", "verified", true]]);
  // 2. the child exited 0 but the receipt says the snapshot changed: NOT status 0, a ledger problem, settled (the job ended) but failed
  const changed = run(line({ receipt: badReceipt }));
  assert.notEqual(changed.status, 0); assert.equal(changed.refusal.code, "RK_SNAPSHOT_CHANGED");
  assert.ok(ledger().problems.some((p) => /was failed/.test(p)), "the parent's finish sees a failed seam call");
  // 2b. a receipt that is not this call's request (another argv, another directory, another job, or none named) is never this call's pass, whatever its gate fields say
  for (const [file, why] of [[otherCommand, /requestedArgv/], [otherDir, /workspace/], [otherJob, /jobId/], [noRequest, /names no admitted request/]]) {
    const r = run(line({ receipt: file }));
    assert.notEqual(r.status, 0, `status for ${path.basename(file)}`); assert.equal(r.refusal.code, "RECEIPT_MISMATCH"); assert.match(r.refusal.message, why);
  }
  // 3. no result line at all: not status 0, unsettled (the lock must stay)
  const none = run("nothing printed\n");
  assert.notEqual(none.status, 0); assert.equal(none.refusal.code, "NO_RESULT_LINE");
  const unsettled = () => ledger().entries.filter((e) => e.mode === "seam" && e.settled !== true).length;
  // 2b left two unsettled: another job's receipt and one naming no job prove nothing about THIS request's job (fresh review of f4c18bc, finding 2); the other
  // argv and other directory receipts are this job's terminal record and settle it. 3 adds one.
  assert.equal(unsettled(), 3);
  // 4. a client failure AFTER admission (requestId kept, a code): unsettled
  run(`rustright-result ${JSON.stringify({ requestId: "owned", exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "RK_CLIENT_ERROR", message: "lost" })}\n`, 125);
  assert.equal(unsettled(), 4);
  // 5. a refusal BEFORE admission (requestId null): settled, a ledger problem, status non-zero
  const refused = run(`rustright-result ${JSON.stringify({ requestId: null, exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "RK_GATE_ADMISSION", message: "no verdict" })}\n`, 125);
  assert.equal(refused.status, 125); assert.equal(unsettled(), 4);
  assert.ok(ledger().problems.some((p) => /was refused/.test(p)));
});

test("the direct read wrapper passes a seam compile (RUSTRIGHT_GATE_COMMIT set) through to the client untouched and still refuses a bare compile in the snapshot", async () => {
  const { directReadSpawn } = await import("./snapshot-metadata.mjs");
  const snapshotRoot = fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-snap-")); const cwd = path.join(snapshotRoot, "ws"); fs.mkdirSync(cwd);
  const calls = [];
  const inner = (cmd, args, opts) => { calls.push({ cmd, args, env: opts.env }); return { status: 0, signal: null, error: null, stdout: "", stderr: "" }; };
  const wrapped = directReadSpawn({ spawn: inner, cargoCommand: ["rustright", "cargo"], snapshotRoot, directPath: "/usr/bin", tmpRoot: fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-tmp-")), env: {}, err: () => {} });
  const seam = wrapped("rustright", ["cargo", "check", "-p", "a", "--locked"], { cwd, env: { RUSTRIGHT_GATE_COMMIT: "a".repeat(40), RUSTRIGHT_CLASS_A: "/v/class-a.json" } });
  assert.equal(seam.status, 0); assert.equal(calls.length, 1); assert.equal(calls[0].cmd, "rustright"); assert.deepEqual(calls[0].args, ["cargo", "check", "-p", "a", "--locked"]);
  assert.equal(calls[0].env.RUSTRIGHT_GATE_COMMIT, "a".repeat(40), "the seam variables reach the client");
  assert.equal(calls[0].env.CARGO_TARGET_DIR, undefined, "no scratch target is forced on a broker compile");
  const bare = wrapped("rustright", ["cargo", "check", "-p", "a", "--locked"], { cwd, env: {} });
  assert.notEqual(bare.status, 0); assert.match(bare.stderr, /not one of the read-only shapes/); assert.equal(calls.length, 1, "a bare compile in the snapshot never reaches the client");
});

const commit = "a".repeat(40); const tree = "b".repeat(40); const repoKey = "0123456789abcdef";
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-test-"));
// A receipt as the broker's producer shapes the fields the gate compares (finish_impl.rs: provenance.payload.jobId and .request.{requestedArgv, workspace, adapter}).
// The broker's finish writes the job id and terminal time on every receipt (top-level `id`/`terminalAt`, and in the signed payload); the gate settles a row only on them.
// A reaped job with a clean survivor census (reap REAPED, descendants tree-contained): what the gate needs to call the job's processes gone.
const receiptOf = (gate, { jobId = "q1", argv = [], workspace = "/snap/app", adapter = null, extra = {} } = {}) => ({ id: jobId, status: "SUCCEEDED", terminalAt: "2026-11-09T00:00:00.000Z", reap: { status: "REAPED", reaped: true, escalated: false }, processOwnership: { pid: 1 }, startedAt: "2026-11-09T00:00:00.000Z", gate, provenance: { payload: { jobId, terminalAt: "2026-11-09T00:00:00.000Z", startedAt: "2026-11-09T00:00:00.000Z", descendants: { status: "tree-contained" }, request: { kind: adapter ? "adapter" : "cargo", adapter, requestedArgv: argv, workspace } } }, ...extra });


test("the Class A verdict exists only for a complete green plan and binds commit, tree, repository and workspace", () => {
  const v = classAVerdict({ commit, tree, repoKey, workspace: "./crates/../.", scans: [{ id: "a", status: "pass" }, { id: "b", status: "pass" }] });
  assert.equal(v.schema, CLASS_A_VERDICT_SCHEMA); assert.equal(v.workspace, "."); assert.equal(v.status, "pass"); assert.deepEqual(v.scans.map((s) => s.id), ["a", "b"]);
  assert.equal(normalizeWorkspace("crates/x/"), "crates/x");
  assert.throws(() => classAVerdict({ commit, tree, repoKey, workspace: ".", scans: [{ id: "a", status: "pass" }, { id: "b", status: "fail" }] }), /CLASS_A_RED|not complete and green/);
  assert.throws(() => classAVerdict({ commit, tree, repoKey, workspace: ".", scans: [{ id: "a", status: "pass" }, { id: "b", status: "not-run" }] }), /not complete and green/);
  assert.throws(() => classAVerdict({ commit, tree, repoKey, workspace: ".", scans: [] }), /at least one scan/);
  assert.throws(() => classAVerdict({ commit, tree, repoKey, workspace: ".", scans: [{ id: "a", status: "pass" }, { id: "a", status: "pass" }] }), /twice/);
  assert.throws(() => classAVerdict({ commit: "abc", tree, repoKey, workspace: ".", scans: [{ id: "a", status: "pass" }] }), /full sha/);
  assert.throws(() => classAVerdict({ commit, tree, repoKey, workspace: "../x", scans: [{ id: "a", status: "pass" }] }), /workspace/);
  const dir = tmp();
  const file = writeClassAVerdict(dir, v);
  assert.equal(path.basename(file), `class-a-${commit.slice(0, 12)}-full-root.json`);
  assert.deepEqual(JSON.parse(fs.readFileSync(file, "utf8")), v);
  assert.equal(fs.statSync(file).mode & 0o777, 0o600);
  assert.throws(() => writeClassAVerdict(dir, { ...v, status: "fail" }), /passing verdict/);
  assert.throws(() => writeClassAVerdict("relative", v), /absolute/);
  assert.deepEqual(seamEnv({ commit, classA: file }), { [GATE_COMMIT_ENV]: commit, [CLASS_A_ENV]: file });
  assert.throws(() => seamEnv({ commit, classA: "class-a.json" }), /absolute/);
});

test("a verdict is bound to the scope its class A covered: a class B run reads its own scope's, a covering one or a full one, never another unit's (fresh review E1)", () => {
  const verdictDir = tmp();
  const seamFor = (scope) => ({ commit, tree, repoKey, verdictDir, scope });
  const rows = [{ id: "a", status: "pass" }, { id: "b", status: "pass" }];
  // a class A scoped to packages/release writes that scope's verdict, recording its scope; nothing named "full"
  const scoped = verdictForRun({ seam: seamFor(["packages/release"]), workspace: ".", classARows: rows });
  assert.match(path.basename(scoped), /^class-a-a{12}-s[0-9a-f]{12}-root\.json$/);
  assert.deepEqual(JSON.parse(fs.readFileSync(scoped, "utf8")).scope, ["packages/release"]);
  // a class B of the same scope reads it; a class B of ANOTHER unit finds nothing and is refused; a full class B is refused too
  assert.equal(verdictForRun({ seam: seamFor(["packages/release"]), workspace: "." }), scoped);
  assert.throws(() => verdictForRun({ seam: seamFor(["crates/rightkit-fs"]), workspace: "." }), /no Class A verdict covering/);
  assert.throws(() => verdictForRun({ seam: seamFor(null), workspace: "." }), /no Class A verdict covering/);
  // a unit-list class A over several directories covers the compile of any one of them
  const list = verdictForRun({ seam: seamFor(["crates/a", "crates/b"]), workspace: ".", classARows: rows });
  assert.equal(verdictForRun({ seam: seamFor(["crates/b"]), workspace: "." }), list);
  // a full class A covers any scope; the exact scope is preferred when both exist
  const full = verdictForRun({ seam: seamFor(null), workspace: ".", classARows: rows });
  assert.equal(verdictForRun({ seam: seamFor(["crates/rightkit-fs"]), workspace: "." }), full);
  assert.equal(verdictForRun({ seam: seamFor(["packages/release"]), workspace: "." }), scoped);
  // a nested workspace's verdict is derived with the covering scope and its own workspace binding
  const nested = verdictForRun({ seam: seamFor(["crates/b"]), workspace: "crates/b" });
  const nv = JSON.parse(fs.readFileSync(nested, "utf8")); assert.equal(nv.workspace, "crates/b"); assert.deepEqual(nv.scope, ["crates/a", "crates/b"]);
  // a red row: nothing written
  assert.throws(() => verdictForRun({ seam: seamFor(["x"]), workspace: ".", classARows: [{ id: "a", status: "fail" }] }), /not complete and green/);
  // a file whose name and recorded scope disagree, or of another tree, is never read; the pre-scope name (class-a-<c>-root.json) is never matched
  const forged = path.join(verdictDir, verdictFileName({ commit, scope: ["crates/z"], workspace: "." }));
  fs.writeFileSync(forged, JSON.stringify({ ...JSON.parse(fs.readFileSync(full, "utf8")), scope: ["crates/y"] }));
  fs.writeFileSync(path.join(verdictDir, `class-a-${commit.slice(0, 12)}-root.json`), JSON.stringify({ ...JSON.parse(fs.readFileSync(full, "utf8")), scope: undefined }));
  fs.rmSync(full);
  assert.throws(() => verdictForRun({ seam: seamFor(["crates/z"]), workspace: "." }), /no Class A verdict covering/);
  assert.throws(() => verdictForRun({ seam: { ...seamFor(["packages/release"]), tree: "c".repeat(40) }, workspace: "." }), /no Class A verdict covering/);
});

test("exactly one rustright-result line is the result; none or several fails the row", () => {
  const ok = parseResultLine(`rustright: request 1\nrustright-result ${JSON.stringify({ requestId: "r1", exit: 0, signal: null, outputDir: "/t/out", receipt: "/t/r.json", receiptPersisted: true })}\n`);
  assert.equal(ok.requestId, "r1");
  assert.throws(() => parseResultLine("cargo finished\n"), /NO_RESULT_LINE|no rustright-result/);
  assert.throws(() => parseResultLine("rustright-result {}\nrustright-result {}\n"), /2 rustright-result lines/);
  assert.throws(() => parseResultLine("rustright-result not json\n"), /not JSON/);
  assert.throws(() => parseResultLine(`rustright-result ${JSON.stringify({ requestId: "r", exit: 0, signal: null, outputDir: "relative", receipt: null, receiptPersisted: true })}`), /outputDir/);
  assert.throws(() => parseResultLine(`rustright-result ${JSON.stringify({ requestId: null, exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "bad code" })}`), /code/);
  // The broker writes the signal as a number (its own tests use 9); another documented short prefix is a code too.
  const signalled = parseResultLine(`rustright-result ${JSON.stringify({ requestId: "r", exit: null, signal: 9, outputDir: "/t/out", receipt: "/t/r.json", receiptPersisted: true })}`);
  assert.equal(signalled.signal, 9);
  assert.equal(parseResultLine(`rustright-result ${JSON.stringify({ requestId: null, exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "RR_GATE_ADMISSION", message: "no verdict" })}`).code, "RR_GATE_ADMISSION");
});

test("the receipt's postconditions are judged before the exit code; a refusal keeps its code; a killed child fails", () => {
  const dir = tmp();
  const receipt = (gate, extra = {}) => { const f = path.join(dir, `${Math.random().toString(16).slice(2)}.json`); fs.writeFileSync(f, JSON.stringify({ status: "SUCCEEDED", gate, ...extra })); return f; };
  const line = (o) => `rustright-result ${JSON.stringify({ requestId: "r1", exit: 0, signal: null, outputDir: "/t/out", receiptPersisted: true, ...o })}\n`;
  const good = { commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true };
  // pass: exit 0, durable receipt, postconditions hold, test executables carried
  const r1 = receipt(good, { testExecutables: [{ executable: "/t/out/release/deps/x-1" }] });
  const pass = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: r1 }), commit, tree, classA: "/v/class-a.json" });
  assert.equal(pass.status, "pass"); assert.deepEqual(pass.evidence.testExecutables, [{ executable: "/t/out/release/deps/x-1" }]); assert.equal(pass.evidence.requestId, "r1");
  // The receipt's target triple is evidence (the gate records a broker row's linked binaries under <outputDir>/<triple>/release and nothing without it): carried when the
  // broker wrote one (finish_impl.rs `targetTriple`), null otherwise (the deleted carrier carried it the same way; the reset judge dropped it: rustright 1fd4595c install refused).
  assert.equal(pass.evidence.targetTriple, null);
  const withTriple = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: receipt(good, { testExecutables: [], targetTriple: "aarch64-apple-darwin" }) }), commit, tree, classA: "/v/class-a.json" });
  assert.equal(withTriple.status, "pass"); assert.equal(withTriple.evidence.targetTriple, "aarch64-apple-darwin");
  const emptyTriple = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: receipt(good, { testExecutables: [], targetTriple: "" }) }), commit, tree, classA: "/v/class-a.json" });
  assert.equal(emptyTriple.evidence.targetTriple, null); assert.match(pass.evidence.receiptSha256, /^[0-9a-f]{64}$/);
  // the snapshot changed during the job: fails at exit 0
  const changed = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: receipt({ ...good, snapshotUnchanged: false }) }), commit, tree });
  assert.equal(changed.status, "fail"); assert.equal(changed.code, "RK_SNAPSHOT_CHANGED");
  // another commit's receipt
  const other = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: receipt({ ...good, commit: "c".repeat(40) }) }), commit, tree });
  assert.equal(other.status, "fail"); assert.equal(other.code, "RECEIPT_MISMATCH");
  // another verdict than the one this request named
  const wrongVerdict = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: receipt(good) }), commit, tree, classA: "/v/other.json" });
  assert.equal(wrongVerdict.status, "fail"); assert.match(wrongVerdict.detail, /classA/);
  // success without a durable receipt
  const noReceipt = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: null, receiptPersisted: false }), commit, tree });
  assert.equal(noReceipt.status, "fail"); assert.equal(noReceipt.code, "RECEIPT_MISSING");
  // a refusal: nothing ran, the code is the broker's
  const refused = judgeSeamOutcome({ child: { status: 125 }, stderr: `rustright-result ${JSON.stringify({ requestId: null, exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "RK_APP_BUILD_IN_PROGRESS", message: "already queued" })}\n`, commit, tree });
  assert.equal(refused.status, "fail"); assert.equal(refused.code, "RK_APP_BUILD_IN_PROGRESS"); assert.match(refused.detail, /already queued/); assert.match(refused.detail, /before anything was admitted/);
  // a client that lost its connection AFTER admission (requestId kept, exit 125, a code, no receipt): the job's state is unknown, and the detail says so
  const lost = judgeSeamOutcome({ child: { status: 125 }, stderr: `rustright-result ${JSON.stringify({ requestId: "owned-request", exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "RK_CLIENT_ERROR", message: "connection lost" })}\n`, commit, tree });
  assert.equal(lost.status, "fail"); assert.equal(lost.code, "RK_CLIENT_ERROR"); assert.match(lost.detail, /after request owned-request was admitted/); assert.equal(lost.evidence, null);
  // an executed compile that failed keeps its exit; its receipt is still evidence
  const failed = judgeSeamOutcome({ child: { status: 101 }, stderr: line({ exit: 101, receipt: receipt(good) }), commit, tree });
  assert.equal(failed.status, "fail"); assert.equal(failed.code, "EXIT"); assert.equal(failed.detail, "exit 101"); assert.ok(failed.evidence);
  // the client's exit and its line disagree (receipt fine): a mismatch, with the receipt kept as evidence
  const mismatch = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ exit: 101, receipt: receipt(good) }), commit, tree });
  assert.equal(mismatch.code, "RESULT_EXIT_MISMATCH"); assert.ok(mismatch.evidence);
  // a postcondition failure makes the client exit 125 while the line keeps the command's exit 0: the receipt is read first, so the row says why
  const postFirst = judgeSeamOutcome({ child: { status: 125 }, stderr: line({ exit: 0, receipt: receipt({ ...good, snapshotUnchanged: false }) }), commit, tree });
  assert.equal(postFirst.code, "RK_SNAPSHOT_CHANGED");
  // Windows: the broker canonicalizes the verdict path to its extended form; the binding still holds
  const winReceipt = receipt({ ...good, classA: "\\\\?\\C:\\data\\class-a-verdicts\\Class-A.json" });
  assert.equal(readSeamReceipt(winReceipt, { commit, tree, classA: "C:/data/class-a-verdicts/class-a.json" }).problems.length, process.platform === "win32" ? 0 : 1);
  assert.equal(samePath("\\\\?\\C:\\data\\x.json", "C:/data/X.json", "win32"), true);
  assert.equal(samePath("/v/./class-a.json", "/v/class-a.json", "linux"), true);
  assert.equal(samePath("/v/a.json", "/v/b.json", "linux"), false);
  // killed before it replied: no line, never a success, never a retry
  const killed = judgeSeamOutcome({ child: { status: null, signal: "SIGKILL" }, stderr: "rustright: request 9\n", commit, tree });
  assert.equal(killed.status, "fail"); assert.equal(killed.code, "NO_RESULT_LINE");
  // killed AFTER a line that says exit 0 (the gate's timeout landed as the client was replying): the kill wins, the row never passes on the line alone
  const killedLate = judgeSeamOutcome({ child: { status: null, signal: "SIGTERM" }, stderr: line({ receipt: r1 }), commit, tree, classA: "/v/class-a.json" });
  assert.equal(killedLate.status, "fail"); assert.equal(killedLate.code, "CLIENT_KILLED");
  // the executed command died by a signal, reported as the broker spells it (a number): EXIT with the signal named, never a pass
  const sigChild = judgeSeamOutcome({ child: { status: 137 }, stderr: `rustright-result ${JSON.stringify({ requestId: "r1", exit: null, signal: 9, outputDir: "/t/out", receipt: r1, receiptPersisted: true })}\n`, commit, tree, classA: "/v/class-a.json" });
  assert.equal(sigChild.status, "fail"); assert.equal(sigChild.code, "EXIT"); assert.match(sigChild.detail, /signal 9/);
  assert.deepEqual(readSeamReceipt(path.join(dir, "missing.json"), { commit, tree }).problems.length, 1);
});

test("the lock body is { pid, identity, commit, token } with the broker's identity spelling, and a dead seam holder is reclaimed only on proof", () => {
  const spawnSync = (cmd, args) => ({ status: 0, stdout: cmd === "/bin/ps" && args[0] === "-o" ? "Sun Nov  9 10:00:00 2026\n" : "" });
  const darwin = processIdentity(4242, { platform: "darwin", spawnSync });
  assert.equal(darwin, "darwin:4242:Sun Nov  9 10:00:00 2026");
  assert.equal(processIdentity(7, { platform: "linux", readFile: () => "7 (node) S 1 7 7 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 123456 1000 10 18446744073709551615" }), "linux:7:123456");
  assert.equal(processIdentity(7, { platform: "linux", readFile: () => { throw new Error("gone"); } }), null);
  assert.equal(processIdentity(1, { platform: "darwin", spawnSync: () => ({ status: 1, stdout: "" }) }), null);
  assert.deepEqual(parseIdentity("win32:88:1762682400000"), { platform: "win32", pid: 88, start: "1762682400000" });
  assert.equal(parseIdentity("88"), null);
  const body = snapshotLockBody({ pid: 4242, identity: darwin, commit });
  assert.deepEqual(Object.keys(body).sort(), ["commit", "identity", "pid", "token"]);
  assert.throws(() => snapshotLockBody({ pid: 1, identity: darwin, commit }), /not this process/);
  // The lock file carries exactly that body; a live seam holder is busy; a dead one without broker proof stays busy; with proof it is reclaimed.
  const dir = tmp(); const lockFile = path.join(dir, "snap.lock");
  const mine = snapshotLockBody({ pid: process.pid, identity: `${process.platform}:${process.pid}:x`, commit });
  const release = acquireSnapshotLock(lockFile, {}, { body: mine });
  assert.deepEqual(JSON.parse(fs.readFileSync(lockFile, "utf8")), mine);
  release(); assert.equal(fs.existsSync(lockFile), false);
  let deadPid = 2 ** 22 + 7; // a pid no process has: kill(pid, 0) answers ESRCH
  while (true) { try { process.kill(deadPid, 0); deadPid += 1; } catch (e) { if (e.code === "ESRCH") break; deadPid += 1; } }
  fs.writeFileSync(lockFile, JSON.stringify({ pid: deadPid, identity: `${process.platform}:${deadPid}:gone`, commit, token: "t" }));
  assert.throws(() => acquireSnapshotLock(lockFile, {}, { body: mine }), /reclaimed only on the broker's proof.*GAP/);
  assert.throws(() => acquireSnapshotLock(lockFile, {}, { body: mine, judge: () => false }), /gave no such proof/);
  const reclaimed = acquireSnapshotLock(lockFile, {}, { body: mine, judge: (holder) => holder.pid === deadPid });
  assert.deepEqual(JSON.parse(fs.readFileSync(lockFile, "utf8")), mine);
  reclaimed();
  // A LIVE pid with another birth identity is a reused pid, not the holder: reclaimed on the broker's proof; the same identity is live and busy; an unreadable one is busy
  const holderBody = (identity) => JSON.stringify({ pid: process.pid, identity, commit, token: "t" });
  const liveIdentity = `${process.platform}:${process.pid}:birth-A`;
  fs.writeFileSync(lockFile, holderBody(liveIdentity));
  assert.throws(() => acquireSnapshotLock(lockFile, {}, { body: mine, judge: () => true, identityOf: () => liveIdentity }), /is held by/, "the same birth identity is the live holder");
  assert.throws(() => acquireSnapshotLock(lockFile, {}, { body: mine, judge: () => true, identityOf: () => null }), /cannot be read to tell it from a reused pid/, "an unreadable identity proves nothing");
  assert.throws(() => acquireSnapshotLock(lockFile, {}, { body: mine, judge: () => false, identityOf: () => `${process.platform}:${process.pid}:birth-B` }), /gave no such proof/, "a reused pid still needs the broker's proof");
  const reused = acquireSnapshotLock(lockFile, {}, { body: mine, judge: () => true, identityOf: () => `${process.platform}:${process.pid}:birth-B` });
  assert.deepEqual(JSON.parse(fs.readFileSync(lockFile, "utf8")), mine); reused();
  // another platform's seam lock is never judged here
  fs.writeFileSync(lockFile, JSON.stringify({ pid: deadPid, identity: `${process.platform === "win32" ? "linux" : "win32"}:${deadPid}:x`, commit, token: "t" }));
  assert.throws(() => acquireSnapshotLock(lockFile, {}, { body: mine, judge: () => true }), /is held by/);
});

test("the door's lock step resolves the judge's client as the seam does (absolute `which rustright`): with the real Mac/Dell executor shape, which carries no client field, the judge still asks the broker and reclaims on its IDLE echo (rightkit#1548)", async () => {
  if (process.platform === "win32") return;
  const { seamLockFor } = await import("./bin/rightkit-gate.mjs");
  const { spawnSync } = await import("node:child_process");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-lockfor-")); const sock = path.join(dir, "b.sock"); const seenFile = path.join(dir, "seen.json");
  const clientDir = path.join(dir, "runtime"); fs.mkdirSync(clientDir); const client = path.join(clientDir, "rustright");
  fs.writeFileSync(client, "#!/bin/sh\n", { mode: 0o755 });
  fs.writeFileSync(path.join(clientDir, "runtime-manifest.json"), JSON.stringify({ schema: "rustright.build-control.runtime.v2", version: "0.1.1", clientDigest: "a".repeat(64), brokerDigest: "b".repeat(64) }));
  const snapshotsAt = path.join(dir, "snapshots"); fs.mkdirSync(snapshotsAt); const key = "abc"; const snapshot = path.join(snapshotsAt, key);
  const holder = { pid: 4242, identity: "darwin:4242:Sun Nov  9 10:00:00 2026", commit: "a".repeat(40), token: "t-dead" };
  const idle = { type: "SNAPSHOT_STATUS", schema: SNAPSHOT_STATUS_SCHEMA, snapshot, owner: holder, state: "IDLE", reasons: [] };
  const serverScript = `const net=require("node:net"),fs=require("node:fs");const [sock,seen,reply]=process.argv.slice(1);net.createServer((c)=>{c.write(JSON.stringify({type:"HANDSHAKE",runtime:{},capabilities:[]})+"\\n");c.on("data",(d)=>{fs.writeFileSync(seen,String(d));c.write(reply+"\\n");});}).listen(sock);`;
  const server = spawn(process.execPath, ["-e", serverScript, sock, seenFile, JSON.stringify(idle)], { stdio: "ignore" });
  try {
    for (let i = 0; i < 100 && !fs.existsSync(sock); i += 1) await new Promise((r) => setTimeout(r, 50));
    const o = {
      platform: process.platform, env: { PATH: "/usr/bin:/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: sock }, exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), err: () => {},
      // `which rustright` answers the installed client by absolute path; everything else (the identity's `ps`) runs for real.
      spawn: (cmd, args, opts) => (/(^|\/)which$/.test(cmd) && args[0] === "rustright" ? { status: 0, stdout: `${client}\n`, stderr: "" } : spawnSync(cmd, args, { encoding: "utf8", ...opts })),
      resolveExecutor: () => ({ kind: "broker", cargo: ["rustright", "cargo"] }), // gate-executor.mjs's Mac/Dell shape: no `client` field
    };
    const snap = { repositoryOf: () => ({ key }) };
    const { lock, body, refusal } = seamLockFor({ o, root: dir, snapshotsAt, commit: "b".repeat(40), snap, tag: "rightkit-gate:" });
    assert.equal(refusal, undefined); assert.ok(body?.identity, "the lock body carries this process's identity");
    assert.equal(lock.judge(holder), true, "the judge asked the broker through the resolved client and reclaimed on the IDLE echo");
    const seen = JSON.parse(fs.readFileSync(seenFile, "utf8").trim());
    assert.equal(seen.type, "SNAPSHOT_STATUS"); assert.deepEqual(seen.owner, holder);
    // No absolute client on PATH: no query, no proof.
    const none = seamLockFor({ o: { ...o, spawn: (cmd, args, opts) => (/(^|\/)which$/.test(cmd) ? { status: 1, stdout: "", stderr: "" } : spawnSync(cmd, args, { encoding: "utf8", ...opts })) }, root: dir, snapshotsAt, commit: "b".repeat(40), snap, tag: "rightkit-gate:" });
    assert.equal(none.lock.judge(holder), false, "without a resolvable client the judge never proves anything");
  } finally { server.kill(); }
});
