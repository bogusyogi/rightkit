import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { validateNativeQaContract } from './guard.js';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../../..');

const apps = [
  {
    name: 'CodeRight',
    root: path.join(repoRoot, 'coderight', 'apps', 'coderight-tauri'),
    rustSources: ['src-tauri/src/lib.rs'],
  },
  {
    name: 'HeardRight',
    root: path.join(repoRoot, 'heardright', 'tauri-app-next'),
    rustSources: ['src-tauri/src/app_lib.rs', 'src-tauri/src/app_lib_sections/section08.rs'],
  },
] as const;

describe('first native-QA adopters', () => {
  for (const app of apps) {
    it(`${app.name} keeps WebDriver debug-only and out of release scripts`, () => {
      const packageJson = JSON.parse(fs.readFileSync(path.join(app.root, 'package.json'), 'utf8')) as {
        scripts?: Record<string, string>;
      };
      const rustSource = app.rustSources
        .map((source) => fs.readFileSync(path.join(app.root, source), 'utf8'))
        .join('\n');
      expect(validateNativeQaContract({
        cargoToml: fs.readFileSync(path.join(app.root, 'src-tauri', 'Cargo.toml'), 'utf8'),
        rustSource,
        packageScripts: packageJson.scripts ?? {},
      })).toEqual([]);
    });
  }
});
