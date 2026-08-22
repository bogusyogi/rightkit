#!/usr/bin/env python3
import json, glob, collections, statistics, os, datetime

store = '/Volumes/D/.rightkit-managed/rightkit-build-control/receipts'
files = glob.glob(os.path.join(store, '*.json'))
recs = []
for f in files:
    try: recs.append(json.load(open(f)))
    except Exception: pass

n = len(recs)
def cmd(r): return str(r.get('command') or '?')
kind = lambda r: 'rustc' if cmd(r).startswith('rustc') else 'rustdoc' if cmd(r).startswith('rustdoc') else 'cargo' if cmd(r).startswith('cargo') else 'other'

cargo = [r for r in recs if kind(r) == 'cargo']
rustc = [r for r in recs if kind(r) == 'rustc']
rdoc = [r for r in recs if kind(r) == 'rustdoc']
other = [r for r in recs if kind(r) == 'other']

probes = [r for r in rustc if cmd(r).startswith('rustc -vV') or cmd(r).startswith('rustc --version')]
probe_rms = sorted((r.get('runMs') or 0) for r in probes if isinstance(r.get('runMs'), (int,float)))

children = [r for r in recs if kind(r) in ('rustc','rustdoc') and (r.get('cache') or {}).get('delta') is not None]

print(f'total: {n}, cargo: {len(cargo)}, rustc: {len(rustc)}, rustdoc: {len(rdoc)}, other: {len(other)}')
print(f'probes: {len(probes)}, median runMs: {statistics.median(probe_rms):.0f}, p90: {probe_rms[int(len(probe_rms)*0.9)]:.0f}, sum: {sum(probe_rms)/60000:.1f}min')
print(f'children w/ cache delta: {len(children)}')

import subprocess as S
try:
    sp = S.run(
        ['/Volumes/D/.rightkit-managed/rightkit-build-control/global/tools/sccache/40e5a1e22d3af1dba322c776200f95f9c435c18bf20105783e68402f2fa4f505/sccache',
         '--show-stats', '--stats-format=json'],
        capture_output=True, text=True, timeout=10,
        env={'SCCACHE_DIR': '/Volumes/D/.rightkit-managed/rightkit-build-control/global/compiler-cache',
             'SCCACHE_CACHE_SIZE': '20G'}
    )
    sd = json.loads(sp.stdout)
    hits = sd.get('stats', {}).get('cache_hits', {}).get('counts', {}).get('Rust', 'N/A')
    misses = sd.get('stats', {}).get('cache_misses', {}).get('counts', {}).get('Rust', 'N/A')
    print(f'sccache live Rust hits={hits} misses={misses}')
except Exception as e:
    print(f'sccache stats unavailable: {e}')
    hits, misses = None, None

artifact = {
    'capturedAt': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'method': 'read-only receipt corpus analysis',
    'receiptStorePath': store,
    'totalReceipts': n,
    'byKind': {'cargo': len(cargo), 'rustc': len(rustc), 'rustdoc': len(rdoc), 'other': len(other)},
    'probeMetrics': {'count': len(probes), 'medianRunMs': round(statistics.median(probe_rms)) if probe_rms else None,
                     'p90RunMs': round(probe_rms[int(len(probe_rms)*0.9)]) if probe_rms else None,
                     'cumulativeMs': round(sum(probe_rms)) if probe_rms else None},
    'compilerChildrenWithCacheDelta': len(children),
    'sccacheGlobalCumulative': {'rustHits': hits, 'rustMisses': misses}
}

out = '/Volumes/D/claude/tools/rightkit/docs/plans/evidence/rightkit-thin-build-control-v2.7.0-receipt-corpus-analysis.json'
with open(out, 'w') as f:
    json.dump(artifact, f, indent=2)
print(f'Wrote {out}')