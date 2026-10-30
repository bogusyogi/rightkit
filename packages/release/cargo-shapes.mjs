// The ONE versioned command-shape table of the gate's Cargo calls (GB-01, GB-02; audit gate-broker-2026-10-30).
//
// RustRight owns the admitted grammar (rustright-policy command_policy.rs `ThinCommandPolicy::classify`, `snapshot_read_only`,
// `grant_command`, `carried_refusal`, and `normalize_build_request` for the standard aliases). RightKit owns the callers. Both read one
// vector: docs/contracts/cargo-shapes.fixtures.json of RustRight (schema `rustright.cargo-shapes.fixtures.v1`), copied byte for byte into
// test-support/vectors/ with its source identity pinned (vectors-source.json). Every row of it is judged here by `classifyCargoArgv`, so
// the wrapper (cargo-target-shim.mjs), the carried call (snapshot-request.mjs) and the planners (stage-commands.mjs) decide from the same
// function and cannot drift from the broker: an accepted row reaches the broker's parser, a refused row launches zero processes.
//
// `argv` is cargo's argument list without the `cargo` word. Nothing here rewrites a caller's arguments except the five standard aliases at
// the subcommand position (`b c d r t`), which the broker expands before it classifies (a committed `.cargo/config.toml` `[alias]` can
// otherwise redefine what was classified). A `fmt` argument is never changed and a plugin restriction is never weakened.

export const COMMAND_SHAPES_SCHEMA = "rightkit.command-shapes/1";
export const COMMAND_SHAPES_VERSION = 1;
/** The RustRight vector this table is exercised against (its `schema`); a change of either side is a failing test, not a silent drift. */
export const COMMAND_SHAPES_VECTOR_SCHEMA = "rustright.cargo-shapes.fixtures.v1";

/** Cargo's standard aliases (rustright cargo_argv.rs CARGO_ALIASES), expanded at the subcommand position before classification. */
export const CARGO_COMMAND_ALIASES = Object.freeze({ b: "build", c: "check", d: "doc", r: "run", t: "test" });
export const canonicalCargoCommand = (name) => (typeof name === "string" && Object.hasOwn(CARGO_COMMAND_ALIASES, name) ? CARGO_COMMAND_ALIASES[name] : name);

const VALUE_GLOBALS = new Set(["--manifest-path", "--config", "-C", "-Z", "--color", "--target-dir"]);
const PLAIN_GLOBALS = new Set(["--locked", "--frozen", "--offline", "--quiet", "-q", "--verbose", "-v"]);
const VERSION_FLAGS = new Set(["--version", "-V", "-vV"]);

/**
 * Where the subcommand sits in `argv` (rustright command_policy.rs `command_index`): after an optional `+toolchain` and the global options (the
 * ones that take a value are skipped with it), before any `--`. `--version`, `-V` and `-vV` are the command. null when there is none.
 */
export function cargoCommandIndex(argv) {
  let i = 0;
  while (i < argv.length) {
    const v = argv[i];
    i += 1;
    if (v === "--") break;
    if (v.startsWith("+") && v.length > 1) continue;
    if (VERSION_FLAGS.has(v)) return i - 1;
    if (VALUE_GLOBALS.has(v)) { i += 1; continue; }
    if (PLAIN_GLOBALS.has(v)) continue;
    if (v.startsWith("--manifest-path=") || v.startsWith("--config=") || v.startsWith("--color=") || v.startsWith("-Z")) continue;
    if (v.startsWith("-")) continue;
    return i - 1;
  }
  return null;
}

/**
 * The cargo subcommand of an argv as sent (an alias stays an alias), read the way Cargo reads it: the broker's reading (`cargoCommandIndex`) plus
 * Cargo's own `--explain <code>` global, whose value is skipped here (the broker reads that value as the command and refuses it, so this
 * only ever makes the gate demand MORE evidence, never less). null when there is none.
 */
export function cargoSubcommandOf(argv) {
  const words = [...argv];
  const end = words.indexOf("--");
  const own = [];
  for (let i = 0; i < words.length; i += 1) {
    if ((end < 0 || i < end) && words[i] === "--explain") { i += 1; continue; }
    own.push(words[i]);
  }
  const i = cargoCommandIndex(own);
  return i === null ? null : own[i];
}

/** `argv` with the standard alias at the subcommand position replaced by its canonical command; every other word is kept as sent. */
export function expandCargoAliases(argv) {
  const out = [...argv];
  const i = cargoCommandIndex(out);
  if (i !== null) out[i] = canonicalCargoCommand(out[i]);
  return out;
}

const hasFlag = (argv, flag) => { const end = argv.indexOf("--"); return (end < 0 ? argv : argv.slice(0, end)).includes(flag); };
const lockPinnedOffline = (argv) => hasFlag(argv, "--frozen") || (hasFlag(argv, "--locked") && hasFlag(argv, "--offline"));

/** Whether a short-flag cluster reaches `-Z` or `-C` (`-Zx`, `-vZx`, `-qvC/dir`); `-j`, `-p`, `-F` take the rest as their value. */
function shortClusterCarriesZorC(arg) {
  if (!arg.startsWith("-") || arg.startsWith("--")) return false;
  for (const c of arg.slice(1)) {
    if (c === "Z" || c === "C") return true;
    if (c === "j" || c === "p" || c === "F") return false;
  }
  return false;
}
function hasConfigOrUnstable(argv) {
  const end = argv.indexOf("--");
  return (end < 0 ? argv : argv.slice(0, end)).some((v) => v === "--config" || v.startsWith("--config=") || shortClusterCarriesZorC(v));
}

// ---- external subcommands the gate sends (only when the host registers them) ----

const DENY_FLAGS = new Set(["--metadata-path", "--config"]);
/** `deny [--frozen] [--metadata-path <file>] [--config <file>] check`: each option once, spaced, nothing after `check`. Returns the file options or null. */
function denyCheckFiles(argv) {
  const index = cargoCommandIndex(argv);
  if (index === null || argv[index] !== "deny") return null;
  const files = []; let frozen = false;
  const rest = argv.slice(index + 1);
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "check") return i === rest.length - 1 ? files : null;
    if (arg === "--frozen" && !frozen) { frozen = true; continue; }
    if (DENY_FLAGS.has(arg)) {
      if (files.some(([seen]) => seen === arg)) return null;
      const value = rest[i + 1];
      if (typeof value !== "string" || value === "" || value.startsWith("-")) return null;
      i += 1; files.push([arg, value]); continue;
    }
    return null;
  }
  return null;
}
/** `audit --db <dir> --no-fetch`, either order, nothing else. Returns the database directory or null. */
function auditOfflineDb(argv) {
  const index = cargoCommandIndex(argv);
  if (index === null || argv[index] !== "audit") return null;
  let db = null; let noFetch = false;
  const rest = argv.slice(index + 1);
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--no-fetch" && !noFetch) { noFetch = true; continue; }
    if (arg === "--db" && db === null) {
      const value = rest[i + 1];
      if (typeof value !== "string" || value === "" || value.startsWith("-")) return null;
      i += 1; db = value; continue;
    }
    return null;
  }
  return db !== null && noFetch ? db : null;
}
/** `about generate --frozen --fail [--config f] [--manifest-path f] [--all-features | --features l] <template>` exactly as the licence notices send it. */
function aboutNoticesShape(argv) {
  const index = cargoCommandIndex(argv);
  if (index === null) return false;
  const tail = argv.slice(index + 1);
  if (argv[index] !== "about" || tail.length < 4 || tail[0] !== "generate" || tail[1] !== "--frozen" || tail[2] !== "--fail") return false;
  const seen = []; let template = false; let features = false;
  const rest = tail.slice(3);
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--config" || arg === "--manifest-path" || arg === "--features") {
      const isFeature = arg === "--features";
      const value = rest[i + 1];
      if (seen.includes(arg) || (isFeature && features) || typeof value !== "string" || value === "" || value.startsWith("-")) return false;
      i += 1; features ||= isFeature; seen.push(arg); continue;
    }
    if (arg === "--all-features" && !features) { features = true; continue; }
    if (!arg.startsWith("-") && !template) { template = true; continue; }
    return false;
  }
  return template;
}
/** The part of `argv` cargo itself parses: before the subcommand for the deny and about shapes (the rest is the tool's own), all of it otherwise. */
function cargoOwnArgs(argv) {
  const index = cargoCommandIndex(argv);
  return index !== null && (denyCheckFiles(argv) !== null || aboutNoticesShape(argv)) ? argv.slice(0, index) : argv;
}
function fmtCheckShape(tail) {
  if (tail.length === 3 && tail[0] === "-p" && tail[1] !== "" && !tail[1].startsWith("-") && tail[2] === "--check") return true;
  return tail.length === 2 && tail[0] === "--all" && tail[1] === "--check";
}

// ---- the package observation (rustright build_rules.rs observation_only_cargo, package arm) ----

const PACKAGE_PLAIN_FLAGS = new Set(["--list", "--locked", "--allow-dirty"]);
/**
 * Whether `argv` is the one package form the broker admits as an observation that compiles nothing: no `--`, no `+toolchain`, the subcommand as
 * the one positional, `-p`/`--package` once with a value that is not a flag (`--package=x` is read, `-p=x` is not), and only `--list`,
 * `--locked` and `--allow-dirty` besides; it must carry `--list`, `--locked` and one package. Anything else is a compiling package.
 */
export function isPackageObservation(argv) {
  const words = [...argv];
  if (words.includes("--") || words.some((w) => w.startsWith("+"))) return false;
  const first = words.find((w) => !w.startsWith("-"));
  if (first !== "package") return false;
  let seen = false; let list = false; let locked = false; let packages = 0;
  for (let i = 0; i < words.length; i += 1) {
    const w = words[i];
    if (!w.startsWith("-")) { if (seen || w !== "package") return false; seen = true; continue; }
    const eq = w.startsWith("--") ? w.indexOf("=") : -1;
    const name = eq >= 0 ? w.slice(0, eq) : w; const inline = eq >= 0 ? w.slice(eq + 1) : null;
    if (name === "-p" || name === "--package") {
      const value = inline ?? words[++i];
      if (typeof value !== "string" || value === "" || value.startsWith("-")) return false;
      packages += 1; if (packages > 1) return false;
      continue;
    }
    if (!PACKAGE_PLAIN_FLAGS.has(name) || inline !== null) return false;
    if (name === "--list") list = true; else if (name === "--locked") locked = true;
  }
  return seen && list && locked && packages === 1;
}

// ---- classification (command_policy.rs cargo_capability) ----

const CONTROL_COMMANDS = new Set(["--version", "-V", "-vV", "version", "help", "locate-project", "read-manifest", "logout", "owner", "yank", "login"]);
const ANALYSIS_COMMANDS = new Set(["search", "info", "pkgid", "verify-project", "report", "config"]);
const WORKSPACE_WRITE_COMMANDS = new Set(["new", "init", "update", "vendor", "generate-lockfile", "add", "remove"]);
/** The built-in commands the broker classifies as compiling (a Capability with `compile: true`): `package` only when it is not the observation. */
export const BROKER_COMPILE_COMMANDS = Object.freeze(["fix", "run", "package", "publish", "build", "check", "test", "bench", "clippy", "doc", "rustc", "rustdoc"]);
const CARGO_BUILTINS = new Set(["build", "check", "test", "bench", "clippy", "doc", "run", "install", "rustc", "rustdoc", "clean", "update", "vendor", "generate-lockfile", "add", "remove", "metadata", "tree", "fetch", "fix", "publish", "package", "login", "logout", "owner", "yank", "uninstall", "init", "new", "info", "config", "report", "pkgid", "verify-project", "locate-project", "read-manifest", "search", "help", "version", "fmt", "--version", "-V"]);
/** External subcommands and their classification once the host registers them (command_policy.rs external_policy). */
const EXTERNAL = Object.freeze({ about: { compile: false }, outdated: { compile: false }, audit: { compile: false }, deny: { compile: false }, machete: { compile: false }, upgrade: { compile: false }, "set-version": { compile: false }, nextest: { compile: true }, ndk: { compile: true } });
/** The external subcommands the gate itself sends through the broker: the host registers each (rustright adapter/external registrations). */
export const GATE_EXTERNAL_COMMANDS = Object.freeze(["deny", "audit", "about", "ndk", "nextest"]);

const MANAGED_TOOL_NAMES = new Set(["cargo", "rustup", "rustc", "rustdoc", "swift", "cmake", "make", "cc", "clang", "gcc", "node", "node-gyp", "pnpm", "npm"]);
const REDIRECT_LONG = ["--target-dir", "--config", "--cache-dir", "--build-dir", "--toolchain", "--temp-dir", "-Zbuild-std", "-Zconfig"];
const isRedirectArgument = (a) => REDIRECT_LONG.some((flag) => a === flag || a.startsWith(`${flag}=`));
const isUnmanagedToolPath = (a) => {
  const abs = a.startsWith("/") || /^[A-Za-z]:[\\/]/.test(a) || a.startsWith("\\\\");
  const name = a.split(/[\\/]/).at(-1).toLowerCase().replace(/\.exe$/, "");
  return abs && MANAGED_TOOL_NAMES.has(name);
};
const OWN_FLAG_SUBCOMMANDS = new Set(["about", "outdated", "audit", "deny", "machete"]);

/** rustright request.rs `reject_managed_agent_redirects` for an argv: a redirecting option, a `+toolchain` or an unmanaged tool path before any `--`. */
function redirectRefused(argv) {
  const at = argv.findIndex((a) => !a.startsWith("-"));
  const ownFrom = at >= 0 && OWN_FLAG_SUBCOMMANDS.has(argv[at]) ? at : null;
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--") break;
    const cargoFlag = ownFrom === null || i < ownFrom;
    if ((cargoFlag && isRedirectArgument(argv[i])) || argv[i].startsWith("+") || isUnmanagedToolPath(argv[i])) return true;
  }
  return false;
}

/** The refusal rows: code is the broker's own typed code. */
const refused = (code, message) => ({ refused: { code, message } });

/**
 * What the broker's production parser decides for `argv` (cargo's arguments without `cargo`), in rustright's order: normalisation (the standard
 * aliases, redirect refusals), classification (command, compile) and the carried, grant and carried-refusal predicates.
 *   - `{ refused: { code, message } }`: the broker answers the typed code and runs zero processes;
 *   - `{ normalizedArgv, command, compile, carriedReadOnly, grant, carriedRefusal }` (the vector's columns).
 * `registered` lists the external subcommands the host registers (default none, as in the vector); `deny`, `audit`, `about`, `ndk` and
 * `nextest` are classified only when registered, as on the broker.
 */
export function classifyCargoArgv(argv, { registered = [] } = {}) {
  const words = [...argv];
  if (!words.every((w) => typeof w === "string" && w !== "")) return refused("RK_ARGV_INVALID", "Cargo argv must contain non-empty strings");
  if (redirectRefused(words)) return refused("RK_REDIRECT_REFUSED", "managed agent request cannot redirect cache, build, toolchain, temp, or tool path");
  const normalizedArgv = expandCargoAliases(words);
  const index = cargoCommandIndex(normalizedArgv);
  const command = index === null ? null : normalizedArgv[index];
  if (command === null) return refused("RK_UNKNOWN_COMMAND", `unregistered managed command: cargo ${normalizedArgv.join(" ")}`.trim());
  if (command === "install" || command === "uninstall") return refused("RK_COMMAND_DENIED", `agent tool ${command === "install" ? "installation" : "removal"} is denied; managed tools are digest-registered by the operator`);
  if (command === "clean") return refused("RR_CLEAN_REFUSED", "denied managed command: cargo clean");
  let compile;
  if (CONTROL_COMMANDS.has(command) || ANALYSIS_COMMANDS.has(command) || WORKSPACE_WRITE_COMMANDS.has(command) || command === "metadata" || command === "tree" || command === "fetch" || command === "fmt") compile = false;
  else if (command === "package") compile = !isPackageObservation(normalizedArgv);
  else if (BROKER_COMPILE_COMMANDS.includes(command)) compile = true;
  else if (registered.includes(command) && Object.hasOwn(EXTERNAL, command)) compile = EXTERNAL[command].compile;
  else return refused("RK_UNKNOWN_COMMAND", `unregistered managed command: cargo ${normalizedArgv.join(" ")}`.trim());
  const carriedRefusalText = hasConfigOrUnstable(cargoOwnArgs(normalizedArgv)) ? "a snapshot-bound request does not carry cargo --config or -Z" : null;
  return { normalizedArgv, command, compile, carriedReadOnly: snapshotReadOnly(normalizedArgv), grant: grantCommand(normalizedArgv).ok, carriedRefusal: carriedRefusalText !== null, carriedRefusalText };
}

function offlineReadOnly(argv) {
  if (hasConfigOrUnstable(cargoOwnArgs(argv))) return false;
  const index = cargoCommandIndex(argv);
  if (index === null) return false;
  switch (canonicalCargoCommand(argv[index])) {
    case "--version": case "-V": case "version": case "locate-project": case "read-manifest": return true;
    case "metadata": return hasFlag(argv, "--no-deps") || lockPinnedOffline(argv);
    case "about": {
      const end = argv.indexOf("--");
      const after = argv.slice(index + 1, end < 0 ? undefined : end);
      const at = after.findIndex((v) => !v.startsWith("-"));
      return at >= 0 && after[at] === "generate" && lockPinnedOffline(after.slice(at + 1));
    }
    default: return false;
  }
}
function classAScan(argv) {
  if (hasConfigOrUnstable(cargoOwnArgs(argv))) return null;
  const index = cargoCommandIndex(argv);
  if (index === null) return null;
  switch (canonicalCargoCommand(argv[index])) {
    case "deny": return denyCheckFiles(argv);
    case "audit": { const db = auditOfflineDb(argv); return db === null ? null : [["--db", db]]; }
    case "fetch": case "metadata": return hasFlag(argv, "--locked") || hasFlag(argv, "--frozen") ? [] : null;
    case "fmt": return fmtCheckShape(argv.slice(index + 1)) ? [] : null;
    case "package": return isPackageObservation(argv) ? [] : null;
    default: return null;
  }
}
/** What a snapshot-bound (carried) cargo request that compiles nothing may be (command_policy.rs `snapshot_read_only`). */
export const snapshotReadOnly = (argv) => offlineReadOnly(argv) || classAScan(argv) !== null;

/** What a stage grant's allowlist accepts (command_policy.rs `grant_command`): `{ ok: true }` or `{ ok: false, reason }`. */
export function grantCommand(argv) {
  if (hasConfigOrUnstable(argv)) return { ok: false, reason: "a stage grant does not carry cargo --config or -Z" };
  const index = cargoCommandIndex(argv);
  if (index === null) return { ok: false, reason: "a stage grant needs a cargo subcommand" };
  const command = canonicalCargoCommand(argv[index]);
  if (command === "build" || command === "run") return hasFlag(argv, "--locked") || hasFlag(argv, "--frozen") ? { ok: true } : { ok: false, reason: `a stage grant carries cargo ${argv[index]} only with --locked or --frozen` };
  if (command === "about") return { ok: false, reason: "a stage grant does not carry cargo about" };
  if (offlineReadOnly(argv)) return { ok: true };
  return { ok: false, reason: `a stage grant does not carry cargo ${command} with these arguments` };
}

/** The version spellings the broker answers uncarried inside a snapshot (command_policy.rs VERSION_ARGUMENTS and `version`), matched as the WHOLE argv. */
export const VERSION_PROBES = Object.freeze([["-V"], ["--version"], ["-vV"], ["-Vv"], ["--version", "--verbose"], ["-V", "--verbose"], ["-v", "-V"], ["version"]]);
export const isVersionProbe = (argv) => VERSION_PROBES.some((probe) => probe.length === argv.length && probe.every((w, i) => w === argv[i]));

/**
 * The decision for a CARRIED call (a snapshot-bound request through the broker): the shape the wrapper, the carried seam and the planners
 * share. `{ ok: true, argv, compile }` (`argv` with the aliases expanded, the one rewrite) or `{ ok: false, code, message }`, decided
 * before anything is submitted, so a refused shape launches no process. A carried call must compile or be a carried read-only shape; the
 * broker refuses everything else (`tree`, `pkgid`, `verify-project`, a bare `metadata`, a `fmt` write form, `help`), so it is never sent.
 */
export function carriedShapeDecision(argv, options = {}) {
  const row = classifyCargoArgv(argv, { registered: GATE_EXTERNAL_COMMANDS, ...options });
  if (row.refused) return { ok: false, code: row.refused.code, message: `the broker refuses \`cargo ${[...argv].join(" ")}\` (${row.refused.code}: ${row.refused.message}); it is not sent` };
  if (row.carriedRefusal) return { ok: false, code: "RK_CARGO_BROKER_CARRIED_REFUSED", message: `\`cargo ${[...argv].join(" ")}\`: ${row.carriedRefusalText}; it is not sent` };
  if (!row.compile && !row.carriedReadOnly) return { ok: false, code: "RK_CARGO_BROKER_READ_ONLY_SHAPE", message: `\`cargo ${[...argv].join(" ")}\` is neither a compile nor a carried read-only shape (metadata --no-deps or --locked/--frozen, fetch --locked, fmt --check, locate-project, read-manifest, deny, audit, package --list); the broker refuses it inside a snapshot, so it is not sent` };
  return { ok: true, argv: row.normalizedArgv, compile: row.compile };
}

/**
 * Whether the broker classifies this argv (cargo's arguments) as a compile: the receipt then carries freshness, attribution and toolchain
 * evidence (or typed unavailable evidence), and absence is never a success. An argv the broker refuses outright never produces a receipt; if one
 * is presented anyway it still owes the evidence its compile command names (more evidence is demanded, never less).
 */
export function isCompileCommand(argv) {
  const row = classifyCargoArgv(argv, { registered: GATE_EXTERNAL_COMMANDS });
  if (!row.refused) return row.compile === true;
  const sub = canonicalCargoCommand(cargoSubcommandOf(argv));
  return BROKER_COMPILE_COMMANDS.includes(sub) && !(sub === "package" && isPackageObservation(argv));
}
