import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { localDevelopmentCargoAllowed, localDevelopmentPolicy } from './local-development.mjs';

test('local development binds exact repository, Windows, native target & build-only commands', () => {
  const root = mkdtempSync(join(tmpdir(), 'rightkit-local-dev-'));
  try {
    mkdirSync(join(root, '.git')); mkdirSync(join(root, 'engine'));
    writeFileSync(join(root, '.git/config'), '[remote "origin"]\n url = https://github.com/Orthic-Labs/Membrane.git\n');
    writeFileSync(join(root, 'engine/Cargo.toml'), '[workspace]\n');
    const policy = { schemaVersion: 1, repository: 'Orthic-Labs/Membrane', platform: 'win32', purpose: 'unsigned-installer' };
    writeFileSync(join(root, '.rightkit-local-development.json'), JSON.stringify(policy));
    const fixtureAdmits = (argv, overrides = {}) => localDevelopmentCargoAllowed({ root, workspace: join(root, 'engine'), operation: 'cargo', platform: 'win32', argv, ...overrides });
    for (const command of ['check', 'build', 'test']) assert.equal(fixtureAdmits([command, '--target', 'x86_64-pc-windows-msvc']), true);
    assert.equal(fixtureAdmits(['metadata', '--offline', '--no-deps']), true);
    for (const command of ['publish', 'install', 'run', 'clean']) assert.equal(fixtureAdmits([command, '--target', 'x86_64-pc-windows-msvc']), false);
    assert.equal(fixtureAdmits(['build']), false);
    assert.equal(fixtureAdmits(['build', '--target', 'aarch64-apple-darwin']), false);
    assert.equal(fixtureAdmits(['build', '--target', 'x86_64-pc-windows-msvc'], { platform: 'darwin' }), false);
    assert.equal(fixtureAdmits(['build', '--target', 'x86_64-pc-windows-msvc', '--manifest-path', '../foreign/Cargo.toml']), false);
    assert.equal(fixtureAdmits(['input.rs'], { operation: 'rustc' }), false);
    assert.equal(fixtureAdmits(['input.rs'], { operation: 'rustc', nested: true }), true);
    assert.equal(fixtureAdmits(['-vV'], { operation: 'rustc' }), true);
    writeFileSync(join(root, '.git/config'), '[remote "origin"]\n url = https://github.com/Orthic-Labs/Other.git\n');
    assert.equal(localDevelopmentPolicy(root, 'win32'), null);
    writeFileSync(join(root, '.git/config'), '[remote "upstream"]\n url = https://github.com/Orthic-Labs/Membrane.git\n');
    assert.equal(localDevelopmentPolicy(root, 'win32'), null);
    writeFileSync(join(root, '.rightkit-local-development.json'), JSON.stringify({ ...policy, allowSigning: true }));
    assert.equal(localDevelopmentPolicy(root, 'win32'), null);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
