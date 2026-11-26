// The resident gate (GC-A24, rightkit#1506): a gate-owned, per-host process outside the broker that watches the snapshot root and
// remembers which snapshot directories were proven exactly a tree, so a repeat proof of an unchanged snapshot needs no walk.
//
// Measured before it was built (Mac, 2026-11-05): node with the gate's modules starts in 34-50 ms, so residency buys nothing at
// startup; what it buys is proof. A warm RK check spent ~3.3 s of 10.8 s in six snapshot verifies of an unchanged directory.
//
// Correctness:
// - A proof is recorded only with the change generation taken BEFORE the walk began: any event for that snapshot between "begin" and
//   "record" leaves it unproven.
// - Every answer is preceded by a cookie sync (as watchman does): the server writes a cookie file in the watched root and waits for its
//   own event, so every change made before the request has been delivered and counted before the server answers.
// - A watcher error, a dropped or unknown event, an unreachable server, a timeout, or a server running other gate code all mean "not
//   proven", and the caller does the full verify. The server never says "clean" on a guess.
// - Only plain verifies (no stage-output ledger) use it.
//
// Lifecycle: started detached by the first client that finds no server (no switch disables it: RK_GATE_SERVE was removed 2026-11-24, rightkit#1514); one server per host and data root;
// exits after RK_GATE_SERVE_IDLE_MS (default 30 min) without a request, or when a client runs different gate code. No system service is
// installed. Commands: `node gate-serve.mjs serve`, `node gate-serve.mjs status`, and the internal client ops below.
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { MessageChannel, Worker, receiveMessageOnPort } from "node:worker_threads";
import { fileURLToPath } from "node:url";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { createChangeLedger } from "./snapshot-changes.mjs";

const SELF = fileURLToPath(import.meta.url);
const SCHEMA = "rightkit.gate-serve/1";
const COOKIE_PREFIX = "0000000000000000.gate-serve-cookie-";
// The server's identity covers every local module it runs (this file, managed-roots.mjs and snapshot-changes.mjs, its local imports), so a client never
// takes answers from a server whose code differs from its own, e.g. one started from an edited checkout answering a sealed gate.
/**
 * NN28: the native change source the package ships for this platform (`bin/<platform>/rightkit-gate-watch`, built from RightKit's
 * rightkit-file-watch and rightkit-fsindex crates; crates/rightkit-gate-watch), or null when this package has none for it.
 */
export function changeHelperPath({ platform = process.platform, exists = fs.existsSync } = {}) {
  const file = fileURLToPath(new URL(`./bin/${platform}/rightkit-gate-watch${platform === "win32" ? ".exe" : ""}`, import.meta.url));
  return exists(file) ? file : null;
}
// The helper's bytes are part of the server's identity: another helper is another server.
const SERVER_MODULES = [SELF, fileURLToPath(new URL("./managed-roots.mjs", import.meta.url)), fileURLToPath(new URL("./snapshot-changes.mjs", import.meta.url)), ...(changeHelperPath() ? [changeHelperPath()] : [])];
const IDENTITY = (() => { try { const h = crypto.createHash("sha256"); for (const f of SERVER_MODULES) { h.update(path.basename(f)); h.update("\0"); h.update(fs.readFileSync(f)); h.update("\0"); } return h.digest("hex").slice(0, 16); } catch { return "unknown"; } })();

function dataRoot(env = process.env) { return resolveManagedRoots({ env, platform: process.platform }).gateRoot; }
export function socketPath(env = process.env) {
  // The name carries the server's code identity (Dell 2026-11-19): every gate of this user shared one name, and a server asked by a gate of
  // other code exits ("gate code changed"). Two gates in use on one machine (a sealed gate and a candidate under test, or two agents' gates)
  // therefore ended each other's server at every question, and each lost its proofs each time. Each gate code now has its own server.
  if (process.platform === "win32") return `\\\\.\\pipe\\rightkit-gate-serve-${(os.userInfo().username || "user").replace(/[^A-Za-z0-9_-]/g, "_")}-${IDENTITY}`;
  return path.join(dataRoot(env), "gate-serve", `serve-${IDENTITY.slice(0, 8)}.sock`); // a socket path is limited to about 104 characters
}

// ---- server ---------------------------------------------------------------------------------------------------------------------------

/** `listenOn`: another socket than this gate code's own (a test's private one). */
export async function serve({ root, idleMs = Number(process.env.RK_GATE_SERVE_IDLE_MS ?? 30 * 60 * 1000), log = () => {}, listenOn = null, helper = (() => { const file = changeHelperPath(); return file ? { command: file, args: [] } : null; })() } = {}) {
  const watchedRoot = fs.realpathSync(root);
  const generation = new Map(); // snapshot key -> change count
  const proven = new Map(); // snapshot key -> { tree, generation }
  // NN11: the tree a snapshot was last proven to hold, with every path that changed since (snapshot-changes.mjs). `proven` answers "nothing
  // changed"; this answers "these paths changed", so a later proof checks those files and does not hash the whole tree again.
  const treeBase = new Map(); // snapshot key -> { tree, ledger }
  // GC-A11 non-compiling cargo answers (cargo metadata), keyed on the request and the snapshot's change count: an unchanged snapshot is answered
  // from memory, any change to it (or a restart) runs cargo again. Bounded: oldest dropped past 64 entries.
  const cargoAnswers = new Map(); // request digest -> { key, generation, status, stdout, stderr }
  let watcher = null; let healthy = false; let cookieSeq = 0;
  const cookies = new Map(); // cookie name -> resolve
  // Only entries inside a snapshot directory (`<16 hex>/...`) are snapshot content; `<key>.tree-index`, `<key>.state.json` and the like
  // beside it are the gate's own metadata, and the cookie is `0000000000000000.gate-serve-cookie-*` (a name the snapshot root accepts).
  const keyOf = (rel) => { const base = String(rel ?? "").split(/[\\/]/)[0]; return /^[0-9a-f]{16}$/.test(base) ? base : null; };
  // The change count a proof is taken at and recorded against: the snapshot's own events plus `floor`, which rises whenever something may
  // have changed anywhere without being named (an event without a name, a watcher error, dropped events, the helper ending). Review
  // 2026-11-19: the count used to rise only for snapshots that already had an entry, so a snapshot with no event since this process started
  // stayed at 0 through such a moment, and a proof begun before it was recorded after it.
  let floor = 0;
  const genOf = (key) => floor + (generation.get(key) ?? 0);
  const bumpAll = () => { floor += 1; proven.clear(); treeBase.clear(); };
  // One change under the watched root, from either source: "change" (content or metadata) or "rename" (appeared, disappeared, renamed).
  const onEvent = (type, filename) => {
    if (filename == null) { bumpAll(); return; } // an event without a name: anything may have changed
    const name = String(filename);
    const base = name.split(/[\\/]/)[0];
    if (base.startsWith(COOKIE_PREFIX)) { const r = cookies.get(base); if (r) { cookies.delete(base); r(); } return; }
    const key = keyOf(name);
    // The watched root's own entry (its listing changed) carries no content: the entry that changed reports under its own name.
    if (key === null) { if (/^[0-9a-f]{16}\./.test(base) || name === path.basename(watchedRoot)) return; bumpAll(); return; } // gate metadata beside a snapshot; anything else: drop every proof
    generation.set(key, (generation.get(key) ?? 0) + 1);
    proven.delete(key);
    treeBase.get(key)?.ledger.touch(type, name.split(/[\\/]/).slice(1).join("/"));
  };
  // How this process learns of changes, as it reports it (NN28): the helper's own word, or Node's watcher.
  let changeSource = helper ? "starting" : "node-fs-watch";
  const startWatch = () => {
    try {
      watcher = fs.watch(watchedRoot, { recursive: true, persistent: true }, onEvent);
      watcher.on("error", (error) => { log(`watcher error: ${error?.message ?? error}; every proof dropped, restarting the watch`); healthy = false; bumpAll(); try { watcher.close(); } catch { /* closed */ } setTimeout(startWatch, 200); });
      healthy = true;
    } catch (error) { healthy = false; log(`cannot watch ${watchedRoot}: ${error?.message ?? error}`); setTimeout(startWatch, 1000); }
  };
  // NN28: the native helper (crates/rightkit-gate-watch) names each change from rightkit-file-watch and keeps a rightkit-fsindex listing of
  // every snapshot this process holds a proof of. When events are dropped it lists those snapshots again and reports each difference as an
  // ordinary change. So after an overflow no snapshot is answered "nothing changed" until it is proven again, but a listed snapshot keeps its
  // record of what changed (the next proof walks it and compares those files; the tree is not hashed again). Of a snapshot the helper has
  // not listed nothing is known then.
  // It is this process's own child for as long as this process lives (it ends when its input closes); no gate run starts it.
  let helperChild = null; let helperStarts = 0;
  const helperListed = new Set(); const listWaiters = new Map();
  // Events were dropped. No snapshot may be answered "nothing changed" on the strength of an earlier proof, and no proof begun before this
  // moment may be recorded (the floor rises for all). What survives is the change record of a snapshot the helper has listed: the helper
  // lists it again and reports every difference, so "these paths changed" stays true for it. Of any other snapshot nothing is known.
  const dropUnlisted = () => { floor += 1; proven.clear(); for (const k of [...treeBase.keys()]) if (!helperListed.has(k)) treeBase.delete(k); };
  const onHelper = (m) => {
    if (!m || typeof m.t !== "string") return;
    if (m.t === "ready") { healthy = true; changeSource = typeof m.source === "string" && m.source ? m.source : "fsindex,file-watch"; return; }
    if (m.t === "change" || m.t === "rename") { if (typeof m.p === "string" && m.p) onEvent(m.t, m.p); return; }
    if (m.t === "indexed") { if (typeof m.key === "string") { helperListed.add(m.key); for (const r of listWaiters.get(m.key) ?? []) r(true); listWaiters.delete(m.key); } return; }
    if (m.t === "overflow") { dropUnlisted(); return; }
    if (m.t === "error") log(`change helper: ${m.message}`);
  };
  const startHelper = () => {
    helperStarts += 1;
    let child;
    try { child = spawn(helper.command, [...(helper.args ?? []), watchedRoot], { stdio: ["pipe", "pipe", "ignore"], windowsHide: true }); }
    catch (error) { log(`cannot start the change helper: ${error?.message ?? error}; using Node's watcher`); changeSource = "node-fs-watch"; startWatch(); return; }
    helperChild = child; let buf = "";
    const lost = (why) => {
      if (helperChild !== child) return;
      helperChild = null; healthy = false; helperListed.clear();
      for (const w of listWaiters.values()) for (const r of w) r(false);
      listWaiters.clear(); bumpAll();
      // A helper that keeps ending is not waited for: Node's watcher takes over, and the process says so (change-source=node-fs-watch).
      if (helperStarts >= 3) { log(`change helper ended (${why}) ${helperStarts} times; every proof dropped; using Node's watcher`); changeSource = "node-fs-watch"; startWatch(); }
      else { log(`change helper ended (${why}); every proof dropped; starting it again`); setTimeout(startHelper, 300); }
    };
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (d) => { buf += d; for (let nl = buf.indexOf("\n"); nl >= 0; nl = buf.indexOf("\n")) { const text = buf.slice(0, nl); buf = buf.slice(nl + 1); let m = null; try { m = JSON.parse(text); } catch { m = null; } if (helperChild === child) onHelper(m); } });
    child.stdin.on("error", () => {});
    child.on("error", (error) => lost(error?.message ?? "error"));
    child.on("exit", (code, signal) => lost(signal ? `signal ${signal}` : `exit ${code}`));
  };
  /** Resolves true once the helper holds a listing of snapshot `key` (at once with Node's watcher, which keeps none). */
  const listed = (key) => {
    if (!helperChild) return Promise.resolve(changeSource === "node-fs-watch");
    if (helperListed.has(key)) return Promise.resolve(true);
    return new Promise((resolve) => {
      const waiting = listWaiters.get(key) ?? []; waiting.push(resolve); listWaiters.set(key, waiting);
      if (waiting.length === 1) { try { helperChild.stdin.write(`${key}\n`); } catch { resolve(false); } }
      setTimeout(() => resolve(false), 30_000).unref?.();
    });
  };
  if (helper) startHelper(); else startWatch();
  // Every change made before this call has been delivered once the cookie's own event arrives (events of one watch are ordered).
  const cookieSync = (timeoutMs = 2000) => new Promise((resolve) => {
    if (!healthy) { resolve(false); return; }
    const name = `${COOKIE_PREFIX}${process.pid}-${++cookieSeq}`;
    const file = path.join(watchedRoot, name);
    const timer = setTimeout(() => { cookies.delete(name); try { fs.rmSync(file, { force: true }); } catch { /* gone */ } resolve(false); }, timeoutMs);
    cookies.set(name, () => { clearTimeout(timer); try { fs.rmSync(file, { force: true }); } catch { /* gone */ } resolve(true); });
    try { fs.writeFileSync(file, String(Date.now())); } catch { clearTimeout(timer); cookies.delete(name); resolve(false); }
  });
  let lastRequest = Date.now();
  const answers = new Map(); const ANSWER_MAX_ENTRIES = 512; const ANSWER_MAX_BYTES = 1 << 20;
  const handle = async (req) => {
    lastRequest = Date.now();
    if (req?.schema !== SCHEMA) return { ok: false, why: "schema" };
    if (req.identity !== IDENTITY) { setTimeout(() => process.exit(0), 50); return { ok: false, why: "gate code changed; the server exits" }; }
    // `stop`: end this server now, the same way the idle timer ends it. The server of a candidate gate runs from the candidate's own
    // folder, and on Windows that keeps `trusted-gate promote` from renaming the folder (rightkit#1778: "Access is denied" until the
    // process was ended by hand). `node gate-serve.mjs stop` is the supported way to end it before a promote.
    if (req.op === "stop") { setTimeout(() => process.exit(0), 50); return { ok: true, stopping: true, pid: process.pid }; }
    if (req.op === "status") return { ok: true, healthy, changeSource, root: watchedRoot, proven: [...proven].map(([k, v]) => ({ key: k, tree: v.tree })), pid: process.pid, identity: IDENTITY };
    // Answers kept for later runs (resident-answers.mjs): an opaque string under a sha256 key. The caller stores each answer with the files it
    // depends on and checks them itself before it uses one; this process only holds the bytes, in memory, oldest out first.
    if (req.op === "answer-get" || req.op === "answer-put") {
      if (typeof req.key !== "string" || !/^[0-9a-f]{64}$/.test(req.key)) return { ok: false, why: "bad answer key" };
      if (req.op === "answer-get") return { ok: true, value: answers.get(req.key) ?? null };
      if (typeof req.value !== "string" || req.value.length > ANSWER_MAX_BYTES) return { ok: false, why: "bad answer value" };
      answers.delete(req.key);
      if (answers.size >= ANSWER_MAX_ENTRIES) answers.delete(answers.keys().next().value);
      answers.set(req.key, req.value);
      return { ok: true };
    }
    const dir = typeof req.dir === "string" ? req.dir : "";
    let real; try { real = fs.realpathSync(dir); } catch { return { ok: false, why: "no such directory" }; }
    if (path.dirname(real) !== watchedRoot) return { ok: false, why: "not a snapshot of the watched root" };
    const key = keyOf(path.basename(real));
    if (key === null) return { ok: false, why: "not a snapshot directory name" };
    if (!(await cookieSync())) return { ok: false, why: "watcher not current" };
    if (req.op === "begin") return { ok: true, generation: genOf(key), source: changeSource };
    if (req.op === "cargo-metadata") return cargoMetadataOp(req, key, real);
    if (req.op === "record" || req.op === "tree-record") {
      if (typeof req.tree !== "string" || !Number.isInteger(req.generation)) return { ok: false, why: "bad record" };
      if (genOf(key) !== req.generation) return { ok: false, why: "changed during the proof" };
      // With the helper, a proof is kept only for a snapshot it has listed (that listing is what an overflow is recovered from).
      if (!(await listed(key))) return { ok: false, why: "the change helper has no listing of this snapshot" };
      if (genOf(key) !== req.generation) return { ok: false, why: "changed during the proof" };
      // "tree-record": the commit's files are this tree (a proof that admitted stage outputs beside them). "record": that, and nothing else is there.
      if (req.op === "record") proven.set(key, { tree: req.tree, generation: req.generation });
      treeBase.set(key, { tree: req.tree, ledger: createChangeLedger() });
      return { ok: true };
    }
    if (req.op === "check") {
      const p = proven.get(key);
      const now = genOf(key);
      const clean = Boolean(p && p.tree === req.tree && p.generation === now);
      // `base`: the tree last proven here and what changed since, when that is known; absent otherwise (the caller hashes the tree).
      const tb = treeBase.get(key); const changes = tb && tb.tree === req.tree ? tb.ledger.list() : null;
      return { ok: true, clean, generation: now, source: changeSource, ...(changes ? { base: { tree: tb.tree, ...changes } } : {}) };
    }
    return { ok: false, why: "unknown op" };
  };
  async function cargoMetadataOp(req, key, snapshotDir) {
    const args = Array.isArray(req.args) ? req.args.map(String) : null;
    if (!args || args[0] !== "metadata") return { ok: false, why: "only cargo metadata is served" };
    if (typeof req.cargo !== "string" || !path.isAbsolute(req.cargo) || typeof req.cwd !== "string") return { ok: false, why: "bad cargo request" };
    let cwd; try { cwd = fs.realpathSync(req.cwd); } catch { return { ok: false, why: "no such cwd" }; }
    if (cwd !== snapshotDir && !cwd.startsWith(`${snapshotDir}${path.sep}`)) return { ok: false, why: "cwd outside the snapshot" };
    const env = req.env && typeof req.env === "object" ? Object.fromEntries(Object.entries(req.env).map(([k, v]) => [k, String(v)])) : {};
    let cargoId; try { const st = fs.statSync(req.cargo); cargoId = `${st.size}:${st.mtimeMs}:${st.ino}`; } catch { return { ok: false, why: "no such cargo" }; }
    const digest = crypto.createHash("sha256").update(JSON.stringify({ key, cwd, args, cargo: req.cargo, cargoId, env: Object.entries(env).sort() })).digest("hex");
    const hit = cargoAnswers.get(digest);
    const now = genOf(key);
    if (hit && hit.generation === now) return { ok: true, cached: true, status: hit.status, stdout: hit.stdout, stderr: hit.stderr };
    const before = now;
    const result = await new Promise((resolve) => {
      let child; try { child = spawn(req.cargo, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true }); } catch (error) { resolve({ status: -1, stdout: "", stderr: String(error?.message ?? error) }); return; }
      const out = []; const err = [];
      child.stdout.on("data", (d) => out.push(d)); child.stderr.on("data", (d) => err.push(d));
      child.on("error", (error) => resolve({ status: -1, stdout: "", stderr: String(error?.message ?? error) }));
      child.on("close", (status) => resolve({ status: status ?? -1, stdout: Buffer.concat(out).toString("utf8"), stderr: Buffer.concat(err).toString("utf8") }));
    });
    // Cached only when cargo succeeded and nothing in the snapshot changed while it ran (the cookie makes every earlier event visible first).
    if (result.status === 0 && (await cookieSync()) && genOf(key) === before) {
      cargoAnswers.set(digest, { key, generation: before, ...result });
      while (cargoAnswers.size > 64) cargoAnswers.delete(cargoAnswers.keys().next().value);
    }
    return { ok: true, cached: false, ...result };
  }
  const sock = listenOn ?? socketPath();
  // One server per host: a second one started in the same instant must not steal the socket from a live first one (seen 2026-11-05).
  const live = await new Promise((resolve) => { const c = net.connect(sock); c.on("connect", () => { c.destroy(); resolve(true); }); c.on("error", () => resolve(false)); });
  if (live) { log("another server already answers on the socket; exiting"); process.exit(0); }
  if (process.platform !== "win32") { fs.mkdirSync(path.dirname(sock), { recursive: true }); try { fs.unlinkSync(sock); } catch { /* none */ } }
  const server = net.createServer((conn) => {
    let buf = "";
    conn.on("data", async (d) => {
      buf += d;
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      let req = null; try { req = JSON.parse(buf.slice(0, nl)); } catch { req = null; }
      buf = "";
      let res; try { res = await handle(req); } catch (error) { res = { ok: false, why: String(error?.message ?? error) }; }
      conn.end(`${JSON.stringify(res)}\n`);
    });
    conn.on("error", () => {});
  });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(sock, resolve); });
  log(`serving ${watchedRoot} on ${sock} (pid ${process.pid}, identity ${IDENTITY})`);
  // Exit when idle, or as soon as the socket is no longer this server's (another server replaced it): no orphan lingers.
  let sockIno = null; try { sockIno = fs.statSync(sock).ino; } catch { sockIno = null; }
  const idle = setInterval(() => {
    if (Date.now() - lastRequest > idleMs) { log("idle; exiting"); process.exit(0); }
    if (process.platform !== "win32") { let ino = null; try { ino = fs.statSync(sock).ino; } catch { ino = null; } if (ino !== sockIno) { log("socket replaced; exiting"); process.exit(0); } }
  }, 10_000);
  idle.unref?.();
  return server;
}

// ---- client (synchronous: verifySnapshotDir is synchronous) ------------------------------------------------------------------------------

async function ask(req, timeoutMs) {
  return new Promise((resolve) => {
    const conn = net.connect(socketPath());
    let out = "";
    const timer = setTimeout(() => { conn.destroy(); resolve({ ok: false, why: "timeout" }); }, timeoutMs);
    conn.on("connect", () => conn.write(`${JSON.stringify({ schema: SCHEMA, identity: IDENTITY, ...req })}\n`));
    conn.on("data", (d) => { out += d; });
    conn.on("end", () => { clearTimeout(timer); try { resolve(JSON.parse(out)); } catch { resolve({ ok: false, why: "bad reply" }); } });
    conn.on("error", (error) => { clearTimeout(timer); resolve({ ok: false, why: error?.code ?? "error", unreachable: true }); });
  });
}

// The caller is synchronous (verifySnapshotDir), and Node has no synchronous socket. Until 2026-11-17 each question therefore started a whole
// `node gate-serve.mjs ask` process: six per check, against the contract's rule that the gate asks the resident gate over its socket in process
// (NN26, and NN07 for the repeats). The question is now sent by one worker THREAD of this process, kept for its life, while the caller waits on
// a shared flag: no process is started. The thread speaks the same line protocol as `ask` below.
const ASK_THREAD = `
const { parentPort } = require("node:worker_threads"); const net = require("node:net");
parentPort.on("message", ({ sock, line, timeoutMs, port, flag }) => {
  let out = ""; let done = false; let conn = null;
  const finish = (res) => { if (done) return; done = true; clearTimeout(timer); try { conn && conn.destroy(); } catch {} port.postMessage(res); port.close(); Atomics.store(flag, 0, 1); Atomics.notify(flag, 0); };
  const timer = setTimeout(() => finish({ ok: false, why: "timeout" }), timeoutMs);
  try {
    conn = net.connect(sock);
    conn.on("connect", () => conn.write(line + "\\n"));
    conn.on("data", (d) => { out += d; });
    conn.on("end", () => { try { finish(JSON.parse(out)); } catch { finish({ ok: false, why: "bad reply" }); } });
    conn.on("error", (error) => finish({ ok: false, why: (error && error.code) || "error", unreachable: true }));
  } catch (error) { finish({ ok: false, why: String((error && error.message) || error), unreachable: true }); }
});
`;
let askThread = null;
/** One request over the server's socket from this process, synchronously. Returns the server's reply, or { ok: false, why } as `ask` does. */
export function askInProcess(req, timeoutMs, { sock = socketPath() } = {}) {
  if (!askThread) { askThread = new Worker(ASK_THREAD, { eval: true }); askThread.unref(); askThread.on("error", () => { askThread = null; }); askThread.on("exit", () => { askThread = null; }); }
  const { port1, port2 } = new MessageChannel(); const flag = new Int32Array(new SharedArrayBuffer(4));
  askThread.postMessage({ sock, line: JSON.stringify({ schema: SCHEMA, identity: IDENTITY, ...req }), timeoutMs, port: port2, flag }, [port2]);
  Atomics.wait(flag, 0, 0, timeoutMs + 2000);
  const got = receiveMessageOnPort(port1); port1.close();
  return got?.message ?? { ok: false, why: "timeout" };
}

/** One synchronous request to the resident server; null when it is disabled, unreachable or did not answer. Starts it when absent. */
export function serveRequest(req, { env = process.env, root = null, timeoutMs = 3000, sock = socketPath(env) } = {}) {
  let res = null;
  try { res = askInProcess(req, timeoutMs, { sock }); }
  catch {
    // A host where a worker thread cannot be made: the old way, one process per question, rather than no resident proof at all.
    const r = spawnSync(process.execPath, [SELF, "ask", JSON.stringify(req), String(timeoutMs)], { encoding: "utf8", timeout: timeoutMs + 2000, maxBuffer: 1 << 28, env: { ...env } });
    try { res = JSON.parse(String(r.stdout ?? "").trim().split("\n").pop()); } catch { res = null; }
  }
  // Start a server only when none is listening; a bad reply or a timeout from a live one never starts a second.
  if (res?.unreachable && root) startServer({ root, env });
  return res && res.ok ? res : null;
}

/** Whether `root` is this host's own snapshot root (snapshot.mjs snapshotRoot: `<managed root>/snapshots` on Mac and Windows). */
export function isHostSnapshotRoot(root, { platform = process.platform } = {}) {
  if (platform !== "darwin" && platform !== "win32") return true; // elsewhere the root comes from the host's configuration file
  // Both hosts' file systems fold case: another spelling of the same root is the same root.
  try { return path.resolve(root).toLowerCase() === path.resolve(path.join(resolveManagedRoots({ env: {}, platform }).gateRoot, "snapshots")).toLowerCase(); } catch { return false; }
}

function startServer({ root, env }) {
  // One server per gate code, and it watches one root. A server started for any other directory (a test's temporary snapshot root) would hold
  // the name and answer every real snapshot "not a snapshot of the watched root" for as long as it is asked. Only the host's root starts one.
  if (!isHostSnapshotRoot(root)) return;
  try {
    const logDir = path.join(dataRoot(env), "gate-serve");
    fs.mkdirSync(logDir, { recursive: true });
    const out = fs.openSync(path.join(logDir, "serve.log"), "a");
    // A minimal environment: the server outlives the run that started it, so it must carry none of that run's tokens.
    const keep = ["PATH", "HOME", "USERPROFILE", "SYSTEMROOT", "SystemRoot", "TEMP", "TMP", "TMPDIR", "LOCALAPPDATA", "APPDATA", "RIGHTKIT_MANAGED_ROOT", "RK_GATE_SERVE_IDLE_MS"];
    const childEnv = Object.fromEntries(keep.filter((k) => env[k] !== undefined).map((k) => [k, env[k]]));
    if (process.platform === "win32") { fs.closeSync(out); startOutsideJobs({ root, childEnv, logDir, env }); return; }
    const child = spawn(process.execPath, [SELF, "serve", root], { detached: true, stdio: ["ignore", out, out], env: childEnv, cwd: path.dirname(SELF), windowsHide: true });
    child.unref();
    fs.closeSync(out);
  } catch { /* the next run tries again; this one verifies in full */ }
}

/**
 * Windows: a child of the gate stays in the gate's jobs whatever `detached` says (libuv never asks to break away), so a server spawned
 * here sat in the gate's job, where every release counted it and kept the lock (S0, S9, GC-T06), and died with the launcher's
 * kill-on-close job, so it was never resident (S14; a new server on every run, Dell 2026-11-07). WMI starts it outside both, with no
 * window. WMI passes no environment, so the server reads its environment from a file it removes once read (`serve-env`).
 */
function startOutsideJobs({ root, childEnv, logDir, env }) {
  const specFile = path.join(logDir, `serve-env-${crypto.randomUUID()}.json`);
  fs.writeFileSync(specFile, JSON.stringify({ root, env: childEnv, log: path.join(logDir, "serve.log") }), { flag: "wx", mode: 0o600 });
  const ps = (s) => `'${String(s).replace(/'/g, "''")}'`;
  const commandLine = `"${process.execPath}" "${SELF}" serve-env "${specFile}"`;
  const script = [
    "$ErrorActionPreference = 'Stop'",
    "$s = New-CimInstance -ClassName Win32_ProcessStartup -ClientOnly -Property @{ ShowWindow = [uint16]0 }",
    `$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = ${ps(commandLine)}; CurrentDirectory = ${ps(path.dirname(SELF))}; ProcessStartupInformation = $s }`,
    "exit [int]$r.ReturnValue",
  ].join("\n");
  const launcher = spawn("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], { stdio: "ignore", windowsHide: true, env });
  // A launch that failed leaves no server to remove the file.
  launcher.on("exit", (code) => { if (code !== 0) fs.rmSync(specFile, { force: true }); });
  launcher.on("error", () => fs.rmSync(specFile, { force: true }));
  launcher.unref();
}

/**
 * GC-A11 `cargo metadata` for a directory inside a gate snapshot, answered by the resident gate (cached per snapshot while it is unchanged), else
 * run directly with `cargo` (an absolute path) and `env`. Returns `{ status, stdout, stderr, served, cached }`. Never through the broker.
 */
export function snapshotCargoMetadata({ cwd, args, cargo, env, snapshotRootDir, run = spawnSync }) {
  let snapshotDir = null;
  try {
    const realRoot = fs.realpathSync(snapshotRootDir); const realCwd = fs.realpathSync(cwd);
    const rel = path.relative(realRoot, realCwd);
    const first = rel.split(path.sep)[0];
    if (rel && !rel.startsWith("..") && !path.isAbsolute(rel) && /^[0-9a-f]{16}$/.test(first)) snapshotDir = path.join(realRoot, first);
  } catch { snapshotDir = null; }
  if (snapshotDir && cargo) {
    const keep = Object.fromEntries(Object.entries(env ?? {}).filter(([k]) => /^(PATH|Path|HOME|USERPROFILE|SYSTEMROOT|SystemRoot|TMPDIR|TEMP|TMP|CARGO.*|RUSTUP.*|RUSTC.*|RUSTFLAGS|RUSTDOCFLAGS)$/.test(k)));
    const res = serveRequest({ op: "cargo-metadata", dir: snapshotDir, cwd, args, cargo, env: keep }, { env, root: snapshotRootDir, timeoutMs: 120_000 });
    if (res && Number.isInteger(res.status)) return { status: res.status, stdout: res.stdout ?? "", stderr: res.stderr ?? "", served: true, cached: res.cached === true };
  }
  const r = run(cargo ?? "cargo", args, { cwd, env, encoding: "utf8", maxBuffer: 1 << 28 });
  return { status: r.status, stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? ""), served: false, cached: false };
}

// ---- entry --------------------------------------------------------------------------------------------------------------------------------

// Importing this module must never throw: a `-e` helper (the gate's bounded probe) passes its payload as argv[1], not a file.
const isServeEntry = (() => { try { return Boolean(process.argv[1]) && fs.realpathSync(process.argv[1]) === fs.realpathSync(SELF); } catch { return false; } })();
if (isServeEntry) {
  const [cmd, a, b] = process.argv.slice(2);
  if (cmd === "serve") {
    serve({ root: a, log: (m) => console.log(`${new Date().toISOString()} gate-serve: ${m}`) }).catch((error) => { console.log(`${new Date().toISOString()} gate-serve: cannot serve: ${error?.message ?? error}`); process.exit(1); });
  } else if (cmd === "serve-env") {
    // The Windows start (startOutsideJobs): root, environment and log come from the file, removed once read.
    let spec = null; try { spec = JSON.parse(fs.readFileSync(a, "utf8")); } catch { spec = null; } finally { if (a) fs.rmSync(a, { force: true }); }
    if (typeof spec?.root !== "string" || typeof spec.log !== "string") process.exit(2);
    for (const k of Object.keys(process.env)) delete process.env[k];
    Object.assign(process.env, spec.env ?? {});
    const log = (m) => { try { fs.appendFileSync(spec.log, `${new Date().toISOString()} gate-serve: ${m}\n`); } catch { /* a log that cannot be written never stops the server */ } };
    serve({ root: spec.root, log }).catch((error) => { log(`cannot serve: ${error?.message ?? error}`); process.exit(1); });
  } else if (cmd === "ask") {
    let req = null; try { req = JSON.parse(a); } catch { req = null; }
    // Exit only after the reply is flushed: process.exit right after a large write to a pipe truncates it (a cargo metadata answer is megabytes).
    ask(req ?? {}, Number(b) || 3000).then((res) => { process.stdout.write(`${JSON.stringify(res)}\n`, () => process.exit(0)); });
  } else if (cmd === "status") {
    ask({ op: "status" }, 3000).then((res) => { console.log(JSON.stringify(res, null, 2)); process.exit(res.ok ? 0 : 1); });
  } else if (cmd === "stop") {
    // Exit 0 when a server answered and is stopping, or when none is listening (nothing to stop).
    ask({ op: "stop" }, 3000).then((res) => { console.log(JSON.stringify(res)); process.exit(res.ok || res.unreachable === true ? 0 : 1); });
  } else { console.error("usage: gate-serve.mjs serve <snapshot-root> | status | stop"); process.exit(2); }
}
