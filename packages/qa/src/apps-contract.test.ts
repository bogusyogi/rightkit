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
  {
    name: 'ViewRight',
    root: path.join(repoRoot, 'viewright'),
    rustSources: ['src-tauri/src/lib.rs'],
  },
  {
    name: 'MailRight',
    root: path.join(repoRoot, 'mailright'),
    rustSources: ['src-tauri/src/tauri_run.rs'],
  },
  {
    name: 'ScrapeRight',
    root: path.join(repoRoot, 'scraperight'),
    rustSources: ['src-tauri/src/main.rs'],
  },
] as const;

describe('core native-QA adopters', () => {
  for (const app of apps) {
    it(`${app.name} keeps WebDriver debug-only and out of release scripts`, () => {
      const packageJson = JSON.parse(fs.readFileSync(path.join(app.root, 'package.json'), 'utf8')) as {
        scripts?: Record<string, string>;
        dependencies?: Record<string, string>;
        devDependencies?: Record<string, string>;
      };
      const rustSource = app.rustSources
        .map((source) => fs.readFileSync(path.join(app.root, source), 'utf8'))
        .join('\n');
      expect(validateNativeQaContract({
        cargoToml: fs.readFileSync(path.join(app.root, 'src-tauri', 'Cargo.toml'), 'utf8'),
        rustSource,
        packageScripts: packageJson.scripts ?? {},
      })).toEqual([]);
      expect(fs.existsSync(path.join(app.root, 'right-qa.config.mjs'))).toBe(true);
      expect(fs.existsSync(path.join(app.root, 'qa', 'native', 'smoke.e2e.mjs'))).toBe(true);

      const allDependencies = {
        ...packageJson.dependencies,
        ...packageJson.devDependencies,
      } as Record<string, string>;
      for (const [name, spec] of Object.entries(allDependencies)) {
        if (!name.startsWith('@rightkit/')) continue;
        expect(spec, `${app.name} ${name}`).not.toMatch(/^(?:file|link|workspace|git|https?):/i);
      }
    });
  }
});
