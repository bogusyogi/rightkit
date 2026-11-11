// Deprecated in 0.4.0 and removed in 0.5.0 (shape review 2026-11-11, family-foundation-shell-ui §6):
// a shared package must not name the apps that use it. Each app keeps its own `RightKitTauriAppConfig`
// value and calls `createRightKitTauriClient({ invoke, ...appConfig })`. Everything below calls that
// constructor with the values each app had in 0.3.x, so behaviour is unchanged until removal.
import {
  createRightKitTauriClient,
  logsOnlyRightKitTauriCommands,
  standardRightKitDeviceGetOrCreate,
  standardRightKitTauriCommands,
  type RightKitTauriAdapterConfig,
  type RightKitTauriAppConfig,
  type RightKitTauriClient,
} from './client.js';

const standardAppConfig = {
  commands: standardRightKitTauriCommands,
  deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
  argumentNames: { logsWrite: 'events' },
} as const satisfies RightKitTauriAppConfig;

const logsOnlyAppConfig = {
  commands: logsOnlyRightKitTauriCommands,
  deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
  argumentNames: { logsWrite: 'events' },
} as const satisfies RightKitTauriAppConfig;

/**
 * @deprecated since 0.4.0, removed in 0.5.0. Keep the app's own `RightKitTauriAppConfig` value in the app
 * and pass it to `createRightKitTauriClient`. The 0.3.x values: every entry uses
 * `standardRightKitDeviceGetOrCreate` and `argumentNames.logsWrite: 'events'`; `commands` is
 * `logsOnlyRightKitTauriCommands` for heardright and coderight, `standardRightKitTauriCommands` for the rest.
 */
export const rightSuiteTauriAppConfigs = {
  viewright: standardAppConfig,
  scraperight: standardAppConfig,
  heardright: logsOnlyAppConfig,
  mailright: standardAppConfig,
  coderight: logsOnlyAppConfig,
} as const satisfies Record<string, RightKitTauriAppConfig>;

type DeprecatedApp = keyof typeof rightSuiteTauriAppConfigs;

/**
 * @deprecated since 0.4.0, removed in 0.5.0. Use
 * `createRightKitTauriClient({ ...config, ...appConfig })` with the app's own `RightKitTauriAppConfig`.
 */
export function createRightSuiteAppTauriClient<
  TDevice = unknown,
  TLogBundle = unknown,
  TLogEvent = Record<string, unknown>,
  TDiagnostics = TLogBundle,
>(
  app: DeprecatedApp,
  config: RightKitTauriAdapterConfig,
): RightKitTauriClient<TDevice, TLogBundle, TLogEvent, TDiagnostics> {
  return createRightKitTauriClient<TDevice, TLogBundle, TLogEvent, TDiagnostics>({
    ...config,
    ...rightSuiteTauriAppConfigs[app],
  });
}

function appConstructor(app: DeprecatedApp) {
  return function createAppTauriClient<
    TDevice = unknown,
    TLogBundle = unknown,
    TLogEvent = Record<string, unknown>,
    TDiagnostics = TLogBundle,
  >(
    config: RightKitTauriAdapterConfig,
  ): RightKitTauriClient<TDevice, TLogBundle, TLogEvent, TDiagnostics> {
    return createRightKitTauriClient<TDevice, TLogBundle, TLogEvent, TDiagnostics>({
      ...config,
      ...rightSuiteTauriAppConfigs[app],
    });
  };
}

/**
 * @deprecated since 0.4.0, removed in 0.5.0. Use `createRightKitTauriClient({ invoke,
 * deviceGetOrCreate: standardRightKitDeviceGetOrCreate })`.
 */
export const createViewRightTauriClient = appConstructor('viewright');
/**
 * @deprecated since 0.4.0, removed in 0.5.0. Use `createRightKitTauriClient({ invoke,
 * deviceGetOrCreate: standardRightKitDeviceGetOrCreate })`.
 */
export const createScrapeRightTauriClient = appConstructor('scraperight');
/**
 * @deprecated since 0.4.0, removed in 0.5.0. Use `createRightKitTauriClient({ invoke,
 * commands: logsOnlyRightKitTauriCommands, deviceGetOrCreate: standardRightKitDeviceGetOrCreate })`.
 */
export const createHeardRightTauriClient = appConstructor('heardright');
/**
 * @deprecated since 0.4.0, removed in 0.5.0. Use `createRightKitTauriClient({ invoke,
 * deviceGetOrCreate: standardRightKitDeviceGetOrCreate })`.
 */
export const createMailRightTauriClient = appConstructor('mailright');
/**
 * @deprecated since 0.4.0, removed in 0.5.0. Use `createRightKitTauriClient({ invoke,
 * commands: logsOnlyRightKitTauriCommands, deviceGetOrCreate: standardRightKitDeviceGetOrCreate })`.
 */
export const createCodeRightTauriClient = appConstructor('coderight');
