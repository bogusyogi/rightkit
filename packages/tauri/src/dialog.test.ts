import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { pickFileWith, pickFilesWith, pickFolderWith, type OpenFn } from './dialog-core.js';

const here = dirname(fileURLToPath(import.meta.url));
const source = (file: string) => readFileSync(join(here, file), 'utf8');

function recorder(result: unknown) {
  const calls: Record<string, unknown>[] = [];
  const open: OpenFn = async options => {
    calls.push(options);
    return result;
  };
  return { open, calls };
}

describe('production dialog wrappers', () => {
  it('pickFolder asks the plugin for one directory and passes every other option through', async () => {
    const { open, calls } = recorder('/work/project');
    const filters = [{ name: 'Media', extensions: ['mp4'] }];
    await expect(
      pickFolderWith(open, { title: 'Open', defaultPath: '/work', canCreateDirectories: false, futurePluginOption: 7, filters }),
    ).resolves.toBe('/work/project');
    expect(calls).toEqual([
      { title: 'Open', defaultPath: '/work', canCreateDirectories: false, futurePluginOption: 7, filters, directory: true, multiple: false },
    ]);
  });

  it('options cannot turn a folder pick into a multi-file pick', async () => {
    const { open, calls } = recorder(null);
    await pickFolderWith(open, { directory: false, multiple: true });
    await pickFileWith(open, { directory: true, multiple: true });
    await pickFilesWith(open, { directory: true, multiple: false });
    expect(calls).toEqual([
      { directory: true, multiple: false },
      { directory: false, multiple: false },
      { directory: false, multiple: true },
    ]);
  });

  it('cancel resolves null for all three, never an empty value or an error', async () => {
    const { open } = recorder(null);
    await expect(pickFolderWith(open)).resolves.toBeNull();
    await expect(pickFileWith(open)).resolves.toBeNull();
    await expect(pickFilesWith(open)).resolves.toBeNull();
  });

  it('pickFile returns the single path; pickFiles returns the list, and a lone string becomes a one-item list', async () => {
    await expect(pickFileWith(recorder('/a/b.mp4').open, { title: 'File' })).resolves.toBe('/a/b.mp4');
    await expect(pickFilesWith(recorder(['/a/1.png', '/a/2.png']).open)).resolves.toEqual(['/a/1.png', '/a/2.png']);
    await expect(pickFilesWith(recorder('/a/1.png').open)).resolves.toEqual(['/a/1.png']);
  });

  it('refuses a plugin result of the wrong shape instead of guessing', async () => {
    await expect(pickFolderWith(recorder(['/a']).open)).rejects.toThrow(/pickFolder.*an array/);
    await expect(pickFileWith(recorder(undefined).open)).rejects.toThrow(/pickFile.*undefined/);
    await expect(pickFilesWith(recorder([1, 2]).open)).rejects.toThrow(/pickFiles.*an array/);
    await expect(pickFilesWith(recorder(42).open)).rejects.toThrow(/pickFiles.*number/);
  });

  it('a plugin failure propagates unchanged', async () => {
    const open: OpenFn = async () => {
      throw new Error('plugin down');
    };
    await expect(pickFolderWith(open)).rejects.toThrow('plugin down');
  });
});

describe('production sources carry no QA code', () => {
  const productionFiles = ['dialog.ts', 'dialog-core.ts', 'dialog-types.ts', 'index.ts'];
  const qaMarkers = [/RIGHTKIT_QA_DIALOG/, /__RIGHTKIT_QA_DIALOG__/, /dialog-qa/, /createQaDialog/, /QaDialog/, /answer_next_dialog/];

  it.each(productionFiles)('%s names no QA entrypoint, queue, control or sentinel', file => {
    const text = source(file);
    for (const marker of qaMarkers) expect(text, `${file} matches ${marker}`).not.toMatch(marker);
  });

  it('only dialog.ts imports the plugin, and the main entry never does', () => {
    expect(source('dialog.ts')).toMatch(/from '@tauri-apps\/plugin-dialog'/);
    for (const file of ['dialog-core.ts', 'dialog-types.ts', 'index.ts']) {
      expect(source(file), file).not.toMatch(/plugin-dialog/);
    }
  });
});
