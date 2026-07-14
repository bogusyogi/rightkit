import { describe, expect, it } from 'vitest';
import { createRightKitTauriClient, standardRightKitTauriCommands } from './index.js';

describe('createRightKitTauriClient', () => {
  it('exposes the standard command surface shared by new Right Suite apps', () => {
    expect(standardRightKitTauriCommands).toEqual({
      license: {
        get: 'secure_update_license_get',
        set: 'secure_update_license_set',
        delete: 'secure_update_license_delete',
      },
      device: { get: 'secure_device_id_get', set: 'secure_device_id_set' },
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

  it('routes typed license, device, logs, and diagnostics operations through one command map', async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const invoke = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, args });
      if (command === 'license_read') return { key: 'RIGHT-123' } as T;
      if (command === 'device_read') return { id: 'device-1' } as T;
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
        device: { get: 'device_read', set: 'device_write' },
        logs: { write: 'logs_write', collect: 'logs_collect', clear: 'logs_clear' },
        diagnostics: { collect: 'diagnostics_collect', clear: 'diagnostics_clear' },
      },
    });

    await expect(client.license.get()).resolves.toEqual({ key: 'RIGHT-123' });
    await client.license.set({ key: 'RIGHT-456' });
    await client.license.delete();
    await expect(client.device.get()).resolves.toEqual({ id: 'device-1' });
    await client.device.set({ id: 'device-2' });
    await client.logs.write([{ event: 'opened' }]);
    await expect(client.logs.collect()).resolves.toEqual({ files: [] });
    await client.logs.clear();
    await client.diagnostics.collect();
    await client.diagnostics.clear();

    expect(calls).toEqual([
      { command: 'license_read', args: undefined },
      { command: 'license_write', args: { value: { key: 'RIGHT-456' } } },
      { command: 'license_delete', args: undefined },
      { command: 'device_read', args: undefined },
      { command: 'device_write', args: { value: { id: 'device-2' } } },
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
        device: { get: 'get-device', set: 'set-device' },
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
        device: { get: 'get-device', set: 'set-device' },
        logs: { write: 'write-logs', collect: 'collect-logs', clear: 'clear-logs' },
        diagnostics: { collect: 'collect-diagnostics', clear: 'clear-diagnostics' },
      },
      argumentNames: { licenseSet: 'license', deviceSet: 'id', logsWrite: 'batch' },
    });

    await client.license.set({ key: 'RIGHT-123' });
    await client.device.set({ id: 'device-1' });
    await client.logs.write(['opened']);

    expect(calls).toEqual([
      { command: 'set-license', args: { license: { key: 'RIGHT-123' } } },
      { command: 'set-device', args: { id: { id: 'device-1' } } },
      { command: 'write-logs', args: { batch: ['opened'] } },
    ]);
  });
});
