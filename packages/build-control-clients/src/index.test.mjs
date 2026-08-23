import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { MANAGED_RUST_ENVIRONMENT_KEYS } from '@rightkit/build-control-core';
import { adapterRegistry, brokerEndpoint, cancelManagedAgentJob, createClientRequest, createJobCommand, createManagedAgentRequest, createNestedManagedAgentRequest, createRightkitRequest, installAgentShims, INSTALLED_MANAGED_SHIMS, JOB_REQUEST_TYPES, MAX_BROKER_LINE_BYTES, loadRuntimeIdentity, sendManagedAgentRequest, shimManifest, statusManagedAgentJob, submitManagedAgentJob, ToolChangedError, watchManagedAgentJob } from './index.mjs';

const cleanProcessEnvironment = () => Object.fromEntries(Object.entries(process.env).filter(([key]) => !MANAGED_RUST_ENVIRONMENT_KEYS.includes(key.toUpperCase()) && key !== 'RIGHTKIT_BUILD_BROKER_SOCKET'));
const sourcePackagesRoot = fileURLToPath(new URL('../..', import.meta.url));
const handshake = (message) => `${JSON.stringify({ type: 'HANDSHAKE', runtime: message.runtime })}\n`;
const posixOnly = { skip: process.platform === 'win32' ? 'POSIX wrapper execution test' : false };
let endpointSequence = 0;
const endpointFor = (root, label = 'broker') => process.platform === 'win32' ? `\\\\.\\pipe\\rightkit-client-test-${process.pid}-${++endpointSequence}-${label}` : join(root, `${label}.sock`);

test('every frozen frontend produces a request-only client payload', () => {
  assert.equal(Object.keys(adapterRegistry).length, 7);
  assert.equal(createClientRequest('node-gyp', { argv: ['rebuild'], workspace: '/external/work' }).frontend, 'node-gyp');
  assert.throws(() => createClientRequest('shell', { argv: [], workspace: '/external/work' }));
});

test('all named aliases normalize into a managed-agent request', () => {
  const workspace = resolve('/external/work');
  assert.equal(createManagedAgentRequest({ argv0: 'rustc.exe', argv: ['--version'], workspace: '/external/work' }).request.frontend, 'cargo');
  assert.equal(createManagedAgentRequest({ argv0: 'c++', argv: ['x.cc'], workspace: '/external/work' }).request.frontend, 'raw-compiler');
  assert.equal(createManagedAgentRequest({ argv0: 'npm', workspace: '/external/work' }).request.frontend, 'node-pnpm');
  assert.deepEqual(createManagedAgentRequest({ argv0: 'pnpm', argv: ['--dir', 'packages/app', 'test'], workspace }).request, { frontend: 'node-pnpm', argv: ['test'], workspace: join(workspace, 'packages', 'app'), environment: {}, profile: 'MANAGED_AGENT', configuration: {} });
  assert.equal(createManagedAgentRequest({ argv0: 'npm', argv: ['--prefix=packages/app', 'test'], workspace }).request.workspace, join(workspace, 'packages', 'app'));
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

test('build priority is explicitly allowlisted instead of forwarded from ambient environment', () => {
  const installer = createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work', environment: { RIGHTKIT_BUILD_PRIORITY: 'INSTALLER', CI: '1' } });
  assert.equal(installer.priority, 'INSTALLER'); assert.deepEqual(installer.observedEnvironment, {});
  assert.equal(createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work', environment: { PATH: '/usr/bin' } }).priority, undefined);
  for (const priority of ['', 'installer', 'SYSTEM', 'INSTALLER ']) assert.throws(() => createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work', environment: { RIGHTKIT_BUILD_PRIORITY: priority } }), /RIGHTKIT_BUILD_PRIORITY/);
});

test('nested compiler forwards Cargo semantic environment but not broker controls', () => {
  const request = createManagedAgentRequest({ argv0: 'rustc', workspace: '/external/work', environment: { RIGHTKIT_PARENT_REQUEST_ID: 'parent', RIGHTKIT_PARENT_CAPABILITY_TOKEN: 'token', CARGO_PKG_VERSION: '1.2.3', OUT_DIR: '/managed/target/out', CUSTOM_BUILD_VALUE: 'yes', PATH: '/untrusted', RUSTDOC: '/untrusted/rustdoc' } });
  assert.deepEqual(request.request.environment, { CARGO_PKG_VERSION: '1.2.3', OUT_DIR: '/managed/target/out', CUSTOM_BUILD_VALUE: 'yes' });
  assert.deepEqual(request.execution, { mode: 'NESTED', parentId: 'parent', capabilityToken: 'token' });
});

test('rightkit normalizes explicit build & shim invocations into request-only identity', () => {
  const workspace = resolve('/external/work');
  const build = createRightkitRequest({ argv: ['build', 'cargo', '--', 'test'], workspace });
  assert.equal(build.entrypoint, 'rightkit'); assert.equal(build.shim, 'cargo'); assert.deepEqual(build.request.argv, ['test']);
  const shim = createRightkitRequest({ argv: ['pnpm', '--dir', 'apps/site', 'test'], workspace });
  assert.equal(shim.shim, 'pnpm'); assert.equal(shim.request.workspace, join(workspace, 'apps', 'site'));
  assert.throws(() => createRightkitRequest({ argv: ['build', 'shell', '--', 'x'] }), /usage/);
  assert.throws(() => createRightkitRequest({ argv: ['build', 'cargo', 'test'] }), /usage/);
  assert.throws(() => createRightkitRequest({ argv: [] }), /usage/);
});

test('Cargo manifest path selects its workspace for policy and config discovery', () => {
  const workspace = resolve('/external/work');
  const request = createRightkitRequest({ argv: ['cargo', 'test', '--manifest-path', 'engine/Cargo.toml'], workspace });
  assert.equal(request.request.workspace, join(workspace, 'engine'));
  assert.deepEqual(request.request.argv, ['test', '--manifest-path', join(workspace, 'engine', 'Cargo.toml')]);
  const passthrough = createRightkitRequest({ argv: ['cargo', 'test', '--', '--manifest-path', 'fixture/Cargo.toml'], workspace });
  assert.equal(passthrough.request.workspace, workspace);
  assert.deepEqual(passthrough.request.argv, ['test', '--', '--manifest-path', 'fixture/Cargo.toml']);
});

test('runtime loader fails closed on malformed manifest', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const manifestPath = join(root, 'runtime-manifest.json'); await writeFile(manifestPath, '{"version":"mutable"}\n');
  await assert.rejects(loadRuntimeIdentity({ manifestPath }), /invalid RightKit runtime manifest/); await rm(root, { recursive: true, force: true });
});

test('installs agent-only POSIX wrappers into an empty explicit directory', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const directory = join(root, 'agent-bin');
  const manifest = await installAgentShims({ directory, platform: 'linux', runtimeCurrent: '/opt/rightkit/runtime-current', nodePath: '/opt/rightkit/node' });
  assert.match(await readFile(join(directory, 'cargo'), 'utf8'), /RIGHTKIT_MANAGED_AGENT_SHIM='cargo'/);
  assert.match(await readFile(join(directory, 'cargo'), 'utf8'), /'\/opt\/rightkit\/node' '\/opt\/rightkit\/runtime-current\/build-control-clients\/src\/managed-agent\.mjs'/);
  assert.match(await readFile(join(directory, 'rightkit'), 'utf8'), /rightkit\.mjs/);
  assert.doesNotMatch(await readFile(join(directory, 'rightkit'), 'utf8'), /RIGHTKIT_MANAGED_AGENT_SHIM/);
  await assert.rejects(readFile(join(directory, 'pnpm')));
  await assert.rejects(readFile(join(directory, 'npm')));
  assert.equal(manifest.installRoot, directory);
  await installAgentShims({ directory, platform: 'linux', runtimeCurrent: '/opt/rightkit/runtime-current-v2', nodePath: '/opt/rightkit/node-v2' });
  assert.match(await readFile(join(directory, 'cargo'), 'utf8'), /runtime-current-v2/);
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
  await installAgentShims({ directory, platform: 'win32', runtimeCurrent: 'C:\\RightKit\\runtime-current', nodePath: 'C:\\RightKit\\node.exe', nativeLauncherBytes: Buffer.from([0x4d, 0x5a]) });
  const wrapper = await readFile(join(directory, 'rustdoc.cmd'), 'utf8');
  assert.match(wrapper, /RIGHTKIT_MANAGED_AGENT_SHIM=%~n0/);
  assert.match(wrapper, /C:\\RightKit\\node\.exe/);
  assert.match(wrapper, /C:\\RightKit\\runtime-current\\build-control-clients\\src\\managed-agent\.mjs/);
  const rightkit = await readFile(join(directory, 'rightkit.cmd'), 'utf8');
  assert.match(rightkit, /rightkit\.mjs/); assert.doesNotMatch(rightkit, /RIGHTKIT_MANAGED_AGENT_SHIM/);
  await rm(root, { recursive: true, force: true });
});

test('installs native Rust launchers atomically beside Windows wrappers', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const directory = join(root, 'agent-bin');
  const fakePe = Buffer.from([0x4d, 0x5a, 0x90, 0x00, 0x03, 0x00]);
  const manifest = await installAgentShims({
    directory,
    platform: 'win32',
    runtimeCurrent: 'C:\\RightKit\\runtime-current',
    nodePath: 'C:\\RightKit\\node.exe',
    nativeLauncherBytes: fakePe,
  });
  for (const name of ['cargo', 'rustc', 'rustdoc', 'rustup']) {
    assert.deepEqual(await readFile(join(directory, `${name}.exe`)), fakePe);
    assert.ok(manifest.links.includes(`${name}.exe`));
  }
  assert.ok(manifest.links.includes('rightkit.cmd'));
  await assert.rejects(
    installAgentShims({ directory, platform: 'win32', nativeLauncherBytes: Buffer.from('not-pe') }),
    /PE image/,
  );
  await rm(root, { recursive: true, force: true });
});

test('upgrades a PowerShell-written Windows ownership manifest with a UTF-8 BOM', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const directory = join(root, 'agent-bin');
  const fakePe = Buffer.from([0x4d, 0x5a]);
  await installAgentShims({ directory, platform: 'win32', runtimeCurrent: 'C:\\RightKit\\runtime-current', nodePath: 'C:\\RightKit\\node.exe', nativeLauncherBytes: fakePe });
  const manifestPath = join(directory, '.rightkit-managed-agent-shims.json');
  const manifest = await readFile(manifestPath, 'utf8');
  await writeFile(manifestPath, `\ufeff${manifest}`);
  await installAgentShims({ directory, platform: 'win32', runtimeCurrent: 'C:\\RightKit\\runtime-current', nodePath: 'C:\\RightKit\\node.exe', nativeLauncherBytes: fakePe });
  assert.equal(JSON.parse(await readFile(manifestPath, 'utf8')).platform, 'win32');
  await rm(root, { recursive: true, force: true });
});

test('native Windows launcher quotes every forwarded argument', async () => {
  const source = await readFile(new URL('./native-windows-launcher/ManagedRustLauncher.cs', import.meta.url), 'utf8');
  assert.match(source, /result\[index - 1\] = Quote\(args\[index\]\)/);
  assert.match(source, /string\.Join\(" ", Tail\(Environment\.GetCommandLineArgs\(\)\)\)/);
});

test('rightkit wrapper sends explicit request identity & fails closed without broker', posixOnly, async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const directory = join(root, 'agent-bin'); const endpoint = endpointFor(root);
  const server = net.createServer((socket) => socket.on('data', (data) => {
    const command = JSON.parse(data.toString()); socket.write(handshake({ runtime: command.runtime }));
    if (command.type === 'SUBMIT') { assert.equal(command.job.entrypoint, 'rightkit'); assert.equal(command.job.shim, 'cargo'); assert.deepEqual(command.job.request.argv, ['metadata']); return socket.end(`${JSON.stringify({ type: 'SUBMITTED', id: 'rightkit-job', state: 'QUEUED' })}\n`); }
    assert.equal(command.type, 'WATCH'); assert.equal(command.id, 'rightkit-job'); socket.end(`${JSON.stringify({ type: 'RESULT', id: command.id, exitCode: 0, status: 'SUCCEEDED' })}\n`);
  }));
  await new Promise((resolve) => server.listen(endpoint, resolve)); await installAgentShims({ directory, platform: 'linux', runtimeCurrent: sourcePackagesRoot });
  const child = spawn(join(directory, 'rightkit'), ['build', 'cargo', '--', 'metadata'], { cwd: root, env: cleanProcessEnvironment() });
  assert.equal(await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); }), 0);
  await new Promise((resolve) => server.close(resolve));
  const malformed = spawn(join(directory, 'rightkit'), ['build', 'cargo', 'test'], { cwd: root, env: cleanProcessEnvironment() });
  assert.equal(await new Promise((resolve, reject) => { malformed.once('error', reject); malformed.once('close', resolve); }), 125);
  const unavailable = spawn(join(directory, 'rightkit'), ['cargo', 'test'], { cwd: root, env: { ...cleanProcessEnvironment(), RIGHTKIT_BUILD_BROKER_SOCKET: endpointFor(root, 'missing') } });
  assert.equal(await new Promise((resolve, reject) => { unavailable.once('error', reject); unavailable.once('close', resolve); }), 125);
  await rm(root, { recursive: true, force: true });
});

test('installed cargo wrapper preserves exact shim identity for broker routing', posixOnly, async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const directory = join(root, 'agent-bin');
  const endpoint = endpointFor(root);
  const server = net.createServer((socket) => socket.on('data', (data) => {
    const command = JSON.parse(data.toString()); socket.write(handshake({ runtime: command.runtime }));
    if (command.type === 'SUBMIT') { assert.equal(command.job.shim, 'cargo'); return socket.end(`${JSON.stringify({ type: 'SUBMITTED', id: 'cargo-job', state: 'QUEUED' })}\n`); }
    assert.equal(command.type, 'WATCH'); assert.equal(command.id, 'cargo-job'); socket.end(`${JSON.stringify({ type: 'RESULT', id: command.id, exitCode: 0, status: 'SUCCEEDED' })}\n`);
  }));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  await installAgentShims({ directory, platform: 'linux', runtimeCurrent: sourcePackagesRoot });
  const child = spawn(join(directory, 'cargo'), ['metadata'], { cwd: root, env: { ...cleanProcessEnvironment(), RIGHTKIT_BUILD_BROKER_SOCKET: endpoint } });
  const exitCode = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  assert.equal(exitCode, 0);
  await new Promise((resolve) => server.close(resolve));
  await rm(root, { recursive: true, force: true });
});

test('managed agent streams typed broker events and receives exit status', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const endpoint = endpointFor(root);
  const server = net.createServer((socket) => socket.on('data', (data) => {
    const message = JSON.parse(data.toString());
    assert.equal(message.type, 'MANAGED_AGENT');
    socket.write(handshake(message));
    socket.write(`${JSON.stringify({ type: 'STDOUT', data: 'ok\\n' })}\n`);
    socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 7 })}\n`);
  }));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  const events = [];
  const result = await sendManagedAgentRequest(createManagedAgentRequest({ argv0: 'cargo', argv: ['test'], workspace: '/external/work' }), { endpoint, onEvent: (event) => events.push(event) });
  assert.equal(result.exitCode, 7);
  assert.deepEqual(events.map((event) => event.type), ['HANDSHAKE', 'STDOUT', 'RESULT']);
  await new Promise((resolve) => server.close(resolve));
  await rm(root, { recursive: true, force: true });
});

test('job API submits, watches, reads status, cancels, & preserves detached work by request id', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const endpoint = endpointFor(root); const seen = []; let submitObserverClosed = false; let resolveSubmitObserverClosed; const submitObserverClosedPromise = new Promise((resolve) => { resolveSubmitObserverClosed = resolve; });
  const message = createManagedAgentRequest({ argv0: 'cargo', argv: ['test'], workspace: '/external/work' });
  const server = net.createServer((socket) => socket.on('data', async (data) => {
    const command = JSON.parse(data.toString()); seen.push(command);
    socket.write(handshake({ runtime: command.runtime }));
    if (command.type === 'SUBMIT') { socket.once('close', () => { submitObserverClosed = true; resolveSubmitObserverClosed(); }); return socket.end(`${JSON.stringify({ type: 'SUBMITTED', id: 'job-42', state: 'QUEUED' })}\n`); }
    await submitObserverClosedPromise; assert.equal(submitObserverClosed, true);
    if (command.type === 'STATUS') return socket.end(`${JSON.stringify({ type: 'STATUS', id: command.id, state: 'RUNNING' })}\n`);
    if (command.type === 'CANCEL') return socket.end(`${JSON.stringify({ type: 'CANCEL_REQUESTED', id: command.id, state: 'REAPING' })}\n`);
    socket.write(`${JSON.stringify({ type: 'JOB_EVENT', id: command.id, event: { type: 'STDOUT', data: 'still running\\n' } })}\n`);
    socket.end(`${JSON.stringify({ type: 'RESULT', id: command.id, exitCode: 0, status: 'SUCCEEDED' })}\n`);
  }));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  assert.deepEqual(await submitManagedAgentJob(message, { endpoint }), { type: 'SUBMITTED', id: 'job-42', state: 'QUEUED' });
  assert.deepEqual(await statusManagedAgentJob('job-42', { endpoint }), { type: 'STATUS', id: 'job-42', state: 'RUNNING' });
  const events = []; assert.equal((await watchManagedAgentJob('job-42', { endpoint, onEvent: (event) => events.push(event) })).status, 'SUCCEEDED');
  assert.deepEqual(seen.map((command) => command.type), ['SUBMIT', 'STATUS', 'WATCH']);
  assert.deepEqual(await cancelManagedAgentJob('job-42', { endpoint }), { type: 'CANCEL_REQUESTED', id: 'job-42', state: 'REAPING' });
  assert.deepEqual(seen.map((command) => command.type), ['SUBMIT', 'STATUS', 'WATCH', 'CANCEL']);
  assert.equal(seen[0].job.type, 'MANAGED_AGENT'); assert.equal(seen[1].id, 'job-42'); assert.equal(events[1].type, 'JOB_EVENT');
  await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true, force: true });
});

test('cancel command carries only an explicit typed provenance source', () => {
  assert.equal(createJobCommand(JOB_REQUEST_TYPES.CANCEL, { id: 'job-1', source: 'CLIENT_SIGNAL' }).source, 'CLIENT_SIGNAL');
  assert.throws(() => createJobCommand(JOB_REQUEST_TYPES.CANCEL, { id: 'job-1', source: 'BROKER_SHUTDOWN' }), /cancellation source/);
});

test('managed entrypoints turn SIGINT into CLIENT_SIGNAL cancellation', async (t) => {
  for (const [name, script, argv, environment] of [
    ['shim', './managed-agent.mjs', ['test'], { RIGHTKIT_MANAGED_AGENT_SHIM: 'cargo' }],
    ['rightkit', './rightkit.mjs', ['cargo', 'test'], {}]
  ]) await t.test(name, async () => {
    const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const endpoint = endpointFor(root); let watchSocket; let cancelSeen = false;
    const finish = () => { if (watchSocket && cancelSeen) watchSocket.end(`${JSON.stringify({ type: 'RESULT', id: 'signal-job', exitCode: 130, status: 'CANCELLED' })}\n`); };
    const server = net.createServer((socket) => socket.on('data', (data) => {
      const command = JSON.parse(data.toString()); socket.write(handshake({ runtime: command.runtime }));
      if (command.type === 'SUBMIT') return socket.end(`${JSON.stringify({ type: 'SUBMITTED', id: 'signal-job', state: 'QUEUED' })}\n`);
      if (command.type === 'WATCH') { watchSocket = socket; finish(); return; }
      assert.equal(command.type, 'CANCEL'); assert.equal(command.id, 'signal-job'); assert.equal(command.source, 'CLIENT_SIGNAL'); cancelSeen = true;
      socket.end(`${JSON.stringify({ type: 'CANCEL_REQUESTED', id: command.id, state: 'REAPING' })}\n`); finish();
    }));
    await new Promise((resolve) => server.listen(endpoint, resolve));
    const child = spawn(process.execPath, [fileURLToPath(new URL(script, import.meta.url)), ...argv], { cwd: root, env: { ...cleanProcessEnvironment(), RIGHTKIT_BUILD_BROKER_SOCKET: endpoint, ...environment } });
    let stderr = ''; let interrupted = false;
    child.stderr.on('data', (chunk) => { stderr += chunk; if (!interrupted && stderr.includes('request signal-job')) { interrupted = true; child.kill('SIGINT'); } });
    assert.equal(await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); }), 130);
    assert.match(stderr, /request signal-job/); assert.match(stderr, /SIGINT cancelling request signal-job/); assert.match(stderr, /CANCEL_REQUESTED/);
    await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true, force: true });
  });
});

test('nested job context is explicit while legacy managed requests stay readable', () => {
  const nested = createNestedManagedAgentRequest({ parentId: 'parent-7', argv0: 'cargo', argv: ['metadata'], workspace: '/external/work' });
  assert.equal(nested.execution.mode, 'NESTED'); assert.equal(nested.execution.parentId, 'parent-7');
  const command = createJobCommand(JOB_REQUEST_TYPES.SUBMIT, { message: nested });
  assert.equal(command.job.execution.parentId, 'parent-7'); assert.equal(createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work' }).execution, undefined);
  const inherited = createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work', environment: { RIGHTKIT_PARENT_REQUEST_ID: 'parent-env', CARGO_TARGET_DIR: '/tmp/blocked' } });
  assert.deepEqual(inherited.execution, { mode: 'NESTED', parentId: 'parent-env' }); assert.deepEqual(inherited.observedEnvironment, { CARGO_TARGET_DIR: '/tmp/blocked' });
  assert.throws(() => createNestedManagedAgentRequest({ parentId: '', argv0: 'cargo', workspace: '/external/work' }), /request id/);
});

test('job API preserves a typed TOOL_CHANGED rejection', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const endpoint = endpointFor(root);
  const server = net.createServer((socket) => socket.on('data', () => socket.end(`${JSON.stringify({ type: 'TOOL_CHANGED', toolId: 'cargo-stable', registrationAction: 'REGISTER_PINNED_TOOL' })}\n`)));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  await assert.rejects(submitManagedAgentJob(createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work' }), { endpoint }), (error) => error instanceof ToolChangedError && error.code === 'TOOL_CHANGED' && error.event.registrationAction === 'REGISTER_PINNED_TOOL');
  await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true, force: true });
});

test('rightkit CLI exposes status, attach, & cancel by detached request id', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const endpoint = endpointFor(root);
  const server = net.createServer((socket) => socket.on('data', (data) => {
    const command = JSON.parse(data.toString()); assert.equal(command.type, 'STATUS'); assert.equal(command.id, 'job-cli');
    socket.write(handshake({ runtime: command.runtime })); socket.end(`${JSON.stringify({ type: 'STATUS', id: 'job-cli', state: 'QUEUED' })}\n`);
  }));
  await new Promise((resolve) => server.listen(endpoint, resolve));
  const child = spawn(process.execPath, [fileURLToPath(new URL('./rightkit.mjs', import.meta.url)), 'status', 'job-cli'], { cwd: root, env: { ...cleanProcessEnvironment(), RIGHTKIT_BUILD_BROKER_SOCKET: endpoint } });
  let stdout = ''; child.stdout.on('data', (chunk) => { stdout += chunk; });
  assert.equal(await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); }), 0);
  assert.deepEqual(JSON.parse(stdout), { type: 'STATUS', id: 'job-cli', state: 'QUEUED' });
  await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true, force: true });
});

test('managed agent fails closed when broker is unavailable', async () => {
  await assert.rejects(sendManagedAgentRequest(createManagedAgentRequest({ argv0: 'make', workspace: '/external/work' }), { endpoint: endpointFor(tmpdir(), 'missing') }), /broker unavailable/);
});

test('managed client rejects broker runtime digest mismatch', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-')); const endpoint = endpointFor(root); const message = createManagedAgentRequest({ argv0: 'cargo', workspace: '/external/work' });
  const server = net.createServer((socket) => socket.on('data', () => socket.end(`${JSON.stringify({ type: 'HANDSHAKE', runtime: { ...message.runtime, brokerDigest: 'f'.repeat(64) } })}\n`)));
  await new Promise((resolve) => server.listen(endpoint, resolve)); await assert.rejects(sendManagedAgentRequest(message, { endpoint }), /runtime digest mismatch/); await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true, force: true });
});

test('managed agent CLI prints typed broker rejection reason', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-client-'));
  const endpoint = endpointFor(root);
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
  const endpoint = endpointFor(root);
  const server = net.createServer((socket) => socket.on('data', (data) => { const message = JSON.parse(data.toString()); socket.end(`${handshake(message)}${JSON.stringify({ type: 'RESULT', exitCode: 0 })}\n${JSON.stringify({ type: 'STDOUT', data: 'late' })}\n`); }));
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
    const endpoint = endpointFor(root);
    const server = net.createServer((socket) => socket.on('data', (data) => { const message = JSON.parse(data.toString()); socket.end(name === 'oversized' ? response : `${handshake(message)}${response}`); }));
    await new Promise((resolve) => server.listen(endpoint, resolve));
    await assert.rejects(sendManagedAgentRequest(createManagedAgentRequest({ argv0: 'make', workspace: '/external/work' }), { endpoint }), expected);
    await new Promise((resolve) => server.close(resolve));
    await rm(root, { recursive: true, force: true });
  });
});
