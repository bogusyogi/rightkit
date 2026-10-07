import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import test from 'node:test';
import { ownershipWriteDecision } from './ownership-write-hook.mjs';
import { ownershipLookup } from '../../build-control-clients/src/ownership-lookup.mjs';
import { ownershipSessionDecision } from './ownership-session-hook.mjs';

const canonical = new URL('../../release/rightkit-ownership.json', import.meta.url);
const snapshot = new URL('./ownership-fingerprints.json', import.meta.url);
const sessionCatalog = new URL('./ownership-session-catalog.json', import.meta.url);
const github = 'const url = "https://github.com/login/device/code";';
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

test('Claude Write and Edit reconstructed post-state block newly introduced exact source fingerprints', () => {
  const readFile = () => clean;
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/login.ts', content: github } }, { readFile }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'src/login.ts', old_string: 'provider.login()', new_string: github } }, { readFile }).hookSpecificOutput.permissionDecision, 'deny');
});

test('Claude subagent PreToolUse event blocks the same protected write payload', () => {
  const decision = ownershipWriteDecision({ hook_event_name: 'PreToolUse', session_id: 'sess-1', parent_tool_use_id: 'toolu-parent', tool_name: 'Write', tool_input: { file_path: 'src/signin.rs', content: 'let endpoint = "https://github.com/login/oauth/authorize";' } }, { readFile: () => clean });
  assert.equal(decision.hookSpecificOutput.permissionDecision, 'deny');
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
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch } }, { readFile }).hookSpecificOutput.permissionDecision, 'deny');
  const protectedPatch = '*** Begin Patch\n*** Update File: .rightkit/old.json\n*** Move to: .rightkit/ownership-exceptions.json\n@@\n-a\n+b\n*** End Patch';
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: protectedPatch } }).hookSpecificOutput.permissionDecision, 'deny');
});

test('Codex apply_patch reconstructs standard Add File bodies and mid-file insertion hunks', () => {
  const addFile = `*** Begin Patch\n*** Add File: src/auth.rs\n+pub fn auth() {\n+    let endpoint = "https://github.com/login/device/code";\n+}\n*** End Patch`;
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { input: addFile } }, { readFile: () => { const error = new Error('missing'); error.code = 'ENOENT'; throw error; } }).hookSpecificOutput.permissionDecision, 'deny');
  const original = 'fn initialize() {\n    let start = 1;\n    finish();\n}';
  const midFile = `*** Begin Patch\n*** Update File: src/auth.rs\n@@ fn initialize()\n     let start = 1;\n+    let endpoint = "https://github.com/login/device/code";\n     finish();\n*** End Patch`;
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: midFile } }, { readFile: () => original }).hookSpecificOutput.permissionDecision, 'deny');
});

test('unparsable Codex source patch containing a known fingerprint blocks', () => {
  const malformed = '*** Add File: src/signin.rs\n+ let endpoint = "https://github.com/login/device/code";';
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: malformed } }).hookSpecificOutput.permissionDecision, 'deny');
});

test('comments and documentation do not count as source implementations', () => {
  const comment = '// https://github.com/login/device/code';
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/auth.rs', content: comment } }, { readFile: () => clean }).decision, 'allow');
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'docs/auth.md', content: github } }, { readFile: () => '' }).decision, 'allow');
});

test('normalized Claude paths and shell write targets protect exception records', () => {
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', cwd: 'C:/repo', tool_input: { file_path: '.\\.rightkit\\ownership-exceptions.json', content: '{}' } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(ownershipWriteDecision({ tool_name: 'exec_command', cwd: 'C:/repo', tool_input: { cmd: 'Set-Content -Path .rightkit/ownership-exceptions.json -Value "{}"' } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(ownershipWriteDecision({ tool_name: 'exec_command', cwd: 'C:/repo', tool_input: { cmd: 'Get-Content .rightkit/ownership-exceptions.json' } }).decision, 'allow');
  assert.equal(ownershipWriteDecision({ tool_name: 'functions.exec', cwd: 'C:/repo', tool_input: { code: "await tools.exec_command({cmd: 'Set-Content .rightkit/ownership-exceptions.json {}'})" } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'C:/ProgramData/Orthic Labs/RightKit/ownership/trust.json', content: '{}' } }).hookSpecificOutput.permissionDecision, 'deny');
});
