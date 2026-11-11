export {
  TauriCommandError,
  declaredTauriCommandNames,
  defineTauriCommands,
  missingTauriHandlers,
  tauriHandlerCommandNames,
} from './commands.js';
export {
  NATIVE_FETCH_MESSAGES,
  NativeFetchError,
  RIGHTKIT_FETCH_COMMAND,
  createNativeFetch,
} from './fetch.js';
export type { NativeFetchErrorCode, NativeFetchOptions } from './fetch.js';
export type {
  DefineTauriCommandsConfig,
  TauriCommandClient,
  TauriCommandEntry,
  TauriCommandFn,
  TauriCommandMap,
  TauriCommandSpec,
} from './commands.js';

export {
  createRightKitTauriClient,
  logsOnlyRightKitTauriCommands,
  standardRightKitDeviceGetOrCreate,
  standardRightKitTauriCommands,
} from './client.js';
export type {
  Invoke,
  RightKitTauriAdapterConfig,
  RightKitTauriAppConfig,
  RightKitTauriClient,
  RightKitTauriClientConfig,
  RightKitTauriCommands,
} from './client.js';

// Deprecated in 0.4.0, removed in 0.5.0: per-app constructors and the per-app config table.
export {
  createCodeRightTauriClient,
  createHeardRightTauriClient,
  createMailRightTauriClient,
  createRightSuiteAppTauriClient,
  createScrapeRightTauriClient,
  createViewRightTauriClient,
  rightSuiteTauriAppConfigs,
} from './deprecated-app-clients.js';
