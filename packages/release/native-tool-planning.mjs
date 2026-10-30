// GB-51 (GATE Mac/Dell broker-only; commit output identity): the native build tools an app config declares (xcodegen and xcodebuild for iOS, Gradle for
// Android, CMake for a sidecar) have no carried adapter route today: the broker-host door refuses each when its step is launched
// (build-launch.mjs, "no broker adapter runs this command yet", rightkit#1241), after earlier stages may already have built. This module names them at
// PLANNING, so the route (and `--route`) refuses before anything runs, with the missing capability named. A supported tool needs a constrained adapter
// (pinned executable identity, allowed argument grammar, committed cwd, one persistent output root, explicit safe env, nested-process attribution,
// cancellation and signed output evidence); the gate takes a tool off this list only when its adapter exists, never by a direct-session exemption.
import path from "node:path";

/** Tools with no carried adapter route, by the capability name a refusal reports. */
export const NATIVE_TOOLS_WITHOUT_ADAPTER = Object.freeze({ xcodegen: "adapter:rightkit-xcodegen", xcodebuild: "adapter:rightkit-xcodebuild", gradle: "adapter:rightkit-gradle", gradlew: "adapter:rightkit-gradle", cmake: "adapter:rightkit-cmake" });

const base = (command) => path.basename(String(command ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/**
 * The native-tool uses of a stage, `[{ tool, capability, where }]`, from the commit's own config. `devPlatform` is the dev target's platform for a dev or
 * test run (its declared build steps and, for iOS, the xcodegen/xcodebuild steps the gate itself generates are in play); a `cmake` sidecar is in play for
 * every stage that stages sidecars.
 */
export function nativeToolUses({ config, to, devPlatform = null }) {
  const uses = [];
  const add = (command, where) => { const tool = base(command); if (Object.hasOwn(NATIVE_TOOLS_WITHOUT_ADAPTER, tool)) uses.push({ tool, capability: NATIVE_TOOLS_WITHOUT_ADAPTER[tool], where }); };
  if (!isObject(config)) return uses;
  if (to === "dev" || to === "test") {
    const target = config.development?.targets?.[devPlatform];
    if (isObject(target)) {
      for (const [i, step] of (Array.isArray(target.build) ? target.build : []).entries()) if (isObject(step)) add(step.cmd, `development.targets.${devPlatform}.build[${i}]`);
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

/** The refusal message for a stage's native-tool uses, or null. */
export function nativeToolRefusal(uses) {
  if (!uses.length) return null;
  const caps = [...new Set(uses.map((u) => u.capability))];
  return `${uses.map((u) => `${u.tool} at ${u.where}`).join("; ")}: no carried broker adapter runs ${uses.length === 1 ? "this tool" : "these tools"} yet (missing ${caps.join(", ")}), and a broker-managed host never runs a build tool around the broker (rightkit#1241); nothing ran`;
}
