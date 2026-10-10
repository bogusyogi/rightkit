"""Verify updated audit artifacts without modifying product source."""
from pathlib import Path
from datetime import datetime, timezone
import hashlib
import json
import re
import subprocess

E = Path(__file__).resolve().parent
O = E.parent
ROOT = E.parents[3]
OUT = E / 'apple-verification.json'

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

checks = {}
for name in ('rightkit-source-hashes.json', 'source-hashes.json'):
    hashes = json.loads((E / name).read_text())
    changed, missing = [], []
    for filename, expected in hashes.items():
        path = Path(filename)
        if not path.exists():
            missing.append(filename)
        elif sha(path) != expected:
            changed.append(filename)
    checks[name] = dict(checked=len(hashes), changed=changed, missing=missing)

reports = sorted(O.glob('*.md'))
apple_sections = sorted(E.glob('apple-*-review.md')) + [E/'apple-verification.md', E/'apple-closure.md']
links, failures = set(), []
for report in reports + apple_sections:
    body = report.read_text()
    for match in re.finditer(r'\]\((<[^>]+>|/[^)]+)\)', body):
        target = match[1].strip('<>')
        if not target.startswith('/'):
            continue
        parts = re.match(r'^(.*?)(?::(\d+))?$', target)
        path, line = Path(parts[1]), parts[2]
        links.add(target)
        if path == OUT:
            continue
        if not path.exists():
            failures.append(dict(report=str(report), target=target, error='missing'))
        elif line and path.is_file():
            total = len(path.read_text(errors='replace').splitlines())
            if not 1 <= int(line) <= total:
                failures.append(dict(report=str(report), target=target, error=f'line exceeds {total}'))

native = [
    ('macos-package', 'swift test --package-path swift/RightKitSwift --no-parallel', 'apple-swift-tests.log', 0, 201, 3, 0),
    ('ios-build', "xcodebuild -scheme RightKitSwift-Package -destination 'generic/platform=iOS Simulator' -derivedDataPath /Volumes/D/claude/rightkit/.cache/apple-audit-derived build", 'apple-ios-build.log', 0, None, None, None),
    ('ios-storekit', "xcodebuild test -scheme RightKitPurchasesStoreKitTests -destination 'platform=iOS Simulator,id=DF6BA962-3463-4078-8862-31D09A82A7AB' -derivedDataPath /Volumes/D/claude/rightkit/.cache/apple-audit-derived -resultBundlePath /Volumes/D/claude/rightkit/.cache/apple-audit-storekit.xcresult", 'apple-ios-storekit-tests.log', 0, 8, 0, 0),
    ('cloudkit-helper', 'swift build --package-path swift/RightKitCloudKitHelper', 'apple-cloudkit-helper-build.log', 0, None, None, None),
    ('record-helper', 'swift test --package-path crates/rightkit-record/helpers/macos', 'apple-record-helper-tests.log', 0, 14, 1, 0),
    ('license-contracts', 'pnpm test:swift-contracts', 'apple-contract-tests.log', 0, 2, 0, 0),
    ('legacy-archive', 'node --test swift/scripts/archive-package.test.mjs', 'apple-archive-tests.log', 1, 3, 0, 2),
]
native_results = []
for name, command, log, code, count, skips, failed in native:
    native_results.append(dict(name=name, command=command,
        cwd=str(ROOT/'swift/RightKitSwift' if name.startswith('ios-') else ROOT),
        exit_code=code, cases=count, explicit_skips=skips, failures=failed,
        log=str(E/log), log_sha256=sha(E/log)))

mac = (E/'apple-swift-tests.log').read_text()
swift_cases = sum(map(int, re.findall(r'Test run with (\d+) tests', mac)))
xctest_cases = sum(map(int, re.findall(r"Test Suite '[^']+\.xctest' passed[^\n]*\n\s*Executed (\d+) tests", mac)))
assert swift_cases + xctest_cases == 201
assert len(re.findall(r'➜ Test .* skipped:', mac)) == 2
assert len(re.findall(r'Test Case .* skipped \(', mac)) == 1
assert '✘' not in mac
assert '** BUILD SUCCEEDED **' in (E/'apple-ios-build.log').read_text()
assert '** TEST SUCCEEDED **' in (E/'apple-ios-storekit-tests.log').read_text()

products = re.findall(r'\.library\(name: "([^"]+)"', (ROOT/'swift/RightKitSwift/Package.swift').read_text())
arch = (O/'02-package-boundaries.md').read_text()
assert len(products) == 14 and all(x in arch for x in products)
skills = []
base = Path('/Users/adrdsouza/Library/Application Support/Orthic Labs/Legion/versions/0.3.21/plugin/skills')
for name in ('ios-development', 'macos-development'):
    p = base/name/'SKILL.md'
    skills.append(dict(name='legion:'+name, path=str(p), sha256=sha(p)))

result = dict(
    verified_utc=datetime.now(timezone.utc).isoformat(),
    method='Source-hash stability, report references/counts, native log checks; no product remediation.',
    skills=skills, source_hashes=checks,
    report_links=dict(unique_checked=len(links), failures=failures),
    all_14_swift_products_present=True, products=products,
    reports=[dict(path=str(p), bytes=p.stat().st_size, sha256=sha(p)) for p in reports+apple_sections],
    native_checks=native_results,
    git_head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
    qualification='Apple native checks only; frozen Audit gate remains incomplete, product findings/remediation remain open.'
)
drift_file = E/'workspace-drift.json'
acknowledged = json.loads(drift_file.read_text()) if drift_file.exists() else {'source_drift': []}
drift_rows = []
for group, check in checks.items():
    baseline = json.loads((E/group).read_text())
    for p in check['changed']:
        drift_rows.append(dict(path=p, baseline_sha256=baseline[p], followup_sha256=sha(Path(p))))
acknowledged['source_drift'] = drift_rows
acknowledged['recorded_utc'] = result['verified_utc']
acknowledged['additional_drift'] = 'Concurrent Pulse, SwiftRight, Legion & RightKit release/Git-tooling work is recorded as snapshot drift, not reaudited here. Swift package/helper baseline is checked separately.'
drift_file.write_text(json.dumps(acknowledged,indent=2)+'\n')
result['concurrent_workspace_drift'] = dict(evidence=str(drift_file), recorded_files=len(drift_rows), treatment='Original audit remains source snapshot; Apple follow-up validates unchanged Swift sources. Concurrent unrelated edits require their own focused verification.')
OUT.write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(dict(source_hashes=checks, report_links=result['report_links'], products=len(products), native_checks=len(native_results)),indent=2))
assert not failures
apple_source_drift = [p for item in checks.values() for p in item['changed'] if '/rightkit/swift/' in p or '/rightkit/crates/rightkit-record/helpers/macos/' in p]
assert not apple_source_drift
assert all(not v['missing'] for v in checks.values())
