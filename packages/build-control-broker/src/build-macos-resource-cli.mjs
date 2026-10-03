#!/usr/bin/env node
import { buildMacosResourceAddon } from './bootstrap-macos-cli.mjs';
import { fileURLToPath } from 'node:url';
import { dirname } from 'node:path';

const sourcePackageRoot = dirname(fileURLToPath(import.meta.url));
const sourcePackagesRoot = dirname(dirname(sourcePackageRoot));
const descriptor = await buildMacosResourceAddon({ sourcePackagesRoot });
process.stdout.write(`${JSON.stringify(descriptor)}\n`);
