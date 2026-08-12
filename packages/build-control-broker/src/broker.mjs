#!/usr/bin/env node
import { readFile } from 'node:fs/promises';
import { createLocalNdjsonServer, BuildControlBroker, LocalNdjsonBrokerService, removeStaleUnixSocket } from './index.mjs';
const configPath = process.argv[2] ?? process.env.RIGHTKIT_BUILD_BROKER_CONFIG;
if (!configPath) throw new Error('explicit broker JSON config path is required');
const config = JSON.parse(await readFile(configPath, 'utf8'));
const broker = new BuildControlBroker(config.broker);
const service = new LocalNdjsonBrokerService({ broker, tools: config.tools, shims: config.shims, defaults: config.defaults, outputPolicy: config.outputPolicy, bounds: config.bounds });
const endpoint = process.platform === 'win32' ? config.windowsPipe : config.unixSocket;
if (!endpoint) throw new Error('platform socket/pipe endpoint is required');
if (process.platform !== 'win32') await removeStaleUnixSocket(endpoint);
const server = createLocalNdjsonServer({ service, endpoint });
await new Promise((resolve, reject) => { server.once('error', reject); server.listen(endpoint, resolve); });
