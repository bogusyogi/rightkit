import { describe, expect, it, vi } from 'vitest';
import { createBootProbation, previousInstallPath, type BootProbationRecord } from './probation.js';

function memory() {
  let record: BootProbationRecord | null = null;
  return {
    read: () => record,
    write: (r: BootProbationRecord) => { record = r; },
    clear: () => { record = null; },
    peek: () => record,
  };
}

describe('boot probation', () => {
  it('reports none when no install is pending', async () => {
    const p = createBootProbation({ storage: memory() });
    expect(await p.evaluateBoot('1.0.0')).toEqual({ kind: 'none' });
  });
  it('allows maxBootAttempts boots then declares boot failure (reinstall, no restore) once', async () => {
    const storage = memory();
    const onBootFailure = vi.fn();
    const onRollback = vi.fn();
    const p = createBootProbation({ storage, maxBootAttempts: 2, onBootFailure, onRollback });
    await p.begin('1.0.0', '1.1.0');
    expect(await p.evaluateBoot('1.1.0')).toMatchObject({ kind: 'probation', bootAttempts: 1 });
    expect(await p.evaluateBoot('1.1.0')).toMatchObject({ kind: 'probation', bootAttempts: 2 });
    const decision = await p.evaluateBoot('1.1.0');
    expect(decision).toMatchObject({
      kind: 'reinstall_required', failedVersion: '1.1.0', fromVersion: '1.0.0', bootAttempts: 3, guidance: 'reinstall',
    });
    expect(decision).not.toHaveProperty('restorePath');
    expect(onBootFailure).toHaveBeenCalledTimes(1);
    expect(onBootFailure).toHaveBeenCalledWith({ fromVersion: '1.0.0', toVersion: '1.1.0', bootAttempts: 3, guidance: 'reinstall' });
    expect(onRollback).toHaveBeenCalledTimes(1);
    expect(storage.peek()).toBeNull();
  });
  it('acknowledge clears the record so later boots are none', async () => {
    const storage = memory();
    const p = createBootProbation({ storage });
    await p.begin('1.0.0', '1.1.0');
    await p.evaluateBoot('1.1.0');
    await p.acknowledgeHealthy();
    expect(await p.evaluateBoot('1.1.0')).toEqual({ kind: 'none' });
  });
  it('clears a record for a different running version as stale, without rollback', async () => {
    const storage = memory();
    const onRollback = vi.fn();
    const p = createBootProbation({ storage, onRollback });
    await p.begin('1.0.0', '1.1.0');
    expect(await p.evaluateBoot('1.0.0')).toEqual({ kind: 'stale' });
    expect(onRollback).not.toHaveBeenCalled();
    expect(storage.peek()).toBeNull();
  });
  it('rejects an invalid attempt limit', () => {
    expect(() => createBootProbation({ storage: memory(), maxBootAttempts: 0 })).toThrow(/positive integer/);
  });
  it('defines the rollback copy as <install>.previous beside the fixed path', () => {
    expect(previousInstallPath('/Applications/Mail.app')).toBe('/Applications/Mail.app.previous');
    expect(previousInstallPath('/Applications/Mail.app/')).toBe('/Applications/Mail.app.previous');
    expect(previousInstallPath('C:\\Apps\\Mail')).toBe('C:\\Apps\\Mail.previous');
    expect(() => previousInstallPath('')).toThrow(/non-empty/);
    expect(() => previousInstallPath('/')).toThrow(/filesystem root/);
    expect(() => previousInstallPath('/Applications/Mail.app.previous')).toThrow(/fixed install path/);
  });
  it('the sentinel carries no .previous payload path; legacy previousPath is stripped, never restored', async () => {
    const storage = memory();
    const onBootFailure = vi.fn();
    const p = createBootProbation({ storage, maxBootAttempts: 1, onBootFailure });
    await p.begin('1.0.0', '1.1.0', { installPath: '/Applications/Mail.app' });
    expect(storage.peek()).toEqual({ fromVersion: '1.0.0', toVersion: '1.1.0', bootAttempts: 0, installPath: '/Applications/Mail.app' });
    await p.evaluateBoot('1.1.0');
    const decision = await p.evaluateBoot('1.1.0');
    expect(decision).toMatchObject({ kind: 'reinstall_required', installPath: '/Applications/Mail.app' });
    expect(decision).not.toHaveProperty('restorePath');
    expect(onBootFailure).toHaveBeenCalledWith({
      fromVersion: '1.0.0', toVersion: '1.1.0', installPath: '/Applications/Mail.app', bootAttempts: 2, guidance: 'reinstall',
    });
    // A legacy record with previousPath is readable but the field is dropped on rewrite.
    const legacy = memory();
    legacy.write({ fromVersion: '1.0.0', toVersion: '1.1.0', bootAttempts: 0, installPath: '/Applications/Mail.app', previousPath: '/Applications/Mail.app.previous' });
    const lp = createBootProbation({ storage: legacy, maxBootAttempts: 2 });
    await lp.evaluateBoot('1.1.0');
    expect(legacy.peek()).not.toHaveProperty('previousPath');
  });
});
