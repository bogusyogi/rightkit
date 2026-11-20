// The gate's spawn ledger (GATE-CONTRACT GC-A27 NN07-NN12, NN16): when RK_GATE_SPAWN_LEDGER names a file, every child process this
// Node process starts is appended to it as one JSON line {pid, child, at, ms, sync, cmd, args, cwd, status}. The acceptance runner reads it to
// prove a run starts no identical process twice, no `cargo fmt`, no broker-carried non-compiling cargo and no repeat install.
// The variable is RK_GATE_*, which no cache key reads (bin/rightkit-gate.mjs BUILD_ENV_EXCLUDED), so turning it on changes no result.
// The environment is never recorded. Children inherit the variable, so a nested gate or right-release that installs the ledger appends too.
//
// Beside it, `<ledger>.journal.jsonl` is the process journal the twelve-line judge reads (rightkit#1514 step 3): a `start` line when a child
// starts (before it can end), an `end` line when it closes, and one `exit` line per writer when its process exits, naming the writer that
// started it (`parent`, from RK_GATE_LEDGER_PARENT) and how many of its children were still open. A child started and never closed (a killed
// run) shows as a start with no end. `mono` is process.hrtime.bigint(), one monotonic clock for every process of the host.
import childProcess from "node:child_process";
import { createHash } from "node:crypto";
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
const mono = () => process.hrtime.bigint().toString();
// A cargo test program (a file in a `deps` folder) is hashed as it starts, so the judge can hold it to the digest the broker signed (NN10).
const programDigest = (cmd) => { if (!/[\\/]deps[\\/][^\\/]+$/.test(String(cmd))) return undefined; try { return createHash("sha256").update(fs.readFileSync(cmd)).digest("hex"); } catch { return null; } };

/** Patch node:child_process once so every caller (default or named import) is recorded. No-op without RK_GATE_SPAWN_LEDGER. */
export function installSpawnLedger(env = process.env) {
  const file = env.RK_GATE_SPAWN_LEDGER;
  if (installed || typeof file !== "string" || file === "") return false;
  installed = true;
  const append = (f, entry) => { try { fs.appendFileSync(f, `${JSON.stringify(entry)}\n`); } catch { /* the ledger never fails a run */ } };
  const write = (entry) => append(file, entry);
  const journal = (entry) => append(`${file}.journal.jsonl`, { ...entry, pid: process.pid, mono: mono() });
  // The writer that started this process, and this writer for the children it starts (they inherit the environment).
  const parent = /^\d+$/.test(String(env.RK_GATE_LEDGER_PARENT ?? "")) ? Number(env.RK_GATE_LEDGER_PARENT) : null;
  const root = String(process.ppid) === String(env.RK_GATE_LAUNCHER_PID ?? "");
  try { process.env.RK_GATE_LEDGER_PARENT = String(process.pid); } catch { /* read-only environment: children cannot name their parent */ }
  const open = new Set();
  process.once("exit", (code) => journal({ ev: "exit", parent, root, open: open.size, exit: code, at: new Date().toISOString() }));
  const describe = (cmd, args, options) => {
    const raw = Array.isArray(args) ? args.map(String) : [];
    const { cmd: logical, args: argv } = unwrapCmdShim(cmd, raw);
    const opts = Array.isArray(args) ? options : args;
    return { cmd: logical, args: argv.join("\u0000").length > 4000 ? [...argv.slice(0, 40), `... ${argv.length} args`] : argv, cwd: typeof opts?.cwd === "string" ? opts.cwd : process.cwd() };
  };
  let token = 0;
  for (const name of ["spawnSync", "execFileSync", "execSync"]) {
    const orig = childProcess[name];
    childProcess[name] = function ledgered(cmd, args, options) {
      const started = Date.now(); const id = `s${(token += 1)}`;
      const what = describe(cmd, name === "execSync" ? [] : args, name === "execSync" ? args : options);
      journal({ ev: "start", id, child: null, sync: true, at: new Date(started).toISOString(), ...what, sha256: programDigest(what.cmd) });
      let status = null; let child = null;
      try { const r = orig.apply(this, arguments); status = r && typeof r === "object" && "status" in r ? r.status : 0; child = r && typeof r === "object" && Number.isInteger(r.pid) ? r.pid : null; return r; }
      catch (error) { status = error?.status ?? "throw"; throw error; }
      finally { write({ pid: process.pid, child, at: new Date(started).toISOString(), ms: Date.now() - started, sync: true, ...what, status }); journal({ ev: "end", id, child, status }); }
    };
  }
  for (const name of ["spawn", "execFile", "exec", "fork"]) {
    const orig = childProcess[name];
    childProcess[name] = function ledgered(cmd, args, options) {
      const started = Date.now(); const id = `a${(token += 1)}`;
      const child = orig.apply(this, arguments);
      const pidOf = Number.isInteger(child?.pid) ? child.pid : null;
      const base = { pid: process.pid, child: pidOf, at: new Date(started).toISOString(), sync: false, ...describe(name === "fork" ? process.execPath : cmd, name === "fork" ? [cmd, ...(Array.isArray(args) ? args : [])] : name === "exec" ? [] : args, name === "exec" ? args : options) };
      journal({ ev: "start", id, child: pidOf, sync: false, at: base.at, cmd: base.cmd, args: base.args, cwd: base.cwd, sha256: programDigest(base.cmd) });
      if (child && typeof child.once === "function") {
        open.add(id);
        child.once("close", (code, signal) => { open.delete(id); write({ ...base, ms: Date.now() - started, status: code ?? signal }); journal({ ev: "end", id, child: pidOf, status: code ?? signal }); });
      } else write({ ...base, ms: null, status: null });
      return child;
    };
  }
  module.syncBuiltinESMExports();
  return true;
}
