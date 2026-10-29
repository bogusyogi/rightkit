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

  #line(raw) {
    const line = stripAnsi(raw.replace(/\s+$/, ""));
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
    const text = chars.length > REASON_TEXT_CAP ? `${chars.slice(0, REASON_TEXT_CAP).join("")}…` : reason;
    this.reasons.push({ unit, target, kind: reasonKind(reason), reason: text });
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

// Directories never searched for `.fingerprint`s: rustdoc output and packaging verification are not a profile's units.
const NOT_PROFILES = new Set(["doc", "package", "tmp", "incremental"]);

/** The `.fingerprint` directories of one target directory: `<target>/<profile>/.fingerprint` and `<target>/<triple>/<profile>/.fingerprint`. */
export function fingerprintDirectories(targetDir, fsApi = fs) {
  const found = [];
  const subdirs = (dir) => {
    try { return fsApi.readdirSync(dir, { withFileTypes: true }).filter((e) => e.isDirectory() && !NOT_PROFILES.has(e.name)).map((e) => e.name); } catch { return []; }
  };
  const has = (dir) => { try { return fsApi.statSync(dir).isDirectory(); } catch { return false; } };
  for (const first of subdirs(targetDir)) {
    const direct = path.join(targetDir, first, ".fingerprint");
    if (has(direct)) { found.push(direct); continue; }
    for (const second of subdirs(path.join(targetDir, first))) {
      const nested = path.join(targetDir, first, second, ".fingerprint");
      if (has(nested)) found.push(nested);
    }
  }
  return found;
}

/**
 * Walks every fingerprint directory of `targetDirs`. `withTimes: false` only counts units; `withTimes: true` also takes each unit's
 * newest file time. `capped` is set when more than `cap` entries (units and files) were visited.
 */
export function walkFingerprints(targetDirs, { withTimes, cap = CENSUS_ENTRY_CAP, fsApi = fs } = {}) {
  const units = new Map();
  let visited = 0;
  for (const targetDir of targetDirs) {
    for (const fpDir of fingerprintDirectories(targetDir, fsApi)) {
      let names;
      try { names = fsApi.readdirSync(fpDir); } catch { continue; }
      for (const name of names) {
        visited += 1;
        if (visited > cap) return { units, capped: true };
        const unitDir = path.join(fpDir, name);
        if (!withTimes) { units.set(unitDir, 0); continue; }
        let newest = 0;
        try {
          const own = fsApi.statSync(unitDir);
          newest = own.mtimeMs;
          if (own.isDirectory()) {
            for (const file of fsApi.readdirSync(unitDir)) {
              visited += 1;
              if (visited > cap) return { units, capped: true };
              try { newest = Math.max(newest, fsApi.statSync(path.join(unitDir, file)).mtimeMs); } catch { /* vanished: ignore */ }
            }
          }
        } catch { /* vanished: counted with the time it had */ }
        units.set(unitDir, newest);
      }
    }
  }
  return { units, capped: false };
}

/**
 * One census for one stage of a direct compile: `start()` before the scans, `wrapSpawn(spawn)` around the scan runner's spawner
 * (it turns the fingerprint log on for the compiling cargo processes only), `finish()` after them.
 */
export function createFreshnessCensus({ targetDirs, now = Date.now, fsApi = fs, cap = CENSUS_ENTRY_CAP } = {}) {
  const dirs = [...new Set(Object.values(targetDirs ?? {}).filter((d) => typeof d === "string" && d))];
  const log = new FingerprintLog({ strip: false });
  let since = null; let atStart = null; let processes = 0; let outputCut = false;
  return {
    start() { since = now(); atStart = walkFingerprints(dirs, { withTimes: false, cap, fsApi }); },
    wrapSpawn(spawn) {
      return async (command, args, options = {}) => {
        if (!compilesWorkspace(command, args)) return spawn(command, args, options);
        const env = options.env ?? process.env;
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
      if (since === null || atStart === null) return unavailable("fingerprint-census-not-started");
      const end = walkFingerprints(dirs, { withTimes: true, cap, fsApi });
      if (atStart.capped || end.capped) return unavailable("fingerprint-census-entry-cap-exceeded");
      if (end.units.size === 0) return unavailable("no-fingerprint-units-found");
      let notFresh = 0;
      for (const newest of end.units.values()) if (newest >= since) notFresh += 1;
      return { unitsAtStart: atStart.units.size, fresh: end.units.size - notFresh, notFresh, invalidationReasons: log.reasons, available: true, ...base };
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
  else freshness.invalidationReasons.forEach((r, i) => { if (!r || typeof r.unit !== "string" || typeof r.kind !== "string" || typeof r.reason !== "string") problems.push(`freshness.invalidationReasons[${i}] has no unit, kind and reason`); });
  if (freshness.available !== undefined && typeof freshness.available !== "boolean") problems.push("freshness.available is not a boolean");
  if (freshness.available === false && typeof freshness.reason !== "string") problems.push("an unavailable census names no reason");
  return problems;
}
