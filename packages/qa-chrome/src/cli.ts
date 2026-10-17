#!/usr/bin/env node
import { installChromeForTesting, managedChromeForTestingRoot, managedCurrentDir, managedPreviousDir, managedVersions } from './install.js';
import { resolveChromeForTesting } from './index.js';

const [command] = process.argv.slice(2);
try {
  if (command === 'install') {
    const r = await installChromeForTesting({ log: (l) => process.stderr.write(`${l}\n`) });
    process.stdout.write(`${r.executablePath}\n`);
  } else if (command === 'path') {
    process.stdout.write(`${resolveChromeForTesting()}\n`);
  } else if (command === 'list') {
    process.stdout.write(`${managedChromeForTestingRoot()}\n  current: ${managedCurrentDir()}\n  previous: ${managedPreviousDir()}\n${managedVersions().map((v) => `  legacy ${v}`).join('\n')}\n`);
  } else {
    process.stderr.write('usage: rightkit-qa-chrome <install|path|list>\n');
    process.exitCode = 2;
  }
} catch (e) {
  process.stderr.write(`rightkit-qa-chrome: ${e instanceof Error ? e.message : String(e)}\n`);
  process.exitCode = 1;
}
