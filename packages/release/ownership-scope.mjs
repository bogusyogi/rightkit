// Ownership scan scope for a target that builds from only part of its repository (iOS).
// A scope is a sorted, de-duplicated list of repository-relative directory roots. Only committed files under those roots
// are inventoried; the exceptions file under `.rightkit` is always read. Sealed ownership evidence records the scope and
// upload-time verification re-derives it from the config, so the scope cannot be narrowed after the build.
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";

export const OWNERSHIP_SCOPE_ALWAYS = ".rightkit";

/** Normalize and validate repository-relative scope roots; throws a clear error for absolute, escaping, or missing roots. */
export function normalizeScopeRoots(roots, { repoRoot, label = "ownershipScope" } = {}) {
  if (!Array.isArray(roots)) throw new Error(`${label} must be an array of repository-relative directories`);
  const out = new Set();
  for (const raw of roots) {
    if (typeof raw !== "string" || !raw.trim()) throw new Error(`${label} entries must be non-empty strings`);
    const slashed = raw.replaceAll("\\", "/");
    if (path.posix.isAbsolute(slashed) || path.win32.isAbsolute(raw) || /^[A-Za-z]:/.test(slashed)) throw new Error(`${label} entry ${raw} must be repository-relative, not absolute`);
    const normal = path.posix.normalize(slashed).replace(/\/+$/, "");
    if (normal === ".." || normal.startsWith("../")) throw new Error(`${label} entry ${raw} escapes the repository`);
    if (normal === "." || normal === "") throw new Error(`${label} entry ${raw} is the whole repository; omit the scope instead`);
    if (repoRoot) {
      const absolute = path.join(repoRoot, ...normal.split("/"));
      let directory = false;
      try { directory = existsSync(absolute) && statSync(absolute).isDirectory(); } catch { directory = false; }
      if (!directory) throw new Error(`${label} entry ${raw} is not an existing directory in the repository`);
    }
    out.add(normal);
  }
  return [...out];
}

/** Local SwiftPM package directories named by `path:` inside the `packages:` section of an XcodeGen project.yml. */
export function projectYmlLocalPackagePaths(text) {
  const lines = String(text).split(/\r?\n/);
  const paths = [];
  let inPackages = false;
  for (const line of lines) {
    if (/^\s*(#.*)?$/.test(line)) continue;
    if (/^\S/.test(line)) { inPackages = /^packages:\s*(#.*)?$/.test(line); continue; }
    if (!inPackages) continue;
    const match = /(?:^|[\s{,])path:\s*(?:"([^"]*)"|'([^']*)'|([^\s,}#]+))/.exec(line);
    if (match) paths.push(match[1] ?? match[2] ?? match[3]);
  }
  return paths;
}

/**
 * Scan scope of an iOS target: the Xcode project directory, each local SwiftPM package its project.yml references,
 * `.rightkit` (exceptions), the native Rust assembly (`nativeAssembly.workspaceRoot` and the directory of its cargo manifest,
 * which is compiled into the IPA), and `targets.ios.ownershipScope` additions. Accepts the raw or the expanded target.
 * The scan itself closes this scope under local path dependencies of the in-scope manifests (see ownership.mjs).
 * A native assembly rooted at the repository root is ignored (it would make the scope the whole repository).
 * Returns null when the target has no iosPackage (nothing to scope from).
 */
export function iosOwnershipScope({ repoRoot, appRoot, target, nativeAssembly }) {
  const spec = target?.recipe?.kind === "iosPackage" ? target.recipe.spec : target?.iosPackage;
  if (!spec || typeof spec !== "object") return null;
  const toRepo = (absolute) => {
    const relative = path.relative(repoRoot, absolute).split(path.sep).join("/");
    if (relative === ".." || relative.startsWith("../") || path.isAbsolute(relative)) throw new Error(`ios ownership scope: ${absolute} is outside the repository`);
    return relative || ".";
  };
  const specFile = spec.projectSpec ?? spec.project;
  if (typeof specFile !== "string") throw new Error("ios ownership scope: iosPackage.project or projectSpec is required");
  const specDir = path.resolve(appRoot, path.posix.dirname(specFile.replaceAll("\\", "/")));
  const roots = [];
  const projectDir = toRepo(specDir);
  if (projectDir === ".") throw new Error("ios ownership scope: the Xcode project sits at the repository root, so there is no narrower scope; declare the project in a subdirectory");
  roots.push(projectDir);
  if (spec.projectSpec) {
    const ymlPath = path.resolve(appRoot, spec.projectSpec);
    if (existsSync(ymlPath)) {
      for (const local of projectYmlLocalPackagePaths(readFileSync(ymlPath, "utf8"))) roots.push(toRepo(path.resolve(specDir, local)));
    }
  }
  const native = nativeAssembly && typeof nativeAssembly === "object" ? nativeAssembly : null;
  const nativeDirs = [native?.workspaceRoot ?? native?.cargoWorkspaceRoot, (native?.cargoManifest ?? native?.manifest) ? path.dirname(native.cargoManifest ?? native.manifest) : undefined].filter((value) => typeof value === "string" && value);
  for (const dir of nativeDirs) {
    const nativeRoot = toRepo(path.resolve(appRoot, dir));
    // A native assembly at the repository root is the app's own (usually desktop) workspace; folding it in would widen the
    // scope to the whole repository, so it adds nothing. The scan's path-dependency closure still follows the in-scope manifests.
    if (nativeRoot !== ".") roots.push(nativeRoot);
  }
  const extra = target.ownershipScope;
  if (extra !== undefined) roots.push(...normalizeScopeRoots(extra, { repoRoot, label: "targets.ios.ownershipScope" }));
  const checked = normalizeScopeRoots(roots.filter((root) => root !== OWNERSHIP_SCOPE_ALWAYS), { repoRoot, label: "ios ownership scope" });
  return [...new Set([...checked, OWNERSHIP_SCOPE_ALWAYS])].sort();
}

/**
 * The one place the doctor (`releasePreflight`) and the build (`buildPreflight`) assemble the ownership scan scope, so the
 * two can never disagree about what ships (rightkit#927). iOS builds from its Xcode project and local packages only; every
 * other platform scans the whole repository (null).
 */
export function preflightOwnershipScope({ repoRoot, appRoot, platform, target, nativeAssembly }) {
  return platform === "ios" ? iosOwnershipScope({ repoRoot, appRoot, target, nativeAssembly }) : null;
}

/** Repository-relative file or directory under any scope root (null scope = everything). */
export function inOwnershipScope(scope, rel) {
  if (!scope) return true;
  return scope.some((root) => rel === root || rel.startsWith(`${root}/`));
}
