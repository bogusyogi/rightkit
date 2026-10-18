// Project-type awareness for release preflight: which toolchain a target needs.
// Pure; filesystem access is injectable. Swift (SwiftPM) apps need `swift`, not
// cargo/rustc; Tauri/Rust targets keep their existing requirements.

import { existsSync } from "node:fs";
import path from "node:path";

export const PROJECT_TYPES = Object.freeze(["tauri", "cargo", "swiftpm"]);

/**
 * Explicit `target.projectType` / `config.projectType` wins. Otherwise a Package.swift with no Cargo
 * manifest (root or src-tauri) is swiftpm; everything else is tauri/cargo (unchanged behaviour).
 */
export function resolveProjectType({ appRoot, config = {}, target = {}, exists = existsSync } = {}) {
  const explicit = target.projectType ?? config.projectType;
  if (explicit !== undefined) {
    if (!PROJECT_TYPES.includes(explicit)) throw Object.assign(new Error(`projectType must be one of ${PROJECT_TYPES.join(", ")}; got ${explicit}`), { code: "PROJECT_TYPE_INVALID" });
    return explicit;
  }
  if (appRoot) {
    const roots = [appRoot, path.resolve(appRoot, config.workdir ?? ".")];
    const hasSwift = roots.some((r) => exists(path.join(r, "Package.swift")));
    const hasCargo = roots.some((r) => exists(path.join(r, "Cargo.toml")) || exists(path.join(r, "src-tauri", "Cargo.toml")));
    if (hasSwift && !hasCargo) return "swiftpm";
  }
  return "tauri";
}

export const isSwiftProject = (projectType) => projectType === "swiftpm";

/** Executables the build itself needs (the config's own preflight.executables are appended by callers). */
export function requiredBuildExecutables({ projectType, packageManager = "pnpm", usesPackageManager = true, usesCargo = true } = {}) {
  // A native iOS app with no Cargo host: git, plus the package manager only when the app has a package.json (xcodebuild/xcodegen are the recipe's own preflight).
  if (!usesCargo && !isSwiftProject(projectType)) return ["git", ...(usesPackageManager && packageManager ? [packageManager] : [])];
  if (isSwiftProject(projectType)) return ["git", "swift", ...(usesPackageManager && packageManager ? [packageManager] : [])];
  return ["git", packageManager, "cargo", "rustc"];
}

/** True when the Windows target packages an MSIX (needs makeappx and signtool). */
export function targetUsesMsix(target) {
  return Boolean(target?.msix) || target?.signingContract === "windows-msix-signed-v1";
}

/** SwiftPM targets build and package inside prePackage (mac DMG) or the MSIX step; no separate `package` command is required. */
export const packageCommandOptional = (target, projectType) => targetUsesMsix(target) || Boolean(target?.winPackage || target?.macPackage) || (projectType === "swiftpm" && Boolean(target?.prePackage?.cmd));
