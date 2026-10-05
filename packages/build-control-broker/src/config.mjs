import { HOST_DERIVED_BOUNDS } from './host-sizing.mjs';
import { readFile as defaultReadFile } from 'node:fs/promises';
import { isAbsolute, normalize, relative, win32 } from 'node:path';

const topLevel = new Set(['schemaVersion', 'broker', 'unixSocket', 'windowsPipe', 'tools', 'shims', 'defaults', 'outputPolicy', 'bounds', 'sccache', 'externalCommands']);
const brokerFields = new Set(['externalRoot', 'capacityBytes', 'stateFile', 'receiptsDir', 'activationFencePath']);
const externalRootFields = new Set(['path', 'volumeId', 'mounted', 'writable', 'internal']);
const toolFields = new Set(['id', 'path', 'root', 'sha256']);
const shimNames = new Set(['cargo', 'rustc', 'rustdoc', 'rustup', 'pnpm', 'npm', 'swift', 'swiftc', 'cmake', 'make', 'cc', 'c++', 'clang', 'gcc']);
const defaultFields = new Set(['repositoryIdentity', 'toolchain', 'targetTriple', 'targetTriples', 'buildProfile', 'rustcToolId', 'rustdocToolId', 'sccacheToolId', 'cacheMode', 'cargoBuildJobs']);
const outputPolicyFields = new Set(['mode', 'allowedSourceDirs', 'scanRoot']);
const externalCommandFields = new Set(['shim', 'command', 'toolId', 'lane', 'digest']);
const sccacheFields = new Set(['mode', 'clientSide', 'lifecycleOwner', 'requireOwnershipProof', 'expectedVersion', 'maxSizeBytes', 'basedirs', 'basedirsEnabled', 'gatewayPort', 'healthLeaseMs', 'recoveryTimeoutMs', 'statsIntervalMs']);
const defaultSccacheMaxSizeBytes = 20 * 1024 ** 3;
const boundFields = new Set(['heartbeatIntervalMs', 'processGraceMs', 'freeFloorBytes', 'jobDeadlineMs', 'outputBufferBytes', 'outputHeadBytes', 'outputTailBytes', 'controlConcurrency', 'analysisConcurrency', 'buildConcurrency', 'workspaceWriteConcurrency', 'targetCacheCapBytes', 'workspaceDiscoveryTtlMs', 'workspaceDiscoveryTimeoutMs', 'compileBudgetUnits', 'buildCostUnits', 'gcFreeFloorBytes']);
const EXTERNAL_COMMAND = /^[a-z][a-z0-9-]*$/;
const HEX_256 = /^[a-f0-9]{64}$/i;
const SAFE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const SAFE_TOKEN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

function object(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} must be an object`);
  return value;
}

function only(value, allowed, label) {
  for (const key of Object.keys(value)) if (!allowed.has(key)) throw new TypeError(`unknown ${label} key: ${key}`);
}

function isWindowsPath(value) { return typeof value === 'string' && (/^[A-Za-z]:[\\/]/.test(value) || /^\\\\/.test(value)); }

function absolute(value, label) {
  if (typeof value !== 'string' || !value || (!isAbsolute(value) && !isWindowsPath(value))) throw new TypeError(`${label} must be an absolute path`);
  return isWindowsPath(value) ? win32.normalize(value) : normalize(value);
}

function inside(root, candidate, label) {
  const rootWindows = isWindowsPath(root); const candidateWindows = isWindowsPath(candidate);
  if (rootWindows !== candidateWindows) throw new TypeError(`${label} must use ${rootWindows ? 'Windows' : 'POSIX'} path syntax`);
  const pathApi = rootWindows ? win32 : { relative, isAbsolute };
  const result = pathApi.relative(root, candidate);
  if (!result || result === '.' || result === '..' || result.startsWith(`..${rootWindows ? '\\' : '/'}`) || pathApi.isAbsolute(result)) throw new TypeError(`${label} must remain below broker.externalRoot.path`);
  return candidate;
}

function nonnegative(value, label, { nullable = false, minimum = 0 } = {}) {
  if (nullable && value === null) return null;
  if (!Number.isSafeInteger(value) || value < minimum) throw new TypeError(`${label} must be a safe integer >= ${minimum}`);
  return value;
}

function requiredString(value, label, pattern = null) {
  if (typeof value !== 'string' || !value || (pattern && !pattern.test(value))) throw new TypeError(`${label} is invalid`);
  return value;
}

function requiredBoolean(value, label) { if (typeof value !== 'boolean') throw new TypeError(`${label} must be boolean`); return value; }
function freezeConfig(value) { if (value && typeof value === 'object' && !Object.isFrozen(value)) { for (const child of Object.values(value)) freezeConfig(child); Object.freeze(value); } return value; }

function relativeSourcePath(value, label) {
  if (typeof value !== 'string' || !value || isAbsolute(value) || isWindowsPath(value) || value === '.' || value.split(/[\\/]/).includes('..')) throw new TypeError(`${label} must be a non-empty relative path without traversal`);
  return value;
}

function registeredToolIds(tools) { return new Set(tools.map((tool) => tool.id)); }

function referenceTool(value, label, ids) {
  const id = requiredString(value, label, SAFE_ID);
  if (!ids.has(id)) throw new TypeError(`${label} must reference a registered tool`);
  return id;
}

function validateTools(value) {
  if (!Array.isArray(value)) throw new TypeError('tools must be an array');
  const seen = new Set();
  return value.map((entry, index) => {
    const tool = object(entry, `tools[${index}]`); only(tool, toolFields, `tools[${index}]`);
    const id = requiredString(tool.id, `tools[${index}].id`, SAFE_ID);
    if (seen.has(id)) throw new TypeError(`duplicate tool id: ${id}`);
    seen.add(id);
    const root = absolute(tool.root, `tools[${index}].root`);
    const path = inside(root, absolute(tool.path, `tools[${index}].path`), `tools[${index}].path`);
    if (typeof tool.sha256 !== 'string' || !HEX_256.test(tool.sha256)) throw new TypeError(`tools[${index}].sha256 must be a SHA-256 digest`);
    return { id, root, path, sha256: tool.sha256.toLowerCase() };
  });
}

function validateShims(value, ids) {
  const shims = object(value, 'shims'); only(shims, shimNames, 'shims');
  const normalized = {};
  for (const [shim, toolId] of Object.entries(shims)) normalized[shim] = referenceTool(toolId, `shims.${shim}`, ids);
  return normalized;
}

function validateDefaults(value, ids) {
  const defaults = object(value, 'defaults'); only(defaults, defaultFields, 'defaults');
  const normalized = {};
  for (const field of ['repositoryIdentity', 'toolchain', 'targetTriple', 'buildProfile']) if (defaults[field] !== undefined) normalized[field] = requiredString(defaults[field], `defaults.${field}`, SAFE_TOKEN);
  if (defaults.targetTriples !== undefined) {
    if (!Array.isArray(defaults.targetTriples) || defaults.targetTriples.length < 1 || defaults.targetTriples.length > 8) throw new TypeError('defaults.targetTriples must contain 1-8 target triples');
    normalized.targetTriples = defaults.targetTriples.map((value, index) => requiredString(value, `defaults.targetTriples[${index}]`, SAFE_TOKEN));
    if (new Set(normalized.targetTriples).size !== normalized.targetTriples.length) throw new TypeError('defaults.targetTriples must be unique');
    if (normalized.targetTriple && !normalized.targetTriples.includes(normalized.targetTriple)) throw new TypeError('defaults.targetTriples must include defaults.targetTriple');
  }
  for (const field of ['rustcToolId', 'rustdocToolId', 'sccacheToolId']) if (defaults[field] !== undefined) normalized[field] = referenceTool(defaults[field], `defaults.${field}`, ids);
  if (defaults.cacheMode !== undefined) {
    if (!['disabled', 'managed'].includes(defaults.cacheMode)) throw new TypeError('defaults.cacheMode must be disabled or managed');
    normalized.cacheMode = defaults.cacheMode;
  }
  if (defaults.cargoBuildJobs !== undefined) normalized.cargoBuildJobs = nonnegative(defaults.cargoBuildJobs, 'defaults.cargoBuildJobs', { minimum: 1 });
  return normalized;
}

function validateOutputPolicy(value) {
  const policy = object(value, 'outputPolicy'); only(policy, outputPolicyFields, 'outputPolicy');
  const normalized = {};
  if (policy.mode !== undefined) {
    if (policy.mode !== 'enforce') throw new TypeError('outputPolicy.mode must be enforce');
    normalized.mode = policy.mode;
  }
  if (policy.scanRoot !== undefined) {
    if (policy.scanRoot !== 'repository') throw new TypeError('outputPolicy.scanRoot must be repository');
    normalized.scanRoot = policy.scanRoot;
  }
  if (policy.allowedSourceDirs !== undefined) {
    if (!Array.isArray(policy.allowedSourceDirs)) throw new TypeError('outputPolicy.allowedSourceDirs must be an array');
    const seen = new Set();
    normalized.allowedSourceDirs = policy.allowedSourceDirs.map((path, index) => { const item = relativeSourcePath(path, `outputPolicy.allowedSourceDirs[${index}]`); if (seen.has(item)) throw new TypeError(`duplicate outputPolicy.allowedSourceDirs entry: ${item}`); seen.add(item); return item; });
  }
  return normalized;
}

function validateBounds(value, { capacity }) {
  const bounds = object(value, 'bounds'); only(bounds, boundFields, 'bounds');
  const normalized = {};
  const positive = new Set(['heartbeatIntervalMs', 'processGraceMs', 'controlConcurrency', 'analysisConcurrency', 'buildConcurrency', 'workspaceWriteConcurrency', 'workspaceDiscoveryTimeoutMs', 'compileBudgetUnits', 'buildCostUnits']);
  const minimums = new Map([['controlConcurrency', 1], ['analysisConcurrency', 1], ['buildConcurrency', 1], ['workspaceWriteConcurrency', 1], ['outputBufferBytes', 1024], ['workspaceDiscoveryTimeoutMs', 1], ['workspaceDiscoveryTtlMs', 0]]);
  for (const key of boundFields) if (bounds[key] !== undefined) normalized[key] = nonnegative(bounds[key], `bounds.${key}`, { minimum: minimums.get(key) ?? (positive.has(key) ? 1 : 0) });
  if (normalized.targetCacheCapBytes !== undefined && normalized.targetCacheCapBytes > capacity) throw new TypeError('bounds.targetCacheCapBytes cannot exceed broker.capacityBytes');
  if (normalized.buildCostUnits !== undefined && (normalized.compileBudgetUnits === undefined || normalized.buildCostUnits > normalized.compileBudgetUnits)) throw new TypeError('bounds.buildCostUnits requires compileBudgetUnits and cannot exceed it');
  const outputBuffer = normalized.outputBufferBytes ?? 8 * 1024 * 1024;
  const durableOutput = (normalized.outputHeadBytes ?? 16 * 1024) + (normalized.outputTailBytes ?? 16 * 1024);
  if (durableOutput > outputBuffer) throw new TypeError('bounds.outputHeadBytes + bounds.outputTailBytes cannot exceed bounds.outputBufferBytes');
  return normalized;
}

/** Reject malformed or ambiguous configuration before a process can bind a live endpoint. */
export function validateBrokerConfig(value) {
  const config = object(value, 'broker config'); only(config, topLevel, 'broker config');
  const schemaVersion = config.schemaVersion ?? 2;
  if (schemaVersion !== 2) throw new TypeError('schemaVersion must be 2');

  const broker = object(config.broker, 'broker'); only(broker, brokerFields, 'broker');
  const externalRoot = object(broker.externalRoot, 'broker.externalRoot'); only(externalRoot, externalRootFields, 'broker.externalRoot');
  const rootPath = absolute(externalRoot.path, 'broker.externalRoot.path');
  const capacityBytes = nonnegative(broker.capacityBytes, 'broker.capacityBytes', { minimum: 1 });
  const normalizedBroker = {
    externalRoot: {
      path: rootPath,
      volumeId: requiredString(externalRoot.volumeId, 'broker.externalRoot.volumeId'),
      mounted: requiredBoolean(externalRoot.mounted, 'broker.externalRoot.mounted'),
      writable: requiredBoolean(externalRoot.writable, 'broker.externalRoot.writable'),
      internal: requiredBoolean(externalRoot.internal, 'broker.externalRoot.internal')
    },
    capacityBytes,
    stateFile: inside(rootPath, absolute(broker.stateFile, 'broker.stateFile'), 'broker.stateFile'),
    receiptsDir: inside(rootPath, absolute(broker.receiptsDir, 'broker.receiptsDir'), 'broker.receiptsDir')
  };
  if (broker.activationFencePath !== undefined) normalizedBroker.activationFencePath = inside(rootPath, absolute(broker.activationFencePath, 'broker.activationFencePath'), 'broker.activationFencePath');
  if (schemaVersion === 2 && normalizedBroker.activationFencePath === undefined) throw new TypeError('broker.activationFencePath is required by schemaVersion 2');

  const hasUnixSocket = config.unixSocket !== undefined; const hasWindowsPipe = config.windowsPipe !== undefined;
  if (!hasUnixSocket && !hasWindowsPipe) throw new TypeError('unixSocket or windowsPipe is required');
  const normalized = { schemaVersion, ...config, broker: normalizedBroker };
  delete normalized.unixSocket; delete normalized.windowsPipe;
  if (hasUnixSocket) normalized.unixSocket = inside(rootPath, absolute(config.unixSocket, 'unixSocket'), 'unixSocket');
  if (hasWindowsPipe) {
    if (typeof config.windowsPipe !== 'string' || !/^\\\\\.\\pipe\\[^\\/]+$/i.test(config.windowsPipe)) throw new TypeError('windowsPipe must be a named pipe path');
    normalized.windowsPipe = config.windowsPipe;
  }

  const tools = validateTools(config.tools ?? []); normalized.tools = tools;
  const toolIds = registeredToolIds(tools);
  normalized.shims = validateShims(config.shims ?? {}, toolIds);
  normalized.defaults = validateDefaults(config.defaults ?? {}, toolIds);

  if (config.outputPolicy !== undefined) normalized.outputPolicy = validateOutputPolicy(config.outputPolicy);
  if (config.bounds !== undefined) normalized.bounds = validateBounds(config.bounds, { capacity: capacityBytes });

  if (config.externalCommands !== undefined) {
    if (!Array.isArray(config.externalCommands)) throw new TypeError('externalCommands must be an array');
    const seen = new Set();
    normalized.externalCommands = config.externalCommands.map((entry, index) => {
      const item = object(entry, `externalCommands[${index}]`); only(item, externalCommandFields, `externalCommands[${index}]`);
      // The command name is validated by the capability registry, which owns the
      // rule (a bounded token that may not shadow a built-in). Duplicating a
      // literal allowlist here is what let the registry be opened while config
      // load still rejected the very same entry, crash-looping the broker.
      if (item.shim !== 'cargo' || typeof item.command !== 'string' || !EXTERNAL_COMMAND.test(item.command) || !['CONTROL', 'ANALYSIS', 'BUILD', 'WORKSPACE_WRITE'].includes(item.lane)) throw new TypeError('invalid external command registration');
      const toolId = referenceTool(item.toolId, `externalCommands[${index}].toolId`, toolIds);
      if (typeof item.digest !== 'string' || !HEX_256.test(item.digest)) throw new TypeError(`externalCommands[${index}].digest must be a SHA-256 digest`);
      const key = `${item.shim}:${item.command}`; if (seen.has(key)) throw new TypeError(`duplicate external command: ${key}`); seen.add(key);
      return { shim: item.shim, command: item.command, toolId, lane: item.lane, digest: item.digest.toLowerCase() };
    });
  }

  const sccache = config.sccache === undefined ? {} : object(config.sccache, 'sccache'); only(sccache, sccacheFields, 'sccache');
  const mode = sccache.mode ?? 'disabled';
  if (!['disabled', 'managed'].includes(mode)) throw new TypeError('sccache.mode must be disabled or managed');
  const lifecycleOwner = sccache.lifecycleOwner ?? null;
  if (lifecycleOwner !== null && lifecycleOwner !== 'broker') throw new TypeError('sccache.lifecycleOwner must be broker or null');
  const requireOwnershipProof = sccache.requireOwnershipProof ?? false;
  if (typeof requireOwnershipProof !== 'boolean') throw new TypeError('sccache.requireOwnershipProof must be boolean');
  const clientSide = sccache.clientSide ?? false;
  if (typeof clientSide !== 'boolean') throw new TypeError('sccache.clientSide must be boolean');
  const expectedVersion = sccache.expectedVersion ?? null;
  if (expectedVersion !== null && !/^\d+\.\d+\.\d+$/.test(expectedVersion)) throw new TypeError('sccache.expectedVersion must be an exact semantic version');
  const basedirs = sccache.basedirs;
  const basedirsEnabled = sccache.basedirsEnabled ?? false;
  if (basedirs !== undefined) {
    if (!Array.isArray(basedirs) || !basedirs.every((value) => typeof value === 'string' && isAbsolute(value))) throw new TypeError('sccache.basedirs must be an array of absolute paths');
    if (basedirs.length === 0) throw new TypeError('sccache.basedirs must be non-empty when present');
  }
  if (typeof basedirsEnabled !== 'boolean') throw new TypeError('sccache.basedirsEnabled must be boolean');
  if (mode === 'managed' && (!normalized.defaults.sccacheToolId || normalized.defaults.cacheMode !== 'managed')) throw new TypeError('managed sccache requires a registered tool');
  if (mode === 'managed' && (lifecycleOwner !== 'broker' || requireOwnershipProof !== true)) throw new TypeError('managed sccache requires broker lifecycle ownership proof');
  if (mode === 'managed' && expectedVersion === null) throw new TypeError('managed sccache requires exact expectedVersion');
  if (mode === 'disabled' && (clientSide || lifecycleOwner !== null || requireOwnershipProof !== false || expectedVersion !== null || normalized.defaults.cacheMode === 'managed')) throw new TypeError('disabled sccache cannot carry managed ownership settings');
  // The compiler cache outlives any single mode: its directory persists while
  // sccache is disabled, so the size bound is configured independently of mode
  // and upstream sccache's own LRU enforces it.
  const maxSizeBytes = sccache.maxSizeBytes ?? defaultSccacheMaxSizeBytes;
  if (!Number.isSafeInteger(maxSizeBytes) || maxSizeBytes < 1) throw new TypeError('sccache.maxSizeBytes must be a positive safe integer');
  const gatewayPort = nonnegative(sccache.gatewayPort ?? 4_227, 'sccache.gatewayPort', { minimum: 1 });
  if (gatewayPort > 65_535) throw new TypeError('sccache.gatewayPort must be <= 65535');
  const healthLeaseMs = nonnegative(sccache.healthLeaseMs ?? 10_000, 'sccache.healthLeaseMs', { minimum: 1 });
  const recoveryTimeoutMs = nonnegative(sccache.recoveryTimeoutMs ?? 10_000, 'sccache.recoveryTimeoutMs', { minimum: 1 });
  const statsIntervalMs = nonnegative(sccache.statsIntervalMs ?? 60_000, 'sccache.statsIntervalMs', { minimum: 1 });
  normalized.sccache = { mode, clientSide, lifecycleOwner, requireOwnershipProof, ...(expectedVersion !== null ? { expectedVersion } : {}), maxSizeBytes, gatewayPort, healthLeaseMs, recoveryTimeoutMs, statsIntervalMs, ...(basedirs !== undefined ? { basedirs } : {}), ...(basedirsEnabled ? { basedirsEnabled: true } : {}) };
  return freezeConfig(normalized);
}

// Node's JSON.parse does not strip a leading UTF-8 BOM, and Windows editors
// (PowerShell's Set-Content/Out-File, Notepad) write one by default. A BOM'd
// config previously failed as an opaque "Unexpected token" with no hint that
// the file's own bytes, not its content, were the problem.
export function stripBom(text) { return typeof text === 'string' && text.charCodeAt(0) === 0xfeff ? text.slice(1) : text; }

export async function loadBrokerConfig(path, { readFile = defaultReadFile } = {}) {
  if (typeof path !== 'string' || !path) throw new TypeError('config path is required');
  let parsed;
  try { parsed = JSON.parse(stripBom(await readFile(path, 'utf8'))); } catch (error) { throw new TypeError(`invalid broker config: ${error.message}`); }
  return validateBrokerConfig(parsed);
}

// A --force re-bootstrap regenerates config.json from source defaults, which
// used to silently revert operator-tuned values (sccache.maxSizeBytes reset
// 250 GB -> 20 GiB; hand-set bounds floors deleted). Carry those forward from
// the previous config unless doing so would make the regenerated config
// invalid, in which case the offending field falls back to the default.
export function preserveOperatorTunedConfig(next, previous) {
  if (!next || typeof next !== 'object' || !previous || typeof previous !== 'object') return [];
  const preserved = [];
  const maxSizeBytes = previous.sccache?.maxSizeBytes;
  // The generated sccache size is a volume-derived ceiling: keep a smaller operator value, never a larger one.
  if (next.sccache && typeof next.sccache === 'object' && Number.isSafeInteger(maxSizeBytes) && maxSizeBytes >= 1 && maxSizeBytes < next.sccache.maxSizeBytes) {
    next.sccache.maxSizeBytes = maxSizeBytes;
    preserved.push('sccache.maxSizeBytes');
  }
  if (previous.bounds && typeof previous.bounds === 'object' && !Array.isArray(previous.bounds)) {
    const generatedBounds = next.bounds; const preservedBounds = { ...previous.bounds };
    // Migrate known shipped discovery defaults while retaining genuine
    // operator tuning in every other bound. Mac shipped 10s; Windows 2s.
    if ([2_000, 10_000].includes(preservedBounds.workspaceDiscoveryTimeoutMs) && Number.isSafeInteger(generatedBounds?.workspaceDiscoveryTimeoutMs)) preservedBounds.workspaceDiscoveryTimeoutMs = generatedBounds.workspaceDiscoveryTimeoutMs;
    // Host-derived bounds (CPU, memory, volume) are regenerated, never carried forward.
    for (const key of HOST_DERIVED_BOUNDS) { if (generatedBounds?.[key] !== undefined) preservedBounds[key] = generatedBounds[key]; else delete preservedBounds[key]; }
    next.bounds = preservedBounds;
    try { validateBrokerConfig(next); preserved.push('bounds'); } catch { next.bounds = generatedBounds; }
  }
  return preserved;
}

/** Refuse a regenerated config that drops tools the working config has. Tool discovery reads PATH, so a
 * bootstrap from a shell without ~/.cargo/bin silently lost cargo/sccache and the broker could not start
 * (2026-10-05). */
export function assertNoToolLoss(next, previous) {
  const kept = new Set((next?.tools ?? []).map((tool) => tool.id));
  const lost = (previous?.tools ?? []).map((tool) => tool.id).filter((id) => !kept.has(id));
  if (lost.length) throw new Error(`regenerated config would drop tools: ${lost.join(', ')}; run bootstrap from a shell whose PATH exposes them (or remove them from the existing config deliberately)`);
}
