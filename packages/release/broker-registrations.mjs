// What the host's broker has REGISTERED, read from the live host config (the managed control root's config.json, the file the pnpm adapter grammar
// is read from too: pnpm-adapter-grammar.mjs liveAdapterGrammar). The route probe uses it to refuse, before any build, a config that needs a
// registration this host lacks (M31: `cargo ndk` is an external command the host must register; M28/B5/B6: the cmake, xcode and gradle adapters).
//
// Registrations are broker-host configuration (rustright-config `externalCommands` and `adapters`); the gate only reads them. An unreadable
// config is `null` (unverified), never a refusal: the broker itself still refuses an unregistered command at its own door.
import { readFileSync } from "node:fs";
import { resolveManagedRoots } from "./managed-roots.mjs";

/** The adapters this gate's door routes to, beyond the long-standing pnpm, node, QA and swift ones (build-launch.mjs ADAPTER_PROGRAMS). */
export const REQUIRED_ADAPTERS = Object.freeze({ cmake: "rightkit-cmake", xcodegen: "rightkit-xcodegen", xcodebuild: "rightkit-xcodebuild", gradlew: "rightkit-gradle" });

/** The live broker config of this host, parsed, or null when it cannot be read. */
export function liveBrokerConfig({ env = process.env, platform = process.platform, readFile = (file) => readFileSync(file, "utf8") } = {}) {
  try {
    const { configPath } = resolveManagedRoots({ env, platform });
    const parsed = JSON.parse(readFile(configPath));
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch { return null; }
}

/** Whether `config` registers the external command `<shim> <command>` (for cargo-ndk: shim `cargo`, command `ndk`). */
export const externalCommandRegistered = (config, shim, command) => Array.isArray(config?.externalCommands) && config.externalCommands.some((entry) => entry?.shim === shim && entry?.command === command);

/** Whether `config` registers the adapter `name`. */
export const adapterRegistered = (config, name) => Boolean(config?.adapters && typeof config.adapters === "object" && Object.hasOwn(config.adapters, name));

/** Whether an app's release config declares a cargo-ndk build (an `ndk` object with `abis`, at any depth: `development.targets.android.ndk`, `targets.android.ndk`). */
export function declaresNdk(value) {
  if (!value || typeof value !== "object") return false;
  if (Array.isArray(value)) return value.some(declaresNdk);
  return Object.entries(value).some(([key, inner]) => (key === "ndk" && inner && typeof inner === "object" && !Array.isArray(inner) && ("abis" in inner || "jniLibs" in inner)) || declaresNdk(inner));
}
