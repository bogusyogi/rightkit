import { describe, expect, it, vi } from 'vitest';
import { createAutoUpdateClient, type RuntimeUpdate } from './index.js';
import { createUpdateConsent, createUpdateConsentRunner, deriveUpdateOverlay } from './overlay.js';

function patch(install = vi.fn(async () => {})): RuntimeUpdate {
  return { version: '1.2.3', rawJson: { tier: 'patch' }, downloadAndInstall: install };
}

describe('staged update operations', () => {
  for (const prepared of [false, true]) {
    it(`${prepared ? 'prepared' : 'combined'} installs are fenced across check generations`, async () => {
      let finish!: () => void;
      let entered!: () => void;
      const gate = new Promise<void>((resolve) => { finish = resolve; });
      const started = new Promise<void>((resolve) => { entered = resolve; });
      const firstInstall = vi.fn(async () => { entered(); await gate; });
      const secondInstall = vi.fn(async () => {});
      const secondDownload = vi.fn(async () => {});
      const updates: RuntimeUpdate[] = [firstInstall, secondInstall].map((install, index) => ({
        version: `1.2.${index + 3}`, rawJson: { tier: 'patch' }, downloadAndInstall: install,
        ...(prepared ? { install, download: index === 0 ? vi.fn(async () => {}) : secondDownload } : {}),
      }));
      const client = createAutoUpdateClient({ check: async () => updates.shift()! });
      await client.checkOnce();
      const first = client.installAvailable();
      await started;
      await client.checkOnce();
      const second = await client.downloadAvailable();
      if (!second.ok) throw new Error('expected second generation handle');
      await expect(client.installStaged(second.handle)).resolves.toMatchObject({ kind: 'deferred', reason: 'install_in_progress' });
      expect(secondInstall).not.toHaveBeenCalled();
      if (prepared) expect(secondDownload).toHaveBeenCalledTimes(1);
      finish();
      await first;
      await expect(client.installStaged(second.handle)).resolves.toMatchObject({ kind: 'ready' });
      expect(secondInstall).toHaveBeenCalledTimes(1);
    });
  }

  it('keeps the client fence while relaunch is pending', async () => {
    let finish!: () => void;
    let entered!: () => void;
    const gate = new Promise<void>((resolve) => { finish = resolve; });
    const started = new Promise<void>((resolve) => { entered = resolve; });
    const relaunch = vi.fn(async () => { entered(); await gate; });
    const install = vi.fn(async () => {});
    const client = createAutoUpdateClient({ check: async () => patch(install), relaunchAfterInstall: true, relaunch });
    await client.checkOnce();
    const first = client.installAvailable();
    await started;
    await client.checkOnce();
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'deferred', reason: 'install_in_progress' });
    expect(install).toHaveBeenCalledTimes(1);
    finish();
    await first;
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'ready' });
    expect(install).toHaveBeenCalledTimes(2);
  });

  it('releases the client fence after native installation fails', async () => {
    const install = vi.fn()
      .mockRejectedValueOnce(new Error('native install failed'))
      .mockResolvedValueOnce(undefined);
    const client = createAutoUpdateClient({ check: async () => patch(install) });
    await client.checkOnce();
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'error', message: expect.stringContaining('native install failed') });
    await client.checkOnce();
    await expect(client.installAvailable()).resolves.toMatchObject({ kind: 'ready' });
    expect(install).toHaveBeenCalledTimes(2);
  });

  it('download stages without installing; install consumes the handle exactly once', async () => {
    const install = vi.fn(async () => {});
    const statuses: string[] = [];
    const client = createAutoUpdateClient({
      check: async () => patch(install),
      getTier: () => 'patch',
      onStatus: (status) => statuses.push(status.kind),
    });
    await client.checkOnce();
    const staged = await client.downloadAvailable();
    expect(install).not.toHaveBeenCalled();
    if (!staged.ok) throw new Error('expected a staged update');
    expect(staged.status).toMatchObject({ kind: 'staged', version: '1.2.3', tier: 'patch' });
    await expect(client.installStaged(staged.handle)).resolves.toMatchObject({ kind: 'ready', version: '1.2.3' });
    expect(install).toHaveBeenCalledTimes(1);
    expect(statuses).toEqual(['checking', 'available', 'staged', 'ready']);
    await expect(client.installStaged(staged.handle)).resolves.toMatchObject({ kind: 'error' });
    expect(install).toHaveBeenCalledTimes(1);
  });

  it('a newer check invalidates a staged handle without installing', async () => {
    const install = vi.fn(async () => {});
    const client = createAutoUpdateClient({ check: async () => patch(install), getTier: () => 'patch' });
    await client.checkOnce();
    const staged = await client.downloadAvailable();
    if (!staged.ok) throw new Error('expected a staged update');
    await client.checkOnce();
    await expect(client.installStaged(staged.handle)).resolves.toMatchObject({ kind: 'error' });
    expect(install).not.toHaveBeenCalled();
  });

  it('overlapping installs of one handle run the adapter once', async () => {
    let finish!: () => void;
    const gate = new Promise<void>((resolve) => {
      finish = resolve;
    });
    const install = vi.fn(async () => gate);
    const client = createAutoUpdateClient({ check: async () => patch(install), getTier: () => 'patch' });
    await client.checkOnce();
    const staged = await client.downloadAvailable();
    if (!staged.ok) throw new Error('expected a staged update');
    const first = client.installStaged(staged.handle);
    const second = client.installStaged(staged.handle);
    finish();
    const results = await Promise.all([first, second]);
    expect(install).toHaveBeenCalledTimes(1);
    expect(results.filter((result) => result.kind === 'ready')).toHaveLength(1);
    expect(results.filter((result) => result.kind === 'error')).toHaveLength(1);
  });

  it('a deferred install keeps its handle so a later install retries the same update', async () => {
    const install = vi.fn(async () => {});
    let safe = false;
    const client = createAutoUpdateClient({
      check: async () => patch(install),
      getTier: () => 'patch',
      canInstall: () => safe,
      deferReason: () => 'recording',
    });
    await client.checkOnce();
    const staged = await client.downloadAvailable();
    if (!staged.ok) throw new Error('expected a staged update');
    await expect(client.installStaged(staged.handle)).resolves.toMatchObject({ kind: 'deferred', reason: 'recording' });
    expect(install).not.toHaveBeenCalled();
    safe = true;
    await expect(client.installStaged(staged.handle)).resolves.toMatchObject({ kind: 'ready' });
    expect(install).toHaveBeenCalledTimes(1);
  });

  it('installAvailable keeps the combined status sequence with no staged emit', async () => {
    const statuses: string[] = [];
    const client = createAutoUpdateClient({
      check: async () => patch(),
      getTier: () => 'patch',
      onStatus: (status) => statuses.push(status.kind),
    });
    await client.checkOnce();
    await client.installAvailable();
    expect(statuses).toEqual(['checking', 'available', 'ready']);
  });

  it('prepared adapter: download really prepares, install consumes it with no second download', async () => {
    const download = vi.fn(async () => {});
    const install = vi.fn(async () => {});
    const combined = vi.fn(async () => {});
    const update: RuntimeUpdate = {
      version: '1.2.3',
      target: 'windows-x86_64',
      rawJson: { tier: 'patch', sha256: 'abc', signature: 'sig' },
      downloadAndInstall: combined,
      download,
      install,
    };
    const client = createAutoUpdateClient({ check: async () => update, getTier: () => 'patch', getHeaders: () => ({ Authorization: 'x' }) });
    await client.checkOnce();
    const staged = await client.downloadAvailable();
    if (!staged.ok) throw new Error('expected a staged update');
    expect(download).toHaveBeenCalledTimes(1);
    // Default strict headers: the prepare download received no manifest-check auth.
    expect(download.mock.calls[0]?.[1]).toEqual({ headers: {} });
    expect(install).not.toHaveBeenCalled();
    expect(staged.handle).toMatchObject({ version: '1.2.3', target: 'windows-x86_64', digest: 'abc', signature: 'sig', prepared: true });
    await expect(client.installStaged(staged.handle)).resolves.toMatchObject({ kind: 'ready' });
    expect(install).toHaveBeenCalledTimes(1);
    expect(download).toHaveBeenCalledTimes(1);
    expect(combined).not.toHaveBeenCalled();
  });

  it('legacy combined adapter handle does not claim a true prefetch', async () => {
    const client = createAutoUpdateClient({ check: async () => patch(), getTier: () => 'patch' });
    await client.checkOnce();
    const staged = await client.downloadAvailable();
    if (!staged.ok) throw new Error('expected a staged update');
    expect(staged.handle.prepared).toBe(false);
  });

  it('foreign and discarded handles are fenced without installing', async () => {
    const install = vi.fn(async () => {});
    const client = createAutoUpdateClient({ check: async () => patch(install), getTier: () => 'patch' });
    await client.checkOnce();
    const staged = await client.downloadAvailable();
    if (!staged.ok) throw new Error('expected a staged update');
    const foreign = { ...staged.handle };
    await expect(client.installStaged(foreign)).resolves.toMatchObject({ kind: 'error' });
    expect(client.discardStaged(staged.handle)).toBe(true);
    await expect(client.installStaged(staged.handle)).resolves.toMatchObject({ kind: 'error' });
    expect(install).not.toHaveBeenCalled();
  });

  it('concurrent prepares share one download', async () => {
    let finish!: () => void;
    const gate = new Promise<void>((resolve) => { finish = resolve; });
    const download = vi.fn(async () => gate);
    const update: RuntimeUpdate = { version: '1.2.3', rawJson: { tier: 'patch' }, downloadAndInstall: vi.fn(async () => {}), download, install: vi.fn(async () => {}) };
    const client = createAutoUpdateClient({ check: async () => update, getTier: () => 'patch' });
    await client.checkOnce();
    const both = Promise.all([client.downloadAvailable(), client.downloadAvailable()]);
    finish();
    await both;
    expect(download).toHaveBeenCalledTimes(1);
  });

  it('download with no eligible update returns a non-staged result', async () => {
    const client = createAutoUpdateClient({ check: async () => null });
    await client.checkOnce();
    await expect(client.downloadAvailable()).resolves.toMatchObject({ ok: false, status: { kind: 'up_to_date' } });
  });
});

describe('staged overlay state', () => {
  it('a staged update offers install and no download', () => {
    const model = deriveUpdateOverlay({ kind: 'staged', version: '2', tier: 'patch' });
    expect(model).toMatchObject({ phase: 'staged', version: '2' });
    expect(model.actions).toMatchObject({ download: false, install: true, dismiss: true });
  });
});

describe('consent runner binds each approval to its operation', () => {
  it('approving download never installs; approving install installs the staged handle', async () => {
    const install = vi.fn(async () => {});
    const client = createAutoUpdateClient({ check: async () => patch(install), getTier: () => 'patch' });
    await client.checkOnce();
    const run = createUpdateConsentRunner(client);
    const consent = createUpdateConsent();
    consent.request('download');
    await consent.allowOnce(run);
    expect(install).not.toHaveBeenCalled();
    expect(consent.state()).toMatchObject({ pending: null, busy: false, error: null });
    consent.request('install');
    await consent.allowOnce(run);
    expect(install).toHaveBeenCalledTimes(1);
    expect(consent.state()).toMatchObject({ pending: null, error: null });
  });

  it('a failed install surfaces its error and needs a fresh request', async () => {
    const client = createAutoUpdateClient({
      check: async () => ({ ...patch(), downloadAndInstall: async () => { throw new Error('bad signature'); } }),
      getTier: () => 'patch',
    });
    await client.checkOnce();
    const run = createUpdateConsentRunner(client);
    const consent = createUpdateConsent();
    consent.request('download');
    await consent.allowOnce(run);
    consent.request('install');
    await consent.allowOnce(run);
    expect(consent.state().error).toMatch(/bad signature/);
    expect(consent.state().pending).toBeNull();
  });

  it('download with no eligible update fails visibly instead of silently', async () => {
    const client = createAutoUpdateClient({ check: async () => null });
    await client.checkOnce();
    const run = createUpdateConsentRunner(client);
    await expect(run('download')).rejects.toThrow(/no eligible update/);
  });
});
