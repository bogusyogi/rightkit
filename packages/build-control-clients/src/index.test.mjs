import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { MANAGED_RUST_ENVIRONMENT_KEYS } from '@rightkit/build-control-core';
import { adapterRegistry, brokerEndpoint, createClientRequest, createManagedAgentRequest, createRightkitRequest, installAgentShims, INSTALLED_MANAGED_SHIMS, MAX_BROKER_LINE_BYTES, sendManagedAgentRequest, shimManifest } from './index.mjs';

const cleanProcessEnvironment = () => Object.fromEntries(Object.entries(process.env).filter(([key]) => !MANAGED_RUST_ENVIRONMENT_KEYS.includes(key.toUpperCase()) && key !== 'RIGHTKIT_BUILD_BROKER_SOCKET'));

test('every frozen frontend produces a request-only client payload', () => {
  assert.equal(Object.keys(adapterRegistry).length, 7);
  assert.equal(createClientRequest('node-gyp', { argv: ['rebuild'], workspace: '/external/work' }).frontend, 'node-gyp');
  assert.throws(() => createClientRequest('shell', { argv: [], workspace: '/external/work' }));
});

test('all named aliases normalize into a managed-agent request', () => {
  assert.equal(createManagedAgentRequest({ argv0: 'rustc.exe', argv: ['--version'], workspace: '/external/work' }).request.frontend, 'cargo');
  assert.equal(createManagedAgentRequest({ argv0: 'c++', argv: ['x.cc'], workspace: '/external/work' }).request.frontend, 'raw-compiler');
  assert.equal(createManagedAgentRequest({ argv0: 'npm', workspace: '/external/work' }).request.frontend, 'node-pnpm');
  assert.deepEqual(createManagedAgentRequest({ argv0: 'pnpm', argv: ['--dir', 'packages/app', 'test'], workspace: '/external/work' }).request, { frontend: 'node-pnpm', argv: ['test'], workspace: '/external/work/packages/app', environment: {}, profile: 'MANAGED_AGENT', configuration: {} });
  assert.equal(createManagedAgentRequest({ argv0: 'npm', argv: ['--prefix=packages/app', 'test'], workspace: '/external/work' }).request.workspace, '/external/work/packages/app');
  assert.equal(createManagedAgentRequest({ argv0: 'clang', workspace: '/external/work' }).shim, 'clang');
  assert.throws(() => createManagedAgentRequest({ argv0: 'bash', workspace: '/external/work' }), /unregistered/);
  assert.throws(() => createManagedAgentRequest({ argv0: 'cargo', argv: ['build', '--target-dir', '/tmp/escape'], workspace: '/external/work' }), /cannot redirect/);
  assert.throws(() => createManagedAgentRequest({ argv0: 'rustup', argv: ['run', 'stable', 'cargo', 'test'], workspace: '/external/work' }), /rustup run cannot launch/);
  assert.doesNotThrow(() => createManagedAgentRequest({ argv0: 'rustup', argv: ['toolchain', 'list'], workspace: '/external/work' }));
  assert.deepEqual(createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work', environment: { CARGO_TARGET_DIR: '/tmp/escape' } }).observedEnvironment, { CARGO_TARGET_DIR: '/tmp/escape' });
  assert.equal(shimManifest().links.length, 5);
  assert.deepEqual(INSTALLED_MANAGED_SHIMS, ['cargo', 'rustc', 'rustdoc', 'rustup']);
  assert.throws(() => brokerEndpoint({}), /not configured/);
});

test('ambient Rust/Cargo overrides are captured & rejected instead of silently dropped', () => {
  for (const environment of [
    { CARGO_TARGET_DIR: '/tmp/escape' },
    { CARGO_BUILD_TARGET: 'x86_64-apple-darwin' },
    { cargo_build_build_dir: '/tmp/build' },
    { RUSTC_WRAPPER: '/tmp/wrapper' },
    { RUSTFLAGS: '-C target-dir=/tmp/escape' }
  ]) assert.deepEqual(createRightkitRequest({ argv: ['cargo', 'build'], workspace: '/external/work', environment }).observedEnvironment, environment);
  assert.deepEqual(createRightkitRequest({ argv: ['cargo', 'build'], workspace: '/external/work', environment: { PATH: '/usr/bin', CI: '1' } }).observedEnvironment, {});
});

test('rightkit normalizes explicit build & shim invocations into request-only identity', () => {
  const build = createRightkitRequest({ argv: ['build', 'cargo', '--', 'test'], workspace: '/external/work' });
  assert.equal(build.entrypoint, 'rightkit'); assert.equal(build.shim, 'cargo'); assert.deepEqual(build.request.argv, ['test']);
  const shim = createRightkitRequest({ argv: ['pnpm', '--dir', 'apps/site', 'test'], workspace: '/external/work' });
  assert.equal(shim.shim, 'pnpm'); assert.equal(shim.request.workspace, '/external/work/apps/site');
  assert.throws(() => createRightkitRequest({ argv: ['build', 'shell', '--', 'x'] }), /usage/);
  assert.throws(() => createRightkitRequest({ argv: ['build', 'cargo', 'test'] }), /usage/);
  assert.throws(() => createRightkitRequest({ argv: [] }), /usage/);
});

test('Cargo manifest path selects its workspace for policy and config discovery', () => {
  const request = createRightkitRequest({ argv: ['cargo', 'test', '--manifest-path', 'engine/Cargo.toml'], workspace: '/external/work' });
  assert.equal(request.request.workspace, '/external/work/engine');
  assert.deepEqual(request.request.argv, ['test', '--manifest-path', '/external/work/engine/Cargo.toml']);
  const passthrough = createRightkitRequest({ argv: ['cargo', 'test', '--', '--manifest-path', 'fixture/Cargo.toml'], workspace: '/external/work' });
  assert.equal(passthrough.request.workspace, '/external/work');
  assert.deepEqual(passthrough.request.argv, ['test', '--', '--manifest-path', 'fixture/Cargo.toml']);
});

test('installs agent-only POSIX wrappers into an empty explicit directory', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const directory = join(root, 'agent-bin');
  const manifest = await installAgentShims({ directory, platform: 'linux', launcher: '/opt/rightkit/managed-agent.mjs', nodePath: '/opt/rightkit/node' });
  assert.match(await readFile(join(directory, 'cargo'), 'utf8'), /RIGHTKIT_MANAGED_AGENT_SHIM='cargo'/);
  assert.match(await readFile(join(directory, 'cargo'), 'utf8'), /'\/opt\/rightkit\/node' '\/opt\/rightkit\/managed-agent\.mjs'/);
  assert.match(await readFile(join(directory, 'rightkit'), 'utf8'), /rightkit\.mjs/);
  assert.doesNotMatch(await readFile(join(directory, 'rightkit'), 'utf8'), /RIGHTKIT_MANAGED_AGENT_SHIM/);
  await assert.rejects(readFile(join(directory, 'pnpm')));
  await assert.rejects(readFile(join(directory, 'npm')));
  assert.equal(manifest.installRoot, directory);
  await installAgentShims({ directory, platform: 'linux', launcher: '/opt/rightkit/managed-agent-v2.mjs', nodePath: '/opt/rightkit/node-v2' });
  assert.match(await readFile(join(directory, 'cargo'), 'utf8'), /managed-agent-v2/);
  await writeFile(join(directory, 'foreign'), 'no');
  await assert.rejects(installAgentShims({ directory, platform: 'linux' }), /foreign files/);
  await rm(root, { recursive: true, force: true });
});

test('owned upgrade removes only recorded obsolete non-Rust wrappers', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const directory = join(root, 'agent-bin');
  await installAgentShims({ directory, platform: 'linux' });
  const prior = JSON.parse(await readFile(join(directory, '.rightkit-managed-agent-shims.json'), 'utf8'));
  prior.links = [...prior.links, 'pnpm', 'npm'];
  await writeFile(join(directory, 'pnpm'), '# owned old wrapper\n'); await writeFile(join(directory, 'npm'), '# owned old wrapper\n');
  await writeFile(join(directory, '.rightkit-managed-agent-shims.json'), `${JSON.stringify(prior)}\n`);
  await installAgentShims({ directory, platform: 'linux' });
  await assert.rejects(readFile(join(directory, 'pnpm'))); await assert.rejects(readFile(join(directory, 'npm')));
  assert.match(await readFile(join(directory, 'cargo'), 'utf8'), /RIGHTKIT_MANAGED_AGENT_SHIM='cargo'/);
  await writeFile(join(directory, 'foreign'), 'no');
  await assert.rejects(installAgentShims({ directory, platform: 'linux' }), /foreign files/);
  await rm(root, { recursive: true, force: true });
});

test('installs Windows wrappers that retain each exact shim name', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const directory = join(root, 'agent-bin');
  await installAgentShims({ directory, platform: 'win32', launcher: 'C:\\RightKit\\managed-agent.mjs', nodePath: 'C:\\RightKit\\node.exe' });
  const wrapper = await readFile(join(directory, 'rustdoc.cmd'), 'utf8');
  assert.match(wrapper, /RIGHTKIT_MANAGED_AGENT_SHIM=%~n0/);
  assert.match(wrapper, /C:\\RightKit\\node\.exe/);
  assert.match(wrapper, /C:\\RightKit\\managed-agent\.mjs/);
  const rightkit = await readFile(join(directory, 'rightkit.cmd'), 'utf8');
  assert.match(rightkit, /rightkit\.mjs/); assert.doesNotMatch(rightkit, /RIGHTKIT_MANAGED_AGENT_SHIM/);
  await rm(root, { recursive: true, force: true });
});

test('rightkit wrapper sends explicit request identity & fails closed without broker', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const directory = join(root, 'agent-bin'); const endpoint = join(root, 'broker.sock');
  const server = net.createServer((socket) => socket.on('data', (data) => { const message = JSON.parse(data.toString()); assert.equal(message.entrypoint, 'rightkit'); assert.equal(message.shim, 'cargo'); assert.deepEqual(message.request.argv, ['metadata']); socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 0 })}\n`); }));
  await new Promise((resolve) => server.listen(endpoint, resolve)); await installAgentShims({ directory, platform: 'linux' });
  const child = spawn(join(directory, 'rightkit'), ['build', 'cargo', '--', 'metadata'], { cwd: root, env: cleanProcessEnvironment() });
  assert.equal(await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); }), 0);
  await new Promise((resolve) => server.close(resolve));
  const malformed = spawn(join(directory, 'rightkit'), ['build', 'cargo', 'test'], { cwd: root, env: cleanProcessEnvironment() });
  assert.equal(await new Promise((resolve, reject) => { malformed.once('error', reject); malformed.once('close', resolve); }), 125);
  const unavailable = spawn(join(directory, 'rightkit'), ['cargo', 'test'], { cwd: root, env: { ...cleanProcessEnvironment(), RIGHTKIT_BUILD_BROKER_SOCKET: join(root, 'missing.sock') } });
  assert.equal(await new Promise((resolve, reject) => { unavailable.once('error', reject); unavailable.once('close', resolve); }), 125);
  await rm(root, { recursive: true, force: true });
});

test('installed cargo wrapper preserves exact shim identity for broker routing', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const directory = join(root, 'agent-bin');
  const endpoint = join(root, 'broker.sock');
  const server = net.createServer((socket) => socket.on('data', (data) => {
    assert.equal(JSON.parse(data.toString()).shim, 'cargo');
    socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 0 })}\n`);
  }));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  await installAgentShims({ directory, platform: 'linux' });
  const child = spawn(join(directory, 'cargo'), ['metadata'], { cwd: root, env: { ...cleanProcessEnvironment(), RIGHTKIT_BUILD_BROKER_SOCKET: endpoint } });
  const exitCode = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  assert.equal(exitCode, 0);
  await new Promise((resolve) => server.close(resolve));
  await rm(root, { recursive: true, force: true });
});

test('managed agent streams typed broker events and receives exit status', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const endpoint = join(root, 'broker.sock');
  const server = net.createServer((socket) => socket.on('data', (data) => {
    const message = JSON.parse(data.toString());
    assert.equal(message.type, 'MANAGED_AGENT');
    socket.write(`${JSON.stringify({ type: 'STDOUT', data: 'ok\\n' })}\n`);
    socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 7 })}\n`);
  }));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  const events = [];
  const result = await sendManagedAgentRequest(createManagedAgentRequest({ argv0: 'cargo', argv: ['test'], workspace: '/external/work' }), { endpoint, onEvent: (event) => events.push(event) });
  assert.equal(result.exitCode, 7);
  assert.deepEqual(events.map((event) => event.type), ['STDOUT', 'RESULT']);
  await new Promise((resolve) => server.close(resolve));
  await rm(root, { recursive: true, force: true });
});

test('managed agent fails closed when broker is unavailable', async () => {
  await assert.rejects(sendManagedAgentRequest(createManagedAgentRequest({ argv0: 'make', workspace: '/external/work' }), { endpoint: join(tmpdir(), 'rightkit-missing.sock') }), /broker unavailable/);
});

test('managed agent CLI prints typed broker rejection reason', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const endpoint = join(root, 'broker.sock');
  const server = net.createServer((socket) => socket.on('data', () => socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 125, status: 'REJECTED', error: 'policy rejected request' })}\n`)));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  const child = spawn(process.execPath, [fileURLToPath(new URL('./managed-agent.mjs', import.meta.url)), '--version'], { cwd: root, env: { ...cleanProcessEnvironment(), RIGHTKIT_BUILD_BROKER_SOCKET: endpoint, RIGHTKIT_MANAGED_AGENT_SHIM: 'cargo' } });
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const exitCode = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  assert.equal(exitCode, 125);
  assert.match(stderr, /policy rejected request/);
  await new Promise((resolve) => server.close(resolve));
  await rm(root, { recursive: true, force: true });
});

test('managed agent rejects post-result broker protocol data', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const endpoint = join(root, 'broker.sock');
  const server = net.createServer((socket) => socket.on('data', () => socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 0 })}\n${JSON.stringify({ type: 'STDOUT', data: 'late' })}\n`)));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  await assert.rejects(sendManagedAgentRequest(createManagedAgentRequest({ argv0: 'make', workspace: '/external/work' }), { endpoint }), /after terminal result/);
  await new Promise((resolve) => server.close(resolve));
  await rm(root, { recursive: true, force: true });
});

test('managed agent rejects oversized lines and out-of-range exit status', async (t) => {
  for (const [name, response, expected] of [
    ['oversized', `${JSON.stringify({ type: 'STDOUT', data: 'x'.repeat(MAX_BROKER_LINE_BYTES) })}\n`, /line limit/],
    ['exit-status', `${JSON.stringify({ type: 'RESULT', exitCode: 256 })}\n`, /invalid exit status/]
  ]) await t.test(name, async () => {
    const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
    const endpoint = join(root, 'broker.sock');
    const server = net.createServer((socket) => socket.on('data', () => socket.end(response)));
    await new Promise((resolve) => server.listen(endpoint, resolve));
    await assert.rejects(sendManagedAgentRequest(createManagedAgentRequest({ argv0: 'make', workspace: '/external/work' }), { endpoint }), expected);
    await new Promise((resolve) => server.close(resolve));
    await rm(root, { recursive: true, force: true });
  });
});
