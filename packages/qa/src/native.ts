import path from 'node:path';

export interface RightQaConfig {
  app: string;
  appRoot: string;
  windowLabel: string;
  specs: string[];
  binaries: {
    win32: string;
    darwin: string;
  };
  evidenceDir?: string;
}

export interface NativeQaRuntimeOptions {
  platform?: 'win32' | 'darwin';
  embeddedPort?: number;
}

export interface WdioNativeConfig {
  runner: 'local';
  specs: string[];
  maxInstances: 1;
  maxInstancesPerCapability: 1;
  services: Array<[string, Record<string, unknown>]>;
  capabilities: Array<Record<string, unknown>>;
  framework: 'mocha';
  reporters: string[];
  mochaOpts: { ui: 'bdd'; timeout: number };
  logLevel: 'error';
  bail: number;
  waitforTimeout: number;
  connectionRetryTimeout: number;
  connectionRetryCount: number;
  outputDir: string;
}

export function defineRightQaConfig(input: RightQaConfig): RightQaConfig {
  const app = requireText(input.app, 'app');
  if (!/^[a-z][a-z0-9-]*$/.test(app)) throw new Error('app must be a lowercase app key');
  const appRoot = path.resolve(requireText(input.appRoot, 'appRoot'));
  const specs = input.specs.map((spec) => requireText(spec, 'spec'));
  if (specs.length === 0) throw new Error('at least one native QA spec is required');
  return Object.freeze({
    ...input,
    app,
    appRoot,
    windowLabel: requireText(input.windowLabel, 'windowLabel'),
    specs: Object.freeze([...specs]) as unknown as string[],
    binaries: Object.freeze({
      win32: requireText(input.binaries.win32, 'win32 binary'),
      darwin: requireText(input.binaries.darwin, 'darwin binary'),
    }),
  });
}

export function createWdioConfig(
  rawConfig: RightQaConfig,
  options: NativeQaRuntimeOptions = {},
): WdioNativeConfig {
  const app = defineRightQaConfig(rawConfig);
  const platform = options.platform ?? currentDesktopPlatform();
  const embeddedPort = options.embeddedPort ?? 4445;
  if (!Number.isInteger(embeddedPort) || embeddedPort < 1024 || embeddedPort > 65_535) {
    throw new Error(`invalid embedded WebDriver port: ${embeddedPort}`);
  }
  const appBinaryPath = path.resolve(app.appRoot, app.binaries[platform]);
  const evidenceRoot = path.resolve(app.appRoot, app.evidenceDir ?? '.cache/rightkit-qa');

  return {
    runner: 'local',
    specs: app.specs.map((spec) => path.resolve(app.appRoot, spec)),
    maxInstances: 1,
    maxInstancesPerCapability: 1,
    services: [[
      '@wdio/tauri-service',
      {
        appBinaryPath,
        driverProvider: 'embedded',
        embeddedPort,
        windowLabel: app.windowLabel,
        autoInstallTauriDriver: false,
        autoDownloadEdgeDriver: true,
        captureBackendLogs: false,
        captureFrontendLogs: false,
        startTimeout: 60_000,
        statusPollTimeout: 5_000,
        commandTimeout: 30_000,
        logDir: path.join(evidenceRoot, 'service'),
      },
    ]],
    capabilities: [{
      browserName: 'tauri',
      'tauri:options': { application: appBinaryPath },
    }],
    framework: 'mocha',
    reporters: ['spec'],
    mochaOpts: { ui: 'bdd', timeout: 60_000 },
    logLevel: 'error',
    bail: 1,
    waitforTimeout: 10_000,
    connectionRetryTimeout: 90_000,
    connectionRetryCount: 1,
    outputDir: path.join(evidenceRoot, 'wdio'),
  };
}

function currentDesktopPlatform(): 'win32' | 'darwin' {
  if (process.platform === 'win32' || process.platform === 'darwin') return process.platform;
  throw new Error(`Right Suite native QA currently supports Windows and macOS, received ${process.platform}`);
}

function requireText(value: string, name: string): string {
  const normalized = value.trim();
  if (!normalized) throw new Error(`${name} is required`);
  return normalized;
}
