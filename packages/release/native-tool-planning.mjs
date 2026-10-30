// GB-51 (GATE Mac/Dell broker-only; commit output identity): the native build tools an app config declares (xcodegen and xcodebuild for iOS, Gradle for Android, CMake for a sidecar)
// each have a constrained broker adapter (native-adapters.mjs: capability `adapter:rightkit-<tool>`) and a launch route onto it (build-launch.mjs). This module decides at PLANNING, from the
// commit's own config, whether a stage that uses them can run, so the route (and `--route`) refuses before anything builds, naming the missing capability or the unsupported command:
//   - a tool whose adapter capability the broker does not advertise (or whose capability query failed) is refused naming `adapter:rightkit-<tool>`;
//   - a declared build step (a dev target's `build` list) whose argv the tool's registration would refuse (an output override, an unregistered flag, a release task, a signing identity) is
//     refused naming the capability and the reason: an unsupported custom command never reaches the broker;
//   - a Gradle step needs the committed wrapper's distribution pinned (`distributionSha256Sum`), the adapter's pinned executable identity.
// There is no direct-session exemption: a tool leaves the refusal only when its adapter is advertised.
import path from "node:path";
import { NATIVE_ADAPTERS, gradleWrapperPinProblem, nativeToolLaunch } from "./native-adapters.mjs";

/** Tools with no carried adapter route, by the capability name a refusal reports. */
/** The tools that run through a constrained adapter, by the capability name a refusal reports (xcodegen, xcodebuild, gradle and its wrapper, cmake). */
export const NATIVE_TOOLS_WITHOUT_ADAPTER = Object.freeze({ xcodegen: NATIVE_ADAPTERS.xcodegen.capability, xcodebuild: NATIVE_ADAPTERS.xcodebuild.capability, gradle: NATIVE_ADAPTERS.gradle.capability, gradlew: NATIVE_ADAPTERS.gradle.capability, cmake: NATIVE_ADAPTERS.cmake.capability });

const base = (command) => path.basename(String(command ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/**
 * The native-tool uses of a stage, `[{ tool, capability, where }]`, from the commit's own config. `devPlatform` is the dev target's platform for a dev or
 * test run (its declared build steps and, for iOS, the xcodegen/xcodebuild steps the gate itself generates are in play); a `cmake` sidecar is in play for
 * every stage that stages sidecars.
 */
export function nativeToolUses({ config, to, devPlatform = null }) {
  const uses = [];
  const add = (command, where, args = null, cwd = ".") => { const tool = base(command); if (Object.hasOwn(NATIVE_TOOLS_WITHOUT_ADAPTER, tool)) uses.push({ tool, capability: NATIVE_TOOLS_WITHOUT_ADAPTER[tool], where, ...(args ? { args, cwd } : {}) }); };
  if (!isObject(config)) return uses;
  if (to === "dev" || to === "test") {
    const target = config.development?.targets?.[devPlatform];
    if (isObject(target)) {
      for (const [i, step] of (Array.isArray(target.build) ? target.build : []).entries()) if (isObject(step)) add(step.cmd, `development.targets.${devPlatform}.build[${i}]`, Array.isArray(step.args) ? step.args.map(String) : [], typeof step.cwd === "string" ? step.cwd : ".");
      if (devPlatform === "ios") {
        if (target.projectSpec) add("xcodegen", `development.targets.ios.projectSpec ${target.projectSpec} (xcodegen generate)`);
        add("xcodebuild", "development.targets.ios (xcodebuild build)");
      }
    }
  }
  for (const [i, entry] of (Array.isArray(config.sidecars?.sidecars) ? config.sidecars.sidecars : Array.isArray(config.sidecars) ? config.sidecars : []).entries()) {
    if (isObject(entry) && entry.kind === "cmake") add("cmake", `sidecars[${i}] ${entry.name ?? ""} (kind cmake)`);
  }
  return uses;
}

/**
 * The refusal message for a stage's native-tool uses, or null. `capabilities` is the broker's advertised state (`{ ok, capabilities }` from `rustright doctor --json`, or null when it was not
 * queried: every use is then unsupported). `readFile(relative)` reads the COMMIT's bytes (a Gradle wrapper's properties).
 */
export function nativeToolRefusal(uses, { capabilities = null, readFile = null } = {}) {
  if (!uses.length) return null;
  const advertised = capabilities?.ok === true && Array.isArray(capabilities.capabilities) ? new Set(capabilities.capabilities) : new Set();
  const problems = [];
  for (const use of uses) {
    if (!advertised.has(use.capability)) { problems.push(`${use.tool} at ${use.where}: the broker does not advertise ${use.capability}${capabilities?.ok === true ? "" : ` (${capabilities?.problem ?? "its capabilities were not queried"})`}`); continue; }
    if (use.args) {
      const launch = nativeToolLaunch(use.tool === "gradlew" ? "gradle" : use.tool, use.args, { cwd: path.posix.resolve("/", use.cwd ?? ".") });
      if (launch.refusal) { problems.push(`${use.tool} at ${use.where}: ${launch.refusal.message}`); continue; }
    }
    if ((use.tool === "gradle" || use.tool === "gradlew") && typeof readFile === "function") {
      const pin = gradleWrapperPinProblem(readFile, use.cwd ?? ".");
      if (pin) problems.push(`${use.tool} at ${use.where}: ${pin}`);
    }
  }
  if (!problems.length) return null;
  return `${problems.join("; ")} (a broker-managed host never runs a build tool around the broker, rightkit#1241; the capability names are in docs/GATE-CONTRACT.md "Native tool adapters"); nothing ran`;
}
