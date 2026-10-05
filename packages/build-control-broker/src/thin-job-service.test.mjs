import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, stat, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import test from 'node:test';
import { BUILD_CONTROL_PROTOCOL, SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';
import { brokerEndpointForPlatform, CachedWorkspaceResolver, ThinJobService, createThinJobServer, resolveCargoWorkspace, resolveRepositoryRoot } from './thin-job-service.mjs';
import { ThinState } from './thin-state.mjs';
import { beginCutoverDrain, endCutoverDrain } from './cutover-shared.mjs';

const digest = 'a'.repeat(64);
const message = (workspace, argv, extra = {}) => ({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, shim: 'cargo', observedEnvironment: {}, request: { frontend: 'cargo', argv, workspace, environment: {}, profile: 'MANAGED_AGENT', configuration: {} }, ...extra });
const compilerMessage = (workspace, argv, extra = {}) => ({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, shim: 'rustc', observedEnvironment: {}, ...extra, request: { frontend: 'raw-compiler', argv, workspace, environment: {}, profile: 'MANAGED_AGENT', configuration: {}, ...(extra.request ?? {}) } });
const command = (type, value) => ({ type, protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, ...(type === 'SUBMIT' ? { job: value } : { id: value }) });

class Child extends EventEmitter {
  constructor(pid, payload) { super(); this.pid = pid; this.payload = payload; this.stdout = new EventEmitter(); this.stderr = new EventEmitter(); this.connected = true; }
}

async function fixture({ externalCommands = [], cache = null, cacheEnvironment = {}, targetTriples = null, gc = null, platform = 'linux', cargoPath = '/managed/cargo', readResources = null, readHostResources = null, now = null, nativeOpenSsl = null } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-job-')); const repository = join(root, 'repository'); const workspace = join(repository, 'workspace'); const other = join(root, 'other');
  await Promise.all([mkdir(join(repository, '.git'), { recursive: true }), mkdir(join(workspace, '.cargo'), { recursive: true }), mkdir(other)]);
  await writeFile(join(workspace, '.cargo', 'config.toml'), '[net]\ngit-fetch-with-cli = true\n');
  const state = await ThinState.open({ externalRoot: root }); const children = []; let pid = 300;
  if (gc) state.gc = gc;
  const tools = new Map([['cargo', { id: 'cargo', path: cargoPath, sha256: digest }], ['rustc', { id: 'rustc', path: '/managed/rustc', sha256: digest }], ['rustdoc', { id: 'rustdoc', path: '/managed/rustdoc', sha256: digest }], ['rustup', { id: 'rustup', path: '/managed/rustup', sha256: digest }], ['cargo-outdated', { id: 'cargo-outdated', path: '/managed/cargo-outdated', sha256: digest }]]);
  const registry = { verify: async (id) => { const tool = tools.get(id); if (!tool) throw new Error(`missing ${id}`); return tool; } };
  const logger = { events: [], flushes: 0, append(event) { this.events.push(event); return Promise.resolve(event); }, flush() { this.flushes += 1; return Promise.resolve(); }, close: async () => {} };
  const selectedCache = cache ?? { prepare: async ({ layout }) => ({ mode: 'sccache', status: 'OK', environment: { CARGO_HOME: state.paths.cargoHome, CARGO_TARGET_DIR: layout.target, CARGO_BUILD_TARGET_DIR: layout.target, RUSTC_WRAPPER: '/managed/sccache', TMPDIR: layout.temp, TMP: layout.temp, TEMP: layout.temp, ...cacheEnvironment }, before: {} }), finish: async () => ({ mode: 'sccache', status: 'OK', delta: { compile_requests: 1 } }) };
  const config = { broker: { externalRoot: { path: root } }, unixSocket: join(root, 'broker.sock'), windowsPipe: '\\\\.\\pipe\\rightkit-test', shims: { cargo: 'cargo', rustc: 'rustc', rustdoc: 'rustdoc', rustup: 'rustup' }, tools: [], externalCommands, defaults: { targetTriple: 'aarch64-apple-darwin', ...(targetTriples ? { targetTriples } : {}), buildProfile: 'development', cargoBuildJobs: 4, toolchain: 'stable-aarch64-apple-darwin' }, sccache: { mode: 'managed', maxSizeBytes: 1024 }, bounds: { buildConcurrency: 1, controlConcurrency: 4, analysisConcurrency: 2, workspaceWriteConcurrency: 4, processGraceMs: 10, heartbeatIntervalMs: 100, outputBufferBytes: 64 * 1024, outputHeadBytes: 1024, outputTailBytes: 1024, freeFloorBytes: 0 } };
  const service = await ThinJobService.create({ config, state, cache: selectedCache, registry, logger, singleton: { release: async () => true }, platform, ...(nativeOpenSsl ? { nativeOpenSsl } : {}), workspaceResolver: async (cwd) => ({ executionCwd: cwd, workspaceRoot: cwd }), spawn: (_command, argv, options) => { const payload = JSON.parse(Buffer.from(argv[1], 'base64url').toString('utf8')); const child = new Child(++pid, payload); child.environment = options.env; children.push(child); return child; }, processIdentity: async (childPid) => ({ status: 'OK', identity: `identity-${childPid}` }), processOwnership: (child) => ({ pid: child.pid, processGroupId: child.pid, platform: 'posix', identity: `identity-${child.pid}` }), startProcess: async (child) => { if (child.payload.argv.includes('--version')) queueMicrotask(() => child.emit('close', 0, null)); return { pid: child.pid }; }, terminateProcess: async (child) => { if (!child.closed) { child.closed = true; queueMicrotask(() => child.emit('close', null, 'SIGTERM')); } return { reaped: true, reason: 'gone' }; }, statfs: async () => ({ bavail: 10_000, bsize: 4096 }), ...(readResources ? { readResources } : {}), ...(readHostResources ? { readHostResources } : {}), ...(now ? { now } : {}) });
  return { root, repository, workspace, other, state, service, children, logger, close: async () => { await service.shutdown(); await rm(root, { recursive: true, force: true }); } };
}

async function call(service, value) { const events = []; const result = await service.dispatch(value, (event) => { events.push(event); return true; }); return { result, events }; }
async function waitFor(service, id, states) { let last; for (let attempt = 0; attempt < 100; attempt += 1) { const { result } = await call(service, command('STATUS', id)); last = result; if (states.includes(result.state)) return result; if (['SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT'].includes(result.state)) break; await new Promise((resolve) => setTimeout(resolve, 2)); } throw new Error(`job ${id} did not reach ${states}: ${JSON.stringify(last)}`); }

test('Cargo iteration cache mode follows profile flags before executable arguments', async () => {
  const prepared = [];
  const cache = {
    prepare: async (options) => { prepared.push(options); return { mode: 'sccache', status: 'OK', environment: {}, before: {} }; },
    finish: async () => ({ mode: 'sccache', status: 'OK', delta: {} })
  };
  const item = await fixture({ cache });
  try {
    for (const [argv, expected] of [
      [['build', '--profile', 'release-iterate'], 'release-iterate'],
      [['build', '--profile=release-iterate'], 'release-iterate'],
      [['build', '--release'], 'release'],
      [['test', '--', '--profile=release-iterate'], 'release']
    ]) {
      const iterationRequest = (await call(item.service, command('SUBMIT', message(item.workspace, argv)))).result;
      await waitFor(item.service, iterationRequest.id, ['RUNNING']);
      assert.equal(prepared.at(-1).profile, expected);
      item.children.at(-1).emit('close', 0, null);
      await waitFor(item.service, iterationRequest.id, ['SUCCEEDED']);
    }
  } finally { await item.close(); }
});

test('broker readiness does not wait for retired-generation sizing', async () => {
  let release; let markGcStarted;
  const gate = new Promise((resolve) => { release = resolve; });
  const gcStarted = new Promise((resolve) => { markGcStarted = resolve; });
  const creating = fixture({ gc: async () => { markGcStarted(); return gate; } });
  // Wait until detached maintenance is definitely blocked on GC. Correct
  // startup returns the service in the same event-loop turn; an accidental
  // await leaves creation blocked until `gate` is released. This proves causal
  // ordering without a wall-clock threshold that flakes under filesystem load.
  await gcStarted;
  const outcome = await Promise.race([
    creating.then((item) => ({ item })),
    new Promise((resolve) => setImmediate(() => resolve({ item: null })))
  ]);
  assert.ok(outcome.item, 'broker creation waited for GC');
  const item = outcome.item;
  try {
    assert.equal(item.logger.events.some((event) => event.type === 'BROKER_READY'), true);
    release({ deleted: [], totalBytes: 0, capBytes: null });
  } finally { await item.close(); }
});

test('termination signal is durably logged with active request provenance', async () => {
  const item = await fixture();
  try {
    const { result } = await call(item.service, command('SUBMIT', message(item.workspace, ['check'])));
    await waitFor(item.service, result.id, ['RUNNING']);
    const recorded = await item.service.recordTerminationSignal('SIGINT', { pid: 41, ppid: 17, platform: 'win32' });
    assert.deepEqual(recorded, { signal: 'SIGINT', activeRequestIds: [result.id] });
    assert.equal(item.logger.flushes, 1);
    assert.deepEqual(item.logger.events.at(-1), { type: 'BROKER_TERMINATION_SIGNAL', signal: 'SIGINT', pid: 41, ppid: 17, platform: 'win32', activeRequestIds: [result.id] });
    await call(item.service, command('CANCEL', result.id));
    await waitFor(item.service, result.id, ['CANCELLED']);
  } finally { await item.close(); }
});

test('disconnected watcher EPIPE detaches without crashing broker', async () => {
  let connect; let finish;
  const completed = new Promise((resolve) => { finish = resolve; });
  const service = {
    async dispatch(_message, emit) {
      assert.equal(emit({ type: 'HANDSHAKE' }), true);
      assert.equal(emit({ type: 'RESULT', exitCode: 0 }), false);
      finish();
    }
  };
  createThinJobServer({
    service,
    endpoint: '/test/rightkit.sock',
    netModule: { createServer(handler) { connect = handler; return {}; } }
  });
  const socket = new EventEmitter();
  socket.destroyed = false; socket.writableLength = 0;
  socket.write = () => { socket.emit('error', Object.assign(new Error('broken pipe'), { code: 'EPIPE' })); return false; };
  socket.end = () => {};
  connect(socket);
  socket.emit('data', Buffer.from('{}\n'));
  await completed;
});

test('workspace discovery is bounded, deduplicated, cached & reports hit timing', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-workspace-resolver-'));
  let calls = 0; let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const resolver = new CachedWorkspaceResolver({ concurrency: 1, resolver: async (cwd, options) => { calls += 1; assert.equal(options.timeoutMs, 30_000); await gate; return { executionCwd: cwd, workspaceRoot: cwd, workspaceDiscoveryMs: 8 }; } });
  try {
    const first = resolver.resolve(root); const duplicate = resolver.resolve(root);
    for (let attempt = 0; attempt < 20 && calls === 0; attempt += 1) await new Promise((resolve) => setTimeout(resolve, 1));
    assert.equal(calls, 1); assert.equal(resolver.stats.inflight, 1);
    release();
    const initial = await Promise.all([first, duplicate]);
    assert.ok(initial.some((value) => value.workspaceDiscoveryCache === 'MISS'));
    assert.ok(initial.every((value) => ['MISS', 'HIT'].includes(value.workspaceDiscoveryCache)));
    const cached = await resolver.resolve(root);
    assert.equal(cached.workspaceDiscoveryCache, 'HIT'); assert.equal(cached.workspaceDiscoveryMs, 0);
    assert.equal(resolver.stats.lookup, 3); assert.equal(resolver.stats.hit + resolver.stats.miss, 3);
    assert.deepEqual({ error: resolver.stats.error, active: resolver.stats.active, queued: resolver.stats.queued, inflight: resolver.stats.inflight }, { error: 0, active: 0, queued: 0, inflight: 0 });
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('workspace discovery reports timeout & Cargo failure explicitly', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-workspace-errors-'));
  try {
    await assert.rejects(
      resolveCargoWorkspace(root, { cargoPath: '/managed/cargo', timeoutMs: 17_000, run: async () => { throw Object.assign(new Error('Command failed'), { killed: true, signal: 'SIGTERM' }); } }),
      (error) => error.code === 'RK_WORKSPACE_DISCOVERY_TIMEOUT' && error.timeoutMs === 17_000 && error.message.includes(root),
    );
    await assert.rejects(
      resolveCargoWorkspace(root, { cargoPath: '/managed/cargo', run: async () => { throw Object.assign(new Error('Command failed'), { stderr: 'could not find Cargo.toml' }); } }),
      (error) => error.code === 'RK_WORKSPACE_DISCOVERY_FAILED' && error.message.includes('could not find Cargo.toml') && error.message.includes(root),
    );
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('two BUILD roots serialize globally while CONTROL completes', async () => {
  const item = await fixture();
  try {
    const first = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result;
    const second = (await call(item.service, command('SUBMIT', message(item.other, ['test'])))).result;
    await waitFor(item.service, first.id, ['RUNNING']);
    assert.equal((await call(item.service, command('STATUS', second.id))).result.state, 'QUEUED');
    const control = (await call(item.service, command('SUBMIT', message(item.workspace, ['--version'])))).result;
    assert.equal((await call(item.service, command('WATCH', control.id))).result.status, 'SUCCEEDED');
    item.children.find((child) => child.payload.argv.includes('check')).emit('close', 0, null);
    await waitFor(item.service, second.id, ['RUNNING']);
    item.children.find((child) => child.payload.argv.includes('test')).emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', second.id))).result.status, 'SUCCEEDED');
  } finally { await item.close(); }
});

test('macOS jobs override stale cache loader state with broker-owned Rust toolchain library path', async (context) => {
  if (process.platform !== 'darwin') return context.skip('macOS-only dynamic loader contract');
  const item = await fixture({ cacheEnvironment: { DYLD_FALLBACK_LIBRARY_PATH: '/stale/toolchain/lib' }, platform: 'darwin' });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['--version'])))).result;
    assert.equal((await call(item.service, command('WATCH', submitted.id))).result.status, 'SUCCEEDED');
    assert.equal(item.children.at(-1).environment.DYLD_FALLBACK_LIBRARY_PATH, join(dirname('/managed/cargo'), '..', 'lib'));
  } finally { await item.close(); }
});

test('external Cargo tools inherit validated broker endpoint with broker-owned precedence', async () => {
  const unixSocket = '/validated/rightkit.sock'; const windowsPipe = '\\\\.\\pipe\\rightkit-validated';
  assert.equal(brokerEndpointForPlatform({ unixSocket, windowsPipe }, 'darwin'), unixSocket);
  assert.equal(brokerEndpointForPlatform({ unixSocket, windowsPipe }, 'win32'), windowsPipe);
  const item = await fixture({
    platform: 'darwin',
    externalCommands: [{ shim: 'cargo', command: 'nextest', toolId: 'cargo-outdated', digest, lane: 'BUILD' }],
    cacheEnvironment: { rightkit_build_broker_socket: '/stale/rightkit.sock' }
  });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['nextest', 'run'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const child = item.children.at(-1);
    assert.equal(child.environment.RIGHTKIT_BUILD_BROKER_SOCKET, join(item.root, 'broker.sock'));
    assert.equal(child.environment.rightkit_build_broker_socket, undefined);
    child.emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', submitted.id))).result.status, 'SUCCEEDED');
  } finally { await item.close(); }
});

test('jobs select the rustup toolchain registered by Cargo path', async () => {
  const toolchain = 'stable-x86_64-pc-windows-msvc';
  const item = await fixture({ cargoPath: join('/managed', 'toolchains', toolchain, 'bin', 'cargo') });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['--version'])))).result;
    assert.equal((await call(item.service, command('WATCH', submitted.id))).result.status, 'SUCCEEDED');
    assert.equal(item.children.at(-1).environment.RUSTUP_TOOLCHAIN, toolchain);
  } finally { await item.close(); }
});

test('Clippy finds registered Cargo toolchain with managed compiler shims first', async () => {
  const bin = join('/managed', 'toolchains', '1.98.0-x86_64-pc-windows-msvc', 'bin');
  const item = await fixture({ cargoPath: join(bin, 'cargo'), cacheEnvironment: { PATH: '/stale/toolchain/bin' } });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['clippy'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const child = item.children.at(-1);
    assert.deepEqual(child.environment.PATH.split(delimiter).slice(0, 2), [join(item.root, 'agent-bin'), bin]);
    assert.equal(child.environment.RUSTUP_TOOLCHAIN, '1.98.0-x86_64-pc-windows-msvc');
    assert.equal(child.environment.RUSTC, '/managed/rustc');
    assert.equal(child.environment.RUSTC_WRAPPER, '/managed/sccache');
    child.emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', submitted.id))).result.status, 'SUCCEEDED');
  } finally { await item.close(); }
});

test('registered target installation uses the Rustup home that owns registered Cargo', async () => {
  const rustupHome = join('/managed', 'rustup-home');
  const toolchain = 'stable-aarch64-apple-darwin';
  const target = 'aarch64-apple-ios';
  const item = await fixture({ cargoPath: join(rustupHome, 'toolchains', toolchain, 'bin', 'cargo'), targetTriples: ['aarch64-apple-darwin', target] });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['target', 'add', target], { shim: 'rustup' })))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const child = item.children.at(-1);
    assert.equal(child.environment.RUSTUP_HOME, rustupHome);
    assert.equal(child.environment.RUSTUP_TOOLCHAIN, toolchain);
    assert.deepEqual(child.payload.argv, ['target', 'add', target]);
    queueMicrotask(() => child.emit('close', 0, null));
    assert.equal((await call(item.service, command('WATCH', submitted.id))).result.status, 'SUCCEEDED');
  } finally { await item.close(); }
});

test('unregistered target installation is rejected before execution', async () => {
  const item = await fixture({ targetTriples: ['aarch64-apple-darwin'] });
  try {
    const rejected = (await call(item.service, command('SUBMIT', message(item.workspace, ['target', 'add', 'aarch64-apple-ios'], { shim: 'rustup' })))).result;
    assert.equal(rejected.status, 'REJECTED');
    assert.equal(rejected.code, 'RK_TARGET_NOT_REGISTERED');
    assert.equal(item.children.length, 0);
  } finally { await item.close(); }
});

test('registered explicit Cargo target overrides host default within persistent workspace generation', async () => {
  const item = await fixture({ targetTriples: ['aarch64-apple-darwin', 'x86_64-apple-darwin'] });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['build', '--target=x86_64-apple-darwin'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const child = item.children.at(-1);
    assert.deepEqual(child.payload.argv.slice(0, 3), ['build', '--target=x86_64-apple-darwin', '--target-dir']);
    assert.ok(child.payload.argv[3].endsWith(join('g', '001', 'target')));
    child.emit('close', 0, null);
    const result = (await call(item.service, command('WATCH', submitted.id))).result;
    assert.equal(result.status, 'SUCCEEDED');
    assert.equal(result.receipt.targetTriple, 'x86_64-apple-darwin');

    const rejected = (await call(item.service, command('SUBMIT', message(item.workspace, ['build', '--target', 'wasm32-unknown-unknown'])))).result;
    assert.equal(rejected.status, 'REJECTED');
    assert.equal(rejected.code, 'RK_TARGET_NOT_REGISTERED');
  } finally { await item.close(); }
});

test('allows structurally parsed harmless Cargo config but rejects output redirection', async () => {
  const item = await fixture();
  try {
    assert.equal((await call(item.service, command('SUBMIT', message(item.workspace, ['--version'])))).result.type, 'SUBMITTED');
    await writeFile(join(item.workspace, '.cargo', 'config.toml'), '[build]\ntarget-dir = "local-target"\n');
    const rejected = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result;
    assert.equal(rejected.status, 'REJECTED'); assert.equal(rejected.code, 'RK_CARGO_CONFIG_UNSUPPORTED');
  } finally { await item.close(); }
});

test('same workspace consecutive BUILD jobs reuse current generation', async () => {
  const item = await fixture();
  try {
    const run = async (argv) => { const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, argv)))).result; await waitFor(item.service, submitted.id, ['RUNNING']); item.children.at(-1).emit('close', 0, null); return (await call(item.service, command('WATCH', submitted.id))).result.receipt; };
    assert.equal((await run(['check'])).generation, 1);
    assert.equal((await run(['test'])).generation, 1);
    assert.equal((await item.state.workspace(item.workspace)).state.currentGeneration, 1);
  } finally { await item.close(); }
});

test('cache preparation receives broker-owned repository, workspace & active-generation roots', async () => {
  const prepared = [];
  const cache = { prepare: async (input) => { prepared.push(input); return { mode: 'direct', status: 'DISABLED', environment: {}, before: null }; }, finish: async () => ({ mode: 'direct', status: 'DISABLED', delta: null }) };
  const item = await fixture({ cache });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    item.children.at(-1).emit('close', 0, null);
    await call(item.service, command('WATCH', submitted.id));
    assert.equal(prepared.length, 1);
    assert.equal(prepared[0].repositoryRoot, item.repository);
    assert.equal(prepared[0].workspaceRoot, item.repository);
    assert.match(prepared[0].layout.root, /g[\\/]001$/);
  } finally { await item.close(); }
});

test('repository marker scopes independent Cargo workspaces to one cache root', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-repository-root-'));
  const repository = join(root, 'product');
  const engine = join(repository, 'engine');
  const tray = join(repository, 'apps', 'tray');
  try {
    await Promise.all([mkdir(engine, { recursive: true }), mkdir(tray, { recursive: true })]);
    await writeFile(join(repository, '.rightkit-local-development.json'), '{}\n');
    assert.equal(await resolveRepositoryRoot(engine), repository);
    assert.equal(await resolveRepositoryRoot(tray), repository);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('Cargo roots own exact pinned compilers directly while raw rustc remains brokered', async () => {
  const item = await fixture({ cache: { prepare: async ({ layout }) => ({ mode: 'sccache', status: 'OK', environment: { CARGO_HOME: '/managed/cargo-home', CARGO_TARGET_DIR: layout.target, CARGO_BUILD_TARGET_DIR: layout.target, RUSTC_WRAPPER: '/managed/sccache', TMPDIR: layout.temp, TMP: layout.temp, TEMP: layout.temp }, before: {} }), finish: async () => ({ mode: 'sccache', status: 'SERVICE_OK', delta: { compile_requests: 0, requests_executed: 7 } }) } });
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    const cargo = item.children.at(-1);
    assert.equal(cargo.environment.RUSTC, '/managed/rustc');
    assert.equal(cargo.environment.RUSTDOC, '/managed/rustdoc');
    assert.doesNotMatch(cargo.environment.RUSTC, /agent-bin/);
    cargo.emit('close', 0, null);
    const terminal = (await call(item.service, command('WATCH', root.id))).result;
    assert.equal(terminal.receipt.compilerBrokerRoundTrips, 0);
    assert.equal(terminal.receipt.compilerProcessCount, 7);
    assert.equal(terminal.receipt.repository, item.repository); assert.equal(terminal.receipt.profile, 'development');
    assert.deepEqual(terminal.receipt.compilerIdentity.rustc, { id: 'rustc', path: '/managed/rustc', sha256: digest });
    assert.equal(terminal.receipt.ownership.pid, cargo.pid); assert.equal(terminal.receipt.ownership.processGroupId, cargo.pid);

    const raw = (await call(item.service, command('SUBMIT', { ...compilerMessage(item.workspace, ['--version']), execution: undefined }))).result;
    await waitFor(item.service, raw.id, ['RUNNING']);
    assert.equal(item.children.at(-1).payload.command, '/managed/rustc');
    item.children.at(-1).emit('close', 0, null);
    const rawTerminal = (await call(item.service, command('WATCH', raw.id))).result;
    assert.equal(rawTerminal.status, 'SUCCEEDED'); assert.equal(rawTerminal.receipt.compilerProcessCount, 1);
    assert.deepEqual(rawTerminal.receipt.compilerIdentity.rustc, { id: 'rustc', path: '/managed/rustc', sha256: digest });
  } finally { await item.close(); }
});

test('release Cargo receipt uses effective argv profile instead of configured default', async () => {
  const item = await fixture();
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['build', '--release'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    item.children.at(-1).emit('close', 0, null);
    const terminal = (await call(item.service, command('WATCH', root.id))).result;
    assert.equal(terminal.receipt.profile, 'release');
    assert.equal(terminal.receipt.cacheCohort.profile, 'release');
  } finally { await item.close(); }
});

test('nested heavy work rejects under active analysis root', async () => {
  const item = await fixture({ externalCommands: [{ shim: 'cargo', command: 'outdated', toolId: 'cargo-outdated', digest, lane: 'ANALYSIS' }] });
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['outdated'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    const rejected = await call(item.service, command('SUBMIT', message(item.workspace, ['check'], { execution: { mode: 'NESTED', parentId: root.id, capabilityToken: 'x'.repeat(43) } })));
    assert.equal(rejected.result.status, 'REJECTED'); assert.equal(rejected.result.code, 'RK_NESTED_HEAVY_REJECTED');
    item.children.at(-1).emit('close', 0, null); await call(item.service, command('WATCH', root.id));
  } finally { await item.close(); }
});

test('cutover fence rejects new roots while drain status exposes active analysis', async () => {
  const item = await fixture({ externalCommands: [{ shim: 'cargo', command: 'outdated', toolId: 'cargo-outdated', digest, lane: 'ANALYSIS' }] });
  let fence;
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['outdated'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    const draining = (await call(item.service, command('DRAIN_STATUS'))).result;
    assert.equal(draining.open, 1);
    assert.equal(draining.inFlight[0].id, root.id);
    assert.equal(draining.inFlight[0].state, 'RUNNING');
    assert.equal(draining.inFlight[0].command, 'cargo outdated');
    assert.equal(draining.inFlight[0].workspace, item.repository);
    assert.ok(draining.inFlight[0].elapsedMs >= 0);
    fence = await beginCutoverDrain(item.root);
    const rejected = (await call(item.service, command('SUBMIT', message(item.workspace, ['outdated'])))).result;
    assert.equal(rejected.code, 'RK_CUTOVER_DRAIN_PENDING');
    assert.match(rejected.error, new RegExp(root.id));
    assert.match(rejected.error, /cargo outdated/);
    assert.match(rejected.error, /rightkit list/);
    assert.equal(rejected.inFlight[0].id, root.id);
    item.children.at(-1).emit('close', 0, null);
    await call(item.service, command('WATCH', root.id));
    assert.equal((await call(item.service, command('DRAIN_STATUS'))).result.open, 0);
  } finally {
    if (fence) await endCutoverDrain(fence);
    await item.close();
  }
});

test('list & status preserve observable detached results after client loss', async () => {
  const item = await fixture();
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['test'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const child = item.children.at(-1); child.stdout.emit('data', Buffer.from('252 passed; 0 failed\n'));
    const active = (await call(item.service, command('LIST'))).result;
    assert.equal(active.inFlight[0].id, submitted.id);
    assert.match(active.inFlight[0].command, /^cargo test --target aarch64-apple-darwin --target-dir /);
    assert.equal(active.inFlight[0].outputBytes, 21);
    assert.ok(active.inFlight[0].elapsedMs >= 0);
    const running = (await call(item.service, command('STATUS', submitted.id))).result;
    assert.equal(running.command, active.inFlight[0].command);
    assert.equal(running.receipt, null);
    child.emit('close', 0, null);
    const terminal = (await call(item.service, command('WATCH', submitted.id))).result;
    assert.equal(terminal.status, 'SUCCEEDED');
    const listed = (await call(item.service, command('LIST'))).result;
    assert.equal(listed.inFlight.length, 0);
    assert.equal(listed.recent[0].id, submitted.id);
    assert.equal(listed.recent[0].state, 'SUCCEEDED');
    const recovered = (await call(item.service, command('STATUS', submitted.id))).result;
    assert.equal(recovered.receipt.state, 'SUCCEEDED');
    assert.match(recovered.receipt.output.head, /252 passed; 0 failed/);
  } finally { await item.close(); }
});

test('Cargo BUILD delegates nested rustc without a second permit or output rewrite', async () => {
  const item = await fixture();
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    const rootChild = item.children.at(-1); const token = rootChild.environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN;
    const registryCwd = join(rootChild.environment.CARGO_HOME, 'registry', 'src', 'crate'); await mkdir(registryCwd, { recursive: true });
    const output = join(rootChild.environment.CARGO_TARGET_DIR, 'release', 'deps', 'crate.rlib');
    const nested = (await call(item.service, command('SUBMIT', compilerMessage(registryCwd, ['src/lib.rs', '-o', output], {
      observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME, RUSTC_WRAPPER: rootChild.environment.RUSTC_WRAPPER, RUSTC: '/managed/rustc', RUSTDOC: 'rustdoc', CARGO_ENCODED_RUSTFLAGS: '-C\u001ftarget-cpu=native' },
      request: { environment: { CARGO_PKG_VERSION: '1.2.3', OUT_DIR: output } },
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token }
    })))).result;
    assert.equal(nested.type, 'SUBMITTED', JSON.stringify(nested));
    await waitFor(item.service, nested.id, ['RUNNING']);
    const compiler = item.children.at(-1);
    assert.deepEqual(compiler.payload.argv, ['src/lib.rs', '-o', output]);
    assert.equal(compiler.environment.CARGO_PKG_VERSION, '1.2.3');
    assert.equal(compiler.environment.OUT_DIR, output);
    compiler.emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', nested.id))).result.status, 'SUCCEEDED');
    const pathDependencyCwd = join(item.repository, 'vendor', 'path-dependency'); await mkdir(pathDependencyCwd, { recursive: true });
    const probe = (await call(item.service, command('SUBMIT', compilerMessage(pathDependencyCwd, ['--print', 'sysroot'], {
      observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME, RUSTC_WRAPPER: rootChild.environment.RUSTC_WRAPPER, RUSTDOC: '/managed/rustdoc' },
      request: { environment: { CARGO_PKG_VERSION_PRE: '', OUT_DIR: output } },
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token }
    })))).result;
    assert.equal(probe.type, 'SUBMITTED', JSON.stringify(probe));
    await waitFor(item.service, probe.id, ['RUNNING']);
    const probeChild = item.children.at(-1);
    assert.equal(probeChild.environment.CARGO_PKG_VERSION_PRE, '');
    probeChild.emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', probe.id))).result.status, 'SUCCEEDED');
    const substituted = await call(item.service, command('SUBMIT', compilerMessage(registryCwd, ['src/lib.rs', '-o', output], {
      observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME, RUSTC_WRAPPER: rootChild.environment.RUSTC_WRAPPER },
      request: { environment: { PATH: '/unmanaged/bin' } },
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token }
    })));
    assert.match(substituted.result.error, /compiler environment cannot set broker-controlled PATH/);
    rootChild.emit('close', 0, null); await call(item.service, command('WATCH', root.id));
  } finally { await item.close(); }
});

test('Cargo metadata delegates nested compiler probes with ambient macOS metadata stripped', async () => {
  const item = await fixture();
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['metadata', '--locked'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    const rootChild = item.children.at(-1); const token = rootChild.environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN;
    const probe = (await call(item.service, command('SUBMIT', compilerMessage(item.workspace, ['-vV'], {
      observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME },
      request: { environment: { __CF_USER_TEXT_ENCODING: '0x1F5:0x0:0x0' } },
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token }
    })))).result;
    assert.equal(probe.type, 'SUBMITTED', JSON.stringify(probe));
    await waitFor(item.service, probe.id, ['RUNNING']);
    const probeChild = item.children.at(-1);
    assert.equal(probeChild.environment.__CF_USER_TEXT_ENCODING, undefined);
    probeChild.emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', probe.id))).result.status, 'SUCCEEDED');
    rootChild.emit('close', 0, null); await call(item.service, command('WATCH', root.id));
  } finally { await item.close(); }
});

test('delegated rustc still rejects a cwd outside parent repository, Cargo, & managed build roots', async () => {
  const item = await fixture();
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    const rootChild = item.children.at(-1); const token = rootChild.environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN;
    const escaped = await call(item.service, command('SUBMIT', compilerMessage(item.other, ['src/lib.rs'], {
      observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME, RUSTC_WRAPPER: rootChild.environment.RUSTC_WRAPPER },
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token }
    })));
    assert.equal(escaped.result.code, 'RK_NESTED_WORKSPACE_ESCAPE');
    rootChild.emit('close', 0, null); await call(item.service, command('WATCH', root.id));
  } finally { await item.close(); }
});

test('external workspace writer delegates only same-workspace nested writes', async () => {
  const item = await fixture({ externalCommands: [{ shim: 'cargo', command: 'upgrade', toolId: 'cargo-outdated', digest, lane: 'WORKSPACE_WRITE' }] });
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['upgrade'])))).result; await waitFor(item.service, root.id, ['RUNNING']);
    const rootChild = item.children.at(-1); const token = rootChild.environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN;
    const delegated = (await call(item.service, command('SUBMIT', message(item.workspace, ['update'], { observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME }, execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token } })))).result;
    await waitFor(item.service, delegated.id, ['RUNNING']); item.children.at(-1).emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', delegated.id))).result.status, 'SUCCEEDED');
    const escaped = await call(item.service, command('SUBMIT', message(item.other, ['update'], { execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token } })));
    assert.equal(escaped.result.code, 'RK_NESTED_WORKSPACE_ESCAPE');
    rootChild.emit('close', 0, null); await call(item.service, command('WATCH', root.id));
  } finally { await item.close(); }
});

test('root cancellation reaches delegated workspace writer', async () => {
  const item = await fixture({ externalCommands: [{ shim: 'cargo', command: 'upgrade', toolId: 'cargo-outdated', digest, lane: 'WORKSPACE_WRITE' }] });
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['upgrade'])))).result; await waitFor(item.service, root.id, ['RUNNING']);
    const rootChild = item.children.at(-1); const nested = (await call(item.service, command('SUBMIT', message(item.workspace, ['update'], { observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME }, execution: { mode: 'NESTED', parentId: root.id, capabilityToken: rootChild.environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN } })))).result;
    await waitFor(item.service, nested.id, ['RUNNING']); await call(item.service, command('CANCEL', root.id));
    assert.equal((await call(item.service, command('WATCH', nested.id))).result.status, 'CANCELLED');
    assert.equal((await call(item.service, command('WATCH', root.id))).result.status, 'CANCELLED');
  } finally { await item.close(); }
});

test('narrow nested metadata re-enters broker & accepts inherited cache environment', async () => {
  const item = await fixture({ externalCommands: [{ shim: 'cargo', command: 'nextest', toolId: 'cargo-outdated', digest, lane: 'BUILD' }] });
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['nextest', 'run'])))).result; await waitFor(item.service, root.id, ['RUNNING']);
    const rootChild = item.children.at(-1); const token = rootChild.environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN;
    const nested = (await call(item.service, command('SUBMIT', message(item.workspace, ['metadata'], { observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME, RUSTC_WRAPPER: rootChild.environment.RUSTC_WRAPPER }, execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token } })))).result;
    await waitFor(item.service, nested.id, ['RUNNING']); item.children.at(-1).emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', nested.id))).result.status, 'SUCCEEDED');
    rootChild.emit('close', 0, null); await call(item.service, command('WATCH', root.id));
  } finally { await item.close(); }
});

test('active cancellation terminates owned tree & persists receipt', async () => {
  const item = await fixture();
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result; await waitFor(item.service, submitted.id, ['RUNNING']);
    assert.equal((await call(item.service, command('CANCEL', submitted.id))).result.accepted, true);
    const terminal = (await call(item.service, command('WATCH', submitted.id))).result;
    assert.equal(terminal.status, 'CANCELLED'); assert.equal(terminal.receipt.reap.reaped, true);
    assert.equal(terminal.receipt.cancellation.source, 'USER_CANCEL');
    assert.equal(terminal.receipt.cancellation.requests.length, 1);
    assert.equal((await item.state.receipt(submitted.id)).state, 'CANCELLED');
  } finally { await item.close(); }
});

test('idle-current-generation eviction holds workspace write exclusion & a queued build re-resolves to the fresh generation', async () => {
  let evictIdle = null;
  const item = await fixture({
    gc: async ({ capBytes, evictIdle: callback }) => { evictIdle = callback; return { capBytes, beforeBytes: 0, afterBytes: 0, deleted: [], evicted: [], unenforceable: null }; }
  });
  try {
    assert.equal(typeof evictIdle, 'function', 'maintenance GC must receive the eviction callback');
    // Occupy the single BUILD lane with the unrelated workspace.
    const first = (await call(item.service, command('SUBMIT', message(item.other, ['check'])))).result;
    await waitFor(item.service, first.id, ['RUNNING']);
    // Queue a build for the main workspace behind the occupied lane.
    const second = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result;
    assert.equal((await call(item.service, command('STATUS', second.id))).result.state, 'QUEUED');
    // Background GC evicts the idle main-workspace generation while the build waits.
    const workspace = await item.state.workspace(item.repository);
    const evicted = await evictIdle({ workspaceKey: workspace.key, canonicalRoot: workspace.canonicalRoot, generation: workspace.state.currentGeneration, sizeBytes: 1024 });
    assert.equal(evicted.evictedGeneration, 1);
    assert.equal(evicted.newGeneration, 2);
    await assert.rejects(stat(workspace.layout.root), { code: 'ENOENT' });
    // The queued build proceeds on the fresh generation, never the evicted one.
    item.children[0].emit('close', 0, null);
    await waitFor(item.service, second.id, ['RUNNING']);
    const child = item.children[1];
    assert.ok(child.environment.CARGO_TARGET_DIR.includes(join('g', '002', process.platform === 'win32' ? 't' : 'target')), `build targeted ${child.environment.CARGO_TARGET_DIR}`);
    child.emit('close', 0, null);
    const terminal = (await call(item.service, command('WATCH', second.id))).result;
    assert.equal(terminal.status, 'SUCCEEDED');
    assert.equal(terminal.receipt.generation, 2);
  } finally { await item.close(); }
});

test('eviction never retires a generation with an active writer', async () => {
  let evictIdle = null;
  const item = await fixture({
    gc: async ({ capBytes, evictIdle: callback }) => { evictIdle = callback; return { capBytes, beforeBytes: 0, afterBytes: 0, deleted: [], evicted: [], unenforceable: null }; }
  });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const workspace = await item.state.workspace(item.repository);
    assert.equal(await evictIdle({ workspaceKey: workspace.key, canonicalRoot: workspace.canonicalRoot, generation: workspace.state.currentGeneration, sizeBytes: 1024 }), null);
    await assert.doesNotReject(stat(workspace.layout.root));
    const terminal = (await call(item.service, command('CANCEL', submitted.id))).result;
    assert.equal(terminal.accepted, true);
    await waitFor(item.service, submitted.id, ['CANCELLED']);
  } finally { await item.close(); }
});

test('cleanly-reaped sccache-unavailable cancellation keeps the warm generation', async () => {
  const item = await fixture();
  try {
    const first = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, first.id, ['RUNNING']);
    item.service.cacheUnavailable({ reason: 'health-lease-expired' });
    const terminal = (await call(item.service, command('WATCH', first.id))).result;
    assert.equal(terminal.status, 'CANCELLED');
    assert.equal(terminal.receipt.reap.reaped, true);
    // The reaped writer left a trustworthy tree: the next build must resolve to
    // the SAME generation instead of a rotated empty one.
    const followup = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, followup.id, ['RUNNING']);
    const child = item.children.at(-1);
    assert.match(child.environment.CARGO_TARGET_DIR, /g[\\/]001[\\/]/);
    await call(item.service, command('CANCEL', followup.id));
    await call(item.service, command('WATCH', followup.id));
  } finally { await item.close(); }
});


const measuredBuildUsage = (cpuTotalMs, extra = {}) => ({ status: 'OK', cpuUserMs: cpuTotalMs, cpuKernelMs: 0, cpuTotalMs, peakCommittedBytes: 4096, totalProcesses: 2, activeProcesses: 0, ...extra });

test('short builds persist final CPU & committed-memory measurements with existing timings', async () => {
  let reads = 0; let cacheCpu = 0; let clock = 1000;
  const item = await fixture({
    now: () => (clock += 10),
    readResources: () => measuredBuildUsage(++reads * 100),
    readHostResources: () => ({ logicalCpuCount: 20, totalMemoryBytes: 64 * 1024 ** 3, freeMemoryBytes: 30 * 1024 ** 3 }),
    cache: {
      prepare: async () => ({ mode: 'sccache', status: 'OK', environment: {}, before: {} }),
      resourceUsage: () => measuredBuildUsage(++cacheCpu * 10, { epoch: 'gateway-1' }),
      finish: async () => ({ mode: 'sccache', status: 'OK', delta: {}, attribution: { exact: true } }),
    },
  });
  try {
    const request = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, request.id, ['RUNNING']);
    item.children.at(-1).emit('close', 0, null);
    const result = (await call(item.service, command('WATCH', request.id))).result;
    const resources = result.receipt.resources;
    assert.equal(result.status, 'SUCCEEDED');
    assert.equal(resources.status, 'OK');
    assert.equal(resources.processTree.cpuTotalMs, 200);
    assert.equal(resources.processTree.peakCommittedBytes, 4096);
    assert.equal(resources.processTree.memoryKind, 'committed');
    assert.equal(resources.sharedCompilerCache.cpuTotalMs, 20);
    assert.equal(resources.cpuTotalMs, 220);
    assert.ok(resources.averageCpuCores > 0);
    assert.equal(resources.host.logicalCpuCount, 20);
    assert.equal(resources.host.cargoBuildJobs, 4);
    assert.equal(resources.host.compileBudgetUnits, null);
    assert.ok(result.receipt.queueMs >= 0);
    assert.ok(result.receipt.cargoRunMs >= 0);
    assert.equal((await item.state.receipt(request.id)).resources.cpuTotalMs, 220);
    assert.deepEqual(item.logger.events.find((event) => event.type === 'TERMINAL' && event.id === request.id).resources, resources);
  } finally { await item.close(); }
});

test('RUNNING records expose measurement failures without changing build outcome', async () => {
  const item = await fixture({ readResources: () => { throw new Error('native query unavailable'); } });
  try {
    const sampled = new Promise((resolve) => {
      const append = item.logger.append.bind(item.logger);
      item.logger.append = (event) => { if (event.type === 'RUNNING') resolve(event); return append(event); };
    });
    const request = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, request.id, ['RUNNING']);
    const event = await sampled;
    assert.equal(event.resources.processTree.status, 'ERROR');
    assert.match(event.resources.processTree.error, /native query unavailable/);
    item.children.at(-1).emit('close', 0, null);
    const result = (await call(item.service, command('WATCH', request.id))).result;
    assert.equal(result.status, 'SUCCEEDED');
    assert.equal(result.receipt.resources.status, 'UNAVAILABLE');
    assert.equal(result.receipt.resources.cpuTotalMs, undefined);
  } finally { await item.close(); }
});

test('delegated compiler accounting is retained in root resource receipt', async () => {
  const item = await fixture({ readResources: (child) => measuredBuildUsage(child.pid === 301 ? 100 : 200) });
  try {
    const root = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, root.id, ['RUNNING']);
    const rootChild = item.children.at(-1);
    const nested = (await call(item.service, command('SUBMIT', compilerMessage(item.workspace, ['--version'], {
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: rootChild.environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN },
    })))).result;
    await call(item.service, command('WATCH', nested.id));
    rootChild.emit('close', 0, null);
    const result = (await call(item.service, command('WATCH', root.id))).result;
    assert.equal(result.receipt.resources.delegatedCompilers.count, 1);
    assert.equal(result.receipt.resources.delegatedCompilers.cpuTotalMs, 200);
    assert.equal(result.receipt.resources.delegatedCompilers.sumPeakCommittedBytes, 4096);
    assert.equal(result.receipt.resources.processTree.peakCommittedBytes, 4096);
  } finally { await item.close(); }
});

test('Cargo receipt records effective jobs, wait blockers, freshness counts, target state & request count', async () => {
  const item = await fixture({ cacheEnvironment: { CARGO_BUILD_JOBS: '4' } });
  try {
    const first = (await call(item.service, command('SUBMIT', message(item.workspace, ['build', '--release', '-j', '2', '--message-format=json'])))).result;
    await waitFor(item.service, first.id, ['RUNNING']);
    // A second BUILD queues behind the first and must record it as blocker.
    const second = (await call(item.service, command('SUBMIT', message(item.workspace, ['build', '--release'])))).result;
    const child = item.children.at(-1);
    child.stdout.emit('data', Buffer.from('{"reason":"compiler-artifact","package_id":"a 1.0.0","fresh":true}\n{"reason":"compiler-artifact","package_id":"b 1.0.0","fresh":false}\n{"reason":"build-script-executed","package_id":"b 1.0.0"}\n{"reason":"compiler-artifact","package_id":"c 1.0.0","fres'));
    child.stdout.emit('data', Buffer.from('h":false}\n'));
    child.emit('close', 0, null);
    const done = await waitFor(item.service, first.id, ['SUCCEEDED']);
    const receipt = done.receipt;
    assert.deepEqual({ ...receipt.jobs }, { requested: 2, brokerConfigured: 4, effective: 2, source: 'cli-argv' });
    assert.equal(receipt.freshness.compilerArtifacts, 3);
    assert.equal(receipt.freshness.fresh, 1);
    assert.equal(receipt.freshness.notFresh, 2);
    assert.equal(receipt.freshness.buildScriptExecutedEvents, 1);
    assert.deepEqual([...receipt.freshness.notFreshPackages], ['b 1.0.0', 'c 1.0.0']);
    assert.equal(receipt.target.existedAtStart, true);
    assert.equal(receipt.coldStart, receipt.target.entriesAtStart > 0 ? false : true);
    assert.equal(receipt.compilerRequestCount, receipt.compilerProcessCount);
    assert.equal(receipt.waits.brokerQueue.ms, receipt.queueMs);
    // Second job waited on the first.
    await waitFor(item.service, second.id, ['RUNNING']);
    item.children.at(-1).emit('close', 0, null);
    const secondDone = await waitFor(item.service, second.id, ['SUCCEEDED']);
    assert.equal(secondDone.receipt.waits.brokerQueue.blockers[0].id, first.id);
    assert.match(secondDone.receipt.waits.brokerQueue.blockers[0].reason, /lane-cap:BUILD|workspace-exclusion/);
    assert.equal(secondDone.receipt.jobs.source, 'broker-injected-env');
    assert.equal(secondDone.receipt.freshness.available, false);
    const attached = await call(item.service, command('WATCH', second.id));
    const lines = attached.events.filter((event) => event.event?.type === 'STDERR' && /\[rightkit\] target/.test(event.event.data));
    assert.equal(lines.length, 2);
  } finally { await item.close(); }
});

test('prebuilt OpenSSL environment is injected into root cargo runs, recorded in the receipt, and pinned to the generation', async () => {
  const calls = [];
  const nativeOpenSsl = { prepare: async (input) => { calls.push(input); return { status: 'hit', key: 'abcdabcdabcdabcd', path: '/n/abcdabcdabcdabcd', buildMs: 0, environment: { AARCH64_APPLE_DARWIN_OPENSSL_DIR: '/n/abcdabcdabcdabcd', AARCH64_APPLE_DARWIN_OPENSSL_NO_VENDOR: '1', AARCH64_APPLE_DARWIN_OPENSSL_STATIC: '1' } }; }, gc: async () => ({ evicted: [] }) };
  const item = await fixture({ nativeOpenSsl });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['build'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const child = item.children.at(-1);
    assert.equal(child.environment.AARCH64_APPLE_DARWIN_OPENSSL_NO_VENDOR, '1');
    assert.equal(calls.length, 1);
    child.emit('close', 0, null);
    const result = (await call(item.service, command('WATCH', submitted.id))).result;
    assert.equal(result.receipt.native.openssl.status, 'hit');
    assert.equal(result.receipt.native.openssl.key, 'abcdabcdabcdabcd');
    assert.deepEqual([...await item.state.pinnedNativeKeys('openssl')], ['abcdabcdabcdabcd']);
  } finally { await item.close(); }
});


test('broker owns Mac account identity without changing other platforms', async () => {
  for (const platform of ['darwin', 'linux']) {
    const item = await fixture({ platform });
    try {
      const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result;
      await waitFor(item.service, submitted.id, ['RUNNING']);
      const child = item.children.at(-1);
      assert.equal(child.environment.HOME, platform === 'darwin' ? homedir() : undefined);
      assert.equal(child.environment.CARGO_HOME, item.state.paths.cargoHome);
      assert.equal(child.environment.RUSTC_WRAPPER, '/managed/sccache');
      child.emit('close', 0, null);
      assert.equal((await call(item.service, command('WATCH', submitted.id))).result.status, 'SUCCEEDED');
    } finally { await item.close(); }
  }
});
