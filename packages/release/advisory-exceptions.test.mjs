import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import {
  checkAdvisoryExceptions, parseToml, lockedVersions,
  EXCEPTIONS_FILE, DENY_FILE, AUDIT_FILE, LOCK_FILE,
} from './advisory-exceptions.mjs';

const NOW = new Date('2026-10-22T12:00:00Z');

const entry = (over = {}) => ({
  id: 'RUSTSEC-2099-0001', crate: 'foo', affectedVersions: ['1.0.0'], path: 'app -> foo', dependents: ['app 0.1.0'],
  owner: 'RK Fable', rationale: 'Only used on a path that never sees untrusted input.',
  evidence: 'src/lib.rs:1', reviewBy: '2026-11-22', approvedBy: 'Astra 2026-10-22', ...over,
});
const APP = '[[package]]\nname = "app"\nversion = "0.1.0"\ndependencies = [\n "foo 1.0.0",\n "bar",\n]\n';
const LOCK = `[[package]]\nname = "foo"\nversion = "1.0.0"\n\n[[package]]\nname = "bar"\nversion = "2.0.0"\n\n${APP}`;

function files({ entries = [entry()], deny, audit, lock = LOCK, schema = 1 } = {}) {
  const denyIgnore = deny ?? entries.map(e => `  { id = "${e.id}", reason = "${e.rationale}" },`).join('\n');
  const auditIgnore = audit ?? entries.map(e => `  "${e.id}",`).join('\n');
  return {
    [EXCEPTIONS_FILE]: JSON.stringify({ schema, exceptions: entries }),
    [DENY_FILE]: `[advisories]\nversion = 2\nunmaintained = "workspace"\nyanked = "deny"\nignore = [\n${denyIgnore}\n]\n\n[licenses]\nallow = ["MIT"]\n`,
    [AUDIT_FILE]: `# c\n[advisories]\nignore = [\n${auditIgnore}\n]\n`,
    [LOCK_FILE]: lock,
  };
}
const run = (f, extra = {}) => checkAdvisoryExceptions({ root: '.', now: NOW, readFile: p => { if (!(p in f)) throw new Error('ENOENT'); return f[p]; }, ...extra });
const has = (problems, re) => problems.some(p => re.test(p));

test('a consistent set passes', () => assert.deepEqual(run(files()), []));

test('missing field fails for every required field', () => {
  for (const f of ['id', 'crate', 'path', 'owner', 'rationale', 'evidence', 'reviewBy', 'approvedBy', 'affectedVersions']) {
    const e = entry(); delete e[f];
    const problems = run(files({ entries: [e], deny: '', audit: '' }));
    assert.ok(has(problems, new RegExp(`missing field ${f}`)), f);
  }
  assert.ok(has(run(files({ entries: [entry({ owner: '  ' })] })), /missing field owner/));
  assert.ok(has(run(files({ entries: [entry({ affectedVersions: [] })] })), /missing field affectedVersions/));
});

test('reviewBy: expired, invalid and boundary', () => {
  assert.ok(has(run(files({ entries: [entry({ reviewBy: '2026-10-21' })] })), /expired/));
  assert.deepEqual(run(files({ entries: [entry({ reviewBy: '2026-10-22' })] })), []);
  assert.ok(has(run(files({ entries: [entry({ reviewBy: 'next month' })] })), /absolute YYYY-MM-DD/));
  assert.ok(has(run(files({ entries: [entry({ reviewBy: '2026-02-31' })] })), /real calendar date/));
  assert.ok(has(run(files({ entries: [entry({ reviewBy: '2026-11-22T00:00:00Z' })] })), /absolute YYYY-MM-DD/));
});

test('deny ignore without an entry, and entry without a deny ignore', () => {
  const f = files({ deny: '  { id = "RUSTSEC-2099-0001", reason = "Only used on a path that never sees untrusted input." },\n  { id = "RUSTSEC-2099-0002", reason = "x" },' });
  assert.ok(has(run(f), /ignore RUSTSEC-2099-0002 has no entry/));
  assert.ok(has(run(files({ deny: '' })), /entry RUSTSEC-2099-0001 has no ignore in deny\.toml/));
});

test('audit.toml set must equal the entries', () => {
  assert.ok(has(run(files({ audit: '  "RUSTSEC-2099-0001",\n  "RUSTSEC-2099-0002",' })), /audit\.toml: ignore RUSTSEC-2099-0002 has no entry/));
  assert.ok(has(run(files({ audit: '' })), /audit\.toml: entry RUSTSEC-2099-0001/));
  assert.ok(has(run(files({ audit: '  "RUSTSEC-2099-0001",\n  "RUSTSEC-2099-0001",' })), /duplicate ignore/));
});

test('deny reason must equal the rationale', () => {
  const f = files({ deny: '  { id = "RUSTSEC-2099-0001", reason = "different words" },' });
  assert.ok(has(run(f), /reason differs/));
  const g = files({ deny: '  "RUSTSEC-2099-0001",' });
  assert.ok(has(run(g), /reason differs/));
});

test('scope drift: affected version absent from Cargo.lock', () => {
  assert.ok(has(run(files({ entries: [entry({ affectedVersions: ['1.0.1'] })], lock: LOCK })), /foo 1\.0\.1 is not in Cargo\.lock/));
  assert.ok(has(run(files({ lock: '[[package]]\nname = "bar"\nversion = "2.0.0"\n\n' + APP })), /foo 1\.0\.0 is not in Cargo\.lock/));
});

test('scope drift: Cargo.lock holds an unlisted version of the crate', () => {
  const lock = LOCK + '\n[[package]]\nname = "foo"\nversion = "1.1.0"\n';
  assert.ok(has(run(files({ lock })), /holds foo 1\.1\.0, which the entry does not list/));
  // Listing it as unaffected is explicit and passes.
  assert.deepEqual(run(files({ lock, entries: [entry({ unaffectedVersions: ['1.1.0'] })] })), []);
  // An unaffected version that is not locked is drift too.
  assert.ok(has(run(files({ entries: [entry({ unaffectedVersions: ['0.5.0'] })] })), /foo 0\.5\.0 is not in Cargo\.lock/));
  assert.ok(has(run(files({ entries: [entry({ unaffectedVersions: ['1.0.0'] })] })), /both affected and unaffected/));
});

test('duplicate ids fail', () => {
  const problems = run(files({ entries: [entry(), entry()], deny: '  { id = "RUSTSEC-2099-0001", reason = "Only used on a path that never sees untrusted input." },', audit: '  "RUSTSEC-2099-0001",' }));
  assert.ok(has(problems, /duplicate id/));
});

test('pending approval fails unless allowed', () => {
  const e = entry({ approvedBy: 'pending Astra' });
  assert.ok(has(run(files({ entries: [e] })), /approval is pending/));
  assert.deepEqual(run(files({ entries: [e] }), { allowPending: true }), []);
});

test('deny policy keys and schema are enforced', () => {
  const f = files();
  f[DENY_FILE] = f[DENY_FILE].replace('yanked = "deny"', 'yanked = "warn"').replace('"workspace"', '"all"');
  const p = run(f);
  assert.ok(has(p, /yanked must be "deny"/));
  assert.ok(has(p, /unmaintained must be "workspace"/));
  assert.ok(has(run(files({ schema: 2 })), /schema must be 1/));
});

test('unreadable or malformed inputs fail closed', () => {
  const f = files(); delete f[AUDIT_FILE];
  assert.ok(has(run(f), /audit\.toml: cannot read or parse/));
  const g = files(); g[DENY_FILE] = '[advisories\nignore = [';
  assert.ok(has(run(g), /deny\.toml: cannot read or parse/));
});

test('toml subset parser: strings, escapes, comments, multiline arrays, inline tables', () => {
  const t = parseToml('# c\n[a.b]\nx = "q\\"s # not comment" # c\nn = 2\nok = true\nl = [\n  { id = \'i\', reason = "r" }, # c\n  "s",\n]\n[[arr]]\nk = 1\n[[arr]]\nk = 2\n');
  assert.equal(t.a.b.x, 'q"s # not comment');
  assert.equal(t.a.b.n, 2);
  assert.equal(t.a.b.ok, true);
  assert.deepEqual(t.a.b.l, [{ id: 'i', reason: 'r' }, 's']);
  assert.deepEqual(t.arr, [{ k: 1 }, { k: 2 }]);
  assert.throws(() => parseToml('a = 1\na = 2\n'));
  assert.throws(() => parseToml('a = "x\n'));
  assert.throws(() => parseToml('a = what\n'));
});

test('lockedVersions reads every package version', () => {
  const m = lockedVersions(LOCK + '\n[[package]]\nname = "foo"\nversion = "1.1.0"\n');
  assert.deepEqual([...m.get('foo')].sort(), ['1.0.0', '1.1.0']);
});


const NEW_CONSUMER = '\n[[package]]\nname = "other"\nversion = "0.2.0"\ndependencies = [\n "foo 1.0.0",\n]\n';

test('dependents: a new consumer of the affected version fails', () => {
  const p = run(files({ lock: LOCK + NEW_CONSUMER }));
  assert.ok(has(p, /other 0\.2\.0 depends on foo in Cargo\.lock but is not in dependents/));
  // Declaring it explicitly is the approved way to widen scope.
  assert.deepEqual(run(files({ lock: LOCK + NEW_CONSUMER, entries: [entry({ dependents: ['app 0.1.0', 'other 0.2.0'] })] })), []);
});

test('dependents: a removed consumer is stale and fails', () => {
  const p = run(files({ entries: [entry({ dependents: ['app 0.1.0', 'gone 1.0.0'] })] }));
  assert.ok(has(p, /dependents lists gone 1\.0\.0, which does not depend on an affected foo/));
  assert.ok(has(run(files({ lock: LOCK.replace(' "foo 1.0.0",\n', '') })), /dependents lists app 0\.1\.0, which does not depend on an affected foo/));
});

test('dependents: missing or empty fails; a consumer of an unaffected version does not count', () => {
  const e = entry(); delete e.dependents;
  assert.ok(has(run(files({ entries: [e] })), /missing field dependents/));
  assert.ok(has(run(files({ entries: [entry({ dependents: [] })] })), /missing field dependents/));
  const lock = LOCK + '\n[[package]]\nname = "foo"\nversion = "1.1.0"\n\n[[package]]\nname = "viaold"\nversion = "0.3.0"\ndependencies = [\n "foo 1.1.0",\n]\n';
  assert.deepEqual(run(files({ lock, entries: [entry({ unaffectedVersions: ['1.1.0'] })] })), []);
});

test('path: a hop that is not a dependency edge fails', () => {
  assert.ok(has(run(files({ entries: [entry({ path: 'bar -> foo' })] })), /hop "bar -> foo" is not a dependency edge/));
  assert.ok(has(run(files({ entries: [entry({ path: 'app -> bar -> foo' })] })), /hop "bar -> foo" is not a dependency edge/));
  assert.ok(has(run(files({ entries: [entry({ path: 'app -> foo 9.9.9' })] })), /hop "app -> foo 9\.9\.9" is not a dependency edge/));
  assert.ok(has(run(files({ entries: [entry({ path: 'app -> bar' })] })), /path must end at foo/));
  assert.ok(has(run(files({ entries: [entry({ path: 'foo' })] })), /at least two hops/));
  assert.deepEqual(run(files({ entries: [entry({ path: 'app 0.1.0 -> foo 1.0.0' })] })), []);
});

test('an unresolvable lock dependency fails closed', () => {
  const lock = LOCK + '\n[[package]]\nname = "x"\nversion = "1.0.0"\ndependencies = [\n "nowhere",\n]\n';
  assert.ok(has(run(files({ lock })), /does not resolve to exactly one locked package/));
});

test('the real repository passes', () => {
  const root = path.resolve(import.meta.dirname, '..', '..');
  assert.deepEqual(checkAdvisoryExceptions({ root, now: NOW }), []);
});
