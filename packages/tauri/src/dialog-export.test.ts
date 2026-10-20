import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { afterAll, describe, expect, it } from 'vitest';

// Real Vite resolution of the `./dialog` export map, against the package as published: every build,
// whatever its conditions (the retired `rightkit-qa` condition included), resolves the plugin wrapper
// and no QA module exists to resolve. The owner builds the package (dist/) before this runs.
// Use installed Vitest's pinned Vite peer, without introducing another Vite version.
const require = createRequire(import.meta.url);
const vitestManifest = realpathSync(require.resolve('vitest/package.json'));
const vitestRequire = createRequire(vitestManifest);
const viteEntry = pathToFileURL(vitestRequire.resolve('vite')).href;
const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const scratch = resolve(process.platform === 'win32' ? 'D:/scratch' : '/Volumes/D/scratch');
const fixtures: string[] = [];

const PLUGIN_STUB_MARKER = 'RK_FIXTURE_PLUGIN_DIALOG_STUB';
const QA_SENTINEL = 'RIGHTKIT_QA_DIALOG_DO_NOT_SHIP';
const QA_STORE = '__RIGHTKIT_QA_DIALOG__';

function fixture(main: string) {
  mkdirSync(scratch, { recursive: true });
  const root = mkdtempSync(join(scratch, 'rk-tauri-dialog-exports-'));
  fixtures.push(root);
  const staged = join(root, 'node_modules', '@rightkit', 'tauri');
  mkdirSync(join(staged, 'dist'), { recursive: true });
  copyFileSync(join(packageRoot, 'package.json'), join(staged, 'package.json'));
  for (const file of ['dialog', 'dialog-core', 'dialog-types']) {
    copyFileSync(join(packageRoot, 'dist', `${file}.js`), join(staged, 'dist', `${file}.js`));
    copyFileSync(join(packageRoot, 'dist', `${file}.d.ts`), join(staged, 'dist', `${file}.d.ts`));
  }
  // The dialog plugin, as an identifiable stand-in: its presence in a bundle proves the wrapper is in it.
  const plugin = join(root, 'node_modules', '@tauri-apps', 'plugin-dialog');
  mkdirSync(plugin, { recursive: true });
  writeFileSync(join(plugin, 'package.json'), '{"name":"@tauri-apps/plugin-dialog","version":"2.7.1","type":"module","main":"index.js","exports":{".":"./index.js"}}');
  writeFileSync(join(plugin, 'index.js'), `export const marker = '${PLUGIN_STUB_MARKER}'; export async function open() { return marker; }`);
  writeFileSync(join(root, 'package.json'), '{"private":true,"type":"module"}');
  writeFileSync(join(root, 'index.html'), '<script type="module" src="/main.ts"></script>');
  writeFileSync(join(root, 'main.ts'), main);
  writeFileSync(join(root, 'runner.mjs'), `
    import { build, defaultClientConditions } from ${JSON.stringify(viteEntry)};
    const qa = process.argv[2] === 'qa';
    const result = await build({ root: process.cwd(), configFile: false, logLevel: 'silent',
      mode: process.env.NODE_ENV === 'production' ? 'production' : 'development',
      resolve: { conditions: qa ? [...defaultClientConditions, 'rightkit-qa'] : [...defaultClientConditions] },
      build: { outDir: 'out', minify: false } });
    const bundles = Array.isArray(result) ? result : [result];
    const chunks = bundles.flatMap(bundle => bundle.output.filter(item => item.type === 'chunk'));
    console.log(JSON.stringify({
      modules: chunks.flatMap(chunk => Object.keys(chunk.modules)),
      code: chunks.map(chunk => chunk.code).join('\\n'),
    }));
  `);
  return root;
}

function build(root: string, nodeEnv: 'production' | 'development', conditions: 'default' | 'qa') {
  return spawnSync(process.execPath, [join(root, 'runner.mjs'), conditions], {
    cwd: root, env: { ...process.env, NODE_ENV: nodeEnv }, encoding: 'utf8', timeout: 60_000, windowsHide: true,
  });
}
function output(result: ReturnType<typeof build>) {
  expect(result.error).toBeUndefined();
  expect(result.status, result.stdout + result.stderr).toBe(0);
  return JSON.parse(result.stdout.trim().split('\n').at(-1)!) as { modules: string[]; code: string };
}
function expectBlocked(result: ReturnType<typeof build>) {
  expect(result.error).toBeUndefined();
  expect(result.status, result.stdout + result.stderr).not.toBe(0);
  expect(result.stderr).toMatch(/@rightkit\/tauri|dialog/);
}

const APP = "import { pickFolder } from '@rightkit/tauri/dialog'; pickFolder().then(console.log);";

afterAll(() => {
  for (const root of fixtures) {
    if (!resolve(root).startsWith(scratch + sep)) throw new Error('Fixture escaped scratch');
    rmSync(root, { recursive: true, force: true });
  }
});

describe('@rightkit/tauri/dialog export conditions under real Vite resolution', () => {
  it('production build: the plugin wrapper, the plugin, and no QA module, queue or sentinel', () => {
    const out = output(build(fixture(APP), 'production', 'default'));
    expect(out.modules.some(id => /dialog-qa/.test(id))).toBe(false);
    expect(out.modules.some(id => /plugin-dialog/.test(id))).toBe(true);
    expect(out.code).toContain(PLUGIN_STUB_MARKER);
    for (const marker of [QA_SENTINEL, QA_STORE, 'QaDialogError', 'unanswered native dialog']) expect(out.code).not.toContain(marker);
  }, 70_000);

  it('development build, with or without the retired rightkit-qa condition, gets the plugin wrapper', () => {
    for (const conditions of ['default', 'qa'] as const) {
      const out = output(build(fixture(APP), 'development', conditions));
      expect(out.modules.some(id => /dialog-qa/.test(id))).toBe(false);
      expect(out.code).toContain(PLUGIN_STUB_MARKER);
      expect(out.code).not.toContain(QA_SENTINEL);
    }
  }, 140_000);

  it.each(['@rightkit/tauri/dist/dialog-qa.js', '@rightkit/tauri/src/dialog-qa.ts', '@rightkit/tauri/dialog-qa'])(
    'blocks unexported deep path %s in every environment',
    specifier => {
      const root = fixture(`import { pickFolder } from ${JSON.stringify(specifier)}; pickFolder();`);
      expectBlocked(build(root, 'production', 'default'));
      expectBlocked(build(root, 'development', 'qa'));
    },
    130_000,
  );

  it('a retained dynamic import of the dialog entry resolves the same way', () => {
    const main = "import('@rightkit/tauri/dialog').then(m => m.pickFile());";
    const prod = output(build(fixture(main), 'production', 'default'));
    expect(prod.code).not.toContain(QA_SENTINEL);
    expect(prod.code).toContain(PLUGIN_STUB_MARKER);
  }, 70_000);
});
