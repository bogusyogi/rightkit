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
// Lifecycle: started detached by the first client that finds no server (RK_GATE_SERVE=0 disables it); one server per host and data root;
// exits after RK_GATE_SERVE_IDLE_MS (default 30 min) without a request, or when a client runs different gate code. No system service is
// installed. Commands: `node gate-serve.mjs serve`, `node gate-serve.mjs status`, and the internal client ops below.
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { resolveManagedRoots } from "./managed-roots.mjs";

const SELF = fileURLToPath(import.meta.url);
const SCHEMA = "rightkit.gate-serve/1";
const COOKIE_PREFIX = "0000000000000000.gate-serve-cookie-";
const IDENTITY = (() => { try { return crypto.createHash("sha256").update(fs.readFileSync(SELF)).digest("hex").slice(0, 16); } catch { return "unknown"; } })();

function dataRoot(env = process.env) { return resolveManagedRoots({ env, platform: process.platform }).dataRoot; }
export function socketPath(env = process.env) {
  if (process.platform === "win32") return `\\\\.\\pipe\\rightkit-gate-serve-${(os.userInfo().username || "user").replace(/[^A-Za-z0-9_-]/g, "_")}`;
  return path.join(dataRoot(env), "gate-serve", "serve.sock");
}

// ---- server ---------------------------------------------------------------------------------------------------------------------------

export async function serve({ root, idleMs = Number(process.env.RK_GATE_SERVE_IDLE_MS ?? 30 * 60 * 1000), log = () => {} } = {}) {
  const watchedRoot = fs.realpathSync(root);
  const generation = new Map(); // snapshot key -> change count
  const proven = new Map(); // snapshot key -> { tree, generation }
  let watcher = null; let healthy = false; let cookieSeq = 0;
  const cookies = new Map(); // cookie name -> resolve
  // Only entries inside a snapshot directory (`<16 hex>/...`) are snapshot content; `<key>.tree-index`, `<key>.state.json` and the like
  // beside it are the gate's own metadata, and the cookie is `0000000000000000.gate-serve-cookie-*` (a name the snapshot root accepts).
  const keyOf = (rel) => { const base = String(rel ?? "").split(/[\\/]/)[0]; return /^[0-9a-f]{16}$/.test(base) ? base : null; };
  const bumpAll = () => { for (const k of new Set([...generation.keys(), ...proven.keys()])) generation.set(k, (generation.get(k) ?? 0) + 1); proven.clear(); };
  const startWatch = () => {
    try {
      watcher = fs.watch(watchedRoot, { recursive: true, persistent: true }, (_event, filename) => {
        if (filename == null) { bumpAll(); return; } // an event without a name: anything may have changed
        const name = String(filename);
        const base = name.split(/[\\/]/)[0];
        if (base.startsWith(COOKIE_PREFIX)) { const r = cookies.get(base); if (r) { cookies.delete(base); r(); } return; }
        const key = keyOf(name);
        // The watched root's own entry (its listing changed) carries no content: the entry that changed reports under its own name.
        if (key === null) { if (/^[0-9a-f]{16}\./.test(base) || name === path.basename(watchedRoot)) return; bumpAll(); return; } // gate metadata beside a snapshot; anything else: drop every proof
        generation.set(key, (generation.get(key) ?? 0) + 1);
        proven.delete(key);
      });
      watcher.on("error", (error) => { log(`watcher error: ${error?.message ?? error}; every proof dropped, restarting the watch`); healthy = false; bumpAll(); try { watcher.close(); } catch { /* closed */ } setTimeout(startWatch, 200); });
      healthy = true;
    } catch (error) { healthy = false; log(`cannot watch ${watchedRoot}: ${error?.message ?? error}`); setTimeout(startWatch, 1000); }
  };
  startWatch();
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
  const handle = async (req) => {
    lastRequest = Date.now();
    if (req?.schema !== SCHEMA) return { ok: false, why: "schema" };
    if (req.identity !== IDENTITY) { setTimeout(() => process.exit(0), 50); return { ok: false, why: "gate code changed; the server exits" }; }
    if (req.op === "status") return { ok: true, healthy, root: watchedRoot, proven: [...proven].map(([k, v]) => ({ key: k, tree: v.tree })), pid: process.pid, identity: IDENTITY };
    const dir = typeof req.dir === "string" ? req.dir : "";
    let real; try { real = fs.realpathSync(dir); } catch { return { ok: false, why: "no such directory" }; }
    if (path.dirname(real) !== watchedRoot) return { ok: false, why: "not a snapshot of the watched root" };
    const key = keyOf(path.basename(real));
    if (key === null) return { ok: false, why: "not a snapshot directory name" };
    if (!(await cookieSync())) return { ok: false, why: "watcher not current" };
    if (req.op === "begin") return { ok: true, generation: generation.get(key) ?? 0 };
    if (req.op === "record") {
      if (typeof req.tree !== "string" || !Number.isInteger(req.generation)) return { ok: false, why: "bad record" };
      if ((generation.get(key) ?? 0) !== req.generation) return { ok: false, why: "changed during the proof" };
      proven.set(key, { tree: req.tree, generation: req.generation });
      return { ok: true };
    }
    if (req.op === "check") {
      const p = proven.get(key);
      const now = generation.get(key) ?? 0;
      const clean = Boolean(p && p.tree === req.tree && p.generation === now);
      return { ok: true, clean, generation: now };
    }
    return { ok: false, why: "unknown op" };
  };
  const sock = socketPath();
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
  const idle = setInterval(() => { if (Date.now() - lastRequest > idleMs) { log("idle; exiting"); process.exit(0); } }, 10_000);
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

/** One synchronous request to the resident server; null when it is disabled, unreachable or did not answer. Starts it when absent. */
export function serveRequest(req, { env = process.env, root = null, timeoutMs = 3000 } = {}) {
  if (env.RK_GATE_SERVE === "0") return null;
  const r = spawnSync(process.execPath, [SELF, "ask", JSON.stringify(req), String(timeoutMs)], { encoding: "utf8", timeout: timeoutMs + 2000, env: { ...env } });
  let res = null; try { res = JSON.parse(String(r.stdout ?? "").trim().split("\n").pop()); } catch { res = null; }
  if ((!res || res.unreachable) && root) startServer({ root, env });
  return res && res.ok ? res : null;
}

function startServer({ root, env }) {
  try {
    const logDir = path.join(dataRoot(env), "gate-serve");
    fs.mkdirSync(logDir, { recursive: true });
    const out = fs.openSync(path.join(logDir, "serve.log"), "a");
    // A minimal environment: the server outlives the run that started it, so it must carry none of that run's tokens (the build
    // ownership guard rightly refuses a run while any process still holds an earlier run's RIGHTKIT_BUILD_OWNERSHIP).
    const keep = ["PATH", "HOME", "USERPROFILE", "SYSTEMROOT", "SystemRoot", "TEMP", "TMP", "TMPDIR", "LOCALAPPDATA", "APPDATA", "RIGHTKIT_MANAGED_ROOT", "RK_GATE_SERVE_IDLE_MS"];
    const childEnv = Object.fromEntries(keep.filter((k) => env[k] !== undefined).map((k) => [k, env[k]]));
    const child = spawn(process.execPath, [SELF, "serve", root], { detached: true, stdio: ["ignore", out, out], env: childEnv, cwd: path.dirname(SELF), windowsHide: true });
    child.unref();
    fs.closeSync(out);
  } catch { /* the next run tries again; this one verifies in full */ }
}

// ---- entry --------------------------------------------------------------------------------------------------------------------------------

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(SELF)) {
  const [cmd, a, b] = process.argv.slice(2);
  if (cmd === "serve") {
    serve({ root: a, log: (m) => console.log(`${new Date().toISOString()} gate-serve: ${m}`) }).catch((error) => { console.log(`${new Date().toISOString()} gate-serve: cannot serve: ${error?.message ?? error}`); process.exit(1); });
  } else if (cmd === "ask") {
    let req = null; try { req = JSON.parse(a); } catch { req = null; }
    ask(req ?? {}, Number(b) || 3000).then((res) => { process.stdout.write(`${JSON.stringify(res)}\n`); process.exit(0); });
  } else if (cmd === "status") {
    ask({ op: "status" }, 3000).then((res) => { console.log(JSON.stringify(res, null, 2)); process.exit(res.ok ? 0 : 1); });
  } else { console.error("usage: gate-serve.mjs serve <snapshot-root> | status"); process.exit(2); }
}
