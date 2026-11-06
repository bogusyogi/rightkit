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
// The server's identity covers every local module it runs (this file and managed-roots.mjs, its only local import), so a client never
// takes answers from a server whose code differs from its own, e.g. one started from an edited checkout answering a sealed gate.
const SERVER_MODULES = [SELF, fileURLToPath(new URL("./managed-roots.mjs", import.meta.url))];
const IDENTITY = (() => { try { const h = crypto.createHash("sha256"); for (const f of SERVER_MODULES) { h.update(path.basename(f)); h.update("\0"); h.update(fs.readFileSync(f)); h.update("\0"); } return h.digest("hex").slice(0, 16); } catch { return "unknown"; } })();

function dataRoot(env = process.env) { return resolveManagedRoots({ env, platform: process.platform }).gateRoot; }
export function socketPath(env = process.env) {
  if (process.platform === "win32") return `\\\\.\\pipe\\rightkit-gate-serve-${(os.userInfo().username || "user").replace(/[^A-Za-z0-9_-]/g, "_")}`;
  return path.join(dataRoot(env), "gate-serve", "serve.sock");
}

// ---- server ---------------------------------------------------------------------------------------------------------------------------

export async function serve({ root, idleMs = Number(process.env.RK_GATE_SERVE_IDLE_MS ?? 30 * 60 * 1000), log = () => {} } = {}) {
  const watchedRoot = fs.realpathSync(root);
  const generation = new Map(); // snapshot key -> change count
  const proven = new Map(); // snapshot key -> { tree, generation }
  // GC-A11 non-compiling cargo answers (cargo metadata), keyed on the request and the snapshot's change count: an unchanged snapshot is answered
  // from memory, any change to it (or a restart) runs cargo again. Bounded: oldest dropped past 64 entries.
  const cargoAnswers = new Map(); // request digest -> { key, generation, status, stdout, stderr }
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
    if (req.op === "cargo-metadata") return cargoMetadataOp(req, key, real);
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
    const now = generation.get(key) ?? 0;
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
    if (result.status === 0 && (await cookieSync()) && (generation.get(key) ?? 0) === before) {
      cargoAnswers.set(digest, { key, generation: before, ...result });
      while (cargoAnswers.size > 64) cargoAnswers.delete(cargoAnswers.keys().next().value);
    }
    return { ok: true, cached: false, ...result };
  }
  const sock = socketPath();
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

/** One synchronous request to the resident server; null when it is disabled, unreachable or did not answer. Starts it when absent. */
export function serveRequest(req, { env = process.env, root = null, timeoutMs = 3000 } = {}) {
  if (env.RK_GATE_SERVE === "0") return null;
  const r = spawnSync(process.execPath, [SELF, "ask", JSON.stringify(req), String(timeoutMs)], { encoding: "utf8", timeout: timeoutMs + 2000, maxBuffer: 1 << 28, env: { ...env } });
  let res = null; try { res = JSON.parse(String(r.stdout ?? "").trim().split("\n").pop()); } catch { res = null; }
  // Start a server only when none is listening; a bad reply or a timeout from a live one never starts a second.
  if (res?.unreachable && root) startServer({ root, env });
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
    if (process.platform === "win32") { fs.closeSync(out); startOutsideJobs({ root, childEnv, logDir, env }); return; }
    const child = spawn(process.execPath, [SELF, "serve", root], { detached: true, stdio: ["ignore", out, out], env: childEnv, cwd: path.dirname(SELF), windowsHide: true });
    child.unref();
    fs.closeSync(out);
  } catch { /* the next run tries again; this one verifies in full */ }
}

/**
 * Windows: a child of the gate stays in the gate's jobs whatever `detached` says (libuv never asks to break away), so a server spawned
 * here sat in the build-ownership job, where every release counted it and kept the record (S0, S9, GC-T06), and died with the launcher's
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

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(SELF)) {
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
  } else { console.error("usage: gate-serve.mjs serve <snapshot-root> | status"); process.exit(2); }
}
