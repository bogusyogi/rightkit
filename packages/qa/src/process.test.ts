import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { describe, expect, it } from 'vitest';
import { freePort, isProcessAlive, killProcessTree, runOwnedProcess } from './process.js';

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
