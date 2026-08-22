#!/usr/bin/env python3
"""Persist reproducible pre-v2.7 thin receipt-corpus evidence."""

import datetime
import hashlib
import json
from pathlib import Path
import statistics
import subprocess

REPOSITORY = Path('/Volumes/D/claude')
RECEIPT_STORE = Path('/Volumes/D/.rightkit-managed/rightkit-build-control/receipts')
CONFIG_PATH = Path('/Volumes/D/.rightkit-managed/config.json')
RUNTIME_PATH = Path('/Volumes/D/.rightkit-managed/runtime-current/runtime-manifest.json')
OUTPUT = REPOSITORY / 'tools/rightkit/docs/plans/evidence/rightkit-thin-build-control-v2.7.0-receipt-corpus-analysis.json'


def read_json(path):
    with path.open(encoding='utf-8') as source:
        return json.load(source)


def command(receipt):
    return str(receipt.get('command') or '?')


def kind(receipt):
    value = command(receipt)
    if value.startswith('rustc'):
        return 'rustc'
    if value.startswith('rustdoc'):
        return 'rustdoc'
    if value.startswith('cargo'):
        return 'cargo'
    return 'other'


def percentile(values, fraction):
    return values[min(len(values) - 1, int(len(values) * fraction))] if values else None


captured_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
receipt_paths = sorted(RECEIPT_STORE.glob('*.json'))
receipts = []
for path in receipt_paths:
    try:
        receipts.append(read_json(path))
    except (OSError, ValueError):
        pass

by_kind = {name: [receipt for receipt in receipts if kind(receipt) == name] for name in ('cargo', 'rustc', 'rustdoc', 'other')}
probe_rule = 'command starts with rustc -vV, rustc --version, or rustc --print'
probes = [receipt for receipt in by_kind['rustc'] if command(receipt).startswith(('rustc -vV', 'rustc --version', 'rustc --print'))]
probe_run_ms = sorted(receipt['runMs'] for receipt in probes if isinstance(receipt.get('runMs'), (int, float)))
compiler_children = by_kind['rustc'] + by_kind['rustdoc']
children_with_cache_sampling = [receipt for receipt in compiler_children if (receipt.get('cache') or {}).get('delta') is not None]
child_queue_ms = sorted(receipt['queueMs'] for receipt in compiler_children if isinstance(receipt.get('queueMs'), (int, float)))

runtime = read_json(RUNTIME_PATH)
config = read_json(CONFIG_PATH)
sccache_tool = next((tool for tool in config.get('tools', []) if tool.get('id') == 'sccache'), None)
sccache = {'status': 'UNAVAILABLE'}
if sccache_tool:
    environment = {
        'SCCACHE_DIR': '/Volumes/D/.rightkit-managed/rightkit-build-control/global/compiler-cache',
        'SCCACHE_CACHE_SIZE': str(config.get('sccache', {}).get('maxSizeBytes', 20 * 1024 ** 3)),
    }
    if config.get('sccache', {}).get('clientSide'):
        environment['SCCACHE_CLIENT_SIDE'] = '1'
    try:
        completed = subprocess.run(
            [sccache_tool['path'], '--show-stats', '--stats-format=json'],
            capture_output=True, text=True, timeout=10, check=True, env=environment,
        )
        sample = json.loads(completed.stdout)
        identity_basis = json.dumps({
            'version': sample.get('version'), 'cacheLocation': sample.get('cache_location'),
            'basedirs': sample.get('basedirs'), 'clientSide': config.get('sccache', {}).get('clientSide', False),
        }, sort_keys=True)
        stats = sample.get('stats', {})
        sccache = {
            'status': 'SERVICE_OK',
            'measurementMode': 'client-side' if config.get('sccache', {}).get('clientSide') else 'server',
            'serverIdentitySha256': hashlib.sha256(identity_basis.encode()).hexdigest(),
            'serverRestartEpoch': None,
            'serverRestartEpochReason': 'pre-remediation sccache output exposed no restart epoch',
            'version': sample.get('version'), 'cacheLocation': sample.get('cache_location'),
            'basedirs': sample.get('basedirs'),
            'rustHits': stats.get('cache_hits', {}).get('counts', {}).get('Rust'),
            'rustMisses': stats.get('cache_misses', {}).get('counts', {}).get('Rust'),
        }
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        sccache = {'status': 'UNAVAILABLE', 'reason': str(error)}

local_revision = subprocess.run(
    ['git', '-C', str(REPOSITORY), 'rev-parse', 'HEAD'], capture_output=True, text=True, check=True,
).stdout.strip()
script_sha256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()

artifact = {
    'schema': 'rightkit.thin-build-control.receipt-corpus-analysis.v2.7',
    'capturedAt': captured_at,
    'method': 'read-only snapshot of receipt filenames followed by deterministic JSON aggregation',
    'receiptStorePath': str(RECEIPT_STORE),
    'receiptSnapshotFileCount': len(receipt_paths), 'parsedReceiptCount': len(receipts),
    'aggregationScript': str(Path(__file__).relative_to(REPOSITORY)),
    'aggregationScriptSha256': script_sha256,
    'runtimeIdentity': runtime, 'runtimeSourceRevision': runtime.get('sourceRevision'),
    'localSourceRevisionAtCapture': local_revision,
    'byKind': {name: len(values) for name, values in by_kind.items()},
    'probeSelectionRule': probe_rule,
    'probeMetrics': {
        'count': len(probes),
        'medianRunMs': round(statistics.median(probe_run_ms)) if probe_run_ms else None,
        'p90RunMs': round(percentile(probe_run_ms, 0.9)) if probe_run_ms else None,
        'cumulativeMs': round(sum(probe_run_ms)) if probe_run_ms else None,
        'formulas': {'median': 'statistics.median(sorted runMs)', 'p90': 'sorted runMs[floor(n*0.9)]', 'cumulative': 'sum(runMs)'},
    },
    'compilerChildrenWithCacheSampling': len(children_with_cache_sampling),
    'compilerChildQueueMetrics': {
        'count': len(child_queue_ms),
        'medianMs': round(statistics.median(child_queue_ms)) if child_queue_ms else None,
        'cumulativeMs': round(sum(child_queue_ms)) if child_queue_ms else None,
    },
    'nativeProbeBaseline': {
        'status': 'HISTORICAL_RANGE_ONLY', 'rangeMs': [20, 50], 'command': None,
        'reason': 'exact native command/results were not persisted before remediation; workspace policy forbids recreating them through unmanaged rustc',
    },
    'sccacheGlobalCumulativeAtCapture': sccache,
    'interpretation': {
        'receiptStore': 'pre-remediation thin receipts; compiler child cache windows overlap root windows',
        'cacheCounters': 'shared cumulative sccache counters; roots plus children must never be summed',
    },
}

OUTPUT.write_text(json.dumps(artifact, indent=2) + '\n', encoding='utf-8')
print(json.dumps({'output': str(OUTPUT), 'parsedReceiptCount': len(receipts), 'capturedAt': captured_at}))
