import assert from 'node:assert/strict';
import test from 'node:test';
import { createRemoteCoordinator, RemoteCoordinatorError } from './remote-coordinator.mjs';
import { decodePayload, RemoteProtocolError } from './remote-protocol.mjs';

const environment = {
  RIGHTKIT_REMOTE_WINDOWS_HOST: 'win',
  RIGHTKIT_REMOTE_WINDOWS_NODE: 'C:\\Program Files\\nodejs\\node.exe',
  RIGHTKIT_REMOTE_WINDOWS_WORKER: 'D:\\Claude\\tools\\rightkit\\remote-worker.mjs',
  RIGHTKIT_REMOTE_INPUTS_DIR: 'D:/.rightkit-managed/remote-inputs',
};

function snapshotFixture() {
  return {
    archivePath: 'snapshot.tar',
    scratchDir: 'scratch-dir-that-is-not-created-by-test',
    key: 'coderight-a1b2c3d4',
    sha256: 'a'.repeat(64),
    byteLength: 512,
    treeHash: 'tree-hash',
    headHash: 'head-hash',
  };
}

test('workspace Windows transport defaults to immutable active-runtime worker', async () => {
  let invocation;
  const coordinator = createRemoteCoordinator({
    environment: {},
    run: (value) => {
      invocation = value;
      return { status: 0, stdout: `${JSON.stringify({ ok: true, requestId: 'rr-1', state: 'RUNNING', terminal: false })}\n`, stderr: '' };
    },
  });
  await coordinator.remoteStatus('rr-1');
  assert.equal(invocation.args[1].includes("'D:\\.rightkit-managed\\runtime-current\\build-control-clients\\src\\remote-worker.mjs'"), true);
});

test('SUBMIT uploads exact snapshot then detaches with durable request ID', async () => {
  const calls = [];
  const coordinator = createRemoteCoordinator({
    environment,
    now: () => 1_700_000_000_000,
    snapshot: async () => snapshotFixture(),
    upload: async (snapshot) => calls.push({ type: 'upload', snapshot }),
    run: ({ command, args }) => {
      calls.push({ type: 'run', command, args });
      const payload = decodePayload(args[1].split(' ').at(-1));
      assert.equal(payload.op, 'SUBMIT');
      assert.deepEqual(payload.argv, ['check', '--workspace']);
      return { status: 0, stdout: JSON.stringify({ ok: true, requestId: payload.requestId, state: 'QUEUED', brokerId: 'job-1' }) + '\n', stderr: '' };
    },
  });
  const result = await coordinator.submitRemoteCargo({ argv: ['check', '--workspace'], workspace: '/mac/worktree' });
  assert.equal(result.state, 'QUEUED');
  assert.equal(result.brokerId, 'job-1');
  assert.equal(result.treeHash, 'tree-hash');
  assert.equal(calls[0].type, 'upload');
  assert.equal(calls[1].type, 'run');
  assert.deepEqual(calls[1].args.slice(0, 1), ['win']);
  assert.match(calls[1].args[1], /^& 'C:\\Program Files\\nodejs\\node\.exe'/);
});

test('STATUS and CANCEL each make one typed SSH operation and never expose a watch path', async () => {
  const operations = [];
  const coordinator = createRemoteCoordinator({
    environment,
    run: ({ args }) => {
      const words = args[1].split(' ');
      const encoded = words.at(-1);
      operations.push({ operation: words.at(-2), payload: decodePayload(encoded) });
      return { status: 0, stdout: JSON.stringify({ ok: true, requestId: 'rr-1', state: 'RUNNING', terminal: false }) + '\n', stderr: '' };
    },
  });
  await coordinator.remoteStatus('rr-1');
  await coordinator.remoteCancel('rr-1');
  assert.deepEqual(operations.map(({ operation }) => operation), ['status', 'cancel']);
  assert.deepEqual(operations.map(({ payload }) => payload.op), ['STATUS', 'CANCEL']);
  await assert.rejects(() => coordinator.remoteStatus('../escape'), RemoteProtocolError);
});

test('transport configuration rejects shell-sensitive Windows paths', () => {
  assert.throws(() => createRemoteCoordinator({ environment: { ...environment, RIGHTKIT_REMOTE_WINDOWS_WORKER: 'D:\\worker & evil.mjs' } }), /absolute Windows script path/);
  assert.throws(() => createRemoteCoordinator({ environment: { ...environment, RIGHTKIT_REMOTE_WINDOWS_HOST: 'win;evil' } }), /ssh alias/);
});
