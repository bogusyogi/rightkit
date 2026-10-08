import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const cli = fileURLToPath(new URL('../bin/rightkit-hooks.mjs', import.meta.url));
const modulePath = fileURLToPath(new URL('./fixtures/echo.mjs', import.meta.url));

async function run(chunks) {
  const child = spawn(process.execPath, [cli, modulePath], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  // Oversized input can close the pipe before all bytes are consumed.
  child.stdin.on('error', error => { if (error.code !== 'EPIPE') throw error; });
  const closed = once(child, 'close');
  for (const chunk of chunks) child.stdin.write(chunk);
  child.stdin.end();
  const [code] = await closed;
  return { code, stdout, stderr };
}

test('actual CLI reads piped stdin through EOF & preserves split UTF-8', async () => {
  const payload = Buffer.from(JSON.stringify({ event: 'Stop', message: 'hello 🌍' }));
  const split = payload.indexOf(Buffer.from('🌍')) + 2;
  const result = await run([payload.subarray(0, split), payload.subarray(split)]);
  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stderr, '');
  assert.equal(JSON.parse(result.stdout).results[0].output, 'hello 🌍');
});

test('CLI rejects stdin exceeding 1 MiB before importing hook modules', async () => {
  const result = await run([Buffer.alloc(1024 * 1024 + 1, ' ')]);
  assert.equal(result.code, 2);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /RIGHTKIT_HOOKS_INVALID: HookHost stdin exceeds 1048576 bytes/);
});

test('CLI rejects malformed JSON from piped stdin', async () => {
  const result = await run(['{']);
  assert.equal(result.code, 2);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /RIGHTKIT_HOOKS_INVALID/);
});
