#!/usr/bin/env node
import { ownershipSessionDecision } from './ownership-session-hook.mjs';
process.stdout.write(`${JSON.stringify(ownershipSessionDecision())}\n`);
