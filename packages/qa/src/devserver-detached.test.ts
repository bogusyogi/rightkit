import type { ChildProcess } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { DevServerIdentityError, readDetachedRecord, stopDevServer, type DevServerRuntime, type ProcessIdentity } from './devserver-detached.js';
import { startDevServer } from './devserver.js';
import { freePort } from './process.js';

const START = 'Sat Oct 11 13:52:01 2026';
const COMMAND = '/usr/local/bin/node /srv/fixture.mjs --port 5555';
const readiness = { check: () => true, attemptTimeoutMs: 100, totalTimeoutMs: 2_000, intervalMs: 10 };

const cleanup: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const close of cleanup.splice(0).reverse()) await close(); });

async function scratch() {
  const dir = await mkdtemp(path.join(tmpdir(), 'rk-qa-detached-'));
  cleanup.push(() => rm(dir, { recursive: true, force: true }));
  return dir;
}

/** A child stand-in: an event emitter with a pid, no stdio pipes, and a count of unref calls. */
class FakeChild extends EventEmitter {
  readonly stdout = null;
  readonly stderr = null;
  unrefCalls = 0;
  constructor(readonly pid: number) { super(); }
  unref() { this.unrefCalls++; }
}

/**
 * Fake spawn and process lookup over an in-memory process table. Spawning adds the pid to the table with START and
 * COMMAND; stopTree removes it. Every call is recorded, so a test can prove what was (not) signalled.
 */
function fakeRuntime(pid: number) {
  const table = new Map<number, ProcessIdentity>();
  const spawns: Array<{ command: string; args: string[]; options: any }> = [];
  const children: FakeChild[] = [];
  const stops: number[] = [];
  let lookups = 0;
  const runtime: DevServerRuntime = {
    spawn(command, args, options) {
      spawns.push({ command, args, options });
      const child = new FakeChild(pid);
      children.push(child);
      table.set(pid, { pid, startTime: START, command: COMMAND });
      return child as unknown as ChildProcess;
    },
    lookup(target) {
      lookups++;
      const found = table.get(target);
      return found && { ...found };
    },
    async stopTree(target) {
      stops.push(target);
      table.delete(target);
    },
  };
  return { runtime, table, spawns, children, stops, lookups: () => lookups };
}

describe('detached dev server (0.4.8)', () => {
  it('writes a record with pid, start time, command, url and log path, reads it back, and the handle exposes the same fields', async () => {
    const dir = await scratch();
    const recordPath = path.join(dir, 'dev.json');
    const fake = fakeRuntime(5101);
    const port = await freePort();
    const dev = await startDevServer({
      command: '/srv/fixture', args: ['--port', '{port}'], detached: true, recordPath, ports: [port], readiness, runtime: fake.runtime,
    });
    expect(dev).toMatchObject({ pid: 5101, port, url: `http://127.0.0.1:${port}`, startTime: START, command: COMMAND, recordPath, logPath: `${recordPath}.log` });
    const [spawn] = fake.spawns;
    expect(spawn.args).toEqual(['--port', String(port)]);
    expect(spawn.options).toMatchObject({ detached: true, stdio: ['ignore', expect.any(Number), expect.any(Number)] });
    expect(spawn.options.stdio[1]).toBe(spawn.options.stdio[2]);
    expect(fake.children[0].unrefCalls).toBe(1);

    expect(await readDetachedRecord(recordPath)).toEqual({
      format: 'rightkit-dev-server/1', pid: 5101, startTime: START, command: COMMAND, url: `http://127.0.0.1:${port}`,
      port, logPath: `${recordPath}.log`, recordedAt: expect.any(String),
    });
    expect(JSON.parse(await readFile(recordPath, 'utf8')).pid).toBe(5101);
    expect(dev.output()).toBe('');
    await dev.stop();
    expect(fake.stops).toEqual([5101]);
    expect(existsSync(recordPath)).toBe(false);
  });

  it('stop refuses a pid whose start time or command does not match the record, and signals nothing', async () => {
    const dir = await scratch();
    const recordPath = path.join(dir, 'dev.json');
    const fake = fakeRuntime(5102);
    const dev = await startDevServer({ command: '/srv/fixture', detached: true, recordPath, ports: [await freePort()], readiness, runtime: fake.runtime });

    fake.table.set(5102, { pid: 5102, startTime: 'Sun Oct 12 09:00:00 2026', command: COMMAND });
    await expect(stopDevServer({ recordPath }, fake.runtime)).rejects.toThrow('start time differs');
    await expect(stopDevServer({ pid: 5102 }, fake.runtime)).rejects.toBeInstanceOf(DevServerIdentityError);

    fake.table.set(5102, { pid: 5102, startTime: START, command: '/bin/other' });
    await expect(stopDevServer({ recordPath }, fake.runtime)).rejects.toThrow('command differs');
    await expect(stopDevServer({ pid: 5102 }, fake.runtime)).rejects.toThrow('command differs');

    expect(fake.stops).toEqual([]);
    expect(existsSync(recordPath)).toBe(true);

    // The true identity again: the handle stops its own server.
    fake.table.set(5102, { pid: 5102, startTime: START, command: COMMAND });
    await dev.stop();
    expect(fake.stops).toEqual([5102]);
  });

  it('stop with a pid this process did not start refuses, and stop with an absent record is a no-op returning false', async () => {
    const dir = await scratch();
    const fake = fakeRuntime(5103);
    await expect(stopDevServer({ pid: 5199 }, fake.runtime)).rejects.toThrow('no identity is recorded');
    await expect(stopDevServer({ recordPath: path.join(dir, 'missing.json') }, fake.runtime)).resolves.toBe(false);
    expect(fake.lookups()).toBe(0);
    expect(fake.stops).toEqual([]);
  });

  it('stop of a running recorded server stops its tree, removes the record and returns true; a server already gone returns false and removes its record', async () => {
    const dir = await scratch();
    const first = path.join(dir, 'first.json');
    const fakeFirst = fakeRuntime(5104);
    await startDevServer({ command: '/srv/fixture', detached: true, recordPath: first, ports: [await freePort()], readiness, runtime: fakeFirst.runtime });
    await expect(stopDevServer({ recordPath: first }, fakeFirst.runtime)).resolves.toBe(true);
    expect(fakeFirst.stops).toEqual([5104]);
    expect(existsSync(first)).toBe(false);

    const second = path.join(dir, 'second.json');
    const fakeSecond = fakeRuntime(5105);
    await startDevServer({ command: '/srv/fixture', detached: true, recordPath: second, ports: [await freePort()], readiness, runtime: fakeSecond.runtime });
    fakeSecond.table.delete(5105);
    await expect(stopDevServer({ recordPath: second }, fakeSecond.runtime)).resolves.toBe(false);
    expect(fakeSecond.stops).toEqual([]);
    expect(existsSync(second)).toBe(false);
  });

  it('stop by pid works for a server this process started, and a server that survives the stop throws and keeps its record', async () => {
    const dir = await scratch();
    const recordPath = path.join(dir, 'stubborn.json');
    const fake = fakeRuntime(5106);
    const dev = await startDevServer({ command: '/srv/fixture', detached: true, recordPath, ports: [await freePort()], readiness, runtime: fake.runtime });
    const stubborn = { ...fake.runtime, stopTree: async () => {} };
    await expect(stopDevServer({ recordPath }, stubborn)).rejects.toThrow('still running after stop');
    expect(existsSync(recordPath)).toBe(true);
    await expect(stopDevServer({ pid: dev.pid }, fake.runtime)).resolves.toBe(true);
    expect(existsSync(recordPath)).toBe(false);
  });

  it('validates detached options, and refuses to overwrite an existing record before spawning anything', async () => {
    const dir = await scratch();
    const recordPath = path.join(dir, 'taken.json');
    const fake = fakeRuntime(5107);
    await expect(startDevServer({ command: '/srv/fixture', detached: true, ports: [await freePort()], readiness, runtime: fake.runtime })).rejects.toThrow('detached needs recordPath');
    await expect(startDevServer({ command: '/srv/fixture', recordPath, readiness, runtime: fake.runtime })).rejects.toThrow('requires detached: true');
    await writeFile(recordPath, '{}');
    await expect(startDevServer({ command: '/srv/fixture', detached: true, recordPath, ports: [await freePort()], readiness, runtime: fake.runtime })).rejects.toThrow('already exists');
    expect(fake.spawns).toEqual([]);
    // A file that is not a record is never acted on.
    await expect(stopDevServer({ recordPath }, fake.runtime)).rejects.toThrow('malformed');
    expect(fake.stops).toEqual([]);
  });

  it('without detached the server is unchanged: no record, no log, and the detached runtime is never used', async () => {
    const fake = fakeRuntime(5108);
    const dev = await startDevServer({
      command: process.execPath, args: ['-e', 'setInterval(() => {}, 1000)'], ports: [await freePort()], readiness, runtime: fake.runtime,
    });
    try {
      expect(dev.url).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
      expect(dev.recordPath).toBeUndefined();
      expect(dev.logPath).toBeUndefined();
      expect(dev.startTime).toBeUndefined();
      expect(dev.command).toBeUndefined();
      expect(fake.spawns).toEqual([]);
    } finally {
      await dev.stop();
    }
  });
});
