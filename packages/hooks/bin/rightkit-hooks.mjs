#!/usr/bin/env node
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { dispatchHookEvent } from '../lib/runtime.mjs';

const paths = process.argv.slice(2);
if (paths.length === 0) {
  process.stderr.write('usage: rightkit-hooks <module.mjs> [module.mjs ...]\n');
  process.exitCode = 64;
} else {
  try {
    const payload = JSON.parse(await readFile(0, 'utf8'));
    const modules = [];
    for (const path of paths) {
      const loaded = await import(pathToFileURL(resolve(path)));
      modules.push({ id: loaded.id ?? path, handle: loaded.handleHook });
    }
    const result = await dispatchHookEvent(payload, modules);
    process.stdout.write(`${JSON.stringify(result)}\n`);
    process.exitCode = result.status === 'ok' ? 0 : 1;
  } catch (error) {
    process.stderr.write(`RIGHTKIT_HOOKS_INVALID: ${error?.message ?? error}\n`);
    process.exitCode = 2;
  }
}
