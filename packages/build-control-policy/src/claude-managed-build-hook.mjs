#!/usr/bin/env node
import { claudeHookDecision } from './index.mjs';
let raw = ''; for await (const chunk of process.stdin) raw += chunk;
const value = claudeHookDecision(JSON.parse(raw));
if (value.hookSpecificOutput?.permissionDecision === 'deny') { process.stdout.write(`${JSON.stringify(value)}\n`); process.exitCode = 2; }
