#!/usr/bin/env node
import { resolve } from 'node:path';
import { installAgentShims } from './index.mjs';

if (process.argv.length !== 4 || process.argv[2] !== '--dir') throw new Error('usage: rightkit-install-agent-shims --dir <absolute-agent-only-directory>');
const manifest = await installAgentShims({ directory: resolve(process.argv[3]) });
process.stdout.write(`${JSON.stringify(manifest)}\n`);
