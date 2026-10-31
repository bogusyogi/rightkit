// The freshness census of a direct (Linux) snapshot compile (item 7 row 7.19; GATE-CONTRACT "Returned evidence", `freshness`:
// `{ unitsAtStart, fresh, notFresh, invalidationReasons }`, the census the measured-cache rule is judged from; E16: "the second
// build's census shows reuse").
//
// Cargo keeps no invalidation reason on disk: a fingerprint file holds the current hash only. The reason exists only in Cargo's own
// fingerprint log (`CARGO_LOG=cargo::core::compiler::fingerprint=info`: a `fingerprint dirty for <unit>` line, then `dirty: <reason>`),
// the same source the RustRight broker reads for its receipt (rustright-broker job_service/cargo_evidence.rs; attested-runs.md).
// The gate turns that log on for the cargo processes that compile a workspace, on THAT process only (never in the scan environment
// the repository's own tests and scripts inherit), reads it from the process's standard error, and keeps the lines out of the output
// unless the caller set CARGO_LOG itself (a caller's own CARGO_LOG is read the same way and left visible).
//
// Counts come from the target directory the way the broker takes them (rustright-policy cargo_freshness.rs): a unit is one directory
// of a `.fingerprint`; a unit whose newest file is at or after the census start was rebuilt (`notFresh`, a new unit included), the
// others were reused (`fresh`). Judging unexplained rebuilds belongs to item 11; this module only measures and records.
//
// GB-68 (BROKER-CONTRACT section 16; the shared vector test-support/vectors/freshness-evidence-vectors.v1.json, held by freshness-vectors.test.mjs):
//   - the log reader reads only the ROOT's lines: those before Cargo's first execution status word (`Compiling`, `Checking`, ... `Finished`); nested
//     Cargo chatter after it is neither counted nor stripped; a `failed to read ...fingerprint...` reason followed by a `Caused by:` not-found cause
//     (Unix `os error 2`, Windows `os error 3`) is `FingerprintMissing`, any other cause stays `unexplained`; every reason carries its `cause`;
//   - the census reads the SELECTED scope only: `<target>/<profile>/.fingerprint` and, for a `--target` triple, `<target>/<triple>/<profile>/.fingerprint`
//     (never every profile or triple, never `doc` or `package`), taken from the compiling process's own arguments and environment;
//   - a fingerprint directory or unit that exists and cannot be read is `fingerprint-census-read-failed`: unavailable, never an empty census and never a
//     first build. Only an absent directory is empty.
import fs from "node:fs";
import path from "node:path";

/** The `CARGO_LOG` filter the gate sets for a compiling cargo process (the broker's FINGERPRINT_LOG_FILTER). */
export const FINGERPRINT_LOG_FILTER = "cargo::core::compiler::fingerprint=info";
const FINGERPRINT_TARGET = "cargo::core::compiler::fingerprint";
export const FRESHNESS_SOURCE = "cargo-fingerprint-log+fingerprint-mtime";
/** Reasons kept in the receipt; the rest are counted. */
export const REASONS_CAP = 200;
/** Characters kept of one reason (Cargo prints it with `Debug`, paths and mtimes included). */
export const REASON_TEXT_CAP = 512;
/** Unit directories and files visited before the census is declared unavailable rather than partial. */
export const CENSUS_ENTRY_CAP = 250_000;
// A held partial line longer than this is not a log line.
const PARTIAL_CAP = 64 * 1024;

/** Cargo's execution-phase status words: the first one ends the window in which fingerprint lines are the root's (pre-resolution words do not). */
export const EXECUTION_STATUS = Object.freeze(["Compiling", "Checking", "Documenting", "Fresh", "Dirty", "Running", "Finished", "Scraping"]);
const EXECUTION_WORDS = new Set(EXECUTION_STATUS);

const SOURCE_OR_DEPENDENCY = new Set(["FsStatusOutdated", "PathToSourceChanged", "NumberOfDependenciesChanged", "UnitDependencyNameChanged", "UnitDependencyInfoChanged", "LocalLengthsChanged", "PrecalculatedComponentsChanged", "DepInfoOutputChanged", "RerunIfChangedOutputFileChanged", "RerunIfChangedOutputPathsChanged"]);
const ENV_RUSTFLAGS_PROFILE_FEATURES_TARGET = new Set(["EnvVarChanged", "EnvVarsChanged", "RustflagsChanged", "ProfileConfigurationChanged", "FeaturesChanged", "DeclaredFeaturesChanged", "TargetConfigurationChanged", "CompileKindChanged", "LocalFingerprintTypeChanged", "FingerprintMethodChanged", "ConfigSettingsChanged"]);

/**
 * The cause a reason's `kind` explains (RustRight cargo_evidence.rs `cause_of`; the vector's `causes`): the first build, a missing fingerprint (named apart, and still unexplained
 * by itself), a toolchain change, a source or dependency change, an environment, rustflags, profile, features or target change. Any other kind, one Cargo adds later included, is `unexplained`.
 */
export function causeOf(kind) {
  if (kind === "FreshBuild") return "first-build";
  if (kind === "FingerprintMissing") return "fingerprint-missing";
  if (kind === "RustcChanged") return "toolchain-changed";
  if (SOURCE_OR_DEPENDENCY.has(kind)) return "source-or-dependency-changed";
  if (ENV_RUSTFLAGS_PROFILE_FEATURES_TARGET.has(kind)) return "env-rustflags-profile-features-or-target-changed";
  return "unexplained";
}

/** Whether a `Caused by:` line says the fingerprint file was absent: `No such file or directory (os error 2)` on Unix, `... (os error 2)` or `... path specified. (os error 3)` on Windows. */
const missingFingerprint = (cause) => cause.endsWith("(os error 2)") || cause.endsWith("(os error 3)");

/** The profile directory a command line selects (RustRight `cargo_profile_directory`): `--release`/`-r`, `--profile X` (`dev` and `test` are `debug`, `bench` is `release`, a custom name is itself); nothing after `--`. */
export function cargoProfileDirectory(argv) {
  let profile = null; let release = false;
  const words = Array.isArray(argv) ? argv.map(String) : [];
  for (let i = 0; i < words.length && words[i] !== "--"; i += 1) {
    const item = words[i];
    if (item === "--release" || item === "-r") release = true;
    else if (item === "--profile") profile = words[i + 1] ?? null;
    else if (item.startsWith("--profile=")) profile = item.slice("--profile=".length);
  }
  if (profile === "dev" || profile === "test") return "debug";
  if (profile === "bench") return "release";
  if (profile !== null) return profile;
  return release ? "release" : "debug";
}

/** Whether a command line asks for JSON messages (`--message-format json*` in any spelling, before `--`). */
export function wantsCargoJson(argv) {
  const words = Array.isArray(argv) ? argv.map(String) : [];
  for (let i = 0; i < words.length; i += 1) {
    const item = words[i];
    if (item === "--") break;
    const value = item === "--message-format" ? words[i + 1] : item.startsWith("--message-format=") ? item.slice("--message-format=".length) : undefined;
    if (value !== undefined && value.split(",").some((segment) => segment.startsWith("json"))) return true;
  }
  return false;
}

/** The triples a command line names with `--target` (before `--`), then `CARGO_BUILD_TARGET` when the line names none. */
export function cargoTargetTriples(argv, env = {}) {
  const words = Array.isArray(argv) ? argv.map(String) : [];
  const found = [];
  for (let i = 0; i < words.length && words[i] !== "--"; i += 1) {
    const item = words[i];
    const value = item === "--target" ? words[i + 1] : item.startsWith("--target=") ? item.slice("--target=".length) : undefined;
    if (typeof value === "string" && value !== "" && !found.includes(value)) found.push(value);
  }
  if (found.length === 0) {
    const named = Object.entries(env ?? {}).find(([key]) => key.toUpperCase() === "CARGO_BUILD_TARGET");
    if (named && typeof named[1] === "string" && named[1].trim() !== "") for (const t of named[1].trim().split(/\s+/)) if (!found.includes(t)) found.push(t);
  }
  return found;
}

/**
 * The `.fingerprint` directories one compile selects in a target directory: `<target>/<triple>/<profile>/.fingerprint` for each selected triple, then `<target>/<profile>/.fingerprint`
 * (RustRight `fingerprint_census_start`). Never every profile or triple, never `doc` or `package`.
 */
export function selectedFingerprintDirectories(targetDir, { argv = [], triples = [] } = {}) {
  const profile = cargoProfileDirectory(argv);
  const found = [];
  for (const triple of triples) { const dir = path.join(targetDir, triple, profile, ".fingerprint"); if (!found.includes(dir)) found.push(dir); }
  const plain = path.join(targetDir, profile, ".fingerprint");
  if (!found.includes(plain)) found.push(plain);
  return found;
}

const stripAnsi = (text) => text.replace(/\u001b\[[^A-Za-z]*[A-Za-z]/g, "");
const afterPrefix = (message, prefixes) => {
  for (const prefix of prefixes) if (message.startsWith(prefix)) return message.slice(prefix.length);
  return null;
};

/** `name version` and the target of the unit a dirty line names (the span carries `target="..."`; the message `<name> v<ver> (<src>)/<mode>/...`). */
function unitOf(prefix, rest) {
  let target = null;
  const at = prefix.indexOf('target="');
  if (at >= 0) {
    const value = prefix.slice(at + 8);
    const end = value.indexOf('"');
    if (end >= 0) target = value.slice(0, end);
  }
  const head = rest.split("/")[0] ?? rest;
  const unit = (head.split(" (")[0] ?? head).trim();
  return { unit, target };
}

/** The reason's variant name (`FsStatusOutdated`, `RustflagsChanged`, ...): what a summary groups by. */
const reasonKind = (reason) => {
  const kind = reason.split(/[^\p{L}\p{N}_]/u)[0];
  return kind ? kind : "unknown";
};

/** Reads Cargo's fingerprint log from a process's standard error; `strip` keeps its lines out of what the caller sees. */
export class FingerprintLog {
  constructor({ strip = false } = {}) {
    this.strip = strip;
    this.partial = "";
    this.current = null;
    this.dirtyUnits = 0;
    this.reasons = [];
    this.truncated = 0;
    // Cargo printed its first execution-phase status line: later fingerprint lines are not the root's (a nested Cargo's).
    this.executing = false;
    // The reason just recorded was Cargo's `failed to read` of a `.fingerprint` file: its `Caused by:` chain follows on the next lines and says whether the file was simply absent.
    this.awaitingCause = null;
  }

  /** One chunk of standard error in; what the caller sees out. Without stripping that is the chunk itself. */
  feed(chunk) {
    this.partial += String(chunk);
    let shown = "";
    for (let at = this.partial.indexOf("\n"); at >= 0; at = this.partial.indexOf("\n")) {
      const line = this.partial.slice(0, at + 1);
      this.partial = this.partial.slice(at + 1);
      const isLog = this.#line(line);
      if (!isLog || !this.strip) shown += line;
    }
    if (this.partial.length > PARTIAL_CAP) {
      const rest = this.partial;
      this.partial = "";
      if (this.strip) shown += rest;
    }
    return this.strip ? shown : String(chunk);
  }

  /** The end of the stream: a held partial line is judged and, when stripping and it is not a log line, returned. */
  finish() {
    const rest = this.partial;
    this.partial = "";
    if (rest === "" || this.#line(rest) || !this.strip) return "";
    return rest;
  }

  /** Whether `raw` is one of the ROOT's fingerprint lines (counted, and stripped when stripping). */
  #line(raw) {
    const line = stripAnsi(raw.replace(/\s+$/, ""));
    // GB-68: after the first execution status word nothing is the root's: nested Cargo chatter is neither counted nor stripped.
    if (this.executing) { this.awaitingCause = null; return false; }
    const first = line.trim().split(/\s+/)[0];
    if (EXECUTION_WORDS.has(first)) { this.executing = true; return false; }
    // The `Caused by:` chain of a `failed to read` reason: the heading, then the cause, indented. Blank lines inside the chain belong to it.
    if (this.awaitingCause !== null) {
      const text = line.trim();
      if (text === "") return true;
      if (!this.awaitingCause.headingSeen && text === "Caused by:") { this.awaitingCause.headingSeen = true; return true; }
      const { index, headingSeen } = this.awaitingCause;
      this.awaitingCause = null;
      if (headingSeen && /^\s/.test(line)) {
        if (missingFingerprint(text) && this.reasons[index]) { this.reasons[index].kind = "FingerprintMissing"; this.reasons[index].cause = causeOf("FingerprintMissing"); }
        return true;
      }
    }
    const at = line.indexOf(FINGERPRINT_TARGET);
    if (at < 0) return false;
    const message = line.slice(at + FINGERPRINT_TARGET.length).replace(/^:+/, "").trim();
    const unitLine = afterPrefix(message, ["fingerprint dirty for ", "fingerprint error for "]);
    if (unitLine !== null) {
      this.dirtyUnits += 1;
      this.current = unitOf(line.slice(0, at), unitLine);
      return true;
    }
    const reason = afterPrefix(message, ["dirty:", "err:"]);
    if (reason !== null) this.#record(reason.trim());
    return true;
  }

  #record(reason) {
    if (this.reasons.length >= REASONS_CAP) { this.truncated += 1; return; }
    const { unit, target } = this.current ?? { unit: "unknown", target: null };
    const chars = Array.from(reason);
    const text = chars.length > REASON_TEXT_CAP ? `${chars.slice(0, REASON_TEXT_CAP).join("")}\u2026` : reason;
    const kind = reasonKind(reason);
    this.reasons.push({ unit, target, kind, cause: causeOf(kind), reason: text });
    const readsFingerprint = reason.startsWith("failed to read `") && (reason.includes("/.fingerprint/") || reason.includes("\\.fingerprint\\"));
    this.awaitingCause = readsFingerprint ? { index: this.reasons.length - 1, headingSeen: false } : null;
  }

  /** What another log's reading adds to this one (the census keeps one aggregate over every compiling process). */
  merge(other) {
    this.dirtyUnits += other.dirtyUnits;
    this.truncated += other.truncated;
    for (const reason of other.reasons) {
      if (this.reasons.length >= REASONS_CAP) this.truncated += 1;
      else this.reasons.push(reason);
    }
  }
}

// Cargo subcommands that compile a workspace's units (the wrapper's COMPILE set without metadata and package, which the census
// does not count). `--` ends cargo's own arguments; `--color` and `--explain` take a value.
const COMPILING = new Set(["build", "b", "check", "c", "test", "t", "bench", "run", "r", "doc", "d", "rustc", "rustdoc", "clippy"]);
const GLOBAL_VALUE = new Set(["--color", "--explain"]);

/** True when `command args` is a cargo process that compiles a workspace's units (its fingerprint log is the census's). */
export function compilesWorkspace(command, args) {
  if (path.basename(String(command)).replace(/\.exe$/i, "") !== "cargo") return false;
  const words = Array.isArray(args) ? args : [];
  let i = typeof words[0] === "string" && words[0].startsWith("+") ? 1 : 0;
  for (; i < words.length; i += 1) {
    const w = String(words[i]);
    if (w === "--") return false;
    if (GLOBAL_VALUE.has(w)) { i += 1; continue; }
    if (w.startsWith("-")) continue;
    return COMPILING.has(w);
  }
  return false;
}

/** An error that says the path is not there (an empty profile), as opposed to being there and unreadable. */
const isAbsent = (error) => error?.code === "ENOENT";

/** The fingerprint directory's entries: `{ names }`, `{ names: [] }` when it is absent, or `{ error }` when it exists and cannot be read (a file, permission, I/O: never read as empty). */
function readFingerprintDirectory(dir, fsApi) {
  try { return { names: fsApi.readdirSync(dir) }; } catch (error) { return isAbsent(error) ? { names: [] } : { error: `${dir}: ${error?.code ?? error?.message ?? "unreadable"}` }; }
}

/**
 * The `build.target` triples `.cargo/config.toml` / `.cargo/config` name for a compile run from `cwd` (the process's own directory upward, then `$CARGO_HOME`): the compile's output then
 * sits under `<target>/<triple>/<profile>`, so the selected scope must name it as `--target` would. A simple reader of the `[build]` table's `target` string or string list.
 */
export function configuredBuildTargets(cwd, { env = {}, fsApi = fs } = {}) {
  const files = [];
  if (typeof cwd === "string" && cwd) for (let dir = path.resolve(cwd); ; dir = path.dirname(dir)) { files.push(path.join(dir, ".cargo", "config.toml"), path.join(dir, ".cargo", "config")); if (path.dirname(dir) === dir) break; }
  const home = Object.entries(env ?? {}).find(([key]) => key.toUpperCase() === "CARGO_HOME")?.[1];
  if (typeof home === "string" && home) files.push(path.join(home, "config.toml"), path.join(home, "config"));
  for (const file of files) {
    let text;
    try { text = fsApi.readFileSync(file, "utf8"); } catch { continue; }
    let inBuild = false;
    for (const raw of String(text).split(/\r?\n/)) {
      const line = raw.replace(/#.*$/, "").trim();
      const table = /^\[\s*([^\]]+?)\s*\]$/.exec(line);
      if (table) { inBuild = table[1] === "build"; continue; }
      const dotted = /^build\.target\s*=\s*(.+)$/.exec(line);
      const plain = inBuild ? /^target\s*=\s*(.+)$/.exec(line) : null;
      const value = (dotted ?? plain)?.[1];
      if (value !== undefined) return [...value.matchAll(/["']([^"']+)["']/g)].map((m) => m[1]);
    }
  }
  return [];
}

/**
 * One census for one stage of a direct compile: `start()` before the scans, `wrapSpawn(spawn)` around the scan runner's spawner (it selects each compiling cargo process's scope and
 * turns the fingerprint log on for it only), `finish()` after them. `start({ argv, triples, env })` selects a scope itself (a caller that knows the command).
 *
 * The scope is what the compiling processes SELECT (cargoProfileDirectory, cargoTargetTriples), not every profile directory of the target: each selected directory's baseline is read
 * before the first process that selects it, and `since` is the census start. A directory that exists and cannot be read, at the baseline or in the walk, makes the census
 * unavailable (`fingerprint-census-read-failed`); with no compiling process there is no scope and the census is unavailable (`fingerprint-census-no-compiling-process`).
 */
export function createFreshnessCensus({ targetDirs, now = Date.now, fsApi = fs, cap = CENSUS_ENTRY_CAP } = {}) {
  const dirs = [...new Set(Object.values(targetDirs ?? {}).filter((d) => typeof d === "string" && d))];
  const log = new FingerprintLog({ strip: false });
  let since = null; let processes = 0; let outputCut = false;
  const baselines = new Map(); // fingerprint directory -> { count } | { error }
  let unitsAtStart = 0; let capped = false; let readError = null; let selected = false;

  const select = ({ argv = [], triples = null, env = {}, cwd = null } = {}) => {
    const named = triples ?? [...cargoTargetTriples(argv, env), ...(cargoTargetTriples(argv, env).length === 0 ? configuredBuildTargets(cwd, { env, fsApi }) : [])];
    for (const targetDir of dirs) {
      for (const dir of selectedFingerprintDirectories(targetDir, { argv, triples: [...new Set(named)] })) {
        selected = true;
        if (baselines.has(dir)) continue;
        const read = readFingerprintDirectory(dir, fsApi);
        if (read.error !== undefined) { baselines.set(dir, { error: read.error }); readError ??= read.error; continue; }
        baselines.set(dir, { count: read.names.length });
        unitsAtStart += read.names.length;
        if (unitsAtStart > cap) capped = true;
      }
    }
  };

  /** Walks the selected directories: the units' newest file times, or why the walk cannot be trusted. */
  const walk = () => {
    const units = new Map();
    let visited = 0;
    for (const dir of baselines.keys()) {
      const read = readFingerprintDirectory(dir, fsApi);
      if (read.error !== undefined) return { reason: "fingerprint-census-read-failed" };
      for (const name of read.names) {
        visited += 1;
        if (visited > cap) return { reason: "fingerprint-census-entry-cap-exceeded" };
        const unitDir = path.join(dir, name);
        let newest = 0;
        let files = [];
        try { files = fsApi.readdirSync(unitDir); } catch (error) {
          // A unit that vanished meanwhile, or is a plain file, holds no files; any other failure means its freshness was never read.
          if (!isAbsent(error) && error?.code !== "ENOTDIR") return { reason: "fingerprint-census-read-failed" };
        }
        for (const file of files) {
          visited += 1;
          if (visited > cap) return { reason: "fingerprint-census-entry-cap-exceeded" };
          try { newest = Math.max(newest, fsApi.statSync(path.join(unitDir, file)).mtimeMs); } catch { /* vanished: it holds no time */ }
        }
        units.set(unitDir, newest);
      }
    }
    return { units };
  };

  return {
    start(selection) { since = now(); if (selection) select(selection); },
    select,
    /** What the selection reads at the start: the directories, the units counted there, whether the entry cap was passed and the first read error. */
    scope() { return { directories: [...baselines.keys()], unitsAtStart, capped, readError }; },
    wrapSpawn(spawn) {
      return async (command, args, options = {}) => {
        if (!compilesWorkspace(command, args)) return spawn(command, args, options);
        const env = options.env ?? process.env;
        if (since !== null) select({ argv: Array.isArray(args) ? args : [], env, cwd: options.cwd ?? process.cwd() });
        // A caller's own CARGO_LOG stands and stays visible; the gate's own is kept out of the output.
        const ours = !Object.keys(env).some((k) => k.toUpperCase() === "CARGO_LOG");
        const result = await spawn(command, args, ours ? { ...options, env: { ...env, CARGO_LOG: FINGERPRINT_LOG_FILTER } } : options);
        processes += 1;
        if (!result || typeof result.stderr !== "string") return result;
        if (result.truncated) outputCut = true;
        const reading = new FingerprintLog({ strip: ours });
        const shown = reading.feed(result.stderr) + reading.finish();
        log.merge(reading);
        return { ...result, stderr: shown };
      };
    },
    finish() {
      const base = { source: FRESHNESS_SOURCE, cargoProcesses: processes, dirtyUnits: log.dirtyUnits, truncated: log.truncated, ...(outputCut ? { outputCut: true } : {}) };
      const unavailable = (reason) => ({ unitsAtStart: 0, fresh: 0, notFresh: 0, invalidationReasons: log.reasons, available: false, reason, ...base });
      if (since === null) return unavailable("fingerprint-census-not-started");
      if (!selected) return unavailable("fingerprint-census-no-compiling-process");
      if (capped) return unavailable("fingerprint-census-entry-cap-exceeded");
      if (readError !== null) return unavailable("fingerprint-census-read-failed");
      const end = walk();
      if (end.reason) return unavailable(end.reason);
      if (end.units.size === 0) return unavailable("no-fingerprint-units-found");
      let notFresh = 0;
      for (const newest of end.units.values()) if (newest >= since) notFresh += 1;
      return { unitsAtStart, fresh: end.units.size - notFresh, notFresh, invalidationReasons: log.reasons, available: true, ...base };
    },
  };
}

const isCount = (n) => Number.isInteger(n) && n >= 0;

/** What is wrong with a `freshness` object (an empty list: it is the census the contract names, well formed). */
export function validateFreshness(freshness) {
  const problems = [];
  if (!freshness || typeof freshness !== "object" || Array.isArray(freshness)) return ["freshness is absent"];
  for (const key of ["unitsAtStart", "fresh", "notFresh"]) if (!isCount(freshness[key])) problems.push(`freshness.${key} is not a count`);
  if (!Array.isArray(freshness.invalidationReasons)) problems.push("freshness.invalidationReasons is not a list");
  else freshness.invalidationReasons.forEach((r, i) => { if (!r || typeof r.unit !== "string" || typeof r.kind !== "string" || typeof r.reason !== "string") problems.push(`freshness.invalidationReasons[${i}] has no unit, kind and reason`); else if (r.cause !== undefined && typeof r.cause !== "string") problems.push(`freshness.invalidationReasons[${i}].cause is not a string`); });
  if (freshness.available !== undefined && typeof freshness.available !== "boolean") problems.push("freshness.available is not a boolean");
  if (freshness.available === false && typeof freshness.reason !== "string") problems.push("an unavailable census names no reason");
  return problems;
}
