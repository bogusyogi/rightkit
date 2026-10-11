import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { describe, expect, it } from 'vitest';
import { freePort, freePortSync, isProcessAlive, killProcessTree, ownedProcessReceipt, runOwnedProcess, spawnOwnedProcess, stopOwnedProcess } from './process.js';

async function waitUntilDead(pid: number, timeoutMs = 5_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline && isProcessAlive(pid)) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

describe('owned process cleanup', () => {
  it('kills the timed-out process tree and leaves an unrelated process alive', async () => {
    const sibling = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
      stdio: 'ignore',
      windowsHide: true,
    });
    if (!sibling.pid) throw new Error('missing sibling pid');

    const parentProgram = [
      "const { spawn } = require('node:child_process')",
      "const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' })",
      'console.log(child.pid)',
      'setInterval(() => {}, 1000)',
    ].join(';');

    try {
      const result = runOwnedProcess(process.execPath, ['-e', parentProgram], { timeoutMs: 300 });
      await expect(result).rejects.toMatchObject({ name: 'ProcessTimeoutError' });
      try {
        await result;
      } catch (error) {
        const timeout = error as Error & { pid: number; stdout: string };
        const grandchildPid = Number(timeout.stdout.trim().split(/\s+/)[0]);
        await waitUntilDead(timeout.pid);
        await waitUntilDead(grandchildPid);
        expect(isProcessAlive(timeout.pid)).toBe(false);
        expect(isProcessAlive(grandchildPid)).toBe(false);
      }
      expect(isProcessAlive(sibling.pid)).toBe(true);
    } finally {
      await killProcessTree(sibling.pid);
    }
  }, 15_000);
});

describe('freePort', () => {
  it('returns a loopback port number that can be bound immediately afterwards', async () => {
    const port = await freePort();
    expect(Number.isInteger(port) && port > 0 && port < 65536).toBe(true);
    const server = createServer();
    await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(port, '127.0.0.1', () => resolve()); });
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });
});

describe.skipIf(process.platform === 'win32')('spawnOwnedProcess registry (POSIX process groups)', () => {
  it('stops the whole launched group, grandchildren included, and refuses anything it did not launch', async () => {
    const owned = spawnOwnedProcess('/bin/sh', ['-c', 'sleep 600 & echo $!; wait'], { stdio: 'pipe' });
    const grandchild = await new Promise<number>((resolve, reject) => {
      owned.child.stdout!.setEncoding('utf8');
      owned.child.stdout!.once('data', (chunk: string) => resolve(Number.parseInt(chunk, 10)));
      owned.child.once('error', reject);
    });
    expect(grandchild).toBeGreaterThan(0);
    expect(ownedProcessReceipt().leftoverPids).toContain(owned.pid);
    // A bare pid, a forged handle, or this test process are all refused, and nothing is signalled.
    await expect(stopOwnedProcess(owned.pid as any)).rejects.toThrow('did not launch');
    await expect(stopOwnedProcess({ pid: owned.pid, child: owned.child } as any)).rejects.toThrow('did not launch');
    await expect(stopOwnedProcess({ pid: process.pid, child: owned.child } as any)).rejects.toThrow('did not launch');
    expect(isProcessAlive(owned.pid)).toBe(true);
    expect(isProcessAlive(grandchild)).toBe(true);

    await stopOwnedProcess(owned);
    await waitUntilDead(grandchild);
    expect(isProcessAlive(grandchild)).toBe(false);
    expect(ownedProcessReceipt().leftoverPids).not.toContain(owned.pid);
    await stopOwnedProcess(owned); // idempotent
  });

  it('the receipt lists launched processes whose group is still alive, and only those', async () => {
    const quick = spawnOwnedProcess('/bin/sh', ['-c', 'exit 0']);
    await new Promise<void>((resolve) => quick.child.once('exit', () => resolve()));
    const held = spawnOwnedProcess('/bin/sh', ['-c', 'sleep 600']);
    try {
      expect(ownedProcessReceipt().leftoverPids).toEqual([held.pid]);
    } finally {
      await stopOwnedProcess(held);
    }
  });
});

describe('spawnOwnedProcess registry (every platform: taskkill tree on Windows, process group on POSIX)', () => {
  const idle = 'setInterval(() => {}, 1000);';
  // The launched program starts a grandchild, prints its pid and stays alive.
  const parent = `const c = require('node:child_process').spawn(process.execPath, ['-e', ${JSON.stringify(idle)}], { stdio: 'ignore' }); console.log(c.pid); ${idle}`;

  it('stops the launched process and its grandchild, and refuses anything it did not launch', async () => {
    const owned = spawnOwnedProcess(process.execPath, ['-e', parent], { stdio: 'pipe' });
    let grandchild = 0;
    try {
      grandchild = await new Promise<number>((resolve, reject) => {
        owned.child.stdout!.setEncoding('utf8');
        owned.child.stdout!.once('data', (chunk: string) => resolve(Number.parseInt(chunk, 10)));
        owned.child.once('error', reject);
      });
      expect(grandchild).toBeGreaterThan(0);
      expect(ownedProcessReceipt().leftoverPids).toContain(owned.pid);
      await expect(stopOwnedProcess(owned.pid as any)).rejects.toThrow('did not launch');
      await expect(stopOwnedProcess({ pid: owned.pid, child: owned.child } as any)).rejects.toThrow('did not launch');
      expect(isProcessAlive(owned.pid)).toBe(true);
      expect(isProcessAlive(grandchild)).toBe(true);
    } finally {
      await stopOwnedProcess(owned);
    }
    await waitUntilDead(owned.pid);
    await waitUntilDead(grandchild);
    expect(isProcessAlive(owned.pid)).toBe(false);
    expect(isProcessAlive(grandchild)).toBe(false);
    expect(ownedProcessReceipt().leftoverPids).not.toContain(owned.pid);
    await stopOwnedProcess(owned); // idempotent
  }, 30_000);

  it('the receipt lists a launched process that is still running and drops one that exited', async () => {
    const quick = spawnOwnedProcess(process.execPath, ['-e', '']);
    await new Promise<void>((resolve) => quick.child.once('exit', () => resolve()));
    const held = spawnOwnedProcess(process.execPath, ['-e', idle]);
    try {
      const { leftoverPids } = ownedProcessReceipt();
      expect(leftoverPids).toContain(held.pid);
      expect(leftoverPids).not.toContain(quick.pid);
    } finally {
      await stopOwnedProcess(held);
    }
    await waitUntilDead(held.pid);
    expect(ownedProcessReceipt().leftoverPids).not.toContain(held.pid);
  }, 30_000);
});

describe('freePortSync (0.4.8)', () => {
  it('returns a port from 1 to 65535 that a 127.0.0.1 listener can bind', async () => {
    const port = freePortSync();
    expect(Number.isInteger(port)).toBe(true);
    expect(port).toBeGreaterThanOrEqual(1);
    expect(port).toBeLessThanOrEqual(65_535);
    const server = createServer();
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject);
      server.listen(port, '127.0.0.1', () => resolve());
    });
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });
});
