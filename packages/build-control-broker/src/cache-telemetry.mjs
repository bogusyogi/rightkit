import { arch, hostname } from 'node:os';

function totalCounts(value) {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (!value || typeof value !== 'object' || Array.isArray(value)) return 0;
  const selected = value.counts && typeof value.counts === 'object' ? value.counts : value;
  return Object.values(selected).reduce((sum, entry) => sum + (typeof entry === 'number' && Number.isFinite(entry) ? entry : 0), 0);
}

/** Convert one attributable sccache counter window into reuse truth. */
export function classifyCacheReuse(delta) {
  if (!delta || typeof delta !== 'object') return Object.freeze({ status: 'NOT_MEASURED', hits: null, misses: null, hitRate: null });
  const hits = totalCounts(delta.cache_hits);
  const misses = totalCounts(delta.cache_misses);
  const measured = hits + misses;
  const status = measured === 0 ? 'NO_COMPILATION' : misses === 0 ? 'PROVEN' : hits === 0 ? 'MISS' : 'MIXED';
  return Object.freeze({ status, hits, misses, hitRate: measured === 0 ? null : hits / measured });
}

function optionValues(argv, long, short = null) {
  const values = [];
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === long || (short && value === short)) { if (argv[index + 1]) values.push(argv[++index]); continue; }
    if (value.startsWith(`${long}=`)) values.push(value.slice(long.length + 1));
  }
  return values;
}

export function cargoCohort({ argv = [], command = null, workspace, profile = 'development', targetTriple = null, toolchain = null, compilerIdentity = null, cacheEpoch = null, platform = process.platform, hostArch = arch(), hostName = hostname() } = {}) {
  const selectedProfile = argv.includes('--release') ? 'release' : optionValues(argv, '--profile').at(-1) ?? profile;
  const features = optionValues(argv, '--features', '-F').flatMap((value) => value.split(/[ ,]+/)).filter(Boolean).sort();
  const target = optionValues(argv, '--target').at(-1) ?? targetTriple;
  return Object.freeze({
    host: Object.freeze({ name: hostName, platform, arch: hostArch }),
    workspace,
    command: command ?? `cargo ${argv.join(' ')}`.trim(),
    profile: selectedProfile,
    features: Object.freeze(features),
    allFeatures: argv.includes('--all-features'),
    defaultFeatures: !argv.includes('--no-default-features'),
    targetTriple: target,
    toolchain,
    compilerIdentity,
    cacheEpoch
  });
}

export function cacheCohortKey(cohort) {
  if (!cohort || typeof cohort !== 'object') throw new TypeError('cache cohort is required');
  return JSON.stringify(cohort);
}

/** Aggregate only exact receipt cohorts; unmeasured jobs never create a hit rate. */
export function summarizeCacheCohorts(receipts) {
  if (!Array.isArray(receipts)) throw new TypeError('receipts must be an array');
  const groups = new Map();
  for (const receipt of receipts) {
    if (!receipt?.cacheCohort) continue;
    const key = cacheCohortKey(receipt.cacheCohort);
    const group = groups.get(key) ?? { cohort: receipt.cacheCohort, jobs: 0, measuredJobs: 0, notMeasuredJobs: 0, noCompilationJobs: 0, hits: 0, misses: 0 };
    group.jobs += 1;
    const reuse = receipt.cache?.reuse;
    if (!reuse || reuse.status === 'NOT_MEASURED') group.notMeasuredJobs += 1;
    else if (reuse.status === 'NO_COMPILATION') group.noCompilationJobs += 1;
    else { group.measuredJobs += 1; group.hits += reuse.hits ?? 0; group.misses += reuse.misses ?? 0; }
    groups.set(key, group);
  }
  return Object.freeze([...groups.values()].map((group) => Object.freeze({ ...group, hitRate: group.hits + group.misses === 0 ? null : group.hits / (group.hits + group.misses) })));
}

export const cacheCountTotal = totalCounts;
