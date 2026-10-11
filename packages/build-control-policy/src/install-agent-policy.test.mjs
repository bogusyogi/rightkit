import assert from 'node:assert/strict';
import { appendFileSync, cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { HOOKS } from './hook-launcher.mjs';
import { appendHook, installAgentPolicyHooks, registeredHookKind } from './install-agent-policy.mjs';
import { installPolicyAssets } from './install-policy-assets.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));

// ---- appendHook on strings: Windows-shaped paths, so the folding rules are covered on every host ----
const root = 'D:\\.rightkit-managed\\agent-policy';
const node = 'C:\\node.exe';
const policyFor = (hook, extra = {}) => ({ root, roots: [root], platform: 'win32', ...HOOKS[hook], ...extra });
const contentCommand = (digest, script, base = root) => `"${node}" "${base}\\content-${digest.repeat(64)}\\src\\${script}"`;
const launcherCommand = (hook, nodePath = node) => `"${nodePath}" "${root}\\launch\\${HOOKS[hook].launcher}"`;
const SESSION_CLI = 'ownership-session-hook-cli.mjs';
const WRITE_CLI = 'ownership-write-hook-cli.mjs';

test('a content-addressed registration is migrated to the launcher in place, keeping its position and fields', () => {
  const before = { matcher: 'Bash', hooks: [{ type: 'command', command: 'node before.mjs' }] };
  const after = { matcher: 'Bash', hooks: [{ type: 'command', command: 'node after.mjs' }] };
  const stale = { hooks: [{ type: 'command', command: contentCommand('a', SESSION_CLI), timeout: 30, statusMessage: 'RightKit' }], note: 'kept' };
  const groups = { SessionStart: [before, stale, after] };
  appendHook(groups, 'SessionStart', {}, launcherCommand('session'), policyFor('session'));
  assert.equal(groups.SessionStart.length, 3);
  assert.equal(groups.SessionStart[0], before);
  assert.equal(groups.SessionStart[2], after);
  assert.deepEqual(groups.SessionStart[1], { hooks: [{ type: 'command', command: launcherCommand('session'), timeout: 30, statusMessage: 'RightKit' }], note: 'kept' });
  assert.deepEqual(Object.keys(groups.SessionStart[1].hooks[0]), ['type', 'command', 'timeout', 'statusMessage']);
  const settled = JSON.stringify(groups);
  appendHook(groups, 'SessionStart', {}, launcherCommand('session'), policyFor('session'));
  assert.equal(JSON.stringify(groups), settled, 'idempotent');
});

test('a later policy activation produces the same registered command, so there is nothing new to review', () => {
  const groups = { SessionStart: [{ hooks: [{ type: 'command', command: contentCommand('a', SESSION_CLI) }] }] };
  appendHook(groups, 'SessionStart', {}, launcherCommand('session'), policyFor('session'));
  const registered = JSON.stringify(groups);
  // The command text no longer contains a digest, so the next activation presents the same command.
  assert.ok(!/content-/.test(registered));
  appendHook(groups, 'SessionStart', {}, launcherCommand('session'), policyFor('session'));
  assert.equal(JSON.stringify(groups), registered);
});

test('only the same hook script is migrated', () => {
  const write = { matcher: 'Write|Edit|MultiEdit', hooks: [{ type: 'command', command: contentCommand('a', WRITE_CLI) }] };
  const groups = { PreToolUse: [write] };
  appendHook(groups, 'PreToolUse', { matcher: 'Write|Edit|MultiEdit' }, launcherCommand('session'), policyFor('session'));
  assert.equal(groups.PreToolUse.length, 2);
  assert.equal(groups.PreToolUse[0], write);
  assert.equal(groups.PreToolUse[1].hooks[0].command, launcherCommand('session'));
});

test('duplicate RightKit registrations collapse to one while unrelated hooks keep their order', () => {
  const unrelated = { matcher: 'Bash', hooks: [{ type: 'command', command: 'node guard.mjs' }] };
  const groups = { PreToolUse: [
    { matcher: 'Bash', hooks: [{ type: 'command', command: contentCommand('a', WRITE_CLI) }, { type: 'command', command: 'node keep.mjs' }] },
    unrelated,
    { matcher: 'Bash', hooks: [{ type: 'command', command: launcherCommand('write', 'C:\\old\\node.exe') }] },
    { matcher: 'Bash', hooks: [{ type: 'command', command: contentCommand('b', WRITE_CLI) }] },
  ] };
  appendHook(groups, 'PreToolUse', { matcher: 'Bash' }, launcherCommand('write'), policyFor('write'));
  assert.deepEqual(groups.PreToolUse, [
    { matcher: 'Bash', hooks: [{ type: 'command', command: launcherCommand('write') }, { type: 'command', command: 'node keep.mjs' }] },
    unrelated,
  ]);
});

test('a registration under another matcher is replaced by a group with the wanted matcher', () => {
  const groups = { PreToolUse: [{ matcher: 'Write|Edit', hooks: [{ type: 'command', command: contentCommand('a', WRITE_CLI) }] }] };
  appendHook(groups, 'PreToolUse', { matcher: 'Write|Edit|Bash' }, launcherCommand('write'), policyFor('write'));
  assert.deepEqual(groups.PreToolUse, [{ matcher: 'Write|Edit|Bash', hooks: [{ type: 'command', command: launcherCommand('write') }] }]);
});

test('an event with no RightKit hook gets one group; a non-array event is rejected', () => {
  const groups = { PostToolUse: [{ hooks: [] }] };
  appendHook(groups, 'SessionStart', {}, launcherCommand('session'), policyFor('session'));
  appendHook(groups, 'SessionStart', {}, launcherCommand('session'), policyFor('session'));
  assert.deepEqual(groups, { PostToolUse: [{ hooks: [] }], SessionStart: [{ hooks: [{ type: 'command', command: launcherCommand('session') }] }] });
  assert.throws(() => appendHook({ SessionStart: {} }, 'SessionStart', {}, launcherCommand('session'), policyFor('session')), TypeError);
  assert.throws(() => appendHook({}, 'SessionStart', {}, launcherCommand('session')), TypeError);
});

test('registeredHookKind claims only this hook under a managed root', () => {
  const kind = (command, extra) => registeredHookKind(command, policyFor('write', extra));
  assert.equal(kind(contentCommand('a', WRITE_CLI)), 'content');
  assert.equal(kind(launcherCommand('write')), 'launcher');
  assert.equal(kind(`node ${root}\\launch\\${HOOKS.write.launcher}`), 'launcher');
  assert.equal(kind(contentCommand('a', SESSION_CLI)), null, 'other hook script');
  assert.equal(kind(launcherCommand('session')), null, 'other hook launcher');
  assert.equal(kind(contentCommand('a', WRITE_CLI, 'E:\\other\\agent-policy')), null, 'another root');
  assert.equal(kind(`"${node}" "${root}\\content-${'a'.repeat(63)}\\src\\${WRITE_CLI}"`), null, 'short digest');
  assert.equal(kind(`"${node}" "${root}\\content-${'A'.repeat(64)}\\src\\${WRITE_CLI}"`, { platform: 'darwin' }), null, 'uppercase digest on a case-sensitive platform');
  assert.equal(kind(`"${node}" "${root}\\content-${'a'.repeat(64)}\\src\\${WRITE_CLI}" --extra`), null, 'path is not the last token');
  assert.equal(kind(`"${node}" "x${root}\\content-${'a'.repeat(64)}\\src\\${WRITE_CLI}"`), null, 'not at a token boundary');
  assert.equal(kind('node guard.mjs'), null);
  assert.equal(kind(undefined), null);
  assert.equal(kind(contentCommand('a', WRITE_CLI).toLowerCase()), 'content', 'Windows paths ignore case');
  assert.equal(registeredHookKind(contentCommand('a', WRITE_CLI).toLowerCase(), policyFor('write', { platform: 'darwin' })), null, 'POSIX paths do not');
  assert.equal(kind(contentCommand('a', WRITE_CLI, 'E:\\old\\agent-policy'), { roots: [root, 'E:\\old\\agent-policy'] }), 'content', 'legacy managed root');
});

// ---- installAgentPolicyHooks against temporary homes and policy roots ----
async function withHome(run) {
  const base = realpathSync(mkdtempSync(path.join(tmpdir(), 'rk-agent-hooks-')));
  try { return await run(base); } finally { rmSync(base, { recursive: true, force: true }); }
}

function sourceCopy(base, name, activation) {
  const sourceRoot = path.join(base, name);
  cpSync(here, path.join(sourceRoot, 'src'), { recursive: true });
  appendFileSync(path.join(sourceRoot, 'src', 'rightkit-drift.mjs'), `\n// ${activation}\n`);
  return sourceRoot;
}

const fakeNode = '/opt/rightkit-test/node/bin/node';
const quoted = (script, nodePath = fakeNode) => `"${nodePath}" "${script}"`;
const staleScript = (policyRoot, digest, script) => path.join(policyRoot, `content-${digest.repeat(64)}`, 'src', script);
const readJson = (file) => JSON.parse(readFileSync(file, 'utf8'));

/** Settings that already hold content-addressed RightKit hooks plus hooks, groups and fields that are not RightKit's. */
function seedHosts(home, policyRoot) {
  const claudeWrite = { type: 'command', command: quoted(staleScript(policyRoot, 'c', WRITE_CLI)), timeout: 7 };
  const claudeSession = { type: 'command', command: quoted(staleScript(policyRoot, 'c', SESSION_CLI)) };
  const claude = {
    model: 'opus',
    permissions: { allow: ['Bash(ls)'], deny: [] },
    hooks: {
      PreToolUse: [
        { matcher: 'Bash', hooks: [{ type: 'command', command: 'node /membrane/hook.mjs', timeout: 5 }] },
        { matcher: 'Write|Edit|MultiEdit|Bash', hooks: [claudeWrite] },
        { matcher: 'WebFetch', hooks: [{ type: 'command', command: 'node /other/fetch-guard.mjs' }] },
      ],
      PostToolUse: [{ matcher: '*', hooks: [{ type: 'command', command: 'node /other/log.mjs' }] }],
      SessionStart: [{ hooks: [{ type: 'command', command: 'node /other/start.mjs' }, claudeSession] }],
    },
    env: { KEEP: '1' },
  };
  const codex = {
    version: 1,
    hooks: {
      PreToolUse: [
        { matcher: 'Bash|apply_patch|exec_command|functions\\.exec', hooks: [{ type: 'command', command: quoted(staleScript(policyRoot, 'c', WRITE_CLI), 'C:\\old\\node.exe') }] },
        { matcher: 'Bash', hooks: [{ type: 'command', command: 'node /other/codex-guard.mjs' }] },
      ],
      SessionStart: [{ hooks: [{ type: 'command', command: quoted(staleScript(policyRoot, 'c', SESSION_CLI)) }] }],
      Stop: [{ hooks: [{ type: 'command', command: 'node /other/stop.mjs' }] }],
    },
  };
  mkdirSync(path.join(home, '.claude'), { recursive: true });
  mkdirSync(path.join(home, '.codex'), { recursive: true });
  writeFileSync(path.join(home, '.claude', 'settings.json'), `${JSON.stringify(claude, null, 2)}\n`);
  writeFileSync(path.join(home, '.codex', 'hooks.json'), `${JSON.stringify(codex, null, 2)}\n`);
  return { claude, codex };
}

const hostFiles = (home) => ({ claude: path.join(home, '.claude', 'settings.json'), codex: path.join(home, '.codex', 'hooks.json') });
const install = (home, policyRoot, sourceRoot) => installAgentPolicyHooks({ home, root: policyRoot, sourceRoot, nodePath: fakeNode });

test('install migrates content-addressed registrations to the launcher and leaves everything else as it was', async () => {
  await withHome(async (base) => {
    const home = path.join(base, 'home');
    const policyRoot = path.join(base, 'agent-policy');
    const seeded = seedHosts(home, policyRoot);
    const result = await install(home, policyRoot, sourceCopy(base, 'one', 'activation one'));
    const writeCommand = quoted(result.writeLauncher);
    const sessionCommand = quoted(result.sessionLauncher);
    assert.equal(result.claudeChanged, true);
    assert.equal(result.codexChanged, true);

    const expectedClaude = structuredClone(seeded.claude);
    expectedClaude.hooks.PreToolUse[1].hooks[0].command = writeCommand;
    expectedClaude.hooks.SessionStart[0].hooks[1].command = sessionCommand;
    const expectedCodex = structuredClone(seeded.codex);
    expectedCodex.hooks.PreToolUse[0].hooks[0].command = writeCommand;
    expectedCodex.hooks.SessionStart[0].hooks[0].command = sessionCommand;
    const files = hostFiles(home);
    // Same keys, same order, same values everywhere except the four RightKit commands; no group added or removed.
    assert.equal(readFileSync(files.claude, 'utf8'), `${JSON.stringify(expectedClaude, null, 2)}\n`);
    assert.equal(readFileSync(files.codex, 'utf8'), `${JSON.stringify(expectedCodex, null, 2)}\n`);
    for (const file of [files.claude, files.codex]) assert.ok(!readFileSync(file, 'utf8').includes('content-'));
  });
});

test('activating a different policy digest changes no hook command and rewrites no host config', async () => {
  await withHome(async (base) => {
    const home = path.join(base, 'home');
    const policyRoot = path.join(base, 'agent-policy');
    seedHosts(home, policyRoot);
    const files = hostFiles(home);
    const first = await install(home, policyRoot, sourceCopy(base, 'one', 'activation one'));
    // A hand-formatted config must survive a no-op install untouched, not be reformatted.
    for (const file of [files.claude, files.codex]) writeFileSync(file, `${JSON.stringify(readJson(file), null, 4)}\n`);
    const snapshot = Object.fromEntries(Object.entries(files).map(([name, file]) => [name, { bytes: readFileSync(file), ino: statSync(file).ino }]));
    const launcherBytes = [first.writeLauncher, first.sessionLauncher].map((file) => readFileSync(file));

    const second = await install(home, policyRoot, sourceCopy(base, 'two', 'activation two'));
    assert.notEqual(second.digest, first.digest);
    assert.equal(second.writeLauncher, first.writeLauncher);
    assert.equal(second.claudeChanged, false);
    assert.equal(second.codexChanged, false);
    for (const [name, file] of Object.entries(files)) {
      assert.ok(readFileSync(file).equals(snapshot[name].bytes), `${name} config bytes changed`);
      assert.equal(statSync(file).ino, snapshot[name].ino, `${name} config was rewritten`);
    }
    assert.deepEqual([second.writeLauncher, second.sessionLauncher].map((file) => readFileSync(file)), launcherBytes);
    assert.deepEqual(readJson(path.join(policyRoot, 'active.json')), { schemaVersion: 1, digest: second.digest });
  });
});

test('two installs of different digests register byte-identical commands and launchers with different pointers', async () => {
  await withHome(async (base) => {
    const policyRoot = path.join(base, 'agent-policy');
    const runs = [];
    for (const name of ['alpha', 'beta']) {
      const home = path.join(base, `${name}-home`);
      seedHosts(home, policyRoot);
      const result = await install(home, policyRoot, sourceCopy(base, name, `activation ${name}`));
      const files = hostFiles(home);
      runs.push({
        result,
        claude: readFileSync(files.claude, 'utf8'),
        codex: readFileSync(files.codex, 'utf8'),
        launchers: [result.writeLauncher, result.sessionLauncher].map((file) => readFileSync(file)),
        pointer: readFileSync(path.join(policyRoot, 'active.json'), 'utf8'),
      });
    }
    const [one, two] = runs;
    assert.notEqual(one.result.digest, two.result.digest);
    assert.notEqual(one.pointer, two.pointer);
    assert.equal(one.claude, two.claude);
    assert.equal(one.codex, two.codex);
    assert.deepEqual(one.launchers, two.launchers);
    assert.ok(!one.claude.includes(one.result.digest) && !one.claude.includes(two.result.digest));
    assert.ok(!one.codex.includes(one.result.digest) && !one.codex.includes(two.result.digest));
    // A third, independent root with yet another digest gets the same launcher bytes: they depend on no digest and no root.
    const separate = await installPolicyAssets({ root: path.join(base, 'separate-policy'), sourceRoot: sourceCopy(base, 'gamma', 'activation gamma') });
    assert.deepEqual([separate.writeLauncher, separate.sessionLauncher].map((file) => readFileSync(file)), one.launchers);
  });
});

test('install on configs without RightKit hooks adds one group per hook and does not duplicate on repeat', async () => {
  await withHome(async (base) => {
    const home = path.join(base, 'home');
    const policyRoot = path.join(base, 'agent-policy');
    const files = hostFiles(home);
    mkdirSync(path.join(home, '.claude'), { recursive: true });
    mkdirSync(path.join(home, '.codex'), { recursive: true });
    writeFileSync(files.claude, `${JSON.stringify({ hooks: {} })}\n`);
    writeFileSync(files.codex, `${JSON.stringify({ hooks: { PreToolUse: [{ matcher: 'Bash', hooks: [{ type: 'command', command: 'node /other/guard.mjs' }] }] } })}\n`);
    const sourceRoot = sourceCopy(base, 'one', 'activation one');
    const first = await install(home, policyRoot, sourceRoot);
    const again = await install(home, policyRoot, sourceRoot);
    assert.equal(again.claudeChanged, false);
    assert.equal(again.codexChanged, false);
    assert.deepEqual(readJson(files.claude).hooks, {
      PreToolUse: [{ matcher: 'Write|Edit|MultiEdit|Bash', hooks: [{ type: 'command', command: quoted(first.writeLauncher) }] }],
      SessionStart: [{ hooks: [{ type: 'command', command: quoted(first.sessionLauncher) }] }],
    });
    assert.deepEqual(readJson(files.codex).hooks, {
      PreToolUse: [
        { matcher: 'Bash', hooks: [{ type: 'command', command: 'node /other/guard.mjs' }] },
        { matcher: 'Bash|apply_patch|exec_command|functions\\.exec', hooks: [{ type: 'command', command: quoted(first.writeLauncher) }] },
      ],
      SessionStart: [{ hooks: [{ type: 'command', command: quoted(first.sessionLauncher) }] }],
    });
  });
});

test('install changes nothing in the host configs when the policy assets cannot be installed', async () => {
  await withHome(async (base) => {
    const home = path.join(base, 'home');
    const policyRoot = path.join(base, 'agent-policy');
    seedHosts(home, policyRoot);
    const files = hostFiles(home);
    const before = [files.claude, files.codex].map((file) => readFileSync(file));
    const broken = sourceCopy(base, 'broken', 'activation broken');
    rmSync(path.join(broken, 'src', 'ownership-source.mjs'));
    await assert.rejects(install(home, policyRoot, broken), { code: 'ENOENT' });
    assert.deepEqual([files.claude, files.codex].map((file) => readFileSync(file)), before);
  });
});
