import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createManagedAgentRequest, sendManagedAgentRequest } from '@rightkit/build-control-clients';
import { BuildControlBroker, createLocalNdjsonServer, LocalNdjsonBrokerService } from './index.mjs';

test('real client reaches broker & tool sees only broker-owned target path', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-managed-e2e-'));
  let server;
  try {
    const externalRoot = join(root, 'external');
    const workspace = join(root, 'workspace');
    await Promise.all([mkdir(externalRoot), mkdir(workspace)]);
    const sha256 = createHash('sha256').update(await readFile(process.execPath)).digest('hex');
    const broker = new BuildControlBroker({ externalRoot: { path: externalRoot, volumeId: 'e2e', mounted: true, writable: true, internal: false }, capacityBytes: 1024 * 1024, stateFile: join(externalRoot, 'state.json'), receiptsDir: join(externalRoot, 'receipts') });
    const service = new LocalNdjsonBrokerService({
      broker,
      tools: [{ id: 'node', path: process.execPath, root: dirname(process.execPath), sha256 }],
      shims: { cargo: 'node' },
      defaults: { toolchain: 'node-test', targetTriple: process.arch, buildProfile: 'test', reservationBytes: 1024 }
    });
    const endpoint = join(root, 'broker.sock');
    server = createLocalNdjsonServer({ service, endpoint });
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(endpoint, resolve); });
    const events = [];
    const message = createManagedAgentRequest({ argv0: 'cargo', argv: ['-e', 'process.stdout.write(process.env.CARGO_TARGET_DIR)'], workspace });
    const result = await sendManagedAgentRequest(message, { endpoint, onEvent: (event) => events.push(event) });
    assert.equal(result.exitCode, 0);
    assert.deepEqual(events.map((event) => event.type), ['STDOUT', 'RESULT']);
    assert.match(events[0].data, new RegExp(`^${externalRoot.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}/rightkit-build-control/`));
  } finally {
    if (server) await new Promise((resolve) => server.close(resolve));
    await rm(root, { recursive: true, force: true });
  }
});
