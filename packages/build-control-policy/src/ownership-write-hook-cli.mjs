#!/usr/bin/env node
import { ownershipWriteDecision } from './ownership-write-hook.mjs';
let raw = ''; for await (const chunk of process.stdin) raw += chunk;
const decision = ownershipWriteDecision(JSON.parse(raw));
// Claude Code accepts only the legacy `approve`/`block` in a top-level `decision`; `allow` fails its hook-output
// validation on every tool call. An allow is the hook's default, so drop it and keep only hookSpecificOutput.
const { decision: verdict, ...wire } = decision;
process.stdout.write(`${JSON.stringify(verdict === 'allow' ? wire : decision)}\n`);
if (decision.hookSpecificOutput?.permissionDecision === 'deny') process.exitCode = 2;
