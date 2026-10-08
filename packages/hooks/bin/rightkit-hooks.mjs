#!/usr/bin/env node
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { dispatchHookEvent } from '../lib/runtime.mjs';

const STDIN_LIMIT_BYTES = 1024 * 1024;

// fs/promises readFile(0) cannot read the numeric stdin descriptor on piped input,
// so stream process.stdin directly and enforce the size cap while reading.
async function readStdin(limit = STDIN_LIMIT_BYTES) {
  const chunks = [];
  let size = 0;
  for await (const chunk of process.stdin) {
    size += chunk.length;
    if (size > limit) throw new Error(`HookHost stdin exceeds ${limit} bytes`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

const paths = process.argv.slice(2);
if (paths.length === 0) {
  process.stderr.write('usage: rightkit-hooks <module.mjs> [module.mjs ...]\n');
  process.exitCode = 64;
} else {
  try {
    const payload = JSON.parse(await readStdin());
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
