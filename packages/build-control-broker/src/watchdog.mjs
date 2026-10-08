// Broker watchdog (P0 exit). Detects a wedged broker and restarts it through
// launchctl kickstart. A broker is wedged when:
//   - its socket file is present but connect is refused, or
//   - it accepts a connection but sends no HANDSHAKE/STATUS/RESULT line within the bound, or
//   - its lifecycle heartbeat (HEARTBEAT events in lifecycle.ndjson) is older than the bound.
// Absent socket = broker is down, which launchd KeepAlive already handles; it is reported, not restarted.
import { execFile as nodeExecFile } from 'node:child_process';
import net from 'node:net';
import { existsSync, realpathSync } from 'node:fs';
import { open } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { BUILD_CONTROL_PROTOCOL, SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';

const execFile = promisify(nodeExecFile);
export const WATCHDOG_DEFAULTS = Object.freeze({ probeTimeoutMs: 5_000, heartbeatMaxAgeMs: 120_000, restartCooldownMs: 300_000, label: 'com.rightkit.build-broker' });
export const HEARTBEAT_INTERVAL_MS = 30_000;
const ALIVE_TYPES = new Set(['HANDSHAKE', 'STATUS', 'RESULT']);
const TAIL_BYTES = 256 * 1024;

/** Sends a runtime-health STATUS probe and waits for the first reply line. Any reply type from the broker proves its loop is serving. */
export function probeSocket({ endpoint, timeoutMs = WATCHDOG_DEFAULTS.probeTimeoutMs, runtime = SOURCE_RUNTIME_IDENTITY, connect = net.createConnection }) {
  return new Promise((resolve) => {
    let settled = false; let buffer = '';
    const finish = (verdict) => { if (settled) return; settled = true; clearTimeout(timer); socket.destroy(); resolve(verdict); };
    const socket = connect(endpoint);
    const timer = setTimeout(() => finish({ alive: false, reason: 'NO_REPLY', detail: `no HANDSHAKE/STATUS reply within ${timeoutMs} ms` }), timeoutMs);
    timer.unref?.();
    socket.on('connect', () => socket.write(`${JSON.stringify({ type: 'STATUS', id: 'runtime-health-probe', protocol: BUILD_CONTROL_PROTOCOL, runtime })}\n`));
    socket.on('data', (chunk) => {
      buffer += chunk.toString('utf8');
      const index = buffer.indexOf('\n');
      if (index < 0) return;
      let message = null;
      try { message = JSON.parse(buffer.slice(0, index)); } catch { return finish({ alive: false, reason: 'MALFORMED_REPLY', detail: 'first reply line was not JSON' }); }
      finish(ALIVE_TYPES.has(message?.type) ? { alive: true, reason: 'REPLIED', replyType: message.type } : { alive: false, reason: 'MALFORMED_REPLY', detail: `unexpected reply type ${JSON.stringify(message?.type)}` });
    });
    socket.on('error', (error) => finish({ alive: false, reason: error.code === 'ENOENT' ? 'SOCKET_ABSENT' : 'CONNECT_FAILED', detail: error.code ?? error.message }));
    socket.on('close', () => finish({ alive: false, reason: 'NO_REPLY', detail: 'connection closed before any reply' }));
  });
}

/** Reads the newest HEARTBEAT timestamp from the lifecycle log tail (bounded read). */
export async function lastHeartbeat(lifecyclePath, { read = open } = {}) {
  let handle;
  try { handle = await read(lifecyclePath, 'r'); } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
  try {
    const { size } = await handle.stat();
    const start = Math.max(0, size - TAIL_BYTES);
    const buffer = Buffer.alloc(size - start);
    await handle.read(buffer, 0, buffer.length, start);
    const lines = buffer.toString('utf8').split('\n').reverse();
    for (const line of lines) {
      if (!line.includes('"HEARTBEAT"')) continue;
      try { const event = JSON.parse(line); if (event?.type === 'HEARTBEAT' && Number.isFinite(Date.parse(event.at))) return Date.parse(event.at); } catch { /* partial line at tail boundary */ }
    }
    return null;
  } finally { await handle.close(); }
}

/** Single watchdog pass. Restarts only on wedge evidence and never more often than the cooldown. */
export async function runWatchdogOnce({
  endpoint, lifecyclePath, now = Date.now, probe = probeSocket, heartbeat = lastHeartbeat, socketExists = existsSync,
  exec = execFile, emit = () => {}, lastRestartAt = null, options = {},
}) {
  const config = { ...WATCHDOG_DEFAULTS, ...options };
  const at = now();
  if (!socketExists(endpoint)) {
    const event = { type: 'WATCHDOG_OBSERVED', at: new Date(at).toISOString(), state: 'SOCKET_ABSENT', endpoint };
    emit(event); return { action: 'none', ...event };
  }
  const reply = await probe({ endpoint, timeoutMs: config.probeTimeoutMs });
  if (!reply.alive && reply.reason === 'SOCKET_ABSENT') {
    const event = { type: 'WATCHDOG_OBSERVED', at: new Date(at).toISOString(), state: 'SOCKET_ABSENT', endpoint };
    emit(event); return { action: 'none', ...event };
  }
  const beat = reply.alive ? await heartbeat(lifecyclePath) : null;
  const stale = reply.alive && (beat === null || at - beat > config.heartbeatMaxAgeMs);
  const wedged = !reply.alive ? reply.reason : stale ? 'HEARTBEAT_STALE' : null;
  if (!wedged) {
    const event = { type: 'WATCHDOG_OBSERVED', at: new Date(at).toISOString(), state: 'HEALTHY', endpoint, heartbeatAt: beat ? new Date(beat).toISOString() : null };
    emit(event); return { action: 'none', ...event };
  }
  if (lastRestartAt !== null && at - lastRestartAt < config.restartCooldownMs) {
    const event = { type: 'WATCHDOG_SUPPRESSED', at: new Date(at).toISOString(), reason: wedged, cooldownMs: config.restartCooldownMs, endpoint };
    emit(event); return { action: 'none', ...event };
  }
  const target = `system/${config.label}`;
  let exitCode = 0; let stderr = '';
  try { await exec('launchctl', ['kickstart', '-k', target]); }
  catch (error) { exitCode = typeof error.code === 'number' ? error.code : 1; stderr = String(error.stderr ?? error.message).slice(0, 500); }
  const event = { type: 'WATCHDOG_RESTART', at: new Date(at).toISOString(), reason: wedged, probe: reply.reason, detail: reply.detail ?? null, target, exitCode, stderr: stderr || null, endpoint };
  emit(event);
  return { action: 'restart', ...event, restartedAt: at };
}

/** Lifecycle heartbeat emitter used by the broker process itself. */
export function startHeartbeat({ append, intervalMs = HEARTBEAT_INTERVAL_MS, now = Date.now, pid = process.pid }) {
  const timer = setInterval(() => { void append({ type: 'HEARTBEAT', at: new Date(now()).toISOString(), pid }).catch(() => {}); }, intervalMs);
  timer.unref?.();
  return () => clearInterval(timer);
}

async function main(argv) {
  const get = (name, fallback) => { const index = argv.indexOf(name); return index >= 0 ? argv[index + 1] : fallback; };
  const endpoint = get('--endpoint'); const lifecyclePath = get('--lifecycle');
  if (!endpoint || !lifecyclePath) throw new Error('usage: watchdog.mjs --endpoint <socket> --lifecycle <lifecycle.ndjson> [--label <launchd label>]');
  const label = get('--label', WATCHDOG_DEFAULTS.label);
  const result = await runWatchdogOnce({ endpoint, lifecyclePath, options: { label }, emit: (event) => process.stdout.write(`${JSON.stringify(event)}\n`) });
  return result.action === 'restart' && result.exitCode !== 0 ? 1 : 0;
}

function isMainModule() {
  try { return Boolean(process.argv[1]) && realpathSync(fileURLToPath(import.meta.url)) === realpathSync(process.argv[1]); } catch { return false; }
}
if (isMainModule()) {
  main(process.argv.slice(2)).then((code) => { process.exitCode = code; }, (error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 2; });
}
