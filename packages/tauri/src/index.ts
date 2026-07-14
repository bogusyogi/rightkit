import type { AutoUpdateClient } from '@rightkit/updates';

export type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export interface RightKitTauriCommands {
  license: { get: string; set: string; delete: string };
  logs: { write: string; collect: string; clear: string };
  diagnostics: { collect: string; clear: string };
}

export const standardRightKitTauriCommands = {
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
    licenseSet?: string;
    logsWrite?: string;
  };
}

export type RightKitTauriAdapterConfig = Pick<RightKitTauriClientConfig, 'invoke' | 'updates'>;
export type RightKitTauriAppConfig = Pick<
  RightKitTauriClientConfig,
  'commands' | 'deviceGetOrCreate' | 'argumentNames'
>;

export interface RightKitTauriClient<TLicense, TDevice, TLogBundle, TLogEvent, TDiagnostics> {
  license: {
    get(): Promise<TLicense | null>;
    set(value: TLicense): Promise<void>;
    delete(): Promise<void>;
  };
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
    logsWrite: config.argumentNames?.logsWrite ?? 'events',
  };
  validateArgumentNames(argumentNames);
  if (config.deviceGetOrCreate) {
    validateCommand('device.getOrCreate', config.deviceGetOrCreate.command);
  }
  return {
    license: {
      get: () => invoke<TLicense | null>(commands.license.get),
      set: (value) => invoke<void>(commands.license.set, { [argumentNames.licenseSet]: value }),
      delete: () => invoke<void>(commands.license.delete),
    },
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
    ['license.get', commands.license.get],
    ['license.set', commands.license.set],
    ['license.delete', commands.license.delete],
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

const heardRightCommands = {
  license: {
    get: 'secure_license_get',
    set: 'secure_license_set',
    delete: 'secure_license_delete',
  },
  logs: {
    write: 'rightkit_logs_write',
    collect: 'rightkit_logs_collect',
    clear: 'rightkit_logs_clear',
  },
  diagnostics: { collect: 'rightkit_logs_collect', clear: 'rightkit_logs_clear' },
} as const satisfies RightKitTauriCommands;

const codeRightCommands = {
  ...standardRightKitTauriCommands,
  diagnostics: { collect: 'rightkit_logs_collect', clear: 'rightkit_logs_clear' },
} as const satisfies RightKitTauriCommands;

/** Canonical inventory of the native command contracts currently exposed by each core app. */
export const rightSuiteTauriAppConfigs = {
  viewright: {
    commands: standardRightKitTauriCommands,
    argumentNames: { licenseSet: 'value', logsWrite: 'events' },
  },
  scraperight: {
    commands: standardRightKitTauriCommands,
    argumentNames: { licenseSet: 'license', logsWrite: 'events' },
  },
  heardright: {
    commands: heardRightCommands,
    argumentNames: { licenseSet: 'license', logsWrite: 'events' },
  },
  mailright: {
    commands: standardRightKitTauriCommands,
    argumentNames: { licenseSet: 'license', logsWrite: 'events' },
  },
  coderight: {
    commands: codeRightCommands,
    argumentNames: { licenseSet: 'value', logsWrite: 'events' },
  },
} as const satisfies Record<string, RightKitTauriAppConfig>;

type CoreApp = keyof typeof rightSuiteTauriAppConfigs;

export function createRightSuiteAppTauriClient<
  TLicense = unknown,
  TDevice = unknown,
  TLogBundle = unknown,
  TLogEvent = Record<string, unknown>,
  TDiagnostics = TLogBundle,
>(
  app: CoreApp,
  config: RightKitTauriAdapterConfig,
): RightKitTauriClient<TLicense, TDevice, TLogBundle, TLogEvent, TDiagnostics> {
  return createRightKitTauriClient<TLicense, TDevice, TLogBundle, TLogEvent, TDiagnostics>({
    ...config,
    ...rightSuiteTauriAppConfigs[app],
  });
}

export const createViewRightTauriClient = createAppConstructor('viewright');
export const createScrapeRightTauriClient = createAppConstructor('scraperight');
export const createHeardRightTauriClient = createAppConstructor('heardright');
export const createMailRightTauriClient = createAppConstructor('mailright');
export const createCodeRightTauriClient = createAppConstructor('coderight');

function createAppConstructor(app: CoreApp) {
  return function createAppTauriClient<
    TLicense = unknown,
    TDevice = unknown,
    TLogBundle = unknown,
    TLogEvent = Record<string, unknown>,
    TDiagnostics = TLogBundle,
  >(
    config: RightKitTauriAdapterConfig,
  ): RightKitTauriClient<TLicense, TDevice, TLogBundle, TLogEvent, TDiagnostics> {
    return createRightSuiteAppTauriClient<
      TLicense,
      TDevice,
      TLogBundle,
      TLogEvent,
      TDiagnostics
    >(app, config);
  };
}

function validateCommand(path: string, command: string): void {
  if (command.trim().length === 0) throw new Error(`RightKit Tauri command ${path} is required`);
}
