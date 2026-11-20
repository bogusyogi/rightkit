// Answers the open gate process keeps for later runs (resident-answers.mjs), and the broker's status asked over its pipe (broker-admission.mjs
// wireStatus). Both replace a child process per question; neither may ever give back an answer whose grounds have changed.
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { LARGE_FILE_BYTES, answerKey, fileMark, fileMarks, recallAnswer, rememberAnswer } from "./resident-answers.mjs";
import { wireStatus } from "./broker-admission.mjs";

/** A stand-in for the open process's store: the two operations, in memory. */
function store() {
  const kept = new Map(); const asked = [];
  const ask = (req) => { asked.push(req.op); if (req.op === "answer-put") { kept.set(req.key, req.value); return { ok: true }; } if (req.op === "answer-get") return { ok: true, value: kept.get(req.key) ?? null }; return null; };
  return { ask, kept, asked };
}

test("a file's mark is its bytes' digest, its size and time when large, or that it is absent or unreadable", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-answers-"));
  try {
    const f = path.join(dir, "a.txt"); fs.writeFileSync(f, "one");
    const [, first] = fileMark(f);
    assert.match(first, /^sha256:[0-9a-f]{64}$/);
    fs.writeFileSync(f, "two"); assert.notEqual(fileMark(f)[1], first);
    assert.deepEqual(fileMark(path.join(dir, "missing")), [path.join(dir, "missing"), "absent"]);
    assert.equal(fileMark(dir)[1], "unreadable:not-a-file");
    assert.equal(fileMark(f, { stat: () => ({ isFile: () => true, size: LARGE_FILE_BYTES + 1, mtimeMs: 1234.9 }) })[1], `stat:${LARGE_FILE_BYTES + 1}:1234`);
    assert.equal(fileMark(f, { stat: () => { const e = new Error("no"); e.code = "EACCES"; throw e; } })[1], "unreadable:EACCES");
    assert.deepEqual(fileMarks([f, f, "", null, path.join(dir, "missing")]).map(([p]) => p), [f, path.join(dir, "missing")]);
    assert.match(answerKey(["q", 1]), /^[0-9a-f]{64}$/); assert.notEqual(answerKey(["q", 1]), answerKey(["q", 2]));
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("an answer is given back only while every file it depends on still has the mark it had before the tool was asked", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-answers-"));
  try {
    const config = path.join(dir, ".npmrc"); const later = path.join(dir, "pnpm-workspace.yaml");
    fs.writeFileSync(config, "store-dir=x\n");
    const s = store(); const key = answerKey(["pnpm --version", dir]);
    assert.equal(recallAnswer(key, { ask: s.ask }), null);
    const marks = fileMarks([config, later]); // taken before the tool runs; `later` does not exist yet
    rememberAnswer(key, "12.16.1", marks, { ask: s.ask });
    assert.equal(recallAnswer(key, { ask: s.ask }), "12.16.1");
    fs.writeFileSync(config, "store-dir=y\n");
    assert.equal(recallAnswer(key, { ask: s.ask }), null, "an edited file asks again");
    fs.writeFileSync(config, "store-dir=x\n");
    assert.equal(recallAnswer(key, { ask: s.ask }), "12.16.1", "the same bytes again are the same grounds");
    fs.writeFileSync(later, "packages: []\n");
    assert.equal(recallAnswer(key, { ask: s.ask }), null, "a file that was absent and now exists asks again");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("nothing is kept on an unreadable mark, and nothing is believed from a reply that is not a kept answer", () => {
  const s = store(); const key = answerKey(["q"]);
  rememberAnswer(key, "v", [["/x", "unreadable:EACCES"]], { ask: s.ask });
  rememberAnswer(key, 42, [["/x", "absent"]], { ask: s.ask });
  rememberAnswer(key, "v", null, { ask: s.ask });
  assert.equal(s.kept.size, 0);
  const reply = (value) => recallAnswer(key, { ask: () => ({ ok: true, value }) });
  assert.equal(reply(null), null);
  assert.equal(reply("not json"), null);
  assert.equal(reply(JSON.stringify({ answer: 1, files: [] })), null);
  assert.equal(reply(JSON.stringify({ answer: "v", files: "x" })), null);
  assert.equal(reply(JSON.stringify({ answer: "v", files: [["/x"]] })), null);
  assert.equal(reply(JSON.stringify({ answer: "v", files: [["/x", "unreadable:EACCES"]] })), null);
  assert.equal(reply(JSON.stringify({ answer: "v", files: [] })), "v");
  assert.equal(recallAnswer(key, { ask: () => { throw new Error("no server"); } }), null);
  assert.equal(recallAnswer(key, { ask: () => null }), null);
  assert.doesNotThrow(() => rememberAnswer(key, "v", [], { ask: () => { throw new Error("no server"); } }));
});

/** A stand-in for the broker's pipe: records what was written and lets the test answer. */
function pipe() {
  const conn = new EventEmitter(); conn.written = []; conn.destroyed = false;
  conn.write = (text) => { conn.written.push(text); return true; };
  conn.destroy = () => { conn.destroyed = true; };
  return conn;
}

test("the broker's status over its pipe: one STATUS line out, the matching frame back, anything else is no answer", async () => {
  const ask = (act, options = {}) => { const conn = pipe(); const p = wireStatus({ endpoint: "\\\\.\\pipe\\rustright-broker", protocol: "p/1", runtime: { version: "r" }, id: "job-1", connect: () => conn, timeoutMs: 200, ...options }); act(conn); return p.then((frame) => ({ frame, conn })); };
  const ok = await ask((c) => { c.emit("connect"); c.emit("data", 'noise\n{"type":"STATUS","id":"other","state":"RUNNING"}\n{"type":"STATUS","id":"job-1","sta'); c.emit("data", 'te":"QUEUED"}\n'); });
  assert.deepEqual(ok.frame, { type: "STATUS", id: "job-1", state: "QUEUED" });
  assert.deepEqual(JSON.parse(ok.conn.written[0]), { type: "STATUS", protocol: "p/1", runtime: { version: "r" }, id: "job-1" });
  assert.ok(ok.conn.written[0].endsWith("\n")); assert.equal(ok.conn.destroyed, true);
  const rejected = await ask((c) => { c.emit("connect"); c.emit("data", '{"type":"RESULT","status":"REJECTED","error":"runtime mismatch"}\n'); });
  assert.equal(rejected.frame.status, "REJECTED");
  assert.equal((await ask((c) => { c.emit("connect"); c.emit("close"); })).frame, null);
  assert.equal((await ask((c) => { c.emit("error", new Error("ENOENT")); })).frame, null);
  assert.equal((await ask(() => {}, { timeoutMs: 30 })).frame, null, "no answer within the time is no answer");
  assert.equal(await wireStatus({ endpoint: "", protocol: "p/1", runtime: {}, id: "job-1", connect: () => { throw new Error("must not connect"); } }), null);
  assert.equal(await wireStatus({ endpoint: "x", protocol: "p/1", runtime: {}, id: "", connect: () => { throw new Error("must not connect"); } }), null);
  assert.equal(await wireStatus({ endpoint: "x", protocol: "p/1", runtime: {}, id: "job-1", connect: () => { throw new Error("EACCES"); } }), null);
});
