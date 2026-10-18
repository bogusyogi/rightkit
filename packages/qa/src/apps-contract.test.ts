import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { validateNativeQaContract } from './guard.js';
import { cargoManifestIssues, commandFeatureIssues, parseCargoFeaturesAndDependencies } from '../../release/qa-build-features.mjs';

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
    it.skipIf(isolated || !fs.existsSync(path.join(app.root, 'package.json')))(`${app.name} declares an isolated QA lane without enabling it in release scripts`, () => {
      const packageJson = JSON.parse(fs.readFileSync(path.join(app.root, 'package.json'), 'utf8')) as {
        scripts?: Record<string, string>;
        dependencies?: Record<string, string>;
        devDependencies?: Record<string, string>;
      };
      const rustSource = app.rustSources
        .map((source) => fs.readFileSync(path.join(app.root, source), 'utf8'))
        .join('\n');
      const cargoToml = fs.readFileSync(path.join(app.root, 'src-tauri', 'Cargo.toml'), 'utf8');
      const { dependencies, features } = parseCargoFeaturesAndDependencies(cargoToml);
      if (dependencies.has('tauri-plugin-wdio-webdriver')) {
        expect(validateNativeQaContract({ cargoToml, rustSource, packageScripts: packageJson.scripts ?? {} })).toEqual([]);
      } else {
        // Migrated apps use RightKit's embedded server. Check its declared lane with
        // release's authoritative feature gate, never require the removed WDIO plugin.
        // This is source/declaration evidence, not native artifact qualification.
        expect(dependencies.get('rightkit-qa')?.optional).toBe(true);
        expect(features.get('qa-native')).toEqual(expect.arrayContaining(['dep:rightkit-qa', 'rightkit-qa/control']));
        expect(rustSource).toContain('rightkit_qa::embedded::Control');
        expect(rustSource).toContain('.build_if_enabled()');
        expect(cargoManifestIssues(cargoToml)).toEqual([]);
        for (const [name, command] of Object.entries(packageJson.scripts ?? {})) {
          if (/^(qa|test|dev)(:|$)/.test(name)) continue;
          if (!/(release|publish|build|dmg|installer|package)/i.test(name) && !/\btauri\s+build\b/.test(command)) continue;
          expect(commandFeatureIssues(command, { label: name, scripts: packageJson.scripts ?? {} })).toEqual([]);
        }
        // Prove default-feature leakage is still rejected for this actual manifest.
        const leaked = cargoToml.replace(/(\[features\][\s\S]*?)(?=\n\[|$)/, (_section, body) =>
          body.replace(/^\s*default\s*=\s*\[[\s\S]*?\]/m, '') + '\ndefault = ["qa-native"]\n');
        expect(cargoManifestIssues(leaked).join('\n')).toMatch(/default feature set reaches/);
      }

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
