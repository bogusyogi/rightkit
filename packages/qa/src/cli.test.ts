import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { runNativeQa } from './cli.js';

describe('right-qa native CLI runner', () => {
  it('writes passing evidence around the owned WDIO command', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rightkit-native-cli-'));
    const binary = path.join(root, 'app.exe');
    const spec = path.join(root, 'smoke.e2e.mjs');
    fs.writeFileSync(binary, 'fixture');
    fs.writeFileSync(spec, 'fixture');
    const seen: Array<{ command: string; args: string[]; env: NodeJS.ProcessEnv }> = [];

    const result = await runNativeQa({
      app: 'fixture',
      appRoot: root,
      windowLabel: 'main',
      specs: ['smoke.e2e.mjs'],
      binaries: { win32: 'app.exe', darwin: 'app' },
    }, {
      platform: 'win32',
      wdioBin: path.join(root, 'wdio.js'),
      runProcess: async (command, args, options) => {
        seen.push({ command, args, env: options.env });
        return { pid: 10, code: 0, signal: null, stdout: 'ok', stderr: '' };
      },
    });

    expect(result.exitCode).toBe(0);
    expect(seen).toHaveLength(1);
    expect(seen[0]?.command).toBe(process.execPath);
    expect(seen[0]?.args.slice(0, 2)).toEqual([path.join(root, 'wdio.js'), 'run']);
    expect(seen[0]?.env.RIGHTKIT_QA_HIDDEN).toBe('1');
    const generatedConfig = fs.readFileSync(String(seen[0]?.args[2]), 'utf8');
    expect(generatedConfig).toContain('setGlobalDispatcher(new Agent())');
    expect(generatedConfig).toContain('config.beforeSession = () =>');
    const evidence = JSON.parse(fs.readFileSync(result.evidencePath, 'utf8'));
    expect(evidence).toMatchObject({ app: 'fixture', platform: 'win32', status: 'passed' });
    expect(evidence.checks).toContainEqual(expect.objectContaining({ name: 'wdio.run', status: 'passed' }));
    expect(fs.existsSync(path.join(root, '.cache/rightkit-qa/native.lock'))).toBe(false);
  });
});
