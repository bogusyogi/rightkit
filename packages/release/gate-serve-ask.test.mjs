import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// The client blocks its own thread while it waits, so the fake server must be another process.
const serverScript = `
const net = require("node:net"); const [sock, mode] = process.argv.slice(1); let n = 0;
net.createServer((c) => { let buf = ""; c.on("data", (d) => { buf += d; if (!buf.includes("\\n")) return; n += 1;
  const req = JSON.parse(buf.slice(0, buf.indexOf("\\n")));
  if (mode === "silent") return;
  if (mode === "garbage") return c.end("not json\\n");
  c.end(JSON.stringify({ ok: true, echo: req.op, n, schema: req.schema, hasIdentity: typeof req.identity === "string", big: "x".repeat(req.size || 0) }) + "\\n"); });
}).listen(sock, () => process.stdout.write("listening\\n"));
`;
const start = async (mode) => {
  const sock = process.platform === "win32" ? `\\\\.\\pipe\\rk-ask-test-${process.pid}-${mode}-${Date.now()}` : path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-")), "a");
  const child = spawn(process.execPath, ["-e", serverScript, sock, mode], { stdio: ["ignore", "pipe", "inherit"] });
  await new Promise((resolve, reject) => { child.stdout.once("data", resolve); child.once("exit", (c) => reject(new Error(`the fake server ended: ${c}`))); });
  return { sock, stop: () => child.kill() };
};

test("the gate asks the resident gate from its own process: a reply comes back, and no child process is started", async () => {
  const { installSpawnLedger } = await import("./spawn-ledger.mjs");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-ask-")); const ledger = path.join(dir, "ledger.jsonl");
  installSpawnLedger({ RK_GATE_SPAWN_LEDGER: ledger });
  const { askInProcess, serveRequest } = await import("./gate-serve.mjs");
  const s = await start("ok");
  try {
    const before = fs.existsSync(ledger) ? fs.readFileSync(ledger, "utf8").split("\n").filter(Boolean).length : 0;
    const a = askInProcess({ op: "status" }, 3000, { sock: s.sock });
    assert.equal(a.ok, true); assert.equal(a.echo, "status"); assert.equal(a.schema, "rightkit.gate-serve/1"); assert.equal(a.hasIdentity, true); assert.equal(a.n, 1);
    // Again, and a large reply (a cargo metadata answer is megabytes): each is its own connection, whole.
    const b = askInProcess({ op: "check", size: 3_000_000 }, 5000, { sock: s.sock });
    assert.equal(b.n, 2); assert.equal(b.big.length, 3_000_000);
    const c = serveRequest({ op: "check" }, { env: {}, sock: s.sock });
    assert.equal(c.ok, true); assert.equal(c.n, 3);
    const after = fs.existsSync(ledger) ? fs.readFileSync(ledger, "utf8").split("\n").filter(Boolean).length : 0;
    assert.equal(after, before, "three questions, no process started");
  } finally { s.stop(); }
});

test("no server, a server that never answers and one that answers nonsense are all 'no answer', within the time given", async () => {
  const { askInProcess, serveRequest } = await import("./gate-serve.mjs");
  const missing = process.platform === "win32" ? `\\\\.\\pipe\\rk-ask-test-missing-${process.pid}` : path.join(os.tmpdir(), `rk-missing-${process.pid}`);
  const none = askInProcess({ op: "status" }, 2000, { sock: missing });
  assert.equal(none.ok, false); assert.equal(none.unreachable, true);
  const silent = await start("silent");
  try { const t0 = Date.now(); const r = askInProcess({ op: "status" }, 400, { sock: silent.sock }); assert.deepEqual(r, { ok: false, why: "timeout" }); assert.ok(Date.now() - t0 < 2500, "it does not wait past its time"); } finally { silent.stop(); }
  const garbage = await start("garbage");
  try { assert.deepEqual(askInProcess({ op: "status" }, 2000, { sock: garbage.sock }), { ok: false, why: "bad reply" }); assert.equal(serveRequest({ op: "status" }, { env: {}, sock: garbage.sock }), null); } finally { garbage.stop(); }
  // Switched off, nothing is asked.
  assert.equal(serveRequest({ op: "status" }, { env: { RK_GATE_SERVE: "0" }, sock: missing }), null);
});
