#!/usr/bin/env node
import { readFileSync } from 'node:fs';
import { ownershipSessionDecision } from './ownership-session-hook.mjs';

let input = {};
try { input = JSON.parse(readFileSync(0, 'utf8') || '{}'); } catch { /* no hook payload: catalog only */ }
process.stdout.write(`${JSON.stringify(ownershipSessionDecision(input))}
`);
