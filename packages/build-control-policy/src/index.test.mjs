import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { agentDoorViolation, claudeHookDecision, clineHookDecision, githubActionsOnlyViolation, managedBuildViolation, rustRightManagedHost } from './index.mjs';

test('agent door blocks recognized direct builds and permits the managed entries', () => {
  const options = { platform: 'darwin', home: '/home', hasDoor: () => true, read: () => JSON.stringify({ buildRules: { checkAdmission: { trustedGate: { path: '/pin/rightkit-gate.mjs' } } } }), realpath: (p) => p };
  for (const command of ['vite build', 'pnpm run build', 'pnpm exec vite build', 'node --test unit.test.mjs', 'tsc', 'node scripts/build.mjs', 'node /app/rightkit-gate.mjs --to dev', 'rightkit-gate --to dev']) {
    assert.ok(agentDoorViolation(command, options), command);
  }
  for (const command of ['rightkit-host-gate --to dev', 'pnpm exec rightkit-host-gate --to dev', 'rustright cargo check', 'pnpm exec rustright cargo check', 'node /pin/rightkit-gate.mjs --to dev', 'node --check scripts/build.mjs']) {
    assert.equal(agentDoorViolation(command, options), null, command);
  }
  assert.equal(agentDoorViolation('vite build', { ...options, platform: 'linux' }), null);
  assert.equal(agentDoorViolation('vite build', { ...options, hasDoor: () => false }), null);
  assert.equal(agentDoorViolation('vite build', { ...options, read: () => '{}' }), null);
});

test('public agent hooks apply the door policy in a managed repository', async () => {
  const scratch = process.env.RIGHTKIT_SCRATCH_ROOT ?? (process.platform === 'darwin' ? '/Volumes/D/scratch' : process.platform === 'win32' ? 'D:\\scratch' : tmpdir());
  const root = await mkdtemp(join(scratch, 'rightkit-hook-door-'));
  try {
    const repo = join(root, 'repo');
    const home = join(root, 'home');
    const configDir = join(home, 'Library', 'Application Support', 'RightKit', 'build-control');
    await mkdir(join(repo, '.git'), { recursive: true });
    await mkdir(configDir, { recursive: true });
    await writeFile(join(repo, 'right-release.config.json'), '{}');
    const pin = join(root, 'rightkit-gate.mjs');
    await writeFile(pin, '');
    await writeFile(join(configDir, 'config.json'), JSON.stringify({ buildRules: { checkAdmission: { trustedGate: { path: pin } } } }));
    for (const command of ['pnpm run build', 'node --test unit.test.mjs', 'vite build']) {
      const input = { platform: 'darwin', cwd: repo, environment: { HOME: home }, command };
      assert.equal(claudeHookDecision(input).hookSpecificOutput?.permissionDecision, 'deny', command);
      assert.equal(clineHookDecision(input).cancel, true, command);
    }
    const safe = { platform: 'darwin', cwd: repo, environment: { HOME: home }, command: `node ${pin} --to dev` };
    assert.equal(claudeHookDecision(safe).decision, 'allow');
    assert.equal(clineHookDecision(safe).cancel, false);
  } finally { await rm(root, { recursive: true, force: true }); }
});

async function setOrigin(root, url = 'https://github.com/Orthic-Labs/example.git') {
  await mkdir(join(root, '.git'), { recursive: true });
  await writeFile(join(root, '.git/config'), `[remote "origin"]\n url = ${url}\n`);
}

const commandCases = [
  ['legion run --help', false],
  ['legion run --help; cat docs/rules/release-signing.md', false],
  ['sed -n 1,20p crates/legion-apple/src/build_analysis.rs', false],
  ['grep build_analysis.rs crates/legion-apple/src/lib.rs', false],
  ['echo cargo', false],
  ['echo PATH=/tmp', false],
  ['PATH=/tmp grep cargo log', false],
  ['export PATH=/tmp; grep cargo log', false],
  ['grep "Failed to list rustc sysroot" log', false],
  ['grep cargo log', false],
  ['python3 -c "import json; d = {}; print(d[\'stagedCargo\'])"', false],
  ['python3 -c "print(\'cargo rustc PATH=/tmp\')"', false],
  ['node -e "console.log(\'cargo rustc PATH=/tmp\')"', false],
  ['node packages/release/rightkit-sync.mjs stage cargo rightkit-qa 0.2.9', false],
  ['node scripts/inspect.mjs build cargo rustc --test', false],
  ['python3 scripts/inspect.py build cargo rustc', false],
  ['node cargo rustc', false],
  ['node --check scripts/build.mjs', false],
  ['python3 -m json.tool cargo', false],
  ['bash -c "echo cargo; grep rustc log"', false],
  ['rustup run stable echo cargo', false],
  ['command -v cargo', false],
  ['command -p -v cargo', false],
  ['bash inspect.sh -c "cargo build"', false],
  ['bash -C inspect.sh cargo', false],
  ['echo "cmd: \'cargo build\'"', false],
  ["python3 <<'PY'\nprint('cargo rustc PATH=/tmp build_analysis.rs')\nPY", false],
  ['echo safe # cargo build; rustc x.rs', false],
  ['echo safe\n# cargo build\nprintf "%s" rustdoc', false],
  ['cat cargo; sed -n 1p rustc | grep rustdoc && ls build_analysis.rs', false],
  ['head cargo; tail rustc; wc rustdoc; find . -name cargo; rg release-signing.md', false],
  ['grep -c cargo log', false],
  ['xargs -p echo cargo', false],
  ['pnpm exec grep build_analysis.rs log', false],
  ['npm exec -- node packages/release/rightkit-sync.mjs stage cargo rightkit-qa 0.2.9', false],
  ['cat < cargo > rustc', false],
  ['cargo build', true],
  ['"cargo" build', true],
  ['python3 -m build', true, false],
  ['env X=1 cargo test', true],
  ['cd x && cargo clippy', true],
  ['bash -c "cargo build"', true],
  ['/path/to/rustc x.rs', true],
  ['xargs cargo build', true],
  ['printf "%s" x | xargs -I{} cargo test {}', true],
  ['(echo safe; cargo build)', true],
  ['echo safe | rustdoc x.rs', true],
  ['echo "$(cargo build)"', true],
  ['echo `cargo build`', true],
  ['rustup run stable cargo test', true],
  ['cargo-clippy --workspace', true],
  ['env -u X /path/to/rustdoc x.rs', true],
  ['command -p cargo build', true],
  ['command cargo -V', true],
  ['rustup run stable bash -c "cargo build"', true],
  ['xargs -p cargo test', true],
  ['echo safe # <<EOF\ncargo build', true],
  ['pnpm run build', true, false],
  ['pnpm --dir app run test:unit', true, false],
  ['pnpm -w exec cargo test', true, false],
  ['npm run build', true, false],
  ['npx tsc --noEmit', true, false],
  ['pnpm exec vite build', true, false],
  ['npm exec -- tauri build', true, false],
  ['npx --yes tauri build', true, false],
  ['(PATH=/tmp; echo cargo); pnpm run build', true, false],
  ['(PATH=/tmp; pnpm run build)', true],
  ['export PATH=/tmp; pnpm run build', true],
];

test('executable classification regression table (public hook)', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-hook-table-'));
  try {
    await setOrigin(root);
    await writeFile(join(root, '.rightgit.json'), '{"compile":"github-actions-only"}');
    for (const [command, blocked, managed = blocked] of commandCases) await t.test(command, () => {
      const decision = claudeHookDecision({ platform: 'darwin', cwd: root, environment: {}, tool_input: { command } });
      assert.equal(decision.hookSpecificOutput?.permissionDecision === 'deny', blocked);
      assert.equal(clineHookDecision({ platform: 'darwin', cwd: root, environment: {}, command }).cancel, blocked);
      assert.equal(Boolean(managedBuildViolation(command)), managed);
    });
  } finally { await rm(root, { recursive: true, force: true }); }
});
for (const command of ['cargo test', '/opt/rust/bin/rustc x.rs', 'env PATH=/x cargo build', 'FOO=1 cargo test && rustdoc x', 'zsh -lc "cargo build"', 'command rustdoc x', '$(cargo test)', 'rustup run stable cargo test']) test(`blocks ${command}`, () => assert.ok(managedBuildViolation(command)));
test('allows exact rightkit only', () => { assert.equal(managedBuildViolation('rustright cargo -- test'), null); assert.equal(claudeHookDecision({ environment: { GITHUB_ACTIONS: 'true' }, tool_input: { command: 'rustright cargo -- test' } }).decision, 'allow'); });
for (const command of ['rustright cargo test --manifest-path /workspace/Cargo.toml', 'rustright rustc --version', 'rustright rustdoc --version', 'pnpm exec rustright cargo fmt --check', 'rustright cargo test 2>&1 | tail -20', 'cd /workspace && rustright cargo check', 'rustright cargo test > build.log', '(rustright rustdoc --version) | tee result.log']) test(`allows sanctioned entrypoint ${command}`, () => assert.equal(managedBuildViolation(command), null));
for (const command of ['pnpm --version', 'npm --version', 'pnpm exec cargo --version', 'npm run cargo-version', 'env PATH=/opt/.rightkit-managed/agent-bin pnpm run cargo-version', 'env PATH=D:\\RightKitManagedAgent\\agent-bin pnpm exec cargo --version']) test(`allows package manager child Cargo ${command}`, () => assert.equal(managedBuildViolation(command), null));
for (const command of ['env PATH=/real pnpm exec cargo --version', 'env PATH=C:\\Users\\adrds\\.cargo\\bin npm exec cargo --version']) test(`rejects untrusted PATH package manager Cargo ${command}`, () => assert.ok(managedBuildViolation(command)));
for (const command of ['pnpm exec cargo --version && cargo test', 'npm run cargo-version; rustc x.rs']) test(`rejects composed package manager bypass ${command}`, () => assert.ok(managedBuildViolation(command)));
for (const command of ['rustright cargo -- test && cargo test', 'rustright cargo -- test; rustc x.rs', 'rustright cargo -- $(cargo test)', 'rustright cargo test | rustdoc x.rs', '/tmp/rightkit cargo test', 'env PATH=/tmp rustright cargo test', 'alias c=cargo; c test', 'env PATH=/x /opt/rust/bin/cargo test']) test(`rejects rightkit bypass ${command}`, () => assert.ok(managedBuildViolation(command)));
test('hook denial is independent of default permission mode', () => { for (const mode of ['auto', 'bypassPermissions']) assert.equal(claudeHookDecision({ platform: 'darwin', permission_mode: mode, tool_input: { command: 'cargo test' } }).hookSpecificOutput.permissionDecision, 'deny'); });

test('Codex cmd field and composed functions.exec payload retain managed build policy', () => {
  assert.equal(claudeHookDecision({ platform: 'darwin', tool_name: 'exec_command', tool_input: { cmd: 'cargo test' } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(claudeHookDecision({ platform: 'darwin', tool_name: 'functions.exec', tool_input: { code: "await tools.exec_command({cmd: 'cargo test'})" } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(claudeHookDecision({ platform: 'darwin', tool_name: 'functions.exec', tool_input: { command: "await tools.exec_command({cmd: 'cargo test'})" } }).hookSpecificOutput.permissionDecision, 'deny');
  assert.equal(claudeHookDecision({ platform: 'darwin', tool_name: 'functions.exec', tool_input: { code: 'await tools.exec_command({cmd: "echo cargo"})' } }).decision, 'allow');
  assert.equal(claudeHookDecision({ platform: 'darwin', tool_name: 'functions.exec', tool_input: { code: 'await tools.exec_command({cmd: "grep \\"Failed to list rustc sysroot\\" log"})' } }).decision, 'allow');
});
test('cline hook blocks direct cargo', () => { assert.equal(clineHookDecision({ platform: 'darwin', preToolUse: { tool: 'execute_command', parameters: { command: 'cargo test' } } }).cancel, true); });
test('cline hook permits rightkit', () => { assert.equal(clineHookDecision({ platform: 'darwin', environment: { GITHUB_ACTIONS: 'true' }, preToolUse: { tool: 'execute_command', parameters: { command: 'rustright cargo -- test' } } }).cancel, false); });

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
  assert.equal(managedBuildViolation(`rustright ${cargo} check`), null);
  // Single-word quoted argument: broke on the tokenizer's own open-quote
  // delimiter before this fix.
  assert.equal(managedBuildViolation(`grep -rn "${cargo}" file.txt`), null);
  // Multi-word quoted argument: the open-quote delimiter alone does not
  // catch this, since an ordinary space *inside* the quotes still looked
  // like a token boundary to a single-pass scan.
  assert.equal(managedBuildViolation(`echo "the ${cargo} team"`), null);
  assert.equal(managedBuildViolation(`grep -rn "${cargo} team meeting notes" file.txt`), null);
  // Unquoted arguments are data too; only executable position can block.
  assert.equal(managedBuildViolation(`grep -rn ${cargo} file.txt`), null);
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
    await setOrigin(root);
    await writeFile(join(root, '.rightgit.json'), '{"compile":"github-actions-only"}\n');
    const commands = [
      'rustright cargo -- test',
      'pnpm exec cargo test',
      'npm exec rustc --version',
      'pnpm exec rustright cargo -- test',
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
    await setOrigin(root);
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

test('only owning repository origin determines public policy', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-hook-origin-'));
  try {
    await setOrigin(root);
    for (const [name, origin] of [
      ['private', 'https://github.com/adrdsouza/private.git'],
      ['public', 'git@github.com:Orthic-Labs/example.git'],
      ['ssh-public', 'ssh://git@github.com/Orthic-Labs/example.git'],
    ]) await t.test(name, async () => {
      const nested = join(root, name); await mkdir(nested);
      await setOrigin(nested, origin);
      // Public upstream & public compile labels never turn a private origin public.
      await writeFile(join(nested, '.git/config'), `[remote "origin"]\n url = ${origin}\n[remote "upstream"]\n url = https://github.com/Orthic-Labs/other.git\n`);
      await writeFile(join(nested, '.rightgit.json'), '{"compile":"github-actions-only"}');
      const options = { cwd: nested, environment: {} };
      assert.equal(Boolean(githubActionsOnlyViolation('pnpm test', options)), name !== 'private');
      assert.equal(githubActionsOnlyViolation('cat release-signing.md build_analysis.rs', options), null);
    });
    await t.test('origin absent stops ancestor lookup', async () => {
      const nested = join(root, 'no-origin'); await mkdir(join(nested, '.git'), { recursive: true });
      await writeFile(join(nested, '.git/config'), '[remote "upstream"]\n url = https://github.com/Orthic-Labs/other.git\n');
      assert.equal(githubActionsOnlyViolation('pnpm test', { cwd: nested, environment: {} }), null);
    });
    await t.test('gitdir & commondir resolve origin for linked checkout', async () => {
      const nested = join(root, 'linked'); const gitDir = join(root, 'metadata', 'worktrees', 'linked');
      await mkdir(nested); await mkdir(gitDir, { recursive: true });
      await writeFile(join(nested, '.git'), 'gitdir: ../metadata/worktrees/linked\n');
      await writeFile(join(gitDir, 'commondir'), '../..\n');
      await writeFile(join(gitDir, 'HEAD'), 'ref: refs/heads/main\n');
      await mkdir(join(root, 'metadata/objects')); await mkdir(join(root, 'metadata/refs'));
      await writeFile(join(root, 'metadata/config'), '[remote "origin"]\n url = git@github.com:Orthic-Labs/linked.git\n');
      assert.ok(githubActionsOnlyViolation('pnpm test', { cwd: nested, environment: {} }));
      await writeFile(join(root, 'metadata/config'), '[remote "origin"]\n url = git@github.com:adrdsouza/private.git\n');
      assert.equal(githubActionsOnlyViolation('pnpm test', { cwd: nested, environment: {} }), null);
    });
    await t.test('origin included by owning repository config', async () => {
      const nested = join(root, 'included'); await mkdir(nested); await mkdir(join(nested, '.git'));
      await writeFile(join(nested, '.git/config'), '[include]\n path = origin.config\n');
      await writeFile(join(nested, '.git/origin.config'), '[remote "origin"]\n url = https://github.com/Orthic-Labs/included.git\n');
      assert.ok(githubActionsOnlyViolation('pnpm test', { cwd: nested, environment: {} }));
    });
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('Membrane local declaration admits only its unsigned Windows installer entry', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-membrane-local-'));
  try {
    await mkdir(join(root, '.git'));
    await writeFile(join(root, '.git/config'), '[remote "origin"]\n url = https://github.com/Orthic-Labs/Membrane.git\n');
    await writeFile(join(root, '.rightgit.json'), '{"compile":"github-actions-only"}');
    await writeFile(join(root, '.rightkit-local-development.json'), JSON.stringify({ schemaVersion: 1, repository: 'Orthic-Labs/Membrane', platform: 'win32', purpose: 'unsigned-installer' }));
    const unsigned = 'pnpm --dir apps/membrane-hub run release:build:win:unsigned';
    const options = { cwd: root, environment: {}, platform: 'win32' };
    assert.equal(githubActionsOnlyViolation(unsigned, options), null);
    assert.match(githubActionsOnlyViolation(unsigned, { ...options, platform: 'darwin' }), /^github-actions-only:/);
    for (const command of ['pnpm run release:build:win', 'pnpm exec right-release sign-windows', 'pnpm run release:qualify:win', `${unsigned}; pnpm run release:build:win`]) {
      assert.match(githubActionsOnlyViolation(command, options), /^github-actions-only:/);
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});

for (const command of ['rustright remote windows cargo clippy --workspace --all-targets -- -D warnings', 'rustright remote windows cargo test -p rightkit-onnx --all-targets', 'rustright remote mac rustc --version', 'cd /workspace && rustright remote windows cargo check 2>&1 | tail -5']) test(`allows documented remote lane ${command}`, () => assert.equal(managedBuildViolation(command), null));
for (const command of ['rustright remote windows cargo test && cargo test', 'rustright remote evil cargo test', 'rustright remote windows cargo test; rustc x.rs', 'env PATH=/tmp rustright remote windows cargo test']) test(`rejects remote lane bypass ${command}`, () => assert.ok(managedBuildViolation(command)));

for (const command of ['node packages/release/cli/right-release.mjs publish cargo --crate rightkit-onnx', 'right-release publish cargo --crate rightkit-onnx --dry-run']) test(`allows right-release cargo publication ${command}`, () => assert.equal(managedBuildViolation(command), null));
for (const command of ['right-release publish cargo --crate x && cargo publish', 'right-release publish cargo; cargo test']) test(`rejects right-release publication bypass ${command}`, () => assert.ok(managedBuildViolation(command)));

// Issue #293: the cargo and rustup guard fires only for LOCAL execution on the Mac or Dell.
const remoteCases = [
  // [command, blocked locally]
  ['ssh host cargo build', false],
  ['ssh host cargo test --workspace', false],
  ["ssh host 'cd x && cargo test'", false],
  ['ssh -i k user@h "cd x && cargo test; rustup show"', false],
  ['ssh -p 2222 -o StrictHostKeyChecking=no user@h cargo check', false],
  ["ssh h bash -lc 'cargo build | tail'", false],
  ["ssh h 'rustup run stable cargo test'", false],
  ['ssh h rustup update', false],
  ['env X=1 ssh h cargo build', false],
  ['plink -i k user@h "cargo build"', false],
  ['ssh h true; cargo build', true],
  ['ssh h cargo test && cargo build', true],
  ['ssh h cargo test || cargo build', true],
  ['ssh h cargo test | cargo run', true],
  ['ssh h true\ncargo build', true],
  ['ssh h true; rustup run stable cargo test', true],
  ['ssh h "$(cargo build)"', true],
  ['ssh h `cargo build`', true],
  ['ssh h true; env X=1 cargo build', true],
  ['env X=1 cargo build', true],
  ['ssh -o ProxyCommand="cargo run" h true', true],
  ['cargo build', true],
  ['rustup run stable cargo test', true],
];

test('cargo and rustup guard fires only for local execution (table)', async (t) => {
  for (const [command, blocked] of remoteCases) await t.test(command, () => {
    assert.equal(Boolean(managedBuildViolation(command)), blocked);
    for (const platform of ['darwin', 'win32']) assert.equal(claudeHookDecision({ platform, environment: {}, tool_input: { command } }).hookSpecificOutput?.permissionDecision === 'deny', blocked);
    assert.equal(clineHookDecision({ platform: 'darwin', environment: {}, command }).cancel, blocked);
  });
});

test('sessions that are not on the Mac or Dell pass every command (table)', async (t) => {
  assert.equal(rustRightManagedHost({ platform: 'darwin' }), true);
  assert.equal(rustRightManagedHost({ platform: 'win32' }), true);
  assert.equal(rustRightManagedHost({ platform: 'linux' }), false);
  for (const [command] of remoteCases) await t.test(`linux: ${command}`, () => {
    assert.equal(claudeHookDecision({ platform: 'linux', environment: {}, tool_input: { command } }).decision, 'allow');
    assert.equal(clineHookDecision({ platform: 'linux', environment: {}, command }).cancel, false);
  });
});

test("denial message names RustRight and never RightKit's build tool", () => {
  const reason = claudeHookDecision({ platform: 'darwin', tool_input: { command: 'cargo build' } }).hookSpecificOutput.permissionDecisionReason;
  assert.match(reason, /RustRight/);
  assert.doesNotMatch(reason, /RightKit's build tool/i);
});

// `rustright` is the sanctioned managed form; `rightkit`, its retired name (rustright#91), is refused by name.
const sanctionedForms = [
  'rustright cargo test --manifest-path /workspace/Cargo.toml', 'rustright rustc --version', 'rustright rustdoc --version',
  'rustright.cmd cargo check', 'pnpm exec rustright cargo fmt --check', 'cd /workspace && rustright cargo check',
  'rustright cargo test 2>&1 | tail -20', 'env PATH=/d/.rightkit-managed/agent-bin /d/.rightkit-managed/agent-bin/rustright.cmd cargo test',
  'rustright cargo test', 'rustright remote windows cargo test', 'rustright remote mac cargo test',
  '/d/.rightkit-managed/agent-bin/rustright.cmd cargo check',
  '/Users/a/Library/RightKitManagedAgent/agent-bin/rustright cargo test',
];
const rejectedForms = [
  'rustright cargo -- test && cargo test', 'rustright cargo -- $(cargo test)', 'rustright cargo test | rustdoc x.rs',
  'env PATH=/tmp rustright cargo test', 'rustright remote linux cargo test', 'cargo test',
];
// Spelled at run time so the retired-alias sweep (scripts/retire-rightkit-alias.mjs) never rewrites these.
const RETIRED = ['right', 'kit'].join('');
const retiredForms = [
  `${RETIRED} cargo test`, `${RETIRED}.cmd cargo check`, `/d/.rightkit-managed/agent-bin/${RETIRED}.cmd cargo check`,
  `${RETIRED} status abc`, `${RETIRED} remote windows cargo test`,
];
test('rustright is the only sanctioned managed form; the retired rightkit name is refused by name', () => {
  for (const command of sanctionedForms) assert.equal(managedBuildViolation(command), null, command);
  for (const command of rejectedForms) assert.ok(managedBuildViolation(command), command);
  for (const command of retiredForms) assert.equal(managedBuildViolation(command), 'retired-client:rightkit', command);
  const reason = claudeHookDecision({ platform: 'darwin', tool_input: { command: retiredForms[0] } }).hookSpecificOutput.permissionDecisionReason;
  assert.match(reason, /retired name of the RustRight client/);
  assert.match(reason, /rustright cargo/);
  assert.equal(claudeHookDecision({ platform: 'darwin', environment: { GITHUB_ACTIONS: 'true' }, tool_input: { command: 'rustright cargo -- test' } }).decision, 'allow');
});

test('hook messages say RustRight and suggest rustright cargo', () => {
  const reason = claudeHookDecision({ platform: 'darwin', tool_input: { command: 'cargo build' } }).hookSpecificOutput.permissionDecisionReason;
  assert.match(reason, /rustright cargo/);
  assert.doesNotMatch(reason, /rightkit/i);
});

// githubActionsOnlyViolation also follows the host gate: off the Mac and Dell, plain builds are the intended path.
test('public-repo rule is skipped off the Mac and Dell, kept on them', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-hook-host-'));
  try {
    await setOrigin(root);
    await writeFile(join(root, '.rightgit.json'), '{"compile":"github-actions-only"}');
    for (const command of ['pnpm test', 'pnpm run build', 'node scripts/build.mjs']) {
      for (const platform of ['darwin', 'win32']) {
        assert.equal(claudeHookDecision({ platform, cwd: root, environment: {}, tool_input: { command } }).hookSpecificOutput?.permissionDecision, 'deny', `${platform} ${command}`);
        assert.equal(clineHookDecision({ platform, cwd: root, environment: {}, command }).cancel, true, `${platform} ${command}`);
      }
      assert.equal(claudeHookDecision({ platform: 'linux', cwd: root, environment: {}, tool_input: { command } }).decision, 'allow', `linux ${command}`);
      assert.equal(clineHookDecision({ platform: 'linux', cwd: root, environment: {}, command }).cancel, false, `linux ${command}`);
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});
