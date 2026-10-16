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
// Opt-in for a real end-to-end run: RIGHTKIT_TEST_REAL_BROKER=1 (mirrors RIGHTKIT_TEST_REAL_DMG).
// Child processes inherit the guard through NODE_OPTIONS, which installation appends to.
// Not shipped: lives under test-support/, outside package.json "files".
import childProcess from "node:child_process";
import { appendFileSync, mkdtempSync, realpathSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

export const OPT_IN_ENV = "RIGHTKIT_TEST_REAL_BROKER";
export const LOG_ENV = "RIGHTKIT_BROKER_GUARD_LOG";
const INSTALLED = Symbol.for("rightkit.release.brokerGuard");
const GUARD_FILE = fileURLToPath(import.meta.url);
const GUARD_IMPORT = `--import=${pathToFileURL(GUARD_FILE).href}`;
const BROKER_BINARIES = new Set(["cargo", "rightkit", "rustright"]);
const MANAGED_SEGMENT = /[\\/](?:agent-bin|runtime-current|build-control|\.rightkit-managed)(?:[\\/]|$)/i;
const CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index";

// Cargo metadata for one manifest, shaped like `cargo metadata --no-deps` for the fields the
// release contracts read (packages[].dependencies, manifest_path, workspace_root, target_directory).
const METADATA_SCRIPT = String.raw`
import json, pathlib, sys, tomllib
manifest = pathlib.Path(sys.argv[1]).resolve()
try:
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
except (OSError, tomllib.TOMLDecodeError) as error:
    sys.stderr.write("error: failed to parse manifest %s: %s\n" % (manifest, error))
    sys.exit(101)
root = manifest.parent
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"

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
           "target_directory": str(root / "target"), "metadata": None, "version": 1}, sys.stdout)
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
export function brokerLaunch(file) {
  const text = String(file ?? "");
  const base = (text.split(/[\\/]/).pop() ?? "").toLowerCase().replace(/\.exe$/, "");
  const reaches = BROKER_BINARIES.has(base) || MANAGED_SEGMENT.test(text);
  return reaches && !insideTemp(text);
}

// Returns the cargo argument vector for a broker-reaching launch, or null when it is not a cargo command.
function cargoArguments(file, argv) {
  const base = (String(file).split(/[\\/]/).pop() ?? "").toLowerCase().replace(/\.exe$/, "");
  if (base === "cargo") return argv;
  if ((base === "rustright" || base === "rightkit") && argv[0] === "cargo") return argv.slice(1);
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

function stubMetadata(cargoArgv, cwd) {
  const index = cargoArgv.indexOf("--manifest-path");
  const manifest = index >= 0 ? cargoArgv[index + 1] : undefined;
  if (!manifest) return { status: 101, stdout: "", stderr: "error: stub Cargo metadata requires --manifest-path" };
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
  process.env.RIGHTKIT_GUARD_DATA_ROOT ??= mkdtempSync(path.join(os.tmpdir(), "rightkit-release-test-root-"));
  process.env.RIGHTKIT_DATA_ROOT = process.env.RIGHTKIT_GUARD_DATA_ROOT;
  // Child Node processes (release.mjs, build-release.mjs, cargo-target.mjs, fixtures) load the guard too.
  const current = process.env.NODE_OPTIONS ?? "";
  if (!current.includes(GUARD_IMPORT)) process.env.NODE_OPTIONS = `${current} ${GUARD_IMPORT}`.trim();
  return state;
}

installBrokerGuard();
