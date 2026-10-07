import assert from 'node:assert/strict';
import test from 'node:test';
import { appendHook } from './install-agent-policy.mjs';

const root = 'D:\\.rightkit-managed\\agent-policy';
const command = (digest, script) => `"C:\\node.exe" "${root}\\content-${digest.repeat(64)}\\src\\${script}"`;

test('reinstalling policy hooks replaces the superseded content root instead of running both', () => {
  const unrelated = { matcher: 'Bash', hooks: [{ type: 'command', command: 'node guard.mjs' }] };
  const groups = { SessionStart: [unrelated, { hooks: [{ type: 'command', command: command('a', 'ownership-session-hook-cli.mjs') }] }] };
  appendHook(groups, 'SessionStart', {}, command('b', 'ownership-session-hook-cli.mjs'));
  assert.deepEqual(groups.SessionStart, [unrelated, { hooks: [{ type: 'command', command: command('b', 'ownership-session-hook-cli.mjs') }] }]);
  appendHook(groups, 'SessionStart', {}, command('b', 'ownership-session-hook-cli.mjs'));
  assert.equal(groups.SessionStart.length, 2, 'idempotent');
});

test('only the same CLI script from a content root is replaced', () => {
  const write = { matcher: 'Write|Edit|MultiEdit', hooks: [{ type: 'command', command: command('a', 'ownership-write-hook-cli.mjs') }] };
  const groups = { PreToolUse: [write] };
  appendHook(groups, 'PreToolUse', { matcher: 'Write|Edit|MultiEdit' }, command('b', 'ownership-session-hook-cli.mjs'));
  assert.equal(groups.PreToolUse.length, 2);
  assert.equal(groups.PreToolUse[0].hooks[0].command, command('a', 'ownership-write-hook-cli.mjs'));
});
