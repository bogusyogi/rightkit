// `right-release deps --cargo-lock --manifest <Cargo.toml> --pin <crate>=<version> [--pin …]` (rightkit#1305): the sanctioned, non-compiling
// Cargo.lock update door. An app adopting a newly published RightKit crate refreshes its lock here, never by hand and never by a build.
//
// - On Mac and Dell (the broker) each pin is one uncarried root request, `cargo update --manifest-path <m> -p <crate> --precise <v>`,
//   sent through the managed client (runCargoSync) in the PRIMARY checkout of the manifest's workspace. Cargo takes one `--precise` per
//   `-p`, so several pins are several requests in sequence inside this one door run. No `--config`, `--target-dir` or `+toolchain` is ever
//   passed: the broker refuses those as redirects.
// - On Linux (direct) the same argv runs as plain `cargo update` (runCargoSync routes it).
// - The door writes only the workspace's Cargo.lock. Before the first request it records `git status --porcelain` and the sha256 of every
//   tracked file under the workspace; after each request it refuses (RK_LOCK_UPDATE_OUTSIDE_LOCK) if anything else changed, and restores
//   the lock to its prior bytes. The broker does not restrict writes to Cargo.lock, so this check is the enforcement.
// - A gate stage's process (RK_GATE_CHECKOUT or the carried cargo map in its environment) never updates a lock: refused as a snapshot.
// - It is never a build: nothing compiles, and a failed request restores the lock too.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { CARRIED_CARGO_MAP_ENV } from "./carried-cargo.mjs";
import { CHECKOUT_ENV } from "./gate-stages.mjs";

export const LOCK_UPDATE_CODES = Object.freeze({
  ARGS: "RK_LOCK_UPDATE_ARGS",
  PIN: "RK_LOCK_UPDATE_PIN",
  SNAPSHOT: "RK_LOCK_UPDATE_SNAPSHOT",
  CHECKOUT: "RK_LOCK_UPDATE_CHECKOUT",
  NO_LOCK: "RK_LOCK_UPDATE_NO_LOCK",
  FAILED: "RK_LOCK_UPDATE_FAILED",
  OUTSIDE_LOCK: "RK_LOCK_UPDATE_OUTSIDE_LOCK",
});

export class CargoLockUpdateError extends Error {
  constructor(code, message) { super(message); this.name = "CargoLockUpdateError"; this.code = code; }
}

const USAGE = "usage: right-release deps --cargo-lock --manifest <Cargo.toml> --pin <crate>=<version> [--pin <crate>=<version>]...";
const CRATE = /^[A-Za-z][A-Za-z0-9_-]*$/;
// A semantic version (SemVer 2.0.0), the form `cargo update --precise` takes.
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

/** `[{ crate, version }]` from `--pin <crate>=<version>` values. Throws RK_LOCK_UPDATE_PIN on a malformed or unparsable pin. */
export function parsePins(values) {
  if (!Array.isArray(values) || values.length === 0) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.ARGS, "at least one --pin <crate>=<version> is required");
  const seen = new Set();
  return values.map((raw) => {
    const text = String(raw);
    const eq = text.indexOf("=");
    const crate = eq < 0 ? "" : text.slice(0, eq);
    const version = eq < 0 ? "" : text.slice(eq + 1);
    if (!CRATE.test(crate)) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.PIN, `--pin ${text}: ${JSON.stringify(crate)} is not a Cargo package name`);
    if (!SEMVER.test(version)) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.PIN, `--pin ${text}: ${JSON.stringify(version)} is not a semantic version`);
    if (seen.has(crate)) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.PIN, `--pin ${crate} is given more than once`);
    seen.add(crate);
    return { crate, version };
  });
}

/** The argv of one pin's request. Never carries --config, --target-dir or a +toolchain. */
export function cargoUpdateArgv({ manifest, crate, version }) {
  return ["update", "--manifest-path", manifest, "-p", crate, "--precise", version];
}

const real = (p) => { try { return realpathSync.native(p); } catch { return path.resolve(p); } };
/** A path with its directory resolved, so a path that no longer exists still compares equal to its live spelling. */
const canonical = (p) => path.join(real(path.dirname(p)), path.basename(p));

function snapshotRefusal(env) {
  if (env?.[CHECKOUT_ENV]) return `${CHECKOUT_ENV} is set: this process runs inside a gate stage's snapshot`;
  if (env?.[CARRIED_CARGO_MAP_ENV]) return `${CARRIED_CARGO_MAP_ENV} is set: this process runs inside a gate stage's snapshot`;
  return null;
}

const gitRun = (args, cwd) => {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  return { status: result.status, stdout: String(result.stdout ?? "") };
};

function primaryCheckoutProblem(ws, git) {
  const dir = git(["rev-parse", "--git-dir"], ws);
  const common = git(["rev-parse", "--git-common-dir"], ws);
  if (dir.status !== 0 || common.status !== 0) return `${ws} is not inside a git checkout`;
  if (real(path.resolve(ws, dir.stdout.trim())) !== real(path.resolve(ws, common.stdout.trim()))) {
    return `${ws} is a linked worktree; the lock is updated only in the primary checkout`;
  }
  return null;
}

const digest = (file) => {
  try { return createHash("sha256").update(readFileSync(file)).digest("hex"); } catch (error) { return error?.code === "ENOENT" ? "<missing>" : "<unreadable>"; }
};

/** `git status --porcelain -z` entries as `{ key, paths }`; a rename or copy carries its source path as the next token. */
function statusEntries(text) {
  const tokens = text.split("\0");
  const entries = [];
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (token.length < 4) continue;
    const entry = { key: token, paths: [token.slice(3)] };
    if (/[RC]/.test(token.slice(0, 2)) && tokens[i + 1]) { entry.paths.push(tokens[++i]); }
    entries.push(entry);
  }
  return entries;
}

/** The state the outside-lock check compares: tracked-file digests under the workspace and the porcelain status. */
function workspaceState(ws, top, git) {
  const tracked = git(["ls-files", "-z", "--", "."], ws);
  if (tracked.status !== 0) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.CHECKOUT, `git ls-files failed under ${ws}`);
  const hashes = new Map();
  for (const rel of tracked.stdout.split("\0")) if (rel) hashes.set(rel, digest(path.join(ws, rel)));
  const status = git(["status", "--porcelain", "-z", "--untracked-files=all"], ws);
  if (status.status !== 0) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.CHECKOUT, `git status failed under ${ws}`);
  return { top, hashes, entries: statusEntries(status.stdout) };
}

/** Canonical paths that changed between the baseline and now, other than the lock. */
function outsideChanges(before, after, ws, lockPath) {
  const changed = new Set();
  for (const rel of new Set([...before.hashes.keys(), ...after.hashes.keys()])) {
    if (before.hashes.get(rel) !== after.hashes.get(rel)) changed.add(canonical(path.join(ws, rel)));
  }
  const beforeKeys = new Set(before.entries.map((entry) => entry.key));
  const afterKeys = new Set(after.entries.map((entry) => entry.key));
  for (const entry of [...before.entries, ...after.entries]) {
    if (beforeKeys.has(entry.key) !== afterKeys.has(entry.key)) for (const p of entry.paths) changed.add(canonical(path.resolve(before.top, p)));
  }
  changed.delete(canonical(lockPath));
  return [...changed].sort();
}

function restoreLock(lock, bytes) {
  if (bytes === null) rmSync(lock, { force: true });
  else writeFileSync(lock, bytes);
}

/**
 * Refresh the Cargo.lock of the workspace that holds `manifest`, one request per pin. Returns `{ workspace, lock, pins }`; throws
 * CargoLockUpdateError with a LOCK_UPDATE_CODES code (the lock is restored first on every failure after the requests start).
 * `run` is the managed cargo runner (injectable), `git` the git runner (injectable), `env` the environment of the requests.
 */
export function updateCargoLock({ manifest, pins, cwd = process.cwd(), env = process.env, run = runCargoSync, git = gitRun, log = () => {} } = {}) {
  const refusal = snapshotRefusal(env);
  if (refusal) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.SNAPSHOT, `refused: ${refusal}`);
  const parsed = parsePins(pins);
  if (typeof manifest !== "string" || !manifest) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.ARGS, "--manifest <Cargo.toml> is required");
  const manifestPath = path.resolve(cwd, manifest);
  if (!existsSync(manifestPath) || !statSync(manifestPath).isFile()) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.ARGS, `manifest not found: ${manifestPath}`);
  const ws = path.dirname(manifestPath);
  const lock = path.join(ws, "Cargo.lock");
  if (!existsSync(lock)) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.NO_LOCK, `${lock} does not exist; an unlocked workspace has no lock to update`);
  const problem = primaryCheckoutProblem(ws, git);
  if (problem) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.CHECKOUT, `refused: ${problem}`);
  const top = canonical(path.resolve(ws, git(["rev-parse", "--show-toplevel"], ws).stdout.trim()));
  const before = workspaceState(ws, top, git);
  const lockBytes = readFileSync(lock);
  try {
    for (const { crate, version } of parsed) {
      const argv = cargoUpdateArgv({ manifest: manifestPath, crate, version });
      log(`right-deps: cargo ${argv.join(" ")}  (${ws})`);
      const result = run(argv, { cwd: ws, env, stdio: "inherit" });
      if (result?.error) throw new CargoLockUpdateError(result.error.code ?? LOCK_UPDATE_CODES.FAILED, `cargo update -p ${crate} --precise ${version} could not run (${result.error.message})`);
      if (result?.status !== 0) throw new CargoLockUpdateError(LOCK_UPDATE_CODES.FAILED, `cargo update -p ${crate} --precise ${version} exited ${result?.status ?? "without status"}`);
      const outside = outsideChanges(before, workspaceState(ws, top, git), ws, lock);
      if (outside.length > 0) {
        throw new CargoLockUpdateError(LOCK_UPDATE_CODES.OUTSIDE_LOCK, `cargo update changed files other than ${lock}: ${outside.join(", ")}; the lock was restored (other files are left for review)`);
      }
    }
  } catch (error) {
    restoreLock(lock, lockBytes);
    throw error;
  }
  return { workspace: ws, lock, pins: parsed };
}

/**
 * The `deps --cargo-lock` command line (argv without the `--cargo-lock` word). Returns the exit code: 0 updated, 1 refused or failed,
 * 2 usage. Prints the typed refusal code on stderr.
 */
export function runCargoLockCommand(argv, { cwd = process.cwd(), env = process.env, run, git, log = (line) => console.log(line), fail = (line) => console.error(line) } = {}) {
  let manifest;
  const pins = [];
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--") continue;
    if (arg === "--manifest" || arg === "--pin") {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) { fail(`right-deps: ${arg} needs a value\n${USAGE}`); return 2; }
      i++;
      if (arg === "--pin") pins.push(value);
      else if (manifest !== undefined) { fail(`right-deps: --manifest given twice\n${USAGE}`); return 2; }
      else manifest = value;
    } else {
      fail(`right-deps: unknown argument: ${arg}\n${USAGE}`);
      return 2;
    }
  }
  if (manifest === undefined || pins.length === 0) { fail(USAGE); return 2; }
  try {
    const { lock, pins: applied } = updateCargoLock({ manifest, pins, cwd, env, run, git, log });
    log(`right-deps: ${lock} updated (${applied.map((pin) => `${pin.crate} ${pin.version}`).join(", ")}); no other file changed`);
    return 0;
  } catch (error) {
    if (error instanceof CargoLockUpdateError) { fail(`right-deps: ${error.code}: ${error.message}`); return 1; }
    fail(`right-deps: ${error?.message ?? error}`);
    return 1;
  }
}
