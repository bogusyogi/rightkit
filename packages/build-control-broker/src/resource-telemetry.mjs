const CPU_FIELDS = ['cpuUserMs', 'cpuKernelMs', 'cpuTotalMs', 'totalProcesses', 'activeProcesses'];
const MEMORY_KINDS = new Set(['committed', 'physical-footprint']);

const validNumber = (value) => Number.isFinite(value) && value >= 0;

function error(reason) {
  return Object.freeze({ status: 'ERROR', reason });
}

/** Normalize one native/resource reader result without turning missing values into zero. */
export function safeResourceSnapshot(reader) {
  let value;
  try {
    value = typeof reader === 'function' ? reader() : reader;
  } catch (cause) {
    return Object.freeze({ status: 'ERROR', reason: 'resource-reader-threw', error: String(cause?.message ?? cause) });
  }
  if (!value || typeof value !== 'object' || typeof value.status !== 'string') return error('resource-snapshot-invalid');
  if (value.status === 'OK') {
    if (CPU_FIELDS.some((field) => !validNumber(value[field]))) return error('resource-snapshot-invalid');
    const memoryKind = value.memoryKind === undefined ? (validNumber(value.peakCommittedBytes) ? 'committed' : 'physical-footprint') : String(value.memoryKind);
    if (!MEMORY_KINDS.has(memoryKind)) return error('resource-memory-kind-unsupported');
    const memoryField = memoryKind === 'committed' ? 'peakCommittedBytes' : 'peakMemoryBytes';
    if (!validNumber(value[memoryField])) return error('resource-snapshot-invalid');
    return Object.freeze({ status: 'OK', ...Object.fromEntries(CPU_FIELDS.map((field) => [field, value[field]])), [memoryField]: value[memoryField], ...(memoryKind === 'committed' ? {} : { memoryKind }), ...(typeof value.memorySemantics === 'string' ? { memorySemantics: value.memorySemantics } : {}), ...(value.epoch === undefined ? {} : { epoch: value.epoch }) });
  }
  if (value.status === 'UNAVAILABLE' || value.status === 'ERROR') {
    return Object.freeze({ status: value.status, ...(value.reason ? { reason: String(value.reason) } : {}), ...(value.error ? { error: String(value.error) } : {}), ...(value.epoch === undefined ? {} : { epoch: value.epoch }) });
  }
  return error('resource-snapshot-invalid');
}

function childSnapshot(value) {
  return safeResourceSnapshot(value);
}

function summarizeCache(cacheBefore, cacheAfter, attribution, expected) {
  const base = { scope: 'shared-cache-gateway-window', attribution: attribution?.exact === true ? 'exclusive' : 'shared' };
  if (expected === false) return Object.freeze({ status: 'NOT_APPLICABLE', ...base });
  const before = childSnapshot(cacheBefore);
  const after = childSnapshot(cacheAfter);
  if (before.status !== 'OK' || after.status !== 'OK') return Object.freeze({ status: 'UNAVAILABLE', ...base, reason: 'shared-cache-resource-unavailable' });
  if (before.epoch === undefined || after.epoch === undefined || before.epoch !== after.epoch) return Object.freeze({ status: 'UNAVAILABLE', ...base, reason: 'shared-cache-epoch-changed' });
  if (after.cpuTotalMs < before.cpuTotalMs) return Object.freeze({ status: 'UNAVAILABLE', ...base, reason: 'shared-cache-counter-reset' });
  const memoryKind = after.memoryKind ?? 'committed';
  const memoryField = memoryKind === 'physical-footprint' ? 'peakMemoryBytesSinceGatewayStart' : 'peakCommittedBytesSinceGatewayStart';
  const sourceField = memoryKind === 'physical-footprint' ? 'peakMemoryBytes' : 'peakCommittedBytes';
  return Object.freeze({ status: 'OK', ...base, cpuTotalMs: after.cpuTotalMs - before.cpuTotalMs, memoryKind, [memoryField]: after[sourceField], ...(after.memorySemantics === undefined ? {} : { memorySemantics: after.memorySemantics }) });
}

/** Build a report from already-collected measurements; this function has no effects or policy. */
export function summarizeBuildResources({ processTree, delegated = [], cacheBefore, cacheAfter, cacheAttribution, cacheExpected = true, runMs, host } = {}) {
  const tree = childSnapshot(processTree);
  const processReport = Object.freeze({ ...tree, memoryKind: tree.memoryKind ?? 'committed' });
  const children = Array.isArray(delegated) ? delegated.map(childSnapshot) : [];
  const childErrors = children.filter((item) => item.status === 'ERROR');
  const allChildrenOk = children.every((item) => item.status === 'OK');
  const childMemoryKinds = new Set(children.filter((item) => item.status === 'OK').map((item) => item.memoryKind ?? 'committed'));
  const delegatedMemoryMixed = allChildrenOk && childMemoryKinds.size > 1;
  const delegatedMemoryKind = childMemoryKinds.size === 1 ? [...childMemoryKinds][0] : undefined;
  const delegatedMemoryField = delegatedMemoryKind === 'physical-footprint' ? 'peakMemoryBytes' : 'peakCommittedBytes';
  const delegatedMemorySemantics = children.length > 0 && children.every((item) => item.memorySemantics === children[0].memorySemantics) ? children[0].memorySemantics : undefined;
  const delegatedReport = delegatedMemoryMixed
    ? Object.freeze({ status: 'ERROR', count: children.length, reason: 'delegated-resource-memory-kind-mixed' })
    : allChildrenOk
    ? Object.freeze({ status: 'OK', count: children.length, cpuTotalMs: children.reduce((sum, item) => sum + item.cpuTotalMs, 0), [delegatedMemoryKind === 'physical-footprint' ? 'sumPeakMemoryBytes' : 'sumPeakCommittedBytes']: children.reduce((sum, item) => sum + item[delegatedMemoryField], 0), memoryKind: delegatedMemoryKind ?? 'committed', ...(delegatedMemorySemantics === undefined ? {} : { memorySemantics: delegatedMemorySemantics }) })
    : Object.freeze({ status: childErrors.length ? 'ERROR' : 'UNAVAILABLE', count: children.length, reason: childErrors.length ? 'delegated-resource-error' : 'delegated-resource-unavailable' });
  const cache = summarizeCache(cacheBefore, cacheAfter, cacheAttribution, cacheExpected);
  const durationValid = validNumber(runMs) && runMs > 0;
  const delegatedAvailable = delegatedReport.status === 'OK' && children.length > 0;
  const available = [tree.status === 'OK', delegatedAvailable, cache.status === 'OK'].filter(Boolean).length;
  const componentsComplete = tree.status === 'OK' && delegatedReport.status === 'OK' && (cache.status === 'OK' || cache.status === 'NOT_APPLICABLE');
  const status = componentsComplete ? 'OK' : available === 0 ? 'UNAVAILABLE' : 'PARTIAL';
  const canCombineCpu = tree.status === 'OK' && delegatedReport.status === 'OK' && (cache.status === 'NOT_APPLICABLE' || (cache.status === 'OK' && cache.attribution === 'exclusive'));
  const cpuTotalMs = canCombineCpu ? tree.cpuTotalMs + delegatedReport.cpuTotalMs + (cache.status === 'OK' ? cache.cpuTotalMs : 0) : undefined;
  const cpuScope = canCombineCpu ? (cache.status === 'OK' ? 'owned-processes-and-exclusive-cache-window' : 'owned-processes') : undefined;
  const combinedAverage = cpuTotalMs !== undefined && durationValid ? cpuTotalMs / runMs : undefined;
  return Object.freeze({
    schema: 'rightkit.build-resources.v1',
    status,
    scope: 'build',
    processTree: processReport,
    delegatedCompilers: delegatedReport,
    sharedCompilerCache: cache,
    ...(cpuTotalMs === undefined ? {} : { cpuTotalMs, cpuScope }),
    ...(combinedAverage === undefined ? {} : { averageCpuCores: combinedAverage }),
    ...(host === undefined ? {} : { host })
  });
}
