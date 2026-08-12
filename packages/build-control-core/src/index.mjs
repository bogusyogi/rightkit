import { createHash } from 'node:crypto';
import { lstat as defaultLstat, readdir as defaultReaddir } from 'node:fs/promises';
import { basename, isAbsolute, join, normalize, relative } from 'node:path';

export const FRONTENDS = Object.freeze(['cargo', 'swiftpm', 'node-pnpm', 'node-gyp', 'cmake', 'make', 'raw-compiler']);
export const PROFILES = Object.freeze({ MANAGED_AGENT: 'MANAGED_AGENT', STRICT_SYSTEM: 'STRICT_SYSTEM' });
export const LOCAL_OUTPUT_SCAN_POLICY = Object.freeze({
  forbiddenNames: Object.freeze(['target', '.cargo-target', '.build', 'build', 'build-output', 'CMakeFiles', '.pnpm-store', '.rightkit-build', 'DerivedData']),
  skippedDirectoryNames: Object.freeze(['.git', 'node_modules', 'vendor', 'third_party', 'deps', 'Pods']),
  reason: 'managed-agent payloads must use deterministic external roots'
});
const digest = /^[a-f0-9]{64}$/;
const managedToolNames = new Set(['cargo', 'rustup', 'rustc', 'rustdoc', 'swift', 'cmake', 'make', 'cc', 'clang', 'gcc', 'node', 'node-gyp', 'pnpm', 'npm']);
const redirectEnvironment = /^(?:CARGO_(?:HOME|TARGET_DIR|BUILD_TARGET_DIR|BUILD_BUILD_DIR|BUILD_RUSTC|BUILD_RUSTDOC|BUILD_RUSTC_WRAPPER|BUILD_RUSTC_WORKSPACE_WRAPPER|BUILD_RUSTFLAGS|ENCODED_RUSTFLAGS|INSTALL_ROOT)|RUSTUP_HOME|RUSTC|RUSTDOC|RUSTFLAGS|RUSTDOCFLAGS|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|TMPDIR|TMP|TEMP|TEMP_DIR|PNPM_HOME|NPM_CONFIG_(?:CACHE|STORE_DIR|STATE_DIR|VIRTUAL_STORE_DIR|PREFIX|NODE_GYP|TMP|USERCONFIG|GLOBALCONFIG|NODEDIR|DEV_DIR|PYTHON)|NODE_GYP|NODE_PATH|CC|CXX|CMAKE_(?:BUILD|INSTALL|PREFIX)_?PATH|SWIFTPM_(?:CACHE|BUILD|MODULECACHE)_PATH)$/;
export const MANAGED_RUST_ENVIRONMENT_KEYS = Object.freeze([
  'CARGO_HOME', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR',
  'CARGO_BUILD_RUSTC', 'CARGO_BUILD_RUSTDOC', 'CARGO_BUILD_RUSTC_WRAPPER',
  'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTFLAGS',
  'CARGO_ENCODED_RUSTFLAGS', 'CARGO_INSTALL_ROOT', 'RUSTUP_HOME', 'RUSTC', 'RUSTDOC',
  'RUSTFLAGS', 'RUSTDOCFLAGS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'
]);
const redirectArgument = /^(?:--(?:target-dir|config|cache-dir|build-dir|toolchain|temp-dir)(?:=|$)|-Z(?:build-std|config)(?:=|$))/;
const redirectConfig = /(?:^|[._-])(?:target(?:[_-]?dir)?|build(?:[_-]?dir)?|cache(?:[_-]?dir)?|toolchain|rustc|rustdoc|temp(?:[_-]?dir)?|home|cargo_(?:target_dir|build_build_dir|install_root))$/i;

function requireProfile(profile) {
  if (profile !== undefined && !Object.values(PROFILES).includes(profile)) throw new TypeError('recognized profile is required');
}

function isUnmanagedToolPath(argument, managedToolRoot) {
  if (!isAbsolute(argument) || !managedToolNames.has(basename(argument))) return false;
  return !managedToolRoot || relative(normalize(managedToolRoot), normalize(argument)).startsWith('..');
}

function configurationKeys(configuration, prefix = '') {
  return Object.entries(configuration).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    return value && typeof value === 'object' && !Array.isArray(value) ? [path, ...configurationKeys(value, path)] : [path];
  });
}

function rejectManagedAgentRedirects(request) {
  const { argv, environment = {}, configuration = {}, managedToolRoot } = request;
  if (managedToolRoot !== undefined && !isAbsolute(managedToolRoot)) throw new TypeError('managed tool root must be absolute');
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === '--') break;
    if (redirectArgument.test(argv[index]) || argv[index].startsWith('+') || isUnmanagedToolPath(argv[index], managedToolRoot)) throw new TypeError('managed agent request cannot redirect cache, build, toolchain, temp, or tool path');
  }
  if (Object.keys(environment).some((key) => redirectEnvironment.test(key.toUpperCase()))) throw new TypeError('managed agent request cannot set redirecting environment');
  if (!configuration || typeof configuration !== 'object' || Array.isArray(configuration)) throw new TypeError('configuration must be an object');
  if (configurationKeys(configuration).some((key) => redirectConfig.test(key))) throw new TypeError('managed agent request cannot set redirecting configuration');
}

export function normalizeBuildRequest(request) {
  if (!request || typeof request !== 'object') throw new TypeError('build request must be an object');
  if (!FRONTENDS.includes(request.frontend)) throw new TypeError('registered frontend is required');
  if (!Array.isArray(request.argv) || request.argv.some((value) => typeof value !== 'string' || !value)) throw new TypeError('argv must contain non-empty strings');
  if (!isAbsolute(request.workspace)) throw new TypeError('workspace must be absolute');
  if (request.environment && Object.values(request.environment).some((value) => typeof value !== 'string')) throw new TypeError('environment values must be strings');
  requireProfile(request.profile);
  if (request.profile === PROFILES.MANAGED_AGENT) rejectManagedAgentRedirects(request);
  const normalized = { frontend: request.frontend, argv: Object.freeze([...request.argv]), workspace: normalize(request.workspace), environment: Object.freeze({ ...(request.environment ?? {}) }) };
  if (request.profile !== undefined) normalized.profile = request.profile;
  if (request.profile === PROFILES.MANAGED_AGENT) {
    normalized.configuration = Object.freeze({ ...(request.configuration ?? {}) });
    if (request.managedToolRoot) normalized.managedToolRoot = normalize(request.managedToolRoot);
  }
  return Object.freeze(normalized);
}

export function normalizeManagedAgentRequest(request) {
  return normalizeBuildRequest({ ...request, profile: PROFILES.MANAGED_AGENT });
}

export function captureManagedRustEnvironment(environment = {}) {
  if (!environment || typeof environment !== 'object' || Array.isArray(environment)) throw new TypeError('environment must be an object');
  const controlled = new Set(MANAGED_RUST_ENVIRONMENT_KEYS);
  return Object.freeze(Object.fromEntries(Object.entries(environment).filter(([key, value]) => controlled.has(key.toUpperCase()) && value !== undefined).map(([key, value]) => {
    if (typeof value !== 'string') throw new TypeError('environment values must be strings');
    return [key, value];
  })));
}

export function validateManagedRustEnvironment(observed = {}, allowed = {}) {
  const captured = captureManagedRustEnvironment(observed);
  for (const [key, value] of Object.entries(captured)) {
    const expected = Object.entries(allowed).find(([candidate]) => candidate.toUpperCase() === key.toUpperCase())?.[1];
    if (expected === undefined || normalize(String(expected)) !== normalize(value)) throw new TypeError(`managed agent request has conflicting ${key}`);
  }
  return captured;
}

export function externalRoots({ externalRoot, repositoryIdentity, toolchain, target, buildProfile = 'development', policyProfile = PROFILES.MANAGED_AGENT }) {
  if (!isAbsolute(externalRoot)) throw new TypeError('external root must be absolute');
  if (![repositoryIdentity, toolchain, target, buildProfile].every((value) => typeof value === 'string' && value.length > 0)) throw new TypeError('repository identity, toolchain, target, & build profile are required');
  requireProfile(policyProfile);
  const base = join(normalize(externalRoot), 'rightkit-build-control');
  const namespace = createHash('sha256').update(JSON.stringify([repositoryIdentity, toolchain, target, buildProfile])).digest('hex');
  const globalRoot = join(base, 'global'); const root = join(base, 'workspaces', namespace);
  const contained = (path) => {
    if (relative(base, path).startsWith('..')) throw new TypeError('external roots must remain contained');
    return path;
  };
  return Object.freeze({
    namespace, policyProfile, buildProfile, root: contained(root),
    cargoHome: contained(join(globalRoot, 'cargo-home')), compilerCache: contained(join(globalRoot, 'compiler-cache')),
    targets: contained(join(root, 'targets')), build: contained(join(root, 'build')), temp: contained(join(base, 'tmp', namespace.slice(0, 16)))
  });
}

export function scanForbiddenLocalOutputs(names) {
  if (!Array.isArray(names) || names.some((name) => typeof name !== 'string')) throw new TypeError('output names must be strings');
  return Object.freeze(names.filter((name) => LOCAL_OUTPUT_SCAN_POLICY.forbiddenNames.includes(name) || /^cmake-build-[^/]+$/i.test(name)));
}

function normalizedAllowlist(workspace, allowedRelativePaths) {
  if (!Array.isArray(allowedRelativePaths) || allowedRelativePaths.some((path) => typeof path !== 'string' || !path)) throw new TypeError('allowed relative paths must be non-empty strings');
  return allowedRelativePaths.map((path) => {
    if (isAbsolute(path)) throw new TypeError('allowed relative paths must not be absolute');
    const candidate = normalize(path);
    if (candidate === '.' || candidate === '..' || candidate.startsWith('../') || candidate.startsWith('..\\') || relative(workspace, join(workspace, candidate)).startsWith('..')) throw new TypeError('allowed relative path escapes workspace');
    return candidate;
  });
}

function isAllowed(relativePath, allowed) {
  return allowed.some((path) => relativePath === path || relativePath.startsWith(`${path}/`));
}

function isForbiddenOutput(relativePath) {
  const segments = relativePath.split('/'); const name = segments.at(-1);
  if (LOCAL_OUTPUT_SCAN_POLICY.forbiddenNames.includes(name) || /^cmake-build-[^/]+$/i.test(name)) return true;
  if (segments.length >= 2 && segments.at(-2) === '.cache' && /target/i.test(name)) return true;
  return segments.length >= 2 && segments.at(-2) === '.right-release' && name === 'cache';
}

export async function scanWorkspaceForUnmanagedOutputReceipt(workspace, { readdir = defaultReaddir, lstat = defaultLstat, maxEntries = 10_000, maxDepth = 32, timeoutMs = 1_000, allowedRelativePaths = [], now = Date.now } = {}) {
  if (!isAbsolute(workspace) || !Number.isInteger(maxEntries) || maxEntries < 1 || !Number.isInteger(maxDepth) || maxDepth < 0 || !Number.isFinite(timeoutMs) || timeoutMs <= 0 || typeof now !== 'function') throw new TypeError('absolute workspace & positive entry, depth, and time caps are required');
  const root = normalize(workspace); const startedAt = now();
  const assertWithinLimits = () => {
    if (now() - startedAt > timeoutMs) throw new RangeError('workspace output scan time cap exceeded');
  };
  const withinDeadline = async (operation) => {
    assertWithinLimits();
    const remaining = timeoutMs - (now() - startedAt);
    let timer;
    try {
      return await Promise.race([operation(), new Promise((_, reject) => { timer = setTimeout(() => reject(new RangeError('workspace output scan time cap exceeded')), remaining); })]);
    } finally { clearTimeout(timer); }
  };
  const rootMetadata = await withinDeadline(() => lstat(root));
  if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) throw new TypeError('workspace must be a real directory');
  const allowed = normalizedAllowlist(root, allowedRelativePaths); const findings = []; let entries = 0;
  async function scan(directory, parentRelative = '', depth = 0) {
    assertWithinLimits();
    const names = await withinDeadline(() => readdir(directory));
    for (const name of names) {
      assertWithinLimits();
      if (LOCAL_OUTPUT_SCAN_POLICY.skippedDirectoryNames.includes(name)) continue;
      entries += 1;
      if (entries > maxEntries) throw new RangeError('workspace output scan entry cap exceeded');
      const path = join(directory, name); const relativePath = parentRelative ? `${parentRelative}/${name}` : name;
      const metadata = await withinDeadline(() => lstat(path));
      if (metadata.isSymbolicLink()) continue;
      const forbidden = metadata.isDirectory() && isForbiddenOutput(relativePath);
      if (forbidden && !isAllowed(relativePath, allowed)) findings.push(Object.freeze({ relativePath, path, reason: LOCAL_OUTPUT_SCAN_POLICY.reason }));
      if (metadata.isDirectory() && !forbidden) {
        if (depth >= maxDepth) throw new RangeError('workspace output scan depth cap exceeded');
        await scan(path, relativePath, depth + 1);
      }
    }
  }
  await scan(root);
  return Object.freeze({ workspace: root, entriesScanned: entries, maxEntries, maxDepth, timeoutMs, skippedDirectoryNames: LOCAL_OUTPUT_SCAN_POLICY.skippedDirectoryNames, findings: Object.freeze(findings.sort((left, right) => left.relativePath.localeCompare(right.relativePath))) });
}

export async function scanWorkspaceForUnmanagedOutputs(workspace, options = {}) {
  const receipt = await scanWorkspaceForUnmanagedOutputReceipt(workspace, options);
  return Object.freeze(receipt.findings.map(({ relativePath }) => relativePath));
}

export function immutableToolRoot(tool) {
  if (!tool || typeof tool !== 'object' || !isAbsolute(tool.path) || !digest.test(tool.sha256 ?? '')) throw new TypeError('immutable tool requires absolute path & sha256');
  if (tool.mutable === true || relative(tool.root ?? tool.path, tool.path).startsWith('..')) throw new TypeError('tool must remain inside immutable root');
  return Object.freeze({ path: normalize(tool.path), root: normalize(tool.root ?? tool.path), sha256: tool.sha256, version: tool.version ?? null });
}
