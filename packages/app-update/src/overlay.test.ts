import { describe, expect, it, vi } from 'vitest';
import { createUpdateConsent, deriveUpdateOverlay } from './overlay.js';

describe('update overlay model', () => {
  it('maps every status kind', () => {
    expect(deriveUpdateOverlay({ kind: 'idle' }).phase).toBe('hidden');
    expect(deriveUpdateOverlay({ kind: 'checking' }).phase).toBe('checking');
    expect(deriveUpdateOverlay({ kind: 'up_to_date' }).phase).toBe('up_to_date');
    expect(deriveUpdateOverlay({ kind: 'ready', version: '2', tier: 'patch' }).phase).toBe('ready');
    expect(deriveUpdateOverlay({ kind: 'error', message: 'x' })).toMatchObject({ phase: 'error', message: 'x' });
    expect(deriveUpdateOverlay({ kind: 'deferred', version: '2', tier: 'patch', reason: 'busy' })).toMatchObject({
      phase: 'deferred',
      reason: 'busy',
    });
  });
  it('offers download only for an eligible update; non-eligible Pro shows its reason and no action', () => {
    expect(deriveUpdateOverlay({ kind: 'available', version: '2', tier: 'patch', eligible: true }).actions.download).toBe(true);
    const pro = deriveUpdateOverlay({ kind: 'available', version: '2', tier: 'update', eligible: false, reason: 'pro_required' });
    expect(pro).toMatchObject({ phase: 'available_ineligible', reason: 'pro_required' });
    expect(pro.actions.download || pro.actions.install).toBe(false);
  });
  it('bounds download progress', () => {
    expect(deriveUpdateOverlay({ kind: 'downloading', version: '2', tier: 'patch', pct: 250 }).progress).toBe(100);
    expect(deriveUpdateOverlay({ kind: 'downloading', version: '2', tier: 'patch', pct: -5 }).progress).toBe(0);
    expect(deriveUpdateOverlay({ kind: 'downloading', version: '2', tier: 'patch', pct: Number.NaN }).progress).toBe(0);
  });
});

describe('update consent', () => {
  it('allow once runs only the requested action and consumes the approval', async () => {
    const c = createUpdateConsent();
    const run = vi.fn();
    c.request('download');
    await c.allowOnce(run);
    expect(run).toHaveBeenCalledExactlyOnceWith('download');
    expect(c.state().pending).toBeNull();
    await c.allowOnce(run);
    expect(run).toHaveBeenCalledTimes(1);
  });
  it("don't allow runs nothing", async () => {
    const c = createUpdateConsent();
    const run = vi.fn();
    c.request('install');
    c.deny();
    await c.allowOnce(run);
    expect(run).not.toHaveBeenCalled();
  });
  it('is busy during the run, surfaces failure, and needs a new request to retry', async () => {
    const c = createUpdateConsent();
    let release!: () => void;
    const gate = new Promise<void>((r) => { release = r; });
    c.request('install');
    const pending = c.allowOnce(async () => { await gate; throw new Error('bad signature'); });
    expect(c.state().busy).toBe(true);
    c.request('download');
    c.deny();
    expect(c.state()).toMatchObject({ pending: 'install', busy: true });
    release();
    await pending;
    expect(c.state()).toMatchObject({ busy: false, pending: null });
    expect(c.state().error).toMatch(/bad signature/);
    const run = vi.fn();
    await c.allowOnce(run);
    expect(run).not.toHaveBeenCalled();
  });
});
