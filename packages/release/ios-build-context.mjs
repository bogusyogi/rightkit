// What the sealed `build` path does differently for a native iOS app with no Cargo host (no src-tauri, no
// nativeAssembly manifest; iosPackage.prepare commands are refused there, see release-invocation.mjs), so the
// release owns no Cargo manifest, lock, target directory, broker target or Rust cache identity. Pure and
// stub-friendly, the same shape as swift-build-context.mjs, so build-release.mjs branches once.

import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import path from "node:path";
import { declaredCargoManifest } from "./native-cargo-layout.mjs";
import { isNativeIosTarget, nativeIosPrepareProblems } from "./release-invocation.mjs";

// Everything the release lane exports for Rust builds is removed from a native iOS build: releaseEnvironment() sets
// CARGO_TARGET_DIR, CARGO_HOME, SCCACHE_DIR, RUSTC_WRAPPER (and in shared mode SCCACHE_CACHE_SIZE, SCCACHE_BASEDIRS,
// SCCACHE_IGNORE_SERVER_IO_ERROR), and the host shell may carry RUSTFLAGS, RUSTC_*, CARGO_REGISTRY_TOKEN and the like. Matching by
// prefix keeps any later addition covered.
const RUST_ENV_PREFIXES = ["CARGO_", "RUST", "SCCACHE_", "RIGHT_RELEASE_CACHE_OWNER"];
// Kept as the host has them, so a hand `cargo build` in iosPackage.prepare behaves as it does in the user's shell: harmless tuning
// knobs, the rustup toolchain locator, CARGO_HOME (the release lane's value is replaced by the host's own, or removed when the host
// has none) and the proxy / network settings.
const RUST_ENV_KEEP = new Set(["CARGO_BUILD_JOBS", "CARGO_JOBS", "RUST_TEST_THREADS", "CARGO_HOME"]);
const RUST_ENV_KEEP_PREFIXES = ["RUSTUP_", "CARGO_NET_", "CARGO_HTTP_"];
const isRustEnv = (upper) => RUST_ENV_PREFIXES.some((prefix) => upper.startsWith(prefix));
const isKeptRustEnv = (upper) => RUST_ENV_KEEP.has(upper) || RUST_ENV_KEEP_PREFIXES.some((prefix) => upper.startsWith(prefix));

/**
 * env is the release lane's environment (host variables plus what releaseEnvironment exported); host is the process
 * environment the release was started from. Release-lane Rust variables are dropped; the kept ones come from host, never from the lane.
 */
export function stripRustEnvironment(env, host = env) {
  const next = {};
  for (const [name, value] of Object.entries(env)) if (!isRustEnv(name.toUpperCase())) next[name] = value;
  for (const [name, value] of Object.entries(host)) {
    const upper = name.toUpperCase();
    if (isRustEnv(upper) && isKeptRustEnv(upper)) next[name] = value;
  }
  return next;
}

// ---- release-switch inputs: the Rust the prepare commands build must be checked, never skipped --------------------------

/**
 * What the release-switch gates need for a build that runs Rust: the manifest to resolve the feature closure from
 * (targets.<os>.cargoManifest, else the declared nativeAssembly.cargoManifest), the iosPackage.prepare commands to scan, the
 * prepare cargo commands whose --features / --no-default-features / --all-features define the closure, and the reasons the
 * features cannot be determined (which refuse the build). A prepare command that is a shell or wrapper, or a cargo command
 * whose feature list comes from an unexpanded variable, hides what it builds, so it is refused when a Rust manifest is declared.
 */
export function releaseSwitchInputs({ appRoot, config, target, prepare = [], nativeIos = false }) {
  // Every other platform, and iOS with src-tauri, keeps the pre-existing resolver order: targets.<os>.cargoManifest only.
  if (!nativeIos) return { cargoManifest: target?.cargoManifest, commands: [], cargoCommands: [], problems: [] };
  const cargoManifest = target?.cargoManifest ?? declaredCargoManifest(config);
  const commands = prepare.map((command, index) => ({ label: `iosPackage.prepare[${index}]`, command }));
  const cargoCommands = prepare.filter((command) => command?.cmd === "cargo");
  return { cargoManifest, commands, cargoCommands, problems: nativeIosPrepareProblems({ appRoot, config, prepare }) };
}

/** Bare executable names the iosPackage.prepare commands invoke (a path such as ./script.sh is the app's own file, not a tool). */
export function iosPrepareExecutables(iosSpec) {
  return [...new Set((iosSpec?.prepare ?? []).map((command) => command?.cmd).filter((cmd) => typeof cmd === "string" && cmd && !/[\\/]/.test(cmd)))];
}

/**
 * The iOS side of the sealed build, with every host effect injected: whether the app owns a Cargo layout (null when not), the
 * tool versions to record, and nothing probed before its presence is asserted. exec(cmd, args) -> stdout;
 * assertExecutables(names) throws the normal "required executable not found" failure.
 */
export function resolveIosBuildContext({ appRoot, config, target, iosSpec, exec, assertExecutables, exists = existsSync }) {
  // An iOS app with src-tauri (Pulse) returns null: its build is exactly the Cargo-backed one, with nothing added or probed here.
  if (!isNativeIosTarget({ appRoot, target })) return null;
  const hasPackageJson = exists(path.join(appRoot, "package.json"));
  const packageManager = hasPackageJson ? config.packageManager ?? "pnpm" : null;
  const rust = Boolean(declaredCargoManifest(config));
  assertExecutables(["xcodebuild", ...(iosSpec.projectSpec ? ["xcodegen"] : []), ...(packageManager ? [packageManager] : []), ...(rust ? ["cargo", "rustc"] : [])]);
  const toolVersions = collectIosNativeToolVersions({ packageManager, exec, projectSpec: iosSpec.projectSpec, rust });
  return { nativeIos: true, nativeLayout: null, hasPackageJson, toolVersions, prepareExecutables: iosPrepareExecutables(iosSpec) };
}

const oneLine = (text) => String(text ?? "").trim().split(/\r?\n/).map((line) => line.trim()).filter(Boolean).join(" ");

/**
 * The Xcode toolchain an iOS release is built with, in the exact form the IPA receipt records it
 * (`xcodebuild -version`, one line). `xcodegen --version` only when iosPackage names a project.yml.
 * exec(cmd, args) -> stdout string; throws when the tool is missing, which is correct: the iOS lane cannot build without it.
 */
export function collectIosXcodeToolVersions({ exec, projectSpec }) {
  const xcodebuild = oneLine(exec("xcodebuild", ["-version"]));
  if (!xcodebuild) throw new Error("xcodebuild -version printed nothing");
  const xcodegen = projectSpec ? oneLine(exec("xcodegen", ["--version"])) : null;
  if (projectSpec && !xcodegen) throw new Error("xcodegen --version printed nothing");
  return { xcodebuild, xcodegen };
}

/** Tool versions for a native iOS build: Rust fields are explicit nulls unless a Rust manifest is declared (then cargo -V and rustc -V). */
export function collectIosNativeToolVersions({ packageManager, exec, projectSpec, rust = false, nodeVersion = process.version }) {
  return {
    node: nodeVersion,
    packageManager: packageManager ? `${packageManager} ${exec(packageManager, ["--version"]).trim()}` : null,
    ...collectIosXcodeToolVersions({ exec, projectSpec }),
    cargo: rust ? oneLine(exec("cargo", ["-V"])) : null,
    rustc: rust ? oneLine(exec("rustc", ["-V"])) : null,
    rustHost: null,
    sccache: null,
  };
}

export function iosArchitecture({ hostArch = process.arch } = {}) {
  return hostArch === "arm64" ? "arm64" : "x64";
}

/**
 * Plan for a Cargo-less iOS sealed build: identity from the Xcode toolchain, forced non-shared cache mode, no broker
 * cargo target, no target bridge, a plain cache dir under the vault, and an environment with every Rust variable removed.
 */
export function planIosNativeBuild({ vaultRoot, appRoot, platform = "ios", toolVersions, profile = "release", hostArch, hostEnv = process.env }) {
  if (!toolVersions?.xcodebuild) throw new Error("iOS build identity requires the xcodebuild version");
  const architecture = iosArchitecture({ hostArch });
  const payload = { projectType: "ios-native", xcodebuild: toolVersions.xcodebuild, xcodegen: toolVersions.xcodegen ?? null, architecture, profile };
  const identity = { ...payload, target: architecture, features: [], fingerprint: createHash("sha256").update(JSON.stringify(payload)).digest("hex").slice(0, 16) };
  const buildDir = path.resolve(vaultRoot, "cache", "ios-native", platform, identity.fingerprint);
  return {
    identity, architecture, buildDir,
    cacheMode: "legacy",
    managedCargoTarget: null,
    targetBridge: { run: (fn) => fn(), ensure() {} },
    // The kept Rust variables (CARGO_HOME, proxy settings, rustup) come from the environment the release was started from.
    sanitizeEnv: (env) => stripRustEnvironment(env, hostEnv),
    watchDirs: [buildDir, path.join(appRoot, ".cache")],
  };
}
