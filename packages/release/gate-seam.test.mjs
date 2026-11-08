// The gate's half of the seam (reset plan section 3): verdict writer, lock body, result-line parser, receipt judge. Pure checks, no broker;
// the integration against the installed broker runs after the cutover (plan section 7, step 4).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { CLASS_A_ENV, CLASS_A_VERDICT_SCHEMA, GATE_COMMIT_ENV, classAVerdict, judgeSeamOutcome, normalizeWorkspace, parseIdentity, parseResultLine, processIdentity, readSeamReceipt, samePath, seamEnv, snapshotLockBody, writeClassAVerdict } from "./gate-seam.mjs";
import { acquireSnapshotLock } from "./snapshot.mjs";

const commit = "a".repeat(40); const tree = "b".repeat(40); const repoKey = "0123456789abcdef";
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-seam-test-"));

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
  assert.equal(path.basename(file), `class-a-${commit.slice(0, 12)}-root.json`);
  assert.deepEqual(JSON.parse(fs.readFileSync(file, "utf8")), v);
  assert.equal(fs.statSync(file).mode & 0o777, 0o600);
  assert.throws(() => writeClassAVerdict(dir, { ...v, status: "fail" }), /passing verdict/);
  assert.throws(() => writeClassAVerdict("relative", v), /absolute/);
  assert.deepEqual(seamEnv({ commit, classA: file }), { [GATE_COMMIT_ENV]: commit, [CLASS_A_ENV]: file });
  assert.throws(() => seamEnv({ commit, classA: "class-a.json" }), /absolute/);
});

test("exactly one rustright-result line is the result; none or several fails the row", () => {
  const ok = parseResultLine(`rustright: request 1\nrustright-result ${JSON.stringify({ requestId: "r1", exit: 0, signal: null, outputDir: "/t/out", receipt: "/t/r.json", receiptPersisted: true })}\n`);
  assert.equal(ok.requestId, "r1");
  assert.throws(() => parseResultLine("cargo finished\n"), /NO_RESULT_LINE|no rustright-result/);
  assert.throws(() => parseResultLine("rustright-result {}\nrustright-result {}\n"), /2 rustright-result lines/);
  assert.throws(() => parseResultLine("rustright-result not json\n"), /not JSON/);
  assert.throws(() => parseResultLine(`rustright-result ${JSON.stringify({ requestId: "r", exit: 0, signal: null, outputDir: "relative", receipt: null, receiptPersisted: true })}`), /outputDir/);
  assert.throws(() => parseResultLine(`rustright-result ${JSON.stringify({ requestId: null, exit: 125, signal: null, outputDir: null, receipt: null, receiptPersisted: false, code: "bad code" })}`), /code/);
});

test("the receipt's postconditions are judged before the exit code; a refusal keeps its code; a killed child fails", () => {
  const dir = tmp();
  const receipt = (gate, extra = {}) => { const f = path.join(dir, `${Math.random().toString(16).slice(2)}.json`); fs.writeFileSync(f, JSON.stringify({ gate, ...extra })); return f; };
  const line = (o) => `rustright-result ${JSON.stringify({ requestId: "r1", exit: 0, signal: null, outputDir: "/t/out", receiptPersisted: true, ...o })}\n`;
  const good = { commit, tree, snapshot: "/snap", classA: "/v/class-a.json", snapshotUnchanged: true };
  // pass: exit 0, durable receipt, postconditions hold, test executables carried
  const r1 = receipt(good, { testExecutables: [{ executable: "/t/out/release/deps/x-1" }] });
  const pass = judgeSeamOutcome({ child: { status: 0 }, stderr: line({ receipt: r1 }), commit, tree, classA: "/v/class-a.json" });
  assert.equal(pass.status, "pass"); assert.deepEqual(pass.evidence.testExecutables, [{ executable: "/t/out/release/deps/x-1" }]); assert.equal(pass.evidence.requestId, "r1"); assert.match(pass.evidence.receiptSha256, /^[0-9a-f]{64}$/);
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
  assert.equal(refused.status, "fail"); assert.equal(refused.code, "RK_APP_BUILD_IN_PROGRESS"); assert.match(refused.detail, /already queued/);
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
  // another platform's seam lock is never judged here
  fs.writeFileSync(lockFile, JSON.stringify({ pid: deadPid, identity: `${process.platform === "win32" ? "linux" : "win32"}:${deadPid}:x`, commit, token: "t" }));
  assert.throws(() => acquireSnapshotLock(lockFile, {}, { body: mine, judge: () => true }), /is held by/);
});
