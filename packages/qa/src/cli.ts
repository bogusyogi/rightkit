#!/usr/bin/env node
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {
  addEvidenceCheck,
  createEvidenceBundle,
  finishEvidenceBundle,
  writeEvidenceBundle,
} from './evidence.js';
import { createWdioConfig, defineRightQaConfig } from './native.js';
import type { NativeQaRuntimeOptions, RightQaConfig } from './native.js';
import { acquireRunOwnership } from './ownership.js';
import { runOwnedProcess } from './process.js';
import type { OwnedProcessResult } from './process.js';

type DesktopPlatform = 'win32' | 'darwin';
type RunProcess = (
  command: string,
  args: string[],
  options: { cwd: string; timeoutMs: number; env: NodeJS.ProcessEnv },
) => Promise<OwnedProcessResult>;

export interface NativeRunOptions extends NativeQaRuntimeOptions {
  timeoutMs?: number;
  wdioBin?: string;
  runProcess?: RunProcess;
}

export interface NativeRunResult {
  exitCode: number;
  evidencePath: string;
}

export async function runNativeQa(
  rawConfig: RightQaConfig,
  options: NativeRunOptions = {},
): Promise<NativeRunResult> {
  const app = defineRightQaConfig(rawConfig);
  const platform = options.platform ?? requireDesktopPlatform();
  const wdioConfig = createWdioConfig(app, { platform, embeddedPort: options.embeddedPort });
  const binary = String(
    (wdioConfig.capabilities[0]?.['tauri:options'] as { application?: string } | undefined)?.application ?? '',
  );
  if (!fs.existsSync(binary)) throw new Error(`native QA binary not found: ${binary}`);

  const cacheRoot = path.resolve(app.appRoot, app.evidenceDir ?? '.cache/rightkit-qa');
  const runId = crypto.randomUUID();
  const runRoot = path.join(cacheRoot, 'runs', runId);
  const generatedConfig = path.join(runRoot, 'wdio.generated.mjs');
  const evidencePath = path.join(runRoot, 'evidence.json');
  fs.mkdirSync(runRoot, { recursive: true });
  fs.writeFileSync(
    generatedConfig,
    [
      `import { Agent, setGlobalDispatcher } from ${JSON.stringify(resolveUndiciEntry())};`,
      'setGlobalDispatcher(new Agent());',
      `const config = ${JSON.stringify(wdioConfig, null, 2)};`,
      'config.beforeSession = () => { setGlobalDispatcher(new Agent()); };',
      'export { config };',
      '',
    ].join('\n'),
    'utf8',
  );

  const ownership = await acquireRunOwnership(path.join(cacheRoot, 'native.lock'), `${app.app}:native`);
  const evidence = createEvidenceBundle({ app: app.app, platform, runId });
  const runProcess = options.runProcess ?? runOwnedProcess;
  try {
    const result = await runProcess(
      process.execPath,
      [options.wdioBin ?? resolveWdioBin(), 'run', generatedConfig],
      {
        cwd: app.appRoot,
        timeoutMs: options.timeoutMs ?? 5 * 60_000,
        env: {
          ...process.env,
          RIGHTKIT_QA_NATIVE: '1',
          RIGHTKIT_QA_HIDDEN: '1',
          TAURI_WEBDRIVER_PORT: String(options.embeddedPort ?? 4445),
        },
      },
    );
    const passed = result.code === 0;
    addEvidenceCheck(evidence, {
      name: 'wdio.run',
      status: passed ? 'passed' : 'failed',
      details: {
        code: result.code,
        signal: result.signal,
        stdoutTail: tail(result.stdout),
        stderrTail: tail(result.stderr),
      },
    });
    finishEvidenceBundle(evidence, { status: passed ? 'passed' : 'failed' });
    writeEvidenceBundle(evidencePath, evidence);
    return { exitCode: passed ? 0 : 1, evidencePath };
  } catch (error) {
    addEvidenceCheck(evidence, {
      name: 'wdio.run',
      status: 'failed',
      details: { error: error instanceof Error ? error.message : String(error) },
    });
    finishEvidenceBundle(evidence, { status: 'failed' });
    writeEvidenceBundle(evidencePath, evidence);
    throw error;
  } finally {
    ownership.release();
  }
}

export async function loadRightQaConfig(configPath: string): Promise<RightQaConfig> {
  const resolved = path.resolve(configPath);
  const loaded = await import(`${pathToFileURL(resolved).href}?t=${Date.now()}`) as { default?: RightQaConfig };
  if (!loaded.default) throw new Error(`${resolved} must export a default RightKit QA config`);
  return defineRightQaConfig(loaded.default);
}

async function main(argv = process.argv.slice(2)): Promise<number> {
  const command = argv[0];
  const configArg = valueAfter(argv, '--config') ?? 'right-qa.config.mjs';
  if (command === 'doctor') {
    const config = await loadRightQaConfig(configArg);
    process.stdout.write(`right-qa config valid: ${config.app}\n`);
    return 0;
  }
  if (command === 'native') {
    const config = await loadRightQaConfig(configArg);
    const result = await runNativeQa(config);
    process.stdout.write(`right-qa evidence: ${result.evidencePath}\n`);
    return result.exitCode;
  }
  throw new Error('usage: right-qa <doctor|native> [--config right-qa.config.mjs]');
}

function resolveWdioBin(): string {
  const require = createRequire(import.meta.url);
  const entry = require.resolve('@wdio/cli');
  return path.resolve(path.dirname(entry), '../bin/wdio.js');
}

function resolveUndiciEntry(): string {
  const require = createRequire(import.meta.url);
  return pathToFileURL(require.resolve('undici')).href;
}

function requireDesktopPlatform(): DesktopPlatform {
  if (process.platform === 'win32' || process.platform === 'darwin') return process.platform;
  throw new Error(`Right Suite native QA supports Windows and macOS, received ${process.platform}`);
}

function valueAfter(argv: string[], flag: string): string | undefined {
  const index = argv.indexOf(flag);
  return index >= 0 ? argv[index + 1] : undefined;
}

function tail(value: string, maxLength = 4_000): string {
  return value.length <= maxLength ? value : value.slice(-maxLength);
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  main().then(
    (code) => { process.exitCode = code; },
    (error) => {
      process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
      process.exitCode = 1;
    },
  );
}
