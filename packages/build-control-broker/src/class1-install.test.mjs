import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import test from 'node:test';

const here = dirname(fileURLToPath(import.meta.url));
const scripts = join(here, '..', 'scripts');
const install = join(scripts, 'install-class1-macos.sh');
const uninstall = join(scripts, 'uninstall-class1-macos.sh');
const common = ['--node', '/usr/bin/true', '--broker-entry', join(here, 'broker.mjs'), '--broker-config', join(here, '..', 'package.json'), '--watchdog-entry', join(here, 'watchdog.mjs'), '--cargo-home', here, '--git-db', here, '--sccache', here, '--build-root-parent', here];
const run = (script, args) => spawnSync('/bin/sh', [script, ...args], { encoding: 'utf8' });
const darwin = process.platform === 'darwin';

test('both operator scripts are syntactically valid POSIX sh', () => {
  for (const script of [install, uninstall]) assert.equal(spawnSync('/bin/sh', ['-n', script]).status, 0, script);
});

test('install defaults to dry-run: every step is printed, nothing is executed, and sudo is never invoked', { skip: !darwin && 'macOS only' }, () => {
  const result = run(install, common);
  assert.equal(result.status, 0, result.stderr);
  const out = result.stdout;
  for (const step of ['STEP 0', 'STEP 1', 'STEP 2', 'STEP 3', 'STEP 4', 'STEP 5', 'STEP 6', 'STEP 7', 'STEP 8']) assert.match(out, new RegExp(step));
  assert.match(out, /mode: dry-run/);
  assert.match(out, /would run: dscl \. -create \/Users\/_rightkit UniqueID/);
  assert.match(out, /would write \/Library\/LaunchDaemons\/com\.rightkit\.build-broker\.plist/);
  assert.match(out, /<key>UserName<\/key><string>_rightkit<\/string>/);
  assert.match(out, /<key>KeepAlive<\/key><true\/>/);
  assert.match(out, /<key>StartInterval<\/key><integer>60<\/integer>/);
  assert.match(out, /dry-run complete; nothing was changed/);
  assert.doesNotMatch(out, /\bsudo\b/, 'the script must not elevate on its own');
});

test('node pinned from a user home (for example ~/.nvm) is refused in preflight', { skip: !darwin && 'macOS only' }, () => {
  const home = process.env.HOME;
  const pinned = join(home, '.nvm', 'versions', 'node', 'v26', 'bin', 'node');
  const result = run(install, common.map((value, index) => (common[index - 1] === '--node' ? pinned : value)));
  assert.equal(result.status, 2); assert.match(result.stderr, /must be pinned from outside a user home|not an executable file/);
});

test('--apply without root refuses before changing anything', { skip: !darwin && 'macOS only' }, () => {
  if (process.getuid?.() === 0) return;
  const result = run(install, ['--apply', ...common]);
  assert.equal(result.status, 2); assert.match(result.stderr, /must run as root/);
  assert.doesNotMatch(result.stdout, /run: /, 'no command may be executed without root');
});

test('relative paths and plist-breaking characters are refused', { skip: !darwin && 'macOS only' }, () => {
  const relative = run(install, common.map((value, index) => (common[index - 1] === '--cargo-home' ? 'rel/cargo' : value)));
  assert.equal(relative.status, 2); assert.match(relative.stderr, /must be absolute/);
  const injected = run(install, common.map((value, index) => (common[index - 1] === '--cargo-home' ? '/tmp/a&b' : value)));
  assert.equal(injected.status, 2); assert.match(injected.stderr, /cannot be placed in a plist/);
});

test('data roots on a different volume than the build root parent are refused (clonefile needs one volume)', { skip: !darwin && 'macOS only' }, () => {
  const result = run(install, common.map((value, index) => (common[index - 1] === '--build-root-parent' ? '/System/Volumes/Data/private/tmp' : value)));
  // Here = the package on /Volumes/D; /System/Volumes/Data is on the system volume.
  assert.equal(result.status, 2); assert.match(result.stderr, /different volume/);
});

test('rollback defaults to dry-run, removes daemons, and never deletes data roots, logs or receipts', { skip: !darwin && 'macOS only' }, () => {
  const result = run(uninstall, []);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /would run: rm -f \/Library\/LaunchDaemons\/com\.rightkit\.build-broker\.watchdog\.plist/);
  assert.match(result.stdout, /STEP 6: data roots, build roots, logs and receipts are NOT touched/);
  assert.doesNotMatch(result.stdout, /rm -rf .*(cargo|git|sccache|build)/i);
  assert.match(result.stdout, /keep the pinned node runtime/);
});

test('rollback --apply is refused without root', { skip: !darwin && 'macOS only' }, () => {
  if (process.getuid?.() === 0) return;
  const result = run(uninstall, ['--apply']);
  assert.equal(result.status, 2); assert.match(result.stderr, /must run as root/);
});
