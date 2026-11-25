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
  // A tool's own version, asked with nothing else: the version of the binary PATH resolves (GC-A27 NN08: actionlint -version and shellcheck --version
  // asked once by the lookup pass and again by the real pass, from two probe folders, on 0.6.70). Only tools with one binary on PATH: rustup's proxies
  // (cargo, rustc, rustfmt) and pnpm resolve a version per folder, so they are not listed.
  if (FIXED_VERSION_TOOLS.has(name) && args.length === 1 && TOOL_VERSION_FLAGS.has(args[0])) return true;
  if (name !== "git") return false;
  // Leading `-c key=value` pairs configure the call, they are part of the question (and of the key), not a different question.
  let rest = args; while (rest[0] === "-c" && typeof rest[1] === "string" && rest[1].includes("=")) rest = rest.slice(2);
  if (rest[0] === "rev-parse") {
    const tail = rest.slice(1).filter((a) => a !== "--path-format=absolute");
    if (tail.length === 1 && REV_FLAGS.has(tail[0])) return true;
    // Does this id name a commit, or a tree? An object id is content: once it resolves, it always does.
    if (tail.length === 3 && tail[0] === "--verify" && tail[1] === "--quiet") { const m = /^([0-9a-f]+)\^\{(commit|tree)\}$/.exec(tail[2]); return Boolean(m && SHA.test(m[1])); }
    // The app rules' own reading of a commit named by its full id (app-check.mjs): asked again for every stage of a door run (Dell 2026-11-19,
    // crate-edit compile: `rev-parse --verify --end-of-options <id>`, `rev-parse --verify <id>^{}` three times each).
    if (tail.length === 3 && tail[0] === "--verify" && tail[1] === "--end-of-options") return SHA.test(tail[2]);
    if (tail.length === 2 && tail[0] === "--verify") { const m = /^([0-9a-f]+)\^\{\}$/.exec(tail[1]); return Boolean(m && SHA.test(m[1])); }
    // The tree or commit of a full id, asked without --verify (the door, twice per compile run).
    if (tail.length === 1) { const m = /^([0-9a-f]+)\^\{(commit|tree)\}$/.exec(tail[0]); return Boolean(m && SHA.test(m[1])); }
    return false;
  }
  // The type of an object named by its full id: content-addressed.
  if (rest[0] === "cat-file") return rest.length === 3 && rest[1] === "-t" && SHA.test(rest[2]);
  if (rest[0] === "config") return rest.length === 3 && rest[1] === "--get" && rest[2] === "remote.origin.url";
  // A file of a commit or tree named by its full id: content-addressed, the same bytes for ever (NN07: `git show <commit>:rk/Cargo.toml` 16 times in
  // one compile of the witness fixture, Dell, 2026-11-17).
  if (rest[0] === "show") return rest.length === 2 && /^[0-9a-f]{40}([0-9a-f]{24})?:[^\0]+$/.test(rest[1]) && !rest[1].includes("..");
  // The file list of a tree named by its full id, names only or with modes and ids (snapshot.mjs treeEntries).
  // Any listing of a tree named by its full id, whatever its listing flags: kept in this process (only the two forms of treeListOf go to disk).
  if (rest[0] === "ls-tree") return treeListOf(args) !== null || (rest.length >= 2 && SHA.test(rest[rest.length - 1]) && rest.slice(1, -1).every((f) => LS_TREE_FLAGS.has(f)));
  return false;
}
const TOOL_VERSION_FLAGS = new Set(["--version", "-version", "-V"]);
const FIXED_VERSION_TOOLS = new Set(["actionlint", "shellcheck", "oxlint", "cargo-deny", "cargo-audit"]);
/**
 * A question whose answer is fixed by its arguments alone, whatever folder asks it: a git object named by its full id (the same bytes in any
 * repository that has it; one that does not fails, and a failure is never kept), or a tool's own version (keyed by PATH instead). The checkout and
 * the snapshot asked these separately, so each was asked twice (NN08, acceptance cases on 0.6.70). Exported for its test.
 */
export function folderFree(cmd, args) {
  const name = path.basename(String(cmd).replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
  if (name !== "git") return FIXED_VERSION_TOOLS.has(name) && args.length === 1 && TOOL_VERSION_FLAGS.has(args[0]);
  let rest = args; while (rest[0] === "-c" && typeof rest[1] === "string" && rest[1].includes("=")) rest = rest.slice(2);
  if (rest[0] === "show") return rest.length === 2 && /^[0-9a-f]{40}([0-9a-f]{24})?:/.test(rest[1]);
  if (rest[0] === "cat-file") return rest.length === 3 && SHA.test(rest[2]);
  if (rest[0] === "ls-tree") return rest.length >= 2 && SHA.test(rest[rest.length - 1]);
  if (rest[0] === "rev-parse") { const tail = rest.slice(1); return tail.length >= 1 && /^[0-9a-f]{40}/.test(tail[tail.length - 1]) && !tail.some((a) => a === "--show-toplevel" || a === "HEAD"); }
  return false;
}
const LS_TREE_FLAGS = new Set(["-r", "-z", "-l", "-t", "--name-only", "--full-tree", "--long"]);

/**
 * `git grep` over a tree named by its full id (app-check.mjs lists a commit's text files with `grep -z -l -I -e <pattern> <id> --`): the same
 * bytes for ever, so the same answer. Its exit status 1 means "no line matched", which is an answer too; only a pattern given with `-e` and a
 * full id directly before the closing `--` qualify. Exported for its test.
 */
export function pureGrep(cmd, args) {
  if (typeof cmd !== "string" || !Array.isArray(args) || !args.every((a) => typeof a === "string")) return false;
  const name = path.basename(cmd.replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
  if (name !== "git") return false;
  let rest = args; while (rest[0] === "-c" && typeof rest[1] === "string" && rest[1].includes("=")) rest = rest.slice(2);
  if (rest[0] !== "grep" || rest[rest.length - 1] !== "--" || !SHA.test(rest[rest.length - 2] ?? "")) return false;
  const flags = rest.slice(1, -2); const e = flags.indexOf("-e");
  return e >= 0 && e === flags.length - 2 && flags.slice(0, e).every((f) => /^-[A-Za-z]$/.test(f));
}

/**
 * `git cat-file --batch` fed only full object ids on its input: every object is named by its content, so the output is fixed by the input.
 * Returns the sha256 of that input (part of the remembering key), or null when the call is anything else.
 */
export function batchInputKey(cmd, args, options) {
  if (typeof cmd !== "string" || !Array.isArray(args) || options?.input === undefined) return null;
  const name = path.basename(cmd.replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
  if (name !== "git") return null;
  let rest = args; while (rest[0] === "-c" && typeof rest[1] === "string" && rest[1].includes("=")) rest = rest.slice(2);
  if (rest.length !== 2 || rest[0] !== "cat-file" || rest[1] !== "--batch") return null;
  const text = Buffer.isBuffer(options.input) ? options.input.toString("utf8") : typeof options.input === "string" ? options.input : null;
  if (text === null) return null;
  const lines = text.split("\n").filter((l) => l !== "");
  if (!lines.length || !lines.every((l) => SHA.test(l))) return null;
  return createHash("sha256").update(text).digest("hex");
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
 * full object id. Each file starts with the sha256 of its key and the bytes it holds together, so a file whose bytes do not match (truncated,
 * half-written, edited) or that sits under another key's name (copied, renamed) is never returned (astra62 review 2, 2026-11-18). The store is
 * the gate user's own state under the gate runs root, the same trust as the result cache beside it.
 */
export function contentStore(storeDir, { maxEntries = 256 } = {}) {
  if (typeof storeDir !== "string" || !storeDir) return null;
  const fileOf = (key) => path.join(storeDir, `${createHash("sha256").update(String(key)).digest("hex")}.bin`);
  const seal = (key, body) => createHash("sha256").update(String(key)).update("\0").update(body).digest("hex");
  return {
    get(key) {
      try {
        const raw = fs.readFileSync(fileOf(key)); const body = raw.subarray(65);
        return raw.length >= 65 && raw[64] === 0x0a && raw.subarray(0, 64).toString("latin1") === seal(key, body) ? Buffer.from(body) : null;
      } catch { return null; }
    },
    put(key, bytes) {
      if (!Buffer.isBuffer(bytes)) return;
      const f = fileOf(key);
      try {
        fs.mkdirSync(storeDir, { recursive: true });
        const tmp = `${f}.${process.pid}.tmp`; fs.writeFileSync(tmp, Buffer.concat([Buffer.from(`${seal(key, bytes)}\n`, "latin1"), bytes])); fs.renameSync(tmp, f);
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
// NN07 (Dell 2026-11-19, every ledger of the witness fixture): the same question started twice because it was asked two ways. `git config --get
// remote.origin.url` once as `git` (snapshot.mjs) and once as `C:\Program Files\Git\cmd\git.exe` (the gate's default spawn); `rev-parse
// --show-toplevel` and `--git-common-dir` the same; and a question asked through spawnSync and again through execFileSync. The contract test keys
// a start by program name, arguments and directory, so these are one question. Its answer (the bytes of stdout) is kept once under that same key
// and given to every caller in the form that caller asked for. Only a call that reads stdout through a pipe takes part.
const programNameOf = (cmd) => path.basename(String(cmd).replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
const questionKeyOf = (cmd, args, options) => {
  const env = Object.entries(options?.env && typeof options.env === "object" ? options.env : process.env).filter(([k]) => /^GIT_/i.test(k)).sort();
  const free = folderFree(cmd, Array.isArray(args) ? args : []);
  const pathVar = free && programNameOf(cmd) !== "git" ? Object.entries(options?.env && typeof options.env === "object" ? options.env : process.env).filter(([k]) => /^path$/i.test(k)).map(([, v]) => v) : [];
  return JSON.stringify([programNameOf(cmd), args, free ? pathVar : path.resolve(typeof options?.cwd === "string" ? options.cwd : process.cwd()), env]);
};
const piped = (options) => options?.stdio === undefined || options.stdio === "pipe";
/** A batch read or a grep answer larger than this is not kept in memory: it is asked again. */
const KEEP_MAX_BYTES = 8 * 1024 * 1024;
const bytesOf = (out, encoding) => (Buffer.isBuffer(out) ? Buffer.from(out) : typeof out === "string" ? Buffer.from(out, encoding && encoding !== "buffer" ? encoding : "utf8") : null);

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
  const seenSpawn = new Map(); const seenExec = new Map(); const answers = new Map();
  const spawnSync = childProcess.spawnSync;
  childProcess.spawnSync = function memoSpawnSync(cmd, args, options) {
    const opts = Array.isArray(args) ? options : args; const argv = Array.isArray(args) ? args : [];
    // Two more kinds of fixed answer (NN07, Dell 2026-11-19, compile runs: the app rules read the same commit once per stage). A batch read of
    // objects named by full id is keyed by its input as well; a grep over a full id keeps exit status 1 ("no line matched") as its answer.
    // Both are remembered for the identical call only, never shared between ways of asking, and never when the output is large.
    const batch = opts?.shell ? null : batchInputKey(cmd, argv, opts);
    const grep = !batch && opts?.input === undefined && !opts?.shell && pureGrep(cmd, argv);
    if (!batch && !grep && (opts?.input !== undefined || opts?.shell || !pureQuestion(cmd, argv))) return spawnSync.apply(this, arguments);
    stats.asked += 1; const key = keyOf(cmd, argv, opts) + (batch ? `\u0000${batch}` : ""); const hit = seenSpawn.get(key);
    if (hit) { stats.answered += 1; const stdout = copy(hit.stdout); const stderr = copy(hit.stderr); return { ...hit, stdout, stderr, output: [null, stdout, stderr] }; }
    if (batch || grep) {
      const r = spawnSync.apply(this, arguments);
      const answered = r && !r.error && r.signal === null && (r.status === 0 || (grep && r.status === 1));
      // `git cat-file --batch` exits 0 and prints `<id> missing` for an object the repository does not hold yet: that is not an answer to keep
      // (the object may arrive: an ephemeral commit, a fetch), exactly like a failed question.
      const missing = batch && r && /^[0-9a-f]{40,64} missing$/m.test(Buffer.isBuffer(r.stdout) ? r.stdout.toString("latin1") : String(r.stdout ?? ""));
      if (answered && !missing && (r.stdout?.length ?? 0) <= KEEP_MAX_BYTES) seenSpawn.set(key, { ...r, stdout: copy(r.stdout), stderr: copy(r.stderr) });
      return r;
    }
    // A path missing from a commit named by its full id is missing for ever (NN08: `git show <sha>:package.json` asked twice, status 128, 0.6.72).
    if (absentAtCommit(cmd, argv)) {
      const r = spawnSync.apply(this, arguments);
      if (r && !r.error && r.signal === null && r.status === 128 && /does not exist in|exists on disk, but not in/.test(String(r.stderr ?? ""))) seenSpawn.set(key, { ...r, stdout: copy(r.stdout), stderr: copy(r.stderr) });
      else if (r && !r.error && r.status === 0 && (r.stdout?.length ?? 0) <= KEEP_MAX_BYTES) seenSpawn.set(key, { ...r, stdout: copy(r.stdout), stderr: copy(r.stderr) });
      return r;
    }
    const question = piped(opts) ? questionKeyOf(cmd, argv, opts) : null;
    const shared = question ? answers.get(question) : undefined;
    if (shared) {
      stats.answered += 1; const stdout = asEncoded(shared, opts?.encoding); const stderr = asEncoded(Buffer.alloc(0), opts?.encoding);
      const r = { pid: 0, status: 0, signal: null, stdout, stderr, output: [null, stdout, stderr] };
      seenSpawn.set(key, { ...r, stdout: copy(stdout), stderr: copy(stderr) }); return r;
    }
    const kept = store?.get(argv, opts?.cwd);
    if (kept) {
      stats.stored += 1; const stdout = asEncoded(kept, opts?.encoding); const stderr = asEncoded(Buffer.alloc(0), opts?.encoding);
      const r = { pid: 0, status: 0, signal: null, stdout, stderr, output: [null, stdout, stderr] };
      if (question) answers.set(question, Buffer.from(kept));
      seenSpawn.set(key, { ...r, stdout: copy(stdout), stderr: copy(stderr) }); return r;
    }
    const r = spawnSync.apply(this, arguments);
    if (r && !r.error && r.status === 0 && r.signal === null && FINAL_ONLY(cmd, r.stdout)) {
      const bytes = question ? bytesOf(r.stdout, opts?.encoding) : null;
      if (bytes) answers.set(question, bytes);
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
    const question = piped(opts) ? questionKeyOf(cmd, argv, opts) : null;
    const shared = question ? answers.get(question) : undefined;
    if (shared) { stats.answered += 1; const out = asEncoded(shared, opts?.encoding); seenExec.set(key, copy(out)); return out; }
    const kept = store?.get(argv, opts?.cwd);
    if (kept) { stats.stored += 1; const out = asEncoded(kept, opts?.encoding); if (question) answers.set(question, Buffer.from(kept)); seenExec.set(key, copy(out)); return out; }
    const out = execFileSync.apply(this, arguments); // throws on failure: a failure is never remembered
    if (FINAL_ONLY(cmd, out)) {
      const bytes = question ? bytesOf(out, opts?.encoding) : null;
      if (bytes) answers.set(question, bytes);
      seenExec.set(key, copy(out));
      if (store) store.put(argv, opts?.cwd, Buffer.isBuffer(out) ? out : Buffer.from(String(out), opts?.encoding && opts.encoding !== "buffer" ? opts.encoding : "utf8"));
    }
    return out;
  };
  module.syncBuiltinESMExports();
  return stats;
}

export function absentAtCommit(cmd, argv) {
  return /(^|[\\/])git(\.exe)?$/i.test(String(cmd)) && argv.length === 2 && argv[0] === "show" && /^[0-9a-f]{40}:[^:]+$/.test(String(argv[1]));
}
