import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { checkPublicationGate, publishGatedTarball, prepareNpmPublication, verifyNpmRegistryBytes } from './publication-gate.mjs';
import { HOST_UPLOAD_REFUSED } from './host-upload-refusal.mjs';
import { mintTestCapability } from './test-support/publish-capability-fixture.mjs';

// A capability for exactly this npm archive, created through the production admission validator.
const capabilityFor = (name, version, bytes) => mintTestCapability({ registry: 'npm', archives: [{ name, version, sha256: crypto.createHash('sha256').update(bytes).digest('hex') }] });

const SHA = 'a'.repeat(40);
const run = (host) => ({ host: { os: host, arch: 'arm64' }, commit: SHA, command: 'pnpm exec vitest run', features: ['default'], artifactSha256: 'b'.repeat(64), passed: 12, failed: 0, operator: 'adrdsouza', timestamp: '2026-10-07T00:00:00Z' });
const gate = (kind, ev) => checkPublicationGate({
  kind, name: '@rightkit/x', version: '1.0.0', packagePath: '/p', evidencePath: '/e.json',
  git: (args) => (args[0] === 'rev-parse' ? SHA : ''),
  readFile: () => JSON.stringify({ schema: 1, kind, name: '@rightkit/x', version: '1.0.0', commit: SHA, artifactSha256: 'b'.repeat(64), runs: [run('darwin')], ...ev }),
});

test('npm and crates pass on macOS alone only with a recorded darwin-only host policy; Swift needs only macOS anyway', () => {
  assert.match(gate('npm', {}).missing.join('\n'), /no Windows host run receipt/);
  assert.match(gate('npm', { hostPolicy: 'darwin-only' }).missing.join('\n'), /no Windows host run receipt/);
  assert.equal(gate('npm', { hostPolicy: 'darwin-only', hostPolicyReason: 'Adrian 2026-10-07' }).ok, true);
  assert.match(gate('cargo', {}).missing.join('\n'), /no Windows host run receipt/);
  assert.equal(gate('cargo', { hostPolicy: 'darwin-only', hostPolicyReason: 'Adrian 2026-10-07' }).ok, true);
});

test('publishGatedTarball refuses a non-gated tarball, never exposes the token, removes its npmrc and verifies registry bytes', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-gate-test-'));
  const tgz = path.join(dir, 'x.tgz');
  fs.writeFileSync(tgz, 'tarball-bytes');
  const sha256 = crypto.createHash('sha256').update('tarball-bytes').digest('hex');
  const sha1 = crypto.createHash('sha1').update('tarball-bytes').digest('hex');
  const ev = path.join(dir, 'e.json');
  const args = { name: '@rightkit/x', version: '1.0.0', tarball: tgz, evidencePath: ev, capability: capabilityFor('@rightkit/x', '1.0.0', 'tarball-bytes') };

  fs.writeFileSync(ev, JSON.stringify({ artifactSha256: 'c'.repeat(64) }));
  assert.throws(() => publishGatedTarball(args, { readToken: () => 'SECRET', spawnSync: () => ({ status: 0 }) }), /sha256/);

  fs.writeFileSync(ev, JSON.stringify({ artifactSha256: sha256 }));
  const calls = [];
  let config = null;
  const spawn = (cmd, argv, opts) => {
    calls.push(argv[0]);
    if (argv[0] === 'publish') {
      config = argv[argv.indexOf('--userconfig') + 1];
      assert.equal(opts.env.RIGHTKIT_NPM_TOKEN, 'SECRET');
      assert.ok(!argv.join(' ').includes('SECRET'), 'token never on the command line');
      assert.ok(!fs.readFileSync(config, 'utf8').includes('SECRET'), 'token never written to disk');
      return { status: 0, stdout: 'ok', stderr: '' };
    }
    return { status: 0, stdout: `${sha1}\n` };
  };
  const recorded = [];
  publishGatedTarball(args, { readToken: () => 'SECRET', spawnSync: spawn, sleep: () => {}, recordPublished: (entry) => recorded.push(entry) });
  assert.deepEqual(recorded, [{ registry: 'npm', name: args.name, version: args.version }], 'verified publish feeds the release manifest');
  assert.equal(fs.existsSync(config), false, 'temp npmrc removed');
  assert.deepEqual(calls, ['publish', 'view']);

  const failing = (cmd, argv) => (argv[0] === 'publish' ? { status: 1, stdout: '', stderr: 'E401 token SECRET rejected' } : { status: 0, stdout: '' });
  assert.throws(() => publishGatedTarball(args, { readToken: () => 'SECRET', spawnSync: failing }),
    (error) => !error.message.includes('SECRET') && /<redacted>/.test(error.message));

  const wrong = (cmd, argv) => (argv[0] === 'publish' ? { status: 0, stdout: '' } : { status: 0, stdout: 'f'.repeat(40) });
  assert.throws(() => publishGatedTarball(args, { readToken: () => 'SECRET', spawnSync: wrong, sleep: () => {} }), /registry shasum/);
});

test('npm helper refuses absent evidence, catches upload swap, and never records mismatch', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-npm-swap-')), tarball = path.join(dir, 'x.tgz'), evidencePath = path.join(dir, 'evidence.json');
  fs.writeFileSync(tarball, 'gated bytes');
  const args = { name: '@rightkit/x', version: '1.0.0', tarball, evidencePath, capability: capabilityFor('@rightkit/x', '1.0.0', 'gated bytes') };
  const deps = { readToken: () => 'mock-token', spawnSync: () => assert.fail('unexpected transport') };
  assert.throws(() => publishGatedTarball(args, deps), /ENOENT/);
  fs.writeFileSync(evidencePath, JSON.stringify({ artifactSha256: crypto.createHash('sha256').update('gated bytes').digest('hex') }));
  let recorded = false, registry;
  assert.throws(() => publishGatedTarball(args, {
    readToken: () => { fs.writeFileSync(tarball, 'swapped bytes'); return 'mock-token'; },
    spawnSync: (_command, argv) => {
      if (argv[0] === 'publish') { registry = crypto.createHash('sha1').update(fs.readFileSync(tarball)).digest('hex'); return { status: 0 }; }
      return { status: 0, stdout: registry };
    }, recordPublished: () => { recorded = true; },
  }), /registry shasum/);
  assert.equal(recorded, false);
  fs.rmSync(dir, { recursive: true, force: true });
});
for (const mode of ['unsealed', 'mismatch', 'swap', 'happy', 'readback error']) {
  test(`CI npm template boundary: ${mode} requires captured registry equality`, () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-npm-ci-')), tarball = path.join(dir, 'x.tgz'), evidencePath = path.join(dir, 'evidence.json');
    try {
      fs.writeFileSync(tarball, 'gated bytes');
      const sha256 = crypto.createHash('sha256').update('gated bytes').digest('hex');
      if (mode !== 'unsealed') fs.writeFileSync(evidencePath, JSON.stringify({ artifactSha256: sha256 }));
      if (mode === 'mismatch') fs.writeFileSync(tarball, 'tampered');
      const prepare = () => prepareNpmPublication({ name: '@rightkit/x', version: '1.0.0', tarball, evidencePath });
      if (['unsealed', 'mismatch'].includes(mode)) { assert.throws(prepare, /ENOENT|HASH_MISMATCH/); return; }
      const captured = prepare();
      if (mode === 'swap') fs.writeFileSync(tarball, 'swapped bytes');
      const registry = crypto.createHash('sha1').update(fs.readFileSync(tarball)).digest('hex');
      const verify = () => verifyNpmRegistryBytes(captured, { spawnSync: () => ({ status: mode === 'readback error' ? 1 : 0, stdout: registry }), verifyTimeoutMs: -1, sleep: () => assert.fail('must fail immediately') });
      if (mode === 'happy') verify(); else assert.throws(verify, /registry shasum|not visible/);
    } finally { fs.rmSync(dir, { recursive: true, force: true }); }
  });
}
test('CI npm template captures hash before publish and requires shared readback after publish', () => {
  const source = fs.readFileSync(new URL('../ci/templates/publish-npm.yml', import.meta.url), 'utf8');
  assert.ok(source.indexOf('prepareNpmPublication({') < source.indexOf('- run: npm publish'));
  assert.ok(source.indexOf('verifyNpmRegistryBytes({') > source.indexOf('- run: npm publish'));
  assert.match(source, /evidencePath:.*publication-evidence.json/);
  assert.doesNotMatch(source, /continue-on-error|if:.*always/);
});

// A direct import must not publish: every way of calling publishGatedTarball without a capability from a validated admission
// refuses before the token is looked up, an npmrc is written or npm is started.
test('publishGatedTarball refuses without a publish capability from a validated admission, before any credential or transport', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-gate-cap-'));
  try {
    const tarball = path.join(dir, 'x.tgz'), evidencePath = path.join(dir, 'e.json');
    fs.writeFileSync(tarball, 'tarball-bytes');
    const sha256 = crypto.createHash('sha256').update('tarball-bytes').digest('hex');
    fs.writeFileSync(evidencePath, JSON.stringify({ artifactSha256: sha256 }));
    const base = { name: '@rightkit/x', version: '1.0.0', tarball, evidencePath };
    const deps = { readToken: () => assert.fail('the credential must not be read'), spawnSync: () => assert.fail('nothing may be started'), recordPublished: () => assert.fail('nothing may be recorded') };
    const real = capabilityFor('@rightkit/x', '1.0.0', 'tarball-bytes');
    const refused = /host uploads are retired|publish capability/;
    for (const [label, capability] of Object.entries({
      'no capability': undefined,
      'an object literal shaped like one': { kind: 'rightkit-publish-capability', registry: 'npm' },
      'a copy of a real capability': { ...real },
      'a clone of a real capability': structuredClone(real),
      'a string': 'capability',
      'the capability of another version': capabilityFor('@rightkit/x', '2.0.0', 'tarball-bytes'),
      'the capability of another package': capabilityFor('@rightkit/y', '1.0.0', 'tarball-bytes'),
      'the capability of other bytes': capabilityFor('@rightkit/x', '1.0.0', 'other bytes'),
      'a crates capability': mintTestCapability({ registry: 'crates', archives: [{ name: '@rightkit/x', version: '1.0.0', sha256 }] }),
    })) assert.throws(() => publishGatedTarball({ ...base, capability }, deps), (error) => error.code === HOST_UPLOAD_REFUSED && refused.test(error.message), label);
    // The same call with the real capability goes through the whole publisher.
    const calls = [];
    publishGatedTarball({ ...base, capability: real }, { readToken: () => 'SECRET', spawnSync: (cmd, argv) => { calls.push(argv[0]); return argv[0] === 'publish' ? { status: 0, stdout: 'ok', stderr: '' } : { status: 0, stdout: crypto.createHash('sha1').update('tarball-bytes').digest('hex') }; }, sleep: () => {}, recordPublished: () => {} });
    assert.deepEqual(calls, ['publish', 'view']);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
