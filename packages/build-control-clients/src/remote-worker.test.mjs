import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, mkdir, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';
import { createJobCommand, JOB_REQUEST_TYPES } from './index.mjs';
import { encodePayload } from './remote-protocol.mjs';
import { createRemoteWorker, isDirectMain, RemoteWorkerError } from './remote-worker.mjs';

function header(path, size, typeflag = '0') {
  const value = Buffer.alloc(512, 0);
  value.write(path, 0, 100, 'utf8');
  value.write(size.toString(8).padStart(11, '0') + '\0', 124, 12, 'ascii');
  value.write(typeflag, 156, 1, 'ascii');
  value.write('ustar\0', 257, 6, 'ascii');
  value.write('00', 263, 2, 'ascii');
  return value;
}

function archiveFixture() {
  const payload = Buffer.from('[package]\nname = "remote-fixture"\n', 'utf8');
  const padded = Buffer.alloc(Math.ceil(payload.length / 512) * 512);
  payload.copy(padded);
  return Buffer.concat([header('Cargo.toml', payload.length), padded, Buffer.alloc(1024)]);
}

function submitPayload(archive, requestId, { deadlineMs = 60_000, digest = createHash('sha256').update(archive).digest('hex') } = {}) {
  return encodePayload({
    v: 1, op: 'SUBMIT', requestId, repositoryKey: 'repo', digest,
    bytes: archive.length, argv: ['check'], deadlineMs, createdAt: '2026-08-24T00:00:00.000Z',
  });
}

async function workerFixture({ send, now = () => Date.parse('2026-08-24T00:00:00.000Z'), slotsPerRepository = 3 } = {}) {
  const rootDir = await mkdtemp(join(tmpdir(), 'rightkit-remote-worker-'));
  const archive = archiveFixture();
  const digest = createHash('sha256').update(archive).digest('hex');
  await mkdir(join(rootDir, 'remote-inputs'), { recursive: true });
  await writeFile(join(rootDir, 'remote-inputs', `${digest}.tar`), archive);
  const worker = createRemoteWorker({
    rootDir, endpoint: 'test-broker', slotsPerRepository, now,
    send, loadIdentity: async () => SOURCE_RUNTIME_IDENTITY,
  });
  return { rootDir, archive, digest, worker };
}

test('CLI entry detection canonicalizes runtime-current symlink before comparison', () => {
  const canonicalize = (path) => path.replace('runtime-current', 'runtime-content-digest');
  assert.equal(isDirectMain({
    modulePath: 'D:\\.rightkit-managed\\runtime-content-digest\\build-control-clients\\src\\remote-worker.mjs',
    argv1: 'D:\\.rightkit-managed\\runtime-current\\build-control-clients\\src\\remote-worker.mjs',
    canonicalize,
  }), true);
});

test('worker verifies digest, installs isolated replica, and submits through host-local RightKit broker', async () => {
  const calls = [];
  const fixture = await workerFixture({ send: async (command) => {
    calls.push(command);
    if (command.type === JOB_REQUEST_TYPES.SUBMIT) return { type: 'SUBMITTED', id: 'job-1', state: 'QUEUED' };
    throw new Error(`unexpected broker operation ${command.type}`);
  } });
  try {
    const result = await fixture.worker.submit(submitPayload(fixture.archive, 'rr-submit'));
    assert.equal(result.state, 'QUEUED');
    assert.equal(result.brokerId, 'job-1');
    assert.equal(calls.length, 1);
    assert.equal(calls[0].type, JOB_REQUEST_TYPES.SUBMIT);
    assert.equal(calls[0].job.shim, 'cargo');
    assert.deepEqual(calls[0].job.request.argv, ['check']);
    const replica = join(fixture.rootDir, 'remote-workspaces', 'repo', 'slot1', 'Cargo.toml');
    assert.equal(await readFile(replica, 'utf8'), '[package]\nname = "remote-fixture"\n');
    const request = JSON.parse(await readFile(join(fixture.rootDir, 'state', 'remote-requests', 'rr-submit.json'), 'utf8'));
    assert.equal(request.state, 'QUEUED');
    assert.equal(request.brokerId, 'job-1');
  } finally { await rm(fixture.rootDir, { recursive: true, force: true }); }
});

test('worker fails closed on digest mismatch before broker submission or replica replacement', async () => {
  const calls = [];
  const fixture = await workerFixture({ send: async (command) => { calls.push(command); return { type: 'SUBMITTED', id: 'unexpected' }; } });
  try {
    await writeFile(join(fixture.rootDir, 'remote-inputs', `${'b'.repeat(64)}.tar`), fixture.archive);
    const bad = submitPayload(fixture.archive, 'rr-bad-digest', { digest: 'b'.repeat(64) });
    await assert.rejects(() => fixture.worker.submit(bad), (error) => error instanceof RemoteWorkerError && error.code === 'REMOTE_DIGEST_MISMATCH');
    assert.equal(calls.length, 0);
    await assert.rejects(stat(join(fixture.rootDir, 'remote-workspaces', 'repo', 'slot1')), { code: 'ENOENT' });
  } finally { await rm(fixture.rootDir, { recursive: true, force: true }); }
});

test('slot exclusion preserves active accepted job and reports no slot', async () => {
  const calls = [];
  const fixture = await workerFixture({ slotsPerRepository: 1, send: async (command) => {
    calls.push(command);
    if (command.type === JOB_REQUEST_TYPES.SUBMIT) return { type: 'SUBMITTED', id: 'job-active', state: 'QUEUED' };
    if (command.type === JOB_REQUEST_TYPES.STATUS) return { type: 'STATUS', id: 'job-active', state: 'RUNNING' };
    throw new Error('unexpected broker operation');
  } });
  try {
    await fixture.worker.submit(submitPayload(fixture.archive, 'rr-active'));
    await assert.rejects(() => fixture.worker.submit(submitPayload(fixture.archive, 'rr-second')), (error) => error.code === 'REMOTE_NO_SLOT');
    assert.deepEqual(calls.map(({ type }) => type), [JOB_REQUEST_TYPES.SUBMIT, JOB_REQUEST_TYPES.STATUS]);
  } finally { await rm(fixture.rootDir, { recursive: true, force: true }); }
});

test('single-shot STATUS maps missing broker job to terminal LOST, while deadline cancels once', async () => {
  const archive = archiveFixture();
  let clock = Date.parse('2026-08-24T00:00:00.000Z');
  const lostCalls = [];
  const lostFixture = await workerFixture({ now: () => clock, send: async (command) => {
    lostCalls.push(command);
    if (command.type === JOB_REQUEST_TYPES.SUBMIT) return { type: 'SUBMITTED', id: 'job-lost', state: 'QUEUED' };
    throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' });
  } });
  try {
    await lostFixture.worker.submit(submitPayload(archive, 'rr-lost'));
    const lost = await lostFixture.worker.status(encodePayload({ v: 1, op: 'STATUS', requestId: 'rr-lost' }));
    assert.deepEqual({ state: lost.state, reason: lost.reason, terminal: lost.terminal }, { state: 'LOST', reason: 'REMOTE_JOB_LOST', terminal: true });
  } finally { await rm(lostFixture.rootDir, { recursive: true, force: true }); }

  const deadlineCalls = [];
  const deadlineFixture = await workerFixture({ now: () => clock, send: async (command) => {
    deadlineCalls.push(command);
    if (command.type === JOB_REQUEST_TYPES.SUBMIT) return { type: 'SUBMITTED', id: 'job-deadline', state: 'QUEUED' };
    if (command.type === JOB_REQUEST_TYPES.STATUS) return { type: 'STATUS', id: 'job-deadline', state: 'RUNNING' };
    if (command.type === JOB_REQUEST_TYPES.CANCEL) return { type: 'CANCEL_REQUESTED', id: 'job-deadline' };
    throw new Error('unexpected broker operation');
  } });
  try {
    await deadlineFixture.worker.submit(submitPayload(archive, 'rr-deadline'));
    clock += 60_001;
    const terminal = await deadlineFixture.worker.status(encodePayload({ v: 1, op: 'STATUS', requestId: 'rr-deadline' }));
    assert.deepEqual({ state: terminal.state, reason: terminal.reason, terminal: terminal.terminal }, { state: 'DEADLINE', reason: 'REMOTE_JOB_DEADLINE', terminal: true });
    assert.deepEqual(deadlineCalls.map(({ type }) => type), [JOB_REQUEST_TYPES.SUBMIT, JOB_REQUEST_TYPES.STATUS, JOB_REQUEST_TYPES.CANCEL]);
  } finally { await rm(deadlineFixture.rootDir, { recursive: true, force: true }); }
});
