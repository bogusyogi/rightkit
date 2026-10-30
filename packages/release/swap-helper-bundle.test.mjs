import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createSwapPaths } from './install-app.mjs';
import { assertShippedSwapHelperSigned, prepareSwapHelper, SwapHelperError, SWAP_HELPER_CARGO_ARGS } from './swap-helper-bundle.mjs';

const IDENTITY = 'Developer ID Application: Orthic Labs (TEAM123456)';

function workspace() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-swap-helper-'));
  const packageDir = path.join(root, 'packages', 'release');
  fs.mkdirSync(packageDir, { recursive: true });
  return { root, packageDir };
}

function darwinDeps(root, over = {}) {
  const calls = []; const signed = [];
  const deps = {
    workspaceRoot: root, platform: 'darwin', env: {},
    run: (command, args) => {
      calls.push([command, ...args]);
      if (args[0] === 'metadata') return { status: 0, stdout: JSON.stringify({ target_directory: path.join(root, 'target') }), stderr: '' };
      fs.mkdirSync(path.join(root, 'target', 'release'), { recursive: true });
      fs.writeFileSync(path.join(root, 'target', 'release', 'rightkit-swap'), 'macho-bytes');
      return { status: 0, stdout: '', stderr: '' };
    },
    resolveIdentity: () => IDENTITY,
    signOne: (args) => { signed.push(args); return { status: 0, stdout: '', stderr: '' }; },
    inspect: (cmd, args) => (args.includes('--verify')
      ? 'valid on disk\nsatisfies its designated requirement\n'
      : `Authority=${IDENTITY}\nTimestamp=Oct 17 2026\n`),
    ...over,
  };
  return { deps, calls, signed };
}

test('darwin: builds through the managed cargo call, copies, signs with hardened runtime and timestamp, verifies, writes a receipt', () => {
  const { root, packageDir } = workspace();
  const { deps, calls, signed } = darwinDeps(root);
  const result = prepareSwapHelper(deps);
  assert.deepEqual(calls[0], ['cargo', ...SWAP_HELPER_CARGO_ARGS]);
  const dest = path.join(packageDir, 'bin', 'darwin', 'rightkit-swap');
  assert.equal(result.file, dest);
  assert.equal(fs.readFileSync(dest, 'utf8'), 'macho-bytes');
  assert.deepEqual(signed[0].slice(0, 5), ['--force', '--options', 'runtime', '--timestamp', '--sign']);
  assert.equal(signed[0][5], IDENTITY);
  assert.equal(JSON.parse(fs.readFileSync(`${dest}.sign.json`, 'utf8')).identity, IDENTITY);
});

test('darwin: a failed build, a missing identity or a failed codesign throws a typed error and leaves no helper behind', () => {
  const { root, packageDir } = workspace();
  const dest = path.join(packageDir, 'bin', 'darwin', 'rightkit-swap');
  assert.throws(() => prepareSwapHelper(darwinDeps(root, { run: () => ({ status: 1, stdout: '', stderr: 'boom' }) }).deps), (e) => e instanceof SwapHelperError && e.code === 'SWAP_HELPER_BUILD_FAILED');
  assert.throws(() => prepareSwapHelper(darwinDeps(root, { resolveIdentity: () => { throw new Error('no Developer ID Application signing identity found in Keychain'); } }).deps), (e) => e.code === 'SWAP_HELPER_SIGN_FAILED' && /no Developer ID/.test(e.message));
  assert.throws(() => prepareSwapHelper(darwinDeps(root, { signOne: () => ({ status: 1, stderr: 'denied' }) }).deps), (e) => e.code === 'SWAP_HELPER_SIGN_FAILED');
  assert.equal(fs.existsSync(dest), false);
});

test('N22: a carried metadata call reports the signed receipt outputDir, which wins over the target_directory its stdout prints', () => {
  const { root, packageDir } = workspace();
  const receiptDir = path.join(root, 'managed-output');
  const printedDir = path.join(root, 'snapshot-local-target');
  const { deps } = darwinDeps(root, {
    run: (command, args) => {
      if (args[0] === 'metadata') return { status: 0, outputDir: receiptDir, stdout: JSON.stringify({ target_directory: printedDir }), stderr: '' };
      // The build wrote only under the receipt's directory: finding the helper there proves outputDir was read first.
      fs.mkdirSync(path.join(receiptDir, 'release'), { recursive: true });
      fs.writeFileSync(path.join(receiptDir, 'release', 'rightkit-swap'), 'from-receipt-dir');
      return { status: 0, stdout: '', stderr: '' };
    },
  });
  const result = prepareSwapHelper(deps);
  assert.equal(fs.readFileSync(result.file, 'utf8'), 'from-receipt-dir');
  assert.equal(result.file, path.join(packageDir, 'bin', 'darwin', 'rightkit-swap'));
  // Without an outputDir (a direct call) the printed target_directory is what is read, as before.
  const direct = darwinDeps(root);
  assert.equal(fs.readFileSync(prepareSwapHelper(direct.deps).file, 'utf8'), 'macho-bytes');
});

test('non-darwin: refuses unless the helper is present with a receipt matching its bytes', () => {
  const { root, packageDir } = workspace();
  const deps = { workspaceRoot: root, platform: 'win32', run: () => assert.fail('must not build off the Mac') };
  assert.throws(() => prepareSwapHelper(deps), (e) => e.code === 'SWAP_HELPER_MISSING' && /--local/.test(e.message));
  const dir = path.join(packageDir, 'bin', 'darwin');
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'rightkit-swap'), 'bytes');
  assert.throws(() => prepareSwapHelper(deps), /SWAP_HELPER_MISSING|absent or unsigned/);
  const { deps: mac } = darwinDeps(root);
  prepareSwapHelper({ ...mac, run: darwinDeps(root).deps.run });
  assert.equal(prepareSwapHelper(deps).signed, true);
});

test('runtime guard: an unsigned shipped helper is a typed error naming the fix; absent keeps the logged fallback; signed passes', () => {
  const { root } = workspace();
  const helper = path.join(root, 'rightkit-swap');
  assert.deepEqual(assertShippedSwapHelperSigned({ file: helper }), { present: false, file: helper });
  fs.writeFileSync(helper, 'x');
  const unsigned = () => { throw new Error('code object is not signed at all'); };
  assert.throws(() => assertShippedSwapHelperSigned({ file: helper, execFileSync: unsigned }), (e) => e.code === 'SWAP_HELPER_UNSIGNED' && /publish npm --package @rightkit\/release --local/.test(e.message));
  assert.equal(assertShippedSwapHelperSigned({ file: helper, execFileSync: () => '' }).present, true);
});

test('createSwapPaths refuses an unsigned helper before running it, and checks each helper once', () => {
  const { root } = workspace();
  const helper = path.join(root, 'rightkit-swap');
  fs.writeFileSync(helper, 'x');
  const ran = []; let verifies = 0;
  const swap = createSwapPaths({ resolve: () => helper, execFileSync: (file, args) => { if (file === 'codesign') { verifies += 1; return ''; } ran.push(file); return ''; } });
  swap('/a', '/b'); swap('/a', '/b');
  assert.equal(verifies, 1);
  assert.equal(ran.length, 2);
  const refusing = createSwapPaths({ resolve: () => helper, execFileSync: (file) => { if (file === 'codesign') throw new Error('unsigned'); ran.push('ran'); return ''; } });
  assert.throws(() => refusing('/a', '/b'), (e) => e.code === 'SWAP_HELPER_UNSIGNED');
  assert.equal(ran.length, 2);
});
