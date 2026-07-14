import { describe, expect, it } from 'vitest';
import {
  createCodeRightTauriClient,
  createHeardRightTauriClient,
  createMailRightTauriClient,
  createRightKitTauriClient,
  createScrapeRightTauriClient,
  createViewRightTauriClient,
  rightSuiteTauriAppConfigs,
  standardRightKitDeviceGetOrCreate,
  standardRightKitTauriCommands,
} from './index.js';

describe('createRightKitTauriClient', () => {
  it('exposes the standard command surface shared by new Right Suite apps', () => {
    expect(standardRightKitTauriCommands).toEqual({
      license: {
        get: 'secure_update_license_get',
        set: 'secure_update_license_set',
        delete: 'secure_update_license_delete',
      },
      logs: {
        write: 'rightkit_logs_write',
        collect: 'rightkit_logs_collect',
        clear: 'rightkit_logs_clear',
      },
      diagnostics: {
        collect: 'collect_diagnostics',
        clear: 'clear_diagnostic_logs',
      },
    });
    expect(standardRightKitDeviceGetOrCreate).toEqual({
      command: 'secure_device_id_get_or_create',
    });
  });

  it('uses the standard command map when an app has no legacy command names', async () => {
    const calls: string[] = [];
    const client = createRightKitTauriClient({
      invoke: async <T>(command: string) => {
        calls.push(command);
        return undefined as T;
      },
    });

    await client.license.get();
    await client.logs.collect();

    expect(calls).toEqual(['secure_update_license_get', 'rightkit_logs_collect']);
  });

  it('routes typed license, logs, and diagnostics operations through one command map', async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const invoke = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, args });
      if (command === 'license_read') return { key: 'RIGHT-123' } as T;
      if (command === 'logs_collect') return { files: [] } as T;
      return undefined as T;
    };
    const client = createRightKitTauriClient<
      { key: string },
      { id: string },
      { files: unknown[] },
      { event: string }
    >({
      invoke,
      commands: {
        license: { get: 'license_read', set: 'license_write', delete: 'license_delete' },
        logs: { write: 'logs_write', collect: 'logs_collect', clear: 'logs_clear' },
        diagnostics: { collect: 'diagnostics_collect', clear: 'diagnostics_clear' },
      },
    });

    await expect(client.license.get()).resolves.toEqual({ key: 'RIGHT-123' });
    await client.license.set({ key: 'RIGHT-456' });
    await client.license.delete();
    await client.logs.write([{ event: 'opened' }]);
    await expect(client.logs.collect()).resolves.toEqual({ files: [] });
    await client.logs.clear();
    await client.diagnostics.collect();
    await client.diagnostics.clear();

    expect(calls).toEqual([
      { command: 'license_read', args: undefined },
      { command: 'license_write', args: { value: { key: 'RIGHT-456' } } },
      { command: 'license_delete', args: undefined },
      { command: 'logs_write', args: { events: [{ event: 'opened' }] } },
      { command: 'logs_collect', args: undefined },
      { command: 'logs_clear', args: undefined },
      { command: 'diagnostics_collect', args: undefined },
      { command: 'diagnostics_clear', args: undefined },
    ]);
  });

  it('fails closed when a required command name is empty', () => {
    expect(() => createRightKitTauriClient({
      invoke: async () => undefined,
      commands: {
        license: { get: '', set: 'set', delete: 'delete' },
        logs: { write: 'write', collect: 'collect', clear: 'clear' },
        diagnostics: { collect: 'collect-diagnostics', clear: 'clear-diagnostics' },
      },
    })).toThrow(/license\.get/);
  });

  it('supports app-specific Tauri argument names without duplicating the client', async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const client = createRightKitTauriClient<{ key: string }, { id: string }, unknown, string>({
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        return undefined as T;
      },
      commands: {
        license: { get: 'get-license', set: 'set-license', delete: 'delete-license' },
        logs: { write: 'write-logs', collect: 'collect-logs', clear: 'clear-logs' },
        diagnostics: { collect: 'collect-diagnostics', clear: 'clear-diagnostics' },
      },
      argumentNames: { licenseSet: 'license', logsWrite: 'batch' },
    });

    await client.license.set({ key: 'RIGHT-123' });
    await client.logs.write(['opened']);

    expect(calls).toEqual([
      { command: 'set-license', args: { license: { key: 'RIGHT-123' } } },
      { command: 'write-logs', args: { batch: ['opened'] } },
    ]);
  });

  it('rejects renderer-side device get/set because it is not atomic', async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const client = createRightKitTauriClient<unknown, string>({
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        return undefined as T;
      },
    });

    await expect(client.device.getOrCreate()).rejects.toThrow(/atomic native/i);
    expect(calls).toEqual([]);
  });

  it('uses a no-argument atomic command so the native host owns device creation', async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const client = createRightKitTauriClient<unknown, string>({
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        return 'device-from-keyring' as T;
      },
      deviceGetOrCreate: { command: 'secure_device_id_get_or_create' },
    });

    await expect(client.device.getOrCreate()).resolves.toBe('device-from-keyring');
    expect(calls).toEqual([
      { command: 'secure_device_id_get_or_create', args: undefined },
    ]);
  });

  it('composes the shared updater client without creating a second policy runtime', async () => {
    const updates = {
      checkOnce: async () => ({ kind: 'up_to_date' as const }),
      installAvailable: async () => ({ kind: 'up_to_date' as const }),
    };
    const client = createRightKitTauriClient({ invoke: async () => undefined, updates });

    await expect(client.updates?.checkOnce()).resolves.toEqual({ kind: 'up_to_date' });
    expect(client.updates).toBe(updates);
  });

  it('exports the five real app command configurations', () => {
    expect(rightSuiteTauriAppConfigs).toEqual({
      viewright: {
        commands: standardRightKitTauriCommands,
        deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
        argumentNames: { licenseSet: 'value', logsWrite: 'events' },
      },
      scraperight: {
        commands: standardRightKitTauriCommands,
        deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
        argumentNames: { licenseSet: 'license', logsWrite: 'events' },
      },
      heardright: {
        commands: {
          license: { get: 'secure_license_get', set: 'secure_license_set', delete: 'secure_license_delete' },
          logs: {
            write: 'rightkit_logs_write',
            collect: 'rightkit_logs_collect',
            clear: 'rightkit_logs_clear',
          },
          diagnostics: { collect: 'rightkit_logs_collect', clear: 'rightkit_logs_clear' },
        },
        deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
        argumentNames: { licenseSet: 'license', logsWrite: 'events' },
      },
      mailright: {
        commands: standardRightKitTauriCommands,
        deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
        argumentNames: { licenseSet: 'license', logsWrite: 'events' },
      },
      coderight: {
        commands: {
          ...standardRightKitTauriCommands,
          diagnostics: { collect: 'rightkit_logs_collect', clear: 'rightkit_logs_clear' },
        },
        deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
        argumentNames: { licenseSet: 'value', logsWrite: 'events' },
      },
    });
  });

  it.each([
    ['viewright', createViewRightTauriClient, 'value', 'collect_diagnostics', 'clear_diagnostic_logs'],
    ['scraperight', createScrapeRightTauriClient, 'license', 'collect_diagnostics', 'clear_diagnostic_logs'],
    ['mailright', createMailRightTauriClient, 'license', 'collect_diagnostics', 'clear_diagnostic_logs'],
    ['coderight', createCodeRightTauriClient, 'value', 'rightkit_logs_collect', 'rightkit_logs_clear'],
  ] as const)('executes the %s shared command fixture with atomic native device identity', async (
    _app,
    createClient,
    licenseArgument,
    diagnosticsCollect,
    diagnosticsClear,
  ) => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const client = createClient<{ key: string }, string, unknown, string, unknown>({
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        return undefined as T;
      },
    });

    await client.license.get();
    await client.license.set({ key: 'RIGHT-123' });
    await client.license.delete();
    await client.logs.write(['opened']);
    await client.logs.collect();
    await client.logs.clear();
    await client.diagnostics.collect();
    await client.diagnostics.clear();
    await client.device.getOrCreate();

    expect(calls).toEqual([
      { command: 'secure_update_license_get', args: undefined },
      { command: 'secure_update_license_set', args: { [licenseArgument]: { key: 'RIGHT-123' } } },
      { command: 'secure_update_license_delete', args: undefined },
      { command: 'rightkit_logs_write', args: { events: ['opened'] } },
      { command: 'rightkit_logs_collect', args: undefined },
      { command: 'rightkit_logs_clear', args: undefined },
      { command: diagnosticsCollect, args: undefined },
      { command: diagnosticsClear, args: undefined },
      { command: 'secure_device_id_get_or_create', args: undefined },
    ]);
  });

  it('executes the HeardRight fixture with atomic native device identity', async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const client = createHeardRightTauriClient<{ key: string }, string, unknown, string, unknown>({
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        return undefined as T;
      },
    });

    await client.license.get();
    await client.license.set({ key: 'RIGHT-123' });
    await client.license.delete();
    await client.device.getOrCreate();
    await client.logs.write(['opened']);
    await client.logs.collect();
    await client.logs.clear();
    await client.diagnostics.collect();
    await client.diagnostics.clear();

    expect(calls).toEqual([
      { command: 'secure_license_get', args: undefined },
      { command: 'secure_license_set', args: { license: { key: 'RIGHT-123' } } },
      { command: 'secure_license_delete', args: undefined },
      { command: 'secure_device_id_get_or_create', args: undefined },
      { command: 'rightkit_logs_write', args: { events: ['opened'] } },
      { command: 'rightkit_logs_collect', args: undefined },
      { command: 'rightkit_logs_clear', args: undefined },
      { command: 'rightkit_logs_collect', args: undefined },
      { command: 'rightkit_logs_clear', args: undefined },
    ]);
  });
});
