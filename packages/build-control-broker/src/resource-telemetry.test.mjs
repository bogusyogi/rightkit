import assert from 'node:assert/strict';
import test from 'node:test';
import { safeResourceSnapshot, summarizeBuildResources } from './resource-telemetry.mjs';

const snap = (overrides = {}) => ({ status: 'OK', cpuUserMs: 2, cpuKernelMs: 3, cpuTotalMs: 5, peakCommittedBytes: 10, totalProcesses: 2, activeProcesses: 1, ...overrides });

test('safe snapshot catches throws & invalid readings without zeroes', () => {
  assert.equal(safeResourceSnapshot(() => { throw new Error('nope'); }).status, 'ERROR');
  assert.deepEqual(safeResourceSnapshot({ status: 'UNAVAILABLE', reason: 'native-missing' }), { status: 'UNAVAILABLE', reason: 'native-missing' });
  assert.equal(safeResourceSnapshot({ status: 'OK', cpuTotalMs: 0 }).status, 'ERROR');
});

test('safe snapshot rejects unknown memory semantics', () => {
  assert.deepEqual(safeResourceSnapshot({ ...snap(), memoryKind: 'rss' }), { status: 'ERROR', reason: 'resource-memory-kind-unsupported' });
});

test('live positive duration reports average CPU cores', () => {
  const report = summarizeBuildResources({ processTree: snap({ cpuTotalMs: 250 }), runMs: 1000, cacheExpected: false, host: 'win32' });
  assert.equal(report.status, 'OK');
  assert.equal(report.averageCpuCores, 0.25);
  assert.equal(report.sharedCompilerCache.status, 'NOT_APPLICABLE');
});

test('unknown or nonpositive duration omits average', () => {
  const report = summarizeBuildResources({ processTree: snap(), runMs: 0, cacheExpected: false });
  assert.equal('averageCpuCores' in report, false);
});

test('empty children do not make unavailable report available', () => {
  const report = summarizeBuildResources({ processTree: { status: 'UNAVAILABLE' }, delegated: [], cacheExpected: true });
  assert.equal(report.status, 'UNAVAILABLE');
});

test('whole-build CPU combines owned tree, delegated, & exclusive cache', () => {
  const report = summarizeBuildResources({
    processTree: snap({ cpuTotalMs: 100 }),
    delegated: [snap({ cpuTotalMs: 40 })],
    cacheBefore: snap({ epoch: 'e', cpuTotalMs: 10 }),
    cacheAfter: snap({ epoch: 'e', cpuTotalMs: 30 }),
    cacheAttribution: { exact: true },
    runMs: 200
  });
  assert.equal(report.cpuTotalMs, 160);
  assert.equal(report.averageCpuCores, 0.8);
  assert.equal(report.cpuScope, 'owned-processes-and-exclusive-cache-window');
});

test('shared cache attribution omits combined CPU total & average', () => {
  const report = summarizeBuildResources({ processTree: snap(), delegated: [snap()], cacheBefore: snap({ epoch: 'e', cpuTotalMs: 1 }), cacheAfter: snap({ epoch: 'e', cpuTotalMs: 2 }), cacheAttribution: {}, runMs: 100 });
  assert.equal('cpuTotalMs' in report, false);
  assert.equal('averageCpuCores' in report, false);
});

test('cache CPU delta requires monotonic counters & same epoch', () => {
  const common = { processTree: snap(), cacheExpected: true, cacheAttribution: { exact: true } };
  assert.equal(summarizeBuildResources({ ...common, cacheBefore: snap({ epoch: 'e', cpuTotalMs: 4 }), cacheAfter: snap({ epoch: 'e', cpuTotalMs: 9 }) }).sharedCompilerCache.cpuTotalMs, 5);
  assert.equal(summarizeBuildResources({ ...common, cacheBefore: snap({ epoch: 'e' }), cacheAfter: snap({ epoch: 'f' }) }).sharedCompilerCache.status, 'UNAVAILABLE');
});

test('partial child & peaks remain explicit', () => {
  const report = summarizeBuildResources({ processTree: snap(), delegated: [snap({ peakCommittedBytes: 10 }), { status: 'UNAVAILABLE', reason: 'gone' }], cacheExpected: false });
  assert.equal(report.status, 'PARTIAL');
  assert.equal(report.delegatedCompilers.status, 'UNAVAILABLE');
  assert.equal('sumPeakCommittedBytes' in report.delegatedCompilers, false);
  const cache = summarizeBuildResources({ processTree: snap(), cacheBefore: snap({ epoch: 'e', cpuTotalMs: 1 }), cacheAfter: snap({ epoch: 'e', cpuTotalMs: 2, peakCommittedBytes: 99 }), cacheAttribution: {}, }).sharedCompilerCache;
  assert.equal(cache.peakCommittedBytesSinceGatewayStart, 99);
  assert.equal(cache.attribution, 'shared');
});

test('physical-footprint snapshots keep Mac memory semantics explicit', () => {
  const report = summarizeBuildResources({
    processTree: { status: 'OK', cpuUserMs: 3, cpuKernelMs: 1, cpuTotalMs: 4, peakMemoryBytes: 8192, memoryKind: 'physical-footprint', totalProcesses: 2, activeProcesses: 0 },
    delegated: [{ status: 'OK', cpuUserMs: 2, cpuKernelMs: 1, cpuTotalMs: 3, peakMemoryBytes: 4096, memoryKind: 'physical-footprint', totalProcesses: 1, activeProcesses: 0 }],
    cacheExpected: false,
  });
  assert.equal(report.processTree.memoryKind, 'physical-footprint');
  assert.equal(report.processTree.peakMemoryBytes, 8192);
  assert.equal(report.delegatedCompilers.sumPeakMemoryBytes, 4096);
  assert.equal('sumPeakCommittedBytes' in report.delegatedCompilers, false);
});

test('physical-footprint cache deltas retain cache epoch attribution', () => {
  const base = { status: 'OK', cpuUserMs: 1, cpuKernelMs: 1, cpuTotalMs: 2, peakMemoryBytes: 10, memoryKind: 'physical-footprint', totalProcesses: 1, activeProcesses: 1, epoch: 'mac-cache' };
  const report = summarizeBuildResources({ processTree: { ...base }, cacheBefore: { ...base, cpuTotalMs: 4 }, cacheAfter: { ...base, cpuTotalMs: 9, peakMemoryBytes: 99 }, cacheAttribution: { exact: true } });
  assert.equal(report.sharedCompilerCache.memoryKind, 'physical-footprint');
  assert.equal(report.sharedCompilerCache.peakMemoryBytesSinceGatewayStart, 99);
});

test('mixed delegated memory semantics stay typed unavailable', () => {
  const report = summarizeBuildResources({
    processTree: snap(),
    delegated: [snap(), snap({ peakCommittedBytes: undefined, peakMemoryBytes: 20, memoryKind: 'physical-footprint' })],
    cacheExpected: false,
  });
  assert.equal(report.delegatedCompilers.status, 'ERROR');
  assert.equal(report.delegatedCompilers.reason, 'delegated-resource-memory-kind-mixed');
  assert.equal('sumPeakCommittedBytes' in report.delegatedCompilers, false);
  assert.equal('sumPeakMemoryBytes' in report.delegatedCompilers, false);
});
