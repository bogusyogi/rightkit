// GB-51 / RA-07, RA-08, RA-09: the constrained adapters of the native build tools (xcodegen, xcodebuild, Gradle, CMake): the reference registrations RustRight registers EXACTLY
// (rustright-policy native_adapters.rs `reference_spec`, RR-NATIVE 96b6334: capability names, grammars, environment), and the gate's launch mapping onto them. Each is a snapshot-bound
// adapter job: a pinned executable identity (the registration's), a committed cwd (the snapshot's counterpart of the step's directory), an argument grammar judged BEFORE the send by
// the same parser the pnpm registration uses (pnpm-adapter-grammar.mjs, a port of RustRight's adapter scan), no environment, and an output root the BROKER owns: a caller never names
// where the build writes (`-derivedDataPath`, `-archivePath`, `-exportPath`, `-resultBundlePath`, a cmake build tree or install prefix, a Gradle user or cache home or init script) and
// never supplies a signing identity (signing stays in the provisioned identity and session with its evidence handed off). The broker rewrites the caller's words into the one managed call
// (`managed_call`): every write under its per-project state, the receipt's `executable.managedOutputs` naming each location by role. Nested processes (the compiler the tool starts) are
// attributed to the job by the broker's capability token; cancellation reaps the tree. Nothing here is a direct-session exemption: an unsupported custom command, an unregistered tool
// or an output override refuses at planning, naming the capability.
//
// What the grammar cannot express is mirrored here from the broker (`native_call_problem`), so the gate sends exactly what the broker accepts and nothing it refuses:
//   - the call's SHAPE (`nativeShapeProblem`): xcodebuild `-exportArchive` stands alone with its options file, `archive` is the only action of its call, a test plan or selection needs a
//     test action and selections are `Target[/Class[/test]]`; cmake `--build` / `--install` are modes that must be the FIRST argument and never name a directory (the broker names the
//     one managed build tree), a configure takes no `--config`/`--target`/`--parallel`, and no `-D` entry moves an output (`CMAKE_INSTALL_PREFIX`, `*_OUTPUT_DIRECTORY`, ...);
//   - what only the file system can say (`nativeCallFsProblem`): the Gradle wrapper's pinned distribution, and an export options file that is a regular, small, XML, non-uploading file
//     of the snapshot;
//   - signing: no secret rides an argument or an environment key. The one transient Android signing-properties file is a typed FILE handoff (`signingFileProblem` mirrors the broker's
//     `signing_file_problem` and `SigningFileLease`); RustRight has no request field that carries it yet, so a broker route refuses it before launch (`nativeSigningFileRefusal`);
//   - the receipt's output mapping (`nativeManagedOutputs`, `nativeOutputsFromReceipt`): the roles and absolute paths under the job's managed state, read back, never guessed.
//
// The capability names are the contract with RustRight (docs/GATE-CONTRACT.md "Native tool adapters"): a broker advertises `adapter:<name>` in its capabilities, and the gate
// plans a stage that uses the tool only when it does.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";
import { SIGNING_FILE_ENV } from "./adapter-env.mjs";
import { adapterArgvRefusal } from "./pnpm-adapter-grammar.mjs";

const flags = (map) => Object.entries(map).map(([name, value]) => ({ name, value }));
const freeze = (o) => Object.freeze(JSON.parse(JSON.stringify(o)));

/** The reference registrations, keyed by tool. `registration` is the shape of RustRight's `adapters.<id>` entry (the fields the gate judges). */
export const NATIVE_ADAPTERS = Object.freeze({
  xcodegen: freeze({
    adapter: "rightkit-xcodegen", capability: "adapter:rightkit-xcodegen", executable: "xcodegen (digest-pinned by the registration)",
    registration: { tool: "xcodegen", lane: "BUILD", compile: false, snapshot: true, snapshotSubcommands: ["generate"], subcommands: ["generate"],
      flags: flags({ "--spec": "relpath", "--project": "relpath", "--quiet": "none", "--use-cache": "none" }), positionals: "none", env: [] },
  }),
  xcodebuild: freeze({
    adapter: "rightkit-xcodebuild", capability: "adapter:rightkit-xcodebuild", executable: "xcodebuild (the selected Xcode's, digest-pinned by the registration)",
    registration: { tool: "xcodebuild", lane: "BUILD", compile: true, snapshot: true, subcommands: null,
      // A test plan is a bare name of a plan the project declares; `-exportArchive` reads one snapshot-relative options file; `-only-testing:` / `-skip-testing:` carry their identifier in the same word.
      flags: flags({ "-project": "relpath", "-scheme": "any", "-configuration": "identifier", "-destination": "any", "-sdk": "identifier", "-quiet": "none", "-allowProvisioningUpdates": "none", "-allowProvisioningDeviceRegistration": "none", "-testPlan": "identifier", "-exportArchive": "none", "-exportOptionsPlist": "relpath" }),
      flagPrefixes: { "-only-testing:": "any", "-skip-testing:": "any" },
      positionals: "actionOrBuildSetting", positionalValues: ["build", "build-for-testing", "test", "test-without-building", "archive"], env: [] },
  }),
  gradle: freeze({
    adapter: "rightkit-gradle", capability: "adapter:rightkit-gradle", executable: "the committed Gradle wrapper (its distributionSha256Sum pinned in gradle-wrapper.properties)",
    registration: { tool: "gradle", lane: "BUILD", compile: true, snapshot: true, subcommands: null,
      flags: flags({ "--offline": "none", "--no-daemon": "none", "--console": "identifier", "--stacktrace": "none", "--quiet": "none", "-q": "none", "-p": "relpath", "--project-dir": "relpath" }),
      positionals: "gradleTask", env: [] },
  }),
  cmake: freeze({
    adapter: "rightkit-cmake", capability: "adapter:rightkit-cmake", executable: "cmake (digest-pinned by the registration)",
    registration: { tool: "cmake", lane: "BUILD", compile: true, snapshot: true, subcommands: null,
      // `--build` and `--install` only select the mode (first argument): the broker names the one managed build tree, so there is no `-B` and no directory.
      flags: flags({ "-S": "relpath", "-G": "any", "-A": "identifier", "--build": "none", "--config": "identifier", "--target": "any", "--parallel": "none", "--install": "none" }),
      flagPrefixes: { "-D": "cmakeDefine" }, positionals: "none", env: [] },
  }),
});

export const NATIVE_ADAPTER_CAPABILITIES = Object.freeze(Object.values(NATIVE_ADAPTERS).map((a) => a.capability));
const base = (command) => path.basename(String(command ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();
/** The native tool a command names (`./gradlew` is Gradle), or null. */
export const nativeToolOf = (command) => { const b = base(command); const tool = b === "gradlew" ? "gradle" : b; return Object.hasOwn(NATIVE_ADAPTERS, tool) ? tool : null; };

/** The grammar of a tool's registration, in the shape the argument parser takes. */
export function nativeGrammar(tool) {
  const { adapter, registration } = NATIVE_ADAPTERS[tool];
  return Object.freeze({
    adapter, subcommands: registration.subcommands, flags: Object.freeze(Object.fromEntries(registration.flags.map((f) => [f.name, f.value]))), positionals: registration.positionals,
    positionalValues: registration.positionalValues, flagPrefixes: registration.flagPrefixes, env: Object.freeze([...registration.env]), opaqueAfterTarget: Object.freeze([]), opaqueTargets: Object.freeze({}),
    snapshotSubcommands: Object.freeze([...(registration.snapshotSubcommands ?? [])]), scratchRoot: undefined,
  });
}

// Flags whose value is a path the gate generated as an absolute one under the step's directory: a snapshot-bound adapter takes it relative to the job's cwd.
const PATH_VALUED = Object.freeze({ xcodegen: ["--spec", "--project"], xcodebuild: ["-project", "-exportOptionsPlist"], gradle: ["-p", "--project-dir"], cmake: ["-S"] });
// Options that would move where a build writes (the broker owns the output root: they are not registered, so the broker would refuse them anyway); refused here naming the canonical route.
const OUTPUT_OWNED = Object.freeze({
  xcodebuild: ["-derivedDataPath", "-resultBundlePath", "-resultStreamPath", "-archivePath", "-exportPath", "-clonedSourcePackagesDirPath", "-packageCachePath"],
  gradle: ["--gradle-user-home", "-g", "--project-cache-dir", "--init-script", "-I", "--include-build"],
  cmake: ["-B", "--install-prefix", "--toolchain", "--preset", "--workflow"],
  xcodegen: ["--cache-path"],
});

/** The words of a caller's argv read the way the broker reads them (native_adapters.rs `split_words`, itself `adapter::scan`): flag names, each valued flag's value, and the positionals. */
function splitWords(tool, caller) {
  const { registration } = NATIVE_ADAPTERS[tool];
  const valued = new Set(registration.flags.filter((flag) => flag.value !== "none").map((flag) => flag.name));
  const prefixes = Object.keys(registration.flagPrefixes ?? {});
  const found = { flags: [], values: [], positionals: [] };
  let index = 0;
  while (index < caller.length) {
    const word = caller[index];
    index += 1;
    if (word === "--") { found.positionals.push(...caller.slice(index)); break; }
    if (!word.startsWith("-") || word === "-") { found.positionals.push(word); continue; }
    const prefix = prefixes.find((p) => word.startsWith(p) && word.length > p.length);
    if (prefix !== undefined) { found.flags.push(prefix); found.values.push([prefix, word.slice(prefix.length)]); continue; }
    if (word.startsWith("--") && word.includes("=")) { const at = word.indexOf("="); found.flags.push(word.slice(0, at)); found.values.push([word.slice(0, at), word.slice(at + 1)]); continue; }
    found.flags.push(word);
    if (valued.has(word)) { if (index < caller.length) found.values.push([word, caller[index]]); index += 1; }
  }
  return found;
}
const hasFlag = (words, flag) => words.flags.includes(flag);

/** `Target`, `Target/Class` or `Target/Class/test` (a test may end in `()`), each a bare name (native_adapters.rs `test_identifier_ok`). */
const testIdentifierOk = (value) => {
  const parts = value.split("/");
  return value !== "" && value.length <= 256 && parts.length <= 3 && parts.every((part) => { const name = part.endsWith("()") ? part.slice(0, -2) : part; return /^[A-Za-z0-9_][A-Za-z0-9_.-]*$/.test(name); });
};

/** What an xcodebuild call is, or why it has no managed form (native_adapters.rs `xcodebuild_mode`): "export" | "archive" | "test" | "build", or `{ problem }`. */
function xcodebuildMode(words) {
  const actions = words.positionals.filter((word) => !word.includes("="));
  const tests = actions.some((action) => ["test", "test-without-building", "build-for-testing"].includes(action));
  const archives = actions.includes("archive");
  const selecting = hasFlag(words, "-testPlan") || hasFlag(words, "-only-testing:") || hasFlag(words, "-skip-testing:");
  for (const [name, value] of words.values) if ((name === "-only-testing:" || name === "-skip-testing:") && !testIdentifierOk(value)) return { problem: `${name}${value} is not a test identifier (Target, Target/Class or Target/Class/test)` };
  if (hasFlag(words, "-exportArchive")) {
    if (words.positionals.length) return { problem: "-exportArchive takes no action or build setting" };
    for (const flag of ["-project", "-scheme", "-configuration", "-destination", "-sdk"]) if (hasFlag(words, flag)) return { problem: `-exportArchive reads the managed archive; ${flag} belongs to a build` };
    if (selecting) return { problem: "a test plan or selection does not belong to -exportArchive" };
    if (!hasFlag(words, "-exportOptionsPlist")) return { problem: "-exportArchive needs -exportOptionsPlist" };
    return "export";
  }
  if (hasFlag(words, "-exportOptionsPlist")) return { problem: "-exportOptionsPlist belongs to -exportArchive" };
  if (archives && actions.length > 1) return { problem: "archive is the only action of its call" };
  if (selecting && !tests) return { problem: "a test plan or selection needs build-for-testing, test or test-without-building" };
  return archives ? "archive" : tests ? "test" : "build";
}

/** What a cmake call is, or why it has no managed form (native_adapters.rs `cmake_mode`): "configure" | "build" | "install", or `{ problem }`. cmake reads `--build`/`--install` only as its first argument. */
function cmakeMode(words, caller) {
  const build = hasFlag(words, "--build");
  const install = hasFlag(words, "--install");
  if (build && install) return { problem: "cmake takes --build or --install, not both" };
  const mode = build ? "build" : install ? "install" : "configure";
  if (mode === "configure") {
    for (const flag of ["--config", "--target", "--parallel"]) if (hasFlag(words, flag)) return { problem: `${flag} belongs to --build or --install, not to a configure` };
    return mode;
  }
  const selector = build ? "--build" : "--install";
  if (caller[0] !== selector) return { problem: `${selector} must be the first argument: cmake reads its mode there` };
  for (const flag of ["-S", "-G", "-A", "-D"]) if (hasFlag(words, flag)) return { problem: `${flag} configures; it does not belong to ${selector}` };
  return mode;
}

/** CMake cache entries that move where a build or an install writes: the broker owns the build tree and the install prefix (native_adapters.rs `cmake_output_define`). */
const cmakeOutputDefine = (name) => {
  const upper = name.toUpperCase();
  return ["CMAKE_INSTALL_PREFIX", "CMAKE_STAGING_PREFIX", "EXECUTABLE_OUTPUT_PATH", "LIBRARY_OUTPUT_PATH", "FETCHCONTENT_BASE_DIR"].includes(upper)
    || ["CMAKE_RUNTIME_OUTPUT_DIRECTORY", "CMAKE_LIBRARY_OUTPUT_DIRECTORY", "CMAKE_ARCHIVE_OUTPUT_DIRECTORY", "CMAKE_PDB_OUTPUT_DIRECTORY", "CMAKE_COMPILE_PDB_OUTPUT_DIRECTORY"].some((family) => upper.startsWith(family))
    || (upper.startsWith("CMAKE_") && upper.endsWith("_MODULE_DIRECTORY"));
};

const relativeShape = (value) => value !== "" && !value.includes("\0") && !/^[/\\]/.test(value) && !(value.length >= 2 && value[1] === ":") && value.split(/[\\/]/).every((part) => part !== "..");

/**
 * Why a call has no managed form, judged on the argv alone (native_adapters.rs `managed_call` modes and `native_call_problem`'s shape rules), or null: the mode rules of xcodebuild and
 * cmake, test selections, an output-moving `-D` entry and a non-relative `-exportOptionsPlist`. Pure: the file-system rules are `nativeCallFsProblem`.
 */
export function nativeShapeProblem(tool, args) {
  if (!Object.hasOwn(NATIVE_ADAPTERS, tool)) return `${tool} is not a native build tool`;
  const caller = args.map(String);
  const words = splitWords(tool, caller);
  if (tool === "xcodebuild") {
    const mode = xcodebuildMode(words);
    if (typeof mode !== "string") return mode.problem;
    const plist = words.values.find(([name]) => name === "-exportOptionsPlist")?.[1];
    if (plist !== undefined && !relativeShape(plist)) return `-exportOptionsPlist ${JSON.stringify(plist)} must be a relative path with no \`..\``;
  }
  if (tool === "cmake") {
    const mode = cmakeMode(words, caller);
    if (typeof mode !== "string") return mode.problem;
    for (const [name, value] of words.values) {
      if (name !== "-D") continue;
      const entry = value.split(/[=:]/)[0];
      if (cmakeOutputDefine(entry)) return `-D${entry} moves where CMake writes; the broker owns the build tree and the install prefix`;
    }
  }
  return null;
}

/**
 * The argv a carried native-tool step sends, or a typed refusal naming the capability: a path flag the gate wrote as an absolute path inside `cwd` is made relative to it (one outside
 * `cwd` is refused: a snapshot-bound call names only the snapshot); an option that would move the build's OUTPUT (the broker owns the output root) is refused with the canonical route
 * named, never stripped or rewritten; everything left is judged by the tool's registration (snapshot-bound mode) and by the broker's shape rules (`nativeShapeProblem`).
 * `{ args }` or `{ refusal: { code, message } }`. Pure: it reads no file (planning calls it with a directory that does not exist); the file-system rules are `nativeCallFsProblem`.
 */
export function nativeToolLaunch(tool, args, { cwd }) {
  const entry = NATIVE_ADAPTERS[tool];
  const words = args.map(String);
  const refuse = (message) => ({ refusal: { code: "RK_NATIVE_TOOL_REFUSED", message: `${tool} ${words.join(" ")}: ${message}; the broker's ${entry.adapter} adapter (capability ${entry.capability}) would refuse it (rightkit#1241), so it is not sent` } });
  const nameOf = (word) => (word.includes("=") && word.startsWith("--") ? word.slice(0, word.indexOf("=")) : word);
  const owned = words.find((word) => (OUTPUT_OWNED[tool] ?? []).includes(nameOf(word)) || (tool === "cmake" && /^-B./.test(word)));
  if (owned !== undefined) return refuse(`${nameOf(owned)} names where the build writes, and the broker owns the output root (it supplies the one managed location); send the step without it`);
  const pathValued = PATH_VALUED[tool] ?? [];
  const relative = (flag, value) => {
    if (!(path.isAbsolute(value) || path.win32.isAbsolute(value))) return { value };
    const rel = path.relative(path.resolve(cwd), path.resolve(value));
    if (rel === "") return { value: "." };
    if (rel.startsWith("..") || path.isAbsolute(rel)) return { problem: `${flag} ${value} is outside the step's directory ${cwd}` };
    return { value: rel.split(path.sep).join("/") };
  };
  const out = [];
  for (let i = 0; i < words.length; i += 1) {
    const word = words[i];
    const inline = word.startsWith("--") && word.includes("=") && pathValued.includes(word.slice(0, word.indexOf("=")));
    if (inline) {
      const flag = word.slice(0, word.indexOf("="));
      const made = relative(flag, word.slice(flag.length + 1));
      if (made.problem) return refuse(made.problem);
      out.push(`${flag}=${made.value}`);
      continue;
    }
    out.push(word);
    if (!pathValued.includes(word) || i + 1 >= words.length) continue;
    i += 1;
    const made = relative(word, words[i]);
    if (made.problem) return refuse(made.problem);
    out.push(made.value);
  }
  const problem = adapterArgvRefusal(out, nativeGrammar(tool), { snapshot: true }) ?? nativeShapeProblem(tool, out);
  return problem ? refuse(problem) : { args: out };
}

/** Why a committed Gradle wrapper cannot be an adapter's pinned executable: its properties carry no `distributionSha256Sum`. `readFile(relative)` reads the commit's bytes; null when it can. */
export function gradleWrapperPinProblem(readFile, cwd = ".") {
  const properties = path.posix.join(String(cwd).replaceAll("\\", "/"), "gradle", "wrapper", "gradle-wrapper.properties");
  let text = null;
  try { text = readFile(properties.replace(/^\.\//, "")); } catch { text = null; }
  if (typeof text !== "string") return `${properties} is not in the commit, so the Gradle wrapper has no pinned identity`;
  return /^\s*distributionSha256Sum\s*=\s*[0-9a-f]{64}\s*$/m.test(text) ? null : `${properties} carries no distributionSha256Sum, so the Gradle distribution the wrapper downloads is not pinned`;
}

/** The export options an `-exportArchive` reads: a regular file (never a link) of the snapshot, small, an XML property list, that does not ask for an upload (native_adapters.rs `export_options_problem`). */
function exportOptionsProblem(file, fsOps) {
  let meta;
  try { meta = fsOps.lstatSync(file); } catch { return `${file} is not in the snapshot`; }
  if (!meta.isFile()) return `${file} is not a regular file`;
  if (meta.size > 65_536) return `${file} is larger than an export options file`;
  let text;
  try { text = fsOps.readFileSync(file, "utf8"); } catch { return `${file} is not a text property list`; }
  if (!text.replace(/^﻿/, "").trimStart().startsWith("<?xml")) return `${file} must be an XML property list`;
  const key = "<key>destination</key>";
  for (let at = text.indexOf(key); at >= 0; at = text.indexOf(key, at + key.length)) {
    const rest = text.slice(at + key.length).trimStart();
    const value = rest.startsWith("<string>") && rest.includes("</string>") ? rest.slice("<string>".length, rest.indexOf("</string>")).trim() : null;
    if (value === "upload") return `${file} asks the export to upload; a build does not publish`;
  }
  return null;
}

/**
 * What only the file system can say about a call, judged where the call runs (native_adapters.rs `native_call_problem`'s second half), or null. `cwd` is the step's directory in the snapshot.
 * A Gradle call needs the wrapper's distribution pinned (`RK_ADAPTER_PIN_REQUIRED` at the broker: `pin: true`); an `-exportArchive` needs its options file. `{ message, pin }` or null.
 */
export function nativeCallFsProblem(tool, args, { cwd, fsOps = nodeFs } = {}) {
  const caller = args.map(String);
  if (tool === "gradle") {
    let project = path.resolve(cwd);
    for (let i = 0; i < caller.length; i += 1) {
      if ((caller[i] === "-p" || caller[i] === "--project-dir") && i + 1 < caller.length) { project = path.resolve(cwd, caller[i + 1]); i += 1; }
      else if (caller[i].startsWith("--project-dir=")) project = path.resolve(cwd, caller[i].slice("--project-dir=".length));
    }
    const message = gradleWrapperPinProblem((relative) => fsOps.readFileSync(relative, "utf8"), project);
    return message ? { message, pin: true } : null;
  }
  if (tool === "xcodebuild") {
    const plist = splitWords(tool, caller).values.find(([name]) => name === "-exportOptionsPlist")?.[1];
    if (plist === undefined) return null;
    if (!relativeShape(plist)) return { message: `-exportOptionsPlist ${JSON.stringify(plist)} must be a relative path with no \`..\``, pin: false };
    const message = exportOptionsProblem(path.resolve(cwd, plist), fsOps);
    return message ? { message, pin: false } : null;
  }
  return null;
}

// ---- the signing-properties file (RA-08) ----------------------------------------------------------------------------------------------------------------------------
/** The environment variable an Android release hands its Gradle command the path of the transient signing-properties file in (android-release.mjs). Only its PATH ever travels. */
export const NATIVE_SIGNING_FILE_ENV = SIGNING_FILE_ENV;
/** The typed capability a broker would advertise for carrying that file (a lease bound to the owned signing phase). RustRight defines none today (its `SigningFileLease` has no request field). */
export const NATIVE_SIGNING_FILE_CAPABILITY = "native-signing-file-v1";
export const NATIVE_SIGNING_FILE_UNSUPPORTED = "RK_NATIVE_SIGNING_FILE_UNSUPPORTED";
/** The largest signing-properties file a lease binds (native_adapters.rs SIGNING_FILE_MAX_BYTES). */
export const SIGNING_FILE_MAX_BYTES = 16 * 1024;

/**
 * Why `file` is not an acceptable transient signing-properties file of the provisioned signer session (native_adapters.rs `signing_file_problem`, mirrored), or null: an absolute path without
 * `.` or `..` segments, a plain file name, a regular file that is not a link, non-empty and small, reachable only by its owner (POSIX), in a directory not writable by others whose real path is
 * inside the canonical `signerRoot` (the host's own configuration, never a request's). The file holds the secrets; they never ride an argument, an environment value, a carrier or a log.
 */
export function signingFileProblem(file, { signerRoot, fsOps = nodeFs, platform = process.platform } = {}) {
  const shown = String(file);
  if (typeof file !== "string" || shown.includes("\0") || !path.isAbsolute(shown) || shown.split(/[\\/]/).some((part) => part === "." || part === "..")) return `signing file ${shown} must be an absolute path without \`.\` or \`..\``;
  const name = path.basename(shown);
  if (name.length > 128 || name.startsWith(".") || !/^[A-Za-z0-9._-]+$/.test(name)) return `signing file name ${JSON.stringify(name)} must be 1 to 128 of A-Z, a-z, 0-9, '.', '_' and '-', not starting with '.'`;
  let root;
  try { root = fsOps.realpathSync(signerRoot); } catch { return `the signer root ${signerRoot} cannot be resolved`; }
  let meta;
  try { meta = fsOps.lstatSync(shown); } catch (error) { return `signing file ${shown} is not readable: ${error.code ?? error.message}`; }
  if (!meta.isFile()) return `signing file ${shown} is not a regular file (a link or a directory is refused)`;
  if (meta.size === 0 || meta.size > SIGNING_FILE_MAX_BYTES) return `signing file ${shown} must hold 1 to ${SIGNING_FILE_MAX_BYTES} bytes`;
  let parent;
  try { parent = fsOps.realpathSync(path.dirname(shown)); } catch { return `the directory of signing file ${shown} cannot be resolved`; }
  if (parent !== root && !parent.startsWith(root.endsWith(path.sep) ? root : root + path.sep)) return `signing file ${shown} is not inside the signer root ${root}`;
  if (platform !== "win32") {
    if ((meta.mode & 0o077) !== 0) return `signing file ${shown} must be reachable by its owner only`;
    let dir;
    try { dir = fsOps.statSync(parent); } catch { return `the directory of signing file ${shown} is writable by others`; }
    if ((dir.mode & 0o022) !== 0) return `the directory of signing file ${shown} is writable by others`;
  }
  return null;
}

/**
 * The refusal of a signing-file handoff on a broker route, or null when no file is handed. The broker's grammar carries no signing identity and every adapter's `env` is empty, and RustRight
 * has no request field that binds the file to the owned signing phase (its `SigningFileLease` is not wired to a request; the signed Android release phase is an open policy), so the only
 * honest answer today is a typed refusal BEFORE launch: the path never rides RUSTRIGHT_ADAPTER_ENV, an argument or the client's environment, and nothing is sent. A host without a broker
 * (a direct Linux host) never reaches this: its Gradle runs in the gate's own session with the path in its environment, as before.
 */
export function nativeSigningFileRefusal(tool, signingFile, { signerRoot } = {}) {
  if (signingFile === undefined || signingFile === null || signingFile === "") return null;
  const entry = NATIVE_ADAPTERS[tool];
  const shape = signerRoot === undefined ? null : signingFileProblem(signingFile, { signerRoot });
  return { code: NATIVE_SIGNING_FILE_UNSUPPORTED, message: `${tool}: a signing-properties file handoff (${NATIVE_SIGNING_FILE_ENV}, path only) needs the broker capability ${NATIVE_SIGNING_FILE_CAPABILITY}, which no RustRight broker advertises: its ${entry?.adapter ?? tool} adapter admits no environment, and the signed Android release phase is an open policy (the release task itself is refused by the grammar)${shape ? `; the file is also not acceptable (${shape})` : ""}; nothing was sent, and the path is never forwarded in an argument or an environment` };
}

// ---- the managed output mapping (RA-07, RA-09) ------------------------------------------------------------------------------------------------------------------------
/** The leaves under a call's managed state, one per managed location (native_adapters.rs LEAF_*). */
export const NATIVE_STATE_LEAVES = Object.freeze({
  xcodegenCache: "cache", derivedData: "derived-data", archive: "archive/App.xcarchive", export: "export", results: "results",
  gradleHome: "gradle-user-home", gradleCache: "project-cache", gradleBuild: "gradle-build", gradleInit: "output.init.gradle", cmakeBuild: "cmake-build", cmakeInstall: "cmake-install",
});

/** The token naming a job's run-unique leaves (service.rs `native_run_token`): the job id itself when it is 1 to 64 of A-Z, a-z, 0-9, `_`, `-`; else `j` plus 32 hex of its SHA-256. */
export const nativeRunToken = (jobId) => (typeof jobId === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(jobId) ? jobId : `j${createHash("sha256").update(String(jobId)).digest("hex").slice(0, 32)}`);

/** The one managed directory of a tool's state for a call run in `relativeCwd` (snapshot-relative): `<outputDir>/native/<tool>/<sha256 of the components joined by "/">` (native_output_dir). */
export function nativeStateDir(outputDir, tool, relativeCwd = "") {
  const key = String(relativeCwd).split(/[\\/]/).filter((part) => part !== "" && part !== ".").join("/");
  return path.join(outputDir, "native", tool, createHash("sha256").update(key).digest("hex"));
}

/** Whether a call writes managed state at all (xcodegen without `--use-cache` writes only the snapshot inputs it exists to generate). */
export const nativeStateNeeded = (tool, args) => tool !== "xcodegen" || args.map(String).includes("--use-cache");

/**
 * The locations a call may write, by role, exactly as the broker's `managed_call` names them in the receipt (`[{ role, path }]`, in the broker's order), for the call's SENT args under `state`.
 * `jobId` names the run-unique result bundle of a test action; without it that one entry is `{ role: "result-bundle", path: null, unresolved: true }` (the directory is `results`).
 * `{ problem }` when the call has no managed form.
 */
export function nativeManagedOutputs(tool, args, { state, jobId } = {}) {
  const caller = args.map(String);
  const problem = nativeShapeProblem(tool, caller);
  if (problem) return { problem };
  const at = (leaf) => path.join(state, leaf);
  const words = splitWords(tool, caller);
  const L = NATIVE_STATE_LEAVES;
  const out = [];
  const add = (role, file, extra = {}) => out.push({ role, path: file, ...extra });
  if (tool === "xcodegen") { if (hasFlag(words, "--use-cache")) add("cache", at(L.xcodegenCache)); }
  else if (tool === "xcodebuild") {
    const mode = xcodebuildMode(words);
    if (mode === "export") { add("archive", at(L.archive)); add("export", at(L.export)); }
    else {
      add("derived-data", at(L.derivedData));
      if (mode === "archive") add("archive", at(L.archive));
      if (mode === "test") add("result-bundle", jobId === undefined ? null : path.join(at(L.results), `${nativeRunToken(jobId)}.xcresult`), jobId === undefined ? { unresolved: true } : {});
    }
  } else if (tool === "gradle") {
    add("gradle-user-home", at(L.gradleHome)); add("project-cache", at(L.gradleCache)); add("build-tree", at(L.gradleBuild)); add("init-script", at(L.gradleInit));
  } else if (tool === "cmake") {
    const mode = cmakeMode(words, caller);
    add("build-tree", at(L.cmakeBuild));
    if (mode !== "build") add("install-tree", at(L.cmakeInstall));
  }
  return { outputs: out };
}

/**
 * The receipt's output mapping read back (RA-07, RA-09): the signed receipt's `executable.managedOutputs` (`{ tool, state, outputs: [{ role, path }] }`, which the broker writes from the
 * call it really spawned) is accepted only when it is exactly what the SENT args map to under the job's verified `outputDir` and the step's snapshot-relative directory: the same tool, the
 * same state directory, the same roles and paths in the same order. `{ ok: true, state, outputs, byRole }` or `{ ok: false, problem }`; a caller consumes `byRole` (role to absolute path),
 * never a path it composed itself.
 */
export function nativeOutputsFromReceipt(managed, { tool, args, outputDir, relativeCwd = "", jobId }) {
  const bad = (problem) => ({ ok: false, problem });
  if (managed === null || typeof managed !== "object" || !Array.isArray(managed.outputs)) return bad("the receipt carries no managedOutputs (the broker did not report the native call's output mapping)");
  if (typeof outputDir !== "string" || outputDir === "") return bad("the job's verified output directory is not known");
  const state = nativeStateDir(outputDir, tool, relativeCwd);
  const expected = nativeManagedOutputs(tool, args, { state, jobId });
  if (expected.problem) return bad(expected.problem);
  if (managed.tool !== tool) return bad(`the receipt maps a ${JSON.stringify(managed.tool)} call, not ${tool}`);
  if (typeof managed.state !== "string" || path.normalize(managed.state) !== path.normalize(state)) return bad(`the receipt's state ${JSON.stringify(managed.state)} is not the job's managed state ${state}`);
  if (managed.outputs.length !== expected.outputs.length) return bad(`the receipt maps ${managed.outputs.length} output(s), the call's grammar maps ${expected.outputs.length}`);
  for (let i = 0; i < expected.outputs.length; i += 1) {
    const got = managed.outputs[i];
    const exp = expected.outputs[i];
    if (got?.role !== exp.role) return bad(`receipt output ${i} is role ${JSON.stringify(got?.role)}, expected ${exp.role}`);
    if (typeof got.path !== "string") return bad(`receipt output ${exp.role} has no path`);
    if (!exp.unresolved && path.normalize(got.path) !== path.normalize(exp.path)) return bad(`receipt output ${exp.role} is ${got.path}, expected ${exp.path}`);
    if (exp.unresolved && path.dirname(path.normalize(got.path)) !== path.join(state, NATIVE_STATE_LEAVES.results)) return bad(`receipt output ${exp.role} ${got.path} is not under ${path.join(state, NATIVE_STATE_LEAVES.results)}`);
  }
  const outputs = managed.outputs.map(({ role, path: file }) => ({ role, path: file }));
  return { ok: true, state, outputs, byRole: Object.fromEntries(outputs.map(({ role, path: file }) => [role, file])) };
}

