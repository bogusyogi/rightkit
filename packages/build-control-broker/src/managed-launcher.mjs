#!/usr/bin/env node
import { spawn } from 'node:child_process';

const payload = JSON.parse(Buffer.from(process.argv[2] ?? '', 'base64url').toString('utf8'));
if (!payload || typeof payload.command !== 'string' || !Array.isArray(payload.argv)) process.exit(125);

let started = false;
let child = null;
const startupDeadline = setTimeout(() => process.exit(125), 30_000);
startupDeadline.unref?.();

process.once('disconnect', () => {
  if (!started) return process.exit(125);
  if (!payload.killOnDisconnect || !child) return;
  try { child.kill('SIGTERM'); } catch { return; }
  setTimeout(() => { if (child?.exitCode === null) try { child.kill('SIGKILL'); } catch { /* already gone */ } }, 2_000);
});
process.once('message', (message) => {
  if (started || message?.type !== 'START') return;
  started = true;
  clearTimeout(startupDeadline);
  if (Number.isSafeInteger(payload.umask)) process.umask(payload.umask);
  child = spawn(payload.command, payload.argv, { stdio: ['ignore', 'inherit', 'inherit'], env: process.env, cwd: process.cwd() });
  child.once('spawn', () => { process.send?.({ type: 'STARTED', pid: child.pid }); });
  child.once('error', (error) => { process.send?.({ type: 'START_FAILED', error: error.message }); process.stderr.write(`${error.message}\n`); process.exit(125); });
  child.once('close', (code, signal) => {
    if (signal) process.kill(process.pid, signal);
    else process.exit(Number.isInteger(code) ? code : 125);
  });
});
