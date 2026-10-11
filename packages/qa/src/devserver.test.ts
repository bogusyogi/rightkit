import { createServer, type Server } from 'node:net';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { DevServerError, startDevServer } from './devserver.js';
import { freePort, isProcessAlive } from './process.js';

/**
 * Fixture dev server. FIXTURE_MODE: 'ready' answers 200 after FIXTURE_DELAY_MS; 'never' accepts
 * connections but never answers. FIXTURE_CHILD=1 spawns a child that sleeps, printing its pid.
 * The port comes from --listen <port> when given, else from PORT. On a listen error it prints the
 * error code and exits 1, like a real dev server losing a port race.
 */
const FIXTURE = `
import http from 'node:http';
import { spawn } from 'node:child_process';
const at = process.argv.indexOf('--listen');
const port = at > 0 ? Number(process.argv[at + 1]) : Number(process.env.PORT);
const mode = process.env.FIXTURE_MODE ?? 'ready';
const delay = Number(process.env.FIXTURE_DELAY_MS ?? 0);
console.log('fixture-start mode=' + mode + ' port=' + port + ' env-port=' + process.env.FIXTURE_PORT);
if (process.env.FIXTURE_CHILD === '1') {
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
  console.log('child=' + child.pid);
}
const server = http.createServer((request, response) => {
  if (mode === 'never') return;
  response.writeHead(200, { 'content-type': 'text/plain' });
  response.end('ok');
});
server.on('error', (error) => { console.error('listen failed: ' + error.code); process.exit(1); });
setTimeout(() => server.listen(port, '127.0.0.1', () => console.log('listening ' + port)), mode === 'never' ? 0 : delay);
`;

let dir: string;
let script: string;
const cleanup: Array<() => Promise<unknown>> = [];

beforeAll(async () => {
  dir = await mkdtemp(path.join(tmpdir(), 'rk-qa-devserver-'));
  script = path.join(dir, 'fixture-server.mjs');
  await writeFile(script, FIXTURE);
});
afterAll(async () => {
  for (const close of cleanup.splice(0).reverse()) await close();
  await rm(dir, { recursive: true, force: true });
});

/** Polls until the pid is gone; false if it is still alive after the bound. */
async function gone(pid: number, timeoutMs = 4_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (isProcessAlive(pid)) {
    if (Date.now() >= deadline) return false;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return true;
}
/** Holds a port on 127.0.0.1 so another listener on it gets EADDRINUSE. */
async function occupy(port: number): Promise<Server> {
  const server = createServer();
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(port, '127.0.0.1', () => resolve()); });
  cleanup.push(() => new Promise<void>((resolve) => server.close(() => resolve())));
  return server;
}
const readiness = (attemptTimeoutMs = 300, totalTimeoutMs = 10_000) => ({
  urls: (port: number) => [`http://127.0.0.1:${port}/`],
  attemptTimeoutMs,
  totalTimeoutMs,
  intervalMs: 50,
});

describe('startDevServer', () => {
  it('becomes ready after a delay; the port reaches the process by env and by args template; stop() leaves no tree', async () => {
    const port = await freePort();
    const dev = await startDevServer({
      command: process.execPath,
      args: [script, '--listen', '{port}'],
      portEnv: 'FIXTURE_PORT',
      env: { FIXTURE_MODE: 'ready', FIXTURE_DELAY_MS: '400' },
      ports: [port],
      readiness: readiness(),
    });
    expect(dev).toMatchObject({ port, url: `http://127.0.0.1:${port}`, attempts: 1 });
    expect(dev.pid).toBeGreaterThan(0);
    expect(dev.output()).toContain(`port=${port} env-port=${port}`);
    const response = await fetch(`${dev.url}/`);
    expect(response.status).toBe(200);
    const report = await dev.stop();
    expect(report.leftoverPids).toEqual([]);
    expect(await gone(dev.pid)).toBe(true);
    expect(await dev.stop()).toEqual({ leftoverPids: [] });
  }, 20_000);

  it('a server that never answers times out: the whole tree is gone and the error carries the output tail and pid', async () => {
    const port = await freePort();
    const started = Date.now();
    const error = await startDevServer({
      command: process.execPath,
      args: [script],
      env: { FIXTURE_MODE: 'never', FIXTURE_CHILD: '1', PORT: String(port) },
      ports: [port],
      readiness: readiness(200, 1_200),
    }).catch((caught) => caught);
    expect(error).toBeInstanceOf(DevServerError);
    expect(error.reason).toBe('timeout');
    expect(Date.now() - started).toBeLessThan(10_000);
    expect(error.output).toContain('fixture-start mode=never');
    expect(error.message).toContain('fixture-start mode=never');
    expect(error.leftoverPids).toEqual([]);
    const childPid = Number(/child=(\d+)/.exec(error.output)?.[1]);
    expect(childPid).toBeGreaterThan(0);
    expect(await gone(error.pid)).toBe(true);
    expect(await gone(childPid)).toBe(true);
  }, 20_000);

  it('stop() kills the child the server spawned', async () => {
    const port = await freePort();
    const dev = await startDevServer({
      command: process.execPath,
      args: [script],
      env: { FIXTURE_MODE: 'ready', FIXTURE_CHILD: '1', PORT: String(port) },
      ports: [port],
      readiness: readiness(),
    });
    const childPid = Number(/child=(\d+)/.exec(dev.output())?.[1]);
    expect(childPid).toBeGreaterThan(0);
    expect(isProcessAlive(childPid)).toBe(true);
    expect((await dev.stop()).leftoverPids).toEqual([]);
    expect(await gone(childPid)).toBe(true);
    expect(await gone(dev.pid)).toBe(true);
  }, 20_000);

  it('a process that exits before it is ready fails with reason exited and its output', async () => {
    const error = await startDevServer({
      command: process.execPath,
      args: ['-e', "console.log('boom-before-ready'); process.exit(3)"],
      ports: [await freePort()],
      readiness: readiness(),
    }).catch((caught) => caught);
    expect(error).toBeInstanceOf(DevServerError);
    expect(error.reason).toBe('exited');
    expect(error.output).toContain('boom-before-ready');
  }, 20_000);

  it('portFloor is honoured: the chosen port is at or above the floor and skips an occupied one', async () => {
    const floor = await freePort();
    await occupy(floor);
    const dev = await startDevServer({
      command: process.execPath,
      args: [script],
      env: { FIXTURE_MODE: 'ready' },
      portFloor: floor,
      readiness: readiness(),
    });
    try {
      expect(dev.port).toBeGreaterThan(floor);
      expect((await fetch(dev.url)).status).toBe(200);
    } finally {
      await dev.stop();
    }
  }, 20_000);

  it('a port collision on a listed port is retried on the next listed port', async () => {
    const busy = await freePort();
    await occupy(busy);
    const next = await freePort();
    const dev = await startDevServer({
      command: process.execPath,
      args: [script],
      env: { FIXTURE_MODE: 'ready' },
      ports: [busy, next],
      readiness: readiness(),
    });
    try {
      expect(dev.port).toBe(next);
      expect(dev.attempts).toBe(2);
      expect((await fetch(dev.url)).status).toBe(200);
    } finally {
      await dev.stop();
    }
  }, 20_000);

  it('a collision on every listed port fails with reason port-in-use and leaves nothing running', async () => {
    const busy = await freePort();
    await occupy(busy);
    const error = await startDevServer({
      command: process.execPath,
      args: [script],
      env: { FIXTURE_MODE: 'ready' },
      ports: [busy],
      readiness: readiness(),
    }).catch((caught) => caught);
    expect(error).toBeInstanceOf(DevServerError);
    expect(error.reason).toBe('port-in-use');
    expect(error.leftoverPids).toEqual([]);
    // The held port is skipped before any spawn: nothing was started, so there is no pid.
    expect(error.pid).toBeUndefined();
    expect(error.attempts).toBe(1);
  }, 20_000);

  it('a check predicate replaces the URL probe', async () => {
    const dev = await startDevServer({
      command: process.execPath,
      args: [script],
      env: { FIXTURE_MODE: 'never' },
      ports: [await freePort()],
      readiness: { attemptTimeoutMs: 200, totalTimeoutMs: 5_000, intervalMs: 50, check: (port) => port > 0 },
    });
    try {
      expect(dev.pid).toBeGreaterThan(0);
    } finally {
      await dev.stop();
    }
  }, 20_000);

  it('refuses a missing readiness probe or a non-positive budget before spawning anything', async () => {
    await expect(startDevServer({ command: process.execPath, readiness: { attemptTimeoutMs: 100, totalTimeoutMs: 100, intervalMs: 10 } })).rejects.toThrow('urls or check');
    await expect(startDevServer({ command: process.execPath, readiness: { urls: [], attemptTimeoutMs: 0, totalTimeoutMs: 100, intervalMs: 10 } })).rejects.toThrow('Timeout');
    await expect(startDevServer({ command: '', readiness: readiness() })).rejects.toThrow('command');
  });
});
