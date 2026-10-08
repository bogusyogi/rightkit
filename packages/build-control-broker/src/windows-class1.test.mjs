import assert from 'node:assert/strict';
import test from 'node:test';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createProtectedBuildRoot, enforceWindowsCopyCostBound, hashWindowsManifest, validateWindowsInputs, WINDOWS_COPY_COST_BOUND_BYTES, WINDOWS_COPY_COST_BOUND_FILES } from './windows-build-root.mjs';
import { restrictedChildSpec, validateRestrictedChildSpec } from './windows-restricted-child.mjs';

const run = promisify(execFile);
const script = fileURLToPath(new URL('../../../ownership-approval/windows/install-class1-service.ps1', import.meta.url));
async function fixture(t) { const root = await realpath(await mkdtemp(join(tmpdir(), 'rk-class1-'))); t.after(() => rm(root, { recursive: true, force: true })); return root; }

test('real-copy bound accepts exact ceiling & refuses byte/count overflow; clone avoids real-copy cost', () => {
  const bound = { bytes: WINDOWS_COPY_COST_BOUND_BYTES, fileCount: WINDOWS_COPY_COST_BOUND_FILES, blockClone: false };
  assert.equal(enforceWindowsCopyCostBound(bound), 'copy');
  for (const over of [{ ...bound, bytes: bound.bytes + 1 }, { ...bound, fileCount: bound.fileCount + 1 }]) assert.throws(() => enforceWindowsCopyCostBound(over), { code: 'RK_CLASS1_COPY_COST_BOUND' });
  assert.equal(enforceWindowsCopyCostBound({ ...bound, bytes: bound.bytes + 1, blockClone: true }), 'block-clone');
  for (const invalid of [{ ...bound, bytes: -1 }, { ...bound, fileCount: 0 }, { ...bound, blockClone: 'unknown' }, { ...bound, bytes: Number.MAX_SAFE_INTEGER + 1 }]) assert.throws(() => enforceWindowsCopyCostBound(invalid), { code: 'RK_CLASS1_INPUT' });
  for (const invalid of [undefined, null, { ...bound, extra: 1 }]) assert.throws(() => enforceWindowsCopyCostBound(invalid), { code: 'RK_CLASS1_INPUT' });
});

test('declared root rejects lexical escape, duplicate, directory & missing input', async (t) => {
  const temp = await fixture(t); const repo = join(temp, 'repo'); await mkdir(repo);
  const file = join(repo, 'source.rs'); await writeFile(file, 'abc'); const outside = join(temp, 'outside'); await writeFile(outside, 'abc');
  assert.deepEqual(await validateWindowsInputs([file], repo), [{ source: file, path: 'source.rs', size: 3 }]);
  await assert.rejects(validateWindowsInputs([outside], repo), { code: 'RK_CLASS1_INPUT_ESCAPE' });
  await assert.rejects(validateWindowsInputs([file, file], repo), { code: 'RK_CLASS1_INPUT' });
  await assert.rejects(validateWindowsInputs([repo], repo), { code: 'RK_CLASS1_INPUT_ESCAPE' });
  await assert.rejects(validateWindowsInputs([], repo), { code: 'RK_CLASS1_INPUT' });
  await assert.rejects(validateWindowsInputs([join(repo, 'missing')], repo), { code: 'RK_CLASS1_INPUT' });
});

test('junction ancestor rejects reparse even if target stays inside declared root', async (t) => {
  const temp = await fixture(t); const repo = join(temp, 'repo'); const target = join(repo, 'real'); await mkdir(target, { recursive: true }); await writeFile(join(target, 'x'), 'abc');
  const junction = join(repo, 'junction');
  try { await symlink(target, junction, process.platform === 'win32' ? 'junction' : 'dir'); } catch (error) { t.skip(`junction creation unavailable: ${error.code}`); return; }
  await assert.rejects(validateWindowsInputs([join(junction, 'x')], repo), { code: 'RK_CLASS1_REPARSE_POINT' });
});

test('escaping junction & symlink leaf refuse before clone', async (t) => {
  const temp = await fixture(t); const repo = join(temp, 'repo'); const outside = join(temp, 'outside'); await mkdir(repo); await mkdir(outside); await writeFile(join(outside, 'x'), 'abc');
  const junction = join(repo, 'escape');
  try { await symlink(outside, junction, process.platform === 'win32' ? 'junction' : 'dir'); } catch (error) { t.skip(`junction creation unavailable: ${error.code}`); return; }
  await assert.rejects(validateWindowsInputs([join(junction, 'x')], repo), { code: 'RK_CLASS1_REPARSE_POINT' });
  try { await symlink(join(outside, 'x'), join(repo, 'leaf'), 'file'); } catch (error) { t.diagnostic(`leaf symlink unavailable: ${error.code}; junction verified`); return; }
  await assert.rejects(validateWindowsInputs([join(repo, 'leaf')], repo), { code: 'RK_CLASS1_REPARSE_POINT' });
});

test('manifest hashes clone bytes, records .git exclusion & refuses changed/malformed files', async (t) => {
  const root = await fixture(t); await writeFile(join(root, 'x'), 'abc'); await writeFile(join(root, 'empty'), '');
  const files = [{ path: 'x', size: 3 }, { path: 'empty', size: 0 }];
  assert.deepEqual(await hashWindowsManifest(root, files), [
    { path: 'empty', size: 0, sha256: 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', gitIncluded: false },
    { path: 'x', size: 3, sha256: 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', gitIncluded: false },
  ]);
  assert.equal((await hashWindowsManifest(root, files, { includeGit: true }))[0].gitIncluded, true);
  await assert.rejects(hashWindowsManifest(root, [{ path: '../escape', size: 3 }]), { code: 'RK_CLASS1_MANIFEST' });
  await assert.rejects(hashWindowsManifest(root, [{ path: 'x', size: 4 }]), { code: 'RK_CLASS1_MANIFEST' });
  await assert.rejects(hashWindowsManifest(root, [{ path: 'missing', size: 0 }]), { code: 'RK_CLASS1_MANIFEST' });
  await assert.rejects(hashWindowsManifest(root, [files[0], files[0]]), { code: 'RK_CLASS1_MANIFEST' });
  await assert.rejects(hashWindowsManifest(root, [{ path: '.git/config', size: 0 }]), { code: 'RK_CLASS1_MANIFEST' });
});

test('snapshot API fails closed on undeclared roots, unknown options & invalid identities', async () => {
  for (const options of [{}, { unknown: true }, { inputs: [], root: 'D:\\managed', repositoryRoot: 'D:\\repo', serviceSid: 'S-1-5-18', agentSid: 'S-1-5-21-1-2-3-4' }]) await assert.rejects(createProtectedBuildRoot(options), { code: 'RK_CLASS1_INPUT' });
});

test('AppContainer spec binds read-only root & distinct per-job output, denies capabilities', () => {
  const spec = restrictedChildSpec('D:\\managed\\build-roots\\class1-job');
  assert.equal(validateRestrictedChildSpec(spec), true); assert.ok(Object.isFrozen(spec)); assert.ok(Object.isFrozen(spec.writablePaths));
  assert.deepEqual(spec.capabilities, []); assert.deepEqual(spec.readOnlyPaths, ['D:\\managed\\build-roots\\class1-job']);
  assert.deepEqual(spec.writablePaths, ['D:\\managed\\build-roots\\class1-job.output']);
  assert.equal(validateRestrictedChildSpec(restrictedChildSpec('D:\\managed\\job', { outputDir: 'D:\\managed\\outputs\\job' })), true);
  assert.throws(() => restrictedChildSpec('D:\\managed\\job', { unknown: true }), { code: 'RK_CLASS1_CHILD_SPEC' });
  for (const bad of [{ ...spec, capabilities: ['internetClient'] }, { ...spec, extra: true }, { ...spec, writablePaths: [] }, { ...spec, appContainerName: 'unknown' }, { ...spec, readOnlyPaths: ['relative'] }]) assert.throws(() => validateRestrictedChildSpec(bad), { code: 'RK_CLASS1_CHILD_SPEC' });
  for (const outputDir of ['D:\\managed\\job', 'D:\\managed\\job\\target', 'D:\\managed', 'D:\\managed\\..\\output', '\\\\server\\share', 'D:\\managed\\job:stream']) assert.throws(() => restrictedChildSpec('D:\\managed\\job', { outputDir }), { code: 'RK_CLASS1_CHILD_SPEC' });
});

test('operator PowerShell parses & -WhatIf -DryRunOutput needs no elevation or mutation', { skip: process.platform !== 'win32' }, async () => {
  const { stdout, stderr } = await run('powershell', ['-NoProfile', '-NonInteractive', '-File', script, '-WhatIf', '-DryRunOutput', '-AgentUser', 'CLASS1TEST\\Agent'], { windowsHide: true, timeout: 20_000 });
  assert.equal(stderr, '');
  for (const pattern of [/PLAN service:.*NT SERVICE\\RightKitBuildControl/, /global\\cargo-home/, /cargo-home\\git\\db/, /global\\compiler-cache/, /build-roots/, /inheritance=removed/, /ReadAndExecute/, /credentials: untouched/]) assert.match(stdout, pattern);
  assert.doesNotMatch(stdout, /CHANGE|ELEVATION_REQUIRED/);
  const whatIf = await run('powershell', ['-NoProfile', '-NonInteractive', '-File', script, '-WhatIf', '-AgentUser', 'CLASS1TEST\\Agent'], { windowsHide: true, timeout: 20_000 });
  assert.equal(whatIf.stderr, ''); assert.match(whatIf.stdout, /PLAN service:/);
  // Parse embedded helper PowerShell without compiling P/Invoke C# or executing it.
  const source = await readFile(new URL('./windows-build-root.mjs', import.meta.url), 'utf8');
  const helper = source.split('export const WINDOWS_BUILD_ROOT_HELPER = String.raw`')[1].split('`;')[0];
  const encoded = Buffer.from(helper, 'utf8').toString('base64');
  const parsed = await run('powershell', ['-NoProfile', '-NonInteractive', '-Command', `$e=$null;$t=$null;[System.Management.Automation.Language.Parser]::ParseInput([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${encoded}')),[ref]$t,[ref]$e)|Out-Null;if($e.Count){$e|Out-String|Write-Error;exit 1};'helper parse OK'`], { windowsHide: true, timeout: 20_000 });
  assert.match(parsed.stdout, /helper parse OK/);
});
