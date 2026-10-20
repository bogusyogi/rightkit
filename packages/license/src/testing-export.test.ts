import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { afterAll, describe, expect, it } from 'vitest';

// Use installed Vitest's pinned Vite peer, without introducing another Vite version.
const require = createRequire(import.meta.url);
const vitestManifest = realpathSync(require.resolve('vitest/package.json'));
const vitestRequire = createRequire(vitestManifest);
const viteEntry = pathToFileURL(vitestRequire.resolve('vite')).href;
const vitestCli = join(dirname(vitestManifest), 'vitest.mjs');
const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const scratch = resolve(process.platform === 'win32' ? 'D:/scratch' : '/Volumes/D/scratch');
const fixtures: string[] = [];

function fixture(main: string, extra: Record<string, string> = {}) {
  mkdirSync(scratch, { recursive: true });
  const root = mkdtempSync(join(scratch, 'rk-license-exports-'));
  fixtures.push(root);
  const staged = join(root, 'node_modules', '@rightkit', 'license');
  mkdirSync(join(staged, 'dist'), { recursive: true });
  // Stage real published layout & current export map; owner builds package first.
  copyFileSync(join(packageRoot, 'package.json'), join(staged, 'package.json'));
  for (const file of ['index.js', 'index.d.ts', 'testing.js', 'testing.d.ts']) {
    copyFileSync(join(packageRoot, 'dist', file), join(staged, 'dist', file));
  }
  writeFileSync(join(root, 'package.json'), '{"private":true,"type":"module"}');
  writeFileSync(join(root, 'index.html'), '<script type="module" src="/main.ts"></script>');
  writeFileSync(join(root, 'main.ts'), main);
  for (const [file, source] of Object.entries(extra)) writeFileSync(join(root, file), source);
  writeFileSync(join(root, 'runner.mjs'), `
    import { build, createServer } from ${JSON.stringify(viteEntry)};
    const root = process.cwd();
    if (process.argv[2] === 'development') {
      const server = await createServer({ root, configFile: false, logLevel: 'silent',
        server: { middlewareMode: true, watch: null }, ssr: { noExternal: ['@rightkit/license'] } });
      try {
        const module = await server.ssrLoadModule('/main.ts');
        const view = await module.client.status();
        if (view.lifecycle !== 'unlicensed') throw new Error('Mock did not execute');
        console.log('development-mock-executed');
      } finally { await server.close(); }
    } else {
      // Deliberately leave default resolver conditions intact. NODE_ENV decides
      // production|development even when CLI mode is explicitly production.
      const result = await build({ root, configFile: false, mode: 'production', logLevel: 'silent',
        build: { outDir: 'out', minify: false } });
      const bundles = Array.isArray(result) ? result : [result];
      const modules = bundles.flatMap(bundle => bundle.output.flatMap(item =>
        item.type === 'chunk' ? Object.keys(item.modules) : []));
      console.log(JSON.stringify({ modules }));
    }
  `);
  return root;
}

function run(root: string, nodeEnv: 'production' | 'development', action = 'build') {
  return spawnSync(process.execPath, [join(root, 'runner.mjs'), action], {
    cwd: root, env: { ...process.env, NODE_ENV: nodeEnv }, encoding: 'utf8', timeout: 60_000,
    windowsHide: true,
  });
}
function expectBlocked(result: ReturnType<typeof run>) {
  expect(result.error).toBeUndefined();
  expect(result.status, result.stdout + result.stderr).not.toBe(0);
  expect(result.stderr).toMatch(/@rightkit\/license|testing/);
}

afterAll(() => {
  for (const root of fixtures) {
    if (!resolve(root).startsWith(scratch + sep)) throw new Error('Fixture escaped scratch');
    rmSync(root, { recursive: true, force: true });
  }
});

describe('real Vite testing-export conditions', () => {
  it('keeps main production entry usable', () => {
    const root = fixture("import { licenseMessage } from '@rightkit/license'; console.log(licenseMessage('busy'));");
    const result = run(root, 'production');
    expect(result.status, result.stdout + result.stderr).toBe(0);
  }, 70_000);

  it.each([
    ['static', "import { createLicenseMock } from '@rightkit/license/testing'; console.log(createLicenseMock());", {}],
    ['dynamic', "import('@rightkit/license/testing').then(m => console.log(m.createLicenseMock()));", {}],
    ['re-export', "import { createLicenseMock } from './relay.js'; console.log(createLicenseMock());", { 'relay.js': "export { createLicenseMock } from '@rightkit/license/testing';" }],
    ['worker', "new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });", { 'worker.js': "import { createLicenseMock } from '@rightkit/license/testing'; self.postMessage(createLicenseMock().sentinel);" }],
  ] as const)('blocks retained %s import under production conditions', (_name, main, extra) => {
    expectBlocked(run(fixture(main, extra), 'production'));
  }, 70_000);

  it.each(['@rightkit/license/dist/testing.js', '@rightkit/license/src/testing.ts', '@rightkit/license/testing.js'])(
    'blocks unexported deep path %s in both environments', specifier => {
      const root = fixture(`import { createLicenseMock } from ${JSON.stringify(specifier)}; console.log(createLicenseMock());`);
      expectBlocked(run(root, 'production'));
      expectBlocked(run(root, 'development'));
    }, 130_000,
  );

  it('allows an erased type-only import without retaining mock module', () => {
    const root = fixture("import type { createLicenseMock } from '@rightkit/license/testing'; console.log('erased import');");
    const result = run(root, 'production');
    expect(result.status, result.stdout + result.stderr).toBe(0);
    const output = JSON.parse(result.stdout.trim().split('\n').at(-1)!);
    expect(output.modules.some((id: string) => /[/\\]testing\.js(?:$|\?)/.test(id))).toBe(false);
  }, 70_000);

  it('permits unused value import only when resolver rejects it or bundler erases mock', () => {
    const root = fixture("import { createLicenseMock } from '@rightkit/license/testing'; console.log('unused import');");
    const result = run(root, 'production');
    if (result.status !== 0) { expectBlocked(result); return; }
    const output = JSON.parse(result.stdout.trim().split('\n').at(-1)!);
    expect(output.modules.some((id: string) => /[/\\]testing\.js(?:$|\?)/.test(id))).toBe(false);
  }, 70_000);

  it('executes mock through Vite development resolution', () => {
    const root = fixture("import { createLicenseMock } from '@rightkit/license/testing'; export const { client } = createLicenseMock();");
    const result = run(root, 'development', 'development');
    expect(result.status, result.stdout + result.stderr).toBe(0);
    expect(result.stdout).toContain('development-mock-executed');
  }, 70_000);

  it('shows inherited development NODE_ENV admits mock even with production build mode', () => {
    const root = fixture("import { createLicenseMock } from '@rightkit/license/testing'; console.log(createLicenseMock());");
    expectBlocked(run(root, 'production'));
    const inherited = run(root, 'development');
    expect(inherited.status, inherited.stdout + inherited.stderr).toBe(0);
  }, 130_000);

  it('executes mock through Vitest default test resolution', () => {
    const root = fixture('');
    // Bare ESM import selects installed package's public import condition.
    // createRequire.resolve('vitest') would select its throwing CommonJS entry.
    symlinkSync(dirname(vitestManifest), join(root, 'node_modules', 'vitest'), process.platform === 'win32' ? 'junction' : 'dir');
    writeFileSync(join(root, 'mock.test.js'), `
      import { test, expect } from 'vitest';
      import { createLicenseMock } from '@rightkit/license/testing';
      test('mock is usable', async () => {
        expect((await createLicenseMock().client.status()).lifecycle).toBe('unlicensed');
      });
    `);
    writeFileSync(join(root, 'vitest.config.mjs'), 'export default { test: { include: ["mock.test.js"], maxWorkers: 1 } };');
    const result = spawnSync(process.execPath, [vitestCli, 'run', '--config', join(root, 'vitest.config.mjs')], {
      cwd: root, env: { ...process.env, NODE_ENV: 'test' }, encoding: 'utf8', timeout: 60_000, windowsHide: true,
    });
    expect(result.error).toBeUndefined();
    expect(result.status, result.stdout + result.stderr).toBe(0);
  }, 70_000);
});
