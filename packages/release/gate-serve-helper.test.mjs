// NN28: the open gate process takes its change events from the native helper's line protocol (crates/rightkit-gate-watch). The helper here
// is a stand-in that speaks the same protocol from Node's watcher, so this proves the process's side of the protocol, not the helper itself:
// what it reports as its change source, that a proof is kept only for a snapshot the helper has listed, that such a proof survives an
// overflow (the helper re-lists and reports the differences), and that every proof goes when the helper ends.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { test } from "node:test";
import { changeHelperPath, serveRequest } from "./gate-serve.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const STAND_IN = `
import fs from "node:fs"; import path from "node:path"; import readline from "node:readline";
const root = process.argv[2]; const listed = new Set();
const say = (m) => process.stdout.write(JSON.stringify(m) + "\\n");
fs.watch(root, { recursive: true }, (ev, name) => {
  if (name == null) return;
  const p = String(name).split(path.sep).join("/");
  if (p === "trigger-overflow") { if (ev !== "rename" || !fs.existsSync(path.join(root, p))) return; say({ t: "overflow" }); for (const key of listed) { say({ t: "change", p: key + "/src/a.txt" }); say({ t: "resynced", key, entries: 2 }); } say({ t: "resync-done" }); return; }
  if (p === "trigger-exit") { process.exit(7); }
  say({ t: ev === "change" ? "change" : "rename", p });
});
readline.createInterface({ input: process.stdin }).on("line", (line) => { const key = line.trim(); if (!key) return; listed.add(key); say({ t: "indexed", key, entries: 2 }); }).on("close", () => process.exit(0));
say({ t: "ready", source: "fsindex,file-watch" });
`;

test("the helper's path is the package's own file for the platform, and absent when the package ships none", () => {
  assert.equal(changeHelperPath({ platform: "win32", exists: () => false }), null);
  const file = changeHelperPath({ platform: "win32", exists: () => true });
  assert.equal(path.basename(file), "rightkit-gate-watch.exe"); assert.equal(path.basename(path.dirname(file)), "win32");
  assert.equal(path.basename(changeHelperPath({ platform: "darwin", exists: () => true })), "rightkit-gate-watch");
});

test("with the helper: the process reports its source, keeps proofs only for listed snapshots, keeps them across an overflow, drops them when the helper ends", async () => {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-serve-helper-")));
  const root = path.join(base, "snapshots"); const dir = path.join(root, "aaaaaaaaaaaaaaaa");
  fs.mkdirSync(path.join(dir, "src"), { recursive: true }); fs.writeFileSync(path.join(dir, "src", "a.txt"), "a");
  const dirB = path.join(root, "bbbbbbbbbbbbbbbb"); fs.mkdirSync(dirB);
  const standIn = path.join(base, "stand-in.mjs"); fs.writeFileSync(standIn, STAND_IN);
  const sock = process.platform === "win32" ? `\\\\.\\pipe\\rk-serve-helper-test-${process.pid}-${Date.now()}` : path.join(base, "test.sock");
  const log = path.join(base, "serve.log");
  const script = `import(${JSON.stringify(pathToFileURL(path.join(here, "gate-serve.mjs")).href)}).then((m) => m.serve({ root: ${JSON.stringify(root)}, listenOn: ${JSON.stringify(sock)}, helper: { command: process.execPath, args: [${JSON.stringify(standIn)}] }, log: (l) => require("node:fs").appendFileSync(${JSON.stringify(log)}, l + "\\n") }));`;
  const child = spawn(process.execPath, ["--input-type=commonjs", "-e", script], { stdio: "ignore" });
  const ask = (req) => serveRequest(req, { sock, env: {} });
  try {
    let up = null;
    for (let i = 0; i < 100 && !up?.healthy; i += 1) { up = ask({ op: "status" }); if (!up?.healthy) await sleep(100); }
    assert.ok(up?.healthy, "the test server answers and its helper is ready");
    assert.equal(up.changeSource, "fsindex,file-watch");

    const begun = ask({ op: "begin", dir });
    assert.equal(begun.source, "fsindex,file-watch");
    assert.equal(ask({ op: "record", dir, tree: "t1", generation: begun.generation })?.ok, true);
    const clean = ask({ op: "check", dir, tree: "t1" });
    assert.equal(clean.clean, true); assert.equal(clean.source, "fsindex,file-watch");

    // A second snapshot this process has seen no event for: a proof of it is begun now and recorded only after the events were dropped.
    const begunB = ask({ op: "begin", dir: dirB });
    assert.ok(Number.isInteger(begunB?.generation));

    // Events were dropped. The helper lists the snapshot again and reports what differs; the proof's base survives and names it.
    fs.writeFileSync(path.join(root, "trigger-overflow"), "x");
    await sleep(500);
    // Review 2026-11-19: the untouched snapshot's count stayed at 0 through the overflow, so this record was accepted.
    assert.equal(ask({ op: "record", dir: dirB, tree: "t9", generation: begunB.generation }), null, "a proof begun before events were dropped is not recorded after");
    assert.equal(ask({ op: "check", dir: dirB, tree: "t9" }).clean, false);
    const after = ask({ op: "check", dir, tree: "t1" });
    assert.equal(after.clean, false);
    assert.ok(after.base, "the tree's base proof survived the overflow");
    assert.deepEqual(after.base.changed, ["src/a.txt"]);

    // The helper ends: nothing is known any more, every proof goes, and it is started again.
    assert.equal(ask({ op: "record", dir, tree: "t1", generation: after.generation })?.ok, true);
    assert.equal(ask({ op: "check", dir, tree: "t1" }).clean, true);
    const begunAgainB = ask({ op: "begin", dir: dirB });
    fs.writeFileSync(path.join(root, "trigger-exit"), "x");
    let again = null;
    for (let i = 0; i < 100; i += 1) { await sleep(100); again = ask({ op: "check", dir, tree: "t1" }); if (again) break; }
    assert.ok(again, "the process answers again after its helper was restarted");
    assert.equal(again.clean, false); assert.equal(again.base, undefined);
    assert.equal(ask({ op: "record", dir: dirB, tree: "t9", generation: begunAgainB.generation }), null, "a proof begun before the helper ended is not recorded after it was started again");
    assert.match(fs.readFileSync(log, "utf8"), /change helper ended \(exit 7\); every proof dropped; starting it again/);
  } finally {
    child.kill();
    await sleep(300);
    fs.rmSync(base, { recursive: true, force: true });
  }
});

test("without a helper the process says so: Node's watcher", async () => {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-serve-nohelper-")));
  const root = path.join(base, "snapshots"); fs.mkdirSync(path.join(root, "aaaaaaaaaaaaaaaa"), { recursive: true });
  const sock = process.platform === "win32" ? `\\\\.\\pipe\\rk-serve-nohelper-test-${process.pid}-${Date.now()}` : path.join(base, "test.sock");
  const script = `import(${JSON.stringify(pathToFileURL(path.join(here, "gate-serve.mjs")).href)}).then((m) => m.serve({ root: ${JSON.stringify(root)}, listenOn: ${JSON.stringify(sock)}, helper: null }));`;
  const child = spawn(process.execPath, ["--input-type=module", "-e", script], { stdio: "ignore" });
  try {
    let up = null;
    for (let i = 0; i < 100 && !up; i += 1) { up = serveRequest({ op: "status" }, { sock, env: {} }); if (!up) await sleep(100); }
    assert.equal(up?.changeSource, "node-fs-watch");
    assert.equal(serveRequest({ op: "begin", dir: path.join(root, "aaaaaaaaaaaaaaaa") }, { sock, env: {} }).source, "node-fs-watch");
  } finally { child.kill(); await sleep(200); fs.rmSync(base, { recursive: true, force: true }); }
});
