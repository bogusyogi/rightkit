#!/usr/bin/env node
import { installChromeForTesting, managedChromeForTestingRoot, managedVersions } from './install.js';
import { resolveChromeForTesting } from './index.js';

const [command, ...rest] = process.argv.slice(2);
const channelArg = rest[rest.indexOf('--channel') + 1];
try {
  if (command === 'install') {
    const channel = rest.includes('--channel') ? (channelArg[0]!.toUpperCase() + channelArg.slice(1).toLowerCase()) as 'Stable' : 'Stable';
    const r = await installChromeForTesting({ channel, log: (l) => process.stderr.write(`${l}\n`) });
    process.stdout.write(`${r.executablePath}\n`);
  } else if (command === 'path') {
    process.stdout.write(`${resolveChromeForTesting()}\n`);
  } else if (command === 'list') {
    process.stdout.write(`${managedChromeForTestingRoot()}\n${managedVersions().map((v) => `  ${v}`).join('\n')}\n`);
  } else {
    process.stderr.write('usage: rightkit-qa-chrome <install [--channel stable|beta]|path|list>\n');
    process.exitCode = 2;
  }
} catch (e) {
  process.stderr.write(`rightkit-qa-chrome: ${e instanceof Error ? e.message : String(e)}\n`);
  process.exitCode = 1;
}
