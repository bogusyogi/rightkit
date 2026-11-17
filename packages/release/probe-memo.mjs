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
import module from "node:module";
import path from "node:path";

const SHA = /^[0-9a-f]{40}([0-9a-f]{24})?$/;
const REV_FLAGS = new Set(["--show-toplevel", "--absolute-git-dir", "--show-object-format", "--git-common-dir", "--git-dir"]);

/** Whether `cmd args` is a question whose answer is fixed for the life of a gate process. Exported for its test. */
export function pureQuestion(cmd, args) {
  if (typeof cmd !== "string" || !Array.isArray(args) || !args.every((a) => typeof a === "string")) return false;
  const name = path.basename(cmd.replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
  if (name === "where" || name === "which") return args.length === 1 && /^[A-Za-z0-9._-]+$/.test(args[0]);
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
  // The file list of a tree named by its full id.
  if (rest[0] === "ls-tree") return rest.length === 5 && rest[1] === "-r" && rest[2] === "--name-only" && rest[3] === "-z" && SHA.test(rest[4]);
  return false;
}

const keyOf = (cmd, args, options) => {
  const env = options?.env && typeof options.env === "object" ? Object.entries(options.env).filter(([k]) => /^GIT_/i.test(k)).sort() : null;
  return JSON.stringify([cmd, args, typeof options?.cwd === "string" ? options.cwd : process.cwd(), options?.encoding ?? null, options?.input === undefined ? null : "has-input", env]);
};
const copy = (v) => (Buffer.isBuffer(v) ? Buffer.from(v) : v);

let installed = false;
/** Wrap spawnSync and execFileSync once. Returns the counters object ({ asked, answered }) so a caller can report them; null when already installed. */
export function installProbeMemo() {
  if (installed) return null;
  installed = true;
  const stats = { asked: 0, answered: 0 };
  const seenSpawn = new Map(); const seenExec = new Map();
  const spawnSync = childProcess.spawnSync;
  childProcess.spawnSync = function memoSpawnSync(cmd, args, options) {
    const opts = Array.isArray(args) ? options : args; const argv = Array.isArray(args) ? args : [];
    if (opts?.input !== undefined || opts?.shell || !pureQuestion(cmd, argv)) return spawnSync.apply(this, arguments);
    stats.asked += 1; const key = keyOf(cmd, argv, opts); const hit = seenSpawn.get(key);
    if (hit) { stats.answered += 1; const stdout = copy(hit.stdout); const stderr = copy(hit.stderr); return { ...hit, stdout, stderr, output: [null, stdout, stderr] }; }
    const r = spawnSync.apply(this, arguments);
    if (r && !r.error && r.status === 0 && r.signal === null) seenSpawn.set(key, { ...r, stdout: copy(r.stdout), stderr: copy(r.stderr) });
    return r;
  };
  const execFileSync = childProcess.execFileSync;
  childProcess.execFileSync = function memoExecFileSync(cmd, args, options) {
    const opts = Array.isArray(args) ? options : args; const argv = Array.isArray(args) ? args : [];
    if (opts?.input !== undefined || opts?.shell || !pureQuestion(cmd, argv)) return execFileSync.apply(this, arguments);
    stats.asked += 1; const key = keyOf(cmd, argv, opts);
    if (seenExec.has(key)) { stats.answered += 1; return copy(seenExec.get(key)); }
    const out = execFileSync.apply(this, arguments); // throws on failure: a failure is never remembered
    seenExec.set(key, copy(out));
    return out;
  };
  module.syncBuiltinESMExports();
  return stats;
}
