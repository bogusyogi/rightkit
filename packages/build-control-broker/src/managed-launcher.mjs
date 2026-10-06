#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { loadMacosResourceAddon } from './macos-resource.mjs';

const payload = JSON.parse(Buffer.from(process.argv[2] ?? '', 'base64url').toString('utf8'));
if (!payload || typeof payload.command !== 'string' || !Array.isArray(payload.argv)) process.exit(125);

let started = false;
let child = null;
let resourceTracker = null;
let resourceTimer = null;
let shutdownSignal = null;
const startupDeadline = setTimeout(() => process.exit(125), 30_000);
startupDeadline.unref?.();

function sendResourceSnapshot(type, snapshot, callback) {
  if (!snapshot || typeof process.send !== 'function' || !process.connected) { callback?.(); return; }
  try { process.send({ type, snapshot }, callback); } catch { callback?.(); }
}

function sampleResources() {
  if (!resourceTracker) return;
  try { sendResourceSnapshot('RESOURCE', loadMacosResourceAddon()?.sampleProcessResourceTracker(resourceTracker)); }
  catch { /* native telemetry remains explicitly unavailable in broker receipt */ }
}

function beginResourceTelemetry(pid) {
  if (process.platform !== 'darwin') return;
  try {
    const addon = loadMacosResourceAddon();
    if (typeof addon?.createProcessResourceTracker !== 'function' || typeof addon.sampleProcessResourceTracker !== 'function') return;
    resourceTracker = addon.createProcessResourceTracker(pid);
    sampleResources();
    resourceTimer = setInterval(sampleResources, Number.isSafeInteger(payload.resourceIntervalMs) && payload.resourceIntervalMs > 0 ? payload.resourceIntervalMs : 25);
    resourceTimer.unref?.();
  } catch { resourceTracker = null; }
}

function finishResourceTelemetry(callback) {
  if (resourceTimer) clearInterval(resourceTimer);
  resourceTimer = null;
  if (!resourceTracker) { callback?.(); return; }
  try {
    const addon = loadMacosResourceAddon();
    const snapshot = typeof addon?.sampleProcessResourceTracker === 'function' ? addon.sampleProcessResourceTracker(resourceTracker) : null;
    sendResourceSnapshot('RESOURCE_FINAL', snapshot, callback);
  } catch { callback?.(); }
}

function signalExitCode(signal) {
  return signal === 'SIGINT' ? 130 : signal === 'SIGHUP' ? 129 : 143;
}

function forwardShutdownSignal(signal) {
  if (!started) process.exit(signalExitCode(signal));
  if (shutdownSignal) return;
  shutdownSignal = signal;
  if (child?.exitCode === null) {
    try { child.kill(signal); } catch { /* child may have exited between the check & signal */ }
    return;
  }
  finishResourceTelemetry(() => process.exit(signalExitCode(signal)));
}

if (process.platform === 'darwin') {
  process.once('SIGTERM', () => forwardShutdownSignal('SIGTERM'));
  process.once('SIGINT', () => forwardShutdownSignal('SIGINT'));
  process.once('SIGHUP', () => forwardShutdownSignal('SIGHUP'));
}

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
  child = spawn(payload.command, payload.argv, { stdio: ['inherit', 'inherit', 'inherit'], env: process.env, cwd: process.cwd() });
  child.once('spawn', () => {
    process.send?.({ type: 'STARTED', pid: child.pid });
    beginResourceTelemetry(child.pid);
  });
  child.once('error', (error) => { process.send?.({ type: 'START_FAILED', error: error.message }); process.stderr.write(`${error.message}\n`); process.exit(125); });
  child.once('close', (code, signal) => {
    finishResourceTelemetry(() => {
      if (shutdownSignal) process.exit(signalExitCode(shutdownSignal));
      else if (signal) process.kill(process.pid, signal);
      else process.exit(Number.isInteger(code) ? code : 125);
    });
  });
});
