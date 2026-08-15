import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { BUILD_CONTROL_PROTOCOL, SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';
import { ManagedJobService } from './job-service.mjs';

async function fixture({ delayMs = 350, freeBytes = 100_000_000, statfsError = null, statfsOperation = null, bounds = {}, eventLog = null, activationFence = null, targetTriples = null } = {}) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'rightkit-job-service-')); const tokenFile = path.join(root, 'parent-token');
  const workspace = path.join(root, 'workspace'); const managed = path.join(root, 'managed'); const bin = path.join(root, 'fake-cargo');
  await mkdir(path.join(workspace, '.git'), { recursive: true }); await mkdir(managed, { recursive: true });
  await writeFile(bin, `#!/usr/bin/env node\nconst fs=require('node:fs'); const command=process.argv[2]; const ci=String(process.env.CI||''); if(process.env.RUST_LOG) try{fs.writeFileSync(process.env.RUST_LOG, process.env.RIGHTKIT_PARENT_CAPABILITY_TOKEN||'')}catch{}; if(ci.startsWith('out:')) process.stdout.write('x'.repeat(Number(ci.slice(4))||0)); if(command==='--version'||command==='metadata'||command==='show'){ console.log('fake cargo 1.0'); process.exit(0); } setTimeout(()=>process.exit(0), Number(ci)||${delayMs});\n`);
  await chmod(bin, 0o755); const sha256 = createHash('sha256').update(await readFile(bin)).digest('hex');
  const config = {
    broker: { externalRoot: { path: managed, volumeId: 'test-volume', mounted: true, writable: true, internal: false }, capacityBytes: 1_000_000_000, stateFile: path.join(managed, 'state', 'legacy.json'), receiptsDir: path.join(managed, 'receipts'), ...(activationFence ? { activationFencePath: path.join(managed, 'activation-fence.json') } : {}) },
    unixSocket: path.join(managed, 'broker.sock'),
    tools: [{ id: 'cargo', path: bin, root, sha256 }], shims: { cargo: 'cargo' },
    defaults: { toolchain: 'stable', targetTriple: 'x86_64-apple-darwin', ...(targetTriples ? { targetTriples } : {}), buildProfile: 'development' },
    bounds: { heartbeatIntervalMs: 20, diskSampleIntervalMs: 20, processGraceMs: 300, statfsTimeoutMs: 50, freeFloorBytes: 1, eventLogMaxBytes: 64 * 1024, outputBufferBytes: 64 * 1024, ...bounds }
  };
  if (activationFence) await writeFile(config.broker.activationFencePath, `${JSON.stringify(activationFence)}\n`);
  const statfs = statfsOperation ?? (async () => { if (statfsError) throw statfsError; return { bavail: freeBytes, bsize: 1 }; });
  const service = await ManagedJobService.create({ config, statfs, eventLog });
  return { root, workspace, managed, bin, tokenFile, config, service, cleanup: async () => { await service.shutdown(); await rm(root, { recursive: true, force: true }); } };
}

function managed(workspace, argv, extra = {}) {
  return { type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, shim: 'cargo', observedEnvironment: {}, ...extra, request: { frontend: 'cargo', argv, workspace, environment: {}, ...(extra.request ?? {}) } };
}
async function tokenFor(item) { for (let i = 0; i < 100; i += 1) { try { const token = await readFile(item.tokenFile, 'utf8'); if (token.length >= 32) return token; } catch {} await new Promise((resolve) => setTimeout(resolve, 10)); } throw new Error('parent token was not emitted'); }
const submit = (job) => ({ type: 'SUBMIT', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, job });
const command = (type, id) => ({ type, protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY, id });
async function call(service, message) { const events = []; const result = await service.dispatch(message, (event) => events.push(event)); return { result, events }; }
async function waitForState(service, id, expected, timeoutMs = 2_000) { const deadline = Date.now() + timeoutMs; for (;;) { const { result } = await call(service, command('STATUS', id)); if (expected.includes(result.state)) return result; if (Date.now() >= deadline) throw new Error(`job ${id} remained ${result.state}`); await new Promise((resolve) => setTimeout(resolve, 10)); } }

test('SUBMIT owns job after client detaches & WATCH receives one terminal receipt', async () => {
  const item = await fixture();
  try {
    const submitted = await call(item.service, submit(managed(item.workspace, ['test'])));
    assert.equal(submitted.result.type, 'SUBMITTED');
    await waitForState(item.service, submitted.result.id, ['RUNNING', 'REAPING', 'SUCCEEDED']);
    const watched = await call(item.service, command('WATCH', submitted.result.id));
    assert.equal(watched.result.status, 'SUCCEEDED'); assert.equal(watched.result.receipt.state, 'SUCCEEDED');
    const status = await call(item.service, command('STATUS', submitted.result.id));
    assert.equal(status.result.receipt.terminalAt, watched.result.receipt.terminalAt);
  } finally { await item.cleanup(); }
});

test('CONTROL completes while BUILD lane is active & creates no workspace namespace', async () => {
  const item = await fixture({ delayMs: 600 });
  try {
    const heavy = await call(item.service, submit(managed(item.workspace, ['test']))); await waitForState(item.service, heavy.result.id, ['RUNNING']);
    const started = Date.now(); const control = await call(item.service, submit(managed(item.workspace, ['--version'])));
    const controlResult = await call(item.service, command('WATCH', control.result.id));
    assert.equal(controlResult.result.status, 'SUCCEEDED', JSON.stringify(controlResult.result)); assert.equal(controlResult.result.receipt.namespace, null); assert.ok(Date.now() - started < 2_000);
    const heavyStatus = await call(item.service, command('STATUS', heavy.result.id)); assert.equal(heavyStatus.result.state, 'RUNNING');
    await call(item.service, command('WATCH', heavy.result.id));
  } finally { await item.cleanup(); }
});

test('registered universal release targets admit while unknown targets reject', async () => {
  const item = await fixture({ targetTriples: ['x86_64-apple-darwin', 'aarch64-apple-darwin'] });
  try {
    const submitted = await call(item.service, submit(managed(item.workspace, ['build', '--target', 'aarch64-apple-darwin'])));
    const result = await call(item.service, command('WATCH', submitted.result.id));
    assert.equal(result.result.status, 'SUCCEEDED');
    const rejected = await call(item.service, submit(managed(item.workspace, ['build', '--target', 'wasm32-unknown-unknown'])));
    assert.equal(rejected.result.status, 'REJECTED');
    assert.match(rejected.result.error, /must be registered/);
  } finally { await item.cleanup(); }
});

test('explicit CANCEL reaps active process & socket-independent job becomes CANCELLED', async () => {
  const item = await fixture({ delayMs: 5_000 });
  try {
    const submitted = await call(item.service, submit(managed(item.workspace, ['test']))); await waitForState(item.service, submitted.result.id, ['RUNNING']);
    const cancelled = await call(item.service, command('CANCEL', submitted.result.id)); assert.equal(cancelled.result.accepted, true);
    const result = await call(item.service, command('WATCH', submitted.result.id));
    assert.equal(result.result.status, 'CANCELLED', JSON.stringify(result.result)); assert.equal(result.result.receipt.reap.reaped, true);
  } finally { await item.cleanup(); }
});

test('nested request bypasses parent BUILD permit & parent cancellation reaches child', async () => {
  const item = await fixture({ delayMs: 700 });
  try {
    const parent = await call(item.service, submit(managed(item.workspace, ['test'], { request: { environment: { RUST_LOG: item.tokenFile } } }))); await waitForState(item.service, parent.result.id, ['RUNNING']); const token = await tokenFor(item);
    const nested = await call(item.service, submit(managed(item.workspace, ['--version'], { execution: { mode: 'NESTED', parentId: parent.result.id, capabilityToken: token } })));
    const nestedResult = await call(item.service, command('WATCH', nested.result.id)); assert.equal(nestedResult.result.status, 'SUCCEEDED');
    await call(item.service, command('CANCEL', parent.result.id)); const parentResult = await call(item.service, command('WATCH', parent.result.id)); assert.equal(parentResult.result.status, 'CANCELLED');
  } finally { await item.cleanup(); }
});

test('nested execution cannot forge parent lineage or heavy-lane permit', async () => {
  const item = await fixture({ delayMs: 500 });
  try {
    const forged = await call(item.service, submit(managed(item.workspace, ['--version'], { execution: { mode: 'NESTED', parentId: '00000000-0000-4000-8000-000000000000', capabilityToken: 'x'.repeat(32) } })));
    const forgedResult = await call(item.service, command('WATCH', forged.result.id));
    assert.equal(forgedResult.result.status, 'FAILED');
    assert.equal(forgedResult.result.receipt.reason, 'RK_PARENT_NOT_ACTIVE');

    const parent = await call(item.service, submit(managed(item.workspace, ['test'], { request: { environment: { RUST_LOG: item.tokenFile } } })));
    await waitForState(item.service, parent.result.id, ['RUNNING']);
    const token = await tokenFor(item);
    const heavyNested = await call(item.service, submit(managed(item.workspace, ['test'], { execution: { mode: 'NESTED', parentId: parent.result.id, capabilityToken: token } })));
    const heavyResult = await call(item.service, command('WATCH', heavyNested.result.id));
    assert.equal(heavyResult.result.status, 'SUCCEEDED');
    await call(item.service, command('CANCEL', parent.result.id));
    await call(item.service, command('WATCH', parent.result.id));
  } finally { await item.cleanup(); }
});

test('CONTROL root cannot mint a nested BUILD permit', async () => {
  const item = await fixture({ delayMs: 500 });
  try {
    const root = await call(item.service, submit(managed(item.workspace, ['fmt', '--check'], { request: { environment: { RUST_LOG: item.tokenFile } } })));
    await waitForState(item.service, root.result.id, ['RUNNING']);
    const nested = await call(item.service, submit(managed(item.workspace, ['test'], { execution: { mode: 'NESTED', parentId: root.result.id, capabilityToken: await tokenFor(item) } })));
    const result = await call(item.service, command('WATCH', nested.result.id));
    assert.equal(result.result.status, 'FAILED'); assert.equal(result.result.receipt.reason, 'RK_NESTED_HEAVY_ROOT_REQUIRED');
    await call(item.service, command('WATCH', root.result.id));
  } finally { await item.cleanup(); }
});

test('output replay is byte-bounded while attached watcher receives full stream & detaches on backpressure', async () => {
  const item = await fixture({ delayMs: 500, bounds: { outputBufferBytes: 1024, eventHistoryBytes: 1024, outputHeadBytes: 16, outputTailBytes: 16 } });
  try {
    const submitted = await call(item.service, submit(managed(item.workspace, ['test'], { request: { environment: { CI: 'out:4096' } } })));
    await waitForState(item.service, submitted.result.id, ['RUNNING']);
    const attached = []; const detached = item.service.dispatch(command('WATCH', submitted.result.id), (event) => { attached.push(event); return event.event?.type === 'STDOUT' ? false : true; });
    const detachedResult = await detached; assert.equal(detachedResult.type, 'WATCH_DETACHED'); assert.equal(detachedResult.reason, 'RK_WATCH_BACKPRESSURE');
    const complete = await call(item.service, command('WATCH', submitted.result.id)); assert.equal(complete.result.status, 'SUCCEEDED');
    assert.equal(attached.filter((event) => event.event?.type === 'STDOUT').map((event) => event.event.data).join('').length, 4096);
    assert.ok(Buffer.byteLength(complete.result.receipt.output.head) <= 16); assert.ok(Buffer.byteLength(complete.result.receipt.output.tail) <= 16);
    assert.equal(complete.result.receipt.output.digest, createHash('sha256').update('x'.repeat(4096)).digest('hex'));
  } finally { await item.cleanup(); }
});

test('statfs failure blocks new BUILD but CONTROL remains available', async () => {
  const item = await fixture({ statfsError: new Error('probe unavailable') });
  try {
    const heavy = await call(item.service, submit(managed(item.workspace, ['test'])));
    const heavyResult = await call(item.service, command('WATCH', heavy.result.id));
    assert.equal(heavyResult.result.status, 'FAILED'); assert.equal(heavyResult.result.receipt.reason, 'RK_STORAGE_DEGRADED');
    const control = await call(item.service, submit(managed(item.workspace, ['--version'])));
    const controlResult = await call(item.service, command('WATCH', control.result.id)); assert.equal(controlResult.result.status, 'SUCCEEDED');
  } finally { await item.cleanup(); }
});

test('active statfs degradation is diagnostic & never kills admitted BUILD', async () => {
  let calls = 0; const item = await fixture({ delayMs: 250, statfsOperation: async () => { calls += 1; if (calls > 1) throw new Error('active probe failed'); return { bavail: 100_000_000, bsize: 1 }; } });
  try {
    const submitted = await call(item.service, submit(managed(item.workspace, ['test'])));
    const result = await call(item.service, command('WATCH', submitted.result.id));
    assert.equal(result.result.status, 'SUCCEEDED'); assert.ok(calls > 1);
  } finally { await item.cleanup(); }
});

test('changed pinned tool returns typed TOOL_CHANGED without hashing unchanged requests', async () => {
  const item = await fixture();
  try {
    await writeFile(item.bin, '#!/usr/bin/env node\nprocess.exit(0);\n'); await chmod(item.bin, 0o755);
    const result = await call(item.service, submit(managed(item.workspace, ['--version'])));
    assert.equal(result.result.status, 'REJECTED'); assert.equal(result.result.code, 'TOOL_CHANGED'); assert.ok(result.events.some((event) => event.type === 'TOOL_CHANGED'));
  } finally { await item.cleanup(); }
});

test('terminal event-log failure remains typed in durable STATUS receipt', async () => {
  const eventLog = { async appendResult(event) { if (event.type === 'TERMINAL') throw Object.assign(new Error('terminal fsync failed'), { code: 'RK_EVENT_LOG_DURABILITY_UNCONFIRMED' }); return { durable: true }; }, async append() {}, async flush() {} };
  const item = await fixture({ eventLog });
  try {
    const submitted = await call(item.service, submit(managed(item.workspace, ['--version']))); const completed = await call(item.service, command('WATCH', submitted.result.id));
    assert.equal(completed.result.receipt.eventLog.durable, false);
    const status = await call(item.service, command('STATUS', submitted.result.id));
    assert.equal(status.result.receipt.eventLog.durable, false); assert.equal(status.result.receipt.eventLog.failures[0].event, 'TERMINAL');
  } finally { await item.cleanup(); }
});

test('COMMITTED activation fence must match running runtime identity', async () => {
  const item = await fixture({ activationFence: { schema: 'rightkit.build-control.runtime-activation.v1', state: 'COMMITTED', runtime: { ...SOURCE_RUNTIME_IDENTITY, version: 'wrong-runtime' }, tokenDigest: 'f'.repeat(64) } });
  try { await assert.rejects(call(item.service, command('STATUS', 'missing')), (error) => error.code === 'RK_ACTIVATION_FENCE_INVALID'); }
  finally { await item.cleanup(); }
});

test('nested depth two inherits root capability environment & terminalizes after descendants', async () => {
  const item = await fixture({ delayMs: 300 });
  try {
    const root = await call(item.service, submit(managed(item.workspace, ['test'], { request: { environment: { RUST_LOG: item.tokenFile } } })));
    await waitForState(item.service, root.result.id, ['RUNNING']); const token = await tokenFor(item);
    const child = await call(item.service, submit(managed(item.workspace, ['test'], { execution: { mode: 'NESTED', parentId: root.result.id, capabilityToken: token }, request: { environment: { RUST_LOG: item.tokenFile } } })));
    await waitForState(item.service, child.result.id, ['RUNNING']);
    const grandchild = await call(item.service, submit(managed(item.workspace, ['--version'], { execution: { mode: 'NESTED', parentId: child.result.id, capabilityToken: token } })));
    const result = await call(item.service, command('WATCH', grandchild.result.id)); assert.equal(result.result.status, 'SUCCEEDED');
    const rootResult = await call(item.service, command('WATCH', root.result.id)); assert.equal(rootResult.result.status, 'SUCCEEDED');
  } finally { await item.cleanup(); }
});

test('root remains REAPING while detached child runs & queued BUILD starts after root domain drains', async () => {
  const item = await fixture({ delayMs: 100 });
  try {
    const root = await call(item.service, submit(managed(item.workspace, ['test'], { request: { environment: { RUST_LOG: item.tokenFile, CI: '100' } } })));
    await waitForState(item.service, root.result.id, ['RUNNING']); const token = await tokenFor(item);
    const child = await call(item.service, submit(managed(item.workspace, ['test'], { execution: { mode: 'NESTED', parentId: root.result.id, capabilityToken: token }, request: { environment: { CI: '500' } } })));
    const queued = await call(item.service, submit(managed(item.workspace, ['test'])));
    await waitForState(item.service, root.result.id, ['REAPING']);
    assert.equal((await call(item.service, command('STATUS', queued.result.id))).result.state, 'QUEUED');
    assert.equal((await call(item.service, command('WATCH', child.result.id))).result.status, 'SUCCEEDED');
    assert.equal((await call(item.service, command('WATCH', root.result.id))).result.status, 'SUCCEEDED');
    assert.equal((await call(item.service, command('WATCH', queued.result.id))).result.status, 'SUCCEEDED');
  } finally { await item.cleanup(); }
});
