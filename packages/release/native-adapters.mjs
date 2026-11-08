// GB-51 / RA-07, RA-08, RA-09: the gate's launch mapping of the native build tools (xcodegen, xcodebuild, Gradle, CMake) onto their broker adapters, and nothing more. Plan section 4
// ("no capability table, no adapter grammar"): the gate no longer holds RustRight's reference registrations, its argument grammar, its shape rules or its file rules. The broker refuses at
// admission, before spawn: an adapter that is not registered (RK_ADAPTER_UNKNOWN), an unregistered flag, a value of the wrong kind or a denied build setting (RK_ADAPTER_ARGV_DENIED,
// adapter.rs check_argv), an environment key (RK_ADAPTER_ENV_DENIED), and, before it makes any state, a call with no managed form, an export options file it will not read and a Gradle wrapper
// whose distribution is not pinned (native_call_problem: RK_ADAPTER_ARGV_DENIED, RK_ADAPTER_PIN_REQUIRED). What stays here is what only the caller can do or read back:
//   - the tool-to-adapter mapping (`rustright run rightkit-<tool> -- <args>`);
//   - making a path the gate generated as an absolute one under the step's directory relative to it (a snapshot-bound adapter takes relative paths);
//   - the signing-properties file handoff: no secret rides an argument or an environment key. The one transient Android signing-properties file is a typed FILE handoff (`signingFileProblem`
//     mirrors the broker's `signing_file_problem`); RustRight has no request field that carries it, so a broker route refuses it before launch (`nativeSigningFileRefusal`), a refusal no broker makes;
//   - the receipt's output mapping (`nativeManagedOutputs`, `nativeOutputsFromReceipt`): the roles and absolute paths under the job's managed state, read back and compared with the receipt, never guessed.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";
import { SIGNING_FILE_ENV } from "./adapter-env.mjs";

/** The broker adapter of each native tool (the broker's configuration registers exactly these ids; an unregistered one is RK_ADAPTER_UNKNOWN at admission). */
export const NATIVE_ADAPTERS = Object.freeze({
  xcodegen: Object.freeze({ adapter: "rightkit-xcodegen" }),
  xcodebuild: Object.freeze({ adapter: "rightkit-xcodebuild" }),
  gradle: Object.freeze({ adapter: "rightkit-gradle" }),
  cmake: Object.freeze({ adapter: "rightkit-cmake" }),
});

const base = (command) => path.basename(String(command ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();
/** The native tool a command names (`./gradlew` is Gradle), or null. */
export const nativeToolOf = (command) => { const b = base(command); const tool = b === "gradlew" ? "gradle" : b; return Object.hasOwn(NATIVE_ADAPTERS, tool) ? tool : null; };

// Flags whose value is a path the gate generated as an absolute one under the step's directory: a snapshot-bound adapter takes it relative to the job's cwd.
const PATH_VALUED = Object.freeze({ xcodegen: ["--spec", "--project"], xcodebuild: ["-project", "-exportOptionsPlist"], gradle: ["-p", "--project-dir"], cmake: ["-S"] });

/**
 * The argv a carried native-tool step sends: a path flag the gate wrote as an absolute path inside `cwd` is made relative to it (one outside `cwd` is refused: a snapshot-bound call names only
 * the snapshot). Nothing else is judged or changed: the broker refuses an output override, an unregistered flag or a call it has no managed form for. Pure: it reads no file.
 * `{ args }` or `{ refusal: { code, message } }`.
 */
export function nativeToolLaunch(tool, args, { cwd }) {
  const words = args.map(String);
  const refuse = (message) => ({ refusal: { code: "RK_NATIVE_TOOL_REFUSED", message: `${tool} ${words.join(" ")}: ${message}; a snapshot-bound call names only the snapshot, so it is not sent` } });
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
  return { args: out };
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

// What a call is, as the broker reads it (native_adapters.rs `xcodebuild_mode`, `cmake_mode`), for an argv the broker has ADMITTED: it decides which managed locations the receipt must name.
// Not a judgment of the argv: a call the broker refuses never has a receipt.
const XCODEBUILD_VALUED = new Set(["-project", "-scheme", "-configuration", "-destination", "-sdk", "-testPlan", "-exportOptionsPlist"]);
const XCODEBUILD_TEST_ACTIONS = ["test", "test-without-building", "build-for-testing"];
function xcodebuildMode(args) {
  if (args.includes("-exportArchive")) return "export";
  const actions = [];
  for (let i = 0; i < args.length; i += 1) {
    const word = args[i];
    if (XCODEBUILD_VALUED.has(word)) { i += 1; continue; }
    if (!word.startsWith("-") && !word.includes("=")) actions.push(word);
  }
  if (actions.includes("archive")) return "archive";
  return actions.some((action) => XCODEBUILD_TEST_ACTIONS.includes(action)) ? "test" : "build";
}
const cmakeMode = (args) => (args[0] === "--build" ? "build" : args[0] === "--install" ? "install" : "configure");

/**
 * The locations a call may write, by role, exactly as the broker's `managed_call` names them in the receipt (`[{ role, path }]`, in the broker's order), for the call's SENT args under `state`.
 * `jobId` names the run-unique result bundle of a test action; without it that one entry is `{ role: "result-bundle", path: null, unresolved: true }` (the directory is `results`).
 * `{ outputs }`.
 */
export function nativeManagedOutputs(tool, args, { state, jobId } = {}) {
  const caller = args.map(String);
  const at = (leaf) => path.join(state, leaf);
  const L = NATIVE_STATE_LEAVES;
  const out = [];
  const add = (role, file, extra = {}) => out.push({ role, path: file, ...extra });
  if (tool === "xcodegen") { if (caller.includes("--use-cache")) add("cache", at(L.xcodegenCache)); }
  else if (tool === "xcodebuild") {
    const mode = xcodebuildMode(caller);
    if (mode === "export") { add("archive", at(L.archive)); add("export", at(L.export)); }
    else {
      add("derived-data", at(L.derivedData));
      if (mode === "archive") add("archive", at(L.archive));
      if (mode === "test") add("result-bundle", jobId === undefined ? null : path.join(at(L.results), `${nativeRunToken(jobId)}.xcresult`), jobId === undefined ? { unresolved: true } : {});
    }
  } else if (tool === "gradle") {
    add("gradle-user-home", at(L.gradleHome)); add("project-cache", at(L.gradleCache)); add("build-tree", at(L.gradleBuild)); add("init-script", at(L.gradleInit));
  } else if (tool === "cmake") {
    add("build-tree", at(L.cmakeBuild));
    if (cmakeMode(caller) !== "build") add("install-tree", at(L.cmakeInstall));
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
