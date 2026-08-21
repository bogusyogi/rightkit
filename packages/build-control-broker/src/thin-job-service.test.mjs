import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { BUILD_CONTROL_PROTOCOL, SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';
import { ThinJobService } from './thin-job-service.mjs';
import { ThinState } from './thin-state.mjs';

const digest = 'a'.repeat(64);
const message = (workspace, argv, extra = {}) => ({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, shim: 'cargo', observedEnvironment: {}, request: { frontend: 'cargo', argv, workspace, environment: {}, profile: 'MANAGED_AGENT', configuration: {} }, ...extra });
const compilerMessage = (workspace, argv, extra = {}) => ({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, shim: 'rustc', observedEnvironment: {}, request: { frontend: 'raw-compiler', argv, workspace, environment: {}, profile: 'MANAGED_AGENT', configuration: {} }, ...extra });
const command = (type, value) => ({ type, protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, ...(type === 'SUBMIT' ? { job: value } : { id: value }) });

class Child extends EventEmitter {
  constructor(pid, payload) { super(); this.pid = pid; this.payload = payload; this.stdout = new EventEmitter(); this.stderr = new EventEmitter(); this.connected = true; }
}

async function fixture({ externalCommands = [], cache = null } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-job-')); const workspace = join(root, 'workspace'); const other = join(root, 'other');
  await Promise.all([mkdir(join(workspace, '.cargo'), { recursive: true }), mkdir(other)]);
  await writeFile(join(workspace, '.cargo', 'config.toml'), '[net]\ngit-fetch-with-cli = true\n');
  const state = await ThinState.open({ externalRoot: root }); const children = []; let pid = 300;
  const tools = new Map([['cargo', { id: 'cargo', path: '/managed/cargo', sha256: digest }], ['rustc', { id: 'rustc', path: '/managed/rustc', sha256: digest }], ['rustdoc', { id: 'rustdoc', path: '/managed/rustdoc', sha256: digest }], ['cargo-outdated', { id: 'cargo-outdated', path: '/managed/cargo-outdated', sha256: digest }]]);
  const registry = { verify: async (id) => { const tool = tools.get(id); if (!tool) throw new Error(`missing ${id}`); return tool; } };
  const logger = { events: [], append(event) { this.events.push(event); return Promise.resolve(event); }, close: async () => {} };
  const selectedCache = cache ?? { prepare: async ({ layout }) => ({ mode: 'sccache', status: 'OK', environment: { CARGO_HOME: state.paths.cargoHome, CARGO_TARGET_DIR: layout.target, CARGO_BUILD_TARGET_DIR: layout.target, CARGO_INSTALL_ROOT: state.paths.tools, RUSTC_WRAPPER: '/managed/sccache', TMPDIR: layout.temp, TMP: layout.temp, TEMP: layout.temp }, before: {} }), finish: async () => ({ mode: 'sccache', status: 'OK', delta: { compile_requests: 1 } }) };
  const config = { broker: { externalRoot: { path: root } }, shims: { cargo: 'cargo', rustc: 'rustc', rustdoc: 'rustdoc' }, tools: [], externalCommands, defaults: { targetTriple: 'aarch64-apple-darwin', cargoBuildJobs: 4, toolchain: 'stable-aarch64-apple-darwin' }, sccache: { mode: 'managed', maxSizeBytes: 1024 }, bounds: { buildConcurrency: 1, controlConcurrency: 4, analysisConcurrency: 2, workspaceWriteConcurrency: 4, processGraceMs: 10, heartbeatIntervalMs: 100, outputBufferBytes: 64 * 1024, outputHeadBytes: 1024, outputTailBytes: 1024, freeFloorBytes: 0 } };
  const service = await ThinJobService.create({ config, state, cache: selectedCache, registry, logger, singleton: { release: async () => true }, workspaceResolver: async (cwd) => ({ executionCwd: cwd, workspaceRoot: cwd }), spawn: (_command, argv, options) => { const payload = JSON.parse(Buffer.from(argv[1], 'base64url').toString('utf8')); const child = new Child(++pid, payload); child.environment = options.env; children.push(child); return child; }, processIdentity: async (childPid) => ({ status: 'OK', identity: `identity-${childPid}` }), processOwnership: (child) => ({ pid: child.pid, processGroupId: child.pid, platform: 'posix', identity: `identity-${child.pid}` }), startProcess: async (child) => { if (child.payload.argv.includes('--version')) queueMicrotask(() => child.emit('close', 0, null)); return { pid: child.pid }; }, terminateProcess: async (child) => { if (!child.closed) { child.closed = true; queueMicrotask(() => child.emit('close', null, 'SIGTERM')); } return { reaped: true, reason: 'gone' }; }, statfs: async () => ({ bavail: 10_000, bsize: 4096 }) });
  return { root, workspace, other, state, service, children, logger, close: async () => { await service.shutdown(); await rm(root, { recursive: true, force: true }); } };
}

async function call(service, value) { const events = []; const result = await service.dispatch(value, (event) => { events.push(event); return true; }); return { result, events }; }
async function waitFor(service, id, states) { for (let attempt = 0; attempt < 100; attempt += 1) { const { result } = await call(service, command('STATUS', id)); if (states.includes(result.state)) return result; await new Promise((resolve) => setTimeout(resolve, 2)); } throw new Error(`job ${id} did not reach ${states}`); }

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

test('allows harmless Cargo config but rejects managed output redirection', async () => {
  const item = await fixture();
  try {
    assert.equal((await call(item.service, command('SUBMIT', message(item.workspace, ['--version'])))).result.type, 'SUBMITTED');
    await writeFile(join(item.workspace, '.cargo', 'config.toml'), '[build]\ntarget-dir = "local-target"\n');
    const rejected = (await call(item.service, command('SUBMIT', message(item.workspace, ['check'])))).result;
    assert.equal(rejected.status, 'REJECTED'); assert.match(rejected.error, /cannot redirect/);
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
      observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME, RUSTC_WRAPPER: rootChild.environment.RUSTC_WRAPPER, RUSTC: '/managed/rustc', RUSTDOC: 'rustdoc' },
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token }
    })))).result;
    assert.equal(nested.type, 'SUBMITTED', JSON.stringify(nested));
    await waitFor(item.service, nested.id, ['RUNNING']);
    const compiler = item.children.at(-1);
    assert.deepEqual(compiler.payload.argv, ['src/lib.rs', '-o', output]);
    compiler.emit('close', 0, null);
    assert.equal((await call(item.service, command('WATCH', nested.id))).result.status, 'SUCCEEDED');
    const substituted = await call(item.service, command('SUBMIT', compilerMessage(registryCwd, ['src/lib.rs', '-o', output], {
      observedEnvironment: { CARGO_HOME: rootChild.environment.CARGO_HOME, CARGO_TARGET_DIR: rootChild.environment.CARGO_TARGET_DIR, RUSTUP_HOME: rootChild.environment.RUSTUP_HOME, RUSTC_WRAPPER: rootChild.environment.RUSTC_WRAPPER, RUSTDOC: '/unmanaged/rustdoc' },
      execution: { mode: 'NESTED', parentId: root.id, capabilityToken: token }
    })));
    assert.match(substituted.result.error, /conflicting RUSTDOC/);
    rootChild.emit('close', 0, null); await call(item.service, command('WATCH', root.id));
  } finally { await item.close(); }
});

test('delegated rustc still rejects a cwd outside parent Cargo & managed build roots', async () => {
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
    assert.equal((await item.state.receipt(submitted.id)).state, 'CANCELLED');
  } finally { await item.close(); }
});
