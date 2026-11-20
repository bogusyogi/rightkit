// The run record, the process journal and the launcher (rightkit#1514 step 3), with a stand-in for the gate: a small node script that opens
// the record, writes facts and starts one child, as the gate does. No gate, cargo or broker runs.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import { launch, takeLaunchLock } from "./acceptance-launch.mjs";
import { parseRunRecord } from "./run-record.mjs";

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-run-record-"));
const href = (f) => JSON.stringify(pathToFileURL(path.join(import.meta.dirname, f)).href);
const standIn = (dir, body) => {
  const file = path.join(dir, "stand-in-gate.mjs");
  fs.writeFileSync(file, `import { installSpawnLedger } from ${href("spawn-ledger.mjs")};
import { openRunRecord, endRunRecord, fact } from ${href("run-record.mjs")};
import { spawnSync } from "node:child_process";
installSpawnLedger();
if (openRunRecord()) process.once("exit", (code) => endRunRecord(code));
${body}
`);
  return file;
};
const lines = (f) => fs.readFileSync(f, "utf8").split(/\r?\n/).filter(Boolean).map((l) => JSON.parse(l));

test("a launched run leaves the launcher's record, the gate's facts in order, and a process journal whose root is the gate", async () => {
  const dir = tmp(); const out = path.join(dir, "run1");
  const gate = standIn(dir, `fact("lock", { token: "t1", reclaimed: null }); spawnSync(process.execPath, ["-e", "0"]); fact("still-tree", { stage: "check", path: "S", tree: "b".repeat(40) });`);
  const code = await launch({ out, commit: "a".repeat(40), kind: "unit", repo: dir, gate, gateArgs: ["--to", "check"] });
  assert.equal(code, 0);
  const l = JSON.parse(fs.readFileSync(path.join(out, "launch.json"), "utf8"));
  assert.equal(l.schema, "rightkit.acceptance-launch/1"); assert.match(l.runId, /^[0-9a-f]{64}$/); assert.equal(l.commit, "a".repeat(40));
  assert.equal(l.exit, 0); assert.ok(Number.isInteger(l.gatePid)); assert.ok(BigInt(l.endMono) > BigInt(l.startMono)); assert.equal(l.previous, null);
  assert.deepEqual(l.gate.args.slice(-4), ["--to", "check", "--commit", "a".repeat(40)]);
  const { facts, problems } = parseRunRecord(fs.readFileSync(path.join(out, "record.jsonl"), "utf8"));
  assert.deepEqual(problems, []);
  assert.deepEqual(facts.map((f) => f.kind), ["lock", "still-tree", "end"]);
  assert.ok(facts.every((f) => f.run === l.runId && f.pid === l.gatePid));
  assert.equal(facts.at(-1).root, true, "the gate the launcher started is the root writer");
  assert.ok(BigInt(facts[0].mono) > BigInt(l.startMono) && BigInt(facts.at(-1).mono) < BigInt(l.endMono), "the gate's facts fall inside the launcher's clock");
  const journal = lines(path.join(out, "ledger.jsonl.journal.jsonl"));
  assert.deepEqual(journal.map((j) => j.ev), ["start", "end", "exit"]);
  assert.equal(journal[2].root, true); assert.equal(journal[2].open, 0); assert.equal(journal[2].pid, l.gatePid);
  const ledger = lines(path.join(out, "ledger.jsonl"));
  assert.equal(ledger.length, 1); assert.equal(ledger[0].pid, l.gatePid); assert.ok(Number.isInteger(ledger[0].child), "the child's own pid is recorded");
  assert.equal(fs.existsSync(path.join(dir, "acceptance-launch.lock")), false, "the lock is released at the end");
});

test("a run killed before its end leaves no end fact and its lock; the next run takes the lock over and names it", async () => {
  const dir = tmp(); const lock = path.join(dir, "acceptance-launch.lock");
  fs.writeFileSync(lock, `${JSON.stringify({ runId: "k".repeat(64), pid: 999999, out: path.join(dir, "killed") })}\n`);
  const gate = standIn(dir, `fact("lock", { token: "t2", reclaimed: null });`);
  const out = path.join(dir, "next");
  assert.equal(await launch({ out, commit: "a".repeat(40), kind: "unit", repo: dir, gate, lock }), 0);
  const l = JSON.parse(fs.readFileSync(path.join(out, "launch.json"), "utf8"));
  assert.deepEqual(l.previous, { runId: "k".repeat(64), out: path.join(dir, "killed"), pid: 999999, reclaimed: true });
});

test("a live holder refuses the run before anything starts", () => {
  const dir = tmp(); const lock = path.join(dir, "l");
  fs.writeFileSync(lock, `${JSON.stringify({ runId: "r", pid: process.pid })}\n`);
  assert.match(takeLaunchLock(lock, { runId: "s", pid: 1 }).refused, /holds/);
});

test("the record reader refuses lines that are not facts, and orders facts by the shared monotonic clock", () => {
  const f = (kind, mono, pid, seq) => JSON.stringify({ kind, mono: String(mono), pid, seq, at: "x", run: "r" });
  const { facts, problems } = parseRunRecord([f("lock", 30, 2, 1), f("end", 50, 1, 2), "{\"kind\":\"made-up\"}", "not json", f("snapshot", 10, 1, 1)].join("\n"));
  assert.deepEqual(facts.map((x) => `${x.kind}@${x.mono}`), ["snapshot@10", "lock@30", "end@50"]);
  assert.equal(problems.length, 2);
});
