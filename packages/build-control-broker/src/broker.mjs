#!/usr/bin/env node
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { SOURCE_RUNTIME_IDENTITY, normalizeRuntimeIdentity } from '@rightkit/build-control-core';
import { loadBrokerConfig } from './config.mjs';
import { ManagedJobService, createManagedJobServer } from './job-service.mjs';
import { prepareUnixSocketEndpoint, removeStaleUnixSocket, secureUnixSocketEndpoint } from './utilities.mjs';
const configPath = process.argv[2] ?? process.env.RIGHTKIT_BUILD_BROKER_CONFIG;
if (!configPath) throw new Error('explicit broker JSON config path is required');
const config = await loadBrokerConfig(configPath);
const runtimeManifestPath = fileURLToPath(new URL('../../runtime-manifest.json', import.meta.url));
const runtimeIdentity = await readFile(runtimeManifestPath, 'utf8').then((value) => normalizeRuntimeIdentity(JSON.parse(value))).catch((error) => { if (error.code === 'ENOENT') return SOURCE_RUNTIME_IDENTITY; throw error; });
const service = await ManagedJobService.create({ config, runtimeIdentity });
const endpoint = process.platform === 'win32' ? config.windowsPipe : config.unixSocket;
if (!endpoint) throw new Error('platform socket/pipe endpoint is required');
if (process.platform !== 'win32') {
  await prepareUnixSocketEndpoint({ endpoint, externalRoot: config.broker.externalRoot.path });
  await removeStaleUnixSocket(endpoint);
}
const server = createManagedJobServer({ service, endpoint });
try {
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(endpoint, resolve); });
  if (process.platform !== 'win32') await secureUnixSocketEndpoint(endpoint);
} catch (error) {
  server.close();
  if (process.platform !== 'win32') await removeStaleUnixSocket(endpoint).catch(() => {});
  throw error;
}
let stopping = false;
const stop = async () => { if (stopping) return; stopping = true; server.close(); await service.shutdown(); if (process.platform !== 'win32') await removeStaleUnixSocket(endpoint).catch(() => {}); };
process.once('SIGTERM', () => { void stop().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; }); });
process.once('SIGINT', () => { void stop().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; }); });
