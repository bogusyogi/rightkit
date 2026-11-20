// The gate's spawn ledger (GATE-CONTRACT GC-A27 NN07-NN12, NN16): when RK_GATE_SPAWN_LEDGER names a file, every child process this
// Node process starts is appended to it as one JSON line {pid, at, ms, sync, cmd, args, cwd, status}. The acceptance runner reads it to
// prove a run starts no identical process twice, no `cargo fmt`, no broker-carried non-compiling cargo and no repeat install.
// The variable is RK_GATE_*, which no cache key reads (bin/rightkit-gate.mjs BUILD_ENV_EXCLUDED), so turning it on changes no result.
// The environment is never recorded. Children inherit the variable, so a nested gate or right-release that installs the ledger appends too.
import childProcess from "node:child_process";
import fs from "node:fs";
import module from "node:module";

/**
 * A Windows shell-shim start as the process it really starts: `cmd.exe /d /s /c ""<shim>" <args>"` (the only form the gate
 * builds, deterministic-scans.mjs resolveInvocation: every argument matched SAFE_SHIM_ARG, so none holds a space and the
 * split is exact) records as `<shim>` with its arguments, so the acceptance judges see the rustright/cargo/pnpm start the
 * shim carried instead of an opaque cmd.exe string (NN40 saw no `cargo doc` start on Dell although six ran, run
 * muse-fulldev-4). Anything else is returned unchanged. Exported for its test.
 */
export function unwrapCmdShim(cmd, args) {
  if (!/(^|[\\/])cmd\.exe$/i.test(String(cmd)) || !Array.isArray(args)) return { cmd: String(cmd), args };
  const at = args.findIndex((a) => /^\/c$/i.test(String(a)));
  if (at < 0 || at + 1 >= args.length) return { cmd: String(cmd), args };
  let line = String(args[at + 1]);
  if (line.startsWith('""') && line.endsWith('"') && line.length >= 3) line = line.slice(1, -1);
  const m = /^"([^"]+)"(?:\s+(.*))?$/.exec(line) ?? /^(\S+)(?:\s+(.*))?$/.exec(line);
  if (!m) return { cmd: String(cmd), args };
  return { cmd: m[1], args: m[2] ? m[2].split(/\s+/).filter(Boolean) : [] };
}

let installed = false;

/** Patch node:child_process once so every caller (default or named import) is recorded. No-op without RK_GATE_SPAWN_LEDGER. */
export function installSpawnLedger(env = process.env) {
  const file = env.RK_GATE_SPAWN_LEDGER;
  if (installed || typeof file !== "string" || file === "") return false;
  installed = true;
  const write = (entry) => { try { fs.appendFileSync(file, `${JSON.stringify(entry)}\n`); } catch { /* the ledger never fails a run */ } };
  const describe = (cmd, args, options) => {
    const raw = Array.isArray(args) ? args.map(String) : [];
    const { cmd: logical, args: argv } = unwrapCmdShim(cmd, raw);
    const opts = Array.isArray(args) ? options : args;
    return { cmd: logical, args: argv.join("\u0000").length > 4000 ? [...argv.slice(0, 40), `... ${argv.length} args`] : argv, cwd: typeof opts?.cwd === "string" ? opts.cwd : process.cwd() };
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
