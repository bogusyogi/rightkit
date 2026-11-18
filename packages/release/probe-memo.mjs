// One answer per question, per gate process (GATE-CONTRACT NN07: no identical child process twice in one run).
//
// Many parts of the gate ask git the same fixed questions about the same checkout: its top level, its git directory, its object format, whether a
// commit id names a commit, the origin URL. Each part asked for itself. Measured on the Dell, 2026-11-17, one `--to check` of the witness fixture
// in ONE process: 162 child processes, 74 of them repeats of an earlier identical one (git rev-parse --show-toplevel 8 times, --show-object-format
// 9 times, `where rustright` 7 times), about 5.9 s of a 9 s check.
//
// installProbeMemo wraps the synchronous starters of node:child_process. A call is answered from memory only when ALL of this holds:
//   - it is one of the questions listed in `pureQuestion`, whose answer cannot change while a gate process runs;
//   - an identical call (program, arguments, working directory, encoding, the git variables of its environment) already SUCCEEDED in this process.
// A failed call is never remembered (the thing asked about may appear later: an ephemeral commit, a tool being installed). Anything not listed runs
// as before. The memo sits above the spawn ledger, so a remembered answer starts no process and writes no ledger line.
import childProcess from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import module from "node:module";
import path from "node:path";

const SHA = /^[0-9a-f]{40}([0-9a-f]{24})?$/;
const REV_FLAGS = new Set(["--show-toplevel", "--absolute-git-dir", "--show-object-format", "--git-common-dir", "--git-dir"]);

/** Whether `cmd args` is a question whose answer is fixed for the life of a gate process. Exported for its test. */
export function pureQuestion(cmd, args) {
  if (typeof cmd !== "string" || !Array.isArray(args) || !args.every((a) => typeof a === "string")) return false;
  const name = path.basename(cmd.replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
  if (name === "where" || name === "which") return args.length === 1 && /^[A-Za-z0-9._-]+$/.test(args[0]);
  // A broker job's status, by its id: remembered only once the answer is final (see FINAL_ONLY), never while the job may still change.
  if (name === "rustright") return args.length === 2 && args[0] === "status" && /^[0-9a-f-]{36}$/.test(args[1]);
  if (name !== "git") return false;
  // Leading `-c key=value` pairs configure the call, they are part of the question (and of the key), not a different question.
  let rest = args; while (rest[0] === "-c" && typeof rest[1] === "string" && rest[1].includes("=")) rest = rest.slice(2);
  if (rest[0] === "rev-parse") {
    const tail = rest.slice(1).filter((a) => a !== "--path-format=absolute");
    if (tail.length === 1 && REV_FLAGS.has(tail[0])) return true;
    // Does this id name a commit, or a tree? An object id is content: once it resolves, it always does.
    if (tail.length === 3 && tail[0] === "--verify" && tail[1] === "--quiet") { const m = /^([0-9a-f]+)\^\{(commit|tree)\}$/.exec(tail[2]); return Boolean(m && SHA.test(m[1])); }
    return false;
  }
  if (rest[0] === "config") return rest.length === 3 && rest[1] === "--get" && rest[2] === "remote.origin.url";
  // A file of a commit or tree named by its full id: content-addressed, the same bytes for ever (NN07: `git show <commit>:rk/Cargo.toml` 16 times in
  // one compile of the witness fixture, Dell, 2026-11-17).
  if (rest[0] === "show") return rest.length === 2 && /^[0-9a-f]{40}([0-9a-f]{24})?:[^\0]+$/.test(rest[1]) && !rest[1].includes("..");
  // The file list of a tree named by its full id, names only or with modes and ids (snapshot.mjs treeEntries).
  if (rest[0] === "ls-tree") return treeListOf(args) !== null;
  return false;
}

/** The `ls-tree` question of `args` with its leading `-c` pairs dropped, when it lists a whole tree named by its full id; else null. */
function treeListOf(args) {
  let rest = args; while (rest[0] === "-c" && typeof rest[1] === "string" && rest[1].includes("=")) rest = rest.slice(2);
  if (rest[0] !== "ls-tree" || !SHA.test(rest[rest.length - 1] ?? "")) return null;
  const flags = rest.slice(1, -1).join(" ");
  return flags === "-r --name-only -z" || flags === "-r -z --full-tree" ? rest : null;
}

/**
 * NN22: the file list of a commit named by its full id is the same for ever, so a gate process need not list it again when an earlier one did.
 * Kept on disk under `storeDir` (one file per question, by working directory and the question without its `-c` pairs, which do not change a
 * `-z` listing), only for the two whole-tree listings above. Every warm run listed the witness fixture's commit twice (`ls-tree -r --name-only
 * -z` and the sealed `ls-tree -r -z --full-tree`, Dell, 2026-11-17).
 */
export function treeListStore(storeDir, { maxEntries = 256 } = {}) {
  const store = contentStore(storeDir, { maxEntries });
  if (!store) return null;
  const keyOfList = (args, cwd) => { const q = treeListOf(args); return q ? JSON.stringify([path.resolve(cwd ?? process.cwd()), q]) : null; };
  return {
    get(args, cwd) { const k = keyOfList(args, cwd); return k ? store.get(k) : null; },
    put(args, cwd, bytes) { const k = keyOfList(args, cwd); if (k) store.put(k, bytes); },
  };
}

/**
 * Bytes kept on disk under `storeDir` by a string key (one file per key, named by its sha256), for answers that cannot change: a tree listing of a
 * full object id. Each file starts with the sha256 of the bytes it holds; a file whose bytes do not match (truncated, half-written, edited) is
 * never returned. The store is the gate user's own state under the gate runs root, the same trust as the result cache beside it.
 */
export function contentStore(storeDir, { maxEntries = 256 } = {}) {
  if (typeof storeDir !== "string" || !storeDir) return null;
  const fileOf = (key) => path.join(storeDir, `${createHash("sha256").update(String(key)).digest("hex")}.bin`);
  return {
    get(key) {
      try {
        const raw = fs.readFileSync(fileOf(key)); const body = raw.subarray(65);
        return raw.length >= 65 && raw[64] === 0x0a && raw.subarray(0, 64).toString("latin1") === createHash("sha256").update(body).digest("hex") ? Buffer.from(body) : null;
      } catch { return null; }
    },
    put(key, bytes) {
      if (!Buffer.isBuffer(bytes)) return;
      const f = fileOf(key);
      try {
        fs.mkdirSync(storeDir, { recursive: true });
        const tmp = `${f}.${process.pid}.tmp`; fs.writeFileSync(tmp, Buffer.concat([Buffer.from(`${createHash("sha256").update(bytes).digest("hex")}\n`, "latin1"), bytes])); fs.renameSync(tmp, f);
        const names = fs.readdirSync(storeDir).filter((n) => n.endsWith(".bin"));
        if (names.length > maxEntries) {
          const aged = names.map((n) => { try { return [n, fs.statSync(path.join(storeDir, n)).mtimeMs]; } catch { return [n, 0]; } }).sort((a, b) => a[1] - b[1]);
          for (const [n] of aged.slice(0, names.length - maxEntries)) fs.rmSync(path.join(storeDir, n), { force: true });
        }
      } catch { /* a store that cannot be written only means the next run lists again */ }
    },
  };
}
const asEncoded = (bytes, encoding) => (encoding && encoding !== "buffer" ? bytes.toString(encoding) : Buffer.from(bytes));

/** Whether a successful answer may be kept: always for a git question; for `rustright status` only when the receipt records the job's end. */
export const FINAL_ONLY = (cmd, out) => {
  const name = path.basename(String(cmd).replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
  if (name !== "rustright") return true;
  try { const doc = JSON.parse(Buffer.isBuffer(out) ? out.toString("utf8") : String(out)); const r = doc?.receipt ?? doc; return typeof r?.terminalAt === "string" && r.terminalAt.length > 0; } catch { return false; }
};

const keyOf = (cmd, args, options) => {
  // The git variables the child sees: its own env's, or this process's when it inherits (NN25: a call with an env and one without asked the same question twice).
  const env = Object.entries(options?.env && typeof options.env === "object" ? options.env : process.env).filter(([k]) => /^GIT_/i.test(k)).sort();
  return JSON.stringify([cmd, args, typeof options?.cwd === "string" ? options.cwd : process.cwd(), options?.encoding ?? null, options?.input === undefined ? null : "has-input", env]);
};
const copy = (v) => (Buffer.isBuffer(v) ? Buffer.from(v) : v);

let installed = false;
/**
 * Wrap spawnSync and execFileSync once. Returns the counters object ({ asked, answered, stored }) so a caller can report them; null when already
 * installed. `storeDir`: where whole-tree listings of a full commit id are kept between processes (treeListStore); none when absent.
 */
export function installProbeMemo({ storeDir = null } = {}) {
  if (installed) return null;
  installed = true;
  const stats = { asked: 0, answered: 0, stored: 0 };
  const store = treeListStore(storeDir);
  const seenSpawn = new Map(); const seenExec = new Map();
  const spawnSync = childProcess.spawnSync;
  childProcess.spawnSync = function memoSpawnSync(cmd, args, options) {
    const opts = Array.isArray(args) ? options : args; const argv = Array.isArray(args) ? args : [];
    if (opts?.input !== undefined || opts?.shell || !pureQuestion(cmd, argv)) return spawnSync.apply(this, arguments);
    stats.asked += 1; const key = keyOf(cmd, argv, opts); const hit = seenSpawn.get(key);
    if (hit) { stats.answered += 1; const stdout = copy(hit.stdout); const stderr = copy(hit.stderr); return { ...hit, stdout, stderr, output: [null, stdout, stderr] }; }
    const kept = store?.get(argv, opts?.cwd);
    if (kept) {
      stats.stored += 1; const stdout = asEncoded(kept, opts?.encoding); const stderr = asEncoded(Buffer.alloc(0), opts?.encoding);
      const r = { pid: 0, status: 0, signal: null, stdout, stderr, output: [null, stdout, stderr] };
      seenSpawn.set(key, { ...r, stdout: copy(stdout), stderr: copy(stderr) }); return r;
    }
    const r = spawnSync.apply(this, arguments);
    if (r && !r.error && r.status === 0 && r.signal === null && FINAL_ONLY(cmd, r.stdout)) {
      seenSpawn.set(key, { ...r, stdout: copy(r.stdout), stderr: copy(r.stderr) });
      if (store) store.put(argv, opts?.cwd, Buffer.isBuffer(r.stdout) ? r.stdout : Buffer.from(String(r.stdout ?? ""), opts?.encoding && opts.encoding !== "buffer" ? opts.encoding : "utf8"));
    }
    return r;
  };
  const execFileSync = childProcess.execFileSync;
  childProcess.execFileSync = function memoExecFileSync(cmd, args, options) {
    const opts = Array.isArray(args) ? options : args; const argv = Array.isArray(args) ? args : [];
    if (opts?.input !== undefined || opts?.shell || !pureQuestion(cmd, argv)) return execFileSync.apply(this, arguments);
    stats.asked += 1; const key = keyOf(cmd, argv, opts);
    if (seenExec.has(key)) { stats.answered += 1; return copy(seenExec.get(key)); }
    const kept = store?.get(argv, opts?.cwd);
    if (kept) { stats.stored += 1; const out = asEncoded(kept, opts?.encoding); seenExec.set(key, copy(out)); return out; }
    const out = execFileSync.apply(this, arguments); // throws on failure: a failure is never remembered
    if (FINAL_ONLY(cmd, out)) {
      seenExec.set(key, copy(out));
      if (store) store.put(argv, opts?.cwd, Buffer.isBuffer(out) ? out : Buffer.from(String(out), opts?.encoding && opts.encoding !== "buffer" ? opts.encoding : "utf8"));
    }
    return out;
  };
  module.syncBuiltinESMExports();
  return stats;
}
