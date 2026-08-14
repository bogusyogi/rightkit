import assert from 'node:assert/strict';
import { chmod, lstat, mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { prepareUnixSocketEndpoint, rejectWorkspaceCargoConfiguration, removeStaleUnixSocket, secureUnixSocketEndpoint } from './utilities.mjs';

test('Cargo config utility rejects ancestor redirects & permits import-library flags', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-utility-'));
  try {
    await mkdir(join(root, '.cargo'));
    await writeFile(join(root, '.cargo', 'config.toml'), '[target.x86_64-pc-windows-msvc]\nrustflags = ["-C", "link-arg=Advapi32.lib"]\n');
    await assert.doesNotReject(rejectWorkspaceCargoConfiguration(join(root, 'workspace')));
    await writeFile(join(root, '.cargo', 'config.toml'), '[build]\nrustc-wrapper = "/tmp/evil"\n');
    await assert.rejects(rejectWorkspaceCargoConfiguration(join(root, 'workspace')), /cannot redirect/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('stale socket utility is conservative', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-utility-'));
  try {
    assert.equal(await removeStaleUnixSocket(join(root, 'missing.sock')), false);
    await writeFile(join(root, 'endpoint'), 'not a socket');
    await assert.rejects(removeStaleUnixSocket(join(root, 'endpoint')), /non-socket/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('Unix broker endpoint is contained, private, owned & locked after bind', async () => {
  if (process.platform === 'win32') return;
  const root = await mkdtemp(join(tmpdir(), 'rightkit-socket-'));
  const endpoint = join(root, 'broker.sock');
  let server;
  try {
    await prepareUnixSocketEndpoint({ endpoint, externalRoot: root });
    server = net.createServer();
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(endpoint, resolve); });
    await secureUnixSocketEndpoint(endpoint);
    const info = await lstat(endpoint);
    assert.equal(info.isSocket(), true);
    assert.equal(info.uid, process.getuid());
    assert.equal(info.mode & 0o777, 0o600);
    await assert.rejects(prepareUnixSocketEndpoint({ endpoint: join(root, '..', 'outside.sock'), externalRoot: root }), /contained/);
  } finally {
    await new Promise((resolve) => server ? server.close(resolve) : resolve());
    await rm(root, { recursive: true, force: true });
  }
});

test('Unix broker endpoint refuses insecure or foreign-owned parent', async () => {
  if (process.platform === 'win32') return;
  const root = await mkdtemp(join(tmpdir(), 'rightkit-socket-'));
  try {
    await chmod(root, 0o770);
    await assert.rejects(prepareUnixSocketEndpoint({ endpoint: join(root, 'broker.sock'), externalRoot: root }), /group\/world writable/);
    await chmod(root, 0o700);
    await assert.rejects(prepareUnixSocketEndpoint({ endpoint: join(root, 'nested', 'broker.sock'), externalRoot: root, uid: process.getuid() + 1 }), /owner does not match/);
  } finally { await rm(root, { recursive: true, force: true }); }
});
