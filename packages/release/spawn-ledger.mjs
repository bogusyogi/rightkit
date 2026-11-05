// The gate's spawn ledger (GATE-CONTRACT GC-A27 NN07-NN12, NN16): when RK_GATE_SPAWN_LEDGER names a file, every child process this
// Node process starts is appended to it as one JSON line {pid, at, ms, sync, cmd, args, cwd, status}. The acceptance runner reads it to
// prove a run starts no identical process twice, no `cargo fmt`, no broker-carried non-compiling cargo and no repeat install.
// The variable is RK_GATE_*, which no cache key reads (bin/rightkit-gate.mjs BUILD_ENV_EXCLUDED), so turning it on changes no result.
// The environment is never recorded. Children inherit the variable, so a nested gate or right-release that installs the ledger appends too.
import childProcess from "node:child_process";
import fs from "node:fs";
import module from "node:module";

let installed = false;

/** Patch node:child_process once so every caller (default or named import) is recorded. No-op without RK_GATE_SPAWN_LEDGER. */
export function installSpawnLedger(env = process.env) {
  const file = env.RK_GATE_SPAWN_LEDGER;
  if (installed || typeof file !== "string" || file === "") return false;
  installed = true;
  const write = (entry) => { try { fs.appendFileSync(file, `${JSON.stringify(entry)}\n`); } catch { /* the ledger never fails a run */ } };
  const describe = (cmd, args, options) => {
    const argv = Array.isArray(args) ? args.map(String) : [];
    const opts = Array.isArray(args) ? options : args;
    return { cmd: String(cmd), args: argv.join("\u0000").length > 4000 ? [...argv.slice(0, 40), `... ${argv.length} args`] : argv, cwd: typeof opts?.cwd === "string" ? opts.cwd : process.cwd() };
  };
  for (const name of ["spawnSync", "execFileSync", "execSync"]) {
    const orig = childProcess[name];
    childProcess[name] = function ledgered(cmd, args, options) {
      const started = Date.now();
      let status = null;
      try { const r = orig.apply(this, arguments); status = r && typeof r === "object" && "status" in r ? r.status : 0; return r; }
      catch (error) { status = error?.status ?? "throw"; throw error; }
      finally { write({ pid: process.pid, at: new Date(started).toISOString(), ms: Date.now() - started, sync: true, ...describe(cmd, name === "execSync" ? [] : args, name === "execSync" ? args : options), status }); }
    };
  }
  for (const name of ["spawn", "execFile", "exec", "fork"]) {
    const orig = childProcess[name];
    childProcess[name] = function ledgered(cmd, args, options) {
      const started = Date.now();
      const child = orig.apply(this, arguments);
      const base = { pid: process.pid, at: new Date(started).toISOString(), sync: false, ...describe(name === "fork" ? process.execPath : cmd, name === "fork" ? [cmd, ...(Array.isArray(args) ? args : [])] : name === "exec" ? [] : args, name === "exec" ? args : options) };
      if (child && typeof child.once === "function") child.once("close", (code, signal) => write({ ...base, ms: Date.now() - started, status: code ?? signal }));
      else write({ ...base, ms: null, status: null });
      return child;
    };
  }
  module.syncBuiltinESMExports();
  return true;
}
