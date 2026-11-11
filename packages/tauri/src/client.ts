import type { AutoUpdateClient } from '@rightkit/app-update';
import {
  RIGHTKIT_LOGS_CLEAR_COMMAND,
  RIGHTKIT_LOGS_COLLECT_COMMAND,
  RIGHTKIT_LOGS_WRITE_COMMAND,
} from '@rightkit/logs';

export type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export interface RightKitTauriCommands {
  logs: { write: string; collect: string; clear: string };
  diagnostics: { collect: string; clear: string };
}

export const standardRightKitTauriCommands = {
  logs: {
    write: RIGHTKIT_LOGS_WRITE_COMMAND,
    collect: RIGHTKIT_LOGS_COLLECT_COMMAND,
    clear: RIGHTKIT_LOGS_CLEAR_COMMAND,
  },
  diagnostics: {
    collect: 'collect_diagnostics',
    clear: 'clear_diagnostic_logs',
  },
} as const satisfies RightKitTauriCommands;

/**
 * Command map for a host that registers only the `rightkit_logs_*` commands and answers
 * diagnostics from the same log store: `diagnostics.collect/clear` call `rightkit_logs_collect/clear`.
 */
export const logsOnlyRightKitTauriCommands = {
  logs: standardRightKitTauriCommands.logs,
  diagnostics: { collect: RIGHTKIT_LOGS_COLLECT_COMMAND, clear: RIGHTKIT_LOGS_CLEAR_COMMAND },
} as const satisfies RightKitTauriCommands;

/** The only supported native device-identity command contract. */
export const standardRightKitDeviceGetOrCreate = {
  command: 'secure_device_id_get_or_create',
} as const;

export interface RightKitTauriClientConfig {
  invoke: Invoke;
  commands?: RightKitTauriCommands;
  deviceGetOrCreate?: typeof standardRightKitDeviceGetOrCreate;
  updates?: AutoUpdateClient;
  argumentNames?: {
    logsWrite?: string;
  };
}

/** The per-call half of the config: the webview `invoke` and an optional update client. */
export type RightKitTauriAdapterConfig = Pick<RightKitTauriClientConfig, 'invoke' | 'updates'>;
/** The per-app half of the config: the host's command names. An app keeps its own value of this type. */
export type RightKitTauriAppConfig = Pick<
  RightKitTauriClientConfig,
  'commands' | 'deviceGetOrCreate' | 'argumentNames'
>;

export interface RightKitTauriClient<TDevice, TLogBundle, TLogEvent, TDiagnostics> {
  device: {
    getOrCreate(): Promise<TDevice>;
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

/**
 * The one constructor. The app passes its own `invoke` and, as configuration, the command names its
 * native host registers: `commands` (default `standardRightKitTauriCommands`), `deviceGetOrCreate`
 * (no default: without it `device.getOrCreate()` rejects) and `argumentNames.logsWrite` (default `events`).
 *
 * ```ts
 * const native = createRightKitTauriClient({
 *   invoke,
 *   deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
 * });
 * ```
 */
export function createRightKitTauriClient<
  TDevice = unknown,
  TLogBundle = unknown,
  TLogEvent = Record<string, unknown>,
  TDiagnostics = TLogBundle,
>(config: RightKitTauriClientConfig): RightKitTauriClient<
  TDevice,
  TLogBundle,
  TLogEvent,
  TDiagnostics
> {
  const commands = config.commands ?? standardRightKitTauriCommands;
  validateCommands(commands);
  const { invoke } = config;
  const argumentNames = {
    logsWrite: config.argumentNames?.logsWrite ?? 'events',
  };
  validateArgumentNames(argumentNames);
  if (config.deviceGetOrCreate) {
    validateCommand('device.getOrCreate', config.deviceGetOrCreate.command);
  }
  return {
    device: {
      getOrCreate: async () => {
        if (config.deviceGetOrCreate) {
          return invoke<TDevice>(config.deviceGetOrCreate.command);
        }
        throw new Error(
          'RightKit device getOrCreate is unsupported until the host exposes one atomic native command',
        );
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
