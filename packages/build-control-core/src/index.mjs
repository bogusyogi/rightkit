import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { lstat as defaultLstat, readdir as defaultReaddir } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, normalize, parse, relative, resolve, sep } from 'node:path';

export const FRONTENDS = Object.freeze(['cargo', 'swiftpm', 'node-pnpm', 'node-gyp', 'cmake', 'make', 'raw-compiler']);
export const BUILD_CONTROL_PROTOCOL = 'rightkit.build-control.client.v2';
export const RUNTIME_MANIFEST_SCHEMA = 'rightkit.build-control.runtime.v2';
export const SOURCE_RUNTIME_IDENTITY = Object.freeze({
  schema: RUNTIME_MANIFEST_SCHEMA,
  version: 'source',
  clientDigest: createHash('sha256').update('rightkit-build-control-clients-source-v2').digest('hex'),
  brokerDigest: createHash('sha256').update('rightkit-build-control-broker-source-v2').digest('hex')
});
export const PROFILES = Object.freeze({ MANAGED_AGENT: 'MANAGED_AGENT', STRICT_SYSTEM: 'STRICT_SYSTEM' });
export const GITHUB_ACTIONS_ONLY = 'github-actions-only';
export const GITHUB_ACTIONS_ONLY_ERROR = 'RK_GITHUB_ACTIONS_ONLY';
export const LOCAL_OUTPUT_SCAN_POLICY = Object.freeze({
  forbiddenNames: Object.freeze(['target', '.cargo-target', '.build', 'build', 'build-output', 'CMakeFiles', '.pnpm-store', '.rightkit-build', 'DerivedData']),
  skippedDirectoryNames: Object.freeze(['.git', 'node_modules', 'vendor', 'third_party', 'deps', 'Pods']),
  reason: 'managed-agent payloads must use deterministic external roots'
});
const digest = /^[a-f0-9]{64}$/;
const managedToolNames = new Set(['cargo', 'rustup', 'rustc', 'rustdoc', 'swift', 'cmake', 'make', 'cc', 'clang', 'gcc', 'node', 'node-gyp', 'pnpm', 'npm']);
const redirectEnvironment = /^(?:CARGO_(?:HOME|TARGET_DIR|BUILD_TARGET_DIR|BUILD_BUILD_DIR|BUILD_RUSTC|BUILD_RUSTDOC|BUILD_RUSTC_WRAPPER|BUILD_RUSTC_WORKSPACE_WRAPPER|BUILD_RUSTFLAGS|ENCODED_RUSTFLAGS|INSTALL_ROOT)|RUSTUP_HOME|RUSTC|RUSTDOC|RUSTFLAGS|RUSTDOCFLAGS|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|TMPDIR|TMP|TEMP|TEMP_DIR|PNPM_HOME|NPM_CONFIG_(?:CACHE|STORE_DIR|STATE_DIR|VIRTUAL_STORE_DIR|PREFIX|NODE_GYP|TMP|USERCONFIG|GLOBALCONFIG|NODEDIR|DEV_DIR|PYTHON)|NODE_GYP|NODE_PATH|CC|CXX|CMAKE_(?:BUILD|INSTALL|PREFIX)_?PATH|SWIFTPM_(?:CACHE|BUILD|MODULECACHE)_PATH)$/;
export const MANAGED_RUST_ENVIRONMENT_KEYS = Object.freeze([
  'CARGO_HOME', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR',
  'CARGO_BUILD_RUSTC', 'CARGO_BUILD_RUSTDOC', 'CARGO_BUILD_RUSTC_WRAPPER',
  'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTFLAGS',
  'CARGO_ENCODED_RUSTFLAGS', 'CARGO_INSTALL_ROOT', 'RUSTUP_HOME', 'RUSTC', 'RUSTDOC',
  'RUSTFLAGS', 'RUSTDOCFLAGS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'
]);
/**
 * Request-provided environment is deliberately tiny.  Build locations,
 * executable selection, dynamic-loader settings, & all other process state
 * belong to broker injection, never an untrusted client request.
 */
export const MANAGED_REQUEST_ENVIRONMENT_KEYS = Object.freeze([
  'CI', 'CARGO_TERM_COLOR', 'NO_COLOR', 'RUST_BACKTRACE', 'RUST_LOG',
  'SOURCE_DATE_EPOCH', 'TERM', 'TZ', '__CF_USER_TEXT_ENCODING'
]);
const managedRequestEnvironment = new Set(MANAGED_REQUEST_ENVIRONMENT_KEYS);
const delegatedCompilerAmbientEnvironment = /^__CF_USER_TEXT_ENCODING$/i;

function directoryFrom(value) {
  const selected = resolve(value ?? process.cwd());
  try { return statSync(selected).isDirectory() ? realpathSync(selected) : dirname(realpathSync(selected)); }
  catch { return selected; }
}

function gitRemoteIsOrthicPublic(root) {
  const config = join(root, '.git', 'config');
  if (!existsSync(config)) return false;
  try { return /github\.com(?::|\/)Orthic-Labs\//i.test(readFileSync(config, 'utf8')); }
  catch { return false; }
}

export function repositoryExecutionPolicy(workspace = process.cwd()) {
  let cursor = directoryFrom(workspace);
  const filesystemRoot = parse(cursor).root;
  for (;;) {
    const manifestPath = join(cursor, '.rightgit.json');
    if (existsSync(manifestPath)) {
      let manifest;
      try { manifest = JSON.parse(readFileSync(manifestPath, 'utf8')); }
      catch (error) {
        throw Object.assign(new Error(`invalid public repository execution policy: ${manifestPath}: ${error.message}`), { code: 'RK_EXECUTION_POLICY_INVALID' });
      }
      if (gitRemoteIsOrthicPublic(cursor) && manifest.compile !== GITHUB_ACTIONS_ONLY) {
        throw Object.assign(new Error(`Orthic Labs public repository requires compile=${GITHUB_ACTIONS_ONLY}: ${manifestPath}`), { code: 'RK_EXECUTION_POLICY_MISSING' });
      }
      return Object.freeze({ root: cursor, manifestPath, compile: manifest.compile ?? null, orthicPublic: gitRemoteIsOrthicPublic(cursor) });
    }
    if (gitRemoteIsOrthicPublic(cursor)) {
      throw Object.assign(new Error(`Orthic Labs public repository requires .rightgit.json with compile=${GITHUB_ACTIONS_ONLY}: ${cursor}`), { code: 'RK_EXECUTION_POLICY_MISSING' });
    }
    if (cursor === filesystemRoot) return Object.freeze({ root: null, manifestPath: null, compile: null, orthicPublic: false });
    cursor = dirname(cursor);
  }
}

export function assertRepositoryExecutionAllowed({ workspace = process.cwd(), environment = process.env, operation = 'build' } = {}) {
  const policy = repositoryExecutionPolicy(workspace);
  if (policy.compile === GITHUB_ACTIONS_ONLY && environment?.GITHUB_ACTIONS !== 'true') {
    throw Object.assign(new Error(`${operation} is GitHub Actions only for ${policy.root}; push changes & use managed CI`), { code: GITHUB_ACTIONS_ONLY_ERROR });
  }
  return policy;
}
const delegatedCompilerRuntimeEnvironment = /^(?:PATH|HOME|USER|LOGNAME|SHELL|PWD|OLDPWD|SHLVL|_|LANG|LC_.+|TMPDIR|TMP|TEMP|RIGHTKIT_.+|SCCACHE_.+|CARGO_MAKEFLAGS|MAKEFLAGS|MFLAGS)$/i;
const forbiddenRequestEnvironment = /^(?:DYLD_(?:INSERT_LIBRARIES|LIBRARY_PATH|FALLBACK_LIBRARY_PATH|FRAMEWORK_PATH|FALLBACK_FRAMEWORK_PATH)|LD_(?:PRELOAD|LIBRARY_PATH|AUDIT|DEBUG)|NODE_OPTIONS|RUSTC|RUSTC_(?:BOOTSTRAP|FORCE_UNSTABLE)|RUSTDOC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTFLAGS|RUSTDOCFLAGS|CARGO_(?:HOME|TARGET_DIR|BUILD_(?:TARGET|TARGET_DIR|BUILD_DIR|RUSTC|RUSTDOC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTFLAGS)|ENCODED_RUSTFLAGS|INSTALL_ROOT)|CARGO_TARGET_.+_(?:LINKER|RUNNER|RUSTFLAGS)|(?:CC|CXX|AR|AS|LD|RANLIB|STRIP|OBJC|OBJCXX|LINK|CL|RC|CMAKE_.+|SWIFTC|SDKROOT|DEVELOPER_DIR))$/i;
const redirectArgument = /^(?:--(?:target-dir|config|cache-dir|build-dir|toolchain|temp-dir)(?:=|$)|-Z(?:build-std|config)(?:=|$))/;
const redirectConfig = /(?:^|[._-])(?:target(?:[_-]?dir)?|build(?:[_-]?dir)?|cache(?:[_-]?dir)?|toolchain|rustc|rustdoc|temp(?:[_-]?dir)?|home|cargo_(?:target_dir|build_build_dir|install_root))$/i;

function pathEscapesRoot(root, candidate) {
  const scoped = relative(normalize(root), normalize(candidate));
  return isAbsolute(scoped) || scoped === '..' || scoped.startsWith(`..${sep}`);
}

function requireProfile(profile) {
  if (profile !== undefined && !Object.values(PROFILES).includes(profile)) throw new TypeError('recognized profile is required');
}

function isUnmanagedToolPath(argument, managedToolRoot) {
  const toolName = basename(argument).toLowerCase().replace(/\.exe$/, '');
  if (!isAbsolute(argument) || !managedToolNames.has(toolName)) return false;
  return !managedToolRoot || pathEscapesRoot(managedToolRoot, argument);
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

function environmentObject(environment, label) {
  if (!environment || typeof environment !== 'object' || Array.isArray(environment)) throw new TypeError(`${label} must be an object`);
  return environment;
}

function environmentValue(key, value, label) {
  if (typeof value !== 'string' || value.includes('\0')) throw new TypeError(`${label} ${key} must be a string without NUL`);
  return value;
}

/**
 * Validate environment supplied over broker protocol.  Unknown keys are an
 * authority failure, rather than ambient process state waiting to become one.
 */
export function normalizeRequestEnvironment(environment = {}) {
  const input = environmentObject(environment, 'request environment');
  const normalized = {};
  for (const [key, value] of Object.entries(input)) {
    if (typeof key !== 'string' || !key) throw new TypeError('request environment key is required');
    const canonical = key.toUpperCase();
    if (forbiddenRequestEnvironment.test(canonical)) throw new TypeError(`request environment cannot set broker-controlled ${canonical}`);
    if (!managedRequestEnvironment.has(canonical)) throw new TypeError(`request environment key is not allowlisted: ${key}`);
    if (Object.hasOwn(normalized, canonical)) throw new TypeError(`request environment has duplicate key: ${canonical}`);
    normalized[canonical] = environmentValue(canonical, value, 'request environment');
  }
  return Object.freeze(normalized);
}

export function captureDelegatedCompilerEnvironment(environment = {}) {
  const input = environmentObject(environment, 'compiler environment'); const normalized = {};
  for (const [key, value] of Object.entries(input)) {
    const canonical = key.toUpperCase();
    if (delegatedCompilerAmbientEnvironment.test(canonical) || delegatedCompilerRuntimeEnvironment.test(canonical) || redirectEnvironment.test(canonical) || forbiddenRequestEnvironment.test(canonical)) continue;
    if (Object.hasOwn(normalized, canonical)) throw new TypeError(`compiler environment has duplicate key: ${canonical}`);
    normalized[canonical] = environmentValue(canonical, value, 'compiler environment');
  }
  return Object.freeze(normalized);
}

export function normalizeDelegatedCompilerEnvironment(environment = {}) {
  const input = environmentObject(environment, 'compiler environment'); const normalized = {};
  for (const [key, value] of Object.entries(input)) {
    const canonical = key.toUpperCase();
    if (delegatedCompilerAmbientEnvironment.test(canonical)) continue;
    if (delegatedCompilerRuntimeEnvironment.test(canonical) || redirectEnvironment.test(canonical) || forbiddenRequestEnvironment.test(canonical)) throw new TypeError(`compiler environment cannot set broker-controlled ${canonical}`);
    if (Object.hasOwn(normalized, canonical)) throw new TypeError(`compiler environment has duplicate key: ${canonical}`);
    normalized[canonical] = environmentValue(canonical, value, 'compiler environment');
  }
  return Object.freeze(normalized);
}

/**
 * Broker-owned values always win.  Keep this boundary explicit so callers do
 * not accidentally spread an untrusted request after constructing runtime env.
 */
export function injectBrokerEnvironment({ requestEnvironment = {}, runtimeEnvironment = {}, normalizeEnvironment = normalizeRequestEnvironment } = {}) {
  if (typeof normalizeEnvironment !== 'function') throw new TypeError('environment normalizer must be a function');
  const request = normalizeEnvironment(requestEnvironment);
  const runtime = environmentObject(runtimeEnvironment, 'runtime environment');
  const injected = { ...request }; const runtimeKeys = new Set();
  for (const [key, value] of Object.entries(runtime)) {
    if (typeof key !== 'string' || !key) throw new TypeError('runtime environment key is required');
    const canonical = key.toUpperCase();
    if (runtimeKeys.has(canonical)) throw new TypeError(`runtime environment has duplicate key: ${canonical}`);
    runtimeKeys.add(canonical);
    for (const existing of Object.keys(injected)) if (existing.toUpperCase() === canonical) delete injected[existing];
    injected[key] = environmentValue(canonical, value, 'runtime environment');
  }
  return Object.freeze(injected);
}

function compilerOutputPath(value, outputRoot) {
  if (typeof value !== 'string' || !value || value === '-' || value.includes('\0')) throw new TypeError('compiler output path must name a file');
  const name = basename(value);
  if (!name || name === '.' || name === '..') throw new TypeError('compiler output path must remain below broker output root');
  return join(outputRoot, name);
}

function normalizeEmitArgument(value, outputRoot) {
  if (typeof value !== 'string' || !value) throw new TypeError('--emit requires a non-empty value');
  return value.split(',').map((entry) => {
    if (!entry) throw new TypeError('--emit cannot contain empty output entries');
    const separator = entry.indexOf('=');
    if (separator < 0) return entry;
    const kind = entry.slice(0, separator); const path = entry.slice(separator + 1);
    if (!kind) throw new TypeError('--emit output kind is required');
    return `${kind}=${compilerOutputPath(path, outputRoot)}`;
  }).join(',');
}

/**
 * Rewrite raw rustc/rustdoc output destinations into a broker-owned root.
 * Adding --out-dir also prevents compiler defaults from writing beside source.
 */
export function normalizeCompilerOutputArgv(argv, { compiler, outputRoot, targetTriple } = {}) {
  if (!['rustc', 'rustdoc'].includes(compiler)) throw new TypeError('compiler must be rustc or rustdoc');
  if (!Array.isArray(argv) || argv.some((value) => typeof value !== 'string' || !value)) throw new TypeError('compiler argv must contain non-empty strings');
  if (!isAbsolute(outputRoot)) throw new TypeError('broker compiler output root must be absolute');
  if (typeof targetTriple !== 'string' || !targetTriple || targetTriple.includes('\0')) throw new TypeError('broker compiler target is required');
  const root = normalize(outputRoot); const values = [];
  let sawOutDir = false; let sawTarget = false;
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === '--target') {
      const target = argv[++index]; if (!target) throw new TypeError('--target requires a target triple');
      if (target !== targetTriple) throw new TypeError(`compiler target must match broker target ${targetTriple}`);
      values.push('--target', targetTriple); sawTarget = true; continue;
    }
    if (value.startsWith('--target=')) {
      if (value.slice(9) !== targetTriple) throw new TypeError(`compiler target must match broker target ${targetTriple}`);
      values.push(`--target=${targetTriple}`); sawTarget = true; continue;
    }
    if (value === '-Z' || value.startsWith('-Z')) throw new TypeError('raw compiler unstable options require an explicit registered capability');
    if (['--test-builder', '--runtool', '--runtool-arg'].includes(value) || ['--test-builder=', '--runtool=', '--runtool-arg='].some((prefix) => value.startsWith(prefix))) throw new TypeError('raw compiler external tool routing is forbidden');
    if (value === '-C' || value === '--codegen') {
      const option = argv[++index]; if (!option) throw new TypeError(`${value} requires a codegen option`);
      values.push(value, normalizeCodegenOption(option, root)); continue;
    }
    if (value.startsWith('-C') && value.length > 2) { values.push(`-C${normalizeCodegenOption(value.slice(2), root)}`); continue; }
    if (value.startsWith('--codegen=')) { values.push(`--codegen=${normalizeCodegenOption(value.slice(10), root)}`); continue; }
    if (value === '-o' || value === '--output' || value === '--out-dir') {
      const output = argv[++index];
      if (output === undefined) throw new TypeError(`${value} requires an output path`);
      if (value === '--out-dir') { values.push('--out-dir', root); sawOutDir = true; }
      else values.push(value, compilerOutputPath(output, root));
      continue;
    }
    if (value.startsWith('-o') && value.length > 2) { values.push('-o', compilerOutputPath(value.slice(2), root)); continue; }
    if (value.startsWith('--out-dir=')) { values.push(`--out-dir=${root}`); sawOutDir = true; continue; }
    if (value.startsWith('--output=')) { values.push(`--output=${compilerOutputPath(value.slice('--output='.length), root)}`); continue; }
    if (value === '--emit') {
      const emit = argv[++index]; if (emit === undefined) throw new TypeError('--emit requires a value');
      values.push('--emit', normalizeEmitArgument(emit, root)); continue;
    }
    if (value.startsWith('--emit=')) { values.push(`--emit=${normalizeEmitArgument(value.slice('--emit='.length), root)}`); continue; }
    values.push(value);
  }
  if (!sawOutDir) values.push('--out-dir', root);
  if (!sawTarget) values.push('--target', targetTriple);
  return Object.freeze(values);
}

function normalizeCodegenOption(option, outputRoot) {
  const separator = option.indexOf('='); const key = separator < 0 ? option : option.slice(0, separator);
  if (!key) throw new TypeError('codegen option name is required');
  if (['linker', 'link-arg', 'link-args'].includes(key)) throw new TypeError(`raw compiler ${key} authority is forbidden`);
  if (key === 'incremental') return `incremental=${join(outputRoot, 'incremental')}`;
  if (key === 'profile-generate') return `profile-generate=${join(outputRoot, 'profile')}`;
  return option;
}

/**
 * Ensure Cargo's output identity is broker-selected even when caller omits it.
 * Call only after capability classification confirms a target-capable command.
 */
export function normalizeCargoTargetArgv(argv, targetTriple, targetDirectory = null) {
  if (!Array.isArray(argv) || argv.some((value) => typeof value !== 'string' || !value)) throw new TypeError('Cargo argv must contain non-empty strings');
  if (typeof targetTriple !== 'string' || !targetTriple || targetTriple.includes('\0')) throw new TypeError('broker Cargo target is required');
  if (targetDirectory !== null && (typeof targetDirectory !== 'string' || !isAbsolute(targetDirectory))) throw new TypeError('broker Cargo target directory must be absolute');
  const values = []; let suffix = null; let seenTarget = false; let seenTargetDirectory = false;
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === '--') { suffix = argv.slice(index); break; }
    if (value === '--target') {
      const requested = argv[++index];
      if (!requested) throw new TypeError('--target requires a target triple');
      if (requested !== targetTriple) throw new TypeError(`managed Cargo target must match broker target ${targetTriple}`);
      if (!seenTarget) { values.push('--target', targetTriple); seenTarget = true; }
      continue;
    }
    if (value.startsWith('--target=')) {
      const requested = value.slice('--target='.length);
      if (!requested || requested !== targetTriple) throw new TypeError(`managed Cargo target must match broker target ${targetTriple}`);
      if (!seenTarget) { values.push(`--target=${targetTriple}`); seenTarget = true; }
      continue;
    }
    if (value === '--target-dir') {
      const requested = argv[++index];
      if (!requested) throw new TypeError('--target-dir requires a path');
      if (targetDirectory === null) values.push('--target-dir', requested);
      else if (!seenTargetDirectory) { values.push('--target-dir', targetDirectory); seenTargetDirectory = true; }
      continue;
    }
    if (value.startsWith('--target-dir=')) {
      const requested = value.slice('--target-dir='.length);
      if (!requested) throw new TypeError('--target-dir requires a path');
      if (targetDirectory === null) values.push(value);
      else if (!seenTargetDirectory) { values.push(`--target-dir=${targetDirectory}`); seenTargetDirectory = true; }
      continue;
    }
    values.push(value);
  }
  if (!seenTarget) values.push('--target', targetTriple);
  if (targetDirectory !== null && !seenTargetDirectory) values.push('--target-dir', targetDirectory);
  if (suffix) values.push(...suffix);
  return Object.freeze(values);
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

export function validateManagedRustupInvocation(shim, argv) {
  if (shim !== 'rustup') return;
  if (!Array.isArray(argv)) throw new TypeError('argv must be an array');
  const run = argv.indexOf('run');
  if (run >= 0 && argv.slice(run + 1).some((value) => /^(?:cargo|rustc|rustdoc)(?:\.exe)?$/i.test(basename(value)))) throw new TypeError('managed rustup run cannot launch Cargo, rustc, or rustdoc');
}

export function externalRoots({ externalRoot, repositoryIdentity, toolchain, target, buildProfile = 'development', policyProfile = PROFILES.MANAGED_AGENT }) {
  if (!isAbsolute(externalRoot)) throw new TypeError('external root must be absolute');
  if (![repositoryIdentity, toolchain, target, buildProfile].every((value) => typeof value === 'string' && value.length > 0)) throw new TypeError('repository identity, toolchain, target, & build profile are required');
  requireProfile(policyProfile);
  const base = join(normalize(externalRoot), 'rightkit-build-control');
  // Target triple & Cargo profile stay out of namespace identity: Cargo owns that separation
  // inside one CARGO_TARGET_DIR. Including them splits reusable Cargo state per architecture.
  // 16 hex (64 bits) is the on-disk namespace segment, not the full digest:
  // Windows caps a path at 260 chars (MAX_PATH), and a fresh vendored-OpenSSL
  // compile nests ~145 chars of Cargo/openssl-src tree under this directory —
  // a 64-char segment pushed `.../digests/build.info` to 264 and every such
  // build failed. 64 bits is collision-proof across one host's app/toolchain
  // set, and `temp` below already slices to 16. Validators accept {16,64} so
  // pre-existing 64-char workspaces stay recognized and evictable.
  const namespace = createHash('sha256').update(JSON.stringify([repositoryIdentity, toolchain])).digest('hex').slice(0, 16);
  const globalRoot = join(base, 'global'); const root = join(base, 'workspaces', namespace);
  const contained = (path) => {
    if (pathEscapesRoot(base, path)) throw new TypeError('external roots must remain contained');
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
    if (candidate === '.' || candidate === '..' || candidate.startsWith('../') || candidate.startsWith('..\\') || pathEscapesRoot(workspace, join(workspace, candidate))) throw new TypeError('allowed relative path escapes workspace');
    return candidate.split(sep).join('/');
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

export async function scanWorkspaceForUnmanagedOutputReceipt(workspace, { readdir = defaultReaddir, lstat = defaultLstat, maxEntries = 100_000, maxDepth = 32, timeoutMs = 30_000, allowedRelativePaths = [], now = Date.now } = {}) {
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
    if (parentRelative && names.includes('.git')) return;
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
  if (tool.mutable === true || pathEscapesRoot(tool.root ?? tool.path, tool.path)) throw new TypeError('tool must remain inside immutable root');
  return Object.freeze({ path: normalize(tool.path), root: normalize(tool.root ?? tool.path), sha256: tool.sha256, version: tool.version ?? null });
}

export function normalizeRuntimeIdentity(identity) {
  if (!identity || identity.schema !== RUNTIME_MANIFEST_SCHEMA || typeof identity.version !== 'string' || !identity.version || !digest.test(identity.clientDigest ?? '') || !digest.test(identity.brokerDigest ?? '')) throw new TypeError('immutable RightKit runtime v2 identity is required');
  return Object.freeze({ schema: identity.schema, version: identity.version, clientDigest: identity.clientDigest, brokerDigest: identity.brokerDigest });
}
