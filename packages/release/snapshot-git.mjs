// The sealed git invocation of the gate (item 5 D3, signed/item5-sage-decisions-2.md; docs/GATE-CONTRACT.md §1 "exact snapshot",
// rightkit#1255). The snapshot's identity and every object read the gate makes depend on the commit's objects and the snapshot's
// bytes ONLY. The checkout's git configuration cannot reach them: no `filter.*` clean or smudge driver, no core.autocrlf or eol
// conversion, no attributes from the checkout's `.git/info/attributes` or any attributes file, no hooks, no system, global or user
// configuration, no replace refs, no inherited GIT_* variable. (Probes P3a to P3d of the decision: today's unsealed invocation ran a
// checkout filter and hid a byte tamper and a line-ending tamper behind the commit's own id.)
//
// ONE runner serves every identity operation: `<sha>^{tree}`, `ls-tree`, `cat-file --batch`, `read-tree`, `add -A`, `write-tree` and
// the 5.09d ignore oracle (`check-ignore`). Only resolving a revision NAME to a commit runs against the checkout's own git directory
// (it needs the refs), under the same environment plus GIT_NO_REPLACE_OBJECTS (`discoveryGit`).
//
// The private git directory T (mkdtemp, mode 0700, outside the checkout and the snapshot, removed afterwards) holds:
//   HEAD `ref: refs/heads/none`; an empty refs/; objects/info/alternates naming `<checkout git common dir>/objects` (so every read is
//   still against the checkout's object database, and every object `add` and `write-tree` writes lands in T, never in the
//   checkout); `config` with only core.repositoryformatversion and core.bare (plus extensions.objectFormat for SHA-256);
//   info/attributes SEALED_ATTRIBUTES (the highest-precedence attribute source); an empty info/exclude (so the only ignore rules are
//   the commit's own `.gitignore` files); an empty hooks/; an empty file `empty`.
// The environment is built from nothing (sealedEnv). The `-c` flags are SEALED_CONFIG, in this order (sealedConfigArgs).
//
// The exact flags live in SEALED_GIT, one frozen constant: the broker (rustright check_admission.rs `sealed_git()`) implements the
// identical invocation, and the three-way compare of row 5.11 cites this constant.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

/** info/attributes of T, the highest-precedence attribute source: nothing converts, rewrites, identifies or re-encodes a byte. */
export const SEALED_ATTRIBUTES = "* -text -ident -filter -working-tree-encoding !eol\n";

/** The `-c key=value` list, in order. `@FILEMODE` is true on darwin and linux and false on win32 (NTFS has no exec bit: a 100755 lives in the seeded index, signed 5.04); `@T/x` is T/x. */
export const SEALED_CONFIG = Object.freeze([
  ["core.autocrlf", "false"], ["core.safecrlf", "false"], ["core.eol", "lf"],
  ["core.fileMode", "@FILEMODE"], ["core.symlinks", "true"],
  ["core.ignoreCase", "false"], ["core.precomposeUnicode", "false"],
  ["core.attributesFile", "@T/empty"], ["core.excludesFile", "@T/empty"], ["core.hooksPath", "@T/hooks"],
  ["core.fsmonitor", "false"], ["core.untrackedCache", "false"], ["core.splitIndex", "false"],
  ["core.sparseCheckout", "false"], ["index.sparse", "false"], ["core.useReplaceRefs", "false"],
].map((pair) => Object.freeze(pair)));

/** Environment variables the invocation sets to a fixed value; the others (PATH, HOME, XDG_CONFIG_HOME, LC_ALL, GIT_CONFIG_GLOBAL, GIT_INDEX_FILE, GIT_DIR, GIT_WORK_TREE, and on Windows SystemRoot, WINDIR, PATHEXT, TEMP, TMP) are built by sealedEnv. */
export const SEALED_ENV_FIXED = Object.freeze({
  GIT_CONFIG_NOSYSTEM: "1", GIT_ATTR_NOSYSTEM: "1", GIT_NO_REPLACE_OBJECTS: "1", GIT_NO_LAZY_FETCH: "1", GIT_TERMINAL_PROMPT: "0", GIT_OPTIONAL_LOCKS: "0",
});

/** The sealed invocation, whole: what the three-way compare (5.11) and the broker's `sealed_git()` cite. */
export const SEALED_GIT = Object.freeze({
  attributes: SEALED_ATTRIBUTES,
  config: SEALED_CONFIG,
  envFixed: SEALED_ENV_FIXED,
  envBuilt: Object.freeze(["PATH", "HOME", "XDG_CONFIG_HOME", "LC_ALL", "GIT_CONFIG_GLOBAL", "GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE", "SystemRoot", "WINDIR", "PATHEXT", "TEMP", "TMP"]),
  layout: Object.freeze({ head: "ref: refs/heads/none\n", alternates: "<git common dir>/objects", excludeIsEmpty: true, hooksIsEmpty: true, workingDirectory: "the snapshot" }),
  gitVersionNote: "GIT_NO_LAZY_FETCH needs git 2.45 or later; older git ignores it",
});

const MAX_BUFFER = 1 << 30;
const WINDOWS_ENV = ["SystemRoot", "WINDIR", "PATHEXT"];

/** `-c k=v ...` for T and `platform`, in SEALED_CONFIG's order. */
export function sealedConfigArgs(T, platform = process.platform) {
  const out = [];
  for (const [key, value] of SEALED_CONFIG) {
    const v = value === "@FILEMODE" ? (platform === "win32" ? "false" : "true") : value.startsWith("@T/") ? path.join(T, value.slice(3)) : value;
    out.push("-c", `${key}=${v}`);
  }
  return out;
}

const fromEnv = (base, name) => { const k = Object.keys(base).find((x) => x.toLowerCase() === name.toLowerCase()); return k === undefined ? undefined : base[k]; };

/** The environment for git: built from nothing, never inherited. `T` is the private git directory (or the discovery home), `snapshot` the work tree. */
export function sealedEnv(T, { snapshot, platform = process.platform, base = process.env, gitDir = T, index = true } = {}) {
  const env = {};
  const p = fromEnv(base, "PATH");
  if (p !== undefined) env.PATH = p;
  if (platform === "win32") {
    for (const name of WINDOWS_ENV) { const v = fromEnv(base, name); if (v !== undefined) env[name] = v; }
    env.TEMP = T; env.TMP = T;
  }
  env.HOME = T; env.XDG_CONFIG_HOME = T; env.LC_ALL = "C";
  env.GIT_CONFIG_GLOBAL = path.join(T, "empty");
  Object.assign(env, SEALED_ENV_FIXED);
  if (index) env.GIT_INDEX_FILE = path.join(T, "index");
  if (gitDir) env.GIT_DIR = gitDir;
  if (snapshot) env.GIT_WORK_TREE = snapshot;
  return env;
}

/** Creates T. `commonDir` (the checkout's git common directory) wires the object database; `objectFormat` is "sha1" or "sha256". */
export function createSealedGitDir({ commonDir, objectFormat = "sha1" } = {}) {
  const T = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sealed-git-")));
  try {
    for (const d of ["refs", path.join("objects", "info"), "info", "hooks"]) fs.mkdirSync(path.join(T, d), { recursive: true });
    fs.writeFileSync(path.join(T, "HEAD"), SEALED_GIT.layout.head);
    fs.writeFileSync(path.join(T, "config"), `[core]\n\trepositoryformatversion = ${objectFormat === "sha256" ? 1 : 0}\n\tbare = false\n${objectFormat === "sha256" ? "[extensions]\n\tobjectFormat = sha256\n" : ""}`);
    fs.writeFileSync(path.join(T, "info", "attributes"), SEALED_ATTRIBUTES);
    fs.writeFileSync(path.join(T, "info", "exclude"), "");
    fs.writeFileSync(path.join(T, "empty"), "");
    if (commonDir) fs.writeFileSync(path.join(T, "objects", "info", "alternates"), `${commonDir}/objects\n`);
  } catch (error) {
    fs.rmSync(T, { recursive: true, force: true });
    throw error;
  }
  return T;
}

const why = (r, args) => `git ${args.filter((a) => a !== "-c" && !/^(core|index)\./.test(a)).join(" ")}: ${r.error ? r.error.message : String(r.stderr ?? "").trim() || `exit ${r.status}`}`;

/**
 * Runs `fn(git)` with a fresh T; `git.run(args, { input, binary, cwd })` returns the spawn result, `git.ok(...)` its stdout or throws an
 * Error naming the command. The working directory is `snapshot` when given, else T. T is removed afterwards.
 */
export function withSealedGit({ commonDir, objectFormat, snapshot, platform = process.platform } = {}, fn) {
  const T = createSealedGitDir({ commonDir, objectFormat });
  const env = sealedEnv(T, { snapshot, platform });
  const prefix = sealedConfigArgs(T, platform);
  const run = (args, { input, binary = false, cwd } = {}) => spawnSync("git", [...prefix, ...args], { cwd: cwd ?? snapshot ?? T, env, input, maxBuffer: MAX_BUFFER, ...(binary ? {} : { encoding: "utf8" }) });
  const ok = (args, options) => { const r = run(args, options); if (r.error || r.status !== 0) throw new Error(why(r, args)); return r.stdout; };
  try { return fn({ run, ok, T, env }); } finally { fs.rmSync(T, { recursive: true, force: true }); }
}

// --- discovery: finding a repository and resolving a revision NAME, the only reads that need the checkout's own git directory -------------

let home = null;
/** One empty home directory per process for discovery runs, removed at exit. */
function discoveryHome() {
  if (home && fs.existsSync(home)) return home;
  home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-git-home-")));
  fs.writeFileSync(path.join(home, "empty"), "");
  const mine = home;
  process.once("exit", () => { try { fs.rmSync(mine, { recursive: true, force: true }); } catch { /* best effort */ } });
  return home;
}

/** git run in `cwd` against the checkout's own repository (found from `cwd`), under the sealed environment (no GIT_DIR, no index) plus GIT_NO_REPLACE_OBJECTS. Returns the spawn result. */
export function discoveryRun(cwd, args, { platform = process.platform, input } = {}) {
  const env = sealedEnv(discoveryHome(), { platform, gitDir: null, index: false });
  return spawnSync("git", args, { cwd, env, input, encoding: "utf8", maxBuffer: MAX_BUFFER });
}
/** As discoveryRun, returning trimmed-by-caller stdout or throwing an Error naming the command. */
export function discoveryGit(cwd, args, options) {
  const r = discoveryRun(cwd, args, options);
  if (r.error || r.status !== 0) throw new Error(why(r, args));
  return r.stdout;
}

const infoCache = new Map();
/** `{ commonDir, objectFormat }` of the repository `checkout` belongs to, found by discovery and cached per canonical path. */
export function sealedRepoInfo(checkout) {
  let key;
  try { key = fs.realpathSync.native(checkout); } catch { key = path.resolve(checkout); }
  const cached = infoCache.get(key);
  if (cached && fs.existsSync(path.join(cached.commonDir, "objects"))) return cached;
  let common;
  try { common = discoveryGit(key, ["rev-parse", "--path-format=absolute", "--git-common-dir"]).trim(); }
  catch { common = path.resolve(key, discoveryGit(key, ["rev-parse", "--git-common-dir"]).trim()); }
  const info = { commonDir: common, objectFormat: discoveryGit(key, ["rev-parse", "--show-object-format"]).trim() || "sha1" };
  infoCache.set(key, info);
  return info;
}

/** A single sealed git command against `checkout`'s object database (T is made and removed around it). Returns the spawn result. */
export function sealedRun(checkout, args, options = {}) {
  const info = options.repo ?? sealedRepoInfo(checkout);
  return withSealedGit({ ...info, snapshot: options.snapshot }, (git) => git.run(args, options));
}
