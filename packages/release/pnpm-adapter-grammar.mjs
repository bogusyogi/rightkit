// The door's mirror of the broker's `rightkit-pnpm` adapter grammar (rustright-policy adapter.rs `scan`, `check_argv`, `snapshot_argv_check`,
// `check_environment`; GB-03 and GB-04 of the gate-vs-broker audit).
//
// RustRight owns the registered grammar; RightKit validates every pnpm argv and every request environment against that registration BEFORE it is
// sent, and refuses with a typed reason naming the registration, so a refusal never costs a mark, a process or a stage. The reference registration
// is RustRight's `adapters.pnpm` entry of docs/contracts/adapter-argv.fixtures.json (test-support/vectors/, pinned in vectors-source.json): the
// operator installs exactly that registration, and every accepted and refused row of the vector is judged by `adapterArgvRefusal` here.
//
// Facts the planner depends on (the registration, not RightKit inventions): a global flag that takes a value is registered as a flag AND as a leading
// subcommand token (`--dir`, `-C`, `--filter`); `--if-present` is a `run` option and goes after `run`; everything after the target of `run` or
// `exec` belongs to the target and is opaque (`run x --flag` and `run x -- --flag` are both admitted, so the gate never rewrites them); `exec`
// admits only the targets in `opaqueTargets.exec`, never an arbitrary executable.
//
// The door reads the LIVE registration when it can (liveAdapterGrammar) and otherwise judges by PNPM_ADAPTER_GRAMMAR, the reference. A test compares
// the constant with the vector and, on a host that has the registration, with the live entry, so neither can drift without a failing test.
import fs, { readFileSync } from "node:fs";
import path from "node:path";
import { resolveManagedRoots } from "./managed-roots.mjs";

export const PNPM_ADAPTER = "rightkit-pnpm";
/** The scratch child prefix of a `scratchPackDir` / `scratchPath` value (adapter.rs SCRATCH_CHILD_PREFIX). */
const SCRATCH_CHILD_PREFIX = "rk-local-publish-";

const frozenList = (v) => Object.freeze(Array.isArray(v) ? v.filter((s) => typeof s === "string") : []);

/**
 * The reference registration of the pnpm adapter, as RustRight's vector carries it (docs/contracts/adapter-argv.fixtures.json `adapter`, minus the
 * host's pinned script and scratch root): `subcommands` are the registered first arguments, `flags` the registered flags with their value kind,
 * `env` the only non-reserved request-environment keys the adapter takes.
 */
export const PNPM_ADAPTER_GRAMMAR = Object.freeze({
  subcommands: Object.freeze(["install", "run", "exec", "pack", "-r", "--dir", "-C", "--filter", "--workspace-concurrency=1"]),
  flags: Object.freeze({
    "--dir": "relpathOrScratchPath", "-C": "relpathOrScratchPath", "--filter": "any", "--if-present": "none", "--workspace-concurrency": "int", "--pack-destination": "scratchPackDir",
  }),
  positionals: "relpath",
  env: Object.freeze(["RK_ISOLATED_CHECKOUT", "RIGHTKIT_QA_APP_BINARY", "RIGHTKIT_QA_SIGNING_EVIDENCE"]),
  opaqueAfterTarget: Object.freeze(["run", "exec"]),
  opaqueTargets: Object.freeze({ exec: Object.freeze(["tauri"]) }),
  snapshotSubcommands: Object.freeze(["run", "exec", "pack"]),
  // The host's scratch root is host configuration (the vector's `{scratch}` placeholder): unknown here, so a scratch-kind value is judged by shape and the
  // broker's canonical containment check decides (a live registration, which names its root, is judged canonically too).
  scratchRoot: undefined,
});

/** The grammar a registered adapter spec declares (the fields the door uses), or null when `spec` is not a usable registration. */
export function grammarFromSpec(spec) {
  if (!spec || typeof spec !== "object" || (spec.subcommands !== undefined && spec.subcommands !== null && (!Array.isArray(spec.subcommands) || !spec.subcommands.every((s) => typeof s === "string" && s !== "")))) return null;
  if (!Array.isArray(spec.flags) || !spec.flags.every((f) => f && typeof f.name === "string" && typeof f.value === "string")) return null;
  const targets = spec.opaqueTargets && typeof spec.opaqueTargets === "object" && !Array.isArray(spec.opaqueTargets) ? spec.opaqueTargets : {};
  return Object.freeze({
    subcommands: Array.isArray(spec.subcommands) ? Object.freeze([...spec.subcommands]) : null,
    flags: Object.freeze(Object.fromEntries(spec.flags.map((f) => [f.name, f.value]))),
    positionals: typeof spec.positionals === "string" ? spec.positionals : "none",
    env: frozenList(spec.env),
    opaqueAfterTarget: frozenList(spec.opaqueAfterTarget),
    ...(Array.isArray(spec.positionalValues) ? { positionalValues: frozenList(spec.positionalValues) } : {}),
    ...(spec.flagPrefixes && typeof spec.flagPrefixes === "object" ? { flagPrefixes: Object.freeze({ ...spec.flagPrefixes }) } : {}),
    opaqueTargets: Object.freeze(Object.fromEntries(Object.entries(targets).map(([command, list]) => [command, frozenList(list)]))),
    snapshotSubcommands: frozenList(spec.snapshotSubcommands),
    scratchRoot: typeof spec.scratchRoot === "string" && spec.scratchRoot !== "" ? spec.scratchRoot : null,
  });
}

/** The live registration on this host (the managed control root's config.json, adapters["rightkit-pnpm"]), or null when it cannot be read. */
export function liveAdapterGrammar({ env = process.env, platform = process.platform, readFile = (file) => readFileSync(file, "utf8"), adapter = PNPM_ADAPTER } = {}) {
  try {
    const { configPath } = resolveManagedRoots({ env, platform });
    return grammarFromSpec(JSON.parse(readFile(configPath))?.adapters?.[adapter]);
  } catch { return null; }
}

// ---- value kinds (adapter.rs value_ok / relpath_ok / scratch_shape_ok) ----

const relpathOk = (v) => v !== "" && !v.includes("\0") && !v.startsWith("/") && !v.startsWith("\\") && !(v.length >= 2 && v[1] === ":") && v.split(/[\\/]/).every((part) => part !== "..");
const isAbsoluteAnywhere = (v) => path.posix.isAbsolute(v) || path.win32.isAbsolute(v) || v.startsWith("\\");
const scratchShapeOk = (v) => v !== "" && !v.includes("\0") && (path.isAbsolute(v) || path.posix.isAbsolute(v) || path.win32.isAbsolute(v)) && v.split(/[\\/]/).every((part) => part !== "..");
/** Build settings a caller never sets for an Apple build: they redirect where the build writes (the broker owns the output root) or choose the signing identity (the provisioned signer's). */
const XCODE_DENIED_SETTINGS = /^(?:SYMROOT|OBJROOT|DSTROOT|BUILD_DIR|BUILD_ROOT|CONFIGURATION_BUILD_DIR|CONFIGURATION_TEMP_DIR|TARGET_BUILD_DIR|SHARED_PRECOMPS_DIR|MODULE_CACHE_DIR|DERIVED_DATA_DIR|CODE_SIGN_IDENTITY|CODE_SIGN_STYLE|OTHER_CODE_SIGN_FLAGS|PROVISIONING_PROFILE(?:_SPECIFIER)?|CODE_SIGN_ENTITLEMENTS|CODE_SIGNING_ALLOWED|CODE_SIGNING_REQUIRED)=/;
/** CMake cache entries that run or load arbitrary code from a path the caller names. */
const CMAKE_DENIED_DEFINES = /^(?:CMAKE_COMMAND|CMAKE_MAKE_PROGRAM|CMAKE_PROJECT_INCLUDE(?:_BEFORE)?|CMAKE_MODULE_PATH|CMAKE_PREFIX_PATH|CMAKE_PROGRAM_PATH)[:=]/;
function valueOk(kind, v) {
  switch (kind) {
    case "identifier": return /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/.test(v);
    case "gradleTask": return /^:?[A-Za-z0-9][A-Za-z0-9_:.-]{0,255}$/.test(v) && !/(?:^|:)(?:publish|upload)|(?:bundle|assemble|install)[A-Za-z0-9]*Release/i.test(v);
    case "actionOrBuildSetting": return v.includes("=") ? valueOk("buildSetting", v) : valueOk("identifier", v);
    case "buildSetting": return /^[A-Z][A-Za-z0-9_]*=[^\0]*$/.test(v) && !XCODE_DENIED_SETTINGS.test(v);
    case "cmakeDefine": return /^[A-Za-z_][A-Za-z0-9_.+-]*(?::[A-Z]+)?=[^\0]*$/.test(v) && !CMAKE_DENIED_DEFINES.test(v);
    case "none": return false;
    case "any": return !v.includes("\0");
    case "int": return v !== "" && /^[0-9]+$/.test(v);
    case "swiftConfiguration": return v === "debug" || v === "release";
    case "swiftArch": return v === "arm64" || v === "x86_64";
    case "relpath": return relpathOk(v);
    case "scratchPath": case "scratchPackDir": return scratchShapeOk(v);
    case "relpathOrScratchPath": return relpathOk(v) || scratchShapeOk(v);
    default: return false;
  }
}
const needsScratchCheck = (kind, v) => kind === "scratchPath" || kind === "scratchPackDir" || (kind === "relpathOrScratchPath" && !relpathOk(v));

/** adapter.rs `scratch_canonical_ok`: the value exists and, resolved, is strictly inside an `rk-local-publish-*` child of the scratch root (a pack dir: an empty-of-tgz directory). */
function scratchCanonicalProblem(root, value, packDir, { realpath = (p) => fs.realpathSync.native(p), stat = (p) => fs.statSync(p), list = (p) => fs.readdirSync(p) } = {}) {
  if (!scratchShapeOk(value)) return "must be an absolute path with no '..'";
  let canonicalRoot; let canonical;
  try { canonicalRoot = realpath(root); } catch (e) { return `the scratch root cannot be resolved: ${e.message}`; }
  try { canonical = realpath(value); } catch (e) { return `cannot be resolved: ${e.message}`; }
  const rel = path.relative(canonicalRoot, canonical);
  if (rel === "" ) return "is the scratch root itself";
  if (rel.startsWith("..") || path.isAbsolute(rel)) return "resolves outside the scratch root";
  const parts = rel.split(path.sep);
  if (!parts[0].startsWith(SCRATCH_CHILD_PREFIX)) return `is not inside a ${SCRATCH_CHILD_PREFIX}* child of the scratch root`;
  if (parts.length < 2) return "is the scratch child itself; it must be strictly inside it";
  if (packDir) {
    try { if (!stat(canonical).isDirectory()) return "must be an existing directory"; if (list(canonical).some((n) => n.toLowerCase().endsWith(".tgz"))) return "must hold no .tgz yet"; } catch (e) { return `cannot be read: ${e.message}`; }
  }
  return null;
}

const flagNameOf = (word) => (word.startsWith("--") && word.includes("=") ? word.slice(0, word.indexOf("=")) : word);

/**
 * The pure pass of adapter.rs `scan`: every argument is a registered flag (with its value), a positional of the allowed kind, or the first-argument
 * subcommand. Returns `{ error }` or `{ command, positionals, opaqueTail, flagValues, scratch }`. A subcommand that is also a registered flag
 * (`--dir`) is read as that flag, so its value is judged by the flag's kind and never taken for a positional.
 */
export function scanAdapterArgv(argv, grammar = PNPM_ADAPTER_GRAMMAR) {
  const args = [...argv].map(String);
  const found = { command: null, target: null, positionals: [], opaqueTail: [], flagValues: [], scratch: [] };
  let index = 0; let awaiting = false; let opaque = false; let commandSeen = false;
  const first = args[0];
  const name = grammar.adapter ?? PNPM_ADAPTER;
  if (grammar.subcommands === null) {
    // No registered first argument (an adapter whose tool takes flags and positionals only, such as gradle, cmake and xcodebuild): the scan starts at the first word.
    index = 0;
  } else {
    if (first === undefined) return { error: "a subcommand is required" };
    if (!grammar.subcommands.includes(first)) return { error: `subcommand ${JSON.stringify(first)} is not registered for ${name} (registered: ${grammar.subcommands.join(", ")}); a leading flag such as --if-present, --prefix or --config is refused (--if-present goes after \`run\`)` };
    awaiting = grammar.opaqueAfterTarget.includes(first);
    commandSeen = !first.startsWith("-");
    if (commandSeen) found.command = first;
    index = first.startsWith("-") && Object.hasOwn(grammar.flags, first) ? 0 : 1;
  }
  let afterSeparator = false;
  while (index < args.length) {
    const word = args[index]; index += 1;
    if (word.includes("\0")) return { error: "arguments must not contain NUL" };
    if (opaque) { found.opaqueTail.push(word); continue; }
    if (!afterSeparator && word === "--") { afterSeparator = true; continue; }
    if (afterSeparator || !word.startsWith("-") || word === "-") {
      if (grammar.positionals === "none" || !valueOk(grammar.positionals, word)) return { error: `positional argument ${JSON.stringify(word)} is not allowed` };
      if (grammar.positionalValues && !word.includes("=") && !grammar.positionalValues.includes(word)) return { error: `positional argument ${JSON.stringify(word)} is not registered for ${grammar.adapter ?? PNPM_ADAPTER} (registered: ${grammar.positionalValues.join(", ")})` };
      if (needsScratchCheck(grammar.positionals, word)) found.scratch.push(["positional argument", grammar.positionals, word]);
      found.positionals.push(word);
      if (awaiting) {
        awaiting = false; opaque = true; found.target = word;
        const allowed = grammar.opaqueTargets[found.command ?? ""];
        if (allowed && !allowed.includes(word)) return { error: `target ${JSON.stringify(word)} is not registered for ${found.command} (registered targets: ${allowed.join(", ")}); an arbitrary executable is never admitted through exec` };
      } else if (!commandSeen) {
        commandSeen = true; found.command = word; awaiting = grammar.opaqueAfterTarget.includes(word);
      }
      continue;
    }
    // A registered flag PREFIX carries its value in the same word (`-DNAME=value`): the rest of the word is the value, judged by the prefix's kind.
    const prefix = grammar.flagPrefixes ? Object.keys(grammar.flagPrefixes).find((p) => word.startsWith(p) && word.length > p.length) : undefined;
    if (prefix !== undefined) {
      const value = word.slice(prefix.length);
      if (!valueOk(grammar.flagPrefixes[prefix], value)) return { error: `flag ${JSON.stringify(prefix)} has a value of the wrong kind or one ${grammar.adapter ?? PNPM_ADAPTER} never admits (${JSON.stringify(value)})` };
      found.flagValues.push([`flag ${JSON.stringify(prefix)}`, grammar.flagPrefixes[prefix], value]);
      continue;
    }
    const name = flagNameOf(word);
    const inline = word.startsWith("--") && word.includes("=") ? word.slice(word.indexOf("=") + 1) : null;
    if (!Object.hasOwn(grammar.flags, name)) return { error: `flag ${JSON.stringify(name)} is not registered for ${grammar.adapter ?? PNPM_ADAPTER}` };
    const kind = grammar.flags[name];
    let value;
    if (kind === "none") { if (inline !== null) return { error: `flag ${JSON.stringify(name)} takes no value` }; continue; }
    if (inline !== null) value = inline;
    else {
      value = args[index]; index += 1;
      if (value === undefined) return { error: `flag ${JSON.stringify(name)} needs a value` };
      if (value.includes("\0")) return { error: "arguments must not contain NUL" };
    }
    if (!valueOk(kind, value)) return { error: `flag ${JSON.stringify(name)} has a value of the wrong kind (${kind})` };
    found.flagValues.push([`flag ${JSON.stringify(name)}`, kind, value]);
    if (needsScratchCheck(kind, value)) found.scratch.push([`flag ${JSON.stringify(name)}`, kind, value]);
  }
  if (awaiting) return { error: "launcher subcommand requires a target" };
  return found;
}

/** The pnpm command token the registration reads from `args` (adapter.rs scan's `command`), or null when the argv does not parse. */
export function pnpmCommandOf(args, grammar = PNPM_ADAPTER_GRAMMAR) {
  const found = scanAdapterArgv(args, grammar);
  return found.error ? null : found.command;
}

/**
 * Why the broker's pnpm adapter would refuse these arguments, or null (adapter.rs `check_argv`; with `snapshot`, also `snapshot_argv_check`'s
 * subcommand and relative-path rules). Judged against `grammar` (the live registration, else the reference). A scratch-kind value is checked canonically
 * against the registration's scratch root when it names one (a value of that kind with no scratch root registered is refused). The tail of an opaque
 * target is the target's own and is never interpreted here.
 */
export function adapterArgvRefusal(words, grammar = PNPM_ADAPTER_GRAMMAR, { snapshot = false, fsOps = {} } = {}) {
  const found = scanAdapterArgv(words, grammar);
  if (found.error) return found.error;
  for (const [what, kind, value] of found.scratch) {
    if (grammar.scratchRoot === undefined) continue;
    if (grammar.scratchRoot === null) return `${what}: this adapter registers no scratch root`;
    const problem = scratchCanonicalProblem(grammar.scratchRoot, value, kind === "scratchPackDir", fsOps);
    if (problem) return `${what} ${JSON.stringify(value)}: ${problem}`;
  }
  if (snapshot) {
    if (grammar.snapshotSubcommands.length) {
      if (found.command === null) return "a snapshot-bound call needs a command";
      if (!grammar.snapshotSubcommands.includes(found.command)) return `subcommand ${JSON.stringify(found.command)} is not allowed in a snapshot-bound call (allowed: ${grammar.snapshotSubcommands.join(", ")})`;
    }
    for (const [what, kind, value] of found.flagValues) {
      if ((kind === "relpath" || kind === "relpathOrScratchPath" || kind === "scratchPath") && isAbsoluteAnywhere(value)) return `${what} ${JSON.stringify(value)} must be relative in a snapshot-bound call`;
    }
  }
  return null;
}

/** Kept name of the door's pnpm check: the argv judged by `grammar` (a snapshot-bound call when `snapshot`). */
export const pnpmGrammarRefusal = (words, grammar = PNPM_ADAPTER_GRAMMAR, options = {}) => adapterArgvRefusal(words, grammar, options);
