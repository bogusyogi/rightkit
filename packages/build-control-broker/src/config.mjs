import { readFile as defaultReadFile } from 'node:fs/promises';
import { isAbsolute, normalize, relative, win32 } from 'node:path';

const topLevel = new Set(['schemaVersion', 'broker', 'unixSocket', 'windowsPipe', 'tools', 'shims', 'defaults', 'outputPolicy', 'bounds', 'sccache', 'externalCommands']);
const brokerFields = new Set(['externalRoot', 'volumeId', 'capacityBytes', 'usedBytes', 'stateFile', 'receiptsDir', 'databasePath', 'eventLogPath', 'activationFencePath']);
const externalRootFields = new Set(['path', 'volumeId', 'mounted', 'writable', 'internal']);
const toolFields = new Set(['id', 'path', 'root', 'sha256']);
const shimNames = new Set(['cargo', 'rustc', 'rustdoc', 'rustup', 'pnpm', 'npm', 'swift', 'swiftc', 'cmake', 'make', 'cc', 'c++', 'clang', 'gcc']);
const defaultFields = new Set(['repositoryIdentity', 'toolchain', 'targetTriple', 'targetTriples', 'buildProfile', 'reservationBytes', 'rustcToolId', 'rustdocToolId', 'sccacheToolId', 'cacheMode', 'cargoBuildJobs']);
const outputPolicyFields = new Set(['mode', 'allowedSourceDirs', 'scanRoot']);
const externalCommandFields = new Set(['shim', 'command', 'toolId', 'lane', 'digest']);
const sccacheFields = new Set(['mode', 'lifecycleOwner', 'requireOwnershipProof', 'maxSizeBytes']);
const defaultSccacheMaxSizeBytes = 20 * 1024 ** 3;
const boundFields = new Set(['pollIntervalMs', 'heartbeatIntervalMs', 'diskSampleIntervalMs', 'processGraceMs', 'freeFloorBytes', 'quotaWatermarkBytes', 'retentionMs', 'gcIntervalMs', 'statfsTimeoutMs', 'jobDeadlineMs', 'outputBufferBytes', 'outputHeadBytes', 'outputTailBytes', 'eventHistoryEntries', 'eventHistoryBytes', 'eventLogMaxBytes', 'maintenanceBatchEntries', 'controlConcurrency', 'buildConcurrency', 'workspaceWriteConcurrency', 'maintenanceConcurrency', 'priorityAgingMs', 'targetCacheCapBytes']);
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

function validateDefaults(value, ids, capacity) {
  const defaults = object(value, 'defaults'); only(defaults, defaultFields, 'defaults');
  const normalized = {};
  for (const field of ['repositoryIdentity', 'toolchain', 'targetTriple', 'buildProfile']) if (defaults[field] !== undefined) normalized[field] = requiredString(defaults[field], `defaults.${field}`, SAFE_TOKEN);
  if (defaults.targetTriples !== undefined) {
    if (!Array.isArray(defaults.targetTriples) || defaults.targetTriples.length < 1 || defaults.targetTriples.length > 8) throw new TypeError('defaults.targetTriples must contain 1-8 target triples');
    normalized.targetTriples = defaults.targetTriples.map((value, index) => requiredString(value, `defaults.targetTriples[${index}]`, SAFE_TOKEN));
    if (new Set(normalized.targetTriples).size !== normalized.targetTriples.length) throw new TypeError('defaults.targetTriples must be unique');
    if (normalized.targetTriple && !normalized.targetTriples.includes(normalized.targetTriple)) throw new TypeError('defaults.targetTriples must include defaults.targetTriple');
  }
  if (defaults.reservationBytes !== undefined) {
    normalized.reservationBytes = nonnegative(defaults.reservationBytes, 'defaults.reservationBytes');
    if (normalized.reservationBytes > capacity) throw new TypeError('defaults.reservationBytes cannot exceed broker.capacityBytes');
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

function validateBounds(value, { schemaVersion, capacity }) {
  const bounds = object(value, 'bounds'); only(bounds, boundFields, 'bounds');
  if (bounds.pollIntervalMs !== undefined && schemaVersion === 2) throw new TypeError('bounds.pollIntervalMs is only accepted by schemaVersion 1');
  const normalized = {};
  // A null target-cache cap means uncapped: age retention alone governs.
  const nullable = new Set(['quotaWatermarkBytes', 'retentionMs', 'targetCacheCapBytes']);
  const positive = new Set(['pollIntervalMs', 'heartbeatIntervalMs', 'diskSampleIntervalMs', 'gcIntervalMs', 'statfsTimeoutMs', 'eventHistoryEntries', 'eventLogMaxBytes', 'maintenanceBatchEntries', 'controlConcurrency', 'buildConcurrency', 'workspaceWriteConcurrency', 'maintenanceConcurrency', 'priorityAgingMs']);
  const minimums = new Map([['eventHistoryEntries', 16], ['eventLogMaxBytes', 1024], ['maintenanceBatchEntries', 1], ['controlConcurrency', 1], ['buildConcurrency', 1], ['workspaceWriteConcurrency', 1], ['maintenanceConcurrency', 1], ['outputBufferBytes', 1024]]);
  for (const key of boundFields) if (bounds[key] !== undefined) normalized[key] = nonnegative(bounds[key], `bounds.${key}`, { nullable: nullable.has(key), minimum: minimums.get(key) ?? (positive.has(key) ? 1 : 0) });
  if (bounds.diskSampleIntervalMs === undefined && bounds.pollIntervalMs !== undefined) normalized.diskSampleIntervalMs = normalized.pollIntervalMs;
  if (normalized.quotaWatermarkBytes !== undefined && normalized.quotaWatermarkBytes !== null && normalized.quotaWatermarkBytes > capacity) throw new TypeError('bounds.quotaWatermarkBytes cannot exceed broker.capacityBytes');
  if (normalized.targetCacheCapBytes !== undefined && normalized.targetCacheCapBytes !== null && normalized.targetCacheCapBytes > capacity) throw new TypeError('bounds.targetCacheCapBytes cannot exceed broker.capacityBytes');
  const outputBuffer = normalized.outputBufferBytes ?? 8 * 1024 * 1024;
  const durableOutput = (normalized.outputHeadBytes ?? 16 * 1024) + (normalized.outputTailBytes ?? 16 * 1024);
  if (durableOutput > outputBuffer) throw new TypeError('bounds.outputHeadBytes + bounds.outputTailBytes cannot exceed bounds.outputBufferBytes');
  return normalized;
}

/** Reject malformed or ambiguous configuration before a process can bind a live endpoint. */
export function validateBrokerConfig(value) {
  const config = object(value, 'broker config'); only(config, topLevel, 'broker config');
  const schemaVersion = config.schemaVersion ?? 1;
  if (schemaVersion !== 1 && schemaVersion !== 2) throw new TypeError('schemaVersion must be 1 or 2');

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
  if (broker.volumeId !== undefined) normalizedBroker.volumeId = requiredString(broker.volumeId, 'broker.volumeId');
  if (broker.usedBytes !== undefined) {
    normalizedBroker.usedBytes = nonnegative(broker.usedBytes, 'broker.usedBytes');
    if (normalizedBroker.usedBytes > capacityBytes) throw new TypeError('broker.usedBytes cannot exceed broker.capacityBytes');
  }
  for (const field of ['databasePath', 'eventLogPath', 'activationFencePath']) if (broker[field] !== undefined) normalizedBroker[field] = inside(rootPath, absolute(broker[field], `broker.${field}`), `broker.${field}`);
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
  normalized.defaults = validateDefaults(config.defaults ?? {}, toolIds, capacityBytes);

  if (config.outputPolicy !== undefined) normalized.outputPolicy = validateOutputPolicy(config.outputPolicy);
  if (config.bounds !== undefined) normalized.bounds = validateBounds(config.bounds, { schemaVersion, capacity: capacityBytes });

  if (config.externalCommands !== undefined) {
    if (!Array.isArray(config.externalCommands)) throw new TypeError('externalCommands must be an array');
    const seen = new Set();
    normalized.externalCommands = config.externalCommands.map((entry, index) => {
      const item = object(entry, `externalCommands[${index}]`); only(item, externalCommandFields, `externalCommands[${index}]`);
      if (item.shim !== 'cargo' || !['about', 'nextest'].includes(item.command) || !['CONTROL', 'BUILD', 'WORKSPACE_WRITE'].includes(item.lane)) throw new TypeError('invalid external command registration');
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
  if (mode === 'managed' && (lifecycleOwner !== 'broker' || requireOwnershipProof !== true || !normalized.defaults.sccacheToolId || normalized.defaults.cacheMode !== 'managed')) throw new TypeError('managed sccache requires registered broker lifecycle ownership proof');
  if (mode === 'disabled' && (lifecycleOwner !== null || requireOwnershipProof !== false || normalized.defaults.cacheMode === 'managed')) throw new TypeError('disabled sccache cannot carry managed ownership settings');
  // The compiler cache outlives any single mode: its directory persists while
  // sccache is disabled, so the size bound is configured independently of mode
  // and upstream sccache's own LRU enforces it.
  const maxSizeBytes = sccache.maxSizeBytes ?? defaultSccacheMaxSizeBytes;
  if (!Number.isSafeInteger(maxSizeBytes) || maxSizeBytes < 1) throw new TypeError('sccache.maxSizeBytes must be a positive safe integer');
  normalized.sccache = { mode, lifecycleOwner, requireOwnershipProof, maxSizeBytes };
  return freezeConfig(normalized);
}

export async function loadBrokerConfig(path, { readFile = defaultReadFile } = {}) {
  if (typeof path !== 'string' || !path) throw new TypeError('config path is required');
  let parsed;
  try { parsed = JSON.parse(await readFile(path, 'utf8')); } catch (error) { throw new TypeError(`invalid broker config: ${error.message}`); }
  return validateBrokerConfig(parsed);
}
