import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { BUILD_CONTROL_PROTOCOL, SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';
import { CachedWorkspaceResolver, ThinJobService, createThinJobServer } from './thin-job-service.mjs';
import { ThinState } from './thin-state.mjs';

const digest = 'a'.repeat(64);
const message = (workspace, argv, extra = {}) => ({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, shim: 'cargo', observedEnvironment: {}, request: { frontend: 'cargo', argv, workspace, environment: {}, profile: 'MANAGED_AGENT', configuration: {} }, ...extra });
const compilerMessage = (workspace, argv, extra = {}) => ({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, shim: 'rustc', observedEnvironment: {}, ...extra, request: { frontend: 'raw-compiler', argv, workspace, environment: {}, profile: 'MANAGED_AGENT', configuration: {}, ...(extra.request ?? {}) } });
const command = (type, value) => ({ type, protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, ...(type === 'SUBMIT' ? { job: value } : { id: value }) });

class Child extends EventEmitter {
  constructor(pid, payload) { super(); this.pid = pid; this.payload = payload; this.stdout = new EventEmitter(); this.stderr = new EventEmitter(); this.connected = true; }
}

async function fixture({ externalCommands = [], cache = null, cacheEnvironment = {}, targetTriples = null, gc = null } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-job-')); const repository = join(root, 'repository'); const workspace = join(repository, 'workspace'); const other = join(root, 'other');
  await Promise.all([mkdir(join(repository, '.git'), { recursive: true }), mkdir(join(workspace, '.cargo'), { recursive: true }), mkdir(other)]);
  await writeFile(join(workspace, '.cargo', 'config.toml'), '[net]\ngit-fetch-with-cli = true\n');
  const state = await ThinState.open({ externalRoot: root }); const children = []; let pid = 300;
  if (gc) state.gc = gc;
  const tools = new Map([['cargo', { id: 'cargo', path: '/managed/cargo', sha256: digest }], ['rustc', { id: 'rustc', path: '/managed/rustc', sha256: digest }], ['rustdoc', { id: 'rustdoc', path: '/managed/rustdoc', sha256: digest }], ['cargo-outdated', { id: 'cargo-outdated', path: '/managed/cargo-outdated', sha256: digest }]]);
  const registry = { verify: async (id) => { const tool = tools.get(id); if (!tool) throw new Error(`missing ${id}`); return tool; } };
  const logger = { events: [], append(event) { this.events.push(event); return Promise.resolve(event); }, close: async () => {} };
  const selectedCache = cache ?? { prepare: async ({ layout }) => ({ mode: 'sccache', status: 'OK', environment: { CARGO_HOME: state.paths.cargoHome, CARGO_TARGET_DIR: layout.target, CARGO_BUILD_TARGET_DIR: layout.target, RUSTC_WRAPPER: '/managed/sccache', TMPDIR: layout.temp, TMP: layout.temp, TEMP: layout.temp, ...cacheEnvironment }, before: {} }), finish: async () => ({ mode: 'sccache', status: 'OK', delta: { compile_requests: 1 } }) };
  const config = { broker: { externalRoot: { path: root } }, shims: { cargo: 'cargo', rustc: 'rustc', rustdoc: 'rustdoc' }, tools: [], externalCommands, defaults: { targetTriple: 'aarch64-apple-darwin', ...(targetTriples ? { targetTriples } : {}), buildProfile: 'development', cargoBuildJobs: 4, toolchain: 'stable-aarch64-apple-darwin' }, sccache: { mode: 'managed', maxSizeBytes: 1024 }, bounds: { buildConcurrency: 1, controlConcurrency: 4, analysisConcurrency: 2, workspaceWriteConcurrency: 4, processGraceMs: 10, heartbeatIntervalMs: 100, outputBufferBytes: 64 * 1024, outputHeadBytes: 1024, outputTailBytes: 1024, freeFloorBytes: 0 } };
  const service = await ThinJobService.create({ config, state, cache: selectedCache, registry, logger, singleton: { release: async () => true }, workspaceResolver: async (cwd) => ({ executionCwd: cwd, workspaceRoot: cwd }), spawn: (_command, argv, options) => { const payload = JSON.parse(Buffer.from(argv[1], 'base64url').toString('utf8')); const child = new Child(++pid, payload); child.environment = options.env; children.push(child); return child; }, processIdentity: async (childPid) => ({ status: 'OK', identity: `identity-${childPid}` }), processOwnership: (child) => ({ pid: child.pid, processGroupId: child.pid, platform: 'posix', identity: `identity-${child.pid}` }), startProcess: async (child) => { if (child.payload.argv.includes('--version')) queueMicrotask(() => child.emit('close', 0, null)); return { pid: child.pid }; }, terminateProcess: async (child) => { if (!child.closed) { child.closed = true; queueMicrotask(() => child.emit('close', null, 'SIGTERM')); } return { reaped: true, reason: 'gone' }; }, statfs: async () => ({ bavail: 10_000, bsize: 4096 }) });
  return { root, repository, workspace, other, state, service, children, logger, close: async () => { await service.shutdown(); await rm(root, { recursive: true, force: true }); } };
}

async function call(service, value) { const events = []; const result = await service.dispatch(value, (event) => { events.push(event); return true; }); return { result, events }; }
async function waitFor(service, id, states) { let last; for (let attempt = 0; attempt < 100; attempt += 1) { const { result } = await call(service, command('STATUS', id)); last = result; if (states.includes(result.state)) return result; if (['SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT'].includes(result.state)) break; await new Promise((resolve) => setTimeout(resolve, 2)); } throw new Error(`job ${id} did not reach ${states}: ${JSON.stringify(last)}`); }

test('broker readiness does not wait for retired-generation sizing', async () => {
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const item = await Promise.race([
    fixture({ gc: async () => gate }),
    new Promise((_, reject) => setTimeout(() => reject(new Error('broker creation waited for GC')), 100))
  ]);
  try {
    assert.equal(item.logger.events.some((event) => event.type === 'BROKER_READY'), true);
    release({ deleted: [], totalBytes: 0, capBytes: null });
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
  const resolver = new CachedWorkspaceResolver({ concurrency: 1, resolver: async (cwd, options) => { calls += 1; assert.equal(options.timeoutMs, 2_000); await gate; return { executionCwd: cwd, workspaceRoot: cwd, workspaceDiscoveryMs: 8 }; } });
  try {
    const first = resolver.resolve(root); const duplicate = resolver.resolve(root);
    for (let attempt = 0; attempt < 20 && calls === 0; attempt += 1) await new Promise((resolve) => setTimeout(resolve, 1));
    assert.equal(calls, 1); assert.equal(resolver.stats.inflight, 1);
    release();
    assert.equal((await first).workspaceDiscoveryCache, 'MISS');
    assert.equal((await duplicate).workspaceDiscoveryCache, 'MISS');
    const cached = await resolver.resolve(root);
    assert.equal(cached.workspaceDiscoveryCache, 'HIT'); assert.equal(cached.workspaceDiscoveryMs, 0);
    assert.deepEqual(resolver.stats, { lookup: 3, hit: 1, miss: 2, error: 0, active: 0, queued: 0, inflight: 0 });
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
  const item = await fixture({ cacheEnvironment: { DYLD_FALLBACK_LIBRARY_PATH: '/stale/toolchain/lib' } });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['--version'])))).result;
    assert.equal((await call(item.service, command('WATCH', submitted.id))).result.status, 'SUCCEEDED');
    assert.equal(item.children.at(-1).environment.DYLD_FALLBACK_LIBRARY_PATH, join(dirname('/managed/cargo'), '..', 'lib'));
  } finally { await item.close(); }
});

test('registered explicit Cargo target overrides host default within persistent workspace generation', async () => {
  const item = await fixture({ targetTriples: ['aarch64-apple-darwin', 'x86_64-apple-darwin'] });
  try {
    const submitted = (await call(item.service, command('SUBMIT', message(item.workspace, ['build', '--target=x86_64-apple-darwin'])))).result;
    await waitFor(item.service, submitted.id, ['RUNNING']);
    const child = item.children.at(-1);
    assert.deepEqual(child.payload.argv, ['build', '--target=x86_64-apple-darwin']);
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
    const workspace = await item.state.workspace(item.workspace);
    const evicted = await evictIdle({ workspaceKey: workspace.key, canonicalRoot: workspace.canonicalRoot, generation: workspace.state.currentGeneration, sizeBytes: 1024 });
    assert.equal(evicted.evictedGeneration, 1);
    assert.equal(evicted.newGeneration, 2);
    await assert.rejects(stat(workspace.layout.root), { code: 'ENOENT' });
    // The queued build proceeds on the fresh generation, never the evicted one.
    item.children[0].emit('close', 0, null);
    await waitFor(item.service, second.id, ['RUNNING']);
    const child = item.children[1];
    assert.ok(child.environment.CARGO_TARGET_DIR.includes(join('generations', '000002', 'target')), `build targeted ${child.environment.CARGO_TARGET_DIR}`);
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
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(await evictIdle({ workspaceKey: workspace.key, canonicalRoot: workspace.canonicalRoot, generation: workspace.state.currentGeneration, sizeBytes: 1024 }), null);
    await assert.doesNotReject(stat(workspace.layout.root));
    const terminal = (await call(item.service, command('CANCEL', submitted.id))).result;
    assert.equal(terminal.accepted, true);
    await waitFor(item.service, submitted.id, ['CANCELLED']);
  } finally { await item.close(); }
});
