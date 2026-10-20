import type { ChildProcess } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, describe, expect, it, vi } from 'vitest';

// Every spawnSync is recorded; the default answer is an empty, successful scan. Nothing real is started.
const spawned = vi.hoisted(() => ({
  calls: [] as unknown[][],
  response: undefined as { status: number | null; stdout: unknown } | undefined,
}));
vi.mock('node:child_process', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:child_process')>();
  return {
    ...actual,
    spawnSync: (...args: unknown[]) => {
      spawned.calls.push(args);
      return { status: 0, stdout: 'RK_QA_SCAN_COMPLETE\n', stderr: '', pid: 0, output: [], signal: null, ...spawned.response };
    },
  };
});

import { closeChromeGracefully, killVerifiedProcesses, probeProcessIdentities, probeProfileProcesses } from './index.js';

const scratch = mkdtempSync(path.join(tmpdir(), 'rk-qa-spawn-guard-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

describe('no spawn without a positive whole-millisecond timeout (spawnSync reads 0 as no timeout)', () => {
  it('scan, identity probe and kill never spawn at 0, a fraction of a millisecond, a negative or a NaN time', () => {
    spawned.calls.length = 0;
    for (const ms of [0, 0.4, -5, Number.NaN]) {
      expect(probeProfileProcesses(path.join(scratch, 'p'), ms)).toBeNull();
      expect(probeProcessIdentities([{ pid: 5, cimFileTime: '1' }], ms)).toBeNull();
      killVerifiedProcesses([{ pid: 5, creation: '1' }], ms);
    }
    expect(spawned.calls).toEqual([]);
  });

  it('a positive time is spawned with a whole-millisecond timeout, never 0', () => {
    spawned.calls.length = 0;
    probeProfileProcesses(path.join(scratch, 'p'), 1.6);
    expect(spawned.calls).toHaveLength(1);
    const options = spawned.calls[0][2] as { timeout: number };
    expect(options.timeout).toBe(1);
  });

  if (process.platform === 'win32') it('Windows scan & kill declare native imports in memory without a compiler child', () => {
    spawned.calls.length = 0;
    probeProfileProcesses(path.join(scratch, 'p'), 1_000);
    killVerifiedProcesses([{ pid: 5, creation: '123' }], 1_000);
    expect(spawned.calls).toHaveLength(2);
    for (const [, args, options] of spawned.calls) {
      const script = (args as string[]).at(-1)!;
      expect(script).toContain('DefineDynamicAssembly');
      expect(script).not.toContain('Add-Type');
      expect(script).not.toContain('RK_QA_CS');
      expect(options).toMatchObject({ windowsHide: true, timeout: 1_000 });
    }
  });

  if (process.platform === 'win32') it('bootstraps system modules in order only for profile scans', () => {
    spawned.calls.length = 0;
    expect(probeProfileProcesses(path.join(scratch, 'p'), 30_000)).toEqual([]);
    probeProcessIdentities([{ pid: 5, cimFileTime: '123' }], 1_000);
    killVerifiedProcesses([{ pid: 5, creation: '123' }], 1_000);
    expect(spawned.calls).toHaveLength(3);
    const scripts = spawned.calls.map(([, args]) => (args as string[]).at(-1)!);
    const utility = 'Import-Module "$PSHOME\\Modules\\Microsoft.PowerShell.Utility\\Microsoft.PowerShell.Utility.psd1"';
    const cim = 'Import-Module "$PSHOME\\Modules\\CimCmdlets\\CimCmdlets.psd1"';
    const disableAutoload = "$PSModuleAutoLoadingPreference = 'None'";
    const scan = scripts[0];
    expect(scan).toContain(`${disableAutoload}; ${utility}; ${cim}; $p = $env:RK_QA_PROFILE;`);
    expect(scan.indexOf("$ErrorActionPreference = 'Stop'")).toBeLessThan(scan.indexOf(disableAutoload));
    expect(scan.indexOf(disableAutoload)).toBeLessThan(scan.indexOf('Get-CimInstance Win32_Process |'));
    expect(scan).toContain("}; 'RK_QA_SCAN_COMPLETE'");
    expect(spawned.calls[0][2]).toMatchObject({ timeout: 15_000, windowsHide: true });
    for (const script of scripts.slice(1)) {
      expect(script).not.toContain('Import-Module');
      expect(script).not.toContain('$PSModuleAutoLoadingPreference');
    }
  });

  if (process.platform === 'win32') it('keeps failed, timed-out, incomplete & unknown profile scans distinct from clean scans', () => {
    try {
      for (const response of [
        { status: 1, stdout: 'RK_QA_SCAN_COMPLETE\n' },
        { status: null, stdout: 'RK_QA_SCAN_COMPLETE\n' },
        { status: 0, stdout: undefined },
        { status: 0, stdout: '123|638000000000000001\n' },
      ]) {
        spawned.response = response;
        expect(probeProfileProcesses(path.join(scratch, 'p'), 1_000)).toBeNull();
      }
      spawned.response = { status: 0, stdout: '123|638000000000000001\nUNKNOWN|456\nRK_QA_SCAN_COMPLETE\n' };
      expect(probeProfileProcesses(path.join(scratch, 'p'), 1_000)).toEqual([
        { pid: 123, creation: '638000000000000001' },
        { pid: 456, unknown: true },
      ]);
    } finally {
      spawned.response = undefined;
    }
  });

  it('a close with no budget left spawns nothing at all and reports unknown', async () => {
    let t = 0;
    for (const budgetMs of [0, 0.4]) {
      spawned.calls.length = 0;
      const dir = mkdtempSync(path.join(scratch, 'profile-'));
      const report = await closeChromeGracefully({
        child: { pid: 4242, exitCode: 1, signalCode: null } as unknown as ChildProcess,
        profileDir: dir,
        budgetMs,
        seams: { platform: 'win32', now: () => t, sleep: async (ms) => { t += ms; } },
      });
      expect(spawned.calls).toEqual([]);
      expect(report).toMatchObject({ exited: false, livenessUnknown: true });
    }
  });

  it('a live launcher & CDP endpoint with only fractional time create no WebSocket or process', async () => {
    const websocket = vi.fn();
    vi.stubGlobal('WebSocket', websocket);
    try {
      for (const platform of ['win32', 'linux'] as const) {
        spawned.calls.length = 0;
        let t = 0;
        const signal = vi.fn();
        const report = await closeChromeGracefully({
          child: { pid: 4242, exitCode: null, signalCode: null } as unknown as ChildProcess,
          wsEndpoint: 'ws://127.0.0.1:9222/devtools/browser/test',
          budgetMs: 0.4,
          seams: {
            platform,
            launcherLiveness: () => 'alive',
            signalTree: signal,
            now: () => t,
            sleep: async (ms) => { t += ms; },
          },
        });
        expect(websocket).not.toHaveBeenCalled();
        expect(signal).not.toHaveBeenCalled();
        expect(spawned.calls).toEqual([]);
        expect(report).toMatchObject({ step: 'not-attempted', exited: false, livenessUnknown: true });
      }
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
