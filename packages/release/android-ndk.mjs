// right-release Android native step: Rust .so libraries built per ABI with cargo-ndk into the Gradle jniLibs directory, before
// Gradle runs. The plan is pure data (commands, never a shell string) so tests build and inspect it without running anything.
//
// Config (release: targets.android.ndk; dev: development.targets.android.ndk):
//   ndk: {
//     abis: ["arm64-v8a"],              // release default; dev default is ["arm64-v8a", "x86_64"] (x86_64 is the emulator)
//     jniLibs: "android/app/src/main/jniLibs",   // GENERATED directory named exactly jniLibs, inside the app, never the app root
//     manifestPath?: "rust/Cargo.toml", package?: "heardright-android", features?: ["a", "b"],
//     apiLevel?: 26,                    // cargo-ndk --platform (minSdk)
//     cwd?: ".", cmd?: "cargo", timeoutMs?,
//   }
// Release builds always pass --release; dev builds use Cargo's built-in dev profile (no flag). Nothing else reaches cargo-ndk:
// no free-form arguments, so a release can never be redirected to a debug profile or a secret passed on argv.
import { rmSync } from "node:fs";
import path from "node:path";

export const ANDROID_ABIS = ["arm64-v8a", "armeabi-v7a", "x86_64", "x86"];
export const ANDROID_RELEASE_DEFAULT_ABIS = ["arm64-v8a"];
export const ANDROID_DEV_DEFAULT_ABIS = ["arm64-v8a", "x86_64"];

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const nonEmpty = (value) => typeof value === "string" && value.trim() !== "";
const inside = (root, child) => { const rel = path.relative(path.resolve(root), path.resolve(child)); return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel); };
const KNOWN_KEYS = ["abis", "jniLibs", "manifestPath", "package", "features", "apiLevel", "cwd", "cmd", "timeoutMs"];
const FEATURE = /^[A-Za-z0-9_][A-Za-z0-9_./-]*$/;
const PACKAGE = /^[A-Za-z0-9_][A-Za-z0-9_-]*$/;

// Returns the normalised ndk config, or throws an Error naming the problem. `root` is the app root.
export function validateAndroidNdk(ndk, { root, kind = "release" } = {}) {
  if (!isObject(ndk)) throw new Error("ndk must be an object");
  const unknown = Object.keys(ndk).filter((key) => !KNOWN_KEYS.includes(key));
  if (unknown.length) throw new Error(`ndk has unsupported key(s) ${unknown.join(", ")}; cargo-ndk arguments are not free-form`);
  const abis = ndk.abis === undefined ? [...(kind === "dev" ? ANDROID_DEV_DEFAULT_ABIS : ANDROID_RELEASE_DEFAULT_ABIS)] : ndk.abis;
  if (!Array.isArray(abis) || abis.length === 0 || abis.some((abi) => !ANDROID_ABIS.includes(abi)) || new Set(abis).size !== abis.length) {
    throw new Error(`ndk.abis must be a non-empty list of distinct ABIs from ${ANDROID_ABIS.join(", ")}`);
  }
  if (!nonEmpty(ndk.jniLibs) || path.isAbsolute(ndk.jniLibs) || path.win32.isAbsolute(ndk.jniLibs)) throw new Error("ndk.jniLibs must be a relative path inside the app");
  const jniLibs = path.resolve(root, ndk.jniLibs);
  if (!inside(root, jniLibs) || path.basename(jniLibs) !== "jniLibs") throw new Error("ndk.jniLibs must name a generated directory called jniLibs inside the app (it is emptied before every build)");
  if (ndk.manifestPath !== undefined && (!nonEmpty(ndk.manifestPath) || path.isAbsolute(ndk.manifestPath) || path.win32.isAbsolute(ndk.manifestPath) || ndk.manifestPath.split(/[\\/]/).includes(".."))) {
    throw new Error("ndk.manifestPath must be a relative path inside the app");
  }
  if (ndk.package !== undefined && (typeof ndk.package !== "string" || !PACKAGE.test(ndk.package))) throw new Error("ndk.package must be a Cargo package name");
  if (ndk.features !== undefined && (!Array.isArray(ndk.features) || ndk.features.some((feature) => typeof feature !== "string" || !FEATURE.test(feature)))) throw new Error("ndk.features must be a list of Cargo feature names");
  if (ndk.apiLevel !== undefined && (!Number.isInteger(ndk.apiLevel) || ndk.apiLevel < 21 || ndk.apiLevel > 99)) throw new Error("ndk.apiLevel must be an Android API level (21 or higher)");
  if (ndk.cmd !== undefined && (!nonEmpty(ndk.cmd) || /\s/.test(ndk.cmd.trim()))) throw new Error("ndk.cmd must be one executable");
  const cwd = path.resolve(root, ndk.cwd ?? ".");
  if (path.resolve(root) !== cwd && !inside(root, cwd)) throw new Error("ndk.cwd must stay inside the app");
  return {
    abis, jniLibs, jniLibsRelative: String(ndk.jniLibs).replaceAll("\\", "/"), manifestPath: ndk.manifestPath, package: ndk.package,
    features: ndk.features ?? [], apiLevel: ndk.apiLevel, cwd, cmd: ndk.cmd ?? "cargo", timeoutMs: ndk.timeoutMs,
  };
}

// The one cargo-ndk command. profile "release" adds --release; "dev" adds nothing (Cargo's built-in dev profile).
export function androidNdkCommand(ndk, { profile = "release" } = {}) {
  if (profile !== "release" && profile !== "dev") throw new Error(`unsupported Android Cargo profile: ${profile}`);
  const args = ["ndk"];
  if (ndk.apiLevel !== undefined) args.push("--platform", String(ndk.apiLevel));
  for (const abi of ndk.abis) args.push("-t", abi);
  args.push("-o", ndk.jniLibs, "build");
  if (profile === "release") args.push("--release");
  if (ndk.manifestPath) args.push("--manifest-path", ndk.manifestPath);
  if (ndk.package) args.push("-p", ndk.package);
  if (ndk.features.length) args.push("--features", ndk.features.join(","));
  return { cmd: ndk.cmd, args, cwd: ndk.cwd, ...(ndk.timeoutMs === undefined ? {} : { timeoutMs: ndk.timeoutMs }) };
}

// Runs the native step. `run(command)` executes the command (release.mjs: runCommand, which already honours --dry-run);
// `clean` empties the generated jniLibs directory first so a stale .so (or an ABI no longer configured) can never ship.
export async function runAndroidNdk({ ndk, profile = "release", dryRun = false, run, clean = (dir) => rmSync(dir, { recursive: true, force: true }), log = console.log }) {
  const command = androidNdkCommand(ndk, { profile });
  if (dryRun) log(`right-release: dry-run: cargo-ndk ${ndk.abis.join(",")} (${profile}) -> ${ndk.jniLibsRelative}`);
  else clean(ndk.jniLibs);
  await run(command);
  return { command, abis: ndk.abis, jniLibs: ndk.jniLibs };
}
