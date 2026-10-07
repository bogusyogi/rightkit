import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { checkPublicationGate, publishGatedTarball } from './publication-gate.mjs';

const SHA = 'a'.repeat(40);
const run = (host) => ({ host: { os: host, arch: 'arm64' }, commit: SHA, command: 'pnpm exec vitest run', passed: 12, failed: 0, operator: 'adrdsouza', timestamp: '2026-10-07T00:00:00Z' });
const gate = (kind, ev) => checkPublicationGate({
  kind, name: '@rightkit/x', version: '1.0.0', packagePath: '/p', evidencePath: '/e.json',
  git: (args) => (args[0] === 'rev-parse' ? SHA : ''),
  readFile: () => JSON.stringify({ schema: 1, kind, name: '@rightkit/x', version: '1.0.0', commit: SHA, artifactSha256: 'b'.repeat(64), runs: [run('darwin')], ...ev }),
});

test('npm passes on macOS alone only with a recorded darwin-only host policy; crates never do', () => {
  assert.match(gate('npm', {}).missing.join('\n'), /no Windows host run receipt/);
  assert.match(gate('npm', { hostPolicy: 'darwin-only' }).missing.join('\n'), /no Windows host run receipt/);
  assert.equal(gate('npm', { hostPolicy: 'darwin-only', hostPolicyReason: 'Adrian 2026-10-07' }).ok, true);
  assert.match(gate('cargo', { hostPolicy: 'darwin-only', hostPolicyReason: 'x' }).missing.join('\n'), /no Windows host run receipt/);
});

test('publishGatedTarball refuses a non-gated tarball, never exposes the token, removes its npmrc and verifies registry bytes', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-gate-test-'));
  const tgz = path.join(dir, 'x.tgz');
  fs.writeFileSync(tgz, 'tarball-bytes');
  const sha256 = crypto.createHash('sha256').update('tarball-bytes').digest('hex');
  const sha1 = crypto.createHash('sha1').update('tarball-bytes').digest('hex');
  const ev = path.join(dir, 'e.json');
  const args = { name: '@rightkit/x', version: '1.0.0', tarball: tgz, evidencePath: ev };

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
  publishGatedTarball(args, { readToken: () => 'SECRET', spawnSync: spawn, sleep: () => {} });
  assert.equal(fs.existsSync(config), false, 'temp npmrc removed');
  assert.deepEqual(calls, ['publish', 'view']);

  const failing = (cmd, argv) => (argv[0] === 'publish' ? { status: 1, stdout: '', stderr: 'E401 token SECRET rejected' } : { status: 0, stdout: '' });
  assert.throws(() => publishGatedTarball(args, { readToken: () => 'SECRET', spawnSync: failing }),
    (error) => !error.message.includes('SECRET') && /<redacted>/.test(error.message));

  const wrong = (cmd, argv) => (argv[0] === 'publish' ? { status: 0, stdout: '' } : { status: 0, stdout: 'f'.repeat(40) });
  assert.throws(() => publishGatedTarball(args, { readToken: () => 'SECRET', spawnSync: wrong, sleep: () => {} }), /registry shasum/);
});
