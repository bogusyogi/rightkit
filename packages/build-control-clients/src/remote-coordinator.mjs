// Mac coordinator for dual-host Rust routing (docs/RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md).
// Builds a content-addressed worktree snapshot, transfers it over the existing
// passwordless `ssh win` bridge, and drives the detached remote lifecycle:
// SUBMIT returns one durable remote request ID; STATUS is one-shot per agent
// wake; CANCEL requests cancellation. No watch path exists here — agents must
// never poll Cargo status or hold SSH connections.

import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { buildSnapshot, repositoryKey as deriveRepositoryKey } from './remote-snapshot.mjs';
import { decodePayload, encodePayload, generateRemoteRequestId, REMOTE_VERBS, validateRemoteCargoArgv } from './remote-protocol.mjs';

export class RemoteCoordinatorError extends Error {
  constructor(message, code) {
    super(message);
    this.name = 'RemoteCoordinatorError';
    this.code = code;
  }
}

const WINDOWS_ABSOLUTE = /^[A-Za-z]:\\[A-Za-z0-9 ._\\-]+$/; // no quotes/metachars by charset
const SCP_DIR = /^[A-Za-z]:\/[A-Za-z0-9 ._-]+(\/[A-Za-z0-9 ._-]+)*$/;

function configuredTransport(environment = process.env) {
  const host = environment.RIGHTKIT_REMOTE_WINDOWS_HOST ?? 'win';
  if (!/^[A-Za-z0-9._-]+$/.test(host)) throw new RemoteCoordinatorError('RIGHTKIT_REMOTE_WINDOWS_HOST must be an ssh alias', 'REMOTE_CONFIG_INVALID');
  const nodeExe = environment.RIGHTKIT_REMOTE_WINDOWS_NODE ?? 'C:\\nvm4w\\nodejs\\node.exe';
  const workerScript = environment.RIGHTKIT_REMOTE_WINDOWS_WORKER ?? 'D:\\Claude\\tools\\rightkit\\packages\\build-control-clients\\src\\remote-worker.mjs';
  if (!nodeExe || !WINDOWS_ABSOLUTE.test(nodeExe)) throw new RemoteCoordinatorError('RIGHTKIT_REMOTE_WINDOWS_NODE must be an absolute Windows node.exe path without shell metacharacters', 'REMOTE_CONFIG_INVALID');
  if (!workerScript || !WINDOWS_ABSOLUTE.test(workerScript)) throw new RemoteCoordinatorError('RIGHTKIT_REMOTE_WINDOWS_WORKER must be an absolute Windows script path without shell metaracters', 'REMOTE_CONFIG_INVALID');
  const inputsDir = environment.RIGHTKIT_REMOTE_INPUTS_DIR ?? 'D:/.rightkit-managed/remote-inputs';
  if (!SCP_DIR.test(inputsDir)) throw new RemoteCoordinatorError('RIGHTKIT_REMOTE_INPUTS_DIR must be a plain Windows directory for scp targets', 'REMOTE_CONFIG_INVALID');
  return { host, nodeExe, workerScript, inputsDir };
}

/** Execute the worker over ssh with one validated payload. `run` is injectable
 * for tests; production uses spawnSync against the ssh bridge. */
async function runWorkerOperation({ transport, op, payloadB64, run }) {
  // base64url alphabet + fixed argv layout: no user string becomes shell syntax.
  // Windows OpenSSH enters PowerShell in this workspace. PowerShell requires
  // call operator before quoted executable path; strict path charset above
  // excludes apostrophes, so single-quoted literals remain non-interpreted.
  const command = `& '${transport.nodeExe}' '${transport.workerScript}' ${op} ${payloadB64}`;
  const result = run({ command: 'ssh', args: [transport.host, command] });
  if (result.error) throw new RemoteCoordinatorError(`ssh bridge failed: ${result.error.message}`, 'REMOTE_HOST_UNREACHABLE');
  if (result.status !== 0) {
    let detail = result.stderr?.trim() ?? '';
    try { detail = JSON.parse(detail.split('\n').filter(Boolean).pop()).error ?? detail; } catch { /* keep raw stderr */ }
    const code = (() => { try { return JSON.parse(result.stderr.split('\n').filter(Boolean).pop()).code ?? 'REMOTE_OPERATION_FAILED'; } catch { return 'REMOTE_OPERATION_FAILED'; } })();
    throw new RemoteCoordinatorError(`remote ${op} failed: ${detail}`, code);
  }
  const lines = result.stdout.split('\n').filter(Boolean);
  let parsed;
  try { parsed = JSON.parse(lines[lines.length - 1]); } catch { throw new RemoteCoordinatorError(`remote ${op} returned unparseable output`, 'REMOTE_PROTOCOL_VIOLATION'); }
  if (parsed.ok !== true) throw new RemoteCoordinatorError(`remote ${op} rejected: ${parsed.error ?? 'unknown'}`, parsed.code ?? 'REMOTE_OPERATION_FAILED');
  return Object.freeze(parsed);
}

/** Create the coordinator. `run`, `upload`, and `snapshot` are injectable for
 * tests; production uses spawnSync over the ssh/scp bridge and the git-based
 * snapshot builder. */
export function createRemoteCoordinator({
  environment = process.env,
  now = Date.now,
  run = ({ command, args }) => spawnSync(command, args, { encoding: 'utf8', timeout: 120_000 }),
  upload = null,
  snapshot = buildSnapshot,
} = {}) {
  const transport = configuredTransport(environment);

  async function transferArchive(snapshot) {
    if (upload) return upload(snapshot);
    const target = `${transport.host}:${transport.inputsDir}/${snapshot.sha256}.tar`;
    const result = spawnSync('scp', [snapshot.archivePath, target], { encoding: 'utf8', timeout: 300_000 });
    if (result.error || result.status !== 0) throw new RemoteCoordinatorError(`archive transfer failed: ${(result.stderr ?? result.error?.message ?? '').trim()}`, 'REMOTE_TRANSFER_FAILED');
  }

  return {
    /** Build snapshot, transfer it, submit detached. Returns the durable remote
     * request acknowledgement — never attaches to output. */
    async submitRemoteCargo({ argv, workspace = process.cwd(), deadlineMs } = {}) {
      const normalizedArgv = validateRemoteCargoArgv(argv);
      if (!REMOTE_VERBS.includes(normalizedArgv[0])) throw new RemoteCoordinatorError(`unregistered remote verb: ${normalizedArgv[0]}`, 'REMOTE_COMMAND_DENIED');
      const builtSnapshot = await snapshot({ workspace, tempRoot: environment.RIGHTKIT_SNAPSHOT_TMPDIR ?? tmpdir() });
      try {
        await transferArchive(builtSnapshot);
        const requestId = generateRemoteRequestId(now);
        const payload = encodePayload({
          v: 1, op: 'SUBMIT', requestId,
          repositoryKey: builtSnapshot.key,
          digest: builtSnapshot.sha256,
          bytes: builtSnapshot.byteLength,
          argv: [...normalizedArgv],
          deadlineMs,
          createdAt: new Date(now()).toISOString(),
        });
        decodePayload(payload); // transmitted bytes are validated before ssh runs
        const ack = await runWorkerOperation({ transport, op: 'submit', payloadB64: payload, run });
        return Object.freeze({ ...ack, treeHash: builtSnapshot.treeHash, headHash: builtSnapshot.headHash });
      } finally {
        const { rm } = await import('node:fs/promises');
        await rm(builtSnapshot.scratchDir, { recursive: true, force: true }).catch(() => {});
      }
    },
    /** One-shot status read by durable request ID (one wake = one read). */
    async remoteStatus(requestId) {
      const payload = encodePayload({ v: 1, op: 'STATUS', requestId });
      return runWorkerOperation({ transport, op: 'status', payloadB64: payload, run });
    },

    /** Request cancellation by durable request ID. */
    async remoteCancel(requestId) {
      const payload = encodePayload({ v: 1, op: 'CANCEL', requestId });
      return runWorkerOperation({ transport, op: 'cancel', payloadB64: payload, run });
    },
  };
}

export { buildSnapshot, deriveRepositoryKey };
