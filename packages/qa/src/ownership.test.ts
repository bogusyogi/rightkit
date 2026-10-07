import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { acquireRunOwnership } from './ownership.js';

describe('QA run ownership', () => {
  it('only lets the owning token release a live lock', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rightkit-owner-'));
    const lockDir = path.join(root, 'run.lock');
    const ownership = await acquireRunOwnership(lockDir, 'app-smoke');
    const ownerPath = path.join(lockDir, 'owner.json');
    const owner = JSON.parse(fs.readFileSync(ownerPath, 'utf8'));

    fs.writeFileSync(ownerPath, JSON.stringify({ ...owner, token: 'replacement-owner' }));
    ownership.release();
    expect(fs.existsSync(lockDir)).toBe(true);
  });

  it('reclaims a dead owner without touching a live owner', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rightkit-owner-'));
    const lockDir = path.join(root, 'run.lock');
    fs.mkdirSync(lockDir, { recursive: true });
    fs.writeFileSync(
      path.join(lockDir, 'owner.json'),
      JSON.stringify({ label: 'dead-run', pid: 424242, token: 'dead', startedAtMs: 1 }),
    );

    const ownership = await acquireRunOwnership(lockDir, 'replacement', {
      isProcessAlive: () => false,
      staleMs: 60_000,
    });
    const owner = JSON.parse(fs.readFileSync(path.join(lockDir, 'owner.json'), 'utf8'));
    expect(owner.label).toBe('replacement');
    expect(owner.token).not.toBe('dead');
    ownership.release();
    expect(fs.existsSync(lockDir)).toBe(false);
  });
});
