// Everything the sealed `build` path does differently for a SwiftPM target, as pure/stub-friendly
// functions so build-release.mjs only branches once. Rust-only steps (rustc/cargo probes, Cargo lock
// identity, broker cargo target, Cache V2 shared Rust cache, target bridge, CARGO/SCCACHE env) are skipped.

import path from "node:path";
import { resolveSwiftCacheIdentity } from "./cache-policy.mjs";

const RUST_ENV = ["CARGO_HOME", "CARGO_TARGET_DIR", "RUSTC_WRAPPER", "SCCACHE_DIR", "SCCACHE_BASEDIRS", "SCCACHE_CACHE_SIZE", "SCCACHE_IGNORE_SERVER_IO_ERROR", "RUSTFLAGS", "RIGHT_RELEASE_CACHE_OWNER"];

export function swiftArchitecture({ target = {}, platform, hostArch = process.arch }) {
  const arch = target.msix?.identity?.architecture ?? target.architecture ?? (hostArch === "arm64" ? "arm64" : "x64");
  return platform === "mac" ? (arch === "x64" ? "x86_64" : arch) : arch;
}

/** exec(cmd, args) -> stdout string. Throws when swift is missing. */
export function collectSwiftToolVersions({ packageManager, exec, nodeVersion = process.version }) {
  const swift = exec("swift", ["--version"]).trim();
  if (!swift) throw new Error("swift --version printed nothing");
  return { node: nodeVersion, packageManager: packageManager ? `${packageManager} ${exec(packageManager, ["--version"]).trim()}` : null, swift, cargo: null, rustc: null, rustHost: null, sccache: null };
}

/**
 * Plan for a swiftpm sealed build: identity, forced non-shared cache mode, no broker cargo target, no target
 * bridge, a plain build/cache dir under the vault, and an environment with every Rust variable removed.
 */
export function planSwiftBuild({ appRoot, vaultRoot, platform, target, toolVersions, profile = "release", hostArch }) {
  const architecture = swiftArchitecture({ target, platform, hostArch });
  const identity = resolveSwiftCacheIdentity({ swiftVersion: toolVersions.swift, packageResolvedPath: path.join(appRoot, "Package.resolved"), architecture, profile: target.profile ?? profile });
  const buildDir = path.resolve(vaultRoot, "cache", "swift-build", platform, identity.fingerprint);
  return {
    identity, architecture, buildDir,
    cacheMode: "legacy",
    managedCargoTarget: null,
    targetBridge: { run: (fn) => fn(), ensure() {} },
    sanitizeEnv(env) {
      const next = { ...env };
      for (const name of RUST_ENV) delete next[name];
      next.RIGHT_RELEASE_SWIFT_BUILD_DIR = buildDir;
      return next;
    },
    watchDirs: [buildDir, path.join(appRoot, ".build")],
  };
}
