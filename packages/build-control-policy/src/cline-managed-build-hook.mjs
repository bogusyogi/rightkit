#!/usr/bin/env node
import { clineHookDecision } from './index.mjs';
let raw = ''; for await (const chunk of process.stdin) raw += chunk;
process.stdout.write(`${JSON.stringify(clineHookDecision(JSON.parse(raw)))}\n`);
