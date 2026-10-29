// Test-only broker guard for @rightkit/release.
//
// Loaded by `pnpm test` (node --import) and by the release test files. It sits at the one
// launch boundary every release subprocess already crosses (background-process.mjs wraps
// child_process) and at the broker socket connect, and it:
//   - stubs Cargo metadata for the managed shim, the rustright/rightkit client and plain cargo,
//     computing it from the real manifest (no broker, no cargo build), and records each call;
//   - refuses every other launch of a broker-reaching command and any broker socket connect,
//     throwing a RightKit broker guard error so the test fails loudly;
//   - leaves temp-directory fixture clients alone (tests build those as POSIX shell stubs).
//   - outside hosted CI (CI !== "true") treats every cargo, rustc, tauri and cargo-tauri launch as
//     broker-reaching wherever PATH resolves it, so a local run never starts a real build or a new target root.
// Opt-in for a real end-to-end run: RIGHTKIT_TEST_REAL_BROKER=1 (mirrors RIGHTKIT_TEST_REAL_DMG).
// Real managed root (item 5 row 5.25, incident F8): on a Mac or Dell, a process under test that creates, writes, renames or removes anything
// under the real managed root's `snapshots/` or `build-ownership/` (or launches a child whose cwd or owner-record variable points there)
// throws RK_TEST_REAL_MANAGED_ROOT. A real-root run declares itself on purpose: RIGHTKIT_TEST_REAL_MANAGED_ROOT=1 or withRealManagedRoot(fn).
// See "Real managed root guard" at the end of this file; the guard lives only here, never in production code.
// Child processes inherit the guard through NODE_OPTIONS, which installation appends to.
// Not shipped: lives under test-support/, outside package.json "files".
import childProcess from "node:child_process";
import fs, { appendFileSync, mkdtempSync, realpathSync, statSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { resolveManagedRoots } from "../managed-roots.mjs";

export const OPT_IN_ENV = "RIGHTKIT_TEST_REAL_BROKER";
export const LOG_ENV = "RIGHTKIT_BROKER_GUARD_LOG";
const INSTALLED = Symbol.for("rightkit.release.brokerGuard");
const GUARD_FILE = fileURLToPath(import.meta.url);
const GUARD_IMPORT = `--import=${pathToFileURL(GUARD_FILE).href}`;
const BROKER_BINARIES = new Set(["cargo", "rightkit", "rustright"]);
// Release-only signing credentials of the developer's shell (rightkit#1232): fixtures must never see them, or right-release's
// host-secret-env refusal leaks into every doctor/parity/build fixture and the result depends on whoever runs the tests.
// Removed from this process before any test runs; children inherit the scrubbed environment.
export const RELEASE_ONLY_ENV = Object.freeze(["APPLE_ID", "APPLE_PASSWORD", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH", "APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD"]);
if (process.env[OPT_IN_ENV] !== "1") for (const name of RELEASE_ONLY_ENV) delete process.env[name];
const MANAGED_SEGMENT = /[\\/](?:agent-bin|runtime-current|build-control|\.rightkit-managed)(?:[\\/]|$)/i;
const CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index";

// Cargo metadata for one manifest, shaped like `cargo metadata --no-deps` for the fields the
// release contracts read (packages[].dependencies, manifest_path, workspace_root, target_directory).
const METADATA_SCRIPT = String.raw`
import json, pathlib, sys, tomllib
manifest = pathlib.Path(sys.argv[1]).absolute()  # keep the caller's spelling (no symlink resolution), as cargo does
try:
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
except (OSError, tomllib.TOMLDecodeError) as error:
    sys.stderr.write("error: failed to parse manifest %s: %s\n" % (manifest, error))
    sys.exit(101)
root = manifest.parent
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"

def target_directory(root):
    # Real metadata honours an absolute CARGO_TARGET_DIR (a door split binds the target outside the snapshot with it); otherwise <root>/target.
    import os
    named = os.environ.get("CARGO_TARGET_DIR", "")
    return named if named and os.path.isabs(named) else str(root / "target")

def dependency(key, spec, kind, target):
    table = {"version": spec} if isinstance(spec, str) else (spec if isinstance(spec, dict) else {})
    name = table.get("package", key)
    entry = {"name": name, "rename": key if name != key else None, "optional": bool(table.get("optional", False)),
             "uses_default_features": bool(table.get("default-features", True)), "features": list(table.get("features", [])),
             "source": None, "req": "*", "kind": kind, "path": None, "registry": None, "target": target}
    if "path" in table:
        entry["path"] = str((root / table["path"]).resolve())
    elif "git" in table:
        entry["source"] = "git+" + str(table["git"])
    else:
        entry["registry"] = table.get("registry")
        entry["source"] = "registry+" + str(table["registry"]) if "registry" in table else CRATES_IO
        entry["req"] = str(table.get("version", "*"))
    return entry

def collect(section, kind, target=None):
    return [dependency(key, value, kind, target) for key, value in (section or {}).items()]

deps = collect(data.get("dependencies"), None) + collect(data.get("dev-dependencies"), "dev") + collect(data.get("build-dependencies"), "build")
for selector, table in (data.get("target") or {}).items():
    deps += collect(table.get("dependencies"), None, selector) + collect(table.get("dev-dependencies"), "dev", selector) + collect(table.get("build-dependencies"), "build", selector)

package = data.get("package")
packages = []
if isinstance(package, dict) and "name" in package:
    version = str(package.get("version", "0.0.0"))
    packages.append({"name": package["name"], "version": version, "id": "%s %s (path+file://%s)" % (package["name"], version, root),
                     "source": None, "dependencies": deps, "targets": [], "features": {}, "manifest_path": str(manifest),
                     "authors": [], "categories": [], "keywords": [], "edition": str(package.get("edition", "2015")),
                     "metadata": None, "publish": None, "license": None, "license_file": None, "description": None,
                     "readme": None, "repository": None, "homepage": None, "documentation": None, "links": None,
                     "default_run": None, "rust_version": None})
json.dump({"packages": packages, "workspace_members": [p["id"] for p in packages], "workspace_root": str(root),
           "target_directory": target_directory(root), "metadata": None, "version": 1}, sys.stdout)
`;

const original = {
  spawn: childProcess.spawn,
  spawnSync: childProcess.spawnSync,
  execFile: childProcess.execFile,
  execFileSync: childProcess.execFileSync,
  createConnection: net.createConnection,
  connect: net.connect,
};

let pythonRunner;

function enforcing() {
  return process.env[OPT_IN_ENV] !== "1";
}

function refusal(what) {
  return new Error(
    `RightKit broker guard: a test tried to reach the live RightKit build broker (${what}). ` +
      "Tests must inject a stub runner instead of launching the managed Cargo shim, the rustright/rightkit client, " +
      `or the broker socket. For a real end-to-end run, set ${OPT_IN_ENV}=1 on purpose.`,
  );
}

function record(entry) {
  const file = process.env[LOG_ENV];
  if (!file) return;
  try {
    appendFileSync(file, `${JSON.stringify({ pid: process.pid, at: new Date().toISOString(), ...entry })}\n`);
  } catch {
    // The log is evidence for the owner, not a precondition: a failed append never changes the verdict.
  }
}

function tempRoots() {
  const roots = [os.tmpdir()];
  try {
    roots.push(realpathSync(os.tmpdir()));
  } catch {
    // tmpdir that cannot be resolved is still a valid fixture root.
  }
  return roots.map((root) => path.resolve(root));
}

function insideTemp(file) {
  // A bare command name has no directory, so it is never a fixture.
  if (!/[\\/]/.test(file)) return false;
  const resolved = path.resolve(file);
  const fold = (value) => (process.platform === "win32" ? value.toLowerCase() : value);
  return tempRoots().some((root) => {
    const relative = path.relative(fold(root), fold(resolved));
    return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
  });
}

// True when a launch would reach the live broker, the managed shim or a plain cargo binary.
// A bare `cargo` reaches the broker only when PATH resolves it to the managed shim (dev hosts);
// on CI runners plain cargo is real cargo and runs normally.
function resolveOnPath(name) {
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!dir) continue;
    for (const candidate of process.platform === "win32" ? [`${name}.exe`, `${name}.cmd`, name] : [name]) {
      const full = path.join(dir, candidate);
      try { if (statSync(full).isFile()) return full; } catch { /* keep looking */ }
    }
  }
  return null;
}

// Hosted CI (GitHub Actions sets CI=true) is the one place a test may run the toolchain's own cargo.
export function hostedCi() {
  return process.env.CI === "true";
}

// Build tools whose launch from a test outside CI must never reach a real build: the compiler front
// ends a build runs through (their wrappers on a managed host are not always under a managed segment).
const BUILD_TOOLS = new Set(["cargo", "rustc", "tauri", "cargo-tauri"]);

export function brokerLaunch(file) {
  const text = String(file ?? "");
  const base = (text.split(/[\\/]/).pop() ?? "").toLowerCase().replace(/\.(?:exe|cmd)$/, "");
  const bare = !/[\\/]/.test(text);
  if (BUILD_TOOLS.has(base) && !hostedCi()) {
    // Outside hosted CI a build tool is never run for real, wherever PATH resolves it
    // (a managed shim, a rustup proxy or a RustRight wrapper). Fixture clients in the temp dir stay allowed.
    return !insideTemp(text);
  }
  const resolved = base === "cargo" && bare ? resolveOnPath("cargo") ?? "" : text;
  const reaches = base === "cargo" ? MANAGED_SEGMENT.test(resolved) : BROKER_BINARIES.has(base) || MANAGED_SEGMENT.test(text);
  return reaches && !insideTemp(text);
}

// Returns the cargo argument vector for a broker-reaching launch, or null when it is not a cargo command.
function cargoArguments(file, argv) {
  const base = (String(file).split(/[\\/]/).pop() ?? "").toLowerCase().replace(/\.exe$/, "");
  if (base === "cargo") return argv;
  // The retired name is caught too: a test that launches it would still reach an older installed broker.
  if ((base === "rustright" || base === "rightkit" /* retired client name, caught here, never run */) && argv[0] === "cargo") return argv.slice(1);
  return null;
}

function resolvePython() {
  if (pythonRunner) return pythonRunner;
  const candidates = process.platform === "win32"
    ? [["py", ["-3.11"]], ["py", ["-3"]], ["python", []]]
    : [["python3.13", []], ["python3.12", []], ["python3.11", []], ["python3", []]];
  for (const [command, prefix] of candidates) {
    const probe = original.spawnSync(command, [...prefix, "-c", "import tomllib"], { encoding: "utf8", windowsHide: true });
    if (probe.status === 0) {
      pythonRunner = { command, prefix };
      return pythonRunner;
    }
  }
  throw new Error("RightKit broker guard: no Python 3.11+ (tomllib) found to compute stub Cargo metadata; install one or put it on PATH.");
}

function nearestManifest(start) {
  for (let dir = path.resolve(start); ; dir = path.dirname(dir)) {
    try { if (statSync(path.join(dir, "Cargo.toml")).isFile()) return path.join(dir, "Cargo.toml"); } catch { /* keep looking */ }
    if (path.dirname(dir) === dir) return undefined;
  }
}

function stubMetadata(cargoArgv, cwd) {
  const index = cargoArgv.indexOf("--manifest-path");
  // Without --manifest-path cargo reads the nearest Cargo.toml at or above the working directory; the stub does the same.
  const manifest = index >= 0 ? cargoArgv[index + 1] : nearestManifest(cwd ?? process.cwd());
  if (!manifest) return { status: 101, stdout: "", stderr: "error: could not find `Cargo.toml` in the working directory or any parent directory" };
  const python = resolvePython();
  return original.spawnSync(
    python.command,
    [...python.prefix, "-c", METADATA_SCRIPT, path.resolve(cwd ?? process.cwd(), manifest)],
    { encoding: "utf8", windowsHide: true },
  );
}

function asOutput(text, encoding) {
  return encoding ? text : Buffer.from(text, "utf8");
}

function splitArguments(args, options) {
  return Array.isArray(args) ? [args, options] : [[], args ?? options];
}

// Returns the stub result for a broker-reaching launch, or throws the refusal.
function interceptSync(command, args, options) {
  const [argv, opts = {}] = splitArguments(args, options);
  const cargoArgv = cargoArguments(command, argv);
  if (!cargoArgv || cargoArgv[0] !== "metadata") {
    record({ action: "refused", command: String(command), args: argv, cwd: opts.cwd ?? process.cwd() });
    throw refusal(`${path.basename(String(command))} ${argv.join(" ")}`.trim());
  }
  const result = stubMetadata(cargoArgv, opts.cwd);
  const stdout = result.stdout ?? "";
  const stderr = result.stderr ?? "";
  record({ action: "stubbed", command: String(command), args: argv, cwd: opts.cwd ?? process.cwd(), status: result.status ?? null });
  return {
    pid: 0,
    output: [null, asOutput(stdout, opts.encoding), asOutput(stderr, opts.encoding)],
    stdout: asOutput(stdout, opts.encoding),
    stderr: asOutput(stderr, opts.encoding),
    status: result.status ?? null,
    signal: null,
    error: undefined,
  };
}

function guardedSpawnSync(command, ...rest) {
  if (!enforcing() || !brokerLaunch(command)) return original.spawnSync(command, ...rest);
  const [args, options] = rest;
  return interceptSync(command, args, options);
}

function guardedExecFileSync(file, ...rest) {
  if (!enforcing() || !brokerLaunch(file)) return original.execFileSync(file, ...rest);
  const [args, options] = rest;
  const result = interceptSync(file, args, options);
  if (result.status !== 0) {
    const error = new Error(`Command failed: ${file}\n${result.stderr}`);
    Object.assign(error, { status: result.status, stderr: result.stderr, stdout: result.stdout });
    throw error;
  }
  return result.stdout;
}

// Asynchronous launches of broker-reaching commands have no stub: they are refused outright.
function guardedSpawn(command, ...rest) {
  if (enforcing() && brokerLaunch(command)) {
    record({ action: "refused", command: String(command), args: splitArguments(rest[0], rest[1])[0] });
    throw refusal(`${path.basename(String(command))} (asynchronous launch)`);
  }
  return original.spawn(command, ...rest);
}

function guardedExecFile(file, ...rest) {
  if (enforcing() && brokerLaunch(file)) {
    record({ action: "refused", command: String(file), args: splitArguments(rest[0], rest[1])[0] });
    throw refusal(`${path.basename(String(file))} (asynchronous launch)`);
  }
  return original.execFile(file, ...rest);
}

// background-process.mjs promisifies execFile through util.promisify.custom; the guarded form must
// keep that contract and apply the same refusal (Node's own custom form would bypass the guard).
Object.defineProperty(guardedExecFile, promisify.custom, {
  value(file, ...rest) {
    if (enforcing() && brokerLaunch(file)) {
      record({ action: "refused", command: String(file), args: splitArguments(rest[0], rest[1])[0] });
      return Promise.reject(refusal(`${path.basename(String(file))} (asynchronous launch)`));
    }
    return original.execFile[promisify.custom](file, ...rest);
  },
});

function socketTarget(args) {
  const first = args[0];
  if (typeof first === "string") return first;
  if (first && typeof first === "object" && typeof first.path === "string") return first.path;
  return undefined;
}

function isBrokerSocket(target) {
  return /broker\.sock$/i.test(target) || (Boolean(process.env.RIGHTKIT_BUILD_BROKER_SOCKET) && target === process.env.RIGHTKIT_BUILD_BROKER_SOCKET);
}

function guardedConnect(...args) {
  const target = socketTarget(args);
  if (enforcing() && target && isBrokerSocket(target)) {
    record({ action: "refused", socket: target });
    throw refusal(`broker socket ${target}`);
  }
  return original.createConnection.apply(net, args);
}

// Installs the guard once per process. With the opt-in set, nothing is patched and real launches pass through.
/** Names the disk policy protects or the managed roots use: a fixture under one of them changes what the code under test sees. */
const UNSAFE_TEMP_NAMES = new Set(["Application Support", ".rightkit-managed", "rightkitmanagedagent", "build-control", "agent-cache", ".rustright"]);
export function hermeticTempBase({ tmpdir = os.tmpdir(), platform = process.platform, env = process.env } = {}) {
  const clean = (dir) => dir && !dir.split(/[\\/]/).some((seg) => UNSAFE_TEMP_NAMES.has(seg));
  if (clean(env.RIGHTKIT_TEST_SCRATCH)) return env.RIGHTKIT_TEST_SCRATCH;
  if (clean(tmpdir)) return tmpdir;
  const fallback = platform === "win32" ? path.join(env.SystemDrive ?? "C:", "Temp") : "/private/tmp";
  if (!clean(fallback)) throw new Error(`no hermetic temp base: ${tmpdir} and ${fallback} both sit under a protected or managed name`);
  return fallback;
}

export function installBrokerGuard() {
  if (globalThis[INSTALLED]) return globalThis[INSTALLED];
  const state = { enforcing: enforcing(), guardImport: GUARD_IMPORT };
  globalThis[INSTALLED] = state;
  if (!state.enforcing) return state;
  childProcess.spawn = guardedSpawn;
  childProcess.spawnSync = guardedSpawnSync;
  childProcess.execFile = guardedExecFile;
  childProcess.execFileSync = guardedExecFileSync;
  net.createConnection = guardedConnect;
  net.connect = guardedConnect;
  syncBuiltinESMExports();
  // Run leases and other managed-root writes go to a throwaway root, never the real one.
  // One throwaway root per test run, shared by child processes (they inherit the marker).
  // Real path: on macOS os.tmpdir() sits behind the /var -> /private/var symlink, which the signing-agent
  // directory checks (ensurePrivateDirectory) reject as a symlink ancestor.
  // Fixtures are hermetic: they never sit under a protected or managed name, whatever temp directory the host (or the
  // broker, which points TMPDIR under its managed root) hands this process. A test that wrote a fake `target` under such a
  // name passed by hand and failed through the broker (2026-10-27). One per-run temp root, free of those names, is set as
  // TMPDIR/TEMP/TMP so os.tmpdir() in this process and every child answers the same.
  process.env.RIGHTKIT_TEST_TMP ??= realpathSync(mkdtempSync(path.join(hermeticTempBase(), "rk-hermetic-")));
  for (const name of ["TMPDIR", "TEMP", "TMP"]) process.env[name] = process.env.RIGHTKIT_TEST_TMP;
  process.env.RIGHTKIT_GUARD_DATA_ROOT ??= realpathSync(mkdtempSync(path.join(os.tmpdir(), "rightkit-release-test-root-")));
  process.env.RIGHTKIT_DATA_ROOT = process.env.RIGHTKIT_GUARD_DATA_ROOT;
  // Child Node processes (release.mjs, build-release.mjs, cargo-target.mjs, fixtures) load the guard too.
  const current = process.env.NODE_OPTIONS ?? "";
  if (!current.includes(GUARD_IMPORT)) process.env.NODE_OPTIONS = `${current} ${GUARD_IMPORT}`.trim();
  return state;
}

// ---------------------------------------------------------------------------------------------------------------------------------
// Real managed root guard (item 5 row 5.25; incident F8: a gate test reached /Volumes/D/.rightkit-managed/snapshots and the root
// migration emptied it). Mac and Dell only: the real roots are `/Volumes/D/.rightkit-managed`, `D:\.rightkit-managed` and the managed
// data root the environment named when this process started (a throwaway root this guard itself made is never one). The protected
// directories are `<root>/snapshots` and `<root>/build-ownership`. Mutating fs calls (sync, callback and promise forms, and write
// streams) on a path under them throw, and so do child launches whose cwd or RIGHTKIT_BUILD_OWNERSHIP_FILE is under them. Read-only
// access is untouched, and so is everything else under the managed root (a native test may use its own test-only subdirectory).
// ---------------------------------------------------------------------------------------------------------------------------------
export const REAL_ROOT_OPT_IN_ENV = "RIGHTKIT_TEST_REAL_MANAGED_ROOT";
export const REAL_ROOT_ERROR_CODE = "RK_TEST_REAL_MANAGED_ROOT";
const ROOT_GUARD = Symbol.for("rightkit.release.managedRootGuard");
const PROTECTED_SUBDIRS = ["snapshots", "build-ownership"];
const rootState = (globalThis[ROOT_GUARD] ??= { dirs: new Map(), declared: 0, patched: false });

/** The real managed roots of this host (Mac, Dell), from the environment as this process was started; [] elsewhere. */
export function realManagedRoots({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  if (platform !== "darwin" && platform !== "win32") return [];
  const paths = platform === "win32" ? path.win32 : path.posix;
  const roots = new Set([resolveManagedRoots({ env: {}, platform, home }).dataRoot]);
  for (const value of [env.RIGHTKIT_DATA_ROOT, env.RIGHTKIT_MANAGED_ROOT, env.RIGHTKIT_EXTERNAL_ROOT]) {
    if (typeof value === "string" && paths.isAbsolute(value) && value !== env.RIGHTKIT_GUARD_DATA_ROOT && !insideTemp(value)) roots.add(value);
  }
  for (const root of [...roots]) { try { roots.add(realpathSync(root)); } catch { /* an absent root has no other spelling */ } }
  return [...roots];
}

function protectDirs(roots) {
  for (const root of roots) for (const sub of PROTECTED_SUBDIRS) rootState.dirs.set(path.join(root, sub), root);
}

/** Declares `root` (a temporary directory in a test) a real managed root, so the guard's own behaviour is tested without touching the real one. Returns the undo. */
export function protectManagedRoot(root) {
  const added = PROTECTED_SUBDIRS.map((sub) => path.join(root, sub));
  for (const dir of added) rootState.dirs.set(dir, root);
  return () => { for (const dir of added) rootState.dirs.delete(dir); };
}

/** Runs `fn` with the real-root guard off: the declared, deliberate real-root run (sync or async). */
export function withRealManagedRoot(fn) {
  rootState.declared++;
  let async = false;
  try {
    const result = fn();
    if (result && typeof result.then === "function") { async = true; return result.finally(() => { rootState.declared--; }); }
    return result;
  } finally {
    if (!async) rootState.declared--;
  }
}

const realRootOptedIn = () => rootState.declared > 0 || process.env[REAL_ROOT_OPT_IN_ENV] === "1";
const foldCase = (value) => (process.platform === "win32" || process.platform === "darwin" ? value.toLowerCase() : value);

function protectedDirOf(target) {
  let text;
  try { text = typeof target === "string" ? target : target instanceof URL ? fileURLToPath(target) : Buffer.isBuffer(target) ? target.toString("utf8") : null; } catch { return null; }
  if (!text) return null; // a file descriptor, or nothing
  if (process.platform === "win32") text = text.replace(/^\\\\\?\\(UNC\\)?/, (_, unc) => (unc ? "\\\\" : ""));
  const absolute = foldCase(path.resolve(text));
  for (const dir of rootState.dirs.keys()) {
    const rel = path.relative(foldCase(dir), absolute);
    if (rel === "" || (rel !== ".." && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel))) return dir;
  }
  return null;
}

function realRootError(what, target, dir) {
  return Object.assign(
    new Error(
      `RightKit test guard (${REAL_ROOT_ERROR_CODE}): a test tried to ${what} ${String(target)}, under the real managed root's ${path.basename(dir)} (${dir}). ` +
        `Tests must inject a temporary root. A real-root run declares itself on purpose: ${REAL_ROOT_OPT_IN_ENV}=1 or withRealManagedRoot().`,
    ),
    { code: REAL_ROOT_ERROR_CODE, protectedDir: dir },
  );
}

function refuseRealRoot(what, target) {
  if (rootState.dirs.size === 0 || realRootOptedIn()) return;
  const dir = protectedDirOf(target);
  if (!dir) return;
  record({ action: "refused-real-managed-root", what, target: String(target) });
  throw realRootError(what, target, dir);
}

const WRITE_FLAGS = (fs.constants.O_WRONLY ?? 1) | (fs.constants.O_RDWR ?? 2) | (fs.constants.O_CREAT ?? 0o100) | (fs.constants.O_APPEND ?? 0o2000) | (fs.constants.O_TRUNC ?? 0o1000);
const opensForWrite = (flags) => (typeof flags === "number" ? (flags & WRITE_FLAGS) !== 0 : typeof flags === "string" && !/^(?:r|rs|sr)$/.test(flags));

// Mutating fs calls and the argument positions that name a path they change.
const FS_WRITERS = {
  writeFile: [0], appendFile: [0], mkdir: [0], mkdtemp: [0], rm: [0], rmdir: [0], unlink: [0], rename: [0, 1], copyFile: [1],
  link: [1], symlink: [1], truncate: [0], utimes: [0], lutimes: [0], chmod: [0], lchmod: [0], chown: [0], lchown: [0], cp: [1],
};

function patchFs() {
  const wrap = (name, positions, flagsAt) => {
    const original = fs[name];
    if (typeof original !== "function") return;
    fs[name] = function guardedFsWriter(...args) {
      if (flagsAt === undefined || opensForWrite(args[flagsAt] ?? "r")) for (const at of positions) refuseRealRoot(name, args[at]);
      return original.apply(this, args);
    };
    for (const key of Object.getOwnPropertySymbols(original)) Object.defineProperty(fs[name], key, Object.getOwnPropertyDescriptor(original, key));
  };
  const wrapPromise = (name, positions, flagsAt) => {
    const original = fs.promises[name];
    if (typeof original !== "function") return;
    fs.promises[name] = function guardedFsPromise(...args) {
      try {
        if (flagsAt === undefined || opensForWrite(args[flagsAt] ?? "r")) for (const at of positions) refuseRealRoot(name, args[at]);
      } catch (error) { return Promise.reject(error); }
      return original.apply(this, args);
    };
  };
  for (const [name, positions] of Object.entries(FS_WRITERS)) {
    wrap(`${name}Sync`, positions);
    wrap(name, positions);
    wrapPromise(name, positions);
  }
  wrap("openSync", [0], 1);
  wrap("open", [0], 1);
  wrapPromise("open", [0], 1);
  wrap("createWriteStream", [0]);
}

const launchOptions = (args) => args.find((value) => value && typeof value === "object" && !Array.isArray(value) && !(value instanceof URL));

// A child whose working directory or owner-record variable is under a protected directory works in the real root.
function patchChildProcess() {
  for (const name of ["spawn", "spawnSync", "execFile", "execFileSync", "exec", "execSync", "fork"]) {
    const original = childProcess[name];
    if (typeof original !== "function") continue;
    const check = (args) => {
      if (rootState.dirs.size === 0 || realRootOptedIn()) return;
      const options = launchOptions(args) ?? {};
      refuseRealRoot(`launch a child (${name}) with its cwd`, options.cwd);
      refuseRealRoot(`launch a child (${name}) with RIGHTKIT_BUILD_OWNERSHIP_FILE`, (options.env ?? process.env).RIGHTKIT_BUILD_OWNERSHIP_FILE);
    };
    childProcess[name] = function guardedLaunch(...args) { check(args); return original.apply(this, args); };
    const custom = original[promisify.custom];
    if (custom) {
      Object.defineProperty(childProcess[name], promisify.custom, {
        value(...args) { try { check(args); } catch (error) { return Promise.reject(error); } return custom.apply(this, args); },
      });
    }
  }
}

export function installManagedRootGuard(roots = realManagedRoots()) {
  protectDirs(roots);
  if (rootState.patched) return rootState;
  rootState.patched = true;
  patchFs();
  patchChildProcess();
  syncBuiltinESMExports();
  return rootState;
}

// The real roots are read before the broker guard overwrites RIGHTKIT_DATA_ROOT with a throwaway one; the launch wrappers go on top of
// the broker guard's, so a real-root launch is refused first.
const realRootsAtStart = realManagedRoots();
installBrokerGuard();
installManagedRootGuard(realRootsAtStart);
