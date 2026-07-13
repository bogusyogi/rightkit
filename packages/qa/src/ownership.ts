import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { isProcessAlive as defaultIsProcessAlive } from './process.js';

export interface RunOwner {
  label: string;
  pid: number;
  token: string;
  startedAt: string;
  startedAtMs: number;
}

export interface RunOwnership {
  owner: RunOwner;
  release(): void;
}

export interface RunOwnershipOptions {
  timeoutMs?: number;
  staleMs?: number;
  pollIntervalMs?: number;
  isProcessAlive?: (pid: number) => boolean;
}

export async function acquireRunOwnership(
  lockDir: string,
  label: string,
  options: RunOwnershipOptions = {},
): Promise<RunOwnership> {
  const resolvedLockDir = path.resolve(lockDir);
  const ownerPath = path.join(resolvedLockDir, 'owner.json');
  const timeoutMs = options.timeoutMs ?? 120_000;
  const staleMs = options.staleMs ?? 30 * 60_000;
  const pollIntervalMs = options.pollIntervalMs ?? 250;
  const isProcessAlive = options.isProcessAlive ?? defaultIsProcessAlive;
  const waitingSince = Date.now();
  const owner: RunOwner = {
    label: label.trim() || 'qa-run',
    pid: process.pid,
    token: crypto.randomUUID(),
    startedAt: new Date().toISOString(),
    startedAtMs: Date.now(),
  };

  fs.mkdirSync(path.dirname(resolvedLockDir), { recursive: true });
  while (true) {
    try {
      fs.mkdirSync(resolvedLockDir);
      fs.writeFileSync(ownerPath, `${JSON.stringify(owner, null, 2)}\n`, { encoding: 'utf8', flag: 'wx' });
      return {
        owner,
        release() {
          const current = readOwner(ownerPath);
          if (current?.token === owner.token) {
            fs.rmSync(resolvedLockDir, { recursive: true, force: true });
          }
        },
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') {
        fs.rmSync(resolvedLockDir, { recursive: true, force: true });
        throw error;
      }

      const current = readOwner(ownerPath);
      const ageMs = lockAgeMs(resolvedLockDir, current);
      const deadOwner = current ? !isProcessAlive(current.pid) : false;
      if (deadOwner || ageMs > staleMs) {
        fs.rmSync(resolvedLockDir, { recursive: true, force: true });
        continue;
      }
      if (Date.now() - waitingSince >= timeoutMs) {
        throw new Error(
          `timed out waiting for QA run lock held by ${current?.label ?? 'unknown'} pid ${current?.pid ?? 'unknown'}`,
        );
      }
      await delay(pollIntervalMs);
    }
  }
}

function readOwner(ownerPath: string): RunOwner | null {
  try {
    return JSON.parse(fs.readFileSync(ownerPath, 'utf8')) as RunOwner;
  } catch {
    return null;
  }
}

function lockAgeMs(lockDir: string, owner: RunOwner | null): number {
  if (typeof owner?.startedAtMs === 'number') return Date.now() - owner.startedAtMs;
  try {
    return Date.now() - fs.statSync(lockDir).mtimeMs;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

async function delay(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
