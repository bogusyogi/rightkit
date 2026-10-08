import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import net from 'node:net';
import test from 'node:test';
import { HEARTBEAT_INTERVAL_MS, lastHeartbeat, probeSocket, runWatchdogOnce, startHeartbeat } from './watchdog.mjs';

async function fixture() {
  const dir = await mkdtemp(join(tmpdir(), 'rk-watchdog-'));
  const endpoint = join(dir, 'b.sock');
  const lifecyclePath = join(dir, 'lifecycle.ndjson');
  return { dir, endpoint, lifecyclePath, close: () => rm(dir, { recursive: true, force: true }) };
}

// Fake broker: 'reply' answers the probe with a STATUS line, 'silent' accepts and never answers.
function fakeBroker(endpoint, mode) {
  const server = net.createServer((socket) => {
    socket.on('error', () => {});
    socket.on('data', () => { if (mode === 'reply') socket.write(`${JSON.stringify({ type: 'STATUS', id: 'runtime-health-probe', state: 'READY' })}\n`); });
  });
  return new Promise((resolve, reject) => server.listen(endpoint, () => resolve(server)).on('error', reject));
}

const closeServer = (server) => new Promise((resolve) => server.close(() => resolve()));
const heartbeatLine = (at) => `${JSON.stringify({ type: 'HEARTBEAT', at: new Date(at).toISOString(), pid: 1 })}\n`;

function recordingExec(exitCode = null) {
  const calls = [];
  const exec = async (command, args) => { calls.push({ command, args }); if (exitCode !== null) throw Object.assign(new Error('kickstart failed'), { code: exitCode, stderr: 'Could not find service' }); return { stdout: '', stderr: '' }; };
  return { calls, exec };
}

test('probe: a broker that replies is alive', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'reply');
  try {
    const verdict = await probeSocket({ endpoint: f.endpoint, timeoutMs: 1000 });
    assert.deepEqual(verdict, { alive: true, reason: 'REPLIED', replyType: 'STATUS' });
  } finally { await closeServer(server); await f.close(); }
});

test('probe: a silent (wedged) broker times out with NO_REPLY', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'silent');
  try {
    const verdict = await probeSocket({ endpoint: f.endpoint, timeoutMs: 80 });
    assert.equal(verdict.alive, false); assert.equal(verdict.reason, 'NO_REPLY');
  } finally { await closeServer(server); await f.close(); }
});

test('probe: a stale socket file with nothing listening is CONNECT_FAILED', async () => {
  const f = await fixture();
  try {
    await writeFile(f.endpoint, ''); // present, not listening
    const verdict = await probeSocket({ endpoint: f.endpoint, timeoutMs: 500 });
    assert.equal(verdict.alive, false); assert.equal(verdict.reason, 'CONNECT_FAILED');
  } finally { await f.close(); }
});

test('probe: a non-JSON reply is MALFORMED_REPLY', async () => {
  const f = await fixture();
  const server = net.createServer((socket) => { socket.on('error', () => {}); socket.on('data', () => socket.write('not json\n')); });
  await new Promise((resolve) => server.listen(f.endpoint, resolve));
  try {
    const verdict = await probeSocket({ endpoint: f.endpoint, timeoutMs: 500 });
    assert.equal(verdict.reason, 'MALFORMED_REPLY');
  } finally { await closeServer(server); await f.close(); }
});

test('watchdog: healthy broker with a fresh heartbeat is observed, not restarted', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'reply');
  const now = Date.parse('2026-10-08T10:00:00.000Z');
  await writeFile(f.lifecyclePath, heartbeatLine(now - 5_000));
  const { calls, exec } = recordingExec();
  const events = [];
  try {
    const result = await runWatchdogOnce({ endpoint: f.endpoint, lifecyclePath: f.lifecyclePath, now: () => now, exec, emit: (e) => events.push(e), options: { probeTimeoutMs: 1000 } });
    assert.equal(result.action, 'none'); assert.equal(result.state, 'HEALTHY');
    assert.deepEqual(calls, []); assert.equal(events[0].type, 'WATCHDOG_OBSERVED');
  } finally { await closeServer(server); await f.close(); }
});

test('watchdog: a wedged broker (no reply) is restarted with launchctl kickstart and a reasoned WATCHDOG_RESTART event', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'silent');
  const now = Date.parse('2026-10-08T10:00:00.000Z');
  const { calls, exec } = recordingExec();
  const events = [];
  try {
    const result = await runWatchdogOnce({ endpoint: f.endpoint, lifecyclePath: f.lifecyclePath, now: () => now, exec, emit: (e) => events.push(e), options: { probeTimeoutMs: 60, label: 'com.rightkit.build-broker' } });
    assert.equal(result.action, 'restart'); assert.equal(result.reason, 'NO_REPLY');
    assert.deepEqual(calls, [{ command: 'launchctl', args: ['kickstart', '-k', 'system/com.rightkit.build-broker'] }]);
    assert.equal(events.at(-1).type, 'WATCHDOG_RESTART'); assert.equal(events.at(-1).reason, 'NO_REPLY'); assert.equal(events.at(-1).exitCode, 0);
  } finally { await closeServer(server); await f.close(); }
});

test('watchdog: a broker that replies but whose heartbeat is stale is restarted (HEARTBEAT_STALE)', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'reply');
  const now = Date.parse('2026-10-08T10:00:00.000Z');
  await writeFile(f.lifecyclePath, heartbeatLine(now - 600_000));
  const { calls, exec } = recordingExec();
  try {
    const result = await runWatchdogOnce({ endpoint: f.endpoint, lifecyclePath: f.lifecyclePath, now: () => now, exec, emit: () => {}, options: { probeTimeoutMs: 1000, heartbeatMaxAgeMs: 120_000 } });
    assert.equal(result.action, 'restart'); assert.equal(result.reason, 'HEARTBEAT_STALE');
    assert.equal(calls.length, 1);
  } finally { await closeServer(server); await f.close(); }
});

test('watchdog: no heartbeat ever recorded counts as stale for a replying broker', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'reply');
  const { calls, exec } = recordingExec();
  try {
    const result = await runWatchdogOnce({ endpoint: f.endpoint, lifecyclePath: join(f.dir, 'missing.ndjson'), now: () => Date.now(), exec, emit: () => {}, options: { probeTimeoutMs: 1000 } });
    assert.equal(result.reason, 'HEARTBEAT_STALE'); assert.equal(calls.length, 1);
  } finally { await closeServer(server); await f.close(); }
});

test('watchdog: an absent socket is reported as down and is never restarted by the watchdog (launchd KeepAlive owns start)', async () => {
  const f = await fixture(); const { calls, exec } = recordingExec();
  try {
    const result = await runWatchdogOnce({ endpoint: f.endpoint, lifecyclePath: f.lifecyclePath, exec, emit: () => {} });
    assert.equal(result.action, 'none'); assert.equal(result.state, 'SOCKET_ABSENT'); assert.deepEqual(calls, []);
  } finally { await f.close(); }
});

test('watchdog: restarts are rate-limited by the cooldown and report WATCHDOG_SUPPRESSED', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'silent');
  const { calls, exec } = recordingExec(); const now = 1_000_000;
  try {
    const result = await runWatchdogOnce({ endpoint: f.endpoint, lifecyclePath: f.lifecyclePath, now: () => now, lastRestartAt: now - 10_000, exec, emit: () => {}, options: { probeTimeoutMs: 60, restartCooldownMs: 300_000 } });
    assert.equal(result.type, 'WATCHDOG_SUPPRESSED'); assert.equal(result.action, 'none'); assert.deepEqual(calls, []);
  } finally { await closeServer(server); await f.close(); }
});

test('watchdog: a failing kickstart is reported with its exit code and stderr, never swallowed', async () => {
  const f = await fixture(); const server = await fakeBroker(f.endpoint, 'silent');
  const { exec } = recordingExec(113); const events = [];
  try {
    const result = await runWatchdogOnce({ endpoint: f.endpoint, lifecyclePath: f.lifecyclePath, exec, emit: (e) => events.push(e), options: { probeTimeoutMs: 60 } });
    assert.equal(result.action, 'restart'); assert.equal(result.exitCode, 113); assert.match(result.stderr, /Could not find service/);
    assert.equal(events.at(-1).type, 'WATCHDOG_RESTART');
  } finally { await closeServer(server); await f.close(); }
});

test('lastHeartbeat reads the newest heartbeat from a bounded tail and ignores torn lines', async () => {
  const f = await fixture();
  try {
    await writeFile(f.lifecyclePath, `${heartbeatLine(1_000)}{"type":"TERMINAL"}\n${heartbeatLine(5_000)}{"type":"HEART`);
    assert.equal(await lastHeartbeat(f.lifecyclePath), 5_000);
    assert.equal(await lastHeartbeat(join(f.dir, 'absent.ndjson')), null);
    } finally { await f.close(); }
});

test('startHeartbeat appends HEARTBEAT events on an interval and stops on demand', async () => {
  const appended = [];
  const stop = startHeartbeat({ append: async (event) => { appended.push(event); }, intervalMs: 5, now: () => 42, pid: 7 });
  await new Promise((resolve) => setTimeout(resolve, 40));
  stop();
  const count = appended.length;
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.ok(count >= 2); assert.equal(appended.length, count);
  assert.deepEqual(appended[0], { type: 'HEARTBEAT', at: new Date(42).toISOString(), pid: 7 });
  assert.equal(HEARTBEAT_INTERVAL_MS, 30_000);
});
