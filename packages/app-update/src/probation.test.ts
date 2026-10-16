import { describe, expect, it, vi } from 'vitest';
import { createBootProbation, type BootProbationRecord } from './probation.js';

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
  it('allows maxBootAttempts boots then requests rollback and calls the hook once', async () => {
    const storage = memory();
    const onRollback = vi.fn();
    const p = createBootProbation({ storage, maxBootAttempts: 2, onRollback });
    await p.begin('1.0.0', '1.1.0');
    expect(await p.evaluateBoot('1.1.0')).toMatchObject({ kind: 'probation', bootAttempts: 1 });
    expect(await p.evaluateBoot('1.1.0')).toMatchObject({ kind: 'probation', bootAttempts: 2 });
    expect(await p.evaluateBoot('1.1.0')).toMatchObject({
      kind: 'rollback', restoreVersion: '1.0.0', toVersion: '1.1.0', bootAttempts: 3,
    });
    expect(onRollback).toHaveBeenCalledTimes(1);
    expect(onRollback).toHaveBeenCalledWith({ fromVersion: '1.0.0', toVersion: '1.1.0' });
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
});
