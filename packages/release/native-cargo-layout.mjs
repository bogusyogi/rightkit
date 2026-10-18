import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

const DEFAULT_MANIFEST = path.join("src-tauri", "Cargo.toml");
const LOCK_SOURCES = new Set(["manifest", "workspace", "root"]);

/**
 * Resolve every path which is derived from a native Cargo manifest in one
 * place.  RightRelease historically assumed src-tauri; retaining that
 * default keeps existing applications source-compatible while allowing a
 * workspace package to opt into its real manifest.
 */
export function resolveNativeCargoLayout({
  appRoot,
  config,
  nativeAssembly = config?.nativeAssembly,
  exists = existsSync,
  read = (file) => readFileSync(file, "utf8"),
  requireManifest = false,
  requireLock = false,
  // The repository the app lives in. A manifest outside the app searches for its workspace only up to this directory.
  repoRoot,
} = {}) {
  if (!appRoot) throw new TypeError("native Cargo layout requires appRoot");
  const resolvedAppRoot = path.resolve(appRoot);
  const declaredManifest = nativeAssembly?.cargoManifest ?? nativeAssembly?.manifest;
  const manifestPath = resolveDeclaredPath(resolvedAppRoot, declaredManifest || DEFAULT_MANIFEST);
  if (requireManifest && !exists(manifestPath)) throw new Error(`native Cargo manifest missing: ${manifestPath}`);

  const manifestText = readIfPresent(manifestPath, exists, read);
  const manifestDir = path.dirname(manifestPath);
  const parsed = parseCargoManifest(manifestText);
  const explicitWorkspace = nativeAssembly?.workspaceRoot ?? nativeAssembly?.cargoWorkspaceRoot;
  const workspaceRoot = explicitWorkspace
    ? normalizeWorkspaceRoot(resolveDeclaredPath(resolvedAppRoot, explicitWorkspace), exists)
    : discoverWorkspaceRoot({ appRoot: resolvedAppRoot, manifestPath, manifestDir, manifestText, exists, read, repoRoot: repoRoot ? path.resolve(repoRoot) : undefined });

  const lock = resolveCargoLock({
    appRoot: resolvedAppRoot,
    manifestPath,
    manifestDir,
    workspaceRoot,
    nativeAssembly,
    exists,
  });
  if (requireLock && !lock.path) {
    throw new Error(`native Cargo.lock missing (checked ${lock.candidates.join(", ")})`);
  }

  const targetDeclaration = nativeAssembly?.targetDir
    ?? nativeAssembly?.targetRoot
    ?? nativeAssembly?.cargoTargetDir;
  const targetLink = resolveDeclaredPath(resolvedAppRoot, targetDeclaration || path.join(relativeFromRoot(resolvedAppRoot, workspaceRoot), "target"));
  const targetPrefix = relativePrefix(resolvedAppRoot, targetLink);
  const manifestPrefix = relativePrefix(resolvedAppRoot, manifestDir);
  const workspacePrefix = relativePrefix(resolvedAppRoot, workspaceRoot);
  const targetPrefixes = [...new Set([
    targetPrefix,
    manifestPrefix ? `${manifestPrefix}target/` : "",
    workspacePrefix ? `${workspacePrefix}target/` : "",
  ].filter(Boolean))].sort((left, right) => right.length - left.length);
  const layout = {
    appRoot: resolvedAppRoot,
    manifestPath,
    cargoManifest: manifestPath,
    manifestDir,
    manifestPrefix,
    workspaceRoot,
    cargoWorkspaceRoot: workspaceRoot,
    workspacePrefix,
    lockPath: lock.path,
    cargoLock: lock.path,
    lockSource: lock.source,
    lockCandidates: lock.candidates,
    targetRoot: targetLink,
    targetDir: targetLink,
    targetLink,
    targetPrefix,
    targetPrefixes,
    targetRootPrefix: targetPrefix,
    native: Boolean(nativeAssembly),
    fallback: !declaredManifest,
    manifest: parsed,
  };
  layout.resolveArtifactPath = (file) => resolveNativeArtifactPath(layout, file);
  return layout;
}

/**
 * True when the app has a native Cargo manifest the release must hash and build against: the config declares one
 * (nativeAssembly.cargoManifest / manifest) or the historical src-tauri/Cargo.toml exists. A native iOS app (xcodegen
 * project, Rust static library built by iosPackage.prepare) has neither, and then owns no Cargo layout at all.
 */
export function nativeCargoDeclared({ appRoot, config, nativeAssembly = config?.nativeAssembly, exists = existsSync } = {}) {
  if (!appRoot) throw new TypeError("native Cargo layout requires appRoot");
  if (nativeAssembly?.cargoManifest ?? nativeAssembly?.manifest) return true;
  return exists(path.resolve(appRoot, DEFAULT_MANIFEST));
}

/** The manifest nativeAssembly names, if any (a path relative to the app root, or absolute). */
export function declaredCargoManifest(config, nativeAssembly = config?.nativeAssembly) {
  return nativeAssembly?.cargoManifest ?? nativeAssembly?.manifest;
}

/** True when the historical Tauri host manifest exists. */
export function hasDefaultTauriManifest({ appRoot, exists = existsSync } = {}) {
  if (!appRoot) throw new TypeError("native Cargo layout requires appRoot");
  return exists(path.resolve(appRoot, DEFAULT_MANIFEST));
}

export const resolveCargoLayout = resolveNativeCargoLayout;
export const resolveNativeCargoManifest = resolveNativeCargoLayout;
export const resolveCargoManifest = resolveNativeCargoLayout;

export function resolveNativeArtifactPath(layout, file) {
  if (!layout?.appRoot) throw new TypeError("native artifact resolution requires a Cargo layout");
  if (!file) throw new TypeError("native artifact path is required");
  const value = String(file).replaceAll("\\", "/");
  const targetPrefixes = layout.targetPrefixes ?? [layout.targetPrefix];
  for (const prefixValue of targetPrefixes) {
    const targetPrefix = stripTrailingSlash(prefixValue || "");
    if (targetPrefix && (value === targetPrefix || value.startsWith(`${targetPrefix}/`))) {
      return path.join(layout.targetRoot, value.slice(targetPrefix.length).replace(/^\/+/, ""));
    }
  }
  return path.resolve(layout.appRoot, file);
}

export function parseCargoManifest(text = "") {
  const cleaned = String(text)
    .replace(/\/\/.*$/gm, "")
    .replace(/#.*$/gm, "");
  const workspaceSection = section(cleaned, "workspace");
  const packageSection = section(cleaned, "package");
  return {
    hasWorkspace: workspaceSection != null,
    workspacePath: stringValue(workspaceSection?.workspace),
    packageWorkspace: stringValue(packageSection?.workspace),
    packageName: stringValue(packageSection?.name),
  };
}

export function findCargoLock({ manifestPath, appRoot, workspaceRoot, nativeAssembly, exists = existsSync } = {}) {
  const resolvedAppRoot = path.resolve(appRoot || path.dirname(manifestPath || DEFAULT_MANIFEST));
  const resolvedManifest = path.resolve(manifestPath || path.join(resolvedAppRoot, DEFAULT_MANIFEST));
  const manifestDir = path.dirname(resolvedManifest);
  return resolveCargoLock({
    appRoot: resolvedAppRoot,
    manifestPath: resolvedManifest,
    manifestDir,
    workspaceRoot: path.resolve(workspaceRoot || manifestDir),
    nativeAssembly,
    exists,
  });
}

export const resolveCargoLockPath = findCargoLock;

function resolveCargoLock({ appRoot, manifestPath, manifestDir, workspaceRoot, nativeAssembly, exists }) {
  const declaredPath = nativeAssembly?.cargoLockPath
    ?? nativeAssembly?.cargoLockFile
    ?? nativeAssembly?.lockfilePath
    ?? nativeAssembly?.lockPath;
  const declared = nativeAssembly?.cargoLock ?? nativeAssembly?.lockfile ?? nativeAssembly?.lockSource;
  const sourceDeclaration = nativeAssembly?.cargoLockSource ?? nativeAssembly?.lockSource;
  const source = normalizeLockSource(sourceDeclaration ?? (typeof declared === "string" && LOCK_SOURCES.has(declared) ? declared : undefined));
  if (declaredPath || (declared && !LOCK_SOURCES.has(String(declared)))) {
    const explicit = declaredPath || declared;
    const explicitPath = resolveDeclaredPath(appRoot, explicit);
    return { path: exists(explicitPath) ? explicitPath : undefined, source: "explicit", candidates: [explicitPath] };
  }
  const roots = {
    manifest: manifestDir,
    workspace: workspaceRoot,
    root: appRoot,
  };
  const orderedSources = source ? [source] : [workspaceRoot !== manifestDir ? "workspace" : "manifest", "manifest", "root"];
  const candidates = [];
  for (const candidateSource of orderedSources) {
    const candidate = path.join(roots[candidateSource], "Cargo.lock");
    if (!candidates.includes(candidate)) candidates.push(candidate);
  }
  const found = candidates.find((candidate) => exists(candidate));
  return { path: found, source: found ? orderedSources[candidates.indexOf(found)] : undefined, candidates };
}

function discoverWorkspaceRoot({ appRoot, manifestPath, manifestDir, manifestText, exists, read, repoRoot }) {
  const parsed = parseCargoManifest(manifestText);
  const declaredWorkspace = parsed.workspacePath ?? parsed.packageWorkspace;
  if (declaredWorkspace) {
    return normalizeWorkspaceRoot(resolveDeclaredPath(manifestDir, declaredWorkspace), exists);
  }
  if (parsed.hasWorkspace) return manifestDir;
  // A manifest inside the app searches up to the app root (unchanged); one outside it (a shared crates/ tree) searches up to the
  // nearest directory that contains both, which is where its workspace and Cargo.lock live.
  // Only a caller that names the repository opts in to the climb above the app (native iOS); every other caller keeps the
  // exact historical search, which never leaves the app root.
  let boundary = appRoot;
  if (repoRoot && !isWithin(appRoot, manifestDir)) {
    boundary = commonAncestor(appRoot, manifestDir);
    if (!isWithin(repoRoot, boundary)) boundary = repoRoot;
  }
  let current = manifestDir;
  for (;;) {
    const candidate = path.join(current, "Cargo.toml");
    if (candidate !== manifestPath && exists(candidate) && parseCargoManifest(readIfPresent(candidate, exists, read)).hasWorkspace) return current;
    if (current === boundary) break;
    const parent = path.dirname(current);
    if (parent === current || !isWithin(boundary, parent)) break;
    current = parent;
  }
  return manifestDir;
}

function normalizeWorkspaceRoot(candidate, exists) {
  const resolved = path.resolve(candidate);
  if (path.basename(resolved).toLowerCase() === "cargo.toml") return path.dirname(resolved);
  if (exists(resolved) && !path.extname(resolved)) return resolved;
  if (exists(path.join(resolved, "Cargo.toml"))) return resolved;
  return resolved;
}

function section(text, name) {
  const expression = new RegExp(`(?:^|\\n)\\s*\\[${escapeRegExp(name)}\\]\\s*\\n([\\s\\S]*?)(?=\\n\\s*\\[|$)`, "m");
  const match = text.match(expression);
  if (!match) return undefined;
  const values = {};
  for (const line of match[1].split("\n")) {
    const assignment = line.match(/^\s*([A-Za-z0-9_-]+)\s*=\s*(.*?)\s*$/);
    if (assignment) values[assignment[1]] = assignment[2];
  }
  return values;
}

function stringValue(value) {
  const match = String(value ?? "").match(/^['\"]([^'\"]+)['\"]$/);
  return match?.[1];
}

function normalizeLockSource(value) {
  if (value == null) return undefined;
  const normalized = String(value).toLowerCase();
  return LOCK_SOURCES.has(normalized) ? normalized : undefined;
}

function resolveDeclaredPath(root, value) {
  if (path.isAbsolute(String(value))) return path.normalize(String(value));
  return path.resolve(root, String(value));
}

function relativeFromRoot(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative || ".";
}

function relativePrefix(root, candidate) {
  const relative = path.relative(root, candidate).replaceAll("\\", "/");
  return relative && relative !== "." ? `${relative.replace(/\/+$/, "")}/` : "";
}

function stripTrailingSlash(value) { return String(value).replace(/\/+$/, ""); }

function readIfPresent(file, exists, read) {
  try { return exists(file) ? read(file) : ""; } catch { return ""; }
}

function commonAncestor(left, right) {
  let current = path.resolve(left);
  while (!isWithin(current, path.resolve(right))) {
    const parent = path.dirname(current);
    if (parent === current) return current;
    current = parent;
  }
  return current;
}

function isWithin(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === "" || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

function escapeRegExp(value) { return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&"); }
