#!/usr/bin/env node
import { resolveManagedRoots } from '../../release/managed-roots.mjs';
import { DataRootService } from './data-root-check.mjs';
import { ThinLogger } from './thin-logger.mjs';
import { join } from 'node:path';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { SOURCE_RUNTIME_IDENTITY, normalizeRuntimeIdentity } from '@rightkit/build-control-core';
import { loadBrokerConfig } from './config.mjs';
import { ThinJobService, createThinJobServer } from './thin-job-service.mjs';
import { prepareUnixSocketEndpoint, removeStaleUnixSocket, secureUnixSocketEndpoint } from './utilities.mjs';
import { secureWindowsBrokerProcessDefaultDacl } from './windows-job.mjs';
import { startHeartbeat } from './watchdog.mjs';
const configPath = process.argv[2] ?? process.env.RIGHTKIT_BUILD_BROKER_CONFIG;
if (!configPath) throw new Error('explicit broker JSON config path is required');
const config = await loadBrokerConfig(configPath);
const runtimeManifestPath = fileURLToPath(new URL('../../runtime-manifest.json', import.meta.url));
const runtimeIdentity = await readFile(runtimeManifestPath, 'utf8').then((value) => normalizeRuntimeIdentity(JSON.parse(value))).catch((error) => { if (error.code === 'ENOENT') return SOURCE_RUNTIME_IDENTITY; throw error; });
const { controlRoot, logs } = resolveManagedRoots({ dataRoot: config.broker.externalRoot.path, controlRoot: config.broker.controlRoot });
const lifecycle = await ThinLogger.open({ path: join(logs, 'lifecycle.ndjson') });
const service = process.platform === 'win32' ? await ThinJobService.create({ config, runtimeIdentity }) : new DataRootService({ path: config.broker.externalRoot.path, runtimeIdentity,
  createService: () => ThinJobService.create({ config, runtimeIdentity }),
  log: async (event) => { await lifecycle.append(event); process.stdout.write(`${JSON.stringify(event)}\n`); } });
const endpoint = process.platform === 'win32' ? config.windowsPipe : config.unixSocket;
if (!endpoint) throw new Error('platform socket/pipe endpoint is required');
if (process.platform === 'win32') secureWindowsBrokerProcessDefaultDacl();
if (process.platform !== 'win32') {
  await prepareUnixSocketEndpoint({ endpoint, externalRoot: controlRoot });
  await removeStaleUnixSocket(endpoint);
}
const server = createThinJobServer({ service, endpoint });
let stopHeartbeat = () => {};
try {
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(endpoint, resolve); });
  if (process.platform !== 'win32') await secureUnixSocketEndpoint(endpoint);
  await lifecycle.append({ type: 'LISTENING', endpoint, pid: process.pid });
  stopHeartbeat = startHeartbeat({ append: (event) => lifecycle.append(event) });
  process.stdout.write(`${JSON.stringify({ type: 'LISTENING', endpoint, pid: process.pid })}\n`);
  void service.check?.().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); });
} catch (error) {
  server.close();
  await service.shutdown().catch(() => {});
  await lifecycle.close().catch(() => {});
  if (process.platform !== 'win32') await removeStaleUnixSocket(endpoint).catch(() => {});
  throw error;
}
let stopping = false;
const stop = async (signal = null) => {
  if (stopping) return;
  stopping = true;
  if (signal) await service.recordTerminationSignal(signal, {
    pid: process.pid,
    ppid: process.ppid,
    platform: process.platform,
    configPath,
    endpoint,
    runtime: runtimeIdentity,
  });
  stopHeartbeat();
  server.close();
  await service.shutdown();
  await lifecycle.close();
  if (process.platform !== 'win32') await removeStaleUnixSocket(endpoint).catch(() => {});
};
process.once('SIGTERM', () => { void stop('SIGTERM').catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; }); });
process.once('SIGINT', () => { void stop('SIGINT').catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; }); });
