const FIELDS = ['cpuUserMs', 'cpuKernelMs', 'cpuTotalMs', 'peakCommittedBytes', 'totalProcesses', 'activeProcesses'];

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
    if (FIELDS.some((field) => !validNumber(value[field]))) return error('resource-snapshot-invalid');
    return Object.freeze({ status: 'OK', ...Object.fromEntries(FIELDS.map((field) => [field, value[field]])), ...(value.epoch === undefined ? {} : { epoch: value.epoch }) });
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
  return Object.freeze({ status: 'OK', ...base, cpuTotalMs: after.cpuTotalMs - before.cpuTotalMs, peakCommittedBytesSinceGatewayStart: after.peakCommittedBytes });
}

/** Build a report from already-collected measurements; this function has no effects or policy. */
export function summarizeBuildResources({ processTree, delegated = [], cacheBefore, cacheAfter, cacheAttribution, cacheExpected = true, runMs, host } = {}) {
  const tree = childSnapshot(processTree);
  const processReport = Object.freeze({ ...tree, memoryKind: 'committed' });
  const children = Array.isArray(delegated) ? delegated.map(childSnapshot) : [];
  const childErrors = children.filter((item) => item.status === 'ERROR');
  const allChildrenOk = children.every((item) => item.status === 'OK');
  const delegatedReport = allChildrenOk
    ? Object.freeze({ status: 'OK', count: children.length, cpuTotalMs: children.reduce((sum, item) => sum + item.cpuTotalMs, 0), sumPeakCommittedBytes: children.reduce((sum, item) => sum + item.peakCommittedBytes, 0) })
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
