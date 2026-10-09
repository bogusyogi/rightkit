import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import test from 'node:test';
import { ownershipWriteDecision, fingerprintDecision, setScopeDependencies, canonicalRepositoryIdentity } from './ownership-write-hook.mjs';
import { renderSessionCatalog } from './ownership-session-hook.mjs';
import { ownershipSessionDecision } from './ownership-session-hook.mjs';
import { installPolicyAssets } from './install-policy-assets.mjs';

const canonical = new URL('../../release/rightkit-ownership.json', import.meta.url);
const snapshot = new URL('./ownership-fingerprints.json', import.meta.url);
const sessionCatalog = new URL('./ownership-session-catalog.json', import.meta.url);
// Existing suites run in temp dirs; treat every path as inside a registered product repository.
const registeredGit = (args) => args.includes('--show-toplevel') ? '/work/app' : 'git@github.com:BogusYogi/MailRight.git';
setScopeDependencies({ git: registeredGit, exists: () => true, roots: [] });
const github = 'client.post("https://api.anthropic.com/v1/messages");';
const clean = 'export function signIn() { return provider.login(); }';

test('packaged fingerprint catalog matches canonical release catalog', () => {
  assert.deepEqual(JSON.parse(readFileSync(snapshot)), JSON.parse(readFileSync(canonical)));
  assert.deepEqual(JSON.parse(readFileSync(sessionCatalog)), JSON.parse(readFileSync(canonical)));
});

test('session start returns lookup compact index verbatim as additional context', () => {
  const result = ownershipSessionDecision();
  assert.equal(result.hookSpecificOutput.hookEventName, 'SessionStart');
  assert.equal(result.hookSpecificOutput.additionalContext, renderSessionCatalog());
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
    assert.equal(result.hookSpecificOutput.additionalContext, renderSessionCatalog());
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
    assert.equal(imported.ownershipSessionDecision().hookSpecificOutput.additionalContext, renderSessionCatalog());
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('Claude Write and Edit reconstructed post-state advise on newly introduced exact source fingerprints', () => {
  const readFile = () => clean;
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/login.ts', content: github } }, { readFile }).hookSpecificOutput.permissionDecision, undefined);
  assert.equal(ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'src/login.ts', old_string: 'provider.login()', new_string: github } }, { readFile }).hookSpecificOutput.permissionDecision, undefined);
});

test('Claude subagent PreToolUse event advises on same fingerprint', () => {
  const decision = ownershipWriteDecision({ hook_event_name: 'PreToolUse', session_id: 'sess-1', parent_tool_use_id: 'toolu-parent', tool_name: 'Write', tool_input: { file_path: 'src/signin.rs', content: 'client.post("https://api.anthropic.com/v1/messages");' } }, { readFile: () => clean });
  assert.equal(decision.hookSpecificOutput.permissionDecision, undefined);
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
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch } }, { readFile }).hookSpecificOutput.permissionDecision, undefined);
  const protectedPatch = '*** Begin Patch\n*** Update File: .rightkit/old.json\n*** Move to: .rightkit/ownership-exceptions.json\n@@\n-a\n+b\n*** End Patch';
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: protectedPatch } }).hookSpecificOutput.permissionDecision, 'deny');
});

test('Codex apply_patch reconstructs standard Add File bodies and mid-file insertion hunks', () => {
  const addFile = `*** Begin Patch\n*** Add File: src/auth.rs\n+pub fn auth() {\n+    client.post("https://api.anthropic.com/v1/messages");\n+}\n*** End Patch`;
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { input: addFile } }, { readFile: () => { const error = new Error('missing'); error.code = 'ENOENT'; throw error; } }).hookSpecificOutput.permissionDecision, undefined);
  const original = 'fn initialize() {\n    let start = 1;\n    finish();\n}';
  const midFile = `*** Begin Patch\n*** Update File: src/auth.rs\n@@ fn initialize()\n     let start = 1;\n+    client.post("https://api.anthropic.com/v1/messages");\n     finish();\n*** End Patch`;
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: midFile } }, { readFile: () => original }).hookSpecificOutput.permissionDecision, undefined);
});

test('unparsable Codex source patch containing a known advisory fingerprint reports advice', () => {
  const malformed = '*** Add File: src/signin.rs\n+ client.post("https://api.anthropic.com/v1/messages");';
  assert.equal(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: malformed } }).hookSpecificOutput.permissionDecision, undefined);
});

test('comments and documentation do not count as source implementations', () => {
  const comment = '// https://github.com/login/device/code';
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/auth.rs', content: comment } }, { readFile: () => clean }).decision, 'allow');
  assert.equal(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'docs/auth.md', content: github } }, { readFile: () => '' }).decision, 'allow');
});

test('provider requests honor code-only and extension scope in installed write hooks', () => {
  const decide = (file, content) => ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: file, content } }, { readFile: () => '' });
  const call = 'client.post("https://api.anthropic.com/v1/messages");';
  assert.equal(decide('src/provider.rs', call).hookSpecificOutput.permissionDecision, undefined);
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
    assert.ok(context.startsWith(renderSessionCatalog()));
    assert.match(context, /@rightkit\/release 0\.0\.1 -> \d+\.\d+\.\d+/);
    assert.match(context, /rightkit-drift\.mjs" --apply/);
    assert.equal(ownershipSessionDecision({ cwd: tmpdir() }).hookSpecificOutput.additionalContext.includes('RightKit updates'), false);
  } finally { await rm(repo, { recursive: true, force: true }); }
});


test('fingerprint tier governs hook feedback; protected records remain denied', () => {
  assert.equal(fingerprintDecision({ tier: 'block' }, 'candidate').hookSpecificOutput.permissionDecision, 'deny');
  const decision = fingerprintDecision({ tier: 'advise' }, 'candidate');
  assert.equal(decision.hookSpecificOutput.permissionDecision, undefined);
  assert.match(decision.hookSpecificOutput.additionalContext, /ownership:advise/);
  const literal = ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: 'src/auth.ts', content: 'const url="https://github.com/login/device/code";' } }, { readFile: () => '' });
  assert.deepEqual(literal, { decision: 'allow' });
});

// Absolute on every host: a drive-letter path is relative on POSIX and would resolve under the process cwd (inside the RightKit checkout).
const APPS_ROOT = process.platform === 'win32' ? 'D:/Claude/apps' : '/work/apps';
const missingManifest = () => { const error = new Error('missing'); error.code = 'ENOENT'; throw error; };
const manifestWrite = (file, content, before = '', options = {}) => ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: file, content } }, { readFile: () => before, ...options });
const assertAdvice = (result, pattern) => {
  assert.equal(result.decision, 'allow');
  assert.equal(result.hookSpecificOutput.permissionDecision, undefined);
  assert.match(result.hookSpecificOutput.additionalContext, pattern);
};

test('package.json Edit adding blocked dependency denies & names capability, owner & signed exception', () => {
  const result = ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'package.json', old_string: '"dependencies": {}', new_string: '"dependencies": {"puppeteer":"24"}' } }, { readFile: () => '{"dependencies": {}}' });
  assert.equal(result.hookSpecificOutput.permissionDecision, 'deny');
  assert.match(result.hookSpecificOutput.permissionDecisionReason, /browser-automation.*rightkit-browser/);
  assert.match(result.hookSpecificOutput.permissionDecisionReason, /Use rightkit-browser instead or request a signed exception/);
});

test('Cargo.toml Edit adding reqwest advises with RightKit owner', () => {
  const result = ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'Cargo.toml', old_string: '[dependencies]', new_string: '[dependencies]\nreqwest = "0.12"' } }, { readFile: () => '[package]\nname = "app"\n[dependencies]\n' });
  assertAdvice(result, /reqwest.*http-client.*rightkit-http/);
});

test('existing dependencies & version changes are never re-flagged across dependency sections', () => {
  assert.deepEqual(manifestWrite('package.json', '{"devDependencies":{"puppeteer":"25"}}', '{"dependencies":{"puppeteer":"24"}}'), { decision: 'allow' });
  assert.deepEqual(manifestWrite('Cargo.toml', '[dev-dependencies]\nr = { package = "reqwest", version = "0.13" }', '[dependencies]\nreqwest = "0.12"'), { decision: 'allow' });
});

test('dependency-add commands respect tiers, chains, flags, renames, versions & scoped npm aliases', () => {
  const cases = [
    ['Bash', 'cargo add rusqlite --features bundled', 'allow', /rusqlite.*rightkit-db/],
    ['exec_command', 'pnpm add -D playwright', 'deny', /playwright.*rightkit-browser/],
    ['Bash', 'cd app && npm i puppeteer-core', 'deny', /puppeteer-core.*rightkit-browser/],
    ['Shell', 'echo ready; cargo add --package engine --rename browser chromiumoxide@0.7', 'allow', /chromiumoxide.*rightkit-browser/],
    ['Bash', 'cargo add -p engine --features bundled rusqlite@0.32', 'allow', /rusqlite.*rightkit-db/],
    ['shell', 'npm install --registry https://registry.npmjs.org browser@npm:puppeteer@24', 'deny', /puppeteer.*rightkit-browser/],
    ['Bash', 'yarn add "@playwright/test@1.50"', 'deny', /@playwright\/test.*rightkit-browser/],
    ['Bash', 'pnpm install playwright-core@1.50', 'deny', /playwright-core.*rightkit-browser/],
    ['Bash', 'npm add puppeteer@24', 'deny', /puppeteer.*rightkit-browser/],
    ['Bash', 'cargo add reqwest && pnpm add playwright', 'deny', /playwright.*rightkit-browser/],
  ];
  for (const [tool, command, decision, pattern] of cases) {
    const result = ownershipWriteDecision({ tool_name: tool, tool_input: { command, cmd: command } });
    assert.equal(result.hookSpecificOutput.permissionDecision, decision === 'allow' ? undefined : decision, command);
    assert.match(result.hookSpecificOutput.permissionDecisionReason ?? result.hookSpecificOutput.additionalContext, pattern);
  }
});

test('dependency command extraction ignores option values, installed-only commands, quoted echoes & comments', () => {
  for (const command of ['cargo add --rename chromiumoxide serde', 'cargo add --features chromiumoxide serde', 'cargo add -p chromiumoxide serde',
    'pnpm add --filter playwright lodash', 'npm install --registry puppeteer lodash', 'npm install', 'pnpm install', 'npm run puppeteer',
    'echo "npm i puppeteer"', 'echo done # npm i puppeteer', 'echo "ready && npm i puppeteer"']) {
    assert.deepEqual(ownershipWriteDecision({ tool_name: 'Bash', tool_input: { command } }), { decision: 'allow' }, command);
  }
});

test('Cargo dependencies include renames, target tables, dev/build dependencies & dependency subtables', () => {
  for (const declaration of [
    '[dependencies]\nfoo = { package = "chromiumoxide" }',
    '[dev-dependencies]\nreqwest = "0.12"',
    '[build-dependencies]\nreqwest = { version = "0.12", features = ["json"] }',
    '[target.\'cfg(windows)\'.dependencies]\nfoo = { package = "chromiumoxide", version = "0.7" }',
    '[target."cfg(unix)".build-dependencies.reqwest]\nversion = "0.12"',
    '[dependencies.foo]\nversion = "0.7"\npackage = "chromiumoxide"',
    '[workspace.dependencies]\nfoo = {\npackage = "chromiumoxide",\nversion = "0.7"\n}',
  ]) assertAdvice(manifestWrite('Cargo.toml', declaration, '[package]\nname = "app"'), /chromiumoxide|reqwest/);
});

test('all npm dependency sections & npm aliases are screened; dependency names require exact matches', () => {
  for (const section of ['dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies']) {
    const result = manifestWrite('package.json', JSON.stringify({ [section]: { browser: 'npm:puppeteer@24' } }), '{}');
    assert.equal(result.hookSpecificOutput.permissionDecision, 'deny');
  }
  assert.equal(manifestWrite('package.json', '{"dependencies":{"puppeteer-helper":"1"}}', '{}').decision, 'allow');
  assert.deepEqual(manifestWrite('package.json', '{"dependencies":{"browser":"npm:puppeteer@25"}}', '{"dependencies":{"puppeteer":"24"}}'), { decision: 'allow' });
});

test('MultiEdit reconstructs complete manifest & apply_patch screens each file before advisory feedback', () => {
  const result = ownershipWriteDecision({ tool_name: 'MultiEdit', tool_input: { file_path: 'package.json', edits: [
    { old_string: '"app"', new_string: '"renamed"' }, { old_string: '"dependencies":{}', new_string: '"dependencies":{"playwright":"1"}' },
  ] } }, { readFile: () => '{"name":"app","dependencies":{}}' });
  assert.equal(result.hookSpecificOutput.permissionDecision, 'deny');
  const patch = '*** Begin Patch\n*** Update File: Cargo.toml\n@@\n [dependencies]\n+reqwest = "0.12"\n*** Update File: package.json\n@@\n-{}\n+{"dependencies":{"puppeteer":"24"}}\n*** End Patch';
  const decision = ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch } }, { readFile: (file) => file.endsWith('Cargo.toml') ? '[dependencies]\n' : '{}' });
  assert.equal(decision.hookSpecificOutput.permissionDecision, 'deny');
});

test('new-manifest oauth keyword advises only when owner dependency is absent', () => {
  const options = { readFile: missingManifest, cwd: APPS_ROOT + '/acme' };
  assertAdvice(manifestWrite('package.json', '{"name":"acme-oauth-client"}', '', options), /oauth-sign-in.*rightkit-oauth/);
  assert.doesNotMatch(manifestWrite('package.json', '{"name":"acme-oauth-client","dependencies":{"rightkit-oauth":"1"}}', '', options).hookSpecificOutput?.additionalContext ?? '', /rightkit-oauth/);
  assert.doesNotMatch(manifestWrite('package.json', '{"name":"acme-oauth-client","devDependencies":{"owner":"npm:rightkit-oauth@1"}}', '', options).hookSpecificOutput?.additionalContext ?? '', /rightkit-oauth/);
  assertAdvice(manifestWrite('Cargo.toml', '[package]\nname = "acme-oauth-client"', '', options), /rightkit-oauth/);
  assert.doesNotMatch(manifestWrite('Cargo.toml', '[package]\nname = "acme-oauth-client"\n[dependencies]\nowner = { package = "rightkit-oauth", version = "1" }', '', options).hookSpecificOutput?.additionalContext ?? '', /rightkit-oauth/);
});

test('new-manifest keyword advice respects description, whole tokens, owner synonyms & RightKit exclusion', () => {
  const options = { readFile: missingManifest, cwd: APPS_ROOT + '/acme' };
  assertAdvice(manifestWrite('package.json', '{"description":"OAuth client"}', '', options), /rightkit-oauth/);
  assertAdvice(manifestWrite('package.json', '{"name":"acme-model-store"}', '', options), /rightkit-models/);
  assertAdvice(manifestWrite('package.json', '{"name":"acme-keychain"}', '', options), /rightkit-secrets/);
  assert.deepEqual(manifestWrite('package.json', '{"name":"acme-oauthish"}', '', options), { decision: 'allow' });
  assert.deepEqual(manifestWrite('package.json', '{"name":"acme-oauth-client"}', '', { readFile: () => '{}' }), { decision: 'allow' });
  assert.deepEqual(manifestWrite('package.json', '{"name":"acme-oauth-client"}', '', { ...options, cwd: 'D:/Claude/tools/rightkit/packages/new' }), { decision: 'allow' });
});

test('apply_patch Add File performs new-manifest keyword advice & owner suppression', () => {
  for (const [content, expected] of [['{"name":"acme-oauth-client"}', true], ['{"name":"acme-oauth-client","dependencies":{"rightkit-oauth":"1"}}', false]]) {
    const patch = `*** Begin Patch\n*** Add File: app/package.json\n+${content}\n*** End Patch`;
    const result = ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch } }, { readFile: missingManifest, cwd: APPS_ROOT });
    if (expected) assertAdvice(result, /rightkit-oauth/); else assert.doesNotMatch(result.hookSpecificOutput?.additionalContext ?? '', /rightkit-oauth/);
  }
});

test('malformed TOML or JSON, unreadable manifests & ambiguous edits allow with diagnostic advice', () => {
  for (const text of ['[dependencies\nreqwest = "0.12"', '[dependencies]\nreqwest = { package = "reqwest"', '[dependencies]\nreqwest = not-a-value', '[dependencies]\nreqwest = "unterminated']) {
    assertAdvice(manifestWrite('Cargo.toml', text, '[dependencies]\n'), /could not screen manifest edit/);
  }
  assertAdvice(manifestWrite('package.json', '{"dependencies":', '{}'), /could not screen manifest edit/);
  assertAdvice(manifestWrite('package.json', '{"dependencies":{"puppeteer":"24"}}', '{broken'), /could not screen manifest edit/);
  assertAdvice(manifestWrite('Cargo.toml', '[dependencies]\nreqwest = "0.12"', '', { readFile: () => { throw new Error('I/O'); } }), /could not screen manifest edit/);
  assertAdvice(ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'Cargo.toml', old_string: 'missing', new_string: 'reqwest = "0.12"' } }, { readFile: () => '[dependencies]' }), /could not screen manifest edit/);
  assertAdvice(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch: '*** Begin Patch\n*** Update File: Cargo.toml\n@@\n-missing\n+reqwest = "0.12"\n*** End Patch' } }, { readFile: () => '[dependencies]' }), /could not screen manifest edit/);
});

test('manifest comments & quoted examples do not declare competing dependencies', () => {
  assert.deepEqual(manifestWrite('Cargo.toml', '[package]\nname = "app"\ndescription = "reqwest = chromiumoxide"\n[dependencies]\n# reqwest = "0.12"\nserde = { version = "1", features = ["derive"] }', '[package]\nname = "app"'), { decision: 'allow' });
  assert.deepEqual(manifestWrite('Package.swift', '// .package(url: "https://example.com/puppeteer.git", from: "1")\nlet package = Package(name: "app")', ''), { decision: 'allow' });
  assertAdvice(manifestWrite('Package.swift', '.package(url: "unterminated', ''), /could not screen manifest edit/);
});

test('manifest replace_all & numbered multi-hunk patches reconstruct dependency delta', () => {
  const before = '{"dependencies":{},"devDependencies":{}}';
  const edited = ownershipWriteDecision({ tool_name: 'Edit', tool_input: { file_path: 'package.json', old_string: '{}', new_string: '{"puppeteer":"24"}', replace_all: true } }, { readFile: () => before });
  assert.equal(edited.hookSpecificOutput.permissionDecision, 'deny');
  const patch = '*** Begin Patch\n*** Update File: Cargo.toml\n@@ -1,2 +1,2 @@\n [package]\n-name = "app"\n+name = "renamed"\n@@ -3,1 +3,2 @@\n [dependencies]\n+reqwest = "0.12"\n*** End Patch';
  assertAdvice(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch } }, { readFile: () => '[package]\nname = "app"\n[dependencies]\n' }), /reqwest.*rightkit-http/);
});

test('pnpm workspace flag is boolean & shell comment chains preserve following commands', () => {
  for (const command of ['pnpm add -w playwright', 'pnpm add --workspace puppeteer', 'echo done # ignored\nnpm i puppeteer']) {
    assert.equal(ownershipWriteDecision({ tool_name: 'Bash', tool_input: { command } }).hookSpecificOutput.permissionDecision, 'deny', command);
  }
});

test('standalone hook honors banned policy independently of tier, Swift dependencies & catalog errors', async () => {
  const root = mkdtempSync(path.join(tmpdir(), 'rk-ownership-hook-'));
  try {
    const source = path.dirname(fileURLToPath(import.meta.url));
    for (const file of ['ownership-write-hook.mjs', 'ownership-source.mjs', 'ownership-repositories.json']) cpSync(path.join(source, file), path.join(root, file));
    const installed = JSON.parse(readFileSync(snapshot));
    const browser = installed.capabilities.find((entry) => entry.package === 'rightkit-browser');
    browser.fingerprints.find((entry) => entry.name === 'puppeteer').tier = 'advise';
    browser.fingerprints.push({ type: 'dependency', ecosystem: 'swiftpm', name: 'chromiumoxide', tier: 'block' });
    writeFileSync(path.join(root, 'ownership-fingerprints.json'), JSON.stringify(installed));
    const hook = await import(`${pathToFileURL(path.join(root, 'ownership-write-hook.mjs')).href}?valid=1`);
    hook.setScopeDependencies({ git: registeredGit, exists: () => true, roots: [] });
    assert.equal(hook.ownershipWriteDecision({ tool_name: 'Bash', tool_input: { command: 'npm i puppeteer' } }).hookSpecificOutput.permissionDecision, 'deny');
    const swift = '.package(url: "https://example.com/chromiumoxide.git", .upToNextMajor(from: "1.0"))';
    const input = { tool_name: 'Write', tool_input: { file_path: 'Package.swift', content: swift } };
    assert.equal(hook.ownershipWriteDecision(input, { readFile: () => '' }).hookSpecificOutput.permissionDecision, 'deny');
    assert.deepEqual(hook.ownershipWriteDecision(input, { readFile: () => swift }), { decision: 'allow' });
    writeFileSync(path.join(root, 'ownership-fingerprints.json'), '{invalid');
    const broken = await import(`${pathToFileURL(path.join(root, 'ownership-write-hook.mjs')).href}?broken=1`);
    broken.setScopeDependencies({ git: registeredGit, exists: () => true, roots: [] });
    assertAdvice(broken.ownershipWriteDecision(input), /Could not load ownership catalog/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('RightKit owner tree is never screened as a duplicate of itself', () => {
  const decision = ownershipWriteDecision({ tool_name: 'Write', cwd: 'D:/Claude', tool_input: { file_path: 'D:/Claude/tools/rightkit/crates/rightkit-sandbox/src/bin/exec.rs', content: 'sandbox_init(profile); SetFileSecurityW(); SendInput(); chromiumoxide::Browser;' } }, { readFile: () => { const error = new Error('missing'); error.code = 'ENOENT'; throw error; } });
  assert.equal(decision.hookSpecificOutput, undefined);
});

const gitFor = (map) => (args) => {
  const dir = args[1];
  if (args.includes('--show-toplevel')) return Object.keys(map).find((top) => dir === top || dir.startsWith(`${top}/`)) ?? null;
  return map[dir] ?? null;
};
const blockedWrite = (file) => ({ tool_name: 'Write', tool_input: { file_path: file, content: github }, cwd: '/' });
const isDeny = (r) => r.hookSpecificOutput?.permissionDecision === 'deny';
const exceptionFile = ['.right', 'kit/ownership-exceptions.json'].join('');

test('scope: registered repo is screened, unregistered and non-git paths are allowed untouched', () => {
  const git = gitFor({ '/r/mail': 'https://github.com/bogusyogi/mailright.git', '/r/vendor': 'https://github.com/someone/else.git' });
  setScopeDependencies({ git, exists: (p) => !p.includes('/new'), roots: [] });
  const manifest = (file) => ({ tool_name: 'Write', tool_input: { file_path: file, content: '{"name":"x","dependencies":{"puppeteer":"24"}}' }, cwd: '/' });
  assert.ok(ownershipWriteDecision(blockedWrite('/r/mail/src/a.ts')).hookSpecificOutput);
  assert.deepEqual(ownershipWriteDecision(blockedWrite('/r/vendor/src/a.ts')), { decision: 'allow' });
  assert.deepEqual(ownershipWriteDecision(blockedWrite('/tmp/scratch/a.ts')), { decision: 'allow' });
  assert.ok(ownershipWriteDecision(blockedWrite('/r/mail/src/new/deep/a.ts')).hookSpecificOutput);
  assert.deepEqual(ownershipWriteDecision(blockedWrite('/r/vendor/src/new/a.ts')), { decision: 'allow' });
  assert.ok(ownershipWriteDecision(manifest('/r/mail/package.json')).hookSpecificOutput);
  assert.deepEqual(ownershipWriteDecision(manifest('/r/vendor/package.json')), { decision: 'allow' });
});

test('scope: cargo add depends on the event cwd repository; git failure fails open', () => {
  setScopeDependencies({ git: gitFor({ '/r/mail': 'ssh://git@github.com/bogusyogi/mailright' }), exists: () => true, roots: [] });
  const bash = (cwd) => ({ tool_name: 'Bash', tool_input: { command: 'cargo add reqwest' }, cwd });
  assert.ok(ownershipWriteDecision(bash('/r/mail')).hookSpecificOutput);
  assert.deepEqual(ownershipWriteDecision(bash('/r/other')), { decision: 'allow' });
  setScopeDependencies({ git: () => { throw new Error('boom'); }, exists: () => true, roots: [] });
  assert.deepEqual(ownershipWriteDecision(bash('/r/mail')), { decision: 'allow' });
});

test('scope: protected records stay denied everywhere, including apply_patch', () => {
  setScopeDependencies({ git: () => null, exists: () => true, roots: [] });
  assert.equal(isDeny(ownershipWriteDecision({ tool_name: 'Write', tool_input: { file_path: `/tmp/x/${exceptionFile}`, content: '{}' }, cwd: '/' })), true);
  const patch = `*** Begin Patch\n*** Add File: /tmp/x/${exceptionFile}\n+{}\n*** End Patch`;
  assert.equal(isDeny(ownershipWriteDecision({ tool_name: 'apply_patch', tool_input: { patch }, cwd: '/' })), true);
  assert.equal(isDeny(ownershipWriteDecision({ tool_name: 'Bash', tool_input: { command: `echo {} > ${exceptionFile}` }, cwd: '/tmp/x' })), true);
});

test('scope: Windows path forms resolve through git toplevel and canonical identity', () => {
  setScopeDependencies({ git: gitFor({ 'D:/repos/mail': 'git@github.com:BogusYogi/MailRight.git' }), exists: () => true, roots: [] });
  assert.ok(ownershipWriteDecision(blockedWrite('D:/repos/mail/src/a.ts')).hookSpecificOutput);
  assert.deepEqual(ownershipWriteDecision(blockedWrite('D:/repos/other/src/a.ts')), { decision: 'allow' });
  assert.equal(canonicalRepositoryIdentity('git@github.com:BogusYogi/MailRight.git'), 'https://github.com/bogusyogi/mailright');
  assert.equal(canonicalRepositoryIdentity('https://github.com/BogusYogi/MailRight/'), 'https://github.com/bogusyogi/mailright');
});

test('packaged repository registry matches release registry', () => {
  assert.deepEqual(JSON.parse(readFileSync(new URL('./ownership-repositories.json', import.meta.url))), JSON.parse(readFileSync(new URL('../../release/ownership-repositories.json', import.meta.url))));
  setScopeDependencies({ git: registeredGit, exists: () => true, roots: [] });
});

test('scope: workspace root is in scope by path; registry covers registered repos cloned elsewhere', () => {
  const roots = ['/ws', 'D:\\Claude'];
  const git = gitFor({ '/elsewhere/mail': 'https://github.com/bogusyogi/mailright', '/ws/vendor/lib': 'https://github.com/someone/else' });
  setScopeDependencies({ git, exists: () => true, roots, realpath: (p) => p.startsWith('/link/ws') ? p.replace('/link/ws', '/ws') : p });
  assert.ok(ownershipWriteDecision(blockedWrite('/ws/notes/a.ts')).hookSpecificOutput);       // non-git folder under workspace
  assert.ok(ownershipWriteDecision(blockedWrite('/ws/vendor/lib/a.ts')).hookSpecificOutput);  // third-party clone under workspace
  assert.ok(ownershipWriteDecision(blockedWrite('/link/ws/x/a.ts')).hookSpecificOutput);      // symlink resolves into workspace
  assert.ok(ownershipWriteDecision(blockedWrite('/elsewhere/mail/a.ts')).hookSpecificOutput); // registered repo outside workspace
  assert.deepEqual(ownershipWriteDecision(blockedWrite('/elsewhere/other/a.ts')), { decision: 'allow' });
  assert.deepEqual(ownershipWriteDecision(blockedWrite('/wsx/a.ts')), { decision: 'allow' }); // prefix is not containment
  setScopeDependencies({ git, exists: () => true, roots, caseInsensitive: true });
  assert.ok(ownershipWriteDecision(blockedWrite('d:\\claude\\app\\a.ts')).hookSpecificOutput);
});
