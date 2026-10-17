import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { validateNativeQaContract } from './guard.js';

// Workspace root: the RightKit repo lives at <workspace>/rightkit since 2026-10-10 (was tools/rightkit).
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..');

const apps = [
  {
    name: 'CodeRight',
    root: path.join(repoRoot, 'coderight', 'apps', 'coderight-tauri'),
    rustSources: ['src-tauri/src/lib.rs'],
  },
  {
    name: 'HeardRight',
    root: path.join(repoRoot, 'heardright', 'tauri-app-next'),
    rustSources: ['src-tauri/src/app_lib.rs', 'src-tauri/src/app_run.rs'],
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

describe('core app release-backdoor guards', () => {
  for (const app of apps) {
    // These guards read sibling app checkouts next to the RightKit repo; a clean
    // clone (release publish) has none, so the guard is skipped there, not failed.
    // The local publish gate sets RIGHTKIT_ISOLATED_CHECKOUT=1 for the same reason: a package
    // publish is not blocked by a consumer's state. Each app's own release gate still enforces
    // this contract (packages/release qa-contract.mjs / qa-control-scan.mjs).
    const isolated = process.env.RIGHTKIT_ISOLATED_CHECKOUT === '1';
    it.skipIf(isolated || !fs.existsSync(path.join(app.root, 'package.json')))(`${app.name} keeps WebDriver debug-only and out of release scripts`, () => {
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
