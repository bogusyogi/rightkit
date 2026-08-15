import test from 'node:test';
import assert from 'node:assert/strict';
import { claudeHookDecision, clineHookDecision, managedBuildViolation } from './index.mjs';
for (const command of ['cargo test', '/opt/rust/bin/rustc x.rs', 'env PATH=/x cargo build', 'FOO=1 cargo test && rustdoc x', 'zsh -lc "cargo build"', 'command rustdoc x', '$(cargo test)', 'rustup run stable cargo test']) test(`blocks ${command}`, () => assert.ok(managedBuildViolation(command)));
test('allows exact rightkit only', () => { assert.equal(managedBuildViolation('rightkit cargo -- test'), null); assert.equal(claudeHookDecision({ tool_input: { command: 'rightkit cargo -- test' } }).decision, 'allow'); });
for (const command of ['pnpm --version', 'npm --version', 'pnpm exec cargo --version', 'npm run cargo-version', 'env PATH=/opt/.rightkit-managed/agent-bin pnpm run cargo-version', 'env PATH=D:\\RightKitManagedAgent\\agent-bin pnpm exec cargo --version']) test(`allows package manager child Cargo ${command}`, () => assert.equal(managedBuildViolation(command), null));
for (const command of ['env PATH=/real pnpm exec cargo --version', 'env PATH=C:\\Users\\adrds\\.cargo\\bin npm exec cargo --version']) test(`rejects untrusted PATH package manager Cargo ${command}`, () => assert.ok(managedBuildViolation(command)));
for (const command of ['pnpm exec cargo --version && cargo test', 'npm run cargo-version; rustc x.rs']) test(`rejects composed package manager bypass ${command}`, () => assert.ok(managedBuildViolation(command)));
for (const command of ['rightkit cargo -- test && cargo test', 'rightkit cargo -- test; rustc x.rs', 'rightkit cargo -- $(cargo test)', 'rightkit cargo -- test > out', 'alias c=cargo; c test', 'env PATH=/x /opt/rust/bin/cargo test']) test(`rejects rightkit bypass ${command}`, () => assert.ok(managedBuildViolation(command)));
test('hook denial is independent of default permission mode', () => { for (const mode of ['auto', 'bypassPermissions']) assert.equal(claudeHookDecision({ permission_mode: mode, tool_input: { command: 'cargo test' } }).hookSpecificOutput.permissionDecision, 'deny'); });
test('cline hook blocks direct cargo', () => { assert.equal(clineHookDecision({ preToolUse: { tool: 'execute_command', parameters: { command: 'cargo test' } } }).cancel, true); });
test('cline hook permits rightkit', () => { assert.equal(clineHookDecision({ preToolUse: { tool: 'execute_command', parameters: { command: 'rightkit cargo -- test' } } }).cancel, false); });

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
