import type { AutoUpdateClient } from '@rightkit/updates';

export type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export interface RightKitTauriCommands {
  license: { get: string; set: string; delete: string };
  device: { get: string; set: string };
  logs: { write: string; collect: string; clear: string };
  diagnostics: { collect: string; clear: string };
}

export const standardRightKitTauriCommands = {
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
} as const satisfies RightKitTauriCommands;

export interface RightKitTauriClientConfig {
  invoke: Invoke;
  commands?: RightKitTauriCommands;
  deviceGetOrCreate?: { command: string; argumentName?: string };
  updates?: AutoUpdateClient;
  argumentNames?: {
    licenseSet?: string;
    deviceSet?: string;
    logsWrite?: string;
  };
}

export interface RightKitTauriClient<TLicense, TDevice, TLogBundle, TLogEvent, TDiagnostics> {
  license: {
    get(): Promise<TLicense | null>;
    set(value: TLicense): Promise<void>;
    delete(): Promise<void>;
  };
  device: {
    getOrCreate(fallback: TDevice): Promise<TDevice>;
  };
  logs: {
    write(events: TLogEvent[]): Promise<void>;
    collect(): Promise<TLogBundle>;
    clear(): Promise<void>;
  };
  diagnostics: {
    collect(): Promise<TDiagnostics>;
    clear(): Promise<void>;
  };
  updates?: AutoUpdateClient;
}

export function createRightKitTauriClient<
  TLicense = unknown,
  TDevice = unknown,
  TLogBundle = unknown,
  TLogEvent = Record<string, unknown>,
  TDiagnostics = TLogBundle,
>(config: RightKitTauriClientConfig): RightKitTauriClient<
  TLicense,
  TDevice,
  TLogBundle,
  TLogEvent,
  TDiagnostics
> {
  const commands = config.commands ?? standardRightKitTauriCommands;
  validateCommands(commands);
  const { invoke } = config;
  const argumentNames = {
    licenseSet: config.argumentNames?.licenseSet ?? 'value',
    deviceSet: config.argumentNames?.deviceSet ?? 'value',
    logsWrite: config.argumentNames?.logsWrite ?? 'events',
  };
  validateArgumentNames(argumentNames);
  if (config.deviceGetOrCreate) {
    validateCommand('device.getOrCreate', config.deviceGetOrCreate.command);
    validateArgumentNames({
      deviceGetOrCreate: config.deviceGetOrCreate.argumentName ?? 'fallback',
    });
  }
  return {
    license: {
      get: () => invoke<TLicense | null>(commands.license.get),
      set: (value) => invoke<void>(commands.license.set, { [argumentNames.licenseSet]: value }),
      delete: () => invoke<void>(commands.license.delete),
    },
    device: {
      getOrCreate: async (fallback) => {
        if (config.deviceGetOrCreate) {
          const argumentName = config.deviceGetOrCreate.argumentName ?? 'fallback';
          return invoke<TDevice>(config.deviceGetOrCreate.command, { [argumentName]: fallback });
        }
        const current = await invoke<TDevice | null>(commands.device.get);
        if (current !== null && current !== undefined) return current;
        await invoke<void>(commands.device.set, { [argumentNames.deviceSet]: fallback });
        return fallback;
      },
    },
    logs: {
      write: (events) => invoke<void>(commands.logs.write, { [argumentNames.logsWrite]: events }),
      collect: () => invoke<TLogBundle>(commands.logs.collect),
      clear: () => invoke<void>(commands.logs.clear),
    },
    diagnostics: {
      collect: () => invoke<TDiagnostics>(commands.diagnostics.collect),
      clear: () => invoke<void>(commands.diagnostics.clear),
    },
    updates: config.updates,
  };
}

function validateArgumentNames(argumentNames: Record<string, string>): void {
  for (const [path, name] of Object.entries(argumentNames)) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
      throw new Error(`RightKit Tauri argument name ${path} is invalid`);
    }
  }
}

function validateCommands(commands: RightKitTauriCommands): void {
  const entries: Array<[string, string]> = [
    ['license.get', commands.license.get],
    ['license.set', commands.license.set],
    ['license.delete', commands.license.delete],
    ['device.get', commands.device.get],
    ['device.set', commands.device.set],
    ['logs.write', commands.logs.write],
    ['logs.collect', commands.logs.collect],
    ['logs.clear', commands.logs.clear],
    ['diagnostics.collect', commands.diagnostics.collect],
    ['diagnostics.clear', commands.diagnostics.clear],
  ];
  for (const [path, command] of entries) {
    validateCommand(path, command);
  }
}

function validateCommand(path: string, command: string): void {
  if (command.trim().length === 0) throw new Error(`RightKit Tauri command ${path} is required`);
}
