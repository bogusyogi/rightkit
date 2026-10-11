import { spawn } from 'node:child_process';
import { createServer } from 'node:net';

export interface OwnedProcessResult {
  pid: number;
  code: number | null;
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
}

export interface RunOwnedProcessOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  timeoutMs?: number;
  windowsHide?: boolean;
}

export class ProcessTimeoutError extends Error {
  readonly pid: number;
  readonly stdout: string;
  readonly stderr: string;

  constructor(pid: number, timeoutMs: number, stdout: string, stderr: string) {
    super(`process ${pid} exceeded ${timeoutMs}ms and its owned tree was terminated`);
    this.name = 'ProcessTimeoutError';
    this.pid = pid;
    this.stdout = stdout;
    this.stderr = stderr;
  }
}

export function runOwnedProcess(
  command: string,
  args: string[] = [],
  options: RunOwnedProcessOptions = {},
): Promise<OwnedProcessResult> {
  const timeoutMs = options.timeoutMs ?? 120_000;
  const child = spawn(command, args, {
    cwd: options.cwd,
    env: options.env,
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: options.windowsHide ?? true,
    detached: process.platform !== 'win32',
  });
  if (!child.pid) return Promise.reject(new Error(`failed to start owned process: ${command}`));
  const pid = child.pid;
  let stdout = '';
  let stderr = '';
  child.stdout?.setEncoding('utf8');
  child.stderr?.setEncoding('utf8');
  child.stdout?.on('data', (chunk: string) => { stdout += chunk; });
  child.stderr?.on('data', (chunk: string) => { stderr += chunk; });

  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(async () => {
      if (settled) return;
      settled = true;
      await killProcessTree(pid);
      reject(new ProcessTimeoutError(pid, timeoutMs, stdout, stderr));
    }, timeoutMs);

    child.once('error', (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(error);
    });
    child.once('close', (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ pid, code, signal, stdout, stderr });
    });
  });
}

/**
 * A free TCP port on 127.0.0.1, found by binding port 0 and releasing it. Another process can take
 * the port before the caller binds it; bind promptly or retry on EADDRINUSE.
 */
export function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      if (!address || typeof address === 'string') { server.close(); reject(new Error('freePort: no TCP address')); return; }
      const port = address.port;
      server.close((error) => (error ? reject(error) : resolve(port)));
    });
  });
}

export function isProcessAlive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM';
  }
}

export async function killProcessTree(pid: number): Promise<void> {
  if (!isProcessAlive(pid)) return;
  if (process.platform === 'win32') {
    await runCleanupCommand('taskkill.exe', ['/PID', String(pid), '/T', '/F']);
    return;
  }

  try {
    process.kill(-pid, 'SIGTERM');
  } catch {
    try { process.kill(pid, 'SIGTERM'); } catch { return; }
  }
  await waitForExit(pid, 1_000);
  if (!isProcessAlive(pid)) return;
  try {
    process.kill(-pid, 'SIGKILL');
  } catch {
    try { process.kill(pid, 'SIGKILL'); } catch { return; }
  }
  await waitForExit(pid, 1_000);
}

async function runCleanupCommand(command: string, args: string[]): Promise<void> {
  await new Promise<void>((resolve) => {
    const cleanup = spawn(command, args, { stdio: 'ignore', windowsHide: true });
    cleanup.once('error', () => resolve());
    cleanup.once('close', () => resolve());
  });
}

async function waitForExit(pid: number, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline && isProcessAlive(pid)) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}
