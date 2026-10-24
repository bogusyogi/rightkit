import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'hooks-normalize-dispatch-bin-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted module or CLI error.
let stage = 'artifact-provenance';
let scratch = null;
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/hooks');
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/hooks', rootResolvedUrl: root,
    entries: { '@rightkit/hooks': root },
  });
  result.artifact = provenance.artifact;
  const hooks = await import(provenance.entries['@rightkit/hooks']);
  const packageRoot = realpathSync(path.resolve(path.dirname(realpathSync(fileURLToPath(root))), '..'));
  const installed = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const bin = path.join(packageRoot, 'bin', 'rightkit-hooks.mjs');
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };

  // Hook modules the CLI scenarios load. They are written to a private temp directory, never to the install.
  scratch = mkdtempSync(path.join(os.tmpdir(), 'rk-hooks-consumer-'));
  const moduleFile = (name, source) => { const file = path.join(scratch, name); writeFileSync(file, source); return file; };
  const echo = moduleFile('echo.mjs', "export const id = 'echo';\nexport const handleHook = event => event.payload.message ?? event.event;\n");
  const noId = moduleFile('no-id.mjs', 'export const handleHook = event => ({ saw: event.event });\n');
  const boom = moduleFile('boom.mjs', "export const id = 'boom';\nexport const handleHook = () => { throw new Error('boom'); };\n");
  const late = moduleFile('late.mjs', "export const id = 'late';\nexport const handleHook = event => `late:${event.event}`;\n");
  const broken = moduleFile('broken.mjs', "throw new Error('import-time failure');\n");
  const cli = (args, chunks = []) => new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [bin, ...args], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true, timeout: 60_000 });
    let stdout = '', stderr = '';
    child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
    child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
    // Oversized input can close the pipe before all bytes are consumed.
    child.stdin.on('error', error => { if (error.code !== 'EPIPE' && error.code !== 'ERR_STREAM_WRITE_AFTER_END') reject(error); });
    child.on('error', reject);
    once(child, 'close').then(([code, signal]) => resolve({ code, signal, stdout, stderr }), reject);
    for (const chunk of chunks) child.stdin.write(chunk);
    child.stdin.end();
  });

  await run('installed-manifest-and-exact-export-surface', async () => {
    assert.equal(installed.name, '@rightkit/hooks');
    assert.equal(installed.version, version);
    assert.equal(installed.type, 'module');
    assert.deepEqual(installed.exports, { '.': './lib/runtime.mjs' });
    // The packer may or may not strip the leading "./" of a bin target; either spelling is the same file.
    assert.deepEqual(Object.fromEntries(Object.entries(installed.bin).map(([key, target]) => [key, target.replace(/^\.\//, '')])), { 'rightkit-hooks': 'bin/rightkit-hooks.mjs' });
    for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies']) assert.equal(installed[field], undefined);
    assert.ok(readFileSync(bin, 'utf8').startsWith('#!/usr/bin/env node'));
    assert.deepEqual(Object.keys(hooks).sort(), ['dispatchHookEvent', 'normalizeHookEvent']);
    assert.equal(typeof hooks.normalizeHookEvent, 'function');
    assert.equal(typeof hooks.dispatchHookEvent, 'function');
    return { exports: ['.'], bin: 'rightkit-hooks', dependencies: 0, moduleExports: 2 };
  });

  await run('event-normalization-of-host-spellings', async () => {
    const event = hooks.normalizeHookEvent({ hook_event_name: 'PreToolUse', thread_id: 'thread-1', tool_name: 'Write', tool_use_id: 'use-1', extra: { keep: true } });
    assert.deepEqual({ ...event, payload: undefined }, { schemaVersion: 1, event: 'PreToolUse', sessionId: 'thread-1', toolName: 'Write', toolUseId: 'use-1', payload: undefined });
    assert.deepEqual(event.payload, { hook_event_name: 'PreToolUse', thread_id: 'thread-1', tool_name: 'Write', tool_use_id: 'use-1', extra: { keep: true } });
    // The other spellings of each field, and the order they are tried in.
    const camel = hooks.normalizeHookEvent({ hookEventName: 'Stop', sessionId: 's-2', toolName: 'Read', toolUseId: 'u-2' });
    assert.deepEqual([camel.event, camel.sessionId, camel.toolName, camel.toolUseId], ['Stop', 's-2', 'Read', 'u-2']);
    const plain = hooks.normalizeHookEvent({ event: 'SessionStart', session_id: 's-3', tool_name: 'Bash' });
    assert.deepEqual([plain.event, plain.sessionId, plain.toolName, plain.toolUseId], ['SessionStart', 's-3', 'Bash', null]);
    const both = hooks.normalizeHookEvent({ hook_event_name: 'A', hookEventName: 'B', event: 'C', thread_id: 't', session_id: 's', sessionId: 'x' });
    assert.deepEqual([both.event, both.sessionId], ['A', 't']);
    // A blank or non-string spelling is skipped, a missing field is null, and the event is required.
    const skipped = hooks.normalizeHookEvent({ hook_event_name: '   ', hookEventName: 7, event: 'Real', thread_id: '', session_id: 'kept' });
    assert.deepEqual([skipped.event, skipped.sessionId, skipped.toolName], ['Real', 'kept', null]);
    // The result is frozen, and the payload is a copy that later changes to the original do not reach.
    const original = { event: 'Stop', message: 'one' };
    const copy = hooks.normalizeHookEvent(original);
    original.message = 'two';
    assert.equal(copy.payload.message, 'one');
    assert.ok(Object.isFrozen(copy) && Object.isFrozen(copy.payload));
    for (const bad of [null, undefined, [], 'Stop', 7, {}, { event: '' }, { event: '  ' }, { event: 3 }]) {
      assert.throws(() => hooks.normalizeHookEvent(bad), TypeError);
    }
    assert.throws(() => hooks.normalizeHookEvent({}), /event is required/);
    assert.throws(() => hooks.normalizeHookEvent([]), /payload must be an object/);
    return { spellings: 7, refused: 9, frozen: true, payloadCopied: true };
  });

  await run('dispatch-order-results-and-module-errors', async () => {
    const order = [];
    const seen = [];
    const done = await hooks.dispatchHookEvent({ event: 'Stop', message: 'hi' }, [
      { id: 'memory', handle: event => { order.push('memory'); seen.push(event); return { saved: true }; } },
      { id: 'quiet', handle: () => undefined },
      { id: 'async', handle: async () => { await new Promise(resolve => setTimeout(resolve, 5)); order.push('async'); return 'later'; } },
      { id: 'last', handle: () => { order.push('last'); return 0; } },
    ]);
    assert.deepEqual(order, ['memory', 'async', 'last'], 'modules run one after another in declared order');
    assert.deepEqual(done, {
      schemaVersion: 1, event: 'Stop', status: 'ok',
      results: [
        { id: 'memory', status: 'ok', output: { saved: true } },
        { id: 'quiet', status: 'ok', output: null },
        { id: 'async', status: 'ok', output: 'later' },
        { id: 'last', status: 'ok', output: 0 },
      ],
    });
    assert.ok(Object.isFrozen(done) && Object.isFrozen(done.results) && Object.isFrozen(done.results[0]));
    assert.equal(seen[0].event, 'Stop');
    assert.equal(seen[0].payload.message, 'hi');
    assert.deepEqual(await hooks.dispatchHookEvent({ event: 'Stop' }, []), { schemaVersion: 1, event: 'Stop', status: 'ok', results: [] });

    // A throwing module is reported and the next module still runs; the overall status is error.
    const mixed = await hooks.dispatchHookEvent({ hookEventName: 'PreToolUse' }, [
      { id: 'throws', handle: () => { throw new Error('refused'); } },
      { id: 'rejects', handle: async () => { throw 'plain text'; } },
      { id: 'after', handle: () => 'still ran' },
    ]);
    assert.equal(mixed.status, 'error');
    assert.equal(mixed.event, 'PreToolUse');
    assert.deepEqual(mixed.results, [
      { id: 'throws', status: 'error', error: 'refused' },
      { id: 'rejects', status: 'error', error: 'plain text' },
      { id: 'after', status: 'ok', output: 'still ran' },
    ]);
    // A module cannot change the event another module sees.
    const mutated = await hooks.dispatchHookEvent({ event: 'Stop', n: 1 }, [
      { id: 'mutates', handle: event => { try { event.event = 'X'; event.payload.n = 2; return 'mutated'; } catch { return 'frozen'; } } },
      { id: 'reads', handle: event => `${event.event}:${event.payload.n}` },
    ]);
    assert.deepEqual(mutated.results.map(r => r.output), ['frozen', 'Stop:1']);
    return { modules: 4, errorsReported: 2, frozenResults: true, eventImmutable: true };
  });

  await run('dispatch-timeout-aborts-the-signal-and-continues', async () => {
    let signal = null;
    const started = Date.now();
    const done = await hooks.dispatchHookEvent({ event: 'SessionStart' }, [
      { id: 'slow', handle: (event, context) => { signal = context.signal; return new Promise(() => {}); } },
      { id: 'next', handle: () => 'ran' },
    ], { timeoutMs: 20 });
    assert.equal(done.status, 'error');
    assert.deepEqual(done.results, [
      { id: 'slow', status: 'error', error: 'HookHost module timed out: slow' },
      { id: 'next', status: 'ok', output: 'ran' },
    ]);
    assert.ok(signal instanceof AbortSignal && signal.aborted, 'the module was told to stop');
    assert.ok(Date.now() - started < 30_000);
    // A fast module is given a live signal and the deadline does not fire after it returns.
    let live = null;
    const quick = await hooks.dispatchHookEvent({ event: 'Stop' }, [{ id: 'quick', handle: (event, context) => { live = context.signal; return 'ok'; } }], { timeoutMs: 20 });
    await new Promise(resolve => setTimeout(resolve, 60));
    assert.equal(quick.status, 'ok');
    assert.equal(live.aborted, false, 'a finished module is not aborted later');
    return { timedOut: 1, signalAborted: true, nextModuleRan: true };
  });

  await run('dispatch-argument-validation', async () => {
    const ok = { event: 'Stop' };
    const module = { id: 'm', handle: () => 1 };
    await assert.rejects(hooks.dispatchHookEvent(ok, 'not an array'), /modules must be an array/);
    await assert.rejects(hooks.dispatchHookEvent(ok, undefined), TypeError);
    for (const timeoutMs of [0, -1, 1.5, '3000', null, NaN, Infinity]) {
      await assert.rejects(hooks.dispatchHookEvent(ok, [module], { timeoutMs }), /timeout must be positive/, String(timeoutMs));
    }
    await assert.rejects(hooks.dispatchHookEvent({}, [module]), /event is required/);
    await assert.rejects(hooks.dispatchHookEvent(null, [module]), TypeError);
    // A module without an id or a handle is a programming error, not a module result.
    for (const bad of [null, {}, { id: 'only-id' }, { handle: () => 1 }, { id: 4, handle: () => 1 }, { id: 'x', handle: 'no' }]) {
      await assert.rejects(hooks.dispatchHookEvent(ok, [bad]), /modules require id and handle/);
    }
    assert.equal((await hooks.dispatchHookEvent(ok, [module], { timeoutMs: 1000 })).status, 'ok');
    assert.equal((await hooks.dispatchHookEvent(ok, [module], {})).status, 'ok');
    return { optionsRefused: 7, modulesRefused: 6 };
  });

  await run('cli-usage-and-exit-codes', async () => {
    const usage = await cli([]);
    assert.equal(usage.code, 64);
    assert.equal(usage.stdout, '');
    assert.equal(usage.stderr, 'usage: rightkit-hooks <module.mjs> [module.mjs ...]\n');

    const accepted = await cli([echo], [JSON.stringify({ event: 'Stop', message: 'hello' })]);
    assert.equal(accepted.code, 0, accepted.stderr);
    assert.equal(accepted.stderr, '');
    assert.ok(accepted.stdout.endsWith('\n') && accepted.stdout.indexOf('\n') === accepted.stdout.length - 1, 'one JSON line');
    assert.deepEqual(JSON.parse(accepted.stdout), { schemaVersion: 1, event: 'Stop', status: 'ok', results: [{ id: 'echo', status: 'ok', output: 'hello' }] });

    // A module that throws is a non-ok result: exit 1, and the result is still printed.
    const failing = await cli([boom], [JSON.stringify({ event: 'Stop' })]);
    assert.equal(failing.code, 1);
    assert.deepEqual(JSON.parse(failing.stdout), { schemaVersion: 1, event: 'Stop', status: 'error', results: [{ id: 'boom', status: 'error', error: 'boom' }] });

    // Several modules run in the order given; one failure makes the whole result non-ok.
    const several = await cli([echo, boom, late], [JSON.stringify({ hook_event_name: 'PreToolUse', message: 'm' })]);
    assert.equal(several.code, 1);
    const printed = JSON.parse(several.stdout);
    assert.equal(printed.event, 'PreToolUse');
    assert.deepEqual(printed.results.map(r => [r.id, r.status]), [['echo', 'ok'], ['boom', 'error'], ['late', 'ok']]);
    assert.equal(printed.results[2].output, 'late:PreToolUse');

    // A module without an id export is named by the path it was given.
    const unnamed = await cli([noId], [JSON.stringify({ event: 'Stop' })]);
    assert.equal(unnamed.code, 0, unnamed.stderr);
    assert.deepEqual(JSON.parse(unnamed.stdout).results, [{ id: noId, status: 'ok', output: { saw: 'Stop' } }]);
    return { usage: 64, ok: 0, nonOk: 1, modulesInOrder: 3 };
  });

  await run('cli-input-boundaries-fail-closed', async () => {
    // UTF-8 split across chunks is reassembled.
    const text = Buffer.from(JSON.stringify({ event: 'Stop', message: 'hello \u{1F30D}' }));
    const split = text.indexOf(Buffer.from('\u{1F30D}')) + 2;
    const joined = await cli([echo], [text.subarray(0, split), text.subarray(split)]);
    assert.equal(joined.code, 0, joined.stderr);
    assert.equal(joined.stderr, '');
    assert.equal(JSON.parse(joined.stdout).results[0].output, 'hello \u{1F30D}');

    // Exactly 1 MiB is read; one byte more is refused before any module is imported.
    const limit = 1024 * 1024;
    const frame = Buffer.from('{"event":"Stop","pad":""}');
    const exact = Buffer.concat([Buffer.from('{"event":"Stop","pad":"'), Buffer.alloc(limit - frame.length, 'x'), Buffer.from('"}')]);
    assert.equal(exact.length, limit);
    const atLimit = await cli([echo], [exact]);
    assert.equal(atLimit.code, 0, atLimit.stderr);
    assert.equal(JSON.parse(atLimit.stdout).results[0].output, 'Stop');
    const over = await cli([broken], [Buffer.alloc(limit + 1, ' ')]);
    assert.equal(over.code, 2);
    assert.equal(over.stdout, '');
    assert.equal(over.stderr, `RIGHTKIT_HOOKS_INVALID: HookHost stdin exceeds ${limit} bytes\n`, 'refused while reading, so the broken module never loaded');

    // Invalid input is exit 2 with one tagged line on stderr and nothing on stdout.
    for (const body of ['{', '', 'null', '[]', '{"message":"no event"}', '"text"']) {
      const refused = await cli([echo], body === '' ? [] : [body]);
      assert.equal(refused.code, 2, `input ${JSON.stringify(body)}`);
      assert.equal(refused.stdout, '');
      assert.ok(refused.stderr.startsWith('RIGHTKIT_HOOKS_INVALID: ') && refused.stderr.endsWith('\n'), refused.stderr);
    }
    return { splitUtf8: true, atLimit: limit, overLimit: limit + 1, invalidInputs: 6 };
  });

  await run('cli-module-loading-failures', async () => {
    const missing = await cli([path.join(scratch, 'does-not-exist.mjs')], [JSON.stringify({ event: 'Stop' })]);
    assert.equal(missing.code, 2);
    assert.equal(missing.stdout, '');
    assert.ok(missing.stderr.startsWith('RIGHTKIT_HOOKS_INVALID: '));
    const failing = await cli([echo, broken], [JSON.stringify({ event: 'Stop' })]);
    assert.equal(failing.code, 2, 'a module that throws on import is invalid input, and nothing runs');
    assert.equal(failing.stdout, '');
    assert.match(failing.stderr, /import-time failure/);
    return { missingModule: 2, importTimeFailure: 2, outputBeforeFailure: false };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
} finally {
  if (scratch) rmSync(scratch, { recursive: true, force: true });
}
process.stdout.write(`${JSON.stringify(result)}\n`);
