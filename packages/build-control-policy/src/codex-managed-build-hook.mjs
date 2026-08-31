#!/usr/bin/env node
import { claudeHookDecision } from './index.mjs';
let raw = ''; for await (const chunk of process.stdin) raw += chunk;
const input = JSON.parse(raw);
const decision = claudeHookDecision(input);
if (decision.hookSpecificOutput) { process.stdout.write(`${JSON.stringify(decision)}\n`); process.exitCode = 2; }
