// Windows remote request/replica worker for dual-host Rust routing
// (docs/RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md). Invoked over the ssh bridge with
// a validated base64url payload; performs no long-running work after broker
// submission, so closing SSH cannot affect a running Cargo job — the host-local
// RightKit broker owns the process inside its Job Object.
//
// Durability order per contract §7 (write-before-ack):
//   1. durable slot lease + PREPARING remote request
//   2. verify snapshot digest & install replica atomically
//   3. submit to the already-persistent broker service
//   4. atomically persist broker ID + QUEUED + absolute deadline
//   5. acknowledge the remote request ID
//
// STATUS maps a vanished acknowledged job to terminal REMOTE_JOB_LOST and an
// expired absolute deadline to terminal REMOTE_JOB_DEADLINE, so scheduled
// 15-minute agent wakes always terminate.

import { createHash, randomUUID } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createJobCommand, createRightkitRequest, JOB_REQUEST_TYPES, JOB_RESPONSE_TYPES, loadRuntimeIdentity, sendJobCommand } from './index.mjs';
import { decodePayload, generateRemoteRequestId, isTerminalRemoteState } from './remote-protocol.mjs';
import { extractArchive, inspectArchive } from './remote-tar.mjs';

const SLOTS_PER_REPOSITORY = 3;
const STALE_PREPARING_MS = 10 * 60_000;

export class RemoteWorkerError extends Error {
  constructor(message, code) {
    super(message);
    this.name = 'RemoteWorkerError';
    this.code = code;
  }
}

function sha256Hex(buffer) { return createHash('sha256').update(buffer).digest('hex'); }

async function writeJsonAtomic(path, value) {
  const temporary = `${path}.${process.pid}.${randomUUID().slice(0, 8)}.tmp`;
  await writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, { encoding: 'utf8', flag: 'wx' });
  await rename(temporary, path);
}

async function readJson(path) {
  try { return JSON.parse(await readFile(path, 'utf8')); } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

export function defaultRemoteRoot() { return process.env.RIGHTKIT_REMOTE_ROOT ?? 'D:\\.rightkit-managed'; }

function isBrokerJobNotFound(error) {
  return error?.code === 'RK_JOB_NOT_FOUND' || /job not found/i.test(error?.message ?? '');
}

/** Create the worker handlers against explicit storage roots, endpoint, clock,
 * and broker-send function so tests drive every path without a live host. */
export function createRemoteWorker({
  rootDir = defaultRemoteRoot(),
  endpoint = process.env.RIGHTKIT_BUILD_BROKER_SOCKET,
  slotsPerRepository = SLOTS_PER_REPOSITORY,
  now = Date.now,
  send = sendJobCommand,
  loadIdentity = loadRuntimeIdentity,
} = {}) {
  if (!Number.isSafeInteger(slotsPerRepository) || slotsPerRepository < 1) throw new TypeError('slotsPerRepository must be a positive integer');
  const paths = Object.freeze({
    root: rootDir,
    inputs: join(rootDir, 'remote-inputs'),
    workspaces: join(rootDir, 'remote-workspaces'),
    requests: join(rootDir, 'state', 'remote-requests'),
    slots: join(rootDir, 'state', 'remote-slots'),
  });

  const requestPath = (id) => join(paths.requests, `${id}.json`);
  const slotLeasePath = (repositoryKeyValue, slot) => join(paths.slots, `${repositoryKeyValue}.${slot}.json`);
  const slotDirOf = (repositoryKeyValue, slot) => join(paths.workspaces, repositoryKeyValue, `slot${slot}`);

  async function persistRequest(record) {
    await mkdir(paths.requests, { recursive: true });
    await writeJsonAtomic(requestPath(record.id), Object.freeze({ ...record, updatedAt: new Date(now()).toISOString() }));
  }

  /** One broker STATUS for a job ID. Returns null when the broker reports the
   * acknowledged job unknown (RK_JOB_NOT_FOUND); typed failure otherwise. */
  async function brokerStatus(identity, jobId) {
    try {
      return await send(createJobCommand(JOB_REQUEST_TYPES.STATUS, { id: jobId, runtimeIdentity: identity }), { endpoint, completeOn: new Set([JOB_RESPONSE_TYPES.STATUS]) });
    } catch (error) {
      if (isBrokerJobNotFound(error)) return null;
      throw new RemoteWorkerError(`broker STATUS failed: ${error.message}`, 'REMOTE_BROKER_UNAVAILABLE');
    }
  }

  async function cancelBrokerJob(identity, jobId) {
    try {
      await send(createJobCommand(JOB_REQUEST_TYPES.CANCEL, { id: jobId, runtimeIdentity: identity, source: 'USER_CANCEL' }), { endpoint, completeOn: new Set([JOB_RESPONSE_TYPES.CANCEL_REQUESTED]) });
    } catch (error) {
      if (!isBrokerJobNotFound(error)) throw new RemoteWorkerError(`broker CANCEL failed: ${error.message}`, 'REMOTE_BROKER_UNAVAILABLE');
    }
  }

  function brokerTerminal(statusEvent) {
    return Boolean(statusEvent.receipt) || ['SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT'].includes(statusEvent.state);
  }

  async function resolveOwnerFate(identity, owner) {
    // Exactly one broker STATUS decides between terminal receipt & LOST.
    const status = await brokerStatus(identity, owner.brokerId);
    if (status === null) {
      const lost = { ...owner, state: 'LOST', reason: 'REMOTE_JOB_LOST', detail: 'acknowledged job missing from broker' };
      await persistRequest(lost);
      return lost;
    }
    if (brokerTerminal(status)) {
      const terminal = { ...owner, state: 'TERMINAL', brokerState: status.state, exitCode: status.receipt?.exitCode ?? null, receipt: status.receipt ?? null };
      await persistRequest(terminal);
      return terminal;
    }
    return { ...owner, brokerState: status.state };
  }

  /** Slot ownership is a durable record, not a process-held lock. A slot is
   * acquirable when unleased, or its owner request is absent/stale-PREPARING/
   * already-terminal, or one broker STATUS proves the owner job terminal/lost,
   * or the owner's absolute deadline passed (cancel first, then DEADLINE). */
  async function acquireSlot(identity, repositoryKeyValue, requestId) {
    await mkdir(paths.slots, { recursive: true });
    for (let slot = 1; slot <= slotsPerRepository; slot += 1) {
      const leasePath = slotLeasePath(repositoryKeyValue, slot);
      const lease = await readJson(leasePath);
      if (!lease) {
        const fresh = { ownerRequestId: requestId, acquiredAt: new Date(now()).toISOString() };
        await writeJsonAtomic(leasePath, fresh);
        return { slot, leasePath };
      }
      const owner = lease.ownerRequestId === requestId ? null : await readJson(requestPath(lease.ownerRequestId));
      const stalePreparing = owner?.state === 'PREPARING' && now() - Date.parse(owner.createdAt) > STALE_PREPARING_MS;
      if (!owner || stalePreparing || isTerminalRemoteState(owner.state)) {
        await writeJsonAtomic(leasePath, { ownerRequestId: requestId, acquiredAt: new Date(now()).toISOString(), reclaimedFrom: lease.ownerRequestId });
        if (stalePreparing) await persistRequest({ ...owner, state: 'FAILED', reason: 'REMOTE_PREPARING_STALE' });
        return { slot, leasePath };
      }
      if (owner.brokerId !== undefined) {
        const resolved = await resolveOwnerFate(identity, owner);
        if (isTerminalRemoteState(resolved.state)) {
          await writeJsonAtomic(leasePath, { ownerRequestId: requestId, acquiredAt: new Date(now()).toISOString(), reclaimedFrom: lease.ownerRequestId });
          return { slot, leasePath };
        }
        if (now() >= Date.parse(resolved.deadlineAt)) {
          await cancelBrokerJob(identity, resolved.brokerId).catch(() => {});
          await persistRequest({ ...resolved, state: 'DEADLINE', reason: 'REMOTE_JOB_DEADLINE', detail: 'absolute remote deadline expired during slot reclaim' });
          await writeJsonAtomic(leasePath, { ownerRequestId: requestId, acquiredAt: new Date(now()).toISOString(), reclaimedFrom: lease.ownerRequestId });
          return { slot, leasePath };
        }
      }
    }
    throw new RemoteWorkerError(`no replica slot available for ${repositoryKeyValue}`, 'REMOTE_NO_SLOT');
  }

  async function installSnapshot(record, archiveBuffer) {
    inspectArchive(archiveBuffer); // typed rejection before anything is written
    const destination = slotDirOf(record.repositoryKey, record.slot);
    const staging = `${destination}.staging-${record.id}`;
    const retired = `${destination}.retired-${record.id}`;
    await rm(staging, { recursive: true, force: true });
    await mkdir(staging, { recursive: true });
    await extractArchive(archiveBuffer, staging);
    let replaced = false;
    try { await rename(destination, retired); replaced = true; } catch (error) { if (error.code !== 'ENOENT') throw error; }
    try {
      await rename(staging, destination);
    } catch (error) {
      if (replaced) await rename(retired, destination).catch(() => {}); // restore rather than half-install
      await rm(staging, { recursive: true, force: true }).catch(() => {});
      throw error;
    }
    await rm(retired, { recursive: true, force: true }).catch(() => {});
  }

  function publicView(record) {
    return Object.freeze({
      ok: true,
      requestId: record.id,
      state: record.state,
      terminal: isTerminalRemoteState(record.state),
      ...(record.reason ? { reason: record.reason } : {}),
      ...(record.detail ? { detail: record.detail } : {}),
      ...(record.exitCode !== undefined && record.exitCode !== null ? { exitCode: record.exitCode } : {}),
      ...(record.receipt ? { receipt: record.receipt } : {}),
      ...(record.deadlineAt ? { deadlineAt: record.deadlineAt } : {}),
    });
  }

  return {
    paths,
    async submit(payloadText) {
      const payload = decodePayload(payloadText);
      if (!endpoint) throw new RemoteWorkerError('RightKit broker endpoint is not configured', 'REMOTE_BROKER_UNAVAILABLE');
      const identity = await loadIdentity();
      const requestId = payload.requestId || generateRemoteRequestId(now);
      const { slot, leasePath } = await acquireSlot(identity, payload.repositoryKey, requestId);
      // Write-before-ack step 1: durable PREPARING request + selected slot.
      let record = {
        id: requestId, op: 'SUBMIT', state: 'PREPARING',
        repositoryKey: payload.repositoryKey, digest: payload.digest, bytes: payload.bytes,
        argv: payload.argv, slot, leasePath, deadlineMs: payload.deadlineMs,
        createdAt: new Date(now()).toISOString(),
      };
      await persistRequest(record);
      const inputPath = join(paths.inputs, `${payload.digest}.tar`);
      let archiveBuffer;
      try {
        archiveBuffer = await readFile(inputPath);
      } catch {
        await persistRequest({ ...record, state: 'FAILED', reason: 'REMOTE_TRANSFER_FAILED', detail: 'snapshot input archive is missing' });
        throw new RemoteWorkerError(`input archive ${payload.digest} is missing`, 'REMOTE_TRANSFER_FAILED');
      }
      if (sha256Hex(archiveBuffer) !== payload.digest || archiveBuffer.byteLength !== payload.bytes) {
        await persistRequest({ ...record, state: 'FAILED', reason: 'REMOTE_DIGEST_MISMATCH', detail: 'snapshot digest mismatch before extraction' });
        throw new RemoteWorkerError('digest mismatch: refusing to install snapshot', 'REMOTE_DIGEST_MISMATCH');
      }
      await installSnapshot(record, archiveBuffer);
      // Write-before-ack steps 3-4: submit to the persistent broker service,
      // durably map broker ID + QUEUED + absolute deadline, then acknowledge.
      const workspace = slotDirOf(payload.repositoryKey, slot);
      const message = createRightkitRequest({ argv: ['cargo', ...payload.argv], workspace, environment: {}, runtimeIdentity: identity });
      let submitted;
      try {
        submitted = await send(createJobCommand(JOB_REQUEST_TYPES.SUBMIT, { message, runtimeIdentity: identity }), { endpoint, completeOn: new Set([JOB_RESPONSE_TYPES.SUBMITTED]) });
      } catch (error) {
        await persistRequest({ ...record, state: 'FAILED', reason: 'REMOTE_BROKER_REJECTION', detail: error.message });
        throw new RemoteWorkerError(`broker rejected remote job: ${error.message}`, 'REMOTE_BROKER_REJECTION');
      }
      const deadlineAt = new Date(now() + payload.deadlineMs).toISOString();
      record = { ...record, state: 'QUEUED', brokerId: submitted.id, brokerState: submitted.state ?? 'QUEUED', deadlineAt };
      await persistRequest(record);
      return Object.freeze({ ok: true, requestId, state: 'QUEUED', slot, brokerId: submitted.id, digest: payload.digest, deadlineAt });
    },

    async status(payloadText) {
      const payload = decodePayload(payloadText);
      const record = await readJson(requestPath(payload.requestId));
      if (!record) throw new RemoteWorkerError(`unknown remote request ${payload.requestId}`, 'REMOTE_REQUEST_NOT_FOUND');
      if (isTerminalRemoteState(record.state)) return publicView(record);
      if (record.brokerId === undefined) {
        // Never reached broker submission: stale PREPARING recovery marks failed.
        if (now() - Date.parse(record.createdAt) > STALE_PREPARING_MS) {
          const failed = { ...record, state: 'FAILED', reason: 'REMOTE_PREPARING_STALE' };
          await persistRequest(failed);
          return publicView(failed);
        }
        return publicView(record);
      }
      const identity = await loadIdentity();
      const resolved = await resolveOwnerFate(identity, record);
      if (isTerminalRemoteState(resolved.state)) return publicView(resolved);
      // Absolute remote deadline: request cancellation, then go terminal so
      // scheduled wakes terminate even while the broker keeps running.
      if (now() >= Date.parse(resolved.deadlineAt)) {
        await cancelBrokerJob(identity, resolved.brokerId).catch(() => {});
        const deadlineRecord = { ...resolved, state: 'DEADLINE', reason: 'REMOTE_JOB_DEADLINE', detail: 'absolute remote deadline expired' };
        await persistRequest(deadlineRecord);
        return publicView(deadlineRecord);
      }
      return publicView(resolved);
    },

    async cancel(payloadText) {
      const payload = decodePayload(payloadText);
      const record = await readJson(requestPath(payload.requestId));
      if (!record) throw new RemoteWorkerError(`unknown remote request ${payload.requestId}`, 'REMOTE_REQUEST_NOT_FOUND');
      if (isTerminalRemoteState(record.state)) return publicView(record);
      if (record.brokerId === undefined) {
        const cancelled = { ...record, state: 'CANCELLED', reason: 'USER_CANCEL' };
        await persistRequest(cancelled);
        return publicView(cancelled);
      }
      const identity = await loadIdentity();
      await cancelBrokerJob(identity, record.brokerId);
      await persistRequest({ ...record, state: 'CANCELLED_PENDING_RECEIPT' });
      return Object.freeze({ ok: true, requestId: record.id, state: 'CANCEL_REQUESTED', terminal: false });
    },
  };
}

export function isDirectMain({ modulePath = fileURLToPath(import.meta.url), argv1 = process.argv[1], canonicalize = (path) => realpathSync(resolve(path)) } = {}) {
  if (!argv1) return false;
  try {
    return canonicalize(modulePath) === canonicalize(argv1);
  } catch { return false; }
}

/** CLI entrypoint used over ssh: node remote-worker.mjs <op> <base64url>.
 * Emits exactly one JSON line on stdout; errors are one JSON line on stderr. */
export async function runRemoteWorkerCli(argv = process.argv.slice(2), worker = null) {
  const instance = worker ?? createRemoteWorker();
  const [operation, encoded] = argv;
  if (!['submit', 'status', 'cancel'].includes(operation) || !encoded) {
    process.stderr.write('usage: remote-worker.mjs <submit|status|cancel> <base64url-payload>\n');
    process.exitCode = 125;
    return;
  }
  try {
    const result = await instance[operation](encoded);
    process.stdout.write(`${JSON.stringify(result)}\n`);
    process.exitCode = 0;
  } catch (error) {
    process.stderr.write(`${JSON.stringify({ ok: false, code: error.code ?? 'REMOTE_WORKER_ERROR', error: error.message })}\n`);
    process.exitCode = 125;
  }
}

if (isDirectMain()) await runRemoteWorkerCli();
