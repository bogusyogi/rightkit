// GB-51: the constrained adapters of the native build tools (xcodegen, xcodebuild, Gradle, CMake): the reference registrations RustRight registers EXACTLY (capability names,
// grammars, environment), and the gate's launch mapping onto them. Each is a snapshot-bound adapter job: a pinned executable identity (the registration's), a committed cwd (the
// snapshot's counterpart of the step's directory), an argument grammar judged BEFORE the send by the same parser the pnpm registration uses (pnpm-adapter-grammar.mjs, a port of
// RustRight's adapter scan), no environment, and an output root the BROKER owns: a caller never names where the build writes (`-derivedDataPath`, `-resultBundlePath`, a cmake
// build tree outside the snapshot, a Gradle user or cache home) and never supplies a signing identity (signing stays in the provisioned identity and session with its evidence
// handed off). Nested processes (the compiler the tool starts) are attributed to the job by the broker's capability token; cancellation reaps the tree; the signed receipt carries the
// output directory. Nothing here is a direct-session exemption: an unsupported custom command, an unregistered tool or an output override refuses at planning, naming the capability.
//
// The capability names are the contract with RustRight (docs/GATE-CONTRACT.md "Native tool adapters"): a broker advertises `adapter:<name>` in its capabilities, and the gate
// plans a stage that uses the tool only when it does.
import path from "node:path";
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
      flags: flags({ "-project": "relpath", "-scheme": "any", "-configuration": "identifier", "-destination": "any", "-sdk": "identifier", "-quiet": "none", "-allowProvisioningUpdates": "none", "-allowProvisioningDeviceRegistration": "none" }),
      positionals: "actionOrBuildSetting", positionalValues: ["build", "build-for-testing", "test", "test-without-building"], env: [] },
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
      flags: flags({ "-S": "relpath", "-B": "relpath", "-G": "any", "-A": "identifier", "--build": "relpath", "--config": "identifier", "--target": "any", "--parallel": "none", "--install": "relpath" }),
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
const PATH_VALUED = Object.freeze({ xcodegen: ["--spec", "--project"], xcodebuild: ["-project"], gradle: ["-p", "--project-dir"], cmake: ["-S", "-B", "--build", "--install"] });
const OUTPUT_OWNED = Object.freeze({ xcodebuild: ["-derivedDataPath", "-resultBundlePath", "-archivePath", "-exportPath"], gradle: ["--gradle-user-home", "-g", "--project-cache-dir", "--init-script", "-I", "--include-build"], cmake: ["--install-prefix", "--toolchain"], xcodegen: ["--cache-path"] });

/**
 * The argv a carried native-tool step sends, or a typed refusal naming the capability: a path flag the gate wrote as an absolute path inside `cwd` is made relative to it (one outside
 * `cwd` is refused: a snapshot-bound call names only the snapshot); an option that would move the build's OUTPUT (the broker owns the output root) is refused with the canonical route
 * named, never stripped or rewritten; everything left is judged by the tool's registration (snapshot-bound mode). `{ args }` or `{ refusal: { code, message } }`.
 */
export function nativeToolLaunch(tool, args, { cwd }) {
  const entry = NATIVE_ADAPTERS[tool];
  const words = args.map(String);
  const refuse = (message) => ({ refusal: { code: "RK_NATIVE_TOOL_REFUSED", message: `${tool} ${words.join(" ")}: ${message}; the broker's ${entry.adapter} adapter (capability ${entry.capability}) would refuse it (rightkit#1241), so it is not sent` } });
  const owned = words.find((w) => (OUTPUT_OWNED[tool] ?? []).includes(w.includes("=") && w.startsWith("--") ? w.slice(0, w.indexOf("=")) : w));
  if (owned !== undefined) return refuse(`${owned.split("=")[0]} names where the build writes, and the broker owns the output root (it supplies the one managed location); send the step without it`);
  const out = [];
  for (let i = 0; i < words.length; i += 1) {
    const word = words[i];
    out.push(word);
    if (!(PATH_VALUED[tool] ?? []).includes(word) || i + 1 >= words.length) continue;
    i += 1;
    const value = words[i];
    if (path.isAbsolute(value) || path.win32.isAbsolute(value)) {
      const rel = path.relative(path.resolve(cwd), path.resolve(value));
      if (rel === "" ) { out.push("."); continue; }
      if (rel.startsWith("..") || path.isAbsolute(rel)) return refuse(`${word} ${value} is outside the step's directory ${cwd}`);
      out.push(rel.split(path.sep).join("/"));
    } else out.push(value);
  }
  const problem = adapterArgvRefusal(out, nativeGrammar(tool), { snapshot: true });
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
