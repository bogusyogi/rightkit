import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import test from 'node:test';
import { ownershipWriteDecision, fingerprintDecision } from './ownership-write-hook.mjs';
import { ownershipLookup } from '../../build-control-clients/src/ownership-lookup.mjs';
import { ownershipSessionDecision } from './ownership-session-hook.mjs';
import { installPolicyAssets } from './install-policy-assets.mjs';

const canonical = new URL('../../release/rightkit-ownership.json', import.meta.url);
const snapshot = new URL('./ownership-fingerprints.json', import.meta.url);
const sessionCatalog = new URL('./ownership-session-catalog.json', import.meta.url);
const github = 'client.post("https://api.anthropic.com/v1/messages");';
const clean = 'export function signIn() { return provider.login(); }';

test('packaged fingerprint catalog matches canonical release catalog', () => {
  assert.deepEqual(JSON.parse(readFileSync(snapshot)), JSON.parse(readFileSync(canonical)));
  assert.deepEqual(JSON.parse(readFileSync(sessionCatalog)), JSON.parse(readFileSync(canonical)));
});

test('session start returns lookup compact index verbatim as additional context', () => {
  const result = ownershipSessionDecision();
  assert.equal(result.hookSpecificOutput.hookEventName, 'SessionStart');
  assert.equal(result.hookSpecificOutput.additionalContext, ownershipLookup('--compact'));
});

test('copied SessionStart hook runs from package assets without source-tree siblings', async () => {
  const root = mkdtempSync(path.join(tmpdir(), 'rk-policy-copy-'));
  const copy = path.join(root, 'build-control-policy', 'src');
  try {
    const source = path.dirname(fileURLToPath(import.meta.url));
    const destination = path.join(root, 'build-control-policy');
    mkdirSync(destination, { recursive: true });
    cpSync(source, path.join(destination, 'src'), { recursive: true });
    const imported = await import(`${pathToFileURL(path.join(copy, 'ownership-session-hook.mjs')).href}?isolated=1`);
    const result = imported.ownershipSessionDecision();
    assert.equal(result.hookSpecificOutput.hookEventName, 'SessionStart');
    assert.equal(result.hookSpecificOutput.additionalContext, ownershipLookup('--compact'));
    assert.ok(readFileSync(path.join(copy, 'ownership-session-versions.json')));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('generated policy asset closure includes availability helper & runs standalone', async () => {
  const root = mkdtempSync(path.join(tmpdir(), 'rk-policy-assets-'));
  try {
    const installed = await installPolicyAssets({ root });
    const helper = path.join(installed.installRoot, 'src/ownership-availability.mjs');
    assert.ok(readFileSync(helper));
    const manifest = JSON.parse(readFileSync(path.join(installed.installRoot, 'manifest.json')));
    assert.ok(manifest.files.some((entry) => entry.name === 'ownership-availability.mjs'));
    const imported = await import(pathToFileURL(path.join(installed.installRoot, 'src/ownership-session-hook.mjs')).href);
    assert.equal(imported.ownershipSessionDecision().hookSpecificOutput.additionalContext, ownershipLookup('--compact'));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('Claude Write and Edit reconstructed post-state advise on newly introduced exact source fingerprints', () => {
  const readFile = () => clean;
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/login.ts', content: github } }, { readFile }).hookSpecificOutput.permissionDecision, 'allow');
  assert.equal(ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'src/login.ts', old_string: 'provider.login()', new_string: github } }, { readFile }).hookSpecificOutput.permissionDecision, 'allow');
});

test('Claude subagent PreToolUse event advises on same fingerprint', () => {
  const decision = ownershipWriteDecision({ hook_event_name: 'PreToolUse', session_id: 'sess-1', parent_tool_use_id: 'toolu-parent', tool_name: 'Write', tool_input: { file_path: 'src/signin.rs', content: 'client.post("https://api.anthropic.com/v1/messages");' } }, { readFile: () => clean });
  assert.equal(decision.hookSpecificOutput.permissionDecision, 'allow');
});

test('duplicate-removing edits and deletions are allowed', () => {
  const readFile = () => `${github}\n${github}`;
  assert.equal(ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'src/login.ts', old_string: `${github}\n`, new_string: '' } }, { readFile }).decision, 'allow');
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/login.ts', content: clean } }, { readFile }).decision, 'allow');
});

test('ambiguous Edit reconstruction fails open for ownership feedback', () => {
  const readFile = () => 'same same';
  assert.equal(ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'src/login.ts', old_string: 'same', new_string: github } }, { readFile }).decision, 'allow');
});

test('Codex apply_patch parses embedded paths, multiple files, and protected rename targets', () => {
  const readFile = () => clean;
  const patch = `*** Begin Patch\n*** Update File: src/login.rs\n@@\n-${clean}\n+${github}\n*** Update File: src/other.rs\n@@\n-old\n+new\n*** End Patch`;
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch } }, { readFile }).hookSpecificOutput.permissionDecision, 'allow');
  const protectedPatch = '*** Begin Patch\n*** Update File: .rightkit/old.json\n*** Move to: .rightkit/ownership-exceptions.json\n@@\n-a\n+b\n*** End Patch';
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: protectedPatch } }).hookSpecificOutput.permissionDecision, 'deny');
});

test('Codex apply_patch reconstructs standard Add File bodies and mid-file insertion hunks', () => {
  const addFile = `*** Begin Patch\n*** Add File: src/auth.rs\n+pub fn auth() {\n+    client.post("https://api.anthropic.com/v1/messages");\n+}\n*** End Patch`;
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { input: addFile } }, { readFile: () => { const error = new Error('missing'); error.code = 'ENOENT'; throw error; } }).hookSpecificOutput.permissionDecision, 'allow');
  const original = 'fn initialize() {\n    let start = 1;\n    finish();\n}';
  const midFile = `*** Begin Patch\n*** Update File: src/auth.rs\n@@ fn initialize()\n     let start = 1;\n+    client.post("https://api.anthropic.com/v1/messages");\n     finish();\n*** End Patch`;
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: midFile } }, { readFile: () => original }).hookSpecificOutput.permissionDecision, 'allow');
});

test('unparsable Codex source patch containing a known advisory fingerprint reports advice', () => {
  const malformed = '*** Add File: src/signin.rs\n+ client.post("https://api.anthropic.com/v1/messages");';
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: malformed } }).hookSpecificOutput.permissionDecision, 'allow');
});

test('comments and documentation do not count as source implementations', () => {
  const comment = '// https://github.com/login/device/code';
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/auth.rs', content: comment } }, { readFile: () => clean }).decision, 'allow');
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'docs/auth.md', content: github } }, { readFile: () => '' }).decision, 'allow');
});

test('provider requests honor code-only and extension scope in installed write hooks', () => {
  const decide = (file, content) => ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: file, content } }, { readFile: () => '' });
  const call = 'client.post("https://api.anthropic.com/v1/messages");';
  assert.equal(decide('src/provider.rs', call).hookSpecificOutput.permissionDecision, 'allow');
  assert.equal(decide('src/provider.rs', `let example = r#"${call}"#;`).decision, 'allow');
  assert.equal(decide('src/provider.ts', `const example = '${call}';`).decision, 'allow');
  assert.equal(decide('src/provider.rs', `// ${call}`).decision, 'allow');
  assert.equal(decide('src/provider.py', call).decision, 'allow');
});

test('normalized Claude paths and shell write targets protect exception records', () => {
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', cwd: 'C:/repo', tool_input: { file_path: '.\\.rightkit\\ownership-exceptions.json', content: '{}' } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(ownershipWriteDecision({ tool_name: 'exec_command', cwd: 'C:/repo', tool_input: { cmd: 'Set-Content -Path .rightkit/ownership-exceptions.json -Value "{}"' } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(ownershipWriteDecision({ tool_name: 'exec_command', cwd: 'C:/repo', tool_input: { cmd: 'Get-Content .rightkit/ownership-exceptions.json' } }).decision, 'allow');
  assert.equal(ownershipWriteDecision({ tool_name: 'functions.exec', cwd: 'C:/repo', tool_input: { code: "await tools.exec_command({cmd: 'Set-Content .rightkit/ownership-exceptions.json {}'})" } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'C:/ProgramData/Orthic Labs/RightKit/ownership/trust.json', content: '{}' } }).hookSpecificOutput.permissionDecision, 'deny');
});

test('session start in a repository behind RightKit appends its drift and the adopt command', async () => {
  const { mkdtemp, writeFile, rm } = await import('node:fs/promises'); const { tmpdir } = await import('node:os'); const { execFileSync } = await import('node:child_process');
  const repo = await mkdtemp(path.join(tmpdir(), 'rk-session-drift-')); try {
    await writeFile(path.join(repo, 'package.json'), JSON.stringify({ dependencies: { '@rightkit/release': '0.0.1' } }));
    execFileSync('git', ['-C', repo, 'init'], { stdio: 'ignore' }); execFileSync('git', ['-C', repo, 'add', '.'], { stdio: 'ignore' });
    const context = ownershipSessionDecision({ cwd: repo }).hookSpecificOutput.additionalContext;
    assert.ok(context.startsWith(ownershipLookup('--compact')));
    assert.match(context, /@rightkit\/release 0\.0\.1 -> \d+\.\d+\.\d+/);
    assert.match(context, /rightkit-drift\.mjs" --apply/);
    assert.equal(ownershipSessionDecision({ cwd: tmpdir() }).hookSpecificOutput.additionalContext.includes('RightKit updates'), false);
  } finally { await rm(repo, { recursive: true, force: true }); }
});


test('fingerprint tier governs hook feedback; protected records remain denied', () => {
  assert.equal(fingerprintDecision({ tier: 'block' }, 'candidate').hookSpecificOutput.permissionDecision, 'deny');
  const decision = fingerprintDecision({ tier: 'advise' }, 'candidate');
  assert.equal(decision.hookSpecificOutput.permissionDecision, 'allow');
  assert.match(decision.hookSpecificOutput.additionalContext, /ownership:advise/);
  const literal = ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/auth.ts', content: 'const url="https://github.com/login/device/code";' } }, { readFile: () => '' });
  assert.deepEqual(literal, { decision: 'allow' });
});
