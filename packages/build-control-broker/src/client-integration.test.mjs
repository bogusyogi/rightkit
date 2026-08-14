import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createManagedAgentRequest, submitAndWatchManagedAgentJob } from '@rightkit/build-control-clients';
import { createManagedJobServer, ManagedJobService } from './job-service.mjs';

test('real client reaches broker & tool sees only broker-owned target path', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-managed-e2e-'));
  let server; let service;
  try {
    const externalRoot = join(root, 'external');
    const workspace = join(root, 'workspace');
    await Promise.all([mkdir(externalRoot), mkdir(workspace)]);
    const executable = join(root, 'fake-cargo'); await writeFile(executable, '#!/usr/bin/env node\nprocess.stdout.write(process.env.CARGO_TARGET_DIR ?? "missing");\n'); await chmod(executable, 0o755);
    const sha256 = createHash('sha256').update(await readFile(executable)).digest('hex');
    const config = { broker: { externalRoot: { path: externalRoot, volumeId: 'e2e', mounted: true, writable: true, internal: false }, capacityBytes: 1024 * 1024, stateFile: join(externalRoot, 'state.json'), receiptsDir: join(externalRoot, 'receipts') }, unixSocket: join(root, 'broker.sock'), tools: [{ id: 'cargo', path: executable, root, sha256 }], shims: { cargo: 'cargo' }, defaults: { toolchain: 'node-test', targetTriple: process.arch, buildProfile: 'test' }, bounds: { freeFloorBytes: 0, heartbeatIntervalMs: 20, processGraceMs: 200 } };
    service = await ManagedJobService.create({ config, statfs: async () => ({ bavail: 1024 * 1024, bsize: 1 }) });
    const endpoint = join(root, 'broker.sock');
    server = createManagedJobServer({ service, endpoint });
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(endpoint, resolve); });
    const events = [];
    const message = createManagedAgentRequest({ argv0: 'cargo', argv: ['test'], workspace });
    const result = await submitAndWatchManagedAgentJob(message, { endpoint, onEvent: (event) => events.push(event) });
    assert.equal(result.exitCode, 0);
    assert.equal(events[0].type, 'HANDSHAKE'); assert.ok(events.some((event) => event.type === 'JOB_EVENT' && event.event.type === 'HEARTBEAT')); assert.equal(events.at(-1).type, 'RESULT');
    assert.match(events.find((event) => event.type === 'JOB_EVENT' && event.event.type === 'STDOUT').event.data, new RegExp(`^${externalRoot.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}/rightkit-build-control/`));
  } finally {
    if (server) await new Promise((resolve) => server.close(resolve));
    if (service) await service.shutdown();
    await rm(root, { recursive: true, force: true });
  }
});
