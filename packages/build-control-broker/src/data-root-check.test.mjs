import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { randomUUID } from 'node:crypto';
import net from 'node:net';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { BUILD_CONTROL_PROTOCOL, SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';
import { probeDataRoot, DataRootService } from './data-root-check.mjs';
import { createThinJobServer } from './thin-job-service.mjs';

const message = (type) => ({ type, id: 'health', protocol: BUILD_CONTROL_PROTOCOL, runtime: SOURCE_RUNTIME_IDENTITY });

test('hanging child probe times out without holding broker event loop', async () => {
  const child = new EventEmitter(); child.stdout = new EventEmitter();
  let killed = null; let detached = false; let ticked = false;
  child.kill = (signal) => { killed = signal; }; child.unref = () => {};
  child.stdout.destroy = () => { detached = true; };
  setTimeout(() => { ticked = true; }, 1);
  const result = await probeDataRoot('/denied', { timeoutMs: 20, spawn: () => child });
  assert.deepEqual(result, { ok: false, diagnostic: { code: 'EXTERNAL_ROOT_UNREADABLE', path: '/denied', errno: 'timeout' } });
  assert.equal(ticked, true); assert.equal(killed, 'SIGKILL'); assert.equal(detached, true);
});

test('isolated real probe reports missing root & accepts readable temporary root', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'rk-probe-')); t.after(() => rm(root, { recursive: true, force: true }));
  assert.deepEqual(await probeDataRoot(root), { ok: true });
  assert.equal((await probeDataRoot(join(root, 'missing'))).diagnostic.errno, 'ENOENT');
});

test('unreadable root binds socket, answers STATUS, refuses jobs & recovers periodically', async (t) => {
  let available = false; let created = 0; let dispatched = 0; let stopped = 0; const logs = [];
  const service = new DataRootService({ path: '/denied', intervalMs: 15,
    probe: async () => available ? { ok: true } : { ok: false, diagnostic: { code: 'EXTERNAL_ROOT_UNREADABLE', path: '/denied', errno: 'EACCES' } },
    createService: async () => { created++; return { dispatch: async (m, emit) => { dispatched++; emit({ type: 'HANDSHAKE', runtime: SOURCE_RUNTIME_IDENTITY }); const response = { type: m.type, state: 'READY' }; emit(response); return response; }, shutdown: async () => { stopped++; } }; },
    log: async (event) => logs.push(event) });
  const root = await mkdtemp(join(tmpdir(), 'rk-gated-'));
  const endpoint = process.platform === 'win32' ? String.raw`\\.\pipe\rk-gated-${randomUUID()}` : join(root, 'broker.sock');
  const server = createThinJobServer({ service, endpoint });
  await new Promise((resolve) => server.listen(endpoint, resolve));
  t.after(async () => { await service.shutdown(); await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true, force: true }); });
  await service.check();
  const status = await new Promise((resolve, reject) => {
    const socket = net.createConnection(endpoint); let data = '';
    socket.on('connect', () => socket.write(`${JSON.stringify(message('STATUS'))}\n`)); socket.on('data', (chunk) => { data += chunk; });
    socket.on('error', reject); socket.on('end', () => resolve(JSON.parse(data.trim().split('\n').at(-1))));
  });
  assert.equal(status.type, 'STATUS'); assert.equal(status.code, 'EXTERNAL_ROOT_UNREADABLE'); assert.equal(status.errno, 'EACCES');
  for (const type of ['SUBMIT', 'MANAGED_AGENT', 'STORAGE_GC']) {
    const result = await service.dispatch(message(type));
    assert.equal(result.code, 'EXTERNAL_ROOT_UNREADABLE'); assert.equal(result.exitCode, 125);
  }
  assert.equal(created, 0); assert.equal(dispatched, 0);
  const restored = new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('periodic recovery did not run')), 1000);
    const interval = setInterval(() => { if (created === 1 && !service.diagnostic) { clearInterval(interval); clearTimeout(timeout); resolve(); } }, 5);
  });
  available = true; await restored;
  assert.equal((await service.dispatch(message('SUBMIT'))).state, 'READY');
  assert.equal(logs.filter((e) => e.type === 'DATA_ROOT_UNAVAILABLE').length, 1);
  assert.equal(logs.filter((e) => e.type === 'DATA_ROOT_RESTORED').length, 1);
  available = false; await service.check();
  assert.equal((await service.dispatch(message('SUBMIT'))).code, 'EXTERNAL_ROOT_UNREADABLE');
  assert.equal(created, 1);
  await service.shutdown(); assert.equal(stopped, 1);
});

test('initialization cannot block STATUS & invalid runtime cannot bypass gate', async () => {
  let complete;
  const service = new DataRootService({ path: '/data', probe: async () => ({ ok: true }), createService: () => new Promise((resolve) => { complete = resolve; }) });
  await service.check(); await Promise.resolve();
  assert.equal((await service.dispatch(message('STATUS'))).code, 'RK_DATA_ROOT_INITIALIZING');
  await assert.rejects(service.dispatch({ ...message('STATUS'), protocol: 'bad' }), /v2 protocol/);
  complete({ dispatch: async () => ({ state: 'READY' }), shutdown: async () => {} });
  await service.ready(); assert.equal((await service.dispatch(message('STATUS'))).state, 'READY');
  await service.shutdown();
});

test('broker entry binds from control storage & logs LISTENING before denied data startup', { skip: process.platform === 'win32' && 'POSIX broker entry uses the data-root gate; Windows initializes its native service directly' }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'rk-broker-start-')); const control = join(root, 'control'); const data = join(root, 'missing-data');
  await mkdir(control, { mode: 0o700 });
  const endpoint = join(control, 'broker.sock'); const configPath = join(control, 'config.json');
  await writeFile(configPath, JSON.stringify({ schemaVersion: 2, broker: { controlRoot: control, externalRoot: { path: data, volumeId: 'test', mounted: false, writable: false, internal: false }, capacityBytes: 1024, stateFile: join(control, 'state.db'), activationFencePath: join(control, 'state', 'activation.json'), receiptsDir: join(data, 'receipts') }, unixSocket: endpoint, tools: [], shims: {}, defaults: {}, bounds: {} }));
  const child = spawn(process.execPath, [fileURLToPath(new URL('./broker.mjs', import.meta.url)), configPath], { stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(async () => {
    if (child.exitCode === null) await new Promise((resolve) => { child.once('exit', resolve); child.kill('SIGTERM'); });
    await rm(root, { recursive: true, force: true });
  });
  let output = ''; let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`broker startup timeout: ${stderr}`)), 3000);
    child.once('error', (error) => { clearTimeout(timer); reject(error); });
    child.once('exit', (code) => { clearTimeout(timer); reject(new Error(`broker exited ${code}: ${stderr}`)); });
    child.stdout.on('data', (chunk) => { output += chunk; if (output.includes('DATA_ROOT_UNAVAILABLE')) { clearTimeout(timer); resolve(); } });
  });
  assert.ok(output.indexOf('LISTENING') < output.indexOf('DATA_ROOT_UNAVAILABLE'));
  const response = await new Promise((resolve, reject) => {
    const socket = net.createConnection(endpoint); let result = '';
    socket.once('connect', () => socket.write(`${JSON.stringify(message('STATUS'))}\n`));
    socket.on('data', (chunk) => { result += chunk; }); socket.once('error', reject);
    socket.once('end', () => resolve(JSON.parse(result.trim().split('\n').at(-1))));
  });
  assert.equal(response.type, 'STATUS'); assert.equal(response.code, 'EXTERNAL_ROOT_UNREADABLE'); assert.equal(response.path, data); assert.equal(response.errno, 'ENOENT');
  const lifecycle = (await readFile(join(control, 'logs', 'lifecycle.ndjson'), 'utf8')).trim().split('\n').map(JSON.parse);
  assert.deepEqual(lifecycle.map((event) => event.type), ['LISTENING', 'DATA_ROOT_UNAVAILABLE']);
});
