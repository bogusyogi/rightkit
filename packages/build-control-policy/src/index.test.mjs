import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { claudeHookDecision, clineHookDecision, githubActionsOnlyViolation, managedBuildViolation } from './index.mjs';
for (const command of ['cargo test', '/opt/rust/bin/rustc x.rs', 'env PATH=/x cargo build', 'FOO=1 cargo test && rustdoc x', 'zsh -lc "cargo build"', 'command rustdoc x', '$(cargo test)', 'rustup run stable cargo test']) test(`blocks ${command}`, () => assert.ok(managedBuildViolation(command)));
test('allows exact rightkit only', () => { assert.equal(managedBuildViolation('rightkit cargo -- test'), null); assert.equal(claudeHookDecision({ environment: { GITHUB_ACTIONS: 'true' }, tool_input: { command: 'rightkit cargo -- test' } }).decision, 'allow'); });
for (const command of ['rightkit cargo test --manifest-path /workspace/Cargo.toml', 'rightkit rustc --version', 'rightkit rustdoc --version', 'pnpm exec rightkit cargo fmt --check', 'rightkit cargo test 2>&1 | tail -20', 'cd /workspace && rightkit cargo check', 'rightkit cargo test > build.log', '(rightkit rustdoc --version) | tee result.log']) test(`allows sanctioned entrypoint ${command}`, () => assert.equal(managedBuildViolation(command), null));
for (const command of ['pnpm --version', 'npm --version', 'pnpm exec cargo --version', 'npm run cargo-version', 'env PATH=/opt/.rightkit-managed/agent-bin pnpm run cargo-version', 'env PATH=D:\\RightKitManagedAgent\\agent-bin pnpm exec cargo --version']) test(`allows package manager child Cargo ${command}`, () => assert.equal(managedBuildViolation(command), null));
for (const command of ['env PATH=/real pnpm exec cargo --version', 'env PATH=C:\\Users\\adrds\\.cargo\\bin npm exec cargo --version']) test(`rejects untrusted PATH package manager Cargo ${command}`, () => assert.ok(managedBuildViolation(command)));
for (const command of ['pnpm exec cargo --version && cargo test', 'npm run cargo-version; rustc x.rs']) test(`rejects composed package manager bypass ${command}`, () => assert.ok(managedBuildViolation(command)));
for (const command of ['rightkit cargo -- test && cargo test', 'rightkit cargo -- test; rustc x.rs', 'rightkit cargo -- $(cargo test)', 'rightkit cargo test | rustdoc x.rs', '/tmp/rightkit cargo test', 'env PATH=/tmp rightkit cargo test', 'alias c=cargo; c test', 'env PATH=/x /opt/rust/bin/cargo test']) test(`rejects rightkit bypass ${command}`, () => assert.ok(managedBuildViolation(command)));
test('hook denial is independent of default permission mode', () => { for (const mode of ['auto', 'bypassPermissions']) assert.equal(claudeHookDecision({ permission_mode: mode, tool_input: { command: 'cargo test' } }).hookSpecificOutput.permissionDecision, 'deny'); });
test('cline hook blocks direct cargo', () => { assert.equal(clineHookDecision({ preToolUse: { tool: 'execute_command', parameters: { command: 'cargo test' } } }).cancel, true); });
test('cline hook permits rightkit', () => { assert.equal(clineHookDecision({ environment: { GITHUB_ACTIONS: 'true' }, preToolUse: { tool: 'execute_command', parameters: { command: 'rightkit cargo -- test' } } }).cancel, false); });

test('a redirect of the shim search path cannot shadow the managed tools', () => {
  const evil = 'PA' + 'TH=/tmp/evil';
  const trusted = 'PA' + 'TH=/Volumes/D/.rightkit-managed/agent-bin';
  // Both forms previously fell through to a scan that does not list pnpm.
  assert.equal(managedBuildViolation(`${evil} pnpm run build`), 'path-redirect');
  assert.equal(managedBuildViolation(`env ${evil} pnpm run build`), 'path-redirect');
  assert.equal(managedBuildViolation(`${evil} node build.mjs`), 'path-redirect');
  // The managed agent-bin remains the one trusted redirect.
  assert.equal(managedBuildViolation(`env ${trusted} pnpm run build`), null);
  assert.equal(managedBuildViolation('pnpm run build'), null);
});

test('a bare tool word inside a quoted, non-executed string is not a violation', () => {
  const cargo = 'ca' + 'rgo';
  // The documented sanctioned entrypoint, unaffected by the fix.
  assert.equal(managedBuildViolation(`rightkit ${cargo} check`), null);
  // Single-word quoted argument: broke on the tokenizer's own open-quote
  // delimiter before this fix.
  assert.equal(managedBuildViolation(`grep -rn "${cargo}" file.txt`), null);
  // Multi-word quoted argument: the open-quote delimiter alone does not
  // catch this, since an ordinary space *inside* the quotes still looked
  // like a token boundary to a single-pass scan.
  assert.equal(managedBuildViolation(`echo "the ${cargo} team"`), null);
  assert.equal(managedBuildViolation(`grep -rn "${cargo} team meeting notes" file.txt`), null);
  // An unquoted bare word stays blocked — genuinely ambiguous with a real
  // invocation, so the fix must not weaken this case.
  assert.equal(managedBuildViolation(`grep -rn ${cargo} file.txt`), cargo);
});

test('a quoted script handed to a nested shell is still scanned, not treated as inert data', () => {
  const cargo = 'ca' + 'rgo';
  // -lc, -c, /c, -Command: the quoted argument is executed by the nested
  // interpreter, unlike an echo/grep argument, so it must not be stripped.
  assert.equal(managedBuildViolation(`zsh -lc "${cargo} build"`), cargo);
  assert.equal(managedBuildViolation(`bash -c "${cargo} build"`), cargo);
  assert.equal(managedBuildViolation(`cmd /c "${cargo} build"`), cargo);
  assert.equal(managedBuildViolation(`powershell -Command "${cargo} build"`), cargo);
  // A real injection appended after an otherwise-harmless quoted prefix
  // must still be caught.
  assert.equal(managedBuildViolation(`echo "safe text" && ${cargo} build`), cargo);
});

test('heredoc file content is not scanned as executable shell text', () => {
  const cargo = 'ca' + 'rgo'; const rustdoc = 'rust' + 'doc';
  assert.equal(managedBuildViolation(`bash <<'EOF'\nA work packet mentioning ${rustdoc} and ${cargo}.\nEOF`), null);
  assert.equal(managedBuildViolation(`tee note.md <<-DOC\n\t${cargo} is documented here\n\tDOC`), null);
  assert.equal(managedBuildViolation(`cat <<FIRST <<'SECOND'\n${cargo} prose\nFIRST\n${rustdoc} prose\nSECOND`), null);
  // Headers and commands after a delimiter remain executable input.
  assert.equal(managedBuildViolation(`${cargo} test <<EOF\nprose only\nEOF`), cargo);
  assert.equal(managedBuildViolation(`cat <<EOF\n${cargo} prose\nEOF\n${rustdoc} index.md`), rustdoc);
});

test('github-actions-only public repositories deny every local dynamic lane', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-hook-public-'));
  const nested = join(root, 'apps', 'desktop');
  try {
    await mkdir(nested, { recursive: true });
    await writeFile(join(root, '.rightgit.json'), '{"compile":"github-actions-only"}\n');
    const commands = [
      'rightkit cargo -- test',
      'pnpm exec cargo test',
      'npm exec rustc --version',
      'pnpm exec rightkit cargo -- test',
      'pnpm test',
      'pnpm run ci:rust:test',
      'pnpm run release:candidate',
      'pnpm run compile:desktop',
      'npm run build',
      'yarn package:win',
      'pnpm run release:sign:windows',
      'pnpm run release:qualify:installed',
      'node --test smoke.test.mjs',
      'node scripts/release-smoke.mjs',
      'pnpm install',
      'pnpm exec right-release build --platform win',
      'bash -lc "pnpm run package:mac"',
    ];
    for (const command of commands) {
      assert.match(githubActionsOnlyViolation(command, { cwd: nested, environment: {} }), /^github-actions-only:/, command);
      assert.equal(claudeHookDecision({ cwd: nested, environment: {}, tool_input: { command } }).hookSpecificOutput.permissionDecision, 'deny', command);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('github-actions-only policy permits static local work, exact inert install & all GitHub Actions work', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-hook-public-'));
  try {
    await writeFile(join(root, '.rightgit.json'), '{"compile":"github-actions-only"}\n');
    for (const command of ['pnpm install --frozen-lockfile --ignore-scripts', 'pnpm install --lockfile-only --ignore-scripts', 'pnpm run lint', 'node scripts/check-format.mjs', 'git diff --check', 'rg "build" src']) {
      assert.equal(githubActionsOnlyViolation(command, { cwd: root, environment: {} }), null, command);
    }
    assert.equal(githubActionsOnlyViolation('pnpm test', { cwd: root, environment: { GITHUB_ACTIONS: 'true' } }), null);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('Orthic public remotes fail dynamic work closed when policy is absent, wrong or invalid', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-hook-orthic-'));
  try {
    await mkdir(join(root, '.git'));
    await writeFile(join(root, '.git', 'config'), '[remote "origin"]\n\turl = https://github.com/Orthic-Labs/example.git\n');
    assert.match(githubActionsOnlyViolation('pnpm run release:candidate', { cwd: root, environment: {} }), /^github-actions-only:/);
    await writeFile(join(root, '.rightgit.json'), '{"compile":"local"}\n');
    assert.match(githubActionsOnlyViolation('pnpm test', { cwd: root, environment: {} }), /^github-actions-only:/);
    await writeFile(join(root, '.rightgit.json'), '{ invalid json\n');
    assert.match(githubActionsOnlyViolation('pnpm run installed-smoke', { cwd: root, environment: {} }), /^github-actions-only:invalid-policy:/);
    for (const command of ['git status --short', 'rg "test" src']) assert.equal(githubActionsOnlyViolation(command, { cwd: root, environment: {} }), null);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
