#!/usr/bin/env node
import { ownershipWriteDecision } from './ownership-write-hook.mjs';
let raw = ''; for await (const chunk of process.stdin) raw += chunk;
const decision = ownershipWriteDecision(JSON.parse(raw));
process.stdout.write(`${JSON.stringify(decision)}\n`);
if (decision.hookSpecificOutput) process.exitCode = 2;
